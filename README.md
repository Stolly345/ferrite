# Ferrite

A precise BitTorrent client for Linux, written entirely in Rust.

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="docs/screenshots/torrents-dark.png">
  <img alt="Ferrite: the torrent table with labels and progress coloured by state, and the details band showing the selected download's progress, speeds and time left set large" src="docs/screenshots/torrents-light.png">
</picture>

Ferrite downloads and seeds torrents in an interface built like an instrument: square edges, hairlines instead of
boxes, every measurement — sizes, speeds, time left, peers — set in Geist Mono, and colour that only ever means
state. **Your desktop's accent colour** marks downloading, the main action and your current selection; a second
colour marks seeding and upload (sky-blue, or green when your accent is blue-ish so the two never match); **grey**
is paused, **red** is an error, and every colour comes with a status word. Without a desktop accent, Ferrite uses its
own orange — as in these pictures.

- **Interface:** GTK 4 + libadwaita, through [gtk4-rs](https://gtk-rs.org)
- **Engine:** [librqbit](https://github.com/ikatson/rqbit), a BitTorrent engine written in Rust, extended in
  `vendor/librqbit` with protocol encryption, web seeds, file priorities and peer bans: DHT, trackers,
  µTP, UPnP, fast resume
- **No ads, no telemetry, no account.** Apache-2.0.

## A closer look

<table>
  <tr>
    <td width="50%">
      <picture>
        <source media="(prefers-color-scheme: dark)" srcset="docs/screenshots/speed-dark.png">
        <img alt="The Speed tab: two minutes of download speed in orange and upload in sky-blue" src="docs/screenshots/speed-light.png">
      </picture>
      <p><b>Speed</b> — the last two minutes, download in the accent colour, upload in the second.</p>
    </td>
    <td width="50%">
      <picture>
        <source media="(prefers-color-scheme: dark)" srcset="docs/screenshots/peers-dark.png">
        <img alt="The Peers tab: each connected peer's address, client, speeds and connection type" src="docs/screenshots/peers-light.png">
      </picture>
      <p><b>Peers</b> — who you are trading with, their client and speeds.</p>
    </td>
  </tr>
  <tr>
    <td width="50%">
      <picture>
        <source media="(prefers-color-scheme: dark)" srcset="docs/screenshots/files-dark.png">
        <img alt="The Files tab: every file with its size, progress and a checkbox to skip it" src="docs/screenshots/files-light.png">
      </picture>
      <p><b>Files</b> — tick off the files you don't want, even mid-download.</p>
    </td>
    <td width="50%">
      <picture>
        <source media="(prefers-color-scheme: dark)" srcset="docs/screenshots/settings-dark.png">
        <img alt="Settings: theme, download folder, notifications, speed limits, listening port and UPnP" src="docs/screenshots/settings-light.png">
      </picture>
      <p><b>Settings</b> — one page, searchable from the title bar.</p>
    </td>
  </tr>
  <tr>
    <td width="50%">
      <picture>
        <source media="(prefers-color-scheme: dark)" srcset="docs/screenshots/remove-dark.png">
        <img alt="The remove dialog, naming the torrent and offering to delete its files" src="docs/screenshots/remove-light.png">
      </picture>
      <p><b>Remove</b> — always names what goes, and asks before deleting files.</p>
    </td>
    <td width="50%">
      <picture>
        <source media="(prefers-color-scheme: dark)" srcset="docs/screenshots/torrents-light.png">
        <img alt="The same window in the other theme" src="docs/screenshots/torrents-dark.png">
      </picture>
      <p><b>Both themes</b> — warm black or warm paper; follows the system or your choice.</p>
    </td>
  </tr>
</table>

<sub>Pictures come from `ferrite --demo`: open-licensed distros, Blender Studio films and public datasets with
simulated transfers, no network.</sub>

## What it does

- **Add torrents** from `.torrent` files, magnet links (picked up from the clipboard), drag and drop, or straight
  from the file manager and the browser
- **Choose before you add:** the folder and the files, with a warning when they won't fit on the drive
- **A ruled table** with sortable, resizable columns; right-click a header to choose them; your own order (#)
- **Views and labels** in the side pane — All, Downloading, Completed, Paused, and coloured labels — each with a count
- **Search** from the title bar (Ctrl+F): every word, any order, ignoring case and accents
- **The details band:** the selected torrent's progress, speeds and time left set large, a piece strip, transfer
  facts, files, peers, trackers and the speed graph
- **Work on many at once:** pause, resume, label, reorder or remove a selection; a right-click menu on every row
- **Keeps seeding with the window closed,** from a tray icon (Show, Pause all, Resume all, Quit)
- **A download queue:** a few at a time (3 by default), the rest wait in # order; seeding takes no slot; *Start now*
  runs one at once
- **Seeding goals:** stop at a share ratio or after a seeding time — globally or per torrent
- **Per-torrent options:** download and upload limits, its own seeding goal, and its trackers (add or remove)
- **Move data** to another folder or drive, and **verify** files against the torrent at any time
- **File priorities** (High, Normal, Low) and per-file skipping, even mid-download
- **Play while downloading:** videos and songs stream to your player (VLC, mpv, Celluloid…) as the pieces arrive
- **Alternative speed limits,** switched from the toolbar or on a schedule (hours and days)
- **Watch folder:** every `.torrent` dropped there is added on its own
- **RSS feeds:** new items whose title matches a filter are added automatically (Torznab magnets included)
- **Create torrents** from a file or folder — public or private — and start seeding them
- **Remote control:** a web interface and API, password protected, on this computer or your network
- **Networking:** protocol encryption (prefer, require or off), UPnP and NAT-PMP port forwarding, SOCKS5 proxy,
  IP blocklist, binding to one network interface (a VPN's), peers per torrent, and banning peers
- **Web seeds:** torrents that list HTTP mirrors download from them as well as from peers
- **Peers in detail:** address, client, how much of the torrent they have, speeds, connection flags
- **Total speeds** in the title bar
- **Problems in plain words** — "Disk is full — make room, then resume" — and fixed by resuming
- **Notifications** when a download completes
- **Picks up where it left off** after a restart; paused torrents stay paused

## Install

Ferrite is packaged for Debian and Ubuntu. Build the package (see below) and install it:

```sh
sudo apt install ./target/debian/ferrite_*.deb
```

It appears in your app menu and offers to open `.torrent` files and `magnet:` links. To make it the default for
both:

```sh
xdg-mime default io.github.ferrite.Ferrite.desktop application/x-bittorrent x-scheme-handler/magnet
```

## Build

Needs Rust 1.88+ and the GTK 4 / libadwaita development files:

```sh
sudo apt install libgtk-4-dev libadwaita-1-dev
cargo run --release
```

The Debian package:

```sh
cargo install cargo-deb   # once
cargo deb
```

`cargo test` runs offline: the unit tests, round trips through the real engine (add, verify, pause, resume,
remove, move data, edit trackers, private torrents), a web-seed-only download from a local HTTP server, and a small
swarm on this computer where one engine downloads from another with encryption required and a third, having
banned the address, gets nothing.

Two more tests check compatibility with libtorrent (the engine of qBittorrent and Deluge) with encryption forced
both ways; they need another client running and are skipped unless asked for:

```sh
FERRITE_INTEROP_DIR=… FERRITE_INTEROP_PEER=127.0.0.1:6881 cargo test interop_download_encrypted -- --ignored
FERRITE_INTEROP_DIR=… cargo test interop_seed_encrypted -- --ignored
```

## Keyboard

| Keys | Action |
|---|---|
| Ctrl+O | Open torrent file |
| Ctrl+N | Add magnet link |
| Ctrl+F | Search |
| Delete | Remove the selected torrents |
| Alt+↑ / Alt+↓ | Move up / down in the order |
| Ctrl+A / Esc | Select all / clear the selection |
| Ctrl+, | Settings |
| Ctrl+Q | Quit |

## Demo and screenshots

`ferrite --demo` opens the sample torrents shown above, with simulated transfers and no network. It runs beside a
normal instance and never touches your settings. To render a picture:

```sh
FERRITE_SCREENSHOT=out.png FERRITE_THEME=dark FERRITE_TAB=peers cargo run -- --demo
```

`FERRITE_TAB` is `general`, `files`, `peers`, `trackers` or `speed`; `FERRITE_PAGE=settings` opens Settings,
`FERRITE_ACTION=win.remove` opens the remove dialog, and `FERRITE_ACCENT=ferrite` uses Ferrite's orange instead of the
desktop accent (as the pictures here do).

## Where things live

| What | Where |
|---|---|
| Settings, labels, order | `~/.config/ferrite/settings.json` |
| Engine resume data | `~/.local/share/ferrite/session/` |
| A separate profile (tests, a second setup) | `FERRITE_HOME=/some/dir ferrite` |
| Downloads | `~/Downloads` (change in Settings) |

## Code

| File | Role |
|---|---|
| `src/engine.rs` | librqbit wrapper and the demo engine; plain snapshots for the UI |
| `src/policy.rs` | The queue, seeding goals and speed schedule, as pure tested functions |
| `src/tray.rs`, `src/watch.rs`, `src/feeds.rs` | Tray icon, watch folder, RSS feeds |
| `src/server.rs`, `src/natpmp.rs` | HTTP server (streaming, remote control), NAT-PMP port mapping |
| `src/ui/options.rs`, `src/ui/play.rs`, `src/ui/create.rs` | Torrent options, move, verify; play; create torrent |
| `vendor/librqbit/` | The engine with Ferrite's patches, each listed in `FERRITE-PATCHES.md` |
| `src/item.rs` | One table row as a GObject the cells bind to |
| `src/ui/window.rs` | Window, side pane, toolbar, table, refresh loop, commands |
| `src/ui/details.rs` | The details band and its tabs |
| `src/ui/widgets.rs` | Piece strip and speed graph (Cairo, coloured from CSS) |
| `src/ui/dialogs.rs` | Add, magnet, remove, new label |
| `src/ui/settings.rs` | Settings page |
| `src/style.css` | The look: libadwaita mapped onto the tokens, then each region |
| `src/palette-dark.css`, `src/palette-light.css` | The tokens (OKLCH): paper, ink, rules, the two signals, label tones |
| `data/fonts/` | Geist Mono, bundled into the binary and registered at startup |

## Roadmap

BitTorrent v2-only torrents (hybrid v1 + v2 torrents already work), "when downloads finish" actions (sleep, shut
down), translations, Flatpak. The full plan is in the [product requirements](docs/PRD.md).

## Credits

Geist Mono © 2024 The Geist Project Authors, under the SIL Open Font License 1.1 (`data/fonts/OFL.txt`).

## License

[Apache-2.0](LICENSE).
