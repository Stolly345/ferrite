//! Settings: one page of ruled groups — Appearance, Downloads, Speed, Connection, About.

use super::window::Window;
use crate::config::Theme;
use adw::prelude::*;
use gtk::{gio, glib};
use std::rc::Rc;

fn icon(name: &str) -> gtk::Image {
    gtk::Image::builder().icon_name(name).pixel_size(20).build()
}

fn spin_row(title: &str, subtitle: &str, icon_name: &str, max: f64, value: u32) -> adw::SpinRow {
    let row = adw::SpinRow::with_range(0.0, max, 1.0);
    row.set_title(title);
    row.set_subtitle(subtitle);
    row.set_value(value as f64);
    row.add_prefix(&icon(icon_name));
    row
}

pub fn build(win: &Rc<Window>) -> adw::PreferencesPage {
    let page = adw::PreferencesPage::new();
    page.add_css_class("settings-page");
    let cfg = win.config.borrow().clone();

    // Appearance
    let appearance = adw::PreferencesGroup::builder().title("Appearance").build();
    let theme = adw::ComboRow::builder()
        .title("Theme")
        .subtitle("Follow the system, or stay light or dark")
        .model(&gtk::StringList::new(&["As in the system", "Light", "Dark"]))
        .selected(match cfg.theme {
            Theme::System => 0,
            Theme::Light => 1,
            Theme::Dark => 2,
        })
        .build();
    theme.add_prefix(&icon("weather-clear-night-symbolic"));
    let w = win.clone();
    theme.connect_selected_notify(move |r| {
        w.config.borrow_mut().theme = match r.selected() {
            1 => Theme::Light,
            2 => Theme::Dark,
            _ => Theme::System,
        };
        w.config.borrow().save();
        w.apply_theme();
    });
    appearance.add(&theme);
    page.add(&appearance);

    // Downloads
    let downloads = adw::PreferencesGroup::builder().title("Downloads").build();
    let folder = adw::ActionRow::builder()
        .title("Download folder")
        .subtitle(glib::markup_escape_text(&cfg.download_dir.display().to_string()))
        .build();
    folder.add_prefix(&icon("folder-symbolic"));
    let change = gtk::Button::builder().label("Change…").valign(gtk::Align::Center).css_classes(["secondary"]).build();
    folder.add_suffix(&change);
    let w = win.clone();
    let row = folder.clone();
    change.connect_clicked(move |_| {
        let fd = gtk::FileDialog::builder()
            .title("Download folder")
            .initial_folder(&gio::File::for_path(&w.config.borrow().download_dir))
            .build();
        let w2 = w.clone();
        let row = row.clone();
        fd.select_folder(Some(&w.window), gio::Cancellable::NONE, move |r| {
            if let Ok(f) = r
                && let Some(p) = f.path()
            {
                row.set_subtitle(&glib::markup_escape_text(&p.display().to_string()));
                w2.config.borrow_mut().download_dir = p;
                w2.config.borrow().save();
            }
        });
    });
    downloads.add(&folder);

    let ask = adw::SwitchRow::builder()
        .title("Choose files before adding")
        .subtitle("Pick the folder and the files of each new torrent")
        .active(cfg.ask_before_adding)
        .build();
    ask.add_prefix(&icon("selection-mode-symbolic"));
    let w = win.clone();
    ask.connect_active_notify(move |r| {
        w.config.borrow_mut().ask_before_adding = r.is_active();
        w.config.borrow().save();
    });
    downloads.add(&ask);

    let notify = adw::SwitchRow::builder()
        .title("Finished-download notifications")
        .subtitle("Show a desktop notice with the torrent's name when it completes")
        .active(cfg.notify_finished)
        .build();
    notify.add_prefix(&icon("preferences-system-notifications-symbolic"));
    let w = win.clone();
    notify.connect_active_notify(move |r| {
        w.config.borrow_mut().notify_finished = r.is_active();
        w.config.borrow().save();
    });
    downloads.add(&notify);
    page.add(&downloads);

    // Speed
    let speed = adw::PreferencesGroup::builder().title("Speed").build();
    let down = spin_row("Download limit", "In KB/s. Leave at 0 for unlimited.", "network-receive-symbolic", 1_000_000.0, cfg.down_limit_kbps);
    let up = spin_row("Upload limit", "In KB/s. Leave at 0 for unlimited.", "network-transmit-symbolic", 1_000_000.0, cfg.up_limit_kbps);
    for (row, is_down) in [(&down, true), (&up, false)] {
        let w = win.clone();
        row.connect_value_notify(move |r| {
            let v = r.value() as u32;
            {
                let mut c = w.config.borrow_mut();
                if is_down {
                    c.down_limit_kbps = v;
                } else {
                    c.up_limit_kbps = v;
                }
                c.save();
            }
            let c = w.config.borrow();
            if let Some(e) = w.engine() {
                e.set_limits(c.down_limit_kbps, c.up_limit_kbps);
            }
        });
        speed.add(row);
    }
    page.add(&speed);

    // Connection
    let conn = adw::PreferencesGroup::builder().title("Connection").build();
    let port = spin_row(
        "Listening port",
        "Incoming peer connections arrive on this port. Takes effect after a restart.",
        "network-wired-symbolic",
        65535.0,
        cfg.listen_port as u32,
    );
    port.set_adjustment(Some(&gtk::Adjustment::new(cfg.listen_port as f64, 1024.0, 65535.0, 1.0, 10.0, 0.0)));
    let w = win.clone();
    port.connect_value_notify(move |r| {
        w.config.borrow_mut().listen_port = r.value() as u16;
        w.config.borrow().save();
    });
    conn.add(&port);
    let upnp = adw::SwitchRow::builder()
        .title("Open the port on the router")
        .subtitle("Forward the port automatically with UPnP. Takes effect after a restart.")
        .active(cfg.upnp)
        .build();
    upnp.add_prefix(&icon("network-server-symbolic"));
    let w = win.clone();
    upnp.connect_active_notify(move |r| {
        w.config.borrow_mut().upnp = r.is_active();
        w.config.borrow().save();
    });
    conn.add(&upnp);
    page.add(&conn);

    // About
    let about = adw::PreferencesGroup::builder().title("About").build();
    let row = adw::ActionRow::builder()
        .title(format!("Ferrite {}", env!("CARGO_PKG_VERSION")))
        .subtitle("A precise BitTorrent client for Linux, written in Rust. Engine: librqbit. Apache-2.0.")
        .build();
    row.add_prefix(&icon("help-about-symbolic"));
    about.add(&row);
    page.add(&about);

    page
}

/// Shows only the settings whose title or subtitle contains every word of the query.
pub fn filter(page: &adw::PreferencesPage, query: &str) {
    let words: Vec<String> = query.to_lowercase().split_whitespace().map(String::from).collect();
    let mut group_widget = page.first_child();
    // PreferencesPage > ScrolledWindow > Viewport > Clamp > Box > groups
    fn walk(w: &gtk::Widget, words: &[String]) -> bool {
        if let Some(row) = w.downcast_ref::<adw::PreferencesRow>() {
            let mut text = row.title().to_lowercase();
            if let Some(a) = w.downcast_ref::<adw::ActionRow>()
                && let Some(s) = a.subtitle()
            {
                text.push(' ');
                text.push_str(&s.to_lowercase());
            }
            let show = words.iter().all(|wd| text.contains(wd.as_str()));
            row.set_visible(show);
            return show;
        }
        let mut any = false;
        let mut child = w.first_child();
        while let Some(c) = child {
            any |= walk(&c, words);
            child = c.next_sibling();
        }
        if let Some(g) = w.downcast_ref::<adw::PreferencesGroup>() {
            g.set_visible(any);
        }
        any
    }
    while let Some(w) = group_widget {
        walk(&w, &words);
        group_widget = w.next_sibling();
    }
}
