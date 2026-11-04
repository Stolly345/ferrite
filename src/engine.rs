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
    /// Waiting for a free download slot (Ferrite's queue pauses it in the engine).
    Queued,
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
            State::Queued => "Queued",
            State::Error => "Error",
        }
    }

    /// CSS class for the progress bar: the color says the state.
    pub fn css(self) -> &'static str {
        match self {
            State::GettingFileList | State::Checking | State::Downloading => "downloading",
            State::Seeding => "seeding",
            State::Done | State::Paused | State::Queued => "inactive",
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
    /// Share of the torrent the peer has (0–1), when known.
    pub progress: Option<f64>,
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

/// What Ferrite needs to add a torrent again: after moving its data, editing its trackers, or to
/// verify its files from scratch.
#[derive(Clone)]
pub struct TorrentInfo {
    /// The original .torrent file.
    pub torrent: Vec<u8>,
    /// The bencoded info dictionary, byte for byte (its hash is the info hash).
    pub info: Vec<u8>,
    pub folder: PathBuf,
    pub only_files: Option<Vec<usize>>,
    pub paused: bool,
    pub multi_file: bool,
    /// Every file's path inside the torrent folder.
    pub files: Vec<PathBuf>,
}

/// Builds a .torrent around an unchanged info dictionary with the given trackers. Keys are written
/// in sorted order (announce, announce-list, info), as bencode requires.
pub fn torrent_with_trackers(info: &[u8], trackers: &[String]) -> Vec<u8> {
    fn bytes(out: &mut Vec<u8>, b: &[u8]) {
        out.extend_from_slice(b.len().to_string().as_bytes());
        out.push(b':');
        out.extend_from_slice(b);
    }
    let mut out = b"d".to_vec();
    if let Some(first) = trackers.first() {
        bytes(&mut out, b"announce");
        bytes(&mut out, first.as_bytes());
        bytes(&mut out, b"announce-list");
        out.push(b'l');
        for t in trackers {
            out.push(b'l');
            bytes(&mut out, t.as_bytes());
            out.push(b'e');
        }
        out.push(b'e');
    }
    bytes(&mut out, b"info");
    out.extend_from_slice(info);
    out.push(b'e');
    out
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
    /// One add at a time: librqbit can take simultaneous adds for duplicates.
    add_lock: std::sync::Arc<tokio::sync::Mutex<()>>,
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
    /// socks5://[user:pass@]host:port for every outgoing peer connection.
    pub proxy_url: Option<String>,
    /// A list of IP ranges to refuse (P2P / eMule format), by URL or file path.
    pub blocklist_url: Option<String>,
    /// Bind all traffic to this network interface (a VPN's, say), so nothing leaks if it drops.
    pub bind_device: Option<String>,
    /// Most peers connected per torrent.
    pub peer_limit: Option<usize>,
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
            connect: opts.proxy_url.clone().filter(|p| !p.trim().is_empty()).map(|p| librqbit::ConnectionOptions {
                proxy_url: Some(p.trim().to_string()),
                ..Default::default()
            }),
            blocklist_url: opts.blocklist_url.clone().filter(|b| !b.trim().is_empty()),
            bind_device_name: opts.bind_device.clone().filter(|d| !d.trim().is_empty()),
            peer_limit: opts.peer_limit.filter(|&n| n > 0),
            ..Default::default()
        };
        let dir = opts.download_dir.clone();
        let session = rt
            .spawn(Session::new_with_opts(dir, session_opts))
            .await
            .context("engine task failed")?
            .context("couldn't start the torrent engine")?;
        Ok(Engine::Real(Real {
            api: Api::new(session, None, None),
            rt,
            peer_prev: Mutex::new(HashMap::new()),
            add_lock: Default::default(),
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
        let lock = r.add_lock.clone();
        r.rt.spawn(async move {
            let _one_at_a_time = lock.lock().await;
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
        let lock = r.add_lock.clone();
        r.rt.spawn(async move {
            let _one_at_a_time = lock.lock().await;
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

    /// This torrent's own speed limits (0 = none), applied while it runs. Returns false when the
    /// torrent isn't running; Ferrite applies them again once it is.
    pub fn set_torrent_limits(&self, id: usize, down_kbps: u32, up_kbps: u32) -> bool {
        let Engine::Real(r) = self else { return true };
        let Some(live) = r.api.session().get(id.into()).and_then(|h| h.live()) else { return false };
        let limits = live.ratelimits();
        let (down, up) = (bps(down_kbps), bps(up_kbps));
        if limits.get_download_bps() != down {
            limits.set_download_bps(down);
        }
        if limits.get_upload_bps() != up {
            limits.set_upload_bps(up);
        }
        true
    }

    pub fn torrent_info(&self, id: usize) -> Option<TorrentInfo> {
        let Engine::Real(r) = self else { return None };
        let handle = r.api.session().get(id.into())?;
        let meta = handle.metadata.load();
        let meta = meta.as_ref()?;
        let details = r.api.api_torrent_details(id.into()).ok()?;
        let files: Vec<PathBuf> = meta.file_infos.iter().map(|f| f.relative_filename.clone()).collect();
        Some(TorrentInfo {
            torrent: meta.torrent_bytes.to_vec(),
            info: meta.info_bytes.to_vec(),
            folder: PathBuf::from(details.output_folder),
            only_files: handle.only_files(),
            paused: handle.is_paused(),
            multi_file: files.len() > 1,
            files,
        })
    }

    /// Forgets a torrent (keeping its files) and adds it again from `info` with the given torrent
    /// bytes and folder. The engine then checks the files on disk from scratch.
    async fn readd(&self, id: usize, info: TorrentInfo, torrent: Vec<u8>, folder: PathBuf) -> Result<()> {
        let Engine::Real(r) = self else { return Ok(()) };
        let api = r.api.clone();
        r.rt.spawn(async move {
            api.api_torrent_action_forget(id.into()).await.map_err(|e| anyhow!("{e}"))?;
            let opts = AddTorrentOptions {
                paused: info.paused,
                only_files: info.only_files,
                output_folder: Some(folder.to_string_lossy().into_owned()),
                overwrite: true,
                ..Default::default()
            };
            api.session()
                .add_torrent(AddTorrent::from_bytes(torrent), Some(opts))
                .await
                .map(drop)
        })
        .await?
    }

    /// Checks every piece on disk again.
    pub async fn verify(&self, id: usize) -> Result<()> {
        let info = self.torrent_info(id).context("The torrent's details aren't known yet")?;
        let (torrent, folder) = (info.torrent.clone(), info.folder.clone());
        self.readd(id, info, torrent, folder).await
    }

    /// Replaces the torrent's trackers (keeps the info hash: only the outer .torrent changes).
    pub async fn set_trackers(&self, id: usize, trackers: Vec<String>) -> Result<()> {
        let info = self.torrent_info(id).context("The torrent's details aren't known yet")?;
        let torrent = torrent_with_trackers(&info.info, &trackers);
        let folder = info.folder.clone();
        self.readd(id, info, torrent, folder).await
    }

    /// Moves the downloaded files under `dest` and continues from there.
    pub async fn move_data(&self, id: usize, dest: PathBuf) -> Result<()> {
        let Engine::Real(r) = self else { bail!("Moving data is off in the demo") };
        let info = self.torrent_info(id).context("The torrent's details aren't known yet")?;
        // A multi-file torrent keeps its own folder name inside the destination.
        let new_folder = match (info.multi_file, info.folder.file_name()) {
            (true, Some(name)) => dest.join(name),
            _ => dest,
        };
        if new_folder == info.folder {
            return Ok(());
        }
        self.pause(id).await.ok();
        let (from, to, files) = (info.folder.clone(), new_folder.clone(), info.files.clone());
        r.rt
            .spawn_blocking(move || move_files(&from, &to, &files))
            .await
            .context("move task failed")??;
        let torrent = info.torrent.clone();
        self.readd(id, info, torrent, new_folder).await
    }

    /// Makes a .torrent from a file or folder.
    pub async fn create_torrent(&self, path: PathBuf, trackers: Vec<String>, private: bool) -> Result<Vec<u8>> {
        let Engine::Real(r) = self else { bail!("Creating torrents is off in the demo") };
        r.rt.spawn(async move {
            let spawner = librqbit::spawn_utils::BlockingSpawner::new(2);
            let created = librqbit::create_torrent(
                &path,
                librqbit::CreateTorrentOptions { name: None, trackers, piece_length: None },
                &spawner,
            )
            .await?;
            let bytes = created.as_bytes()?.to_vec();
            if !private {
                return Ok(bytes);
            }
            // Mark it private (BEP 27): no DHT or peer exchange, trackers only. The flag lives in the
            // info dictionary, so it is added before the hash is ever computed by a client.
            Ok(make_private(&bytes).unwrap_or(bytes))
        })
        .await?
    }

    /// Downloads files in this order (file indices, first = soonest). Returns false when the
    /// torrent isn't running; Ferrite applies it again once it is.
    pub fn set_file_order(&self, id: usize, order: &[usize]) -> bool {
        let Engine::Real(r) = self else { return true };
        let Some(live) = r.api.session().get(id.into()).and_then(|h| h.live()) else { return false };
        if live.file_priorities().starts_with(order) {
            return true;
        }
        live.set_file_priorities(order);
        true
    }

    /// Refuses this address from now on and disconnects it everywhere.
    pub fn ban(&self, ip: std::net::IpAddr) {
        let Engine::Real(r) = self else { return };
        r.api.session().banned.write().insert(ip);
        r.api.session().with_torrents(|it| {
            for (_, h) in it {
                if let Some(live) = h.live() {
                    live.drop_peers_at(ip);
                }
            }
        });
    }

    pub fn clear_bans(&self) {
        if let Engine::Real(r) = self {
            r.api.session().banned.write().clear();
        }
    }

    /// The port peers reach this engine on.
    pub fn listen_port(&self) -> Option<u16> {
        match self {
            Engine::Real(r) => r.api.session().announce_port(),
            Engine::Demo(_) => None,
        }
    }

    /// The engine's API, for the HTTP server (streaming, remote control).
    pub fn api(&self) -> Option<(Api, tokio::runtime::Handle)> {
        match self {
            Engine::Real(r) => Some((r.api.clone(), r.rt.clone())),
            Engine::Demo(_) => None,
        }
    }

    pub async fn stop(&self) {
        if let Engine::Real(r) = self {
            let session = r.api.session().clone();
            let _ = r.rt.spawn(async move { session.stop().await }).await;
        }
    }
}

/// Moves each file from one torrent folder to another, copying across drives when a rename can't.
fn move_files(from: &std::path::Path, to: &std::path::Path, files: &[PathBuf]) -> Result<()> {
    for rel in files {
        let (src, dst) = (from.join(rel), to.join(rel));
        if !src.exists() {
            continue;
        }
        if let Some(parent) = dst.parent() {
            std::fs::create_dir_all(parent).with_context(|| format!("couldn't create {}", parent.display()))?;
        }
        if std::fs::rename(&src, &dst).is_err() {
            std::fs::copy(&src, &dst).with_context(|| format!("couldn't copy {}", src.display()))?;
            std::fs::remove_file(&src).ok();
        }
    }
    // Tidy up folders the move left empty.
    for rel in files {
        let mut dir = from.join(rel).parent().map(|p| p.to_path_buf());
        while let Some(d) = dir {
            if d == from.parent().unwrap_or(from) || std::fs::remove_dir(&d).is_err() {
                break;
            }
            dir = d.parent().map(|p| p.to_path_buf());
        }
    }
    Ok(())
}

/// Adds `7:privatei1e` to a .torrent's info dictionary (keys stay sorted: "private" sorts after
/// "pieces" and the other keys librqbit writes, so it goes right before the dictionary's end).
fn make_private(torrent: &[u8]) -> Option<Vec<u8>> {
    let pos = torrent.windows(6).position(|w| w == b"4:info")? + 6;
    let (_, rest) = librqbit_bencode::from_bytes_with_rest::<librqbit_bencode::BencodeValue<librqbit_bencode::ByteBuf>>(&torrent[pos..]).ok()?;
    let info_end = torrent.len() - rest.len();
    let mut out = torrent[..info_end - 1].to_vec();
    out.extend_from_slice(b"7:privatei1e");
    out.extend_from_slice(&torrent[info_end - 1..]);
    Some(out)
}

pub enum Source {
    Url(String),
    File(PathBuf),
}

const V2_ONLY: &str = "This is a BitTorrent v2-only torrent, which Ferrite can't open yet. \
Hybrid (v1 + v2) and v1 torrents work — look for one of those.";

impl Source {
    fn into_add(self) -> Result<AddTorrent<'static>> {
        Ok(match self {
            Source::Url(u) => {
                if u.starts_with("magnet:") && u.contains("urn:btmh:") && !u.contains("urn:btih:") {
                    bail!(V2_ONLY);
                }
                AddTorrent::from_url(u)
            }
            Source::File(p) => {
                let bytes = std::fs::read(&p).with_context(|| format!("couldn't read {}", p.display()))?;
                let has = |needle: &[u8]| bytes.windows(needle.len()).any(|w| w == needle);
                if has(b"9:file tree") && !has(b"6:pieces") {
                    bail!(V2_ONLY);
                }
                AddTorrent::from_bytes(bytes)
            }
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

        let d_total_pieces = self
            .api
            .api_torrent_list()
            .torrents
            .into_iter()
            .find(|t| t.id == Some(id))
            .map(|t| t.total_pieces)
            .unwrap_or(0);
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
                    progress: p.have_pieces.filter(|_| d_total_pieces > 0).map(|h| (h as f64 / d_total_pieces as f64).min(1.0)),
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
                        progress: Some(if i % 3 == 1 { 0.35 + i as f64 * 0.04 } else { 1.0 }),
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
                proxy_url: None,
                blocklist_url: None,
                bind_device: None,
                peer_limit: None,
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

    fn offline_opts(root: &std::path::Path, name: &str) -> EngineOptions {
        EngineOptions {
            download_dir: root.join(name).join("downloads"),
            session_dir: root.join(name).join("session"),
            offline: true,
            port: 0,
            upnp: false,
            down_limit_kbps: 0,
            up_limit_kbps: 0,
            proxy_url: None,
            blocklist_url: None,
            bind_device: None,
            peer_limit: None,
        }
    }

    /// Adds a torrent pointing straight at a known peer (no trackers or DHT in these tests).
    async fn add_from_peer(engine: &Engine, torrent: Vec<u8>, folder: PathBuf, peer: std::net::SocketAddr) {
        let (api, rt) = engine.api().unwrap();
        rt.spawn(async move {
            let opts = AddTorrentOptions {
                output_folder: Some(folder.to_string_lossy().into_owned()),
                initial_peers: Some(vec![peer]),
                overwrite: true,
                ..Default::default()
            };
            api.session().add_torrent(AddTorrent::from_bytes(torrent), Some(opts)).await.map(drop)
        })
        .await
        .unwrap()
        .unwrap();
    }

    /// Two engines on this machine: B downloads from A for real; B sees A's progress; a third
    /// engine that banned the address gets nothing.
    #[tokio::test(flavor = "multi_thread")]
    async fn real_transfer_peer_progress_and_ban() {
        let root = std::env::temp_dir().join(format!("ferrite-swarm-{}", std::process::id()));
        let seed_dir = root.join("seed");
        std::fs::create_dir_all(&seed_dir).unwrap();
        let file = seed_dir.join("swarm.bin");
        let content: Vec<u8> = (0..3_000_000u32).map(|i| (i.wrapping_mul(2246822519) >> 11) as u8).collect();
        std::fs::write(&file, &content).unwrap();

        // Require encryption both ways: plaintext can't be a silent fallback, so a finished
        // transfer proves the MSE handshake and the RC4 streams work.
        librqbit::mse::set_mode(librqbit::mse::Mode::Require);
        let handle = tokio::runtime::Handle::current();
        let a = Engine::start(handle.clone(), offline_opts(&root, "a")).await.unwrap();
        let torrent = a.create_torrent(file.clone(), vec![], false).await.unwrap();
        a.add(torrent.clone(), AddOptions { folder: seed_dir.clone(), only_files: None, paused: false }).await.unwrap();
        wait_for(&a, State::Seeding).await;
        let port = a.api().unwrap().0.session().announce_port().expect("A listens");
        let peer_a: std::net::SocketAddr = ([127, 0, 0, 1], port).into();

        // B downloads it from A.
        let b = Engine::start(handle.clone(), offline_opts(&root, "b")).await.unwrap();
        let b_dir = root.join("b-files");
        b.set_limits(400, 0); // slow enough to watch the peer mid-transfer
        add_from_peer(&b, torrent.clone(), b_dir.clone(), peer_a).await;
        let mut saw_seeder = false;
        for _ in 0..300 {
            let row = b.list().into_iter().next().unwrap();
            if let Some(d) = b.details(row.id) {
                saw_seeder |= d.peers.iter().any(|p| p.progress == Some(1.0));
            }
            if row.state == State::Seeding {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        assert_eq!(b.list()[0].state, State::Seeding, "B should finish downloading from A");
        assert_eq!(std::fs::read(b_dir.join("swarm.bin")).unwrap(), content);
        assert!(saw_seeder, "B should see A as a peer with the whole torrent");
        let id = b.list()[0].id;
        assert!(b.set_file_order(id, &[0]));

        // C has banned 127.0.0.1: it never gets a byte.
        let c = Engine::start(handle, offline_opts(&root, "c")).await.unwrap();
        c.ban(std::net::Ipv4Addr::LOCALHOST.into());
        add_from_peer(&c, torrent, root.join("c-files"), peer_a).await;
        tokio::time::sleep(Duration::from_secs(4)).await;
        let row = c.list().into_iter().next().unwrap();
        assert_eq!(row.done, 0, "a banned peer must not be used: {row:?}");

        for e in [a, b, c] {
            e.stop().await;
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A minimal HTTP server with Range support, serving one file at every path.
    async fn range_server(body: Vec<u8>) -> std::net::SocketAddr {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let body = std::sync::Arc::new(body);
        tokio::spawn(async move {
            loop {
                let Ok((mut sock, _)) = listener.accept().await else { break };
                let body = body.clone();
                tokio::spawn(async move {
                    loop {
                        let mut req = Vec::new();
                        let mut buf = [0u8; 1024];
                        while !req.windows(4).any(|w| w == b"\r\n\r\n") {
                            match sock.read(&mut buf).await {
                                Ok(0) | Err(_) => return,
                                Ok(n) => req.extend_from_slice(&buf[..n]),
                            }
                        }
                        let text = String::from_utf8_lossy(&req).to_lowercase();
                        let range = text.lines().find_map(|l| l.strip_prefix("range: bytes=")).and_then(|r| {
                            let (a, b) = r.trim().split_once('-')?;
                            Some((a.parse::<usize>().ok()?, b.parse::<usize>().ok()?))
                        });
                        let (status, slice) = match range {
                            Some((a, b)) => ("206 Partial Content", &body[a..=b.min(body.len() - 1)]),
                            None => ("200 OK", &body[..]),
                        };
                        let head = format!("HTTP/1.1 {status}\r\nContent-Length: {}\r\nContent-Type: application/octet-stream\r\n\r\n", slice.len());
                        if sock.write_all(head.as_bytes()).await.is_err() || sock.write_all(slice).await.is_err() {
                            return;
                        }
                    }
                });
            }
        });
        addr
    }

    /// A torrent whose only source is a web seed downloads completely, with no peers at all.
    #[tokio::test(flavor = "multi_thread")]
    async fn web_seed_only_download() {
        use sha1::{Digest, Sha1};
        let root = std::env::temp_dir().join(format!("ferrite-webseed-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let content: Vec<u8> = (0..1_300_000u32).map(|i| (i.wrapping_mul(97) ^ (i >> 7)) as u8).collect();
        let addr = range_server(content.clone()).await;

        // A single-file torrent with url-list = the server.
        let piece_len = 262_144usize;
        let mut pieces = Vec::new();
        for chunk in content.chunks(piece_len) {
            pieces.extend_from_slice(&Sha1::digest(chunk));
        }
        let bstr = |b: &[u8]| [b.len().to_string().as_bytes(), b":", b].concat();
        let info = [
            b"d6:lengthi".to_vec(),
            content.len().to_string().into_bytes(),
            b"e4:name".to_vec(),
            bstr(b"seed.bin"),
            b"12:piece lengthi".to_vec(),
            piece_len.to_string().into_bytes(),
            b"e6:pieces".to_vec(),
            bstr(&pieces),
            b"e".to_vec(),
        ]
        .concat();
        let url = format!("http://{addr}/files/seed.bin");
        let torrent = [b"d4:info".to_vec(), info, b"8:url-list".to_vec(), bstr(url.as_bytes()), b"e".to_vec()].concat();

        let engine = Engine::start(tokio::runtime::Handle::current(), offline_opts(&root, "w")).await.unwrap();
        let folder = root.join("out");
        engine.add(torrent, AddOptions { folder: folder.clone(), only_files: None, paused: false }).await.unwrap();
        let row = wait_for(&engine, State::Seeding).await;
        assert_eq!(row.done, content.len() as u64);
        assert_eq!(std::fs::read(folder.join("seed.bin")).unwrap(), content);
        engine.stop().await;
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Interop with another client (run by hand: see the README). Ferrite downloads, encryption
    /// required, from a peer at FERRITE_INTEROP_PEER using FERRITE_INTEROP_DIR/interop.torrent.
    #[tokio::test(flavor = "multi_thread")]
    #[ignore]
    async fn interop_download_encrypted() {
        let dir = PathBuf::from(std::env::var("FERRITE_INTEROP_DIR").unwrap());
        let peer: std::net::SocketAddr = std::env::var("FERRITE_INTEROP_PEER").unwrap().parse().unwrap();
        librqbit::mse::set_mode(librqbit::mse::Mode::Require);
        let engine = Engine::start(tokio::runtime::Handle::current(), offline_opts(&dir, "ferrite-dl")).await.unwrap();
        let out = dir.join("ferrite-out");
        add_from_peer(&engine, std::fs::read(dir.join("interop.torrent")).unwrap(), out.clone(), peer).await;
        let row = wait_row(&engine, |r| r.state == State::Seeding).await;
        assert_eq!(std::fs::read(out.join("data.bin")).unwrap(), std::fs::read(dir.join("data.bin")).unwrap());
        eprintln!("interop download ok: {} bytes", row.size);
        engine.stop().await;
    }

    /// Interop: Ferrite seeds FERRITE_INTEROP_DIR/data.bin (encryption required), writes its port
    /// to port.txt and waits for the other client to write `done`.
    #[tokio::test(flavor = "multi_thread")]
    #[ignore]
    async fn interop_seed_encrypted() {
        let dir = PathBuf::from(std::env::var("FERRITE_INTEROP_DIR").unwrap());
        librqbit::mse::set_mode(librqbit::mse::Mode::Require);
        let engine = Engine::start(tokio::runtime::Handle::current(), offline_opts(&dir, "ferrite-seed")).await.unwrap();
        let torrent = std::fs::read(dir.join("interop.torrent")).unwrap();
        engine.add(torrent, AddOptions { folder: dir.clone(), only_files: None, paused: false }).await.unwrap();
        wait_for(&engine, State::Seeding).await;
        std::fs::write(dir.join("port.txt"), engine.listen_port().unwrap().to_string()).unwrap();
        for _ in 0..900 {
            if dir.join("done").exists() {
                eprintln!("interop seed ok");
                engine.stop().await;
                return;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        panic!("the other client never finished downloading from Ferrite");
    }

    async fn wait_row(engine: &Engine, f: impl Fn(&TorrentRow) -> bool) -> TorrentRow {
        for _ in 0..150 {
            if let Some(r) = engine.list().into_iter().find(|r| f(r)) {
                return r;
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
        panic!("condition never met: {:?}", engine.list());
    }

    /// Trackers, moving data, verifying and per-torrent limits through the real engine, offline.
    #[tokio::test(flavor = "multi_thread")]
    async fn real_engine_manage() {
        let root = std::env::temp_dir().join(format!("ferrite-manage-{}", std::process::id()));
        let (data, elsewhere) = (root.join("data"), root.join("elsewhere"));
        std::fs::create_dir_all(&data).unwrap();
        std::fs::create_dir_all(&elsewhere).unwrap();
        let file = data.join("manage.bin");
        let content: Vec<u8> = (0..2_000_000u32).map(|i| (i.wrapping_mul(40503) >> 9) as u8).collect();
        std::fs::write(&file, &content).unwrap();

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
                proxy_url: None,
                blocklist_url: None,
                bind_device: None,
                peer_limit: None,
            },
        )
        .await
        .unwrap();
        let torrent = engine.create_torrent(file.clone(), vec![], false).await.unwrap();
        let private = engine.create_torrent(file.clone(), vec![], true).await.unwrap();
        assert!(private.windows(12).any(|w| w == b"7:privatei1e"));
        assert_ne!(torrent, private);

        let hash = engine
            .add(torrent, AddOptions { folder: data.clone(), only_files: None, paused: false })
            .await
            .unwrap();
        let row = wait_for(&engine, State::Seeding).await;

        // Per-torrent limits apply to a running torrent.
        assert!(engine.set_torrent_limits(row.id, 100, 50));

        // New trackers keep the info hash, and the torrent seeds again after the re-check.
        let trackers = vec!["http://tracker.example/announce".to_string(), "udp://other.example:6969/announce".to_string()];
        engine.set_trackers(row.id, trackers.clone()).await.unwrap();
        let row = wait_row(&engine, |r| r.hash == hash && r.state == State::Seeding).await;
        let mut got = engine.details(row.id).unwrap().trackers;
        got.sort();
        let mut want = trackers.clone();
        want.sort();
        assert_eq!(got, want);

        // Moving the data carries on from the new folder.
        engine.move_data(row.id, elsewhere.clone()).await.unwrap();
        assert!(!file.exists() && elsewhere.join("manage.bin").exists());
        let row = wait_row(&engine, |r| r.hash == hash && r.state == State::Seeding).await;
        assert_eq!(engine.details(row.id).unwrap().folder, elsewhere);

        // Damage a piece: verifying finds it missing.
        let moved = elsewhere.join("manage.bin");
        let mut bytes = std::fs::read(&moved).unwrap();
        bytes[10] ^= 0xff;
        std::fs::write(&moved, &bytes).unwrap();
        engine.pause(row.id).await.unwrap();
        engine.verify(row.id).await.unwrap();
        let row = wait_row(&engine, |r| r.hash == hash && r.size > 0 && !matches!(r.state, State::Checking | State::GettingFileList)).await;
        assert!(row.done < row.size, "verify should notice the damaged piece: {row:?}");

        engine.stop().await;
        let _ = std::fs::remove_dir_all(&root);
    }
}
