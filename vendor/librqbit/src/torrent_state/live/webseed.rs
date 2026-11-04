//! Ferrite patch: web seeds (BEP 19). A torrent's `url-list` names HTTP servers that hold the same
//! files; each becomes a pseudo-peer that reserves pieces through the piece tracker like any peer,
//! fetches them with HTTP range requests, and hands them to the same write → verify → complete path
//! peers use. A server that keeps failing backs off and is eventually dropped.

use std::{
    net::SocketAddr,
    sync::{Arc, atomic::Ordering},
    time::Duration,
};

use anyhow::{Context, bail};
use buffers::ByteBuf;
use peer_binary_protocol::Piece;
use tracing::{debug, debug_span, warn};

use super::TorrentStateLive;
use crate::{
    chunk_tracker::ChunkMarkingResult,
    piece_tracker::{AcquireRequest, AcquireResult},
    type_aliases::PeerHandle,
};
use librqbit_core::lengths::ValidPieceIndex;

/// Web seed URLs from a .torrent's `url-list` (a single string or a list of them).
pub fn url_list(torrent: &[u8]) -> Vec<String> {
    use bencode::BencodeValue;
    let Ok(BencodeValue::Dict(top)) = bencode::from_bytes::<BencodeValue<ByteBuf>>(torrent) else {
        return Vec::new();
    };
    let key = ByteBuf(b"url-list");
    let as_url = |v: &BencodeValue<ByteBuf>| match v {
        BencodeValue::Bytes(b) => std::str::from_utf8(b.0).ok().map(str::to_string),
        _ => None,
    };
    let urls: Vec<String> = match top.get(&key) {
        Some(BencodeValue::List(l)) => l.iter().filter_map(as_url).collect(),
        Some(v) => as_url(v).into_iter().collect(),
        None => Vec::new(),
    };
    urls.into_iter().filter(|u| u.starts_with("http://") || u.starts_with("https://")).collect()
}

/// The URL of one file on a web seed (BEP 19): a URL ending in `/` is a folder that holds the
/// torrent's name; otherwise a single-file torrent's URL is the file itself.
pub fn file_url(base: &str, name: &str, multi_file: bool, relative: &std::path::Path) -> String {
    let enc = |s: &str| urlencoding_component(s);
    if !multi_file {
        return if base.ends_with('/') { format!("{base}{}", enc(name)) } else { base.to_string() };
    }
    let mut url = base.to_string();
    if !url.ends_with('/') {
        url.push('/');
    }
    url.push_str(&enc(name));
    for part in relative.components() {
        url.push('/');
        url.push_str(&enc(&part.as_os_str().to_string_lossy()));
    }
    url
}

fn urlencoding_component(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

impl TorrentStateLive {
    /// Starts one worker per web seed in the torrent's `url-list`. Workers stop with the torrent.
    pub(crate) fn start_web_seeds(self: &Arc<Self>) {
        let urls = url_list(&self.metadata.torrent_bytes);
        for (i, url) in urls.into_iter().enumerate() {
            // Pseudo peer handles that no real peer can have (0.0.0.0:port).
            let handle: PeerHandle = SocketAddr::from(([0, 0, 0, 0], 1 + i as u16));
            let this = self.clone();
            self.spawn(
                debug_span!(parent: self.shared.span.clone(), "webseed", url = %url),
                format!("[{}]webseed", self.shared.id),
                async move {
                    if let Err(e) = this.web_seed_loop(handle, url.clone()).await {
                        warn!(%url, "web seed stopped: {e:#}");
                    }
                    Ok(())
                },
            );
        }
    }

    fn release_web_seed(&self, handle: PeerHandle) {
        if let Some(p) = self.lock_write("webseed_release").pieces.as_mut() {
            p.release_pieces_owned_by(handle);
        }
        self.new_pieces_notify.notify_waiters();
    }

    async fn web_seed_loop(self: Arc<Self>, handle: PeerHandle, base: String) -> anyhow::Result<()> {
        let client = reqwest::Client::builder()
            .user_agent(self.shared.client_name_and_version().to_string())
            .connect_timeout(Duration::from_secs(15))
            .timeout(Duration::from_secs(120))
            .build()?;
        let mut failures: u32 = 0;
        loop {
            if self.is_finished() {
                return Ok(());
            }
            let piece = {
                let mut g = self.lock_write("webseed_acquire");
                let super::TorrentStateLocked { pieces, file_priorities, .. } = &mut **g;
                let Some(pieces) = pieces.as_mut() else { return Ok(()) };
                match pieces.acquire_piece(AcquireRequest {
                    peer: handle,
                    peer_avg_time: None,
                    priority_pieces: self.streams.iter_next_pieces(&self.lengths),
                    file_priorities,
                    file_infos: &self.metadata.file_infos,
                    peer_has_piece: |_| true,
                    can_steal: |_| false,
                }) {
                    AcquireResult::Reserved(p) | AcquireResult::Stolen { piece: p, .. } => Some(p),
                    AcquireResult::NoneAvailable => None,
                }
            };
            let Some(piece) = piece else {
                // Everything left is in flight with peers; wait for something to free up.
                tokio::select! {
                    _ = self.new_pieces_notify.notified() => {}
                    _ = tokio::time::sleep(Duration::from_secs(2)) => {}
                }
                continue;
            };
            match self.fetch_piece(&client, &base, piece).await {
                Ok(data) => {
                    failures = 0;
                    let len = data.len() as u64;
                    self.stats.fetched_bytes.fetch_add(len, Ordering::Relaxed);
                    self.session_stats.counters.fetched_bytes.fetch_add(len, Ordering::Relaxed);
                    let this = self.clone();
                    let stored = self
                        .shared
                        .spawner
                        .block_in_place_with_semaphore(move || this.store_piece(handle, piece, &data))
                        .await;
                    if let Err(e) = stored {
                        debug!(piece = piece.get(), "web seed piece rejected: {e:#}");
                        self.release_web_seed(handle);
                        failures += 1;
                    }
                }
                Err(e) => {
                    debug!(piece = piece.get(), "web seed fetch failed: {e:#}");
                    self.release_web_seed(handle);
                    failures += 1;
                }
            }
            if failures > 0 {
                if failures > 12 {
                    bail!("too many failures, giving up on this web seed");
                }
                tokio::time::sleep(Duration::from_secs(1u64 << failures.min(6))).await;
            }
        }
    }

    /// The bytes of one piece, gathered from the files it spans.
    async fn fetch_piece(&self, client: &reqwest::Client, base: &str, piece: ValidPieceIndex) -> anyhow::Result<Vec<u8>> {
        let start = self.lengths.piece_offset(piece);
        let end = start + self.lengths.piece_length(piece) as u64;
        let name = self.metadata.info.name().map(|n| n.into_owned()).unwrap_or_default();
        let multi = self.metadata.file_infos.len() > 1;
        let mut out = Vec::with_capacity((end - start) as usize);
        for fi in self.metadata.file_infos.iter() {
            let (fs, fe) = (fi.offset_in_torrent, fi.offset_in_torrent + fi.len);
            if fi.len == 0 || fe <= start || fs >= end {
                continue;
            }
            let (a, b) = (start.max(fs) - fs, end.min(fe) - fs);
            if fi.attrs.padding {
                out.resize(out.len() + (b - a) as usize, 0);
                continue;
            }
            let url = file_url(base, &name, multi, &fi.relative_filename);
            let resp = client
                .get(&url)
                .header(reqwest::header::RANGE, format!("bytes={}-{}", a, b - 1))
                .send()
                .await
                .with_context(|| format!("GET {url}"))?;
            let status = resp.status();
            let body = resp.bytes().await?;
            let part: &[u8] = match status.as_u16() {
                206 => &body,
                // A server that ignores ranges sends the whole file.
                200 if body.len() as u64 >= b => &body[a as usize..b as usize],
                code => bail!("{url}: HTTP {code}"),
            };
            if part.len() as u64 != b - a {
                bail!("{url}: expected {} bytes, got {}", b - a, part.len());
            }
            out.extend_from_slice(part);
        }
        Ok(out)
    }

    /// Writes a fetched piece chunk by chunk, then verifies and completes it — the same steps a
    /// piece from a peer goes through.
    fn store_piece(&self, handle: PeerHandle, piece: ValidPieceIndex, data: &[u8]) -> anyhow::Result<()> {
        let _hold = self.per_piece_locks[piece.get() as usize].read();
        let mut download_time = None;
        for ci in self.lengths.iter_chunk_infos(piece) {
            {
                let g = self.lock_read("webseed_check_owner");
                match g.get_pieces()?.get_inflight(piece) {
                    Some(i) if i.peer == handle => {}
                    _ => return Ok(()), // someone else finished or took it
                }
            }
            let block = data
                .get(ci.offset as usize..(ci.offset + ci.size) as usize)
                .context("short piece")?;
            let p = Piece::from_data(piece.get(), ci.offset, block);
            self.file_ops().write_chunk(handle, &p, &ci)?;
            let mut g = self.lock_write("webseed_mark_chunk");
            match g.get_pieces_mut()?.mark_chunk_downloaded(&p) {
                Some(ChunkMarkingResult::Completed) => download_time = g.get_pieces_mut()?.take_inflight(piece),
                Some(ChunkMarkingResult::PreviouslyCompleted) => return Ok(()),
                Some(ChunkMarkingResult::NotCompleted) => {}
                None => bail!("chunk doesn't map to the piece"),
            }
        }
        let Some(took) = download_time else { return Ok(()) };
        if self.file_ops().check_piece(piece)? {
            self.lock_write("webseed_hash_ok").get_pieces_mut()?.mark_piece_hash_ok(piece);
            let piece_len = self.lengths.piece_length(piece) as u64;
            self.stats.downloaded_and_checked_bytes.fetch_add(piece_len, Ordering::Release);
            self.stats.downloaded_and_checked_pieces.fetch_add(1, Ordering::Release);
            self.stats.have_bytes.fetch_add(piece_len, Ordering::Relaxed);
            #[allow(clippy::cast_possible_truncation)]
            self.stats.total_piece_download_ms.fetch_add(took.as_millis() as u64, Ordering::Relaxed);
            self.on_piece_completed(piece)?;
            self.transmit_haves(piece);
            Ok(())
        } else {
            self.lock_write("webseed_hash_failed").get_pieces_mut()?.mark_piece_hash_failed(piece);
            self.new_pieces_notify.notify_waiters();
            bail!("piece {} from the web seed failed its hash check", piece.get())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls() {
        let p = std::path::Path::new("sub dir/file 1.bin");
        assert_eq!(file_url("http://h/f.iso", "f.iso", false, p), "http://h/f.iso");
        assert_eq!(file_url("http://h/pub/", "f.iso", false, p), "http://h/pub/f.iso");
        assert_eq!(file_url("http://h/pub", "My Set", true, p), "http://h/pub/My%20Set/sub%20dir/file%201.bin");
    }

    #[test]
    fn reads_url_list() {
        assert_eq!(url_list(b"d8:url-list11:http://a/x/4:infod1:ai1eee"), vec!["http://a/x/"]);
        assert_eq!(url_list(b"d8:url-listl10:http://a/x9:ftp://a/xe4:infod1:ai1eee"), vec!["http://a/x"]);
        assert!(url_list(b"d4:infod1:ai1eee").is_empty());
    }
}
