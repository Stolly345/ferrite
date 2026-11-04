//! The main window: title bar with search, side pane (status views, labels, Actions, Settings),
//! the torrent table in a card, and the details pane below it.

use super::details::DetailsPane;
use super::widgets::HISTORY;
use crate::config::{Config, Theme, TorrentMeta};
use crate::engine::{Engine, State, TorrentRow};
use crate::item::TorrentItem;
use adw::prelude::*;
use gtk::{gdk, gio, glib};
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet, VecDeque};
use std::rc::{Rc, Weak};
use std::time::Duration;

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum View {
    All,
    Downloading,
    Completed,
    Paused,
    Label(String),
}

impl View {
    fn key(&self) -> String {
        match self {
            View::All => "all".into(),
            View::Downloading => "downloading".into(),
            View::Completed => "completed".into(),
            View::Paused => "paused".into(),
            View::Label(l) => format!("label:{l}"),
        }
    }

    fn from_key(k: &str) -> Option<View> {
        Some(match k {
            "all" => View::All,
            "downloading" => View::Downloading,
            "completed" => View::Completed,
            "paused" => View::Paused,
            _ => View::Label(k.strip_prefix("label:")?.to_string()),
        })
    }

    fn matches(&self, row: &TorrentRow, label: Option<&str>) -> bool {
        match self {
            View::All => true,
            View::Downloading => matches!(
                row.state,
                State::Downloading | State::GettingFileList | State::Checking
            ),
            View::Completed => row.state.is_complete(),
            View::Paused => matches!(row.state, State::Paused | State::Done),
            View::Label(l) => label == Some(l.as_str()),
        }
    }
}

/// Every word of the query, in any order, ignoring case and accents.
fn search_matches(query: &str, name: &str) -> bool {
    let fold = |s: &str| -> String {
        s.chars()
            .flat_map(|c| c.to_lowercase())
            .map(|c| match c {
                'á' | 'à' | 'â' | 'ä' | 'ã' | 'å' => 'a',
                'é' | 'è' | 'ê' | 'ë' => 'e',
                'í' | 'ì' | 'î' | 'ï' => 'i',
                'ó' | 'ò' | 'ô' | 'ö' | 'õ' => 'o',
                'ú' | 'ù' | 'û' | 'ü' => 'u',
                'ñ' => 'n',
                'ç' => 'c',
                c => c,
            })
            .collect()
    };
    let name = fold(name);
    fold(query).split_whitespace().all(|w| name.contains(w))
}

struct ColumnDef {
    key: &'static str,
    title: &'static str,
    width: i32,
    expand: bool,
    locked: bool,
}

const COLUMNS: [ColumnDef; 10] = [
    ColumnDef { key: "num", title: "#", width: 52, expand: false, locked: false },
    ColumnDef { key: "name", title: "Name", width: 200, expand: true, locked: true },
    ColumnDef { key: "size", title: "Size", width: 84, expand: false, locked: false },
    ColumnDef { key: "progress", title: "Progress", width: 170, expand: true, locked: true },
    ColumnDef { key: "status", title: "Status", width: 136, expand: false, locked: true },
    ColumnDef { key: "down", title: "Down", width: 92, expand: false, locked: false },
    ColumnDef { key: "up", title: "Up", width: 92, expand: false, locked: false },
    ColumnDef { key: "eta", title: "Time left", width: 84, expand: false, locked: false },
    ColumnDef { key: "peers", title: "Peers", width: 64, expand: false, locked: false },
    ColumnDef { key: "ratio", title: "Ratio", width: 64, expand: false, locked: false },
];

/// Columns whose values change while downloading: sorting by them re-sorts every second.
const LIVE_COLUMNS: [&str; 7] = ["progress", "status", "down", "up", "eta", "peers", "ratio"];

type RowMenuCallback = Box<dyn Fn(u32, &gtk::Widget, f64, f64)>;

pub struct Window {
    pub window: adw::ApplicationWindow,
    pub app: adw::Application,
    pub config: RefCell<Config>,
    pub engine: RefCell<Option<Rc<Engine>>>,
    store: gio::ListStore,
    items: RefCell<HashMap<String, TorrentItem>>,
    filter: gtk::CustomFilter,
    filter_state: Rc<RefCell<(View, String)>>,
    selection: gtk::MultiSelection,
    column_view: gtk::ColumnView,
    columns: HashMap<&'static str, gtk::ColumnViewColumn>,
    search: gtk::SearchEntry,
    nav: gtk::ListBox,
    bottom_nav: gtk::ListBox,
    counts: RefCell<HashMap<String, gtk::Label>>,
    page_stack: gtk::Stack,
    table_stack: gtk::Stack,
    nothing_found: adw::StatusPage,
    pub details: Rc<DetailsPane>,
    paned: gtk::Paned,
    pause_btn: gtk::Button,
    selection_buttons: Vec<gtk::Widget>,
    pub toasts: adw::ToastOverlay,
    banner: adw::Banner,
    history: RefCell<HashMap<String, VecDeque<(u64, u64)>>>,
    prev_state: RefCell<HashMap<String, State>>,
    settings_page: RefCell<Option<adw::PreferencesPage>>,
    quitting: Cell<bool>,
    /// Total speeds in the title bar.
    totals: gtk::Label,
    /// Alternative ("slow") speed limits, on by hand.
    alt_btn: gtk::ToggleButton,
    /// The global limits last given to the engine (they change with the schedule).
    applied_limits: Cell<Option<(u32, u32)>>,
    /// Torrents asked to start a moment ago (they hold a queue slot until running).
    starting: RefCell<HashMap<String, std::time::Instant>>,
    ticks: Cell<u64>,
    tray: RefCell<Option<ksni::Handle<crate::tray::FerriteTray>>>,
    /// Keeps the app alive while the window is closed and Ferrite seeds in the background.
    hold: RefCell<Option<gio::ApplicationHoldGuard>>,
    pub demo: bool,
    /// Streaming and remote control.
    server: RefCell<Option<crate::server::Server>>,
    /// Stops the NAT-PMP port mapping (and removes it) on quit.
    natpmp_stop: tokio_util::sync::CancellationToken,
}

impl Window {
    pub fn new(app: &adw::Application, demo: bool) -> Rc<Self> {
        let config = if demo { Config::demo() } else { Config::load() };

        let window = adw::ApplicationWindow::builder()
            .application(app)
            .title("Ferrite")
            .default_width(config.window_width)
            .default_height(config.window_height)
            .width_request(640)
            .height_request(480)
            .build();

        // --- Title bar: sidebar toggle and name on the left, search in the middle.
        let header = adw::HeaderBar::new();
        header.add_css_class("shell");
        let sidebar_toggle = gtk::ToggleButton::builder()
            .icon_name("sidebar-show-symbolic")
            .tooltip_text("Side pane")
            .active(true)
            .build();
        header.pack_start(&sidebar_toggle);
        header.pack_start(&gtk::Label::builder().label("Ferrite").css_classes(["app-title"]).build());
        let search = gtk::SearchEntry::builder()
            .placeholder_text("Search torrents")
            .width_request(480)
            .build();
        header.set_title_widget(Some(&search));
        let totals = gtk::Label::builder()
            .css_classes(["measure", "totals"])
            .tooltip_text("Total download and upload speed")
            .build();
        header.pack_end(&totals);

        // --- Side pane
        let nav = gtk::ListBox::builder().css_classes(["navigation-sidebar"]).build();
        let bottom_nav = gtk::ListBox::builder().css_classes(["navigation-sidebar"]).build();
        let sidebar = gtk::Box::new(gtk::Orientation::Vertical, 0);
        sidebar.add_css_class("sidebar-pane");
        let nav_scroll = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vexpand(true)
            .child(&nav)
            .build();
        sidebar.append(&nav_scroll);
        sidebar.append(&bottom_nav);
        bottom_nav.set_margin_bottom(6);

        // --- Torrent page: toolbar, table card, details pane
        let toolbar = gtk::Box::new(gtk::Orientation::Horizontal, 4);
        toolbar.add_css_class("toolbar-row");
        let tool = |icon: &str, tip: &str, action: &str| {
            let b = gtk::Button::builder()
                .icon_name(icon)
                .tooltip_text(tip)
                .action_name(action)
                .css_classes(["flat"])
                .build();
            b.update_property(&[gtk::accessible::Property::Label(tip)]);
            b
        };
        let sep = || gtk::Separator::new(gtk::Orientation::Vertical);
        // Adding is the page's one action: a labelled primary, the magnet link beside it.
        let labelled = |icon: &str, label: &str, tip: &str, action: &str, class: &str| {
            gtk::Button::builder()
                .child(&adw::ButtonContent::builder().icon_name(icon).label(label).build())
                .tooltip_text(tip)
                .action_name(action)
                .css_classes([class])
                .build()
        };
        toolbar.append(&labelled("list-add-symbolic", "Open torrent file", "Open torrent file (Ctrl+O)", "win.add-file", "primary"));
        toolbar.append(&labelled("insert-link-symbolic", "Add magnet link", "Add magnet link (Ctrl+N)", "win.add-magnet", "secondary"));
        toolbar.append(&sep());
        let pause_btn = tool("media-playback-pause-symbolic", "Pause", "win.toggle-pause");
        let open_btn = tool("folder-open-symbolic", "Open folder", "win.open-folder");
        let remove_btn = tool("user-trash-symbolic", "Remove (Delete)", "win.remove");
        toolbar.append(&pause_btn);
        toolbar.append(&open_btn);
        toolbar.append(&remove_btn);
        toolbar.append(&sep());
        let up_btn = tool("go-up-symbolic", "Move up (Alt+Up)", "win.move-up");
        let down_btn = tool("go-down-symbolic", "Move down (Alt+Down)", "win.move-down");
        toolbar.append(&up_btn);
        toolbar.append(&down_btn);
        toolbar.append(&sep());
        let alt_btn = gtk::ToggleButton::builder()
            .icon_name("power-profile-power-saver-symbolic")
            .tooltip_text("Alternative speed limits")
            .css_classes(["flat"])
            .build();
        alt_btn.update_property(&[gtk::accessible::Property::Label("Alternative speed limits")]);
        toolbar.append(&alt_btn);
        let more_menu = gio::Menu::new();
        let order = gio::Menu::new();
        order.append(Some("Move to top"), Some("win.move-top"));
        order.append(Some("Move to bottom"), Some("win.move-bottom"));
        more_menu.append_section(None, &order);
        let all = gio::Menu::new();
        all.append(Some("Select all"), Some("win.select-all"));
        all.append(Some("Copy magnet link"), Some("win.copy-magnet"));
        let make = gio::Menu::new();
        make.append(Some("Create torrent…"), Some("win.create-torrent"));
        more_menu.append_section(None, &make);
        more_menu.append_section(None, &all);
        let more = gtk::MenuButton::builder()
            .icon_name("view-more-horizontal-symbolic")
            .tooltip_text("More")
            .menu_model(&more_menu)
            .css_classes(["flat"])
            .build();
        toolbar.append(&more);

        let filter_state = Rc::new(RefCell::new((View::All, String::new())));
        let store = gio::ListStore::new::<TorrentItem>();
        let fs = filter_state.clone();
        let filter = gtk::CustomFilter::new(move |obj| {
            let item = obj.downcast_ref::<TorrentItem>().unwrap();
            let (view, query) = &*fs.borrow();
            let label = item.label();
            item.with_row(|r| view.matches(r, label.as_deref()) && search_matches(query, &r.name))
        });
        let filter_model = gtk::FilterListModel::new(Some(store.clone()), Some(filter.clone()));
        let column_view = gtk::ColumnView::builder()
            .css_classes(["torrents"])
            .reorderable(false)
            .show_column_separators(false)
            .show_row_separators(false)
            .build();
        let sort_model = gtk::SortListModel::new(Some(filter_model), column_view.sorter());
        let selection = gtk::MultiSelection::new(Some(sort_model));
        column_view.set_model(Some(&selection));

        let row_menu: Rc<RefCell<Option<RowMenuCallback>>> = Rc::default();
        let mut columns = HashMap::new();
        for def in &COLUMNS {
            let col = build_column(def, row_menu.clone());
            col.set_visible(def.locked || config.visible_columns.iter().any(|c| c == def.key));
            column_view.append_column(&col);
            columns.insert(def.key, col);
        }
        column_view.sort_by_column(Some(&columns["num"]), gtk::SortType::Ascending);

        // A classic scrollbar keeps its own lane, so it never sits on the last column's figures.
        let table_scroll = gtk::ScrolledWindow::builder()
            .overlay_scrolling(false)
            .child(&column_view)
            .vexpand(true)
            .build();
        let table_stack = gtk::Stack::new();
        table_stack.add_named(&table_scroll, Some("table"));
        let empty = adw::StatusPage::builder()
            .icon_name("folder-download-symbolic")
            .title("No torrents yet")
            .description("Open a .torrent file or add a magnet link — or drop one here.")
            .build();
        let empty_buttons = gtk::Box::builder().spacing(12).halign(gtk::Align::Center).build();
        empty_buttons.append(
            &gtk::Button::builder()
                .label("Open torrent file…")
                .action_name("win.add-file")
                .css_classes(["secondary"])
                .build(),
        );
        empty_buttons.append(
            &gtk::Button::builder()
                .label("Add magnet link…")
                .action_name("win.add-magnet")
                .css_classes(["secondary"])
                .build(),
        );
        empty.set_child(Some(&empty_buttons));
        empty.add_css_class("compact");
        table_stack.add_named(&empty, Some("empty"));
        let nothing_found = adw::StatusPage::builder()
            .icon_name("edit-find-symbolic")
            .title("Nothing found")
            .build();
        nothing_found.add_css_class("compact");
        table_stack.add_named(&nothing_found, Some("nothing"));
        let starting = adw::StatusPage::builder()
            .title("Starting…")
            .child(&adw::Spinner::builder().width_request(32).height_request(32).build())
            .build();
        starting.add_css_class("compact");
        table_stack.add_named(&starting, Some("starting"));
        table_stack.set_visible_child_name("starting");

        // The table sits on the page, ruled by hairlines; no card around it.
        let table_card = gtk::Box::new(gtk::Orientation::Vertical, 0);
        table_card.add_css_class("table-region");
        table_card.append(&table_stack);

        let details = DetailsPane::new();
        let paned = gtk::Paned::builder()
            .orientation(gtk::Orientation::Vertical)
            .start_child(&table_card)
            .end_child(&details.root)
            .resize_start_child(true)
            .resize_end_child(false)
            .shrink_end_child(false)
            .wide_handle(false)
            .vexpand(true)
            .css_classes(["torrent-split"])
            .build();
        details.root.set_size_request(-1, 200);
        table_card.set_size_request(-1, 160);

        let banner = adw::Banner::new("");
        let torrent_page = gtk::Box::new(gtk::Orientation::Vertical, 0);
        torrent_page.append(&banner);
        torrent_page.append(&toolbar);
        torrent_page.append(&paned);

        let page_stack = gtk::Stack::builder()
            .transition_type(gtk::StackTransitionType::Crossfade)
            .transition_duration(120)
            .build();
        page_stack.add_named(&torrent_page, Some("torrents"));
        page_stack.add_css_class("content-layer");

        let split = adw::OverlaySplitView::builder()
            .sidebar(&sidebar)
            .content(&page_stack)
            .min_sidebar_width(220.0)
            .max_sidebar_width(300.0)
            .sidebar_width_fraction(0.2)
            .build();
        sidebar_toggle
            .bind_property("active", &split, "show-sidebar")
            .bidirectional()
            .sync_create()
            .build();

        let toasts = adw::ToastOverlay::new();
        toasts.set_child(Some(&split));
        let toolbar_view = adw::ToolbarView::new();
        toolbar_view.add_top_bar(&header);
        toolbar_view.set_content(Some(&toasts));
        window.set_content(Some(&toolbar_view));

        // Collapse the side pane on narrow windows.
        let bp = adw::Breakpoint::new(adw::BreakpointCondition::new_length(
            adw::BreakpointConditionLengthType::MaxWidth,
            860.0,
            adw::LengthUnit::Sp,
        ));
        bp.add_setter(&split, "collapsed", Some(&true.to_value()));
        bp.add_setter(&search, "width-request", Some(&240.to_value()));
        window.add_breakpoint(bp);

        let win = Rc::new(Window {
            window,
            app: app.clone(),
            config: RefCell::new(config),
            engine: RefCell::new(None),
            store,
            items: RefCell::default(),
            filter,
            filter_state,
            selection,
            column_view,
            columns,
            search,
            nav,
            bottom_nav,
            counts: RefCell::default(),
            page_stack,
            table_stack,
            nothing_found,
            details,
            paned,
            pause_btn,
            selection_buttons: vec![open_btn.upcast(), remove_btn.upcast(), up_btn.upcast(), down_btn.upcast()],
            toasts,
            banner,
            history: RefCell::default(),
            prev_state: RefCell::default(),
            settings_page: RefCell::default(),
            quitting: Cell::new(false),
            totals,
            alt_btn,
            applied_limits: Cell::new(None),
            starting: RefCell::default(),
            ticks: Cell::new(0),
            tray: RefCell::default(),
            hold: RefCell::default(),
            demo,
            server: RefCell::default(),
            natpmp_stop: Default::default(),
        });

        let weak = Rc::downgrade(&win);
        *row_menu.borrow_mut() = Some(Box::new(move |pos, widget, x, y| {
            if let Some(w) = weak.upgrade() {
                w.show_row_menu(pos, widget, x, y);
            }
        }));

        win.build_sidebar();
        win.setup_actions();
        win.setup_signals();
        win.apply_theme();
        let settings = super::settings::build(&win);
        win.page_stack.add_named(&settings, Some("settings"));
        *win.settings_page.borrow_mut() = Some(settings);
        win.update_toolbar();
        win
    }

    fn weak(self: &Rc<Self>) -> Weak<Self> {
        Rc::downgrade(self)
    }

    pub fn engine(&self) -> Option<Rc<Engine>> {
        self.engine.borrow().clone()
    }

    pub fn toast(&self, text: &str) {
        self.toasts.add_toast(adw::Toast::builder().title(text).timeout(4).build());
    }

    pub fn apply_theme(&self) {
        let scheme = match self.config.borrow().theme {
            Theme::System => adw::ColorScheme::Default,
            Theme::Light => adw::ColorScheme::ForceLight,
            Theme::Dark => adw::ColorScheme::ForceDark,
        };
        adw::StyleManager::default().set_color_scheme(scheme);
    }

    // ---- Engine lifecycle -------------------------------------------------------------------

    pub fn set_engine(self: &Rc<Self>, engine: Engine) {
        // The demo opens with two minutes of simulated history, so the speed graph has a shape.
        if engine.is_demo() {
            let mut history = self.history.borrow_mut();
            for _ in 0..HISTORY {
                for row in engine.list() {
                    let h = history.entry(row.hash).or_default();
                    h.push_back((row.down_bps, row.up_bps));
                    if h.len() > HISTORY {
                        h.pop_front();
                    }
                }
            }
        }
        let rt = engine.api().map(|(_, rt)| rt);
        *self.engine.borrow_mut() = Some(Rc::new(engine));
        self.refresh();
        let weak = self.weak();
        glib::timeout_add_local(Duration::from_secs(1), move || match weak.upgrade() {
            Some(w) if !w.quitting.get() => {
                w.tick();
                glib::ControlFlow::Continue
            }
            _ => glib::ControlFlow::Break,
        });
        if let Some(rt) = rt {
            self.start_tray(rt.clone());
            self.start_server();
            if self.config.borrow().upnp
                && let Some(port) = self.engine().and_then(|e| e.listen_port())
            {
                let stop = self.natpmp_stop.clone();
                rt.spawn(async move {
                    // Routers without NAT-PMP simply don't answer; UPnP (in the engine) covers those.
                    if let Err(e) = crate::natpmp::keep_mapped(port, stop).await {
                        eprintln!("NAT-PMP: {e:#}");
                    }
                });
            }
            if let Some(e) = self.engine() {
                for ip in self.config.borrow().banned.iter().filter_map(|s| s.parse().ok()) {
                    e.ban(ip);
                }
            }
        }
    }

    pub fn server(&self) -> Option<crate::server::Server> {
        self.server.borrow().clone()
    }

    fn start_server(self: &Rc<Self>) {
        let Some((api, rt)) = self.engine().and_then(|e| e.api()) else { return };
        let cfg = self.config.borrow().clone();
        if cfg.remote_enabled && cfg.remote_password.is_empty() {
            self.toast("Remote control needs a password — set one in Settings");
        }
        let weak = self.weak();
        glib::spawn_future_local(async move {
            let result = crate::server::start(api, rt, &cfg).await;
            let Some(w) = weak.upgrade() else { return };
            match result {
                Ok(server) => *w.server.borrow_mut() = Some(server),
                Err(e) => w.toast(&format!("Remote control and streaming are off: {e:#}")),
            }
        });
    }

    /// The tray icon: brings the window back, pauses or resumes everything, quits.
    fn start_tray(self: &Rc<Self>, rt: tokio::runtime::Handle) {
        let (tx, rx) = async_channel::bounded(16);
        let weak = self.weak();
        glib::spawn_future_local(async move {
            let handle = crate::tray::start(rt, tx).await;
            if let Some(w) = weak.upgrade() {
                *w.tray.borrow_mut() = handle;
            }
            while let Ok(event) = rx.recv().await {
                let Some(w) = weak.upgrade() else { break };
                use crate::tray::TrayEvent;
                match event {
                    TrayEvent::Show => w.show(),
                    TrayEvent::PauseAll => w.set_paused(w.all_items(), true),
                    TrayEvent::ResumeAll => w.resume_all(),
                    TrayEvent::Quit => w.quit(),
                }
            }
        });
    }

    /// A setting changed: let the next rules pass re-apply limits.
    pub fn settings_changed(self: &Rc<Self>) {
        self.applied_limits.set(None);
        self.apply_rules();
    }

    pub fn show(&self) {
        self.window.set_visible(true);
        self.window.present();
    }

    /// Once a second: refresh the list, then apply the queue, seeding goals and speed schedule.
    fn tick(self: &Rc<Self>) {
        self.ticks.set(self.ticks.get() + 1);
        self.refresh();
        self.apply_rules();
        if self.ticks.get().is_multiple_of(5) {
            crate::watch::scan(self);
        }
        crate::feeds::tick(self);
    }

    pub fn show_engine_warning(&self, text: &str) {
        self.banner.set_title(text);
        self.banner.set_revealed(true);
    }

    pub fn show_engine_error(&self, err: &anyhow::Error) {
        self.banner.set_title(&format!("The torrent engine couldn't start: {err:#}"));
        self.banner.set_revealed(true);
        self.table_stack.set_visible_child_name("empty");
    }

    // ---- Side pane ----------------------------------------------------------------------------

    fn nav_row(&self, key: &str, icon: Option<&str>, dot_class: Option<String>, title: &str) -> gtk::ListBoxRow {
        let b = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        if let Some(icon) = icon {
            b.append(&gtk::Image::from_icon_name(icon));
        }
        if let Some(class) = dot_class {
            let dot = gtk::Box::builder().valign(gtk::Align::Center).build();
            dot.add_css_class("label-dot");
            dot.add_css_class(&class);
            b.append(&dot);
        }
        b.append(&gtk::Label::builder().label(title).xalign(0.0).hexpand(true).ellipsize(gtk::pango::EllipsizeMode::End).build());
        let count = gtk::Label::builder().css_classes(["count"]).build();
        b.append(&count);
        self.counts.borrow_mut().insert(key.to_string(), count);
        let row = gtk::ListBoxRow::builder().child(&b).name(key).build();
        row.update_property(&[gtk::accessible::Property::Label(title)]);
        row
    }

    pub fn build_sidebar(self: &Rc<Self>) {
        while let Some(c) = self.nav.first_child() {
            self.nav.remove(&c);
        }
        self.counts.borrow_mut().clear();
        for (key, icon, title) in [
            ("all", "view-grid-symbolic", "All torrents"),
            ("downloading", "folder-download-symbolic", "Downloading"),
            ("completed", "object-select-symbolic", "Completed"),
            ("paused", "media-playback-pause-symbolic", "Paused"),
        ] {
            self.nav.append(&self.nav_row(key, Some(icon), None, title));
        }
        let labels = self.config.borrow().labels.clone();
        let header = gtk::ListBoxRow::builder()
            .selectable(false)
            .activatable(false)
            .child(&gtk::Label::builder().label("Labels").xalign(0.0).css_classes(["section-title"]).build())
            .build();
        self.nav.append(&header);
        if labels.is_empty() {
            let hint = gtk::Label::builder()
                .label("Right-click a torrent to give it a label")
                .wrap(true)
                .xalign(0.0)
                .css_classes(["hint", "caption"])
                .margin_start(20)
                .margin_end(12)
                .build();
            self.nav.append(&gtk::ListBoxRow::builder().selectable(false).activatable(false).child(&hint).build());
        }
        for l in &labels {
            let key = format!("label:{}", l.name);
            let row = self.nav_row(&key, None, Some(format!("label-color-{}", l.color)), &l.name);
            // Right-click a label to remove it.
            let click = gtk::GestureClick::builder().button(3).build();
            let weak = self.weak();
            let name = l.name.clone();
            let row_w = row.clone();
            click.connect_pressed(move |_, _, x, y| {
                if let Some(w) = weak.upgrade() {
                    let menu = gio::Menu::new();
                    menu.append(Some("Remove label"), Some(&format!("win.delete-label::{name}")));
                    let pop = gtk::PopoverMenu::from_model(Some(&menu));
                    pop.set_parent(&row_w);
                    pop.set_pointing_to(Some(&gdk::Rectangle::new(x as i32, y as i32, 1, 1)));
                    pop.set_has_arrow(false);
                    pop.connect_closed(|p| {
                        let p = p.clone();
                        glib::idle_add_local_once(move || p.unparent());
                    });
                    pop.popup();
                    let _ = w;
                }
            });
            row.add_controller(click);
            self.nav.append(&row);
        }

        // Bottom: Actions (a menu) and Settings.
        while let Some(c) = self.bottom_nav.first_child() {
            self.bottom_nav.remove(&c);
        }
        let actions_menu = gio::Menu::new();
        actions_menu.append(Some("Resume all"), Some("win.resume-all"));
        actions_menu.append(Some("Pause all"), Some("win.pause-all"));
        let actions_box = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        actions_box.append(&gtk::Image::from_icon_name("power-profile-performance-symbolic"));
        actions_box.append(&gtk::Label::builder().label("Actions").xalign(0.0).hexpand(true).build());
        let actions_row = gtk::ListBoxRow::builder().child(&actions_box).selectable(false).name("actions").build();
        let pop = gtk::PopoverMenu::from_model(Some(&actions_menu));
        pop.set_parent(&actions_box);
        pop.set_position(gtk::PositionType::Top);
        self.bottom_nav.append(&actions_row);
        let settings_box = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        settings_box.append(&gtk::Image::from_icon_name("emblem-system-symbolic"));
        settings_box.append(&gtk::Label::builder().label("Settings").xalign(0.0).hexpand(true).build());
        self.bottom_nav.append(&gtk::ListBoxRow::builder().child(&settings_box).name("settings").build());
        self.bottom_nav.connect_row_activated(move |_, row| {
            if row.widget_name() == "actions" {
                pop.popup();
            }
        });

        // Reselect the current view.
        let key = self.filter_state.borrow().0.key();
        let mut child = self.nav.first_child();
        while let Some(c) = child {
            if let Some(row) = c.downcast_ref::<gtk::ListBoxRow>()
                && row.widget_name() == key
            {
                self.nav.select_row(Some(row));
            }
            child = c.next_sibling();
        }
        if self.nav.selected_row().is_none() && self.page_stack.visible_child_name().as_deref() != Some("settings") {
            self.filter_state.borrow_mut().0 = View::All;
            self.nav.select_row(self.nav.row_at_index(0).as_ref());
        }
        self.update_counts();
    }

    fn set_view(self: &Rc<Self>, view: View) {
        self.page_stack.set_visible_child_name("torrents");
        self.search.set_placeholder_text(Some("Search torrents"));
        if self.filter_state.borrow().0 != view {
            self.filter_state.borrow_mut().0 = view;
            self.filter.changed(gtk::FilterChange::Different);
            self.update_table_state();
        }
    }

    fn show_settings(&self) {
        self.page_stack.set_visible_child_name("settings");
        self.search.set_placeholder_text(Some("Search settings"));
        self.nav.unselect_all();
    }

    fn update_counts(&self) {
        let items = self.items.borrow();
        let mut counts: HashMap<String, usize> = HashMap::new();
        for item in items.values() {
            let label = item.label();
            item.with_row(|r| {
                for v in [View::All, View::Downloading, View::Completed, View::Paused] {
                    if v.matches(r, None) {
                        *counts.entry(v.key()).or_default() += 1;
                    }
                }
            });
            if let Some(l) = label {
                *counts.entry(format!("label:{l}")).or_default() += 1;
            }
        }
        for (k, label) in self.counts.borrow().iter() {
            let n = counts.get(k).copied().unwrap_or(0);
            label.set_label(&if n > 0 { n.to_string() } else { String::new() });
        }
    }

    // ---- Signals & actions --------------------------------------------------------------------

    fn setup_signals(self: &Rc<Self>) {
        let weak = self.weak();
        self.nav.connect_row_selected(move |_, row| {
            let (Some(w), Some(row)) = (weak.upgrade(), row) else { return };
            if let Some(view) = View::from_key(&row.widget_name()) {
                w.bottom_nav.unselect_all();
                w.set_view(view);
            }
        });
        let weak = self.weak();
        self.bottom_nav.connect_row_selected(move |_, row| {
            let (Some(w), Some(row)) = (weak.upgrade(), row) else { return };
            if row.widget_name() == "settings" {
                w.show_settings();
            }
        });

        let weak = self.weak();
        self.search.connect_search_changed(move |s| {
            let Some(w) = weak.upgrade() else { return };
            let text = s.text().to_string();
            if w.page_stack.visible_child_name().as_deref() == Some("settings") {
                if let Some(page) = w.settings_page.borrow().as_ref() {
                    super::settings::filter(page, &text);
                }
                return;
            }
            w.filter_state.borrow_mut().1 = text;
            w.filter.changed(gtk::FilterChange::Different);
            w.update_table_state();
        });
        let weak = self.weak();
        self.search.connect_stop_search(move |s| {
            s.set_text("");
            if let Some(w) = weak.upgrade() {
                w.column_view.grab_focus();
            }
        });

        let weak = self.weak();
        self.selection.connect_selection_changed(move |_, _, _| {
            if let Some(w) = weak.upgrade() {
                w.update_toolbar();
                w.update_details();
            }
        });
        let weak = self.weak();
        self.selection.connect_items_changed(move |_, _, _, _| {
            if let Some(w) = weak.upgrade() {
                w.update_toolbar();
            }
        });

        let weak = self.weak();
        self.details.connect_priority(move |id, idx, prio| {
            let Some(w) = weak.upgrade() else { return };
            let Some(item) = w.items.borrow().values().find(|i| i.id() == id).cloned() else { return };
            {
                let mut cfg = w.config.borrow_mut();
                let meta = cfg.torrents.entry(item.hash()).or_default();
                if meta.priorities.len() <= idx {
                    meta.priorities.resize(idx + 1, 1);
                }
                meta.priorities[idx] = prio;
                cfg.save();
            }
            w.apply_rules();
        });
        let weak = self.weak();
        self.details.connect_ban(move |addr| {
            let Some(w) = weak.upgrade() else { return };
            let Ok(sock) = addr.parse::<std::net::SocketAddr>() else { return };
            let ip = sock.ip();
            if let Some(e) = w.engine() {
                e.ban(ip);
            }
            {
                let mut cfg = w.config.borrow_mut();
                if !cfg.banned.contains(&ip.to_string()) {
                    cfg.banned.push(ip.to_string());
                }
                cfg.save();
            }
            w.toast(&format!("Banned {ip} — clear bans in Settings"));
            w.refresh();
        });
        let weak = self.weak();
        self.details.connect_play(move |id, idx, name| {
            let Some(w) = weak.upgrade() else { return };
            let complete = w
                .engine()
                .and_then(|e| e.details(id))
                .and_then(|d| d.files.iter().find(|f| f.index == idx).map(|f| f.size > 0 && f.done >= f.size))
                .unwrap_or(false);
            super::play::play_file(&w, id, idx, &name, complete);
        });
        let weak = self.weak();
        self.details.connect_files_changed(move |id, files| {
            let Some(w) = weak.upgrade() else { return };
            let Some(engine) = w.engine() else { return };
            let weak = w.weak();
            glib::spawn_future_local(async move {
                if files.is_empty() {
                    if let Some(w) = weak.upgrade() {
                        w.toast("Choose at least one file to download");
                    }
                    return;
                }
                if let Err(e) = engine.set_files(id, files).await
                    && let Some(w) = weak.upgrade()
                {
                    w.toast(&format!("Couldn't change the files: {e}"));
                }
            });
        });

        // Keyboard: Delete and Alt+arrows act on the table, not while typing in search.
        let keys = gtk::ShortcutController::new();
        for (accel, action) in [
            ("Delete", "win.remove"),
            ("<Alt>Up", "win.move-up"),
            ("<Alt>Down", "win.move-down"),
            ("Escape", "win.clear-selection"),
        ] {
            keys.add_shortcut(gtk::Shortcut::new(
                gtk::ShortcutTrigger::parse_string(accel),
                Some(gtk::NamedAction::new(action)),
            ));
        }
        self.column_view.add_controller(keys);

        // Drag and drop: .torrent files and magnet links.
        let drop = gtk::DropTarget::new(glib::Type::INVALID, gdk::DragAction::COPY);
        drop.set_types(&[gdk::FileList::static_type(), String::static_type()]);
        let weak = self.weak();
        drop.connect_drop(move |_, value, _, _| {
            let Some(w) = weak.upgrade() else { return false };
            let mut sources = Vec::new();
            if let Ok(files) = value.get::<gdk::FileList>() {
                for f in files.files() {
                    if let Some(p) = f.path() {
                        sources.push(crate::engine::Source::File(p));
                    }
                }
            } else if let Ok(text) = value.get::<String>() {
                for line in text.lines().map(str::trim).filter(|l| l.starts_with("magnet:")) {
                    sources.push(crate::engine::Source::Url(line.to_string()));
                }
            }
            if sources.is_empty() {
                return false;
            }
            super::dialogs::add_sources(&w, sources);
            true
        });
        self.window.add_controller(drop);

        // Closing the window keeps Ferrite seeding in the background (Settings can turn that off).
        let weak = self.weak();
        self.window.connect_close_request(move |win| {
            let Some(w) = weak.upgrade() else { return glib::Propagation::Proceed };
            if w.quitting.get() {
                return glib::Propagation::Proceed;
            }
            w.remember_geometry();
            let background = w.config.borrow().run_in_background && !w.demo && w.engine().is_some();
            if !background {
                w.quit();
                return glib::Propagation::Stop;
            }
            win.set_visible(false);
            if w.hold.borrow().is_none() {
                *w.hold.borrow_mut() = Some(w.app.hold());
            }
            if !w.config.borrow().background_hint_shown {
                w.config.borrow_mut().background_hint_shown = true;
                w.config.borrow().save();
                let n = gio::Notification::new("Ferrite is still seeding");
                n.set_body(Some("It keeps running in the background. Open it again from the tray icon, or quit there."));
                n.set_icon(&gio::ThemedIcon::new(crate::tray::APP_ICON));
                w.app.send_notification(Some("background"), &n);
            }
            glib::Propagation::Stop
        });

        // Place the splitter once the window has its size.
        let weak = self.weak();
        self.paned.connect_realize(move |p| {
            let Some(w) = weak.upgrade() else { return };
            let wanted = w.config.borrow().details_height;
            let p = p.clone();
            glib::idle_add_local_once(move || {
                let h = p.height();
                if h > 0 {
                    p.set_position((h - wanted).max(160));
                }
            });
        });
    }

    fn remember_geometry(&self) {
        let mut c = self.config.borrow_mut();
        let (width, height) = self.window.default_size();
        c.window_width = width;
        c.window_height = height;
        c.details_height = (self.paned.height() - self.paned.position()).max(200);
        c.save();
    }

    /// Stops the engine (giving it a moment to save resume data) and quits.
    pub fn quit(self: &Rc<Self>) {
        if self.quitting.replace(true) {
            return;
        }
        if self.window.is_visible() {
            self.remember_geometry();
        }
        self.config.borrow().save();
        self.window.set_visible(false);
        let engine = self.engine();
        let app = self.app.clone();
        self.natpmp_stop.cancel();
        self.hold.borrow_mut().take();
        glib::spawn_future_local(async move {
            if let Some(e) = engine {
                let stop = e.stop();
                let timeout = glib::timeout_future(Duration::from_secs(3));
                futures_select(stop, timeout).await;
            }
            app.quit();
        });
    }

    fn setup_actions(self: &Rc<Self>) {
        let add = |name: &str, f: Box<dyn Fn(&Rc<Window>)>| {
            let action = gio::SimpleAction::new(name, None);
            let weak = self.weak();
            action.connect_activate(move |_, _| {
                if let Some(w) = weak.upgrade() {
                    f(&w);
                }
            });
            self.window.add_action(&action);
        };
        add("add-file", Box::new(super::dialogs::open_files));
        add("add-magnet", Box::new(super::dialogs::add_magnet));
        add("toggle-pause", Box::new(|w| w.toggle_pause()));
        add("pause", Box::new(|w| w.set_paused(w.selected(), true)));
        add("resume", Box::new(|w| w.set_paused(w.selected(), false)));
        add("pause-all", Box::new(|w| w.set_paused(w.all_items(), true)));
        add("resume-all", Box::new(|w| w.resume_all()));
        add("start-now", Box::new(|w| w.set_paused(w.selected(), false)));
        add("quit", Box::new(|w| w.quit()));
        add("options", Box::new(|w| super::options::show(w, w.selected())));
        add("move-data", Box::new(|w| super::options::move_data(w, w.selected())));
        add("verify", Box::new(|w| super::options::verify(w, w.selected())));
        add("play", Box::new(|w| {
            if let Some(i) = w.selected().first() {
                super::play::play_torrent(w, i);
            }
        }));
        add("create-torrent", Box::new(super::create::show));
        add("remove", Box::new(|w| {
            let sel = w.selected();
            if !sel.is_empty() {
                super::dialogs::confirm_remove(w, sel);
            }
        }));
        add("open-folder", Box::new(|w| w.open_folder()));
        add("copy-magnet", Box::new(|w| w.copy_magnet()));
        add("move-up", Box::new(|w| w.move_selected(Move::Up)));
        add("move-down", Box::new(|w| w.move_selected(Move::Down)));
        add("move-top", Box::new(|w| w.move_selected(Move::Top)));
        add("move-bottom", Box::new(|w| w.move_selected(Move::Bottom)));
        add("select-all", Box::new(|w| {
            w.selection.select_all();
        }));
        add("clear-selection", Box::new(|w| {
            w.selection.unselect_all();
        }));
        add("find", Box::new(|w| {
            w.search.grab_focus();
        }));
        add("settings", Box::new(|w| {
            w.bottom_nav.select_row(w.bottom_nav.row_at_index(1).as_ref());
        }));
        add("new-label", Box::new(super::dialogs::new_label));

        let set_label = gio::SimpleAction::new("set-label", Some(glib::VariantTy::STRING));
        let weak = self.weak();
        set_label.connect_activate(move |_, param| {
            let (Some(w), Some(name)) = (weak.upgrade(), param.and_then(|p| p.get::<String>())) else { return };
            w.set_label_on(&w.selected(), (!name.is_empty()).then_some(name));
        });
        self.window.add_action(&set_label);

        let delete_label = gio::SimpleAction::new("delete-label", Some(glib::VariantTy::STRING));
        let weak = self.weak();
        delete_label.connect_activate(move |_, param| {
            let (Some(w), Some(name)) = (weak.upgrade(), param.and_then(|p| p.get::<String>())) else { return };
            {
                let mut c = w.config.borrow_mut();
                c.labels.retain(|l| l.name != name);
                for m in c.torrents.values_mut() {
                    if m.label.as_deref() == Some(name.as_str()) {
                        m.label = None;
                    }
                }
                c.save();
            }
            if w.filter_state.borrow().0 == View::Label(name.clone()) {
                w.filter_state.borrow_mut().0 = View::All;
                w.filter.changed(gtk::FilterChange::Different);
            }
            w.build_sidebar();
            w.refresh();
        });
        self.window.add_action(&delete_label);

        // Column picker: right-click any header.
        let header_menu = gio::Menu::new();
        for def in COLUMNS.iter().filter(|d| !d.locked) {
            let name = format!("col-{}", def.key);
            let visible = self.columns[def.key].is_visible();
            let action = gio::SimpleAction::new_stateful(&name, None, &visible.to_variant());
            let weak = self.weak();
            let key = def.key;
            action.connect_change_state(move |a, value| {
                let (Some(w), Some(v)) = (weak.upgrade(), value.and_then(|v| v.get::<bool>())) else { return };
                a.set_state(&v.to_variant());
                w.columns[key].set_visible(v);
                let mut c = w.config.borrow_mut();
                c.visible_columns = COLUMNS
                    .iter()
                    .filter(|d| w.columns[d.key].is_visible())
                    .map(|d| d.key.to_string())
                    .collect();
                c.save();
            });
            self.window.add_action(&action);
            header_menu.append(Some(def.title.trim_start_matches('#').trim()).filter(|s| !s.is_empty()).or(Some("№")), Some(&format!("win.{name}")));
        }
        for col in self.columns.values() {
            col.set_header_menu(Some(&header_menu));
        }

        self.app.set_accels_for_action("win.add-file", &["<Ctrl>o"]);
        self.app.set_accels_for_action("win.add-magnet", &["<Ctrl>n", "<Ctrl>u"]);
        self.app.set_accels_for_action("win.find", &["<Ctrl>f"]);
        self.app.set_accels_for_action("win.settings", &["<Ctrl>comma"]);
        self.app.set_accels_for_action("win.quit", &["<Ctrl>q"]);
        self.app.set_accels_for_action("window.close", &["<Ctrl>w"]);

        let weak = self.weak();
        self.alt_btn.set_active(self.config.borrow().alt_enabled);
        self.alt_btn.connect_toggled(move |b| {
            let Some(w) = weak.upgrade() else { return };
            if w.config.borrow().alt_enabled != b.is_active() {
                w.config.borrow_mut().alt_enabled = b.is_active();
                w.config.borrow().save();
                w.apply_rules();
            }
        });
    }

    fn show_row_menu(self: &Rc<Self>, pos: u32, widget: &gtk::Widget, x: f64, y: f64) {
        // Right-clicking outside the selection selects just that row.
        if !self.selection.is_selected(pos) {
            self.selection.select_item(pos, true);
        }
        let sel = self.selected();
        if sel.is_empty() {
            return;
        }
        let menu = gio::Menu::new();
        let first = gio::Menu::new();
        if sel.len() == 1 && super::play::main_media(self, sel[0].id()).is_some() {
            first.append(Some("Play"), Some("win.play"));
        }
        if sel.iter().any(|i| i.state() == State::Queued) {
            first.append(Some("Start now"), Some("win.start-now"));
        }
        if sel.iter().all(|i| i.state().is_running()) {
            first.append(Some("Pause"), Some("win.pause"));
        } else {
            first.append(Some("Resume"), Some("win.resume"));
        }
        first.append(Some("Open folder"), Some("win.open-folder"));
        first.append(Some("Copy magnet link"), Some("win.copy-magnet"));
        menu.append_section(None, &first);

        let manage = gio::Menu::new();
        manage.append(Some("Options…"), Some("win.options"));
        manage.append(Some("Move data…"), Some("win.move-data"));
        manage.append(Some("Verify files"), Some("win.verify"));
        menu.append_section(None, &manage);

        let second = gio::Menu::new();
        let labels = gio::Menu::new();
        let none = gio::MenuItem::new(Some("No label"), None);
        none.set_action_and_target_value(Some("win.set-label"), Some(&"".to_variant()));
        labels.append_item(&none);
        for l in &self.config.borrow().labels {
            let item = gio::MenuItem::new(Some(&l.name), None);
            item.set_action_and_target_value(Some("win.set-label"), Some(&l.name.to_variant()));
            labels.append_item(&item);
        }
        let new = gio::Menu::new();
        new.append(Some("New label…"), Some("win.new-label"));
        labels.append_section(None, &new);
        second.append_submenu(Some("Label"), &labels);
        let order = gio::Menu::new();
        order.append(Some("Move to top"), Some("win.move-top"));
        order.append(Some("Move up"), Some("win.move-up"));
        order.append(Some("Move down"), Some("win.move-down"));
        order.append(Some("Move to bottom"), Some("win.move-bottom"));
        second.append_submenu(Some("Order"), &order);
        menu.append_section(None, &second);

        let last = gio::Menu::new();
        last.append(Some("Remove…"), Some("win.remove"));
        menu.append_section(None, &last);

        let Some(point) = widget.compute_point(&self.column_view, &gtk::graphene::Point::new(x as f32, y as f32)) else {
            return;
        };
        let pop = gtk::PopoverMenu::from_model(Some(&menu));
        pop.set_parent(&self.column_view);
        pop.set_has_arrow(false);
        pop.set_halign(gtk::Align::Start);
        pop.set_pointing_to(Some(&gdk::Rectangle::new(point.x() as i32, point.y() as i32, 1, 1)));
        pop.connect_closed(|p| {
            let p = p.clone();
            glib::idle_add_local_once(move || p.unparent());
        });
        pop.popup();
    }

    // ---- Selection & commands -----------------------------------------------------------------

    pub fn select_first(&self) {
        if self.selection.n_items() > 0 {
            self.selection.select_item(0, true);
        }
    }

    pub fn selected(&self) -> Vec<TorrentItem> {
        let bits = self.selection.selection();
        let mut out = Vec::new();
        if let Some((iter, first)) = gtk::BitsetIter::init_first(&bits) {
            for pos in std::iter::once(first).chain(iter) {
                if let Some(item) = self.selection.item(pos).and_downcast::<TorrentItem>() {
                    out.push(item);
                }
            }
        }
        out
    }

    fn all_items(&self) -> Vec<TorrentItem> {
        self.items.borrow().values().cloned().collect()
    }

    fn select_hashes(&self, hashes: &HashSet<String>) {
        if hashes.is_empty() {
            return;
        }
        let n = self.selection.n_items();
        let mask = gtk::Bitset::new_empty();
        for pos in 0..n {
            if let Some(item) = self.selection.item(pos).and_downcast::<TorrentItem>()
                && hashes.contains(&item.hash())
            {
                mask.add(pos);
            }
        }
        self.selection.set_selection(&mask, &gtk::Bitset::new_range(0, n));
    }

    fn update_toolbar(&self) {
        let sel = self.selected();
        let any = !sel.is_empty();
        self.pause_btn.set_sensitive(any);
        for b in &self.selection_buttons {
            b.set_sensitive(any);
        }
        // One button: Pause while everything selected runs, Resume otherwise.
        let running = any && sel.iter().all(|i| i.state().is_running());
        let (icon, tip) = if running {
            ("media-playback-pause-symbolic", "Pause")
        } else {
            ("media-playback-start-symbolic", "Resume")
        };
        self.pause_btn.set_icon_name(icon);
        self.pause_btn.set_tooltip_text(Some(tip));
        self.pause_btn.update_property(&[gtk::accessible::Property::Label(tip)]);
    }

    fn toggle_pause(self: &Rc<Self>) {
        let sel = self.selected();
        let running = !sel.is_empty() && sel.iter().all(|i| i.state().is_running());
        self.set_paused(sel, running);
    }

    /// Pause takes torrents out of the queue. Resume (and "Start now") runs them at once, past the
    /// queue's limit; a finished torrent resumed by hand keeps seeding past its goal.
    fn set_paused(self: &Rc<Self>, items: Vec<TorrentItem>, pause: bool) {
        let Some(engine) = self.engine() else { return };
        let mut ids = Vec::new();
        {
            let mut cfg = self.config.borrow_mut();
            for i in &items {
                let meta = cfg.torrents.entry(i.hash()).or_default();
                let state = i.state();
                if pause {
                    meta.queued = false;
                    meta.forced = false;
                    if state.is_running() && state != State::Queued {
                        ids.push(i.id());
                    }
                } else if !state.is_running() || state == State::Queued {
                    meta.queued = false;
                    if state.is_complete() {
                        meta.goal_ignored = true;
                    } else {
                        meta.forced = true;
                        self.starting.borrow_mut().insert(i.hash(), std::time::Instant::now());
                    }
                    ids.push(i.id());
                }
            }
            cfg.save();
        }
        let weak = self.weak();
        glib::spawn_future_local(async move {
            for id in ids {
                let r = if pause { engine.pause(id).await } else { engine.resume(id).await };
                if let Err(e) = r
                    && let Some(w) = weak.upgrade()
                {
                    w.toast(&format!("Couldn't {}: {e}", if pause { "pause" } else { "resume" }));
                }
            }
            if let Some(w) = weak.upgrade() {
                w.refresh();
            }
        });
    }

    /// Resume all, keeping to the queue: unfinished torrents wait for a slot, finished ones seed.
    fn resume_all(self: &Rc<Self>) {
        let mut seeders = Vec::new();
        {
            let mut cfg = self.config.borrow_mut();
            for i in self.all_items() {
                let state = i.state();
                if state.is_running() {
                    continue;
                }
                if i.with_row(|r| r.size > 0 && r.done >= r.size) {
                    seeders.push(i);
                } else {
                    cfg.torrents.entry(i.hash()).or_default().queued = true;
                }
            }
            cfg.save();
        }
        self.set_paused(seeders, false);
        self.apply_rules();
        self.refresh();
    }

    /// The rules on top of the engine: alternative speed limits, per-torrent limits, seeding time and
    /// goals, and the download queue.
    fn apply_rules(self: &Rc<Self>) {
        let Some(engine) = self.engine() else { return };
        if engine.is_demo() {
            return;
        }
        // Alternative limits, by hand or on schedule.
        let now = glib::DateTime::now_local().ok();
        let (alt, limits) = {
            let c = self.config.borrow();
            let (weekday, minute) = now
                .as_ref()
                .map(|d| ((d.day_of_week() - 1) as u32, (d.hour() * 60 + d.minute()) as u32))
                .unwrap_or((0, 0));
            let alt = crate::policy::alt_active(c.alt_enabled, c.alt_scheduled, c.alt_from, c.alt_to, c.alt_days, weekday, minute);
            (alt, if alt { (c.alt_down_kbps, c.alt_up_kbps) } else { (c.down_limit_kbps, c.up_limit_kbps) })
        };
        if self.applied_limits.get() != Some(limits) {
            engine.set_limits(limits.0, limits.1);
            self.applied_limits.set(Some(limits));
        }
        let tip = if alt {
            format!("Alternative speed limits on: {} down, {} up", crate::format::limit(limits.0), crate::format::limit(limits.1))
        } else {
            "Alternative speed limits".to_string()
        };
        self.alt_btn.set_tooltip_text(Some(&tip));
        if alt {
            self.alt_btn.add_css_class("alt-on");
        } else {
            self.alt_btn.remove_css_class("alt-on");
        }

        let items: Vec<TorrentItem> = self.all_items();
        self.starting.borrow_mut().retain(|_, t| t.elapsed() < Duration::from_secs(10));
        let mut stop = Vec::new();
        let mut queue_items = Vec::new();
        let tick_secs = 1;
        {
            let mut cfg = self.config.borrow_mut();
            let global_goal = cfg.goal;
            for i in &items {
                let row = i.row();
                let meta = cfg.torrents.entry(row.hash.clone()).or_default();
                let complete = row.size > 0 && row.done >= row.size;
                if complete {
                    meta.forced = false;
                    meta.queued = false;
                }
                if row.state == State::Seeding {
                    meta.seed_secs += tick_secs;
                    if !meta.goal_ignored
                        && crate::policy::goal_reached(meta.goal, global_goal, row.uploaded, row.size, meta.seed_secs)
                    {
                        stop.push(row.id);
                    }
                }
                if matches!(row.state, State::Downloading | State::Seeding) {
                    self.starting.borrow_mut().remove(&row.hash);
                }
                engine.set_torrent_limits(row.id, meta.down_limit_kbps, meta.up_limit_kbps);
                if meta.priorities.iter().any(|&p| p != 1) {
                    // High files first, then normal, then low; each in file order.
                    let mut order: Vec<usize> = Vec::with_capacity(meta.priorities.len());
                    for want in [2u8, 1, 0] {
                        order.extend(meta.priorities.iter().enumerate().filter(|(_, p)| **p == want).map(|(i, _)| i));
                    }
                    engine.set_file_order(row.id, &order);
                }
                queue_items.push(crate::policy::QueueItem {
                    hash: row.hash.clone(),
                    order: i.order(),
                    state: row.state,
                    complete,
                    queued: meta.queued,
                    forced: meta.forced,
                    starting: self.starting.borrow().contains_key(&row.hash),
                });
            }
            let plan = crate::policy::plan_queue(&queue_items, cfg.queue_limit);
            let by_hash: HashMap<String, usize> = items.iter().map(|i| (i.hash(), i.id())).collect();
            let mut pause_ids = Vec::new();
            let mut start_ids = Vec::new();
            for h in &plan.to_queue {
                cfg.torrents.entry(h.clone()).or_default().queued = true;
                pause_ids.extend(by_hash.get(h).copied());
            }
            for h in &plan.to_start {
                cfg.torrents.entry(h.clone()).or_default().queued = false;
                self.starting.borrow_mut().insert(h.clone(), std::time::Instant::now());
                start_ids.extend(by_hash.get(h).copied());
            }
            let changed = !pause_ids.is_empty() || !start_ids.is_empty() || !stop.is_empty();
            if changed || self.ticks.get().is_multiple_of(30) {
                cfg.save();
            }
            if changed {
                pause_ids.extend(stop);
                glib::spawn_future_local(async move {
                    for id in pause_ids {
                        let _ = engine.pause(id).await;
                    }
                    for id in start_ids {
                        let _ = engine.resume(id).await;
                    }
                });
            }
        }
    }

    pub fn remove(self: &Rc<Self>, items: Vec<TorrentItem>, delete_files: bool) {
        let Some(engine) = self.engine() else { return };
        let weak = self.weak();
        glib::spawn_future_local(async move {
            for item in items {
                let hash = item.hash();
                match engine.remove(item.id(), delete_files).await {
                    Ok(()) => {
                        if let Some(w) = weak.upgrade() {
                            let mut c = w.config.borrow_mut();
                            c.order.retain(|h| *h != hash);
                            c.torrents.remove(&hash);
                        }
                    }
                    Err(e) => {
                        if let Some(w) = weak.upgrade() {
                            w.toast(&format!("Couldn't remove “{}”: {e}", item.name()));
                        }
                    }
                }
            }
            if let Some(w) = weak.upgrade() {
                w.config.borrow().save();
                w.refresh();
            }
        });
    }

    fn open_folder(&self) {
        let Some(item) = self.selected().into_iter().next() else { return };
        let Some(engine) = self.engine() else { return };
        let Some(d) = engine.details(item.id()) else { return };
        let launcher = gtk::FileLauncher::new(Some(&gio::File::for_path(&d.folder)));
        let toasts = self.toasts.clone();
        launcher.launch(Some(&self.window), gio::Cancellable::NONE, move |r| {
            if r.is_err() {
                toasts.add_toast(adw::Toast::new("Couldn't open the folder — it may not exist yet"));
            }
        });
    }

    fn copy_magnet(&self) {
        let links: Vec<String> = self
            .selected()
            .iter()
            .map(|i| {
                let name = glib::Uri::escape_string(&i.name(), None, false);
                format!("magnet:?xt=urn:btih:{}&dn={}", i.hash(), name)
            })
            .collect();
        if links.is_empty() {
            return;
        }
        self.window.clipboard().set_text(&links.join("\n"));
        self.toast(if links.len() == 1 { "Magnet link copied" } else { "Magnet links copied" });
    }

    pub fn set_label_on(self: &Rc<Self>, items: &[TorrentItem], label: Option<String>) {
        {
            let mut c = self.config.borrow_mut();
            for i in items {
                c.torrents.entry(i.hash()).or_default().label = label.clone();
            }
            c.save();
        }
        self.refresh();
        self.filter.changed(gtk::FilterChange::Different);
        self.update_table_state();
    }

    fn move_selected(self: &Rc<Self>, how: Move) {
        let selected: HashSet<String> = self.selected().iter().map(|i| i.hash()).collect();
        if selected.is_empty() {
            return;
        }
        {
            let mut c = self.config.borrow_mut();
            let present: HashSet<String> = self.items.borrow().keys().cloned().collect();
            let mut ranked: Vec<String> = c.order.iter().filter(|h| present.contains(*h)).cloned().collect();
            match how {
                Move::Up => {
                    for i in 1..ranked.len() {
                        if selected.contains(&ranked[i]) && !selected.contains(&ranked[i - 1]) {
                            ranked.swap(i, i - 1);
                        }
                    }
                }
                Move::Down => {
                    for i in (0..ranked.len().saturating_sub(1)).rev() {
                        if selected.contains(&ranked[i]) && !selected.contains(&ranked[i + 1]) {
                            ranked.swap(i, i + 1);
                        }
                    }
                }
                Move::Top | Move::Bottom => {
                    let (mut sel, rest): (Vec<_>, Vec<_>) = ranked.into_iter().partition(|h| selected.contains(h));
                    ranked = if how == Move::Top {
                        sel.extend(rest);
                        sel
                    } else {
                        let mut r = rest;
                        r.extend(sel);
                        r
                    };
                }
            }
            let absent: Vec<String> = c.order.iter().filter(|h| !present.contains(*h)).cloned().collect();
            ranked.extend(absent);
            c.order = ranked;
            c.save();
        }
        self.refresh();
        // The № order changed: re-sort, keeping the selection.
        if self.sort_key().as_deref() == Some("num") {
            if let Some(s) = self.column_view.sorter() {
                s.changed(gtk::SorterChange::Different);
            }
            self.select_hashes(&selected);
        }
    }

    fn sort_key(&self) -> Option<String> {
        let sorter = self.column_view.sorter().and_downcast::<gtk::ColumnViewSorter>()?;
        let col = sorter.primary_sort_column()?;
        self.columns.iter().find(|(_, c)| **c == col).map(|(k, _)| k.to_string())
    }

    // ---- Refresh ------------------------------------------------------------------------------

    pub fn refresh(self: &Rc<Self>) {
        let Some(engine) = self.engine() else { return };
        let rows = engine.list();
        let mut changed_membership = false;
        let mut finished = Vec::new();
        let mut config_dirty = false;
        {
            let mut cfg = self.config.borrow_mut();
            let mut items = self.items.borrow_mut();
            let mut history = self.history.borrow_mut();
            let mut prev = self.prev_state.borrow_mut();
            let present: HashSet<String> = rows.iter().map(|r| r.hash.clone()).collect();

            // № = rank in the saved order among the torrents we have.
            for r in &rows {
                if !cfg.order.contains(&r.hash) {
                    cfg.order.push(r.hash.clone());
                    config_dirty = true;
                }
            }
            let ranks: HashMap<String, u32> = cfg
                .order
                .iter()
                .filter(|h| present.contains(*h))
                .enumerate()
                .map(|(i, h)| (h.clone(), i as u32))
                .collect();

            let (mut total_down, mut total_up) = (0u64, 0u64);
            for mut row in rows {
                total_down += row.down_bps;
                total_up += row.up_bps;
                let hash = row.hash.clone();
                let meta = cfg.torrents.entry(hash.clone()).or_insert_with(|| {
                    config_dirty = true;
                    TorrentMeta {
                        added: glib::real_time() as u64 / 1_000_000,
                        ..Default::default()
                    }
                });
                if meta.queued && row.state == State::Paused {
                    row.state = State::Queued;
                }
                let label = meta.label.clone();
                let color = label
                    .as_ref()
                    .and_then(|l| cfg.labels.iter().find(|x| &x.name == l))
                    .map(|l| l.color);
                let label = if color.is_some() { label } else { None };

                let h = history.entry(hash.clone()).or_default();
                h.push_back((row.down_bps, row.up_bps));
                while h.len() > HISTORY {
                    h.pop_front();
                }

                if let Some(old) = prev.insert(hash.clone(), row.state)
                    && old == State::Downloading
                    && row.state.is_complete()
                {
                    finished.push(row.name.clone());
                }

                let order = ranks.get(&hash).copied().unwrap_or(0);
                match items.get(&hash) {
                    Some(item) => {
                        let old_state = item.state();
                        let old_label = item.label();
                        item.update(row, order, label, color);
                        if old_state != item.state() || old_label != item.label() {
                            changed_membership = true;
                        }
                    }
                    None => {
                        let item = TorrentItem::new(row);
                        item.update(item.row(), order, label, color);
                        self.store.append(&item);
                        items.insert(hash, item);
                    }
                }
            }

            let totals = format!("↓ {}   ↑ {}", crate::format::speed(total_down), crate::format::speed(total_up));
            if self.totals.label() != totals {
                self.totals.set_label(&totals);
                if let Some(tray) = self.tray.borrow().clone()
                    && let Some((_, rt)) = engine.api()
                {
                    rt.spawn(async move { tray.update(|t| t.status = totals).await });
                }
            }

            // Drop torrents that are gone.
            let gone: Vec<String> = items.keys().filter(|h| !present.contains(*h)).cloned().collect();
            for hash in gone {
                if let Some(item) = items.remove(&hash)
                    && let Some(pos) = self.store.find(&item)
                {
                    self.store.remove(pos);
                }
                history.remove(&hash);
                prev.remove(&hash);
            }
        }
        if config_dirty {
            self.config.borrow().save();
        }

        // Keep the view current: re-filter when states change, re-sort by live columns.
        let selected: HashSet<String> = self.selected().iter().map(|i| i.hash()).collect();
        let mut resorted = false;
        if changed_membership && self.filter_state.borrow().0 != View::All {
            self.filter.changed(gtk::FilterChange::Different);
            resorted = true;
        }
        if let Some(key) = self.sort_key()
            && LIVE_COLUMNS.contains(&key.as_str())
            && let Some(s) = self.column_view.sorter()
        {
            s.changed(gtk::SorterChange::Different);
            resorted = true;
        }
        if resorted {
            self.select_hashes(&selected);
        }

        for name in finished {
            self.notify_finished(&name);
        }
        self.update_counts();
        self.update_table_state();
        self.update_toolbar();
        self.update_details();
    }

    fn update_table_state(&self) {
        if self.engine.borrow().is_none() {
            return;
        }
        let name = if self.store.n_items() == 0 {
            "empty"
        } else if self.selection.n_items() == 0 {
            let (view, query) = &*self.filter_state.borrow();
            if !query.is_empty() {
                self.nothing_found.set_title("Nothing found");
                self.nothing_found.set_description(Some("Try other words, or look in All torrents."));
            } else {
                let title = match view {
                    View::Downloading => "Nothing is downloading",
                    View::Completed => "Nothing has finished yet",
                    View::Paused => "Nothing is paused",
                    View::Label(_) => "No torrents with this label",
                    View::All => "No torrents",
                };
                self.nothing_found.set_title(title);
                self.nothing_found.set_description(None);
            }
            "nothing"
        } else {
            "table"
        };
        if self.table_stack.visible_child_name().as_deref() != Some(name) {
            self.table_stack.set_visible_child_name(name);
        }
    }

    fn update_details(&self) {
        let sel = self.selected();
        let Some(item) = sel.first() else {
            self.details.show_empty("Select a torrent to see its details");
            return;
        };
        if sel.len() > 1 {
            self.details.show_empty(&format!("{} torrents selected", sel.len()));
            return;
        }
        let Some(engine) = self.engine() else { return };
        let row = item.row();
        let details = engine.details(row.id);
        let history = self.history.borrow();
        let config = self.config.borrow();
        self.details.update(&row, details.as_ref(), history.get(&row.hash), config.torrents.get(&row.hash));
    }

    fn notify_finished(&self, name: &str) {
        if !self.config.borrow().notify_finished {
            return;
        }
        let n = gio::Notification::new("Download finished");
        n.set_body(Some(name));
        n.set_icon(&gio::ThemedIcon::new("folder-download-symbolic"));
        self.app.send_notification(None, &n);
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Move {
    Up,
    Down,
    Top,
    Bottom,
}

/// Waits for whichever of two futures finishes first.
async fn futures_select(a: impl std::future::Future<Output = ()>, b: impl std::future::Future<Output = ()>) {
    let a = std::pin::pin!(a);
    let b = std::pin::pin!(b);
    let mut a = Some(a);
    let mut b = Some(b);
    std::future::poll_fn(move |cx| {
        use std::task::Poll;
        if let Some(f) = a.as_mut()
            && f.as_mut().poll(cx).is_ready()
        {
            return Poll::Ready(());
        }
        if let Some(f) = b.as_mut()
            && f.as_mut().poll(cx).is_ready()
        {
            return Poll::Ready(());
        }
        Poll::Pending
    })
    .await
}

// ---- Table columns ------------------------------------------------------------------------

fn cell_label(xalign: f32) -> gtk::Label {
    gtk::Label::builder()
        .xalign(xalign)
        .ellipsize(gtk::pango::EllipsizeMode::End)
        .build()
}

fn bind(cell: &gtk::ColumnViewCell, prop: &str, target: &impl IsA<glib::Object>, target_prop: &str) {
    cell.property_expression("item")
        .chain_property::<TorrentItem>(prop)
        .bind(target, target_prop, gtk::Widget::NONE);
}

fn build_column(def: &ColumnDef, row_menu: Rc<RefCell<Option<RowMenuCallback>>>) -> gtk::ColumnViewColumn {
    let factory = gtk::SignalListItemFactory::new();
    let key = def.key;
    factory.connect_setup(move |_, obj| {
        let cell = obj.downcast_ref::<gtk::ColumnViewCell>().unwrap();
        let child: gtk::Widget = match key {
            "name" => {
                let b = gtk::Box::new(gtk::Orientation::Horizontal, 8);
                let name = cell_label(0.0);
                name.set_hexpand(true);
                bind(cell, "name", &name, "label");
                bind(cell, "name", &name, "tooltip-text");
                b.append(&name);
                let pill = gtk::Box::new(gtk::Orientation::Horizontal, 4);
                pill.add_css_class("label-pill");
                pill.set_valign(gtk::Align::Center);
                let dot = gtk::Box::builder().valign(gtk::Align::Center).build();
                bind(cell, "label-classes", &dot, "css-classes");
                pill.append(&dot);
                let text = gtk::Label::new(None);
                bind(cell, "label-text", &text, "label");
                pill.append(&text);
                bind(cell, "label-visible", &pill, "visible");
                b.append(&pill);
                b.upcast()
            }
            "progress" => {
                let b = gtk::Box::new(gtk::Orientation::Horizontal, 12);
                let bar = gtk::ProgressBar::builder().hexpand(true).valign(gtk::Align::Center).build();
                bind(cell, "fraction", &bar, "fraction");
                bind(cell, "bar-classes", &bar, "css-classes");
                let pct = gtk::Label::builder().xalign(1.0).width_chars(6).css_classes(["dim", "measure"]).build();
                bind(cell, "percent", &pct, "label");
                b.append(&bar);
                b.append(&pct);
                b.upcast()
            }
            "status" => {
                let l = cell_label(0.0);
                bind(cell, "status", &l, "label");
                bind(cell, "status", &l, "tooltip-text");
                bind(cell, "status-classes", &l, "css-classes");
                l.upcast()
            }
            "num" => {
                let l = cell_label(1.0);
                l.add_css_class("dim");
                l.add_css_class("measure");
                bind(cell, "num", &l, "label");
                l.upcast()
            }
            other => {
                let l = cell_label(1.0);
                l.add_css_class("measure");
                bind(cell, other, &l, "label");
                l.upcast()
            }
        };
        // Right-click opens the row menu.
        let click = gtk::GestureClick::builder().button(3).build();
        let weak_cell = cell.downgrade();
        let menu = row_menu.clone();
        click.connect_pressed(move |g, _, x, y| {
            let (Some(cell), Some(widget)) = (weak_cell.upgrade(), g.widget()) else { return };
            if let Some(cb) = menu.borrow().as_ref() {
                cb(cell.position(), &widget, x, y);
            }
        });
        child.add_controller(click);
        cell.set_child(Some(&child));
    });

    let col = gtk::ColumnViewColumn::new(Some(def.title), Some(factory));
    col.set_resizable(true);
    col.set_fixed_width(def.width);
    col.set_expand(def.expand);
    col.set_sorter(Some(&sorter_for(def.key)));
    col
}

fn sorter_for(key: &'static str) -> gtk::CustomSorter {
    gtk::CustomSorter::new(move |a, b| {
        let a = a.downcast_ref::<TorrentItem>().unwrap();
        let b = b.downcast_ref::<TorrentItem>().unwrap();
        let ord = a.with_row(|ra| {
            b.with_row(|rb| match key {
                "name" => ra.name.to_lowercase().cmp(&rb.name.to_lowercase()),
                "size" => ra.size.cmp(&rb.size),
                "progress" => ra.progress().total_cmp(&rb.progress()),
                "status" => (ra.state as u8).cmp(&(rb.state as u8)),
                "down" => ra.down_bps.cmp(&rb.down_bps),
                "up" => ra.up_bps.cmp(&rb.up_bps),
                // Unknown time left sorts last.
                "eta" => ra.eta.unwrap_or(Duration::MAX).cmp(&rb.eta.unwrap_or(Duration::MAX)),
                "peers" => ra.peers.cmp(&rb.peers),
                "ratio" => {
                    let r = |r: &TorrentRow| if r.done == 0 { 0.0 } else { r.uploaded as f64 / r.done as f64 };
                    r(ra).total_cmp(&r(rb))
                }
                _ => std::cmp::Ordering::Equal,
            })
        });
        // Ties fall back to the № order, so rows never jump around.
        ord.then_with(|| a.order().cmp(&b.order())).into()
    })
}
