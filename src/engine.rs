//! The torrent engine behind the UI: librqbit (pure Rust) running on a tokio runtime, or a demo
//! engine with sample torrents and no network (`ferrite --demo`, used for screenshots).
//!
//! Everything here is called from the GTK main thread. Cheap reads (`list`, `details`) are
//! synchronous; actions that touch the network or disk run on the tokio runtime and are awaited
//! from the GTK main loop.

use anyhow::{Context, Result, anyhow, bail};
use librqbit::{
    AddTorrent, AddTorrentOptions, AddTorrentResponse, Api, ListenerMode, ListenerOptions,
    Session, SessionOptions, SessionPersistenceConfig, TorrentStatsState,
};
use std::cell::RefCell;
use std::collections::HashMap;
use std::num::NonZeroU32;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant};

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum State {
    #[default]
    GettingFileList,
    Checking,
    Downloading,
    Seeding,
    Done,
    Paused,
    Error,
}

impl State {
    pub fn text(self) -> &'static str {
        match self {
            State::GettingFileList => "Looking up files…",
            State::Checking => "Checking files…",
            State::Downloading => "Downloading",
            State::Seeding => "Seeding",
            State::Done => "Done",
            State::Paused => "Paused",
            State::Error => "Error",
        }
    }

    /// CSS class for the progress bar: the color says the state.
    pub fn css(self) -> &'static str {
        match self {
            State::GettingFileList | State::Checking | State::Downloading => "downloading",
            State::Seeding => "seeding",
            State::Done | State::Paused => "inactive",
            State::Error => "error",
        }
    }

    pub fn is_running(self) -> bool {
        !matches!(self, State::Paused | State::Done | State::Error)
    }

    pub fn is_complete(self) -> bool {
        matches!(self, State::Seeding | State::Done)
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct TorrentRow {
    pub id: usize,
    pub hash: String,
    pub name: String,
    pub size: u64,
    pub done: u64,
    pub uploaded: u64,
    pub state: State,
    pub error: Option<String>,
    pub down_bps: u64,
    pub up_bps: u64,
    pub eta: Option<Duration>,
    pub peers: u32,
}

impl TorrentRow {
    pub fn progress(&self) -> f64 {
        if self.size == 0 {
            0.0
        } else {
            (self.done as f64 / self.size as f64).clamp(0.0, 1.0)
        }
    }

    /// The status line, with problems said in plain words.
    pub fn status_text(&self) -> String {
        match (&self.state, &self.error) {
            (State::Error, Some(e)) => plain_error(e),
            (s, _) => s.text().to_string(),
        }
    }
}

fn plain_error(e: &str) -> String {
    let lower = e.to_lowercase();
    if lower.contains("no space") || lower.contains("disk full") {
        "Disk is full — make room, then resume".into()
    } else if lower.contains("no such file") || lower.contains("not found") {
        "Files are missing — resume to fetch them again".into()
    } else if lower.contains("permission denied") {
        "Can't write to the folder — check its permissions".into()
    } else {
        format!("Error — {}", e.lines().next().unwrap_or(e))
    }
}

#[derive(Clone, Debug)]
pub struct FileRow {
    pub name: String,
    pub size: u64,
    pub done: u64,
    pub included: bool,
}

#[derive(Clone, Debug)]
pub struct PeerRow {
    pub addr: String,
    pub client: String,
    pub down_bps: u64,
    pub up_bps: u64,
    pub flags: String,
}

#[derive(Clone, Debug, Default)]
pub struct Details {
    pub folder: PathBuf,
    pub files: Vec<FileRow>,
    pub pieces: Option<Vec<bool>>,
    pub peers: Vec<PeerRow>,
    pub trackers: Vec<String>,
}

/// What a torrent holds, read before it is added, for the "Add torrent" dialog.
pub struct Preview {
    pub name: String,
    pub files: Vec<(String, u64)>,
    pub torrent: Vec<u8>,
}

pub struct AddOptions {
    pub folder: PathBuf,
    pub only_files: Option<Vec<usize>>,
    pub paused: bool,
}

pub enum Engine {
    Real(Real),
    Demo(RefCell<Demo>),
}

pub struct Real {
    api: Api,
    rt: tokio::runtime::Handle,
    /// Previous byte counters of each peer, to derive per-peer speeds.
    peer_prev: Mutex<HashMap<String, (u64, u64, Instant)>>,
}

pub struct EngineOptions {
    pub download_dir: PathBuf,
    /// Where the engine keeps resume data.
    pub session_dir: PathBuf,
    /// No DHT, trackers or local discovery (tests).
    pub offline: bool,
    pub port: u16,
    pub upnp: bool,
    pub down_limit_kbps: u32,
    pub up_limit_kbps: u32,
}

fn bps(kbps: u32) -> Option<NonZeroU32> {
    NonZeroU32::new(kbps.saturating_mul(1000))
}

impl Engine {
    pub async fn start(rt: tokio::runtime::Handle, opts: EngineOptions) -> Result<Engine> {
        let session_opts = SessionOptions {
            fastresume: true,
            persistence: Some(SessionPersistenceConfig::Json {
                folder: Some(opts.session_dir.clone()),
            }),
            dht: if opts.offline { None } else { Some(Default::default()) },
            disable_trackers: opts.offline,
            disable_local_service_discovery: opts.offline,
            listen: Some(ListenerOptions {
                mode: ListenerMode::TcpAndUtp,
                listen_addr: (std::net::Ipv6Addr::UNSPECIFIED, opts.port).into(),
                enable_upnp_port_forwarding: opts.upnp,
                ..Default::default()
            }),
            ratelimits: librqbit::limits::LimitsConfig {
                download_bps: bps(opts.down_limit_kbps),
                upload_bps: bps(opts.up_limit_kbps),
            },
            client_name_and_version: Some(format!("Ferrite {}", env!("CARGO_PKG_VERSION"))),
            ..Default::default()
        };
        let dir = opts.download_dir.clone();
        let session = rt
            .spawn(Session::new_with_opts(dir, session_opts))
            .await
            .context("engine task failed")?
            .context("couldn't start the torrent engine")?;
        Ok(Engine::Real(Real {
            api: Api::new(session, None),
            rt,
            peer_prev: Mutex::new(HashMap::new()),
        }))
    }

    pub fn demo() -> Engine {
        Engine::Demo(RefCell::new(Demo::new()))
    }

    pub fn is_demo(&self) -> bool {
        matches!(self, Engine::Demo(_))
    }

    pub fn list(&self) -> Vec<TorrentRow> {
        match self {
            Engine::Real(r) => r.list(),
            Engine::Demo(d) => d.borrow_mut().list(),
        }
    }

    pub fn details(&self, id: usize) -> Option<Details> {
        match self {
            Engine::Real(r) => r.details(id),
            Engine::Demo(d) => d.borrow().details(id),
        }
    }

    pub fn set_limits(&self, down_kbps: u32, up_kbps: u32) {
        if let Engine::Real(r) = self {
            let limits = &r.api.session().ratelimits;
            limits.set_download_bps(bps(down_kbps));
            limits.set_upload_bps(bps(up_kbps));
        }
    }

    /// Reads a magnet link or .torrent file without adding it.
    pub async fn preview(&self, source: Source) -> Result<Preview> {
        let Engine::Real(r) = self else {
            bail!("Adding torrents is off in the demo");
        };
        let api = r.api.clone();
        r.rt.spawn(async move {
            let add = source.into_add()?;
            let opts = AddTorrentOptions {
                list_only: true,
                ..Default::default()
            };
            match api.session().add_torrent(add, Some(opts)).await? {
                AddTorrentResponse::ListOnly(l) => {
                    if api.session().get(l.info_hash.into()).is_some() {
                        bail!("This torrent is already in the list");
                    }
                    let name = l
                        .info
                        .name()
                        .map(|n| n.into_owned())
                        .unwrap_or_else(|| l.info_hash.as_string());
                    let files = l
                        .info
                        .iter_file_details()
                        .map(|f| (f.filename.to_string(), f.len))
                        .collect();
                    Ok(Preview {
                        name,
                        files,
                        torrent: l.torrent_bytes.to_vec(),
                    })
                }
                AddTorrentResponse::AlreadyManaged(..) => bail!("This torrent is already in the list"),
                AddTorrentResponse::Added(..) => bail!("Unexpected engine response"),
            }
        })
        .await?
    }

    /// Adds a previewed torrent. Returns its info hash.
    pub async fn add(&self, torrent: Vec<u8>, opts: AddOptions) -> Result<String> {
        let Engine::Real(r) = self else {
            bail!("Adding torrents is off in the demo");
        };
        let api = r.api.clone();
        r.rt.spawn(async move {
            let add_opts = AddTorrentOptions {
                paused: opts.paused,
                only_files: opts.only_files,
                output_folder: Some(opts.folder.to_string_lossy().into_owned()),
                overwrite: true,
                ..Default::default()
            };
            let resp = api
                .session()
                .add_torrent(AddTorrent::from_bytes(torrent), Some(add_opts))
                .await?;
            match resp {
                AddTorrentResponse::Added(_, h) => Ok(h.info_hash().as_string()),
                AddTorrentResponse::AlreadyManaged(..) => bail!("This torrent is already in the list"),
                AddTorrentResponse::ListOnly(_) => bail!("Unexpected engine response"),
            }
        })
        .await?
    }

    pub async fn pause(&self, id: usize) -> Result<()> {
        match self {
            Engine::Real(r) => {
                let api = r.api.clone();
                r.rt.spawn(async move { api.api_torrent_action_pause(id.into()).await.map(drop) })
                    .await?
                    .map_err(|e| anyhow!("{e}"))
            }
            Engine::Demo(d) => {
                d.borrow_mut().set_paused(id, true);
                Ok(())
            }
        }
    }

    pub async fn resume(&self, id: usize) -> Result<()> {
        match self {
            Engine::Real(r) => {
                let api = r.api.clone();
                r.rt.spawn(async move { api.api_torrent_action_start(id.into()).await.map(drop) })
                    .await?
                    .map_err(|e| anyhow!("{e}"))
            }
            Engine::Demo(d) => {
                d.borrow_mut().set_paused(id, false);
                Ok(())
            }
        }
    }

    pub async fn remove(&self, id: usize, delete_files: bool) -> Result<()> {
        match self {
            Engine::Real(r) => {
                let api = r.api.clone();
                r.rt.spawn(async move {
                    if delete_files {
                        api.api_torrent_action_delete(id.into()).await.map(drop)
                    } else {
                        api.api_torrent_action_forget(id.into()).await.map(drop)
                    }
                })
                .await?
                .map_err(|e| anyhow!("{e}"))
            }
            Engine::Demo(d) => {
                d.borrow_mut().torrents.retain(|t| t.row.id != id);
                Ok(())
            }
        }
    }

    pub async fn set_files(&self, id: usize, files: Vec<usize>) -> Result<()> {
        match self {
            Engine::Real(r) => {
                let api = r.api.clone();
                r.rt.spawn(async move {
                    let set = files.into_iter().collect();
                    api.api_torrent_action_update_only_files(id.into(), &set)
                        .await
                        .map(drop)
                })
                .await?
                .map_err(|e| anyhow!("{e}"))
            }
            Engine::Demo(_) => Ok(()),
        }
    }

    pub async fn stop(&self) {
        if let Engine::Real(r) = self {
            let session = r.api.session().clone();
            let _ = r.rt.spawn(async move { session.stop().await }).await;
        }
    }
}

pub enum Source {
    Url(String),
    File(PathBuf),
}

impl Source {
    fn into_add(self) -> Result<AddTorrent<'static>> {
        Ok(match self {
            Source::Url(u) => AddTorrent::from_url(u),
            Source::File(p) => AddTorrent::from_bytes(
                std::fs::read(&p).with_context(|| format!("couldn't read {}", p.display()))?,
            ),
        })
    }
}

impl Real {
    fn list(&self) -> Vec<TorrentRow> {
        self.api.session().with_torrents(|it| {
            it.map(|(id, h)| {
                let stats = h.stats();
                let has_meta = h.metadata.load().is_some();
                let state = match stats.state {
                    TorrentStatsState::Initializing { .. } if !has_meta => State::GettingFileList,
                    TorrentStatsState::Initializing { .. } => State::Checking,
                    TorrentStatsState::Live if stats.finished => State::Seeding,
                    TorrentStatsState::Live => State::Downloading,
                    TorrentStatsState::Paused if stats.finished => State::Done,
                    TorrentStatsState::Paused => State::Paused,
                    TorrentStatsState::Error => State::Error,
                };
                let (down_bps, up_bps, peers) = match &stats.live {
                    Some(live) => {
                        let peers = serde_json::to_value(&live.snapshot.peer_stats)
                            .ok()
                            .and_then(|v| v.get("live").and_then(|n| n.as_u64()))
                            .unwrap_or(0) as u32;
                        (live.download_speed.as_bytes(), live.upload_speed.as_bytes(), peers)
                    }
                    None => (0, 0, 0),
                };
                let remaining = stats.total_bytes.saturating_sub(stats.progress_bytes);
                let eta = (state == State::Downloading && down_bps > 0)
                    .then(|| Duration::from_secs(remaining / down_bps));
                TorrentRow {
                    id,
                    hash: h.info_hash().as_string(),
                    name: h.name().unwrap_or_else(|| h.info_hash().as_string()),
                    size: stats.total_bytes,
                    done: stats.progress_bytes,
                    uploaded: stats.uploaded_bytes,
                    state,
                    error: stats.error.clone(),
                    down_bps: if state == State::Downloading { down_bps } else { 0 },
                    up_bps,
                    eta,
                    peers,
                }
            })
            .collect()
        })
    }

    fn details(&self, id: usize) -> Option<Details> {
        let handle = self.api.session().get(id.into())?;
        let d = self.api.api_torrent_details(id.into()).ok()?;
        let stats = handle.stats();
        let files = d
            .files
            .unwrap_or_default()
            .into_iter()
            .enumerate()
            .map(|(i, f)| FileRow {
                name: f.name,
                size: f.length,
                done: stats.file_progress.get(i).copied().unwrap_or(0),
                included: f.included,
            })
            .collect();
        let pieces = self
            .api
            .api_dump_haves(id.into())
            .ok()
            .map(|(bf, len)| bf.iter().take(len as usize).map(|b| *b).collect());

        let now = Instant::now();
        let mut prev = self.peer_prev.lock().unwrap();
        let mut peers: Vec<PeerRow> = self
            .api
            .api_peer_stats(id.into(), Default::default())
            .map(|s| s.peers)
            .unwrap_or_default()
            .into_iter()
            .map(|(addr, p)| {
                let key = format!("{id}/{addr}");
                let (down, up) = (p.counters.fetched_bytes, p.counters.uploaded_bytes);
                let (down_bps, up_bps) = match prev.insert(key, (down, up, now)) {
                    Some((pd, pu, t)) => {
                        let secs = now.duration_since(t).as_secs_f64().max(0.5);
                        (
                            (down.saturating_sub(pd) as f64 / secs) as u64,
                            (up.saturating_sub(pu) as f64 / secs) as u64,
                        )
                    }
                    None => (0, 0),
                };
                let mut flags = Vec::new();
                if p.counters.incoming_connections > 0 {
                    flags.push("incoming");
                }
                if let Some(kind) = &p.conn_kind {
                    let k = serde_json::to_value(kind).ok();
                    if k.as_ref().and_then(|v| v.as_str()) == Some("utp") {
                        flags.push("µTP");
                    }
                }
                PeerRow {
                    addr,
                    client: p.client_name.clone().unwrap_or_default(),
                    down_bps,
                    up_bps,
                    flags: flags.join(" · "),
                }
            })
            .collect();
        // Forget peers not seen for a minute.
        prev.retain(|_, (_, _, t)| now.duration_since(*t) < Duration::from_secs(60));
        peers.sort_by(|a, b| (b.down_bps + b.up_bps).cmp(&(a.down_bps + a.up_bps)));

        let mut trackers: Vec<String> =
            handle.shared().trackers.iter().map(|u| u.to_string()).collect();
        trackers.sort();
        Some(Details {
            folder: PathBuf::from(d.output_folder),
            files,
            pieces,
            peers,
            trackers,
        })
    }
}

// ---------------------------------------------------------------------------------------------
// Demo engine: open content (Linux images, Blender Studio films) and simulated transfers.

struct DemoTorrent {
    row: TorrentRow,
    target_down: u64,
    target_up: u64,
    files: Vec<(String, u64)>,
}

pub struct Demo {
    torrents: Vec<DemoTorrent>,
    tick: u64,
}

impl Demo {
    fn new() -> Self {
        let samples: [(&str, u64, f64, State, u64, u64); 11] = [
            ("linuxmint-22.2-cinnamon-64bit.iso", 2_970_000_000, 0.472, State::Downloading, 5_400_000, 410_000),
            ("Elephants Dream (2006) 1080p", 815_000_000, 1.0, State::Seeding, 0, 640_000),
            ("openSUSE-Tumbleweed-DVD-x86_64-Current.iso", 4_610_000_000, 0.224, State::Downloading, 2_900_000, 118_000),
            ("Caminandes 3: Llamigos (2016)", 342_000_000, 1.0, State::Done, 0, 0),
            ("wikidata-20251001-lexemes.json.bz2", 3_120_000_000, 0.113, State::Downloading, 1_420_000, 48_500),
            ("pop-os_24.04_amd64_intel.iso", 2_640_000_000, 0.58, State::Paused, 0, 0),
            ("kali-linux-2025.3-installer-amd64.iso", 4_190_000_000, 1.0, State::Seeding, 0, 215_000),
            ("Charge (2022) Blender Studio 4K", 5_270_000_000, 0.66, State::Checking, 0, 0),
            ("planet-251027.osm.pbf", 0, 0.0, State::GettingFileList, 0, 0),
            ("Agent 327: Operation Barbershop (2017)", 1_050_000_000, 1.0, State::Seeding, 0, 96_000),
            ("fedora-kde-43-x86_64.iso", 2_840_000_000, 0.0, State::Paused, 0, 0),
        ];
        let torrents = samples
            .iter()
            .enumerate()
            .map(|(i, &(name, size, p, state, down, up))| DemoTorrent {
                row: TorrentRow {
                    id: i,
                    hash: format!("{:040x}", i + 1),
                    name: name.to_string(),
                    size,
                    done: (size as f64 * p) as u64,
                    uploaded: (size as f64 * p * 0.12) as u64,
                    state,
                    error: None,
                    down_bps: down,
                    up_bps: up,
                    eta: None,
                    peers: if state.is_running() { 23 } else { 0 },
                },
                target_down: down,
                target_up: up,
                files: if name.ends_with(')') || name.contains("4K") || name.contains("1080p") {
                    vec![
                        (format!("{name}/{name}.mkv"), size * 97 / 100),
                        (format!("{name}/subtitles/en.srt"), size / 100),
                        (format!("{name}/poster.jpg"), size * 2 / 100),
                    ]
                } else {
                    vec![(name.to_string(), size)]
                },
            })
            .collect();
        Demo { torrents, tick: 0 }
    }

    fn list(&mut self) -> Vec<TorrentRow> {
        self.tick += 1;
        let t = self.tick as f64;
        for (i, d) in self.torrents.iter_mut().enumerate() {
            // Swarm-like variation: a few slow and fast swings plus deterministic jitter.
            let fi = i as f64;
            let jitter = ((self.tick.wrapping_mul(2_654_435_761).wrapping_add(i as u64 * 40_503) >> 7) % 1000) as f64 / 1000.0 - 0.5;
            let wobble = (1.0
                + 0.16 * (t / 7.3 + fi).sin()
                + 0.10 * (t / 2.1 + fi * 1.7).sin()
                + 0.06 * (t * 0.9 + fi * 0.4).sin()
                + 0.14 * jitter)
                .max(0.2);
            let r = &mut d.row;
            if r.state == State::Downloading {
                r.down_bps = (d.target_down as f64 * wobble) as u64;
                r.up_bps = (d.target_up as f64 * wobble) as u64;
                r.done = (r.done + r.down_bps).min(r.size);
                r.uploaded += r.up_bps;
                if r.done >= r.size {
                    r.state = State::Seeding;
                    r.down_bps = 0;
                }
                r.eta = (r.down_bps > 0).then(|| Duration::from_secs((r.size - r.done) / r.down_bps));
            } else if r.state == State::Seeding {
                r.up_bps = (d.target_up.max(50_000) as f64 * wobble) as u64;
                r.uploaded += r.up_bps;
                r.eta = None;
            } else {
                r.down_bps = 0;
                r.up_bps = 0;
                r.eta = None;
            }
        }
        self.torrents.iter().map(|d| d.row.clone()).collect()
    }

    fn set_paused(&mut self, id: usize, paused: bool) {
        if let Some(d) = self.torrents.iter_mut().find(|d| d.row.id == id) {
            let r = &mut d.row;
            r.state = match (paused, r.done >= r.size && r.size > 0) {
                (true, true) => State::Done,
                (true, false) => State::Paused,
                (false, true) => State::Seeding,
                (false, false) => {
                    if d.target_down == 0 {
                        d.target_down = 2_400_000;
                        d.target_up = 120_000;
                    }
                    State::Downloading
                }
            };
            r.peers = if paused { 0 } else { 23 };
        }
    }

    fn details(&self, id: usize) -> Option<Details> {
        let d = self.torrents.iter().find(|d| d.row.id == id)?;
        let r = &d.row;
        let total_pieces = 400usize;
        let have = (r.progress() * total_pieces as f64) as usize;
        // Mostly in order, with a few pieces fetched ahead, as a real swarm does.
        let pieces = (r.size > 0).then(|| {
            (0..total_pieces)
                .map(|i| i < have || (i * 7919 % 37 == 0 && i < have + 120))
                .collect()
        });
        let p = r.progress();
        let files = d
            .files
            .iter()
            .map(|(n, s)| FileRow {
                name: n.clone(),
                size: *s,
                done: (*s as f64 * p) as u64,
                included: true,
            })
            .collect();
        let clients = ["Deluge 2.1.1", "qBittorrent 5.0.1", "Transmission 4.0.6", "libtorrent 2.0.10", "BiglyBT 3.7"];
        let peers = if r.state.is_running() {
            (0..12)
                .map(|i| {
                    let share = 1.0 / (i as f64 + 2.0);
                    PeerRow {
                        addr: format!("203.0.113.{}:{}", 20 + i * 9, 20000 + i * 1733),
                        client: clients[i % clients.len()].to_string(),
                        down_bps: (r.down_bps as f64 * share) as u64,
                        up_bps: if i % 3 == 0 { (r.up_bps as f64 * share) as u64 } else { 0 },
                        flags: ["", "incoming", "µTP", "incoming · µTP"][i % 4].to_string(),
                    }
                })
                .collect()
        } else {
            Vec::new()
        };
        Some(Details {
            folder: dirs::download_dir().unwrap_or_default(),
            files,
            pieces,
            peers,
            trackers: vec![
                "udp://open.demonii.com:1337/announce".into(),
                "udp://tracker.opentrackr.org:1337/announce".into(),
            ],
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn wait_for(engine: &Engine, want: State) -> TorrentRow {
        for _ in 0..100 {
            if let Some(r) = engine.list().into_iter().next()
                && r.state == want
            {
                return r;
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
        panic!("never reached {want:?}: {:?}", engine.list());
    }

    /// Creates a torrent from a local file, then adds, inspects, pauses, resumes and removes
    /// it through the real engine — no network needed.
    #[tokio::test(flavor = "multi_thread")]
    async fn real_engine_round_trip() {
        let root = std::env::temp_dir().join(format!("ferrite-test-{}", std::process::id()));
        let data = root.join("data");
        std::fs::create_dir_all(&data).unwrap();
        let file = data.join("sample.bin");
        let content: Vec<u8> = (0..3_000_000u32).map(|i| (i.wrapping_mul(2654435761) >> 13) as u8).collect();
        std::fs::write(&file, &content).unwrap();

        let spawner = librqbit::spawn_utils::BlockingSpawner::new(1);
        let created = librqbit::create_torrent(
            &file,
            librqbit::CreateTorrentOptions { name: None, trackers: vec![], piece_length: None },
            &spawner,
        )
        .await
        .unwrap();
        let torrent_path = root.join("sample.torrent");
        std::fs::write(&torrent_path, created.as_bytes().unwrap()).unwrap();

        let engine = Engine::start(
            tokio::runtime::Handle::current(),
            EngineOptions {
                download_dir: root.join("downloads"),
                session_dir: root.join("session"),
                offline: true,
                port: 0,
                upnp: false,
                down_limit_kbps: 0,
                up_limit_kbps: 0,
            },
        )
        .await
        .unwrap();

        let preview = engine.preview(Source::File(torrent_path.clone())).await.unwrap();
        assert_eq!(preview.name, "sample.bin");
        assert_eq!(preview.files, vec![("sample.bin".to_string(), 3_000_000)]);

        let hash = engine
            .add(preview.torrent, AddOptions { folder: data.clone(), only_files: None, paused: false })
            .await
            .unwrap();
        assert_eq!(hash, created.info_hash().as_string());

        // The file is already there: checking finds every piece and it seeds.
        let row = wait_for(&engine, State::Seeding).await;
        assert_eq!(row.name, "sample.bin");
        assert_eq!(row.size, 3_000_000);
        assert_eq!(row.done, row.size);

        let d = engine.details(row.id).unwrap();
        assert_eq!(d.folder, data);
        assert_eq!(d.files.len(), 1);
        assert_eq!(d.files[0].done, 3_000_000);
        assert!(d.pieces.as_ref().is_some_and(|p| !p.is_empty() && p.iter().all(|&b| b)));

        // Adding it again is refused in plain words.
        let again = engine.preview(Source::File(torrent_path)).await;
        assert!(again.is_err_and(|e| e.to_string().contains("already")));

        engine.pause(row.id).await.unwrap();
        wait_for(&engine, State::Done).await;
        engine.resume(row.id).await.unwrap();
        wait_for(&engine, State::Seeding).await;

        engine.remove(row.id, false).await.unwrap();
        assert!(engine.list().is_empty());
        assert!(file.exists(), "removing without deleting keeps the files");

        engine.stop().await;
        let _ = std::fs::remove_dir_all(&root);
    }
}
