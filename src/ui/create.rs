//! Create a .torrent from a file or folder, optionally private, and start seeding it.

use super::window::Window;
use crate::engine::AddOptions;
use adw::prelude::*;
use gtk::{gio, glib};
use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

pub fn show(win: &Rc<Window>) {
    let dialog = adw::Dialog::builder().title("Create torrent").content_width(520).content_height(560).build();
    let header = adw::HeaderBar::builder().show_end_title_buttons(false).show_start_title_buttons(false).build();
    let cancel = gtk::Button::with_label("Cancel");
    let create = gtk::Button::builder().label("Create").css_classes(["primary"]).sensitive(false).build();
    header.pack_start(&cancel);
    header.pack_end(&create);

    let source: Rc<RefCell<Option<PathBuf>>> = Rc::default();
    let page = adw::PreferencesPage::new();

    let what = adw::PreferencesGroup::builder().title("Share").build();
    let source_row = adw::ActionRow::builder().title("File or folder").subtitle("Nothing chosen yet").build();
    let pick_file = gtk::Button::builder().label("File…").valign(gtk::Align::Center).css_classes(["secondary"]).build();
    let pick_dir = gtk::Button::builder().label("Folder…").valign(gtk::Align::Center).css_classes(["secondary"]).build();
    source_row.add_suffix(&pick_file);
    source_row.add_suffix(&pick_dir);
    what.add(&source_row);
    page.add(&what);

    let trackers_group = adw::PreferencesGroup::builder()
        .title("Trackers")
        .description("One address per line. Leave empty to rely on DHT and peer exchange.")
        .build();
    let trackers = gtk::TextView::builder().monospace(true).top_margin(8).bottom_margin(8).left_margin(8).right_margin(8).build();
    trackers.add_css_class("measure");
    trackers_group.add(&gtk::Frame::builder().child(&gtk::ScrolledWindow::builder().child(&trackers).min_content_height(100).build()).build());
    page.add(&trackers_group);

    let opts = adw::PreferencesGroup::new();
    let private = adw::SwitchRow::builder()
        .title("Private")
        .subtitle("Peers come only from the trackers: no DHT or peer exchange")
        .build();
    let seed = adw::SwitchRow::builder().title("Start seeding").subtitle("Add it to Ferrite right away").active(true).build();
    opts.add(&private);
    opts.add(&seed);
    page.add(&opts);

    let tv = adw::ToolbarView::new();
    tv.add_top_bar(&header);
    tv.set_content(Some(&page));
    dialog.set_child(Some(&tv));

    let d = dialog.clone();
    cancel.connect_clicked(move |_| {
        d.close();
    });

    let choose = {
        let (win, source, row, create) = (win.clone(), source.clone(), source_row.clone(), create.clone());
        move |folder: bool| {
            let fd = gtk::FileDialog::builder().title(if folder { "Folder to share" } else { "File to share" }).build();
            let (source, row, create) = (source.clone(), row.clone(), create.clone());
            let done = move |res: Result<gio::File, glib::Error>| {
                if let Ok(f) = res
                    && let Some(p) = f.path()
                {
                    row.set_subtitle(&glib::markup_escape_text(&p.display().to_string()));
                    *source.borrow_mut() = Some(p);
                    create.set_sensitive(true);
                }
            };
            if folder {
                fd.select_folder(Some(&win.window), gio::Cancellable::NONE, done);
            } else {
                fd.open(Some(&win.window), gio::Cancellable::NONE, done);
            }
        }
    };
    let c = choose.clone();
    pick_file.connect_clicked(move |_| c(false));
    pick_dir.connect_clicked(move |_| choose(true));

    let (w, d) = (win.clone(), dialog.clone());
    create.connect_clicked(move |_| {
        let Some(path) = source.borrow().clone() else { return };
        let buf = trackers.buffer();
        let list: Vec<String> = buf
            .text(&buf.start_iter(), &buf.end_iter(), false)
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(String::from)
            .collect();
        let (is_private, start_seeding) = (private.is_active(), seed.is_active());
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "torrent".into());
        let save = gtk::FileDialog::builder().title("Save torrent").initial_name(format!("{name}.torrent")).build();
        let (w, d) = (w.clone(), d.clone());
        save.save(Some(&w.window.clone()), gio::Cancellable::NONE, move |res| {
            let Ok(out) = res else { return };
            let Some(out) = out.path() else { return };
            let Some(engine) = w.engine() else { return };
            d.close();
            w.toast(&format!("Creating “{name}.torrent”… hashing the files"));
            let w = w.clone();
            glib::spawn_future_local(async move {
                let result = async {
                    let bytes = engine.create_torrent(path.clone(), list, is_private).await?;
                    std::fs::write(&out, &bytes)?;
                    if start_seeding {
                        // A folder's files live in the folder itself; a single file in its parent.
                        let folder = if path.is_dir() { path.clone() } else { path.parent().map(|p| p.to_path_buf()).unwrap_or_default() };
                        engine.add(bytes, AddOptions { folder, only_files: None, paused: false }).await?;
                    }
                    anyhow::Ok(())
                }
                .await;
                match result {
                    Ok(()) => w.toast(&format!("Saved {}", out.display())),
                    Err(e) => w.toast(&format!("Couldn't create the torrent: {e:#}")),
                }
                w.refresh();
            });
        });
    });
    dialog.present(Some(&win.window));
}
