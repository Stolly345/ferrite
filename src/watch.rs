//! Watch folder: every .torrent that appears there is added on its own, then renamed to
//! `<name>.torrent.added` (or `.invalid` when it can't be read) so it is never added twice.

use crate::engine::Source;
use crate::ui::window::Window;
use std::cell::RefCell;
use std::collections::HashSet;
use std::path::PathBuf;
use std::rc::Rc;

thread_local! {
    /// Files being added right now.
    static IN_FLIGHT: RefCell<HashSet<PathBuf>> = RefCell::default();
}

pub fn scan(win: &Rc<Window>) {
    let Some(dir) = win.config.borrow().watch_dir.clone() else { return };
    if win.demo {
        return;
    }
    let Ok(entries) = std::fs::read_dir(&dir) else { return };
    let mut paths: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("torrent")) && p.is_file())
        .filter(|p| IN_FLIGHT.with(|f| f.borrow_mut().insert(p.clone())))
        .collect();
    if paths.is_empty() {
        return;
    }
    paths.sort();
    let win = win.clone();
    // One at a time: the engine can mistake simultaneous adds for duplicates.
    gtk::glib::spawn_future_local(async move {
        for path in paths {
            let result = crate::ui::dialogs::add_without_asking(&win, Source::File(path.clone())).await;
            let suffix = match &result {
                Ok(name) => {
                    win.toast(&format!("Added “{name}” from the watch folder"));
                    "added"
                }
                Err(e) if e.to_string().contains("already") => "added",
                Err(e) => {
                    win.toast(&format!("Couldn't add {}: {e:#}", path.display()));
                    "invalid"
                }
            };
            let mut renamed = path.clone().into_os_string();
            renamed.push(format!(".{suffix}"));
            let _ = std::fs::rename(&path, renamed);
            IN_FLIGHT.with(|f| f.borrow_mut().remove(&path));
        }
    });
}
