//! One torrent in the table: a GObject whose text properties the table cells bind to, so a row
//! updates in place every second without being rebuilt.

use crate::engine::{State, TorrentRow};
use crate::format;
use gtk::glib;
use gtk::prelude::*;
use gtk::subclass::prelude::*;

mod imp {
    use super::*;
    use std::cell::{Cell, RefCell};

    #[derive(Default, glib::Properties)]
    #[properties(wrapper_type = super::TorrentItem)]
    pub struct TorrentItem {
        pub row: RefCell<TorrentRow>,
        pub order: Cell<u32>,
        pub label: RefCell<Option<String>>,

        #[property(get, set)]
        num: RefCell<String>,
        #[property(get, set)]
        name: RefCell<String>,
        #[property(get, set)]
        label_text: RefCell<String>,
        #[property(get, set)]
        label_visible: Cell<bool>,
        #[property(get, set)]
        label_classes: RefCell<Vec<String>>,
        #[property(get, set)]
        size: RefCell<String>,
        #[property(get, set)]
        fraction: Cell<f64>,
        #[property(get, set)]
        bar_classes: RefCell<Vec<String>>,
        #[property(get, set)]
        percent: RefCell<String>,
        #[property(get, set)]
        status: RefCell<String>,
        #[property(get, set)]
        status_classes: RefCell<Vec<String>>,
        #[property(get, set)]
        down: RefCell<String>,
        #[property(get, set)]
        up: RefCell<String>,
        #[property(get, set)]
        eta: RefCell<String>,
        #[property(get, set)]
        peers: RefCell<String>,
        #[property(get, set)]
        ratio: RefCell<String>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for TorrentItem {
        const NAME: &'static str = "FerriteTorrentItem";
        type Type = super::TorrentItem;
    }

    #[glib::derived_properties]
    impl ObjectImpl for TorrentItem {}
}

glib::wrapper! {
    pub struct TorrentItem(ObjectSubclass<imp::TorrentItem>);
}

/// Sets a property only when the value changes, so idle rows cause no redraws.
macro_rules! set_if_changed {
    ($obj:expr, $get:ident, $set:ident, $val:expr) => {{
        let v = $val;
        if $obj.$get() != v {
            $obj.$set(v);
        }
    }};
}

impl TorrentItem {
    pub fn new(row: TorrentRow) -> Self {
        let item: Self = glib::Object::new();
        item.update(row, 0, None, None);
        item
    }

    pub fn row(&self) -> TorrentRow {
        self.imp().row.borrow().clone()
    }

    pub fn with_row<R>(&self, f: impl FnOnce(&TorrentRow) -> R) -> R {
        f(&self.imp().row.borrow())
    }

    pub fn hash(&self) -> String {
        self.imp().row.borrow().hash.clone()
    }

    pub fn id(&self) -> usize {
        self.imp().row.borrow().id
    }

    pub fn state(&self) -> State {
        self.imp().row.borrow().state
    }

    pub fn order(&self) -> u32 {
        self.imp().order.get()
    }

    pub fn label(&self) -> Option<String> {
        self.imp().label.borrow().clone()
    }

    pub fn update(
        &self,
        row: TorrentRow,
        order: u32,
        label: Option<String>,
        label_color: Option<usize>,
    ) {
        let imp = self.imp();
        imp.order.set(order);
        set_if_changed!(self, num, set_num, (order + 1).to_string());
        set_if_changed!(self, name, set_name, row.name.clone());

        set_if_changed!(self, label_visible, set_label_visible, label.is_some());
        set_if_changed!(self, label_text, set_label_text, label.clone().unwrap_or_default());
        let classes = vec![
            "label-dot".to_string(),
            format!("label-color-{}", label_color.unwrap_or(7)),
        ];
        set_if_changed!(self, label_classes, set_label_classes, classes);
        *imp.label.borrow_mut() = label;

        let known_size = row.size > 0;
        set_if_changed!(self, size, set_size, if known_size { format::size(row.size) } else { String::new() });
        set_if_changed!(self, fraction, set_fraction, row.progress());
        let bar = vec!["horizontal".to_string(), "torrent".to_string(), row.state.css().to_string()];
        set_if_changed!(self, bar_classes, set_bar_classes, bar);
        set_if_changed!(
            self,
            percent,
            set_percent,
            if known_size { format::percent(row.done, row.size) } else { String::new() }
        );
        set_if_changed!(self, status, set_status, row.status_text());
        let status_classes = if row.state == State::Error {
            vec!["error".to_string()]
        } else {
            vec![]
        };
        set_if_changed!(self, status_classes, set_status_classes, status_classes);
        set_if_changed!(self, down, set_down, format::speed_or_blank(row.down_bps));
        set_if_changed!(self, up, set_up, format::speed_or_blank(row.up_bps));
        set_if_changed!(self, eta, set_eta, row.eta.map(format::duration).unwrap_or_default());
        set_if_changed!(
            self,
            peers,
            set_peers,
            if row.peers > 0 { row.peers.to_string() } else { String::new() }
        );
        set_if_changed!(self, ratio, set_ratio, if row.done > 0 { format::ratio(row.uploaded, row.done) } else { String::new() });

        *imp.row.borrow_mut() = row;
    }
}
