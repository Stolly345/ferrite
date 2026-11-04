//! Per-torrent options (speed limits, seeding goal, trackers), moving data and verifying files.

use super::window::Window;
use crate::config::SeedGoal;
use crate::item::TorrentItem;
use adw::prelude::*;
use gtk::{gio, glib};
use std::rc::Rc;

fn spin(title: &str, subtitle: &str, max: f64, digits: u32, value: f64) -> adw::SpinRow {
    let row = adw::SpinRow::with_range(0.0, max, if digits > 0 { 0.1 } else { 1.0 });
    row.set_digits(digits);
    row.set_title(title);
    row.set_subtitle(subtitle);
    row.set_value(value);
    row
}

/// Options for one torrent, or for several at once (then without trackers).
pub fn show(win: &Rc<Window>, items: Vec<TorrentItem>) {
    let Some(first) = items.first().cloned() else { return };
    let single = items.len() == 1;
    let meta = win.config.borrow().torrents.get(&first.hash()).cloned().unwrap_or_default();
    let global = win.config.borrow().goal;

    let dialog = adw::Dialog::builder()
        .title(if single { "Torrent options".to_string() } else { format!("Options for {} torrents", items.len()) })
        .content_width(520)
        .content_height(620)
        .build();
    let header = adw::HeaderBar::builder().show_end_title_buttons(false).show_start_title_buttons(false).build();
    let cancel = gtk::Button::with_label("Cancel");
    let save = gtk::Button::builder().label("Save").css_classes(["primary"]).build();
    header.pack_start(&cancel);
    header.pack_end(&save);

    let page = adw::PreferencesPage::new();
    if single {
        let g = adw::PreferencesGroup::builder().title(glib::markup_escape_text(&first.name())).build();
        page.add(&g);
    }

    let speed = adw::PreferencesGroup::builder()
        .title("Speed")
        .description("This torrent's own limits; the global limits still apply on top.")
        .build();
    let down = spin("Download limit", "In KB/s. 0 means no limit.", 1_000_000.0, 0, meta.down_limit_kbps as f64);
    let up = spin("Upload limit", "In KB/s. 0 means no limit.", 1_000_000.0, 0, meta.up_limit_kbps as f64);
    speed.add(&down);
    speed.add(&up);
    page.add(&speed);

    let seeding = adw::PreferencesGroup::builder().title("Seeding goal").build();
    let use_global = adw::SwitchRow::builder()
        .title("Use the global goal")
        .subtitle(global_text(global))
        .active(meta.goal.is_none())
        .build();
    let goal = meta.goal.unwrap_or(global);
    let ratio = spin("Share ratio", "0 means no limit", 100.0, 1, goal.ratio);
    let minutes = spin("Seeding time", "In minutes. 0 means no limit.", 525_600.0, 0, goal.minutes as f64);
    for r in [&ratio, &minutes] {
        use_global.bind_property("active", r, "sensitive").invert_boolean().sync_create().build();
    }
    seeding.add(&use_global);
    seeding.add(&ratio);
    seeding.add(&minutes);
    page.add(&seeding);

    // Trackers: one per line, editable for a single torrent whose details are known.
    let trackers_view = gtk::TextView::builder()
        .monospace(true)
        .wrap_mode(gtk::WrapMode::None)
        .top_margin(8)
        .bottom_margin(8)
        .left_margin(8)
        .right_margin(8)
        .build();
    trackers_view.add_css_class("measure");
    let original_trackers: Vec<String> = win
        .engine()
        .filter(|_| single)
        .and_then(|e| e.details(first.id()))
        .map(|d| d.trackers)
        .unwrap_or_default();
    let editable = single && win.engine().is_some_and(|e| e.torrent_info(first.id()).is_some());
    if editable {
        let g = adw::PreferencesGroup::builder()
            .title("Trackers")
            .description("One address per line. Saving checks the files again, which takes a moment.")
            .build();
        trackers_view.buffer().set_text(&original_trackers.join("\n"));
        let frame = gtk::Frame::builder()
            .child(&gtk::ScrolledWindow::builder().child(&trackers_view).min_content_height(120).build())
            .build();
        g.add(&frame);
        page.add(&g);
    }

    let tv = adw::ToolbarView::new();
    tv.add_top_bar(&header);
    tv.set_content(Some(&page));
    dialog.set_child(Some(&tv));

    let d = dialog.clone();
    cancel.connect_clicked(move |_| {
        d.close();
    });
    let (w, d) = (win.clone(), dialog.clone());
    save.connect_clicked(move |_| {
        {
            let mut cfg = w.config.borrow_mut();
            for i in &items {
                let m = cfg.torrents.entry(i.hash()).or_default();
                m.down_limit_kbps = down.value() as u32;
                m.up_limit_kbps = up.value() as u32;
                m.goal = (!use_global.is_active()).then_some(SeedGoal { ratio: ratio.value(), minutes: minutes.value() as u32 });
                m.goal_ignored = false;
            }
            cfg.save();
        }
        w.settings_changed();
        if editable {
            let buf = trackers_view.buffer();
            let text = buf.text(&buf.start_iter(), &buf.end_iter(), false).to_string();
            let trackers: Vec<String> = text.lines().map(str::trim).filter(|l| !l.is_empty()).map(String::from).collect();
            if trackers != original_trackers
                && let Some(engine) = w.engine()
            {
                let (w2, id) = (w.clone(), first.id());
                glib::spawn_future_local(async move {
                    match engine.set_trackers(id, trackers).await {
                        Ok(()) => w2.toast("Trackers saved"),
                        Err(e) => w2.toast(&format!("Couldn't change the trackers: {e:#}")),
                    }
                    w2.refresh();
                });
            }
        }
        d.close();
    });
    dialog.present(Some(&win.window));
}

fn global_text(g: SeedGoal) -> String {
    let ratio = (g.ratio > 0.0).then(|| format!("ratio {:.1}", g.ratio));
    let time = (g.minutes > 0).then(|| format!("{} min", g.minutes));
    match (ratio, time) {
        (None, None) => "No global goal: seed until paused".into(),
        (Some(r), None) => format!("Stop at {r}"),
        (None, Some(t)) => format!("Stop after {t}"),
        (Some(r), Some(t)) => format!("Stop at {r} or after {t}"),
    }
}

/// Moves the selected torrents' files to a folder the user picks.
pub fn move_data(win: &Rc<Window>, items: Vec<TorrentItem>) {
    if items.is_empty() {
        return;
    }
    let fd = gtk::FileDialog::builder().title("Move to").build();
    let w = win.clone();
    fd.select_folder(Some(&win.window), gio::Cancellable::NONE, move |res| {
        let Ok(dir) = res else { return };
        let Some(dest) = dir.path() else { return };
        let Some(engine) = w.engine() else { return };
        let w = w.clone();
        glib::spawn_future_local(async move {
            for i in items {
                w.toast(&format!("Moving “{}”…", i.name()));
                match engine.move_data(i.id(), dest.clone()).await {
                    Ok(()) => w.toast(&format!("Moved “{}”", i.name())),
                    Err(e) => w.toast(&format!("Couldn't move “{}”: {e:#}", i.name())),
                }
                w.refresh();
            }
        });
    });
}

/// Checks every piece of the selected torrents against the files on disk.
pub fn verify(win: &Rc<Window>, items: Vec<TorrentItem>) {
    let Some(engine) = win.engine() else { return };
    let w = win.clone();
    glib::spawn_future_local(async move {
        for i in items {
            if let Err(e) = engine.verify(i.id()).await {
                w.toast(&format!("Couldn't verify “{}”: {e:#}", i.name()));
            }
        }
        w.refresh();
    });
}
