//! Settings: one page of ruled groups — Appearance, Downloads, Seeding, Speed, Watch folder,
//! Connection, Network, Remote control, Feeds, About.

use super::window::Window;
use crate::config::{Config, Feed, Theme};
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

/// A switch bound to one setting.
fn switch(win: &Rc<Window>, title: &str, subtitle: &str, icon_name: &str, value: bool, set: fn(&mut Config, bool)) -> adw::SwitchRow {
    let row = adw::SwitchRow::builder().title(title).subtitle(subtitle).active(value).build();
    row.add_prefix(&icon(icon_name));
    let w = win.clone();
    row.connect_active_notify(move |r| {
        set(&mut w.config.borrow_mut(), r.is_active());
        w.config.borrow().save();
        w.settings_changed();
    });
    row
}

/// A number bound to one setting.
fn number(win: &Rc<Window>, row: adw::SpinRow, set: fn(&mut Config, u32)) -> adw::SpinRow {
    let w = win.clone();
    row.connect_value_notify(move |r| {
        set(&mut w.config.borrow_mut(), r.value() as u32);
        w.config.borrow().save();
        w.settings_changed();
    });
    row
}

/// A line of text bound to one setting; empty clears it. Applied with the row's check button.
fn text(win: &Rc<Window>, title: &str, value: Option<&str>, set: fn(&mut Config, Option<String>)) -> adw::EntryRow {
    let row = adw::EntryRow::builder().title(title).show_apply_button(true).text(value.unwrap_or("")).build();
    let w = win.clone();
    row.connect_apply(move |r| {
        let t = r.text().trim().to_string();
        set(&mut w.config.borrow_mut(), (!t.is_empty()).then_some(t));
        w.config.borrow().save();
        w.toast("Saved — takes effect after a restart");
    });
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

    downloads.add(&switch(
        win,
        "Keep seeding with the window closed",
        "Ferrite stays in the tray; quit it from there or with Ctrl+Q",
        "user-available-symbolic",
        cfg.run_in_background,
        |c, v| c.run_in_background = v,
    ));
    downloads.add(&number(
        win,
        spin_row(
            "Downloads at a time",
            "The rest wait in # order and start as others finish. Seeding takes no slot. 0 means no limit.",
            "view-list-ordered-symbolic",
            100.0,
            cfg.queue_limit,
        ),
        |c, v| c.queue_limit = v,
    ));
    page.add(&downloads);

    // Seeding goal
    let seeding = adw::PreferencesGroup::builder()
        .title("Seeding goal")
        .description("A finished torrent stops seeding at whichever comes first. Each torrent can have its own in its Options.")
        .build();
    let ratio = adw::SpinRow::with_range(0.0, 100.0, 0.1);
    ratio.set_digits(1);
    ratio.set_title("Share ratio");
    ratio.set_subtitle("Uploaded ÷ size. 0 means no limit.");
    ratio.set_value(cfg.goal.ratio);
    ratio.add_prefix(&icon("view-refresh-symbolic"));
    let w = win.clone();
    ratio.connect_value_notify(move |r| {
        w.config.borrow_mut().goal.ratio = r.value();
        w.config.borrow().save();
    });
    seeding.add(&ratio);
    seeding.add(&number(
        win,
        spin_row("Seeding time", "In minutes. 0 means no limit.", "preferences-system-time-symbolic", 525_600.0, cfg.goal.minutes),
        |c, v| c.goal.minutes = v,
    ));
    page.add(&seeding);

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
            w.settings_changed();
        });
        speed.add(row);
    }
    page.add(&speed);

    // Alternative speed
    let alt = adw::PreferencesGroup::builder()
        .title("Alternative speed")
        .description("Slower limits, switched on with the toolbar's leaf button or on a schedule.")
        .build();
    alt.add(&number(
        win,
        spin_row("Alternative download limit", "In KB/s. 0 means no limit.", "network-receive-symbolic", 1_000_000.0, cfg.alt_down_kbps),
        |c, v| c.alt_down_kbps = v,
    ));
    alt.add(&number(
        win,
        spin_row("Alternative upload limit", "In KB/s. 0 means no limit.", "network-transmit-symbolic", 1_000_000.0, cfg.alt_up_kbps),
        |c, v| c.alt_up_kbps = v,
    ));
    alt.add(&switch(
        win,
        "On a schedule",
        "Use the alternative limits between the hours below",
        "alarm-symbolic",
        cfg.alt_scheduled,
        |c, v| c.alt_scheduled = v,
    ));
    alt.add(&number(
        win,
        spin_row("From", "Hour of the day, 0–23", "preferences-system-time-symbolic", 23.0, cfg.alt_from / 60),
        |c, v| c.alt_from = v * 60,
    ));
    alt.add(&number(
        win,
        spin_row("Until", "Hour of the day, 0–23. Earlier than From runs past midnight.", "preferences-system-time-symbolic", 23.0, cfg.alt_to / 60),
        |c, v| c.alt_to = v * 60,
    ));
    let days_row = adw::ActionRow::builder().title("Days").build();
    days_row.add_prefix(&icon("x-office-calendar-symbolic"));
    let days = gtk::Box::builder().css_classes(["linked"]).valign(gtk::Align::Center).build();
    for (i, d) in ["Mo", "Tu", "We", "Th", "Fr", "Sa", "Su"].iter().enumerate() {
        let b = gtk::ToggleButton::builder().label(*d).active(cfg.alt_days & (1 << i) != 0).build();
        let w = win.clone();
        b.connect_toggled(move |b| {
            let mut c = w.config.borrow_mut();
            if b.is_active() {
                c.alt_days |= 1 << i;
            } else {
                c.alt_days &= !(1 << i);
            }
            c.save();
        });
        days.append(&b);
    }
    days_row.add_suffix(&days);
    alt.add(&days_row);
    page.add(&alt);

    // Watch folder
    let watch = adw::PreferencesGroup::builder().title("Watch folder").build();
    let watch_row = adw::ActionRow::builder()
        .title("Add torrents from a folder")
        .subtitle(watch_subtitle(&cfg))
        .build();
    watch_row.add_prefix(&icon("folder-saved-search-symbolic"));
    let choose = gtk::Button::builder().label("Choose…").valign(gtk::Align::Center).css_classes(["secondary"]).build();
    let clear = gtk::Button::builder()
        .icon_name("edit-clear-symbolic")
        .tooltip_text("Stop watching")
        .valign(gtk::Align::Center)
        .css_classes(["flat"])
        .build();
    watch_row.add_suffix(&choose);
    watch_row.add_suffix(&clear);
    let (w, r) = (win.clone(), watch_row.clone());
    choose.connect_clicked(move |_| {
        let fd = gtk::FileDialog::builder().title("Watch folder").build();
        let (w2, r2) = (w.clone(), r.clone());
        fd.select_folder(Some(&w.window), gio::Cancellable::NONE, move |res| {
            if let Ok(f) = res
                && let Some(p) = f.path()
            {
                w2.config.borrow_mut().watch_dir = Some(p);
                w2.config.borrow().save();
                r2.set_subtitle(&watch_subtitle(&w2.config.borrow()));
            }
        });
    });
    let (w, r) = (win.clone(), watch_row.clone());
    clear.connect_clicked(move |_| {
        w.config.borrow_mut().watch_dir = None;
        w.config.borrow().save();
        r.set_subtitle(&watch_subtitle(&w.config.borrow()));
    });
    watch.add(&watch_row);
    page.add(&watch);

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
        .subtitle("Forward the port automatically with UPnP and NAT-PMP. Takes effect after a restart.")
        .active(cfg.upnp)
        .build();
    upnp.add_prefix(&icon("network-server-symbolic"));
    let w = win.clone();
    upnp.connect_active_notify(move |r| {
        w.config.borrow_mut().upnp = r.is_active();
        w.config.borrow().save();
    });
    conn.add(&upnp);
    conn.add(&number(
        win,
        spin_row("Peers per torrent", "Most peers connected to one torrent. 0 means the engine's default. Takes effect after a restart.", "system-users-symbolic", 2000.0, cfg.peer_limit),
        |c, v| c.peer_limit = v,
    ));
    page.add(&conn);

    // Network
    let net = adw::PreferencesGroup::builder()
        .title("Network")
        .description("Proxy, blocklist and interface take effect after a restart; encryption applies to new connections at once.")
        .build();
    let enc = adw::ComboRow::builder()
        .title("Encryption")
        .subtitle("Hides BitTorrent traffic from providers that slow it down")
        .model(&gtk::StringList::new(&["Prefer encrypted", "Require encrypted", "Off"]))
        .selected(match cfg.encryption.as_str() {
            "require" => 1,
            "off" => 2,
            _ => 0,
        })
        .build();
    enc.add_prefix(&icon("channel-secure-symbolic"));
    let w = win.clone();
    enc.connect_selected_notify(move |r| {
        w.config.borrow_mut().encryption = ["prefer", "require", "off"][r.selected().min(2) as usize].into();
        w.config.borrow().save();
        w.config.borrow().apply_encryption();
    });
    net.add(&enc);
    net.add(&text(win, "Proxy (socks5://user:password@host:port)", cfg.proxy_url.as_deref(), |c, v| c.proxy_url = v));
    net.add(&text(win, "Blocklist URL or file (P2P format)", cfg.blocklist_url.as_deref(), |c, v| c.blocklist_url = v));
    net.add(&text(win, "Network interface only (e.g. a VPN's wg0, tun0)", cfg.bind_device.as_deref(), |c, v| c.bind_device = v));
    let bans = adw::ActionRow::builder()
        .title("Banned peers")
        .subtitle(bans_text(cfg.banned.len()))
        .build();
    bans.add_prefix(&icon("action-unavailable-symbolic"));
    let clear_bans = gtk::Button::builder().label("Clear").valign(gtk::Align::Center).css_classes(["secondary"]).build();
    bans.add_suffix(&clear_bans);
    let (w, r) = (win.clone(), bans.clone());
    clear_bans.connect_clicked(move |_| {
        w.config.borrow_mut().banned.clear();
        w.config.borrow().save();
        if let Some(e) = w.engine() {
            e.clear_bans();
        }
        r.set_subtitle(&bans_text(0));
    });
    net.add(&bans);
    page.add(&net);

    // Remote control
    let remote = adw::PreferencesGroup::builder()
        .title("Remote control")
        .description("A web page and API to manage Ferrite from another device. Takes effect after a restart.")
        .build();
    remote.add(&switch(win, "Allow remote control", "Serve the web interface", "network-workgroup-symbolic", cfg.remote_enabled, |c, v| c.remote_enabled = v));
    remote.add(&switch(win, "From other devices", "Listen on your network, not just this computer", "network-wireless-symbolic", cfg.remote_lan, |c, v| c.remote_lan = v));
    remote.add(&number(
        win,
        spin_row("Port", "The web interface is at http://this-computer:port/web/", "network-wired-symbolic", 65535.0, cfg.remote_port as u32),
        |c, v| c.remote_port = v.clamp(1024, 65535) as u16,
    ));
    let user = adw::EntryRow::builder().title("User name").show_apply_button(true).text(&cfg.remote_user).build();
    let w = win.clone();
    user.connect_apply(move |r| {
        w.config.borrow_mut().remote_user = r.text().trim().to_string();
        w.config.borrow().save();
    });
    remote.add(&user);
    let pass = adw::PasswordEntryRow::builder().title("Password (required)").show_apply_button(true).text(&cfg.remote_password).build();
    let w = win.clone();
    pass.connect_apply(move |r| {
        w.config.borrow_mut().remote_password = r.text().to_string();
        w.config.borrow().save();
    });
    remote.add(&pass);
    let open_web = adw::ActionRow::builder()
        .title("Open the web interface")
        .subtitle("In your browser, on this computer")
        .activatable(true)
        .build();
    open_web.add_prefix(&icon("web-browser-symbolic"));
    open_web.add_suffix(&gtk::Image::from_icon_name("adw-external-link-symbolic"));
    let w = win.clone();
    open_web.connect_activated(move |_| match w.server() {
        Some(s) if s.remote => {
            let launcher = gtk::UriLauncher::new(&format!("http://127.0.0.1:{}/web/", s.port));
            launcher.launch(Some(&w.window), gio::Cancellable::NONE, |_| {});
        }
        _ => w.toast("Remote control is off — turn it on, set a password and restart Ferrite"),
    });
    remote.add(&open_web);
    page.add(&remote);

    // Feeds
    let feeds = adw::PreferencesGroup::builder()
        .title("Feeds")
        .description("Ferrite checks these RSS feeds and adds every new item whose title matches the filter.")
        .build();
    let add_feed = gtk::Button::builder().label("Add feed…").valign(gtk::Align::Center).css_classes(["secondary"]).build();
    feeds.set_header_suffix(Some(&add_feed));
    let list = gtk::ListBox::builder().css_classes(["boxed-list"]).selection_mode(gtk::SelectionMode::None).build();
    fill_feeds(win, &list);
    feeds.add(&list);
    feeds.add(&number(
        win,
        spin_row("Check every", "Minutes between checks", "view-refresh-symbolic", 1440.0, cfg.feed_interval_min),
        |c, v| c.feed_interval_min = v.max(5),
    ));
    let (w, l) = (win.clone(), list.clone());
    add_feed.connect_clicked(move |_| feed_dialog(&w, &l));
    page.add(&feeds);

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

fn bans_text(n: usize) -> String {
    match n {
        0 => "None. Ban a peer from the Peers tab.".into(),
        1 => "1 address".into(),
        n => format!("{n} addresses"),
    }
}

fn watch_subtitle(c: &Config) -> String {
    match &c.watch_dir {
        Some(d) => glib::markup_escape_text(&d.display().to_string()).to_string(),
        None => "Off".into(),
    }
}

fn fill_feeds(win: &Rc<Window>, list: &gtk::ListBox) {
    while let Some(c) = list.first_child() {
        list.remove(&c);
    }
    let feeds = win.config.borrow().feeds.clone();
    if feeds.is_empty() {
        list.append(&adw::ActionRow::builder().title("No feeds").subtitle("Add one to download new episodes or releases as they appear").build());
    }
    for (i, f) in feeds.iter().enumerate() {
        let row = adw::ActionRow::builder()
            .title(glib::markup_escape_text(&f.url))
            .subtitle(if f.filter.is_empty() { "Every item".to_string() } else { format!("Titles matching {}", glib::markup_escape_text(&f.filter)) })
            .build();
        let on = gtk::Switch::builder().active(f.enabled).valign(gtk::Align::Center).build();
        let w = win.clone();
        on.connect_active_notify(move |s| {
            if let Some(f) = w.config.borrow_mut().feeds.get_mut(i) {
                f.enabled = s.is_active();
            }
            w.config.borrow().save();
        });
        let del = gtk::Button::builder().icon_name("user-trash-symbolic").tooltip_text("Remove feed").valign(gtk::Align::Center).css_classes(["flat"]).build();
        let (w, l) = (win.clone(), list.clone());
        del.connect_clicked(move |_| {
            if i < w.config.borrow().feeds.len() {
                w.config.borrow_mut().feeds.remove(i);
            }
            w.config.borrow().save();
            fill_feeds(&w, &l);
        });
        row.add_suffix(&on);
        row.add_suffix(&del);
        list.append(&row);
    }
}

fn feed_dialog(win: &Rc<Window>, list: &gtk::ListBox) {
    let dialog = adw::AlertDialog::new(Some("Add feed"), Some("Items whose title matches the filter are added as they appear. Leave the filter empty to take every item."));
    let fields = gtk::Box::new(gtk::Orientation::Vertical, 8);
    let url = gtk::Entry::builder().placeholder_text("https://example.org/feed.xml").activates_default(true).build();
    let filter = gtk::Entry::builder().placeholder_text("Filter, e.g. 1080p|2160p").activates_default(true).build();
    fields.append(&url);
    fields.append(&filter);
    dialog.set_extra_child(Some(&fields));
    dialog.add_responses(&[("cancel", "Cancel"), ("add", "Add")]);
    dialog.set_response_appearance("add", adw::ResponseAppearance::Suggested);
    dialog.set_default_response(Some("add"));
    dialog.set_close_response("cancel");
    dialog.set_response_enabled("add", false);
    let d = dialog.clone();
    url.connect_changed(move |e| d.set_response_enabled("add", e.text().trim().starts_with("http")));
    let (w, l) = (win.clone(), list.clone());
    dialog.connect_response(None, move |_, r| {
        if r != "add" {
            return;
        }
        let f = filter.text().trim().to_string();
        if !f.is_empty() && regex::Regex::new(&f).is_err() {
            w.toast("That filter isn't a valid pattern");
            return;
        }
        w.config.borrow_mut().feeds.push(Feed { url: url.text().trim().to_string(), filter: f, enabled: true });
        w.config.borrow().save();
        fill_feeds(&w, &l);
        crate::feeds::check_now(&w);
    });
    dialog.present(Some(&win.window));
}
