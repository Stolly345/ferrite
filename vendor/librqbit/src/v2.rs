//! Ferrite patch: BitTorrent v2-only torrents (BEP 52), from .torrent files.
//!
//! The engine's model is v1, so a v2-only torrent is translated when it is read:
//! - its file tree becomes a v1 file list, with padding files where v2 starts each file on a piece
//!   boundary;
//! - its info hash is the SHA-256 of the info dictionary, truncated to 20 bytes — what v2 peers,
//!   trackers and the DHT use;
//! - pieces are verified against the v2 piece layers (SHA-256 Merkle roots over 16 KiB blocks),
//!   through a registry `FileOps` consults before its SHA-1 check;
//! - the original .torrent is kept, so resume data and restarts read it the same way.
//!
//! v2-only magnet links need peers to send piece layers (BEP 52 hash requests); not handled yet.

use std::{
    collections::HashMap,
    sync::{Arc, LazyLock},
};

use anyhow::{Context, bail};
use parking_lot::RwLock;
use sha1w::{ISha256, Sha256};

pub const BLOCK: usize = 16 * 1024;

/// What a v2 piece is checked against.
#[derive(Clone, Debug)]
pub struct V2Piece {
    /// Index in the translated (v1) file list.
    pub file: usize,
    /// Expected Merkle root of the piece.
    pub hash: [u8; 32],
    /// Leaves in the piece's tree (a power of two; padding leaves are zero hashes).
    pub leaves: usize,
}

#[derive(Debug)]
pub struct V2Hashes {
    pub pieces: Vec<V2Piece>,
}

/// v2 torrents known to this process, by their translated `pieces` field's first 20 bytes.
static REGISTRY: LazyLock<RwLock<HashMap<[u8; 20], Arc<V2Hashes>>>> = LazyLock::new(Default::default);

pub fn lookup(pieces_field: &[u8]) -> Option<Arc<V2Hashes>> {
    let key: [u8; 20] = pieces_field.get(..20)?.try_into().ok()?;
    REGISTRY.read().get(&key).cloned()
}

// ---- A small bencode reader that keeps byte spans -----------------------------------------------

#[derive(Debug)]
enum B<'a> {
    Int(i64),
    Bytes(&'a [u8]),
    List(Vec<B<'a>>),
    /// Keys in file order, with each value's raw bytes.
    Dict(Vec<(&'a [u8], B<'a>, &'a [u8])>),
}

fn parse<'a>(buf: &'a [u8], pos: &mut usize) -> anyhow::Result<B<'a>> {
    let c = *buf.get(*pos).context("bencode: unexpected end")?;
    match c {
        b'i' => {
            let end = buf[*pos..].iter().position(|&b| b == b'e').context("bencode: bad int")? + *pos;
            let n = std::str::from_utf8(&buf[*pos + 1..end])?.parse()?;
            *pos = end + 1;
            Ok(B::Int(n))
        }
        b'l' => {
            *pos += 1;
            let mut v = Vec::new();
            while buf.get(*pos) != Some(&b'e') {
                v.push(parse(buf, pos)?);
            }
            *pos += 1;
            Ok(B::List(v))
        }
        b'd' => {
            *pos += 1;
            let mut v = Vec::new();
            while buf.get(*pos) != Some(&b'e') {
                let B::Bytes(k) = parse(buf, pos)? else { bail!("bencode: dict key not a string") };
                let start = *pos;
                let val = parse(buf, pos)?;
                v.push((k, val, &buf[start..*pos]));
            }
            *pos += 1;
            Ok(B::Dict(v))
        }
        b'0'..=b'9' => {
            let colon = buf[*pos..].iter().position(|&b| b == b':').context("bencode: bad string")? + *pos;
            let len: usize = std::str::from_utf8(&buf[*pos..colon])?.parse()?;
            let s = buf.get(colon + 1..colon + 1 + len).context("bencode: short string")?;
            *pos = colon + 1 + len;
            Ok(B::Bytes(s))
        }
        _ => bail!("bencode: unexpected byte {c}"),
    }
}

impl<'a> B<'a> {
    fn get(&self, key: &[u8]) -> Option<&B<'a>> {
        match self {
            B::Dict(d) => d.iter().find(|(k, _, _)| *k == key).map(|(_, v, _)| v),
            _ => None,
        }
    }
    fn raw(&self, key: &[u8]) -> Option<&'a [u8]> {
        match self {
            B::Dict(d) => d.iter().find(|(k, _, _)| *k == key).map(|(_, _, r)| *r),
            _ => None,
        }
    }
    fn int(&self) -> Option<i64> {
        if let B::Int(n) = self { Some(*n) } else { None }
    }
    fn bytes(&self) -> Option<&'a [u8]> {
        if let B::Bytes(b) = self { Some(b) } else { None }
    }
}

/// The raw bytes of a top-level key of a .torrent (e.g. `piece layers`), if present.
pub fn top_level_raw<'a>(torrent: &'a [u8], key: &[u8]) -> Option<&'a [u8]> {
    let mut pos = 0;
    parse(torrent, &mut pos).ok()?.raw(key)
}

/// Whether these .torrent bytes are a v2-only torrent (a file tree and no v1 piece hashes).
pub fn is_v2_only(torrent: &[u8]) -> bool {
    let mut pos = 0;
    let Ok(top) = parse(torrent, &mut pos) else { return false };
    top.get(b"info").is_some_and(|i| i.get(b"file tree").is_some() && i.get(b"pieces").is_none())
}

// ---- Merkle trees ---------------------------------------------------------------------------------

fn sha256(parts: &[&[u8]]) -> [u8; 32] {
    let mut h = Sha256::new();
    for p in parts {
        h.update(p);
    }
    h.finish()
}

/// Root of a tree over `leaves`, padded with zero hashes to `width` (a power of two).
pub fn merkle_root(mut level: Vec<[u8; 32]>, width: usize) -> [u8; 32] {
    level.resize(width.max(1), [0u8; 32]);
    while level.len() > 1 {
        level = level.chunks(2).map(|p| sha256(&[&p[0], &p[1]])).collect();
    }
    level[0]
}

/// The v2 hash of one piece's data (the file's bytes only, never padding).
pub fn piece_root(data: &[u8], leaves: usize) -> [u8; 32] {
    let blocks: Vec<[u8; 32]> = data.chunks(BLOCK).map(|b| sha256(&[b])).collect();
    merkle_root(blocks, leaves)
}

// ---- Translation ----------------------------------------------------------------------------------

pub struct Translated {
    /// A v1 .torrent describing the same bytes on disk (with padding files).
    pub v1_torrent: Vec<u8>,
    /// SHA-256 of the v2 info dictionary, truncated to 20 bytes.
    pub info_hash: [u8; 20],
    /// The original v2 info dictionary, byte for byte.
    pub info_bytes: Vec<u8>,
}

struct V2File {
    path: Vec<Vec<u8>>,
    len: u64,
    root: Option<[u8; 32]>,
}

fn walk(node: &B<'_>, prefix: &mut Vec<Vec<u8>>, out: &mut Vec<V2File>) -> anyhow::Result<()> {
    let B::Dict(entries) = node else { bail!("v2: file tree entry is not a dictionary") };
    for (name, child, _) in entries {
        if name.is_empty() {
            let len = child.get(b"length").and_then(B::int).context("v2: file without length")? as u64;
            let root = match child.get(b"pieces root").and_then(B::bytes) {
                Some(r) if r.len() == 32 => Some(r.try_into().unwrap()),
                Some(_) => bail!("v2: bad pieces root"),
                None => None,
            };
            out.push(V2File { path: prefix.clone(), len, root });
        } else {
            prefix.push(name.to_vec());
            walk(child, prefix, out)?;
            prefix.pop();
        }
    }
    Ok(())
}

fn bstr(out: &mut Vec<u8>, b: &[u8]) {
    out.extend_from_slice(b.len().to_string().as_bytes());
    out.push(b':');
    out.extend_from_slice(b);
}

/// Translates a v2-only .torrent and registers its piece hashes. Errors on anything malformed,
/// and when the piece layers needed to check pieces are missing.
pub fn translate(torrent: &[u8]) -> anyhow::Result<Translated> {
    let mut pos = 0;
    let top = parse(torrent, &mut pos)?;
    let info = top.get(b"info").context("v2: no info")?;
    let info_bytes = top.raw(b"info").context("v2: no info")?.to_vec();
    if info.get(b"meta version").and_then(B::int) != Some(2) {
        bail!("v2: meta version is not 2");
    }
    let name = info.get(b"name").and_then(B::bytes).context("v2: no name")?;
    let piece_len = info.get(b"piece length").and_then(B::int).context("v2: no piece length")? as u64;
    if piece_len < BLOCK as u64 || !piece_len.is_power_of_two() {
        bail!("v2: piece length {piece_len} is not a power of two of at least 16 KiB");
    }
    let mut files = Vec::new();
    walk(info.get(b"file tree").context("v2: no file tree")?, &mut Vec::new(), &mut files)?;
    if files.is_empty() {
        bail!("v2: no files");
    }
    // Single-file torrents name the file after the torrent itself; keep that as the file's path.
    let layers = top.get(b"piece layers");

    let last_nonempty = files.iter().rposition(|f| f.len > 0);
    let mut v1_files: Vec<u8> = Vec::new();
    let mut pieces: Vec<V2Piece> = Vec::new();
    let mut v1_index = 0usize;
    let leaves_per_piece = (piece_len as usize) / BLOCK;
    for (i, f) in files.iter().enumerate() {
        // A file entry: d 6:length i..e 4:path l ... e e
        v1_files.push(b'd');
        bstr(&mut v1_files, b"length");
        v1_files.extend_from_slice(format!("i{}e", f.len).as_bytes());
        bstr(&mut v1_files, b"path");
        v1_files.push(b'l');
        for part in &f.path {
            bstr(&mut v1_files, part);
        }
        v1_files.extend_from_slice(b"ee");
        let this_file = v1_index;
        v1_index += 1;

        if f.len > 0 {
            let root = f.root.context("v2: file without pieces root")?;
            let n_pieces = f.len.div_ceil(piece_len) as usize;
            if n_pieces == 1 {
                // One piece: the file's root is that piece's hash, over the next power of two of its blocks.
                let blocks = (f.len as usize).div_ceil(BLOCK);
                pieces.push(V2Piece { file: this_file, hash: root, leaves: blocks.next_power_of_two() });
            } else {
                let layer = layers
                    .and_then(|l| l.get(&root))
                    .and_then(B::bytes)
                    .context("v2: piece layers are missing from the .torrent")?;
                if layer.len() != n_pieces * 32 {
                    bail!("v2: piece layer has the wrong length");
                }
                // The layer must hash up to the file's root.
                let hashes: Vec<[u8; 32]> = layer.chunks(32).map(|c| c.try_into().unwrap()).collect();
                let width = (f.len.div_ceil(BLOCK as u64) as usize).next_power_of_two() / leaves_per_piece;
                let pad_node = merkle_root(Vec::new(), leaves_per_piece);
                let mut level = hashes.clone();
                level.resize(width.max(n_pieces).next_power_of_two(), pad_node);
                while level.len() > 1 {
                    level = level.chunks(2).map(|p| sha256(&[&p[0], &p[1]])).collect();
                }
                if level[0] != root {
                    bail!("v2: piece layer doesn't match the file's root");
                }
                for h in hashes {
                    pieces.push(V2Piece { file: this_file, hash: h, leaves: leaves_per_piece });
                }
            }
            // Pad to the next piece boundary, as v2 lays files out.
            let rem = f.len % piece_len;
            if rem != 0 && Some(i) != last_nonempty {
                let pad = piece_len - rem;
                v1_files.push(b'd');
                bstr(&mut v1_files, b"attr");
                bstr(&mut v1_files, b"p");
                bstr(&mut v1_files, b"length");
                v1_files.extend_from_slice(format!("i{pad}e").as_bytes());
                bstr(&mut v1_files, b"path");
                v1_files.push(b'l');
                bstr(&mut v1_files, b".pad");
                bstr(&mut v1_files, pad.to_string().as_bytes());
                v1_files.extend_from_slice(b"ee");
                v1_index += 1;
            }
        }
    }
    if pieces.is_empty() {
        bail!("v2: torrent has no data");
    }

    // The translated `pieces` field: the first 20 bytes of each v2 piece hash. It is never
    // compared as SHA-1 — FileOps finds the v2 hashes by it.
    let mut pieces_field = Vec::with_capacity(pieces.len() * 20);
    for p in &pieces {
        pieces_field.extend_from_slice(&p.hash[..20]);
    }
    let mut v1_info = b"d".to_vec();
    bstr(&mut v1_info, b"files");
    v1_info.push(b'l');
    v1_info.extend_from_slice(&v1_files);
    v1_info.push(b'e');
    bstr(&mut v1_info, b"name");
    bstr(&mut v1_info, name);
    bstr(&mut v1_info, b"piece length");
    v1_info.extend_from_slice(format!("i{piece_len}e").as_bytes());
    bstr(&mut v1_info, b"pieces");
    bstr(&mut v1_info, &pieces_field);
    v1_info.push(b'e');

    // Keep the announce keys so trackers still work; everything else lives in the original.
    let mut v1_torrent = b"d".to_vec();
    for key in [b"announce".as_slice(), b"announce-list"] {
        if let Some(raw) = top.raw(key) {
            bstr(&mut v1_torrent, key);
            v1_torrent.extend_from_slice(raw);
        }
    }
    bstr(&mut v1_torrent, b"info");
    v1_torrent.extend_from_slice(&v1_info);
    v1_torrent.push(b'e');

    let mut info_hash = [0u8; 20];
    info_hash.copy_from_slice(&sha256(&[&info_bytes])[..20]);
    let key: [u8; 20] = pieces_field[..20].try_into().unwrap();
    REGISTRY.write().insert(key, Arc::new(V2Hashes { pieces }));
    Ok(Translated { v1_torrent, info_hash, info_bytes })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merkle_basics() {
        // One block: the root is that block's hash.
        let data = vec![7u8; 1000];
        assert_eq!(piece_root(&data, 1), sha256(&[&data]));
        // Two blocks padded to four leaves.
        let data = vec![1u8; BLOCK + 10];
        let l0 = sha256(&[&data[..BLOCK]]);
        let l1 = sha256(&[&data[BLOCK..]]);
        let z = [0u8; 32];
        let want = sha256(&[&sha256(&[&l0, &l1]), &sha256(&[&z, &z])]);
        assert_eq!(piece_root(&data, 4), want);
    }
}
