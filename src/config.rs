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
        }
    }
}

pub fn config_dir() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("ferrite")
}

pub fn data_dir() -> PathBuf {
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
