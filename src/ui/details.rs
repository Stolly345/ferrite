//! The pane below the table: General (piece bar, transfer, info), Files, Peers, Trackers, Speed.

use super::widgets::{PieceBar, SpeedGraph};
use crate::config::TorrentMeta;
use crate::engine::{Details, PeerRow, State, TorrentRow};
use crate::format;
use gtk::prelude::*;
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, VecDeque};
use std::rc::Rc;

type FilesCallback = Box<dyn Fn(usize, Vec<usize>)>;

pub struct DetailsPane {
    pub root: gtk::Box,
    name: gtk::Label,
    tab_buttons: Vec<gtk::ToggleButton>,
    content: gtk::Stack,
    pieces: PieceBar,
    figures: HashMap<&'static str, (gtk::Label, gtk::Label)>,
    values: HashMap<&'static str, gtk::Label>,
    files_grid: RefCell<gtk::Grid>,
    files_scroll: gtk::ScrolledWindow,
    file_widgets: RefCell<Vec<(gtk::CheckButton, gtk::ProgressBar, gtk::Label)>>,
    files_key: RefCell<(usize, usize)>,
    updating_files: Rc<Cell<bool>>,
    on_files: Rc<RefCell<Option<FilesCallback>>>,
    peers_scroll: gtk::ScrolledWindow,
    peer_rows: RefCell<Vec<[gtk::Label; 5]>>,
    trackers_box: gtk::Box,
    trackers_key: RefCell<Vec<String>>,
    graph: SpeedGraph,
    graph_down: gtk::Label,
    graph_up: gtk::Label,
    shown: Cell<Option<usize>>,
}

const TABS: [(&str, &str, &str); 5] = [
    ("general", "General", "dialog-information-symbolic"),
    ("files", "Files", "folder-documents-symbolic"),
    ("peers", "Peers", "system-users-symbolic"),
    ("trackers", "Trackers", "network-server-symbolic"),
    ("speed", "Speed", "network-transmit-receive-symbolic"),
];

fn key(text: &str) -> gtk::Label {
    gtk::Label::builder().label(text).xalign(0.0).css_classes(["key"]).build()
}

fn value() -> gtk::Label {
    gtk::Label::builder()
        .xalign(0.0)
        .selectable(true)
        .ellipsize(gtk::pango::EllipsizeMode::Middle)
        .css_classes(["value"])
        .build()
}

fn heading(text: &str) -> gtk::Label {
    gtk::Label::builder().label(text).xalign(0.0).css_classes(["heading"]).build()
}

fn scrolled(child: &impl IsA<gtk::Widget>) -> gtk::ScrolledWindow {
    gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vexpand(true)
        .child(child)
        .build()
}

fn empty_label(text: &str) -> gtk::Label {
    gtk::Label::builder()
        .label(text)
        .css_classes(["dim-label"])
        .vexpand(true)
        .valign(gtk::Align::Center)
        .build()
}

impl DetailsPane {
    pub fn new() -> Rc<Self> {
        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        root.add_css_class("details-band");

        // Tab strip with the torrent's name on the right.
        let tabs = gtk::Box::new(gtk::Orientation::Horizontal, 4);
        tabs.add_css_class("details-tabs");
        let stack = gtk::Stack::builder()
            .transition_type(gtk::StackTransitionType::Crossfade)
            .transition_duration(120)
            .vexpand(true)
            .build();
        let mut first: Option<gtk::ToggleButton> = None;
        let mut tab_buttons = Vec::new();
        for (id, title, icon) in TABS {
            let b = gtk::Box::new(gtk::Orientation::Horizontal, 8);
            b.append(&gtk::Image::from_icon_name(icon));
            b.append(&gtk::Label::new(Some(title)));
            let btn = gtk::ToggleButton::builder().child(&b).css_classes(["flat", "tab"]).build();
            if let Some(f) = &first {
                btn.set_group(Some(f));
            } else {
                btn.set_active(true);
                first = Some(btn.clone());
            }
            let s = stack.clone();
            btn.connect_toggled(move |b| {
                if b.is_active() {
                    s.set_visible_child_name(id);
                }
            });
            tabs.append(&btn);
            tab_buttons.push(btn);
        }
        let name = gtk::Label::builder()
            .hexpand(true)
            .xalign(1.0)
            .ellipsize(gtk::pango::EllipsizeMode::Middle)
            .css_classes(["details-name"])
            .build();
        tabs.append(&name);
        root.append(&tabs);

        // General
        let general = gtk::Box::new(gtk::Orientation::Vertical, 4);
        general.add_css_class("details-body");

        // The signature: the four numbers this torrent is about, set large, parted by hairlines.
        let figures_row = gtk::Box::builder().css_classes(["figures"]).build();
        let mut figures = HashMap::new();
        for (k, label) in [("pct", "Progress"), ("down", "Download speed"), ("up", "Upload speed"), ("eta", "Time left")] {
            let cell = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(4).css_classes(["figure"]).build();
            cell.append(&gtk::Label::builder().label(label).xalign(0.0).css_classes(["figure-label"]).build());
            let line = gtk::Box::new(gtk::Orientation::Horizontal, 4);
            let value = gtk::Label::builder().xalign(0.0).valign(gtk::Align::Baseline).css_classes(["figure-value"]).build();
            let unit = gtk::Label::builder().xalign(0.0).valign(gtk::Align::Baseline).css_classes(["figure-unit"]).build();
            line.append(&value);
            line.append(&unit);
            cell.append(&line);
            figures_row.append(&cell);
            figures.insert(k, (value, unit));
        }
        general.append(&figures_row);
        let pieces = PieceBar::new();
        pieces.root.set_margin_top(4);
        general.append(&pieces.root);

        let mut values = HashMap::new();
        let mut grid_section = |title: &str, rows: &[&[(&'static str, &'static str)]]| {
            general.append(&heading(title));
            // Label/value pairs packed from the left, four to a row.
            let grid = gtk::Grid::builder().column_spacing(12).row_spacing(4).build();
            for (r, row) in rows.iter().enumerate() {
                for (c, (k, label)) in row.iter().enumerate() {
                    let kl = key(label);
                    kl.set_width_chars(12);
                    let v = value();
                    v.set_width_chars(12);
                    v.set_margin_end(12);
                    if *k != "folder" {
                        v.add_css_class("measure");
                    }
                    let span = if row.len() == 1 { 7 } else { 1 };
                    if span > 1 {
                        v.set_hexpand(true);
                    }
                    grid.attach(&kl, (c * 2) as i32, r as i32, 1, 1);
                    grid.attach(&v, (c * 2 + 1) as i32, r as i32, span, 1);
                    values.insert(*k, v);
                }
            }
            general.append(&grid);
        };
        grid_section(
            "Transfer",
            &[
                &[("downloaded", "Downloaded"), ("uploaded", "Uploaded"), ("remaining", "Remaining"), ("ratio", "Share ratio")],
                &[("peers", "Peers")],
            ],
        );
        grid_section(
            "General",
            &[
                &[("folder", "Saved to")],
                &[("size", "Size"), ("files", "Files"), ("pieces", "Pieces"), ("added", "Added")],
                &[("hash", "Info hash")],
            ],
        );
        stack.add_named(&scrolled(&general), Some("general"));

        // Files
        let files_scroll = scrolled(&gtk::Grid::new());
        stack.add_named(&files_scroll, Some("files"));

        // Peers
        let peers_scroll = scrolled(&gtk::Grid::new());
        stack.add_named(&peers_scroll, Some("peers"));

        // Trackers
        let trackers_box = gtk::Box::new(gtk::Orientation::Vertical, 8);
        trackers_box.add_css_class("details-body");
        stack.add_named(&scrolled(&trackers_box), Some("trackers"));

        // Speed
        let speed = gtk::Box::new(gtk::Orientation::Vertical, 8);
        speed.add_css_class("details-body");
        let legend = gtk::Box::new(gtk::Orientation::Horizontal, 16);
        let graph_down = gtk::Label::builder().css_classes(["value", "measure"]).build();
        let graph_up = gtk::Label::builder().css_classes(["value", "measure"]).build();
        legend.append(&graph_down);
        legend.append(&graph_up);
        for (text, class) in [("Download", "swatch-down"), ("Upload", "swatch-up")] {
            let item = gtk::Box::new(gtk::Orientation::Horizontal, 8);
            let swatch = gtk::Box::builder().valign(gtk::Align::Center).build();
            swatch.add_css_class(class);
            item.append(&swatch);
            item.append(&gtk::Label::builder().label(text).css_classes(["key"]).build());
            legend.append(&item);
        }
        speed.append(&legend);
        let graph = SpeedGraph::new();
        speed.append(&graph.root);
        stack.add_named(&speed, Some("speed"));

        let content = gtk::Stack::new();
        content.add_named(&stack, Some("details"));
        content.add_named(&empty_label("Select a torrent to see its details"), Some("empty"));
        content.set_visible_child_name("empty");
        root.append(&content);

        Rc::new(DetailsPane {
            root,
            name,
            tab_buttons,
            content,
            pieces,
            figures,
            values,
            files_grid: RefCell::new(gtk::Grid::new()),
            files_scroll,
            file_widgets: RefCell::default(),
            files_key: RefCell::new((usize::MAX, 0)),
            updating_files: Rc::default(),
            on_files: Rc::default(),
            peers_scroll,
            peer_rows: RefCell::default(),
            trackers_box,
            trackers_key: RefCell::default(),
            graph,
            graph_down,
            graph_up,
            shown: Cell::new(None),
        })
    }

    /// Called when the user ticks or unticks files: (torrent id, files to download).
    pub fn connect_files_changed(&self, f: impl Fn(usize, Vec<usize>) + 'static) {
        *self.on_files.borrow_mut() = Some(Box::new(f));
    }

    pub fn show_empty(&self, text: &str) {
        self.shown.set(None);
        self.name.set_label("");
        if let Some(l) = self.content.child_by_name("empty").and_downcast::<gtk::Label>() {
            l.set_label(text);
        }
        self.content.set_visible_child_name("empty");
    }

    pub fn update(
        &self,
        row: &TorrentRow,
        details: Option<&Details>,
        history: Option<&VecDeque<(u64, u64)>>,
        meta: Option<&TorrentMeta>,
    ) {
        if self.shown.get() != Some(row.id) {
            self.shown.set(Some(row.id));
            self.content.set_visible_child_name("details");
        }
        self.name.set_label(&row.name);
        self.name.set_tooltip_text(Some(&row.name));

        let set = |k: &str, v: String| {
            if let Some(l) = self.values.get(k)
                && l.label() != v
            {
                l.set_label(&v);
            }
        };
        let known = row.size > 0;
        let figure = |k: &str, text: String| {
            if let Some((value, unit)) = self.figures.get(k) {
                let (v, u) = split_unit(&text);
                if value.label() != v {
                    value.set_label(v);
                }
                if unit.label() != u {
                    unit.set_label(u);
                }
            }
        };
        figure("pct", if known { format::percent(row.done, row.size) } else { "—".into() });
        figure("down", format::speed(row.down_bps));
        figure("up", format::speed(row.up_bps));
        figure("eta", match (row.state, row.eta) {
            (State::Downloading, Some(e)) => format::duration(e),
            (State::Downloading, None) => "∞".into(),
            _ => "—".into(),
        });
        set("downloaded", format::size(row.done));
        set("uploaded", format::size(row.uploaded));
        set("remaining", if known { format::size(row.size.saturating_sub(row.done)) } else { "—".into() });
        set("ratio", format::ratio(row.uploaded, row.done));
        set("peers", format!("{} connected", row.peers));
        set("size", if known { format::size(row.size) } else { "—".into() });
        set("hash", row.hash.clone());
        set("added", meta.filter(|m| m.added > 0).map(|m| format_date(m.added)).unwrap_or_else(|| "—".into()));

        if let Some(d) = details {
            set("folder", d.folder.display().to_string());
            set("files", d.files.len().to_string());
            match &d.pieces {
                Some(p) => {
                    set("pieces", p.len().to_string());
                    self.pieces.set(p.clone(), row.state.css());
                }
                None => {
                    set("pieces", "—".into());
                    // Not live: draw the bar from the overall progress.
                    let n = 200;
                    let have = (row.progress() * n as f64) as usize;
                    self.pieces.set((0..n).map(|i| i < have).collect(), row.state.css());
                }
            }
            self.update_files(row.id, row.state.css(), d);
            self.update_peers(&d.peers);
            self.update_trackers(&d.trackers);
        }

        if let Some(h) = history {
            self.graph.set(h);
        }
        self.graph_down.set_label(&format!("↓ {}", format::speed(row.down_bps)));
        self.graph_up.set_label(&format!("↑ {}", format::speed(row.up_bps)));
    }

    /// Switches to a tab by name ("general", "files", "peers", "trackers", "speed").
    pub fn show_tab(&self, name: &str) {
        if let Some(i) = TABS.iter().position(|t| t.0 == name)
            && let Some(btn) = self.tab_buttons.get(i)
        {
            btn.set_active(true);
        }
    }

    fn update_files(&self, id: usize, state_class: &str, d: &Details) {
        let key = (id, d.files.len());
        if *self.files_key.borrow() != key {
            *self.files_key.borrow_mut() = key;
            let grid = gtk::Grid::builder().column_spacing(16).row_spacing(2).build();
            grid.add_css_class("details-body");
            let mut widgets = Vec::new();
            if d.files.is_empty() {
                self.files_scroll.set_child(Some(&empty_label("The file list arrives with the torrent's metadata")));
                self.file_widgets.borrow_mut().clear();
                return;
            }
            for (r, f) in d.files.iter().enumerate() {
                let check = gtk::CheckButton::builder().active(f.included).tooltip_text("Download this file").build();
                let name = gtk::Label::builder()
                    .label(&f.name)
                    .xalign(0.0)
                    .hexpand(true)
                    .ellipsize(gtk::pango::EllipsizeMode::Middle)
                    .tooltip_text(&f.name)
                    .build();
                let size = gtk::Label::builder().label(format::size(f.size)).xalign(1.0).css_classes(["dim", "value", "measure"]).build();
                let bar = gtk::ProgressBar::builder().width_request(140).valign(gtk::Align::Center).css_classes(["file"]).build();
                let pct = gtk::Label::builder().width_chars(6).xalign(1.0).css_classes(["value", "measure"]).build();
                let row_box = gtk::Box::builder().css_classes(["details-row"]).build();
                row_box.append(&check);
                grid.attach(&row_box, 0, r as i32, 1, 1);
                grid.attach(&name, 1, r as i32, 1, 1);
                grid.attach(&size, 2, r as i32, 1, 1);
                grid.attach(&bar, 3, r as i32, 1, 1);
                grid.attach(&pct, 4, r as i32, 1, 1);
                widgets.push((check, bar, pct));
            }
            // Ticking a box sends the new set of files to the engine.
            let checks: Vec<gtk::CheckButton> = widgets.iter().map(|w| w.0.clone()).collect();
            for c in &checks {
                let checks = checks.clone();
                let updating = self.updating_files.clone();
                let cb = self.on_files.clone();
                c.connect_toggled(move |_| {
                    if updating.get() {
                        return;
                    }
                    let included: Vec<usize> = checks
                        .iter()
                        .enumerate()
                        .filter(|(_, c)| c.is_active())
                        .map(|(i, _)| i)
                        .collect();
                    if let Some(cb) = cb.borrow().as_ref() {
                        cb(id, included);
                    }
                });
            }
            self.files_scroll.set_child(Some(&grid));
            *self.files_grid.borrow_mut() = grid;
            *self.file_widgets.borrow_mut() = widgets;
        }
        self.updating_files.set(true);
        for (f, (check, bar, pct)) in d.files.iter().zip(self.file_widgets.borrow().iter()) {
            let fr = if f.size == 0 { 1.0 } else { f.done as f64 / f.size as f64 };
            bar.set_fraction(fr);
            // A finished file is done whatever the torrent does; the rest follow its state.
            let class = if f.done >= f.size { "seeding" } else { state_class };
            if !bar.has_css_class(class) {
                bar.set_css_classes(&["horizontal", "file", class]);
            }
            pct.set_label(&format::percent(f.done, f.size.max(1)));
            if check.is_active() != f.included {
                check.set_active(f.included);
            }
        }
        self.updating_files.set(false);
    }

    fn update_peers(&self, peers: &[PeerRow]) {
        let mut rows = self.peer_rows.borrow_mut();
        if rows.len() != peers.len() {
            let grid = gtk::Grid::builder().column_spacing(24).row_spacing(2).build();
            grid.add_css_class("details-body");
            rows.clear();
            if peers.is_empty() {
                self.peers_scroll.set_child(Some(&empty_label("No peers connected")));
                return;
            }
            for r in 0..peers.len() {
                let mk = |xalign: f32, expand: bool, dim: bool| {
                    let l = gtk::Label::builder()
                        .xalign(xalign)
                        .hexpand(expand)
                        .ellipsize(gtk::pango::EllipsizeMode::End)
                        .css_classes(if dim { vec!["dim", "value"] } else { vec!["value"] })
                        .build();
                    l.add_css_class("details-row");
                    l
                };
                let cells = [
                    mk(0.0, false, false),
                    mk(0.0, true, true),
                    mk(1.0, false, false),
                    mk(1.0, false, false),
                    mk(0.0, false, true),
                ];
                for i in [0, 2, 3] {
                    cells[i].add_css_class("measure");
                }
                cells[0].set_width_chars(24);
                cells[2].set_width_chars(10);
                cells[3].set_width_chars(10);
                cells[4].set_width_chars(16);
                for (c, l) in cells.iter().enumerate() {
                    grid.attach(l, c as i32, r as i32, 1, 1);
                }
                rows.push(cells);
            }
            self.peers_scroll.set_child(Some(&grid));
        }
        for (p, cells) in peers.iter().zip(rows.iter()) {
            cells[0].set_label(&p.addr);
            cells[1].set_label(&p.client);
            cells[2].set_label(&format::speed(p.down_bps));
            cells[3].set_label(&format::speed(p.up_bps));
            cells[4].set_label(&p.flags);
        }
    }

    fn update_trackers(&self, trackers: &[String]) {
        if *self.trackers_key.borrow() == trackers {
            return;
        }
        *self.trackers_key.borrow_mut() = trackers.to_vec();
        while let Some(c) = self.trackers_box.first_child() {
            self.trackers_box.remove(&c);
        }
        if trackers.is_empty() {
            self.trackers_box.append(&empty_label("No trackers — peers are found through DHT"));
        }
        for t in trackers {
            self.trackers_box.append(
                &gtk::Label::builder().label(t).xalign(0.0).selectable(true).css_classes(["value"]).build(),
            );
        }
    }
}

fn format_date(unix: u64) -> String {
    gtk::glib::DateTime::from_unix_local(unix as i64)
        .ok()
        .and_then(|d| d.format("%-d %b %Y, %H:%M %Z").ok())
        .map(|s| s.to_string())
        .unwrap_or_default()
}

/// "6.66 MB/s" → ("6.66", "MB/s"); "63.5%" → ("63.5", "%"); anything else stays whole.
fn split_unit(text: &str) -> (&str, &str) {
    if let Some(v) = text.strip_suffix('%') {
        return (v, "%");
    }
    match text.split_once(' ') {
        Some((v, u)) if v.parse::<f64>().is_ok() && u.chars().all(|c| c.is_alphabetic() || c == '/') => (v, u),
        _ => (text, ""),
    }
}
