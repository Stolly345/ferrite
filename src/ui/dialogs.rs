//! Dialogs: open .torrent files, add a magnet link, choose folder and files before adding,
//! confirm removal, create a label.

use super::window::Window;
use crate::config::{LABEL_COLORS, Label};
use crate::engine::{AddOptions, Preview, Source};
use crate::format;
use crate::item::TorrentItem;
use adw::prelude::*;
use gtk::{gio, glib};
use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

pub fn open_files(win: &Rc<Window>) {
    let filter = gtk::FileFilter::new();
    filter.set_name(Some("Torrent files"));
    filter.add_pattern("*.torrent");
    filter.add_mime_type("application/x-bittorrent");
    let filters = gio::ListStore::new::<gtk::FileFilter>();
    filters.append(&filter);
    let dialog = gtk::FileDialog::builder()
        .title("Open torrent file")
        .filters(&filters)
        .modal(true)
        .build();
    let w = win.clone();
    dialog.open_multiple(Some(&win.window), gio::Cancellable::NONE, move |res| {
        let Ok(files) = res else { return };
        let sources: Vec<Source> = (0..files.n_items())
            .filter_map(|i| files.item(i).and_downcast::<gio::File>())
            .filter_map(|f| f.path())
            .map(Source::File)
            .collect();
        add_sources(&w, sources);
    });
}

pub fn add_magnet(win: &Rc<Window>) {
    let dialog = adw::AlertDialog::new(Some("Add magnet link"), None);
    let entry = gtk::Entry::builder()
        .placeholder_text("magnet:?xt=urn:btih:…")
        .activates_default(true)
        .width_chars(48)
        .build();
    dialog.set_extra_child(Some(&entry));
    dialog.add_responses(&[("cancel", "Cancel"), ("add", "Add")]);
    dialog.set_response_appearance("add", adw::ResponseAppearance::Suggested);
    dialog.set_default_response(Some("add"));
    dialog.set_close_response("cancel");
    dialog.set_response_enabled("add", false);
    let d = dialog.clone();
    entry.connect_changed(move |e| {
        let t = e.text();
        let t = t.trim();
        d.set_response_enabled("add", t.starts_with("magnet:") || t.starts_with("http"));
    });

    // Prefill from the clipboard when it holds a magnet link.
    let e = entry.clone();
    win.window.clipboard().read_text_async(gio::Cancellable::NONE, move |r| {
        if let Ok(Some(text)) = r
            && text.trim().starts_with("magnet:")
            && e.text().is_empty()
        {
            e.set_text(text.trim());
            e.select_region(0, -1);
        }
    });

    let w = win.clone();
    dialog.connect_response(None, move |_, response| {
        if response == "add" {
            let links: Vec<Source> = entry
                .text()
                .split_whitespace()
                .map(|s| Source::Url(s.to_string()))
                .collect();
            add_sources(&w, links);
        }
    });
    dialog.present(Some(&win.window));
}

/// Adds torrents: each is read first, then the user picks the folder and files (unless they
/// turned that off in Settings).
pub fn add_sources(win: &Rc<Window>, sources: Vec<Source>) {
    let Some(engine) = win.engine() else {
        win.toast("The torrent engine isn't ready yet");
        return;
    };
    if engine.is_demo() {
        win.toast("Adding torrents is off in the demo");
        return;
    }
    for source in sources {
        let is_magnet = matches!(source, Source::Url(_));
        let ask = win.config.borrow().ask_before_adding;
        let dialog = ask.then(|| AddDialog::new(win));
        if let Some(d) = &dialog {
            d.dialog.present(Some(&win.window));
        } else if is_magnet {
            win.toast("Looking up the torrent…");
        }
        let engine = engine.clone();
        let w = win.clone();
        glib::spawn_future_local(async move {
            match engine.preview(source).await {
                Ok(preview) => match dialog {
                    Some(d) => d.show_preview(preview),
                    None => {
                        let folder = target_folder(&w.config.borrow().download_dir, &preview);
                        let opts = AddOptions { folder, only_files: None, paused: false };
                        finish_add(&w, preview, opts).await;
                    }
                },
                Err(e) => {
                    if let Some(d) = dialog {
                        d.dialog.close();
                    }
                    w.toast(&format!("Couldn't add the torrent: {e:#}"));
                }
            }
        });
    }
}

/// Multi-file torrents get a folder of their own, as every client does.
fn target_folder(base: &std::path::Path, preview: &Preview) -> PathBuf {
    if preview.files.len() > 1 {
        base.join(sanitize(&preview.name))
    } else {
        base.to_path_buf()
    }
}

fn sanitize(name: &str) -> String {
    let s: String = name
        .chars()
        .map(|c| if c == '/' || c == '\0' { '_' } else { c })
        .collect();
    let s = s.trim().trim_start_matches('.').to_string();
    if s.is_empty() { "torrent".into() } else { s }
}

async fn finish_add(win: &Rc<Window>, preview: Preview, opts: AddOptions) {
    let Some(engine) = win.engine() else { return };
    let name = preview.name.clone();
    match engine.add(preview.torrent, opts).await {
        Ok(_) => win.refresh(),
        Err(e) => win.toast(&format!("Couldn't add “{name}”: {e:#}")),
    }
}

fn free_space(path: &std::path::Path) -> Option<u64> {
    // The folder may not exist yet: ask about the nearest existing parent.
    let mut p = path.to_path_buf();
    while !p.exists() {
        p = p.parent()?.to_path_buf();
    }
    gio::File::for_path(&p)
        .query_filesystem_info("filesystem::free", gio::Cancellable::NONE)
        .ok()
        .map(|i| i.attribute_uint64("filesystem::free"))
}

struct AddDialog {
    dialog: adw::Dialog,
    win: Rc<Window>,
    stack: gtk::Stack,
    title: gtk::Label,
    folder_row: adw::ActionRow,
    size_row: adw::ActionRow,
    files_group: adw::PreferencesGroup,
    add_btn: gtk::Button,
    state: Rc<RefCell<AddState>>,
}

#[derive(Default)]
struct AddState {
    preview: Option<Preview>,
    base: PathBuf,
    checks: Vec<gtk::CheckButton>,
}

impl AddDialog {
    fn new(win: &Rc<Window>) -> Rc<Self> {
        let dialog = adw::Dialog::builder()
            .title("Add torrent")
            .content_width(560)
            .content_height(560)
            .build();
        let header = adw::HeaderBar::builder().show_end_title_buttons(false).show_start_title_buttons(false).build();
        let cancel = gtk::Button::with_label("Cancel");
        header.pack_start(&cancel);
        let add_btn = gtk::Button::builder().label("Add").css_classes(["suggested-action"]).sensitive(false).build();
        header.pack_end(&add_btn);

        let stack = gtk::Stack::new();
        let loading = adw::StatusPage::builder()
            .title("Looking up the torrent…")
            .description("Asking peers for the torrent's details. This can take a moment for magnet links.")
            .child(&adw::Spinner::builder().width_request(32).height_request(32).build())
            .build();
        loading.add_css_class("compact");
        stack.add_named(&loading, Some("loading"));

        let body = gtk::Box::new(gtk::Orientation::Vertical, 18);
        body.set_margin_top(12);
        body.set_margin_bottom(18);
        body.set_margin_start(18);
        body.set_margin_end(18);
        let title = gtk::Label::builder()
            .wrap(true)
            .wrap_mode(gtk::pango::WrapMode::WordChar)
            .xalign(0.0)
            .css_classes(["title-3"])
            .build();
        body.append(&title);
        let where_group = adw::PreferencesGroup::new();
        let folder_row = adw::ActionRow::builder().title("Save to").subtitle_selectable(true).build();
        folder_row.add_prefix(&gtk::Image::from_icon_name("folder-symbolic"));
        let change = gtk::Button::builder().label("Change…").valign(gtk::Align::Center).css_classes(["secondary"]).build();
        folder_row.add_suffix(&change);
        where_group.add(&folder_row);
        let size_row = adw::ActionRow::builder().title("Size").build();
        size_row.add_prefix(&gtk::Image::from_icon_name("drive-harddisk-symbolic"));
        where_group.add(&size_row);
        body.append(&where_group);
        let files_group = adw::PreferencesGroup::builder().title("Files").build();
        body.append(&files_group);
        let scroll = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vexpand(true)
            .child(&adw::Clamp::builder().child(&body).maximum_size(640).build())
            .build();
        stack.add_named(&scroll, Some("ready"));

        let tv = adw::ToolbarView::new();
        tv.add_top_bar(&header);
        tv.set_content(Some(&stack));
        dialog.set_child(Some(&tv));

        let this = Rc::new(AddDialog {
            dialog: dialog.clone(),
            win: win.clone(),
            stack,
            title,
            folder_row,
            size_row,
            files_group,
            add_btn: add_btn.clone(),
            state: Rc::new(RefCell::new(AddState {
                base: win.config.borrow().download_dir.clone(),
                ..Default::default()
            })),
        });

        let d = dialog.clone();
        cancel.connect_clicked(move |_| {
            d.close();
        });

        let t = this.clone();
        change.connect_clicked(move |_| {
            let fd = gtk::FileDialog::builder()
                .title("Save to")
                .initial_folder(&gio::File::for_path(&t.state.borrow().base))
                .build();
            let t2 = t.clone();
            fd.select_folder(Some(&t.win.window), gio::Cancellable::NONE, move |r| {
                if let Ok(f) = r
                    && let Some(p) = f.path()
                {
                    t2.state.borrow_mut().base = p;
                    t2.update_summary();
                }
            });
        });

        let t = this.clone();
        add_btn.connect_clicked(move |_| {
            let (preview, opts) = {
                let mut s = t.state.borrow_mut();
                let Some(preview) = s.preview.take() else { return };
                let included: Vec<usize> = s
                    .checks
                    .iter()
                    .enumerate()
                    .filter(|(_, c)| c.is_active())
                    .map(|(i, _)| i)
                    .collect();
                let only_files = (included.len() != preview.files.len()).then_some(included);
                let folder = target_folder(&s.base, &preview);
                (preview, AddOptions { folder, only_files, paused: false })
            };
            t.dialog.close();
            let w = t.win.clone();
            glib::spawn_future_local(async move {
                finish_add(&w, preview, opts).await;
            });
        });
        this
    }

    fn show_preview(self: &Rc<Self>, preview: Preview) {
        self.title.set_label(&preview.name);
        let mut checks = Vec::new();
        for (name, size) in &preview.files {
            let check = gtk::CheckButton::builder().active(true).valign(gtk::Align::Center).build();
            let row = adw::ActionRow::builder()
                .title(glib::markup_escape_text(name))
                .title_lines(2)
                .activatable_widget(&check)
                .build();
            row.add_prefix(&check);
            row.add_suffix(&gtk::Label::builder().label(format::size(*size)).css_classes(["dim-label"]).build());
            self.files_group.add(&row);
            let t = self.clone();
            check.connect_toggled(move |_| t.update_summary());
            checks.push(check);
        }
        if preview.files.len() > 1 {
            let toggle = gtk::Button::builder().label("Select none").css_classes(["flat"]).build();
            let cs = checks.clone();
            toggle.connect_clicked(move |b| {
                let select = cs.iter().all(|c| !c.is_active());
                for c in &cs {
                    c.set_active(select);
                }
                b.set_label(if select { "Select none" } else { "Select all" });
            });
            self.files_group.set_header_suffix(Some(&toggle));
        }
        {
            let mut s = self.state.borrow_mut();
            s.preview = Some(preview);
            s.checks = checks;
        }
        self.update_summary();
        self.stack.set_visible_child_name("ready");
    }

    fn update_summary(&self) {
        let s = self.state.borrow();
        let Some(preview) = &s.preview else {
            self.folder_row.set_subtitle(&s.base.display().to_string());
            return;
        };
        let folder = target_folder(&s.base, preview);
        self.folder_row.set_subtitle(&glib::markup_escape_text(&folder.display().to_string()));
        let total: u64 = preview.files.iter().map(|f| f.1).sum();
        let chosen: u64 = preview
            .files
            .iter()
            .zip(&s.checks)
            .filter(|(_, c)| c.is_active())
            .map(|(f, _)| f.1)
            .sum();
        let any = s.checks.iter().any(|c| c.is_active());
        let free = free_space(&folder);
        let text = if chosen == total {
            format::size(total)
        } else {
            format!("{} of {}", format::size(chosen), format::size(total))
        };
        match free {
            Some(free) if free < chosen => {
                self.size_row.set_subtitle(&format!(
                    "{text} — only {} free on this drive",
                    format::size(free)
                ));
                self.size_row.add_css_class("error");
            }
            Some(free) => {
                self.size_row.set_subtitle(&format!("{text} · {} free", format::size(free)));
                self.size_row.remove_css_class("error");
            }
            None => {
                self.size_row.set_subtitle(&text);
                self.size_row.remove_css_class("error");
            }
        }
        self.add_btn.set_sensitive(any);
    }
}

pub fn confirm_remove(win: &Rc<Window>, items: Vec<TorrentItem>) {
    let what = if items.len() == 1 {
        format!("“{}”", items[0].name())
    } else {
        format!("{} torrents", items.len())
    };
    let dialog = adw::AlertDialog::new(
        Some(if items.len() == 1 { "Remove torrent?" } else { "Remove torrents?" }),
        Some(&format!("{what} will be removed from the list.")),
    );
    let delete = gtk::CheckButton::with_label("Also delete the downloaded files");
    dialog.set_extra_child(Some(&delete));
    dialog.add_responses(&[("cancel", "Cancel"), ("remove", "Remove")]);
    dialog.set_response_appearance("remove", adw::ResponseAppearance::Destructive);
    dialog.set_default_response(Some("remove"));
    dialog.set_close_response("cancel");
    let w = win.clone();
    dialog.connect_response(None, move |_, r| {
        if r == "remove" {
            w.remove(items.clone(), delete.is_active());
        }
    });
    dialog.present(Some(&win.window));
}

pub fn new_label(win: &Rc<Window>) {
    let items = win.selected();
    let dialog = adw::AlertDialog::new(Some("New label"), None);
    let entry = gtk::Entry::builder().placeholder_text("Name").activates_default(true).build();
    dialog.set_extra_child(Some(&entry));
    dialog.add_responses(&[("cancel", "Cancel"), ("create", "Create")]);
    dialog.set_response_appearance("create", adw::ResponseAppearance::Suggested);
    dialog.set_default_response(Some("create"));
    dialog.set_close_response("cancel");
    dialog.set_response_enabled("create", false);
    let d = dialog.clone();
    let w = win.clone();
    entry.connect_changed(move |e| {
        let name = e.text().trim().to_string();
        let exists = w.config.borrow().labels.iter().any(|l| l.name == name);
        d.set_response_enabled("create", !name.is_empty() && !exists);
    });
    let w = win.clone();
    dialog.connect_response(None, move |_, r| {
        if r != "create" {
            return;
        }
        let name = entry.text().trim().to_string();
        {
            let mut c = w.config.borrow_mut();
            let color = c.labels.len() % LABEL_COLORS.len();
            c.labels.push(Label { name: name.clone(), color });
            c.save();
        }
        w.build_sidebar();
        w.set_label_on(&items, Some(name));
    });
    dialog.present(Some(&win.window));
}
