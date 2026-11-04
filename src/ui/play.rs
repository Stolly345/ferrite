//! Play a video or song while it downloads: a finished file opens in its default app; one still
//! downloading streams over Ferrite's local HTTP server to a player that takes links (the default
//! one if it does, else VLC, mpv, Celluloid…). The engine fetches the pieces the player asks for
//! first, so playback starts long before the download ends.

use super::window::Window;
use crate::item::TorrentItem;
use adw::prelude::*;
use gtk::gio;
use std::rc::Rc;

const MEDIA: [&str; 22] = [
    "mkv", "mp4", "m4v", "avi", "webm", "mov", "wmv", "flv", "ts", "m2ts", "mpg", "mpeg", "ogv", "mp3", "flac",
    "ogg", "opus", "m4a", "aac", "wav", "wma", "mka",
];

pub fn is_media(name: &str) -> bool {
    name.rsplit_once('.').is_some_and(|(_, ext)| MEDIA.contains(&ext.to_ascii_lowercase().as_str()))
}

/// The largest media file of a torrent, if any: (index, name, complete).
pub fn main_media(win: &Window, id: usize) -> Option<(usize, String, bool)> {
    let d = win.engine()?.details(id)?;
    d.files
        .iter()
        .filter(|f| is_media(&f.name))
        .max_by_key(|f| f.size)
        .map(|f| (f.index, f.name.clone(), f.size > 0 && f.done >= f.size))
}

pub fn play_torrent(win: &Rc<Window>, item: &TorrentItem) {
    match main_media(win, item.id()) {
        Some((idx, name, done)) => play_file(win, item.id(), idx, &name, done),
        None => win.toast("This torrent has no video or audio file"),
    }
}

pub fn play_file(win: &Rc<Window>, id: usize, idx: usize, name: &str, complete: bool) {
    let Some(engine) = win.engine() else { return };
    if complete && let Some(d) = engine.details(id) {
        let launcher = gtk::FileLauncher::new(Some(&gio::File::for_path(d.folder.join(name))));
        launcher.launch(Some(&win.window), gio::Cancellable::NONE, |_| {});
        return;
    }
    let Some(server) = win.server() else {
        win.toast("Streaming isn't available right now");
        return;
    };
    let url = server.stream_url(id, idx, name);
    match player_for(name) {
        Some(app) => {
            let ctx = gtk::prelude::WidgetExt::display(&win.window).app_launch_context();
            if let Err(e) = app.launch_uris(&[url.as_str()], Some(&ctx)) {
                win.toast(&format!("Couldn't start {}: {e}", app.display_name()));
            }
        }
        None => {
            win.window.clipboard().set_text(&url);
            win.toast("No player that takes links was found — the stream link is on the clipboard");
        }
    }
}

/// The default app for this kind of file if it opens links, else a known streaming player.
fn player_for(name: &str) -> Option<gio::AppInfo> {
    let (content_type, _) = gio::content_type_guess(Some(name), None);
    if let Some(app) = gio::AppInfo::default_for_type(&content_type, false)
        && app.supports_uris()
    {
        return Some(app);
    }
    let known = ["vlc", "org.videolan.VLC", "mpv", "io.mpv.Mpv", "io.github.celluloid_player.Celluloid", "smplayer", "org.gnome.Totem", "org.kde.haruna"];
    let apps = gio::AppInfo::all();
    known.iter().find_map(|k| {
        let id = format!("{k}.desktop");
        apps.iter().find(|a| a.id().is_some_and(|i| i == id) && a.supports_uris()).cloned()
    })
}
