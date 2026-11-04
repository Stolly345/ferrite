//! Ferrite — a precise BitTorrent client for Linux, written in Rust.
//! GTK 4 + libadwaita for the interface, librqbit for the engine.

mod config;
mod engine;
mod format;
mod item;
mod ui;

use adw::prelude::*;
use engine::{Engine, EngineOptions, Source};
use gtk::{gdk, gio, glib};
use std::cell::RefCell;
use std::rc::Rc;
use ui::window::Window;

const APP_ID: &str = "io.github.ferrite.Ferrite";

fn main() -> glib::ExitCode {
    // `--demo`: sample torrents, no network; runs beside a normal instance.
    let demo = std::env::args().any(|a| a == "--demo");

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .thread_name("ferrite-engine")
        .build()
        .expect("couldn't start the async runtime");
    let handle = runtime.handle().clone();

    let mut flags = gio::ApplicationFlags::HANDLES_OPEN;
    if demo {
        flags |= gio::ApplicationFlags::NON_UNIQUE;
    }
    let app = adw::Application::builder()
        .application_id(if demo { "io.github.ferrite.Ferrite.Demo" } else { APP_ID })
        .flags(flags)
        .build();
    app.add_main_option(
        "demo",
        glib::Char::from(b'd'),
        glib::OptionFlags::NONE,
        glib::OptionArg::None,
        "Show sample torrents without touching the network",
        None,
    );

    let window: Rc<RefCell<Option<Rc<Window>>>> = Rc::default();

    app.connect_startup(|_| load_css());

    let ensure_window = {
        let window = window.clone();
        move |app: &adw::Application| -> Rc<Window> {
            if let Some(w) = window.borrow().as_ref() {
                return w.clone();
            }
            let w = Window::new(app, demo);
            *window.borrow_mut() = Some(w.clone());
            start_engine(&w, handle.clone(), demo);
            w
        }
    };

    let ew = ensure_window.clone();
    app.connect_activate(move |app| {
        ew(app).window.present();
    });

    app.connect_open(move |app, files, _| {
        let w = ensure_window(app);
        w.window.present();
        let sources: Vec<Source> = files
            .iter()
            .filter_map(|f| {
                let uri = f.uri().to_string();
                if uri.starts_with("magnet:") {
                    Some(Source::Url(uri))
                } else {
                    f.path().map(Source::File)
                }
            })
            .collect();
        // Wait until the engine is up, then add.
        let w2 = w.clone();
        let mut pending = Some(sources);
        glib::timeout_add_local(std::time::Duration::from_millis(200), move || {
            if w2.engine().is_none() {
                return glib::ControlFlow::Continue;
            }
            if let Some(sources) = pending.take() {
                ui::dialogs::add_sources(&w2, sources);
            }
            glib::ControlFlow::Break
        });
    });

    let code = app.run();
    drop(runtime);
    code
}

/// Dev aid for screenshots: `FERRITE_SCREENSHOT=out.png ferrite --demo` selects the first torrent,
/// optionally switches the details tab (`FERRITE_TAB=peers`) and theme (`FERRITE_THEME=dark`),
/// renders the window to a PNG and quits.
fn schedule_screenshot(w: &Rc<Window>) {
    let Ok(path) = std::env::var("FERRITE_SCREENSHOT") else { return };
    match std::env::var("FERRITE_THEME").as_deref() {
        Ok("dark") => adw::StyleManager::default().set_color_scheme(adw::ColorScheme::ForceDark),
        Ok("light") => adw::StyleManager::default().set_color_scheme(adw::ColorScheme::ForceLight),
        _ => {}
    }
    let w = w.clone();
    let wait = std::env::var("FERRITE_WAIT").ok().and_then(|s| s.parse::<u64>().ok()).unwrap_or(2);
    glib::timeout_add_local_once(std::time::Duration::from_secs(wait.max(1)), move || {
        w.select_first();
        if let Ok(tab) = std::env::var("FERRITE_TAB") {
            w.details.show_tab(&tab);
        }
        if let Ok(action) = std::env::var("FERRITE_ACTION") {
            gtk::prelude::WidgetExt::activate_action(&w.window, &action, None).ok();
        }
        if std::env::var("FERRITE_PAGE").as_deref() == Ok("settings") {
            gtk::prelude::WidgetExt::activate_action(&w.window, "win.settings", None).ok();
        }
        let mut tries = 0;
        glib::timeout_add_local(std::time::Duration::from_millis(1500), move || {
            tries += 1;
            let done = capture(&w.window, &path) || tries > 10;
            if done {
                w.app.quit();
                glib::ControlFlow::Break
            } else {
                glib::ControlFlow::Continue
            }
        });
    });
}

/// Renders the window's content over the window color into a PNG. False if no frame yet.
fn capture(win: &adw::ApplicationWindow, path: &str) -> bool {
    // The window's own child holds the content and any open dialog above it.
    let Some(content) = win.child() else { return false };
    let (width, height) = (content.width() as f32, content.height() as f32);
    let paintable = gtk::WidgetPaintable::new(Some(&content));
    let snap = gtk::Snapshot::new();
    let bg = if adw::StyleManager::default().is_dark() {
        gdk::RGBA::new(0.035, 0.035, 0.043, 1.0)
    } else {
        gdk::RGBA::new(0.957, 0.957, 0.961, 1.0)
    };
    let rect = gtk::graphene::Rect::new(0.0, 0.0, width, height);
    snap.append_color(&bg, &rect);
    paintable.snapshot(&snap, width as f64, height as f64);
    let Some(node) = snap.to_node() else { return false };
    // Only the background: the content has not been drawn yet.
    if node.node_type() == gtk::gsk::RenderNodeType::ColorNode {
        return false;
    }
    let Some(renderer) = win.native().and_then(|n| n.renderer()) else { return false };
    let tex = renderer.render_texture(&node, Some(&rect));
    tex.save_to_png(path).is_ok()
}

fn start_engine(w: &Rc<Window>, rt: tokio::runtime::Handle, demo: bool) {
    if demo {
        w.set_engine(Engine::demo());
        schedule_screenshot(w);
        return;
    }
    let cfg = w.config.borrow().clone();
    let w = w.clone();
    glib::spawn_future_local(async move {
        let opts = |port| EngineOptions {
            download_dir: cfg.download_dir.clone(),
            session_dir: config::data_dir().join("session"),
            offline: false,
            port,
            upnp: cfg.upnp,
            down_limit_kbps: cfg.down_limit_kbps,
            up_limit_kbps: cfg.up_limit_kbps,
        };
        match Engine::start(rt.clone(), opts(cfg.listen_port)).await {
            Ok(e) => {
                w.set_engine(e);
                schedule_screenshot(&w);
            }
            Err(first) => {
                // The port may be taken (another client running): try any free port.
                match Engine::start(rt, opts(0)).await {
                    Ok(e) => {
                        w.set_engine(e);
                        w.toast(&format!("Port {} is busy — using another one for now", cfg.listen_port));
                    }
                    Err(_) => w.show_engine_error(&first),
                }
            }
        }
    });
}

/// Geist Mono, the face for every measurement (SIL Open Font License, data/fonts/OFL.txt).
const GEIST_MONO: &[u8] = include_bytes!("../data/fonts/GeistMono.ttf");

/// Registers the bundled font with Pango, so it works without being installed.
fn load_fonts() {
    let dir = glib::user_cache_dir().join("ferrite").join("fonts");
    let path = dir.join("GeistMono.ttf");
    let fresh = std::fs::metadata(&path).map(|m| m.len() == GEIST_MONO.len() as u64).unwrap_or(false);
    if !fresh && (std::fs::create_dir_all(&dir).is_err() || std::fs::write(&path, GEIST_MONO).is_err()) {
        return;
    }
    if let Some(map) = gtk::Label::new(None).pango_context().font_map()
        && let Err(e) = map.add_font_file(&path)
    {
        eprintln!("couldn't load Geist Mono: {e}");
    }
}

fn report_css_errors(provider: &gtk::CssProvider, name: &'static str) {
    provider.connect_parsing_error(move |_, section, error| {
        eprintln!("{name}: {}: {error}", section.to_str());
    });
}

fn load_css() {
    load_fonts();
    let display = gdk::Display::default().expect("no display");
    let style = gtk::CssProvider::new();
    report_css_errors(&style, "style.css");
    style.load_from_string(include_str!("style.css"));
    gtk::style_context_add_provider_for_display(&display, &style, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION);

    // The palette follows libadwaita's light/dark state (including a theme forced in Settings) and the
    // desktop's accent colour; both can change while Ferrite runs.
    let palette = gtk::CssProvider::new();
    report_css_errors(&palette, "palette");
    gtk::style_context_add_provider_for_display(&display, &palette, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION);
    let apply = move |sm: &adw::StyleManager| {
        let base = if sm.is_dark() {
            include_str!("palette-dark.css")
        } else {
            include_str!("palette-light.css")
        };
        palette.load_from_string(&format!("{base}\n{}", accent_css(sm)));
    };
    let sm = adw::StyleManager::default();
    apply(&sm);
    sm.connect_dark_notify(apply.clone());
    sm.connect_accent_color_notify(apply);
}

fn css_rgb(c: &gdk::RGBA) -> String {
    let ch = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    format!("rgb({} {} {})", ch(c.red()), ch(c.green()), ch(c.blue()))
}

/// The desktop's accent colour, overriding the palette's orange. The accent marks downloading, the
/// main action, the current selection and focus. Seeding keeps the palette's sky-blue unless the accent
/// is itself blue-ish, in which case seeding turns green so the two states never share a colour.
/// Ferrite's orange stays when the desktop has no accent colour (or with FERRITE_ACCENT=ferrite).
fn accent_css(sm: &adw::StyleManager) -> String {
    if !sm.is_system_supports_accent_colors() || std::env::var("FERRITE_ACCENT").as_deref() == Ok("ferrite") {
        return String::new();
    }
    let accent = sm.accent_color();
    let dark = sm.is_dark();
    let fill = accent.to_rgba();
    let focus = accent.to_standalone_rgba(dark);
    // Text on the accent: dark ink on light accents (yellow, teal…), light ink on deep ones.
    let lum = 0.2126 * fill.red() + 0.7152 * fill.green() + 0.0722 * fill.blue();
    let ink = if lum > 0.55 { "oklch(14% 0.02 80)" } else { "oklch(98% 0.005 80)" };
    let second = match accent {
        adw::AccentColor::Blue | adw::AccentColor::Teal | adw::AccentColor::Slate => {
            if dark { "--color-accent-2: oklch(74% 0.13 150);" } else { "--color-accent-2: oklch(56% 0.13 150);" }
        }
        _ => "",
    };
    format!(
        ":root {{ --color-accent: {}; --color-accent-ink: {ink}; --color-focus: {}; {second} }}",
        css_rgb(&fill),
        css_rgb(&focus)
    )
}
