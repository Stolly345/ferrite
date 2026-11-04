# Ferrite — Product Requirements Document

| | |
|---|---|
| Owner | Alexander Stoll |
| Status | v0.1.0 built; v0.2 planned |
| Last updated | 3 November 2026 |
| License | Apache-2.0 |

## 1. Summary

Ferrite is a BitTorrent client for Linux, written entirely in Rust. It aims to do the job of Transmission —
reliable, light, no ads, no telemetry — in an exact, modern interface built with GTK 4 and libadwaita: a
hairline-ruled torrent table where colour only ever means state, every measurement set in a monospaced face, a
side pane with status views and labels, and a details band with the selected torrent's live figures.

## 2. Problem

Linux has capable torrent clients, but most look and feel dated or dense:

- **Transmission (GTK)** is light and trusted but spare: few views, little at-a-glance state, no labels in the main
  window.
- **qBittorrent** is powerful but dense: dozens of columns, settings that are hard to navigate, a Qt look that does
  not sit well on GNOME.
- **Deluge** depends on plugins for common needs and its GTK UI has aged.

People who download a few torrents a week (Linux images, open films, datasets) want to add a torrent, see clearly how
it is going, find it later and get on with their day — without learning BitTorrent vocabulary.

## 3. Goals and non-goals

### Goals

1. **The essentials first.** The main screen carries name, size, progress, status, speed and time left; peers,
   trackers and pieces wait one click below in the details band.
2. **Native on GNOME.** Follows libadwaita conventions, the system light/dark preference and the user's accent color.
3. **State at a glance.** Progress color says the state: blue downloading, green seeding, grey paused, red error.
4. **Every state explains itself.** Empty lists, lookups in progress and failures each say what is going on and
   what the user can do about it.
5. **Pure Rust.** Interface and engine in Rust, for memory safety and one toolchain.
6. **Trustworthy.** No ads, no telemetry, no account. Open source under Apache-2.0.

### Non-goals (for 1.0)

- Torrent search or built-in indexers.
- RSS auto-downloading.
- A web UI or remote control daemon (may come after 1.0; see §10).
- Windows or macOS builds.
- Creating and publishing new torrents from the UI.

## 4. Users

| Persona | Needs | What Ferrite gives them |
|---|---|---|
| **Occasional downloader** — fetches distro ISOs, open films, a few times a month | Add a magnet, know when it's done, find the file | Clipboard-prefilled magnet dialog, notifications, "Open folder" |
| **Seeder / archivist** — keeps dozens of torrents seeding for months | Organize, see ratios, limit bandwidth | Labels, sortable columns, ratio column, global speed limits |
| **Data user** — downloads large datasets, picks parts of them | Choose files, see disk space, resume after reboot | File picker before adding, free-space warning, fast resume |

## 5. Scope

### 5.1 Shipped in v0.1.0

**Adding torrents**
- From `.torrent` files (file dialog, Ctrl+O), magnet links (Ctrl+N; prefilled from the clipboard), drag and drop,
  and from the system (`ferrite file.torrent`, `magnet:` links from a browser via the desktop entry).
- Before adding: the torrent's name, destination folder (changeable), file list with checkboxes, total and selected
  size, free space on the drive with a warning when it won't fit. Can be turned off in Settings.
- Duplicates are refused with a plain message.

**Torrent list**
- Hairline-ruled table: #, Name (+ label tag), Size, Progress (bar + %), Status, Down, Up, Time left; optional Peers and
  Ratio. Right-click a header to choose columns; Name, Progress and Status are always shown.
- Click a header to sort, again to reverse; ties fall back to the # order. Live columns re-sort every second without
  losing the selection.
- User order (#): Move up/down (Alt+↑/↓), to top/bottom.
- Multi-select (Ctrl/Shift, Ctrl+A, Esc); Pause/Resume is one button that adapts to the selection.
- Right-click menu: Pause/Resume, Open folder, Copy magnet link, Label ▸, Order ▸, Remove….
- Remove asks for confirmation, names what is affected, and offers to delete the downloaded files.

**Side pane**
- All torrents / Downloading / Completed / Paused, with counts.
- Labels with colors (8 tones readable on light and dark), counts, created from the row menu, removed by right-click.
- Actions menu (Resume all, Pause all) and Settings at the bottom.

**Search**
- In the title bar (Ctrl+F). Matches every word in any order, ignoring case and accents. On the Settings page it
  filters settings instead.

**Details pane** (resizable, remembers its height)
- General: piece bar, percent, transfer stats (time left, downloaded, uploaded, remaining, speeds, peers, ratio),
  saved-to folder, size, file count, pieces, date added, info hash.
- Files: per-file progress and a checkbox to download or skip each file.
- Peers: address, client, down/up speed, flags (incoming, µTP).
- Trackers: list of announce URLs.
- Speed: two-minute download/upload graph.

**Settings**
- Theme (system / light / dark), download folder, choose files before adding, finish notifications, download and
  upload limits (KB/s), listening port, UPnP.

**Engine (librqbit)**
- DHT, trackers, local service discovery, TCP + µTP, UPnP port forwarding, fast resume, session persistence.
  Paused torrents stay paused across restarts. If the port is busy, Ferrite falls back to a free port and says so.

**Other**
- Desktop notification when a download finishes.
- `--demo` mode: sample torrents with simulated transfers, no network, never touches user settings.
- Debian package via `cargo deb`.

### 5.2 Planned

Priority: **P0** blocks 1.0, **P1** wanted for 1.0, **P2** after 1.0.

| # | Requirement | Priority | Notes |
|---|---|---|---|
| R1 | **Download queue**: at most N torrents download at a time (default 3, 0 = no limit); the rest show "Queued" and start in # order. Seeding takes no slot. "Start now" skips the queue. | P0 | Engine has no queue; implement in the app, persisted |
| R2 | **Keep seeding with the window closed**: tray / background mode via the XDG background portal and StatusNotifierItem, with "Quit" in its menu | P0 | Today closing the window quits |
| R3 | **Per-torrent speed limits** (row menu → Speed limit…), shown in General | P1 | librqbit supports per-torrent limits at add time; needs runtime update |
| R4 | **Seeding goals**: stop at ratio X or after N hours, globally and per torrent | P1 | |
| R5 | **Move data** to another folder | P1 | |
| R6 | **Play while downloading**: sequential / first-and-last-pieces-first download, open the largest media file in the default player or stream via a local HTTP URL | P1 | librqbit has streaming support |
| R7 | **After downloads finish**: quit, suspend or power off after downloads (and optionally seeding) finish, with a 60 s cancellable countdown; never saved across runs | P2 | |
| R8 | **No-network notice**: one banner above the table instead of every row looking stuck | P1 | |
| R9 | **Trackers tab**: per-tracker status, peers, last announce, add/remove trackers | P2 | Needs engine API |
| R10 | **Availability bar** in General | P2 | Needs per-piece peer counts from the engine |
| R11 | **Translations** (gettext), starting with German | P2 | |
| R12 | **Flatpak** on Flathub | P1 | Requires a final name and icon |
| R13 | **Remember column widths** and allow dragging column order | P2 | |
| R14 | **Keyboard and screen reader pass** (all actions reachable, row names announced) | P0 | |
| R15 | **Remote control** via the rqbit HTTP API, off by default | P2 | |

## 6. Experience principles

1. **One primary line per row**, secondary text in a quieter color, numbers right-aligned, names trimmed with a
   tooltip.
2. **Platform first.** Use libadwaita widgets (AlertDialog, PreferencesPage, Toast, Banner, StatusPage) before
   custom ones. Custom drawing only where GTK has nothing (piece bar, speed graph).
3. **Plain words.** "Looking up files…" while metadata arrives; "Disk is full — make room, then resume" instead of an
   errno. Sentence case. Same terms everywhere: torrent, pause, resume, remove.
4. **Dialogs** name what they affect and use a verb on the primary button ("Add", "Remove", "Create"); destructive
   actions are red.
5. **Color is never the only signal**: every colored state also has a status word.
6. **Works from 640×480 to full screen**; the side pane collapses on narrow windows.
7. **Material**: warm black (dark) or warm paper (light), square edges, hairlines instead of boxes, depth by
   lightness only. Every measurement in Geist Mono. Colour means state and nothing else: the desktop's accent colour
   (Ferrite's orange when there is none) marks downloading, the primary action and the current selection; sky-blue
   marks seeding and upload, turning green when the accent is blue, teal or slate; grey paused; red error. Tokens in OKLCH, one file per
   theme (`src/palette-*.css`).

## 7. Technical design

| Layer | Choice | Why |
|---|---|---|
| Language | Rust 2024 edition | Safety, one toolchain, good GTK and BitTorrent crates |
| UI | GTK 4.18+ / libadwaita 1.7+ via gtk4-rs | Native on GNOME, accessible, themable with CSS |
| Engine | librqbit 9.x | Mature pure-Rust engine: DHT, µTP, UPnP, fast resume, streaming |
| Async | tokio runtime for the engine; GTK main loop for the UI | Engine actions run on tokio and are awaited from the GTK loop |

**Structure**

- `engine.rs` wraps librqbit and returns plain snapshot structs (`TorrentRow`, `Details`) so the UI never holds
  engine types. A demo engine implements the same surface.
- `item.rs`: each row is a GObject whose properties the table cells bind to, so rows update in place each second.
- A one-second refresh loop syncs the list, updates counts, history, details and notifications.
- App data (labels, order, date added, settings) lives in `~/.config/ferrite/settings.json`, written atomically.
  Engine resume data lives in `~/.local/share/ferrite/session/`.

**Testing**

- Unit tests for formatting.
- An offline round trip through the real engine: create a torrent from a local file, preview, add, verify, check
  details, refuse a duplicate, pause, resume, remove.
- Visual checks with `FERRITE_SCREENSHOT=… ferrite --demo` in light and dark.
- To add: UI tests for actions and dialogs; a two-instance local swarm test that actually transfers data.

## 8. Success metrics

Ferrite has no telemetry, so success is measured by testing and by public signals:

| Metric | Target for 1.0 |
|---|---|
| Time from launch to a magnet download starting (warm DHT) | < 15 s |
| Idle CPU with 50 seeding torrents | < 2 % of one core |
| Memory with 50 torrents | < 150 MB |
| Crash-free sessions in a week of daily use by the maintainer | 100 % |
| Resume after reboot: torrents in the same state, no re-download | 100 % |
| Flathub installs in the first 3 months after listing | 1 000 |
| Open issues labelled "bug" older than 30 days | 0 |

## 9. Risks

| Risk | Impact | Mitigation |
|---|---|---|
| librqbit API changes between major versions | Breaks the build | Pin the version; the wrapper keeps engine types out of the UI |
| No tray standard on GNOME | Background seeding is awkward | Background portal + StatusNotifierItem where supported; clear "keep running" setting |
| Name collision ("Ferrite" is a common word) | Confusion, store rejection | Decide the final name before going public (§10) |
| Look drifts into generic defaults | Ferrite stops feeling like its own product | One token file per theme; the design rules in §6 are checked on every UI change |
| Legal exposure from piracy use | Reputation | No search or indexers; demo and docs use open content only |

## 10. Open questions

1. **Final name and icon** before the repository goes public and before Flathub.
2. **Background seeding**: a tray icon, or a "Ferrite keeps seeding" notification with a Quit action?
3. **Default download queue size** (3?) and whether an upgrade from v0.1 should start with no limit
   so running downloads are never stopped.
4. **Remote control**: worth exposing librqbit's HTTP API for headless use, or out of scope?

## 11. Release plan

| Milestone | Contents |
|---|---|
| v0.1 (done) | Core client as in §5.1, `.deb` |
| v0.2 | R1 queue, R2 background seeding, R8 no-network notice, R14 accessibility pass |
| v0.3 | R3 per-torrent limits, R4 seeding goals, R5 move data, R6 play while downloading |
| 1.0 | Final name, Flatpak (R12), translations started, repository public |
