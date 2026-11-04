//! The tray icon (StatusNotifierItem, as GNOME's AppIndicator extension and KDE show it): Ferrite
//! keeps seeding with the window closed, and the icon brings it back. Menu clicks arrive on ksni's
//! thread and are sent to the GTK main loop over a channel.

use ksni::menu::StandardItem;
use ksni::{MenuItem, ToolTip, Tray, TrayMethods};

pub const APP_ICON: &str = "io.github.ferrite.Ferrite";
const ICON_SVG: &[u8] = include_bytes!("../data/io.github.ferrite.Ferrite.svg");

#[derive(Debug, Clone, Copy)]
pub enum TrayEvent {
    Show,
    PauseAll,
    ResumeAll,
    Quit,
}

pub struct FerriteTray {
    tx: async_channel::Sender<TrayEvent>,
    /// One line for the tooltip: total speeds.
    pub status: String,
    icon_dir: String,
}

impl FerriteTray {
    fn send(&self, e: TrayEvent) {
        let _ = self.tx.try_send(e);
    }
}

fn item(label: &str, event: TrayEvent) -> MenuItem<FerriteTray> {
    StandardItem {
        label: label.into(),
        activate: Box::new(move |t: &mut FerriteTray| t.send(event)),
        ..Default::default()
    }
    .into()
}

impl Tray for FerriteTray {
    fn id(&self) -> String {
        APP_ICON.into()
    }

    fn title(&self) -> String {
        "Ferrite".into()
    }

    fn icon_name(&self) -> String {
        APP_ICON.into()
    }

    /// Lets the icon show even when Ferrite isn't installed system-wide (cargo run).
    fn icon_theme_path(&self) -> String {
        self.icon_dir.clone()
    }

    fn tool_tip(&self) -> ToolTip {
        ToolTip { title: "Ferrite".into(), description: self.status.clone(), ..Default::default() }
    }

    fn activate(&mut self, _x: i32, _y: i32) {
        self.send(TrayEvent::Show);
    }

    fn menu(&self) -> Vec<MenuItem<Self>> {
        vec![
            item("Show Ferrite", TrayEvent::Show),
            MenuItem::Separator,
            item("Resume all", TrayEvent::ResumeAll),
            item("Pause all", TrayEvent::PauseAll),
            MenuItem::Separator,
            item("Quit", TrayEvent::Quit),
        ]
    }
}

/// Writes the app icon where the tray host can find it, returning the theme folder.
fn icon_theme_dir() -> String {
    let base = gtk::glib::user_cache_dir().join("ferrite").join("icons");
    let dir = base.join("hicolor").join("scalable").join("apps");
    let _ = std::fs::create_dir_all(&dir);
    let _ = std::fs::write(dir.join(format!("{APP_ICON}.svg")), ICON_SVG);
    base.to_string_lossy().into_owned()
}

/// Starts the tray on the tokio runtime. Fails quietly where no tray host runs.
pub async fn start(
    rt: tokio::runtime::Handle,
    tx: async_channel::Sender<TrayEvent>,
) -> Option<ksni::Handle<FerriteTray>> {
    let tray = FerriteTray { tx, status: String::new(), icon_dir: icon_theme_dir() };
    match rt.spawn(async move { tray.spawn().await }).await {
        Ok(Ok(handle)) => Some(handle),
        Ok(Err(e)) => {
            eprintln!("no tray icon: {e}");
            None
        }
        Err(_) => None,
    }
}
