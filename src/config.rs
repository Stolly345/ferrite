//! Settings and the app's own per-torrent data (order, label, date added), saved as JSON in
//! `~/.config/ferrite/settings.json`. The engine keeps its own resume data separately.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug, Default)]
#[serde(rename_all = "lowercase")]
pub enum Theme {
    #[default]
    System,
    Light,
    Dark,
}

/// Eight mid-tones that read on both light and dark backgrounds.
pub const LABEL_COLORS: [&str; 8] = [
    "#3b82f6", "#8b5cf6", "#22c55e", "#f59e0b", "#ef4444", "#ec4899", "#14b8a6", "#64748b",
];

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Label {
    pub name: String,
    /// Index into `LABEL_COLORS`.
    pub color: usize,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct TorrentMeta {
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub added: u64,
    #[serde(default)]
    pub down_limit_kbps: u32,
    #[serde(default)]
    pub up_limit_kbps: u32,
    /// Waiting for a download slot: paused by the queue, not by the user.
    #[serde(default)]
    pub queued: bool,
    /// Started by hand past the queue's limit ("Start now", Resume): the queue leaves it alone.
    #[serde(default)]
    pub forced: bool,
    /// Seconds spent seeding, for the seeding-time goal.
    #[serde(default)]
    pub seed_secs: u64,
    /// This torrent's seeding goal; None follows the global settings.
    #[serde(default)]
    pub goal: Option<SeedGoal>,
    /// The user resumed it after it reached its goal: keep seeding.
    #[serde(default)]
    pub goal_ignored: bool,
    /// Per-file priority (index → 0 low, 1 normal, 2 high); missing means normal.
    #[serde(default)]
    pub priorities: Vec<u8>,
}

/// When a finished torrent stops seeding. 0 in a field means no limit on it.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, Default, PartialEq)]
pub struct SeedGoal {
    pub ratio: f64,
    pub minutes: u32,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Feed {
    pub url: String,
    /// Only items whose title matches this (a regular expression, case-insensitive); empty takes all.
    #[serde(default)]
    pub filter: String,
    #[serde(default = "yes")]
    pub enabled: bool,
}

fn yes() -> bool {
    true
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(default)]
pub struct Config {
    pub theme: Theme,
    pub download_dir: PathBuf,
    pub notify_finished: bool,
    pub down_limit_kbps: u32,
    pub up_limit_kbps: u32,
    pub listen_port: u16,
    pub upnp: bool,
    pub ask_before_adding: bool,
    pub labels: Vec<Label>,
    /// Info hashes in the user's № order.
    pub order: Vec<String>,
    pub torrents: HashMap<String, TorrentMeta>,
    pub visible_columns: Vec<String>,
    pub window_width: i32,
    pub window_height: i32,
    pub details_height: i32,

    /// Keep running (and seeding) with the window closed, from the tray.
    pub run_in_background: bool,
    /// Told the user once that closing the window keeps Ferrite running.
    pub background_hint_shown: bool,
    /// Downloads at a time; 0 = no limit.
    pub queue_limit: u32,
    /// Global seeding goal; 0 in a field = no limit on it.
    pub goal: SeedGoal,

    /// Alternative speed limits, switched by hand or on a schedule.
    pub alt_down_kbps: u32,
    pub alt_up_kbps: u32,
    pub alt_enabled: bool,
    pub alt_scheduled: bool,
    /// Minutes after midnight.
    pub alt_from: u32,
    pub alt_to: u32,
    /// Bit 0 = Monday … bit 6 = Sunday.
    pub alt_days: u8,

    /// Add every .torrent that appears here.
    pub watch_dir: Option<PathBuf>,

    pub proxy_url: Option<String>,
    pub blocklist_url: Option<String>,
    pub bind_device: Option<String>,
    /// Most peers per torrent; 0 = the engine's default.
    pub peer_limit: u32,
    /// Protocol encryption: "prefer" (default), "require" or "off".
    pub encryption: String,

    /// Remote control (web interface and API).
    pub remote_enabled: bool,
    pub remote_port: u16,
    /// Listen on every network, not just this computer.
    pub remote_lan: bool,
    pub remote_user: String,
    pub remote_password: String,

    pub feeds: Vec<Feed>,
    /// Peers the user banned (IP addresses).
    pub banned: Vec<String>,
    /// Feed items already added (guid or link), newest last.
    pub feed_seen: Vec<String>,
    /// Minutes between feed checks.
    pub feed_interval_min: u32,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            theme: Theme::System,
            download_dir: dirs::download_dir()
                .or_else(|| dirs::home_dir().map(|h| h.join("Downloads")))
                .unwrap_or_else(|| PathBuf::from(".")),
            notify_finished: true,
            down_limit_kbps: 0,
            up_limit_kbps: 0,
            listen_port: 51413,
            upnp: true,
            ask_before_adding: true,
            labels: Vec::new(),
            order: Vec::new(),
            torrents: HashMap::new(),
            visible_columns: ["num", "name", "size", "progress", "status", "down", "up", "eta"]
                .map(String::from)
                .to_vec(),
            window_width: 1280,
            window_height: 800,
            details_height: 300,
            run_in_background: true,
            background_hint_shown: false,
            queue_limit: 3,
            goal: SeedGoal::default(),
            alt_down_kbps: 500,
            alt_up_kbps: 100,
            alt_enabled: false,
            alt_scheduled: false,
            alt_from: 8 * 60,
            alt_to: 23 * 60,
            alt_days: 0b0111_1111,
            watch_dir: None,
            proxy_url: None,
            blocklist_url: None,
            bind_device: None,
            peer_limit: 0,
            encryption: "prefer".into(),
            remote_enabled: false,
            remote_port: 9091,
            remote_lan: false,
            remote_user: "ferrite".into(),
            remote_password: String::new(),
            feeds: Vec::new(),
            banned: Vec::new(),
            feed_seen: Vec::new(),
            feed_interval_min: 15,
        }
    }
}

/// `FERRITE_HOME=/some/dir` keeps settings and engine data there instead (tests, a second profile).
fn home_override() -> Option<PathBuf> {
    std::env::var_os("FERRITE_HOME").map(PathBuf::from)
}

pub fn config_dir() -> PathBuf {
    if let Some(h) = home_override() {
        return h.join("config");
    }
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("ferrite")
}

pub fn data_dir() -> PathBuf {
    if let Some(h) = home_override() {
        return h.join("data");
    }
    dirs::data_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("ferrite")
}

/// Demo mode never reads or writes the user's settings.
static DEMO: AtomicBool = AtomicBool::new(false);

impl Config {
    /// Fresh settings for `--demo`, with sample labels, kept in memory only.
    pub fn demo() -> Self {
        DEMO.store(true, Ordering::Relaxed);
        let mut c = Config::default();
        c.labels = vec![
            Label { name: "Distros".into(), color: 6 },
            Label { name: "Films".into(), color: 3 },
            Label { name: "Datasets".into(), color: 1 },
        ];
        // Labels of the demo torrents, by their demo hashes (see engine::Demo).
        for (i, label) in ["Distros", "Films", "Distros", "Films", "Datasets", "Distros", "Distros", "Films", "Datasets", "Films", "Distros"]
            .iter()
            .enumerate()
        {
            if !label.is_empty() {
                c.torrents.insert(
                    format!("{:040x}", i + 1),
                    TorrentMeta { label: Some(label.to_string()), ..Default::default() },
                );
            }
        }
        c
    }

    /// Applies the encryption setting to the engine (new connections).
    pub fn apply_encryption(&self) {
        use librqbit::mse::{Mode, set_mode};
        set_mode(match self.encryption.as_str() {
            "require" => Mode::Require,
            "off" => Mode::Off,
            _ => Mode::Prefer,
        });
    }

    fn path() -> PathBuf {
        config_dir().join("settings.json")
    }

    pub fn load() -> Self {
        std::fs::read(Self::path())
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) {
        if DEMO.load(Ordering::Relaxed) {
            return;
        }
        let path = Self::path();
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        // Write then rename, so a crash never leaves half a file.
        let tmp = path.with_extension("json.tmp");
        if let Ok(bytes) = serde_json::to_vec_pretty(self)
            && std::fs::write(&tmp, bytes).is_ok()
        {
            let _ = std::fs::rename(&tmp, &path);
        }
    }
}
