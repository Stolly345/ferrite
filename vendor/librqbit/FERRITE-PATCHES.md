# Ferrite patches to librqbit 9.1.0

librqbit is Apache-2.0 (see its upstream repository, https://github.com/ikatson/rqbit). This copy is vendored
so Ferrite can extend the engine. Every change is marked `Ferrite patch` in the source.

- `TorrentStateLive::ratelimits()` — per-torrent rate limits can be read and changed while a torrent runs.
- `build.rs` — skips `npm install && npm run build` when `webui/dist` is already built; the built web UI (4 files, rqbit's own, Apache-2.0) is checked in so Ferrite builds without Node.
- `TorrentStateLive::set_file_priorities()` / `file_priorities()` — change the file download order while a torrent runs (Ferrite's High / Normal / Low file priorities).
- `Session::banned` — a set of banned IP addresses, refused for incoming and outgoing connections like the blocklist; `TorrentStateLive::drop_peers_at()` disconnects a banned peer that is already connected.
- `PeerStats::have_pieces` — how many pieces each live peer has, for the peer's progress.
- Web seeds (BEP 19), `src/torrent_state/live/webseed.rs`: each `url-list` server becomes a pseudo-peer that reserves pieces through the piece tracker, fetches them with HTTP range requests and writes/verifies them like a peer's. Started automatically when a torrent goes live.
- Protocol encryption (MSE/PE), `src/mse.rs`: DH over the 768-bit MODP prime + RC4 (1024 bytes dropped). Outgoing connections try it first (fallback to plaintext in Prefer mode); incoming connections are told apart by their first byte. Hooks in `peer_connection.rs` (outgoing) and `session.rs` (`check_incoming_connection`). Mode via `librqbit::mse::set_mode`.
