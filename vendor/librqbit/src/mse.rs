//! Ferrite patch: Message Stream Encryption / Protocol Encryption (MSE/PE), the "BitTorrent
//! encryption" of µTorrent, libtorrent (qBittorrent, Deluge), Transmission and others. A
//! Diffie-Hellman exchange over the 768-bit MODP prime gives a shared secret S; both directions are
//! then RC4 streams keyed from S and the info hash (first 1024 bytes of keystream discarded). It
//! hides BitTorrent from providers that slow it down; it is obfuscation, not strong cryptography.
//!
//! Modes (set once at start, see [`set_mode`]): Off never initiates encryption; Prefer (default)
//! tries encrypted first and falls back to plaintext; Require refuses plaintext both ways. Encrypted
//! incoming connections are always accepted.

use std::{
    io::IoSliceMut,
    pin::Pin,
    sync::atomic::{AtomicU8, Ordering},
    task::{Context, Poll},
    time::Duration,
};

use anyhow::{Context as _, bail};
use num_bigint::BigUint;
use rand::RngExt;
use sha1w::{ISha1, Sha1};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, ReadBuf};

use crate::{
    type_aliases::{BoxAsyncReadVectored, BoxAsyncWrite},
    vectored_traits::AsyncReadVectored,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Mode {
    Off = 0,
    Prefer = 1,
    Require = 2,
}

static MODE: AtomicU8 = AtomicU8::new(Mode::Prefer as u8);

pub fn set_mode(mode: Mode) {
    MODE.store(mode as u8, Ordering::Relaxed);
}

pub fn mode() -> Mode {
    match MODE.load(Ordering::Relaxed) {
        0 => Mode::Off,
        2 => Mode::Require,
        _ => Mode::Prefer,
    }
}

const P_HEX: &[u8] = b"FFFFFFFFFFFFFFFFC90FDAA22168C234C4C6628B80DC1CD129024E088A67CC74020BBEA63B139B22514A08798E3404DDEF9519B3CD3A431B302B0A6DF25F14374FE1356D6D51C245E485B576625E7EC6F44C42E9A63A36210000000000090563";
const VC: [u8; 8] = [0; 8];
const CRYPTO_PLAIN: u32 = 0x01;
const CRYPTO_RC4: u32 = 0x02;
const MAX_PAD: usize = 512;
const TIMEOUT: Duration = Duration::from_secs(15);

fn prime() -> BigUint {
    BigUint::parse_bytes(P_HEX, 16).unwrap()
}

/// A DH key pair: a random 160-bit private key and Y = 2^X mod P as 96 big-endian bytes.
fn keypair() -> (BigUint, [u8; 96]) {
    let mut x = [0u8; 20];
    for b in &mut x {
        *b = rand::rng().random();
    }
    let x = BigUint::from_bytes_be(&x);
    let y = BigUint::from(2u32).modpow(&x, &prime());
    (x, to_96(&y))
}

fn to_96(n: &BigUint) -> [u8; 96] {
    let b = n.to_bytes_be();
    let mut out = [0u8; 96];
    out[96 - b.len()..].copy_from_slice(&b);
    out
}

fn secret(their_y: &[u8], my_x: &BigUint) -> [u8; 96] {
    to_96(&BigUint::from_bytes_be(their_y).modpow(my_x, &prime()))
}

fn sha1(parts: &[&[u8]]) -> [u8; 20] {
    let mut h = Sha1::new();
    for p in parts {
        h.update(p);
    }
    h.finish()
}

fn random_pad() -> Vec<u8> {
    let len = rand::rng().random::<u16>() as usize % (MAX_PAD + 1);
    (0..len).map(|_| rand::rng().random()).collect()
}

/// RC4 with the first 1024 bytes of keystream discarded, as MSE specifies.
#[derive(Clone)]
pub struct Rc4 {
    s: [u8; 256],
    i: u8,
    j: u8,
}

impl Rc4 {
    pub fn new(key: &[u8]) -> Self {
        let mut s = [0u8; 256];
        for (i, v) in s.iter_mut().enumerate() {
            *v = i as u8;
        }
        let mut j: u8 = 0;
        for i in 0..256 {
            j = j.wrapping_add(s[i]).wrapping_add(key[i % key.len()]);
            s.swap(i, j as usize);
        }
        let mut rc4 = Rc4 { s, i: 0, j: 0 };
        rc4.apply(&mut [0u8; 1024]);
        rc4
    }

    pub fn apply(&mut self, data: &mut [u8]) {
        for b in data {
            self.i = self.i.wrapping_add(1);
            self.j = self.j.wrapping_add(self.s[self.i as usize]);
            self.s.swap(self.i as usize, self.j as usize);
            let k = self.s[(self.s[self.i as usize].wrapping_add(self.s[self.j as usize])) as usize];
            *b ^= k;
        }
    }
}

/// A reader that first returns `prefix` (already plain), then the inner stream, decrypted when an
/// RC4 key is set.
pub struct MseReader<R> {
    inner: R,
    rc4: Option<Rc4>,
    prefix: Vec<u8>,
    pos: usize,
}

impl<R> MseReader<R> {
    pub fn new(inner: R, rc4: Option<Rc4>, prefix: Vec<u8>) -> Self {
        Self { inner, rc4, prefix, pos: 0 }
    }
}

impl<R: AsyncRead + Unpin> AsyncRead for MseReader<R> {
    fn poll_read(self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &mut ReadBuf<'_>) -> Poll<std::io::Result<()>> {
        let this = self.get_mut();
        if this.pos < this.prefix.len() {
            let n = (this.prefix.len() - this.pos).min(buf.remaining());
            buf.put_slice(&this.prefix[this.pos..this.pos + n]);
            this.pos += n;
            return Poll::Ready(Ok(()));
        }
        let before = buf.filled().len();
        std::task::ready!(Pin::new(&mut this.inner).poll_read(cx, buf))?;
        if let Some(rc4) = this.rc4.as_mut() {
            rc4.apply(&mut buf.filled_mut()[before..]);
        }
        Poll::Ready(Ok(()))
    }
}

impl<R: AsyncRead + Unpin> AsyncReadVectored for MseReader<R> {
    fn poll_read_vectored(self: Pin<&mut Self>, cx: &mut Context<'_>, vec: &mut [IoSliceMut<'_>]) -> Poll<std::io::Result<usize>> {
        let Some(slice) = vec.iter_mut().find(|s| !s.is_empty()) else { return Poll::Ready(Ok(0)) };
        let mut rb = ReadBuf::new(slice);
        std::task::ready!(self.poll_read(cx, &mut rb))?;
        Poll::Ready(Ok(rb.filled().len()))
    }
}

/// A writer that encrypts with RC4 when a key is set. It never buffers: each write encrypts a copy
/// with a clone of the cipher, and the real cipher advances by exactly the bytes the socket took,
/// so a partial write can't leave encrypted bytes waiting for a flush that may never come.
pub struct MseWriter<W> {
    inner: W,
    rc4: Option<Rc4>,
    scratch: Vec<u8>,
}

impl<W> MseWriter<W> {
    pub fn new(inner: W, rc4: Option<Rc4>) -> Self {
        Self { inner, rc4, scratch: Vec::new() }
    }
}

impl<W: AsyncWrite + Unpin> AsyncWrite for MseWriter<W> {
    fn poll_write(self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &[u8]) -> Poll<std::io::Result<usize>> {
        let this = self.get_mut();
        let Some(rc4) = this.rc4.as_mut() else {
            return Pin::new(&mut this.inner).poll_write(cx, buf);
        };
        this.scratch.clear();
        this.scratch.extend_from_slice(buf);
        rc4.clone().apply(&mut this.scratch);
        let n = std::task::ready!(Pin::new(&mut this.inner).poll_write(cx, &this.scratch))?;
        // Move the real keystream forward over what was sent.
        rc4.apply(&mut this.scratch[..n]);
        Poll::Ready(Ok(n))
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_flush(cx)
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_shutdown(cx)
    }
}

/// Reads until `pattern` appears within `limit` bytes; returns the bytes after it.
async fn sync_on(r: &mut BoxAsyncReadVectored, buf: &mut Vec<u8>, pattern: &[u8], limit: usize) -> anyhow::Result<Vec<u8>> {
    loop {
        if let Some(i) = buf.windows(pattern.len()).position(|w| w == pattern) {
            return Ok(buf[i + pattern.len()..].to_vec());
        }
        if buf.len() >= limit + pattern.len() {
            bail!("MSE: sync pattern not found");
        }
        let mut chunk = [0u8; 600];
        let n = r.read(&mut chunk).await?;
        if n == 0 {
            bail!("MSE: connection closed during handshake");
        }
        buf.extend_from_slice(&chunk[..n]);
    }
}

/// Reads exactly `n` bytes, taking first from `have` (leftovers already read).
async fn take(r: &mut BoxAsyncReadVectored, have: &mut Vec<u8>, n: usize) -> anyhow::Result<Vec<u8>> {
    while have.len() < n {
        let mut chunk = vec![0u8; n - have.len()];
        r.read_exact(&mut chunk).await?;
        have.extend_from_slice(&chunk);
    }
    let rest = have.split_off(n);
    Ok(std::mem::replace(have, rest))
}

/// Outgoing side (A). Returns the wrapped streams, ready for the BitTorrent handshake.
pub async fn initiate(
    mut r: BoxAsyncReadVectored,
    mut w: BoxAsyncWrite,
    info_hash: [u8; 20],
) -> anyhow::Result<(BoxAsyncReadVectored, BoxAsyncWrite)> {
    tokio::time::timeout(TIMEOUT, async {
        let (xa, ya) = keypair();
        w.write_all(&[ya.as_slice(), &random_pad()].concat()).await?;
        let mut yb = [0u8; 96];
        r.read_exact(&mut yb).await?;
        let s = secret(&yb, &xa);

        let req1 = sha1(&[b"req1", &s]);
        let req2 = sha1(&[b"req2", &info_hash]);
        let req3 = sha1(&[b"req3", &s]);
        let skey_x: Vec<u8> = req2.iter().zip(req3).map(|(a, b)| a ^ b).collect();
        let mut enc = Rc4::new(&sha1(&[b"keyA", &s, &info_hash]));
        let mut dec = Rc4::new(&sha1(&[b"keyB", &s, &info_hash]));

        let provide = if mode() == Mode::Require { CRYPTO_RC4 } else { CRYPTO_RC4 | CRYPTO_PLAIN };
        let mut body = [VC.as_slice(), &provide.to_be_bytes(), &0u16.to_be_bytes(), &0u16.to_be_bytes()].concat();
        enc.apply(&mut body);
        w.write_all(&[req1.as_slice(), &skey_x, &body].concat()).await?;

        // B answers with PadB, then ENCRYPT(VC, crypto_select, len(PadD), PadD).
        let mut vc = VC;
        dec.apply(&mut vc);
        let mut rest = sync_on(&mut r, &mut Vec::new(), &vc, MAX_PAD).await?;
        let mut head = take(&mut r, &mut rest, 6).await?;
        dec.apply(&mut head);
        let select = u32::from_be_bytes(head[0..4].try_into().unwrap());
        let pad_d = u16::from_be_bytes([head[4], head[5]]) as usize;
        if pad_d > MAX_PAD {
            bail!("MSE: bad padding length");
        }
        let mut pad = take(&mut r, &mut rest, pad_d).await?;
        dec.apply(&mut pad);
        match select {
            CRYPTO_RC4 => {
                dec.apply(&mut rest);
                Ok::<_, anyhow::Error>((
                    Box::new(MseReader::new(r, Some(dec), rest)) as BoxAsyncReadVectored,
                    Box::new(MseWriter::new(w, Some(enc))) as BoxAsyncWrite,
                ))
            }
            CRYPTO_PLAIN if mode() != Mode::Require => Ok((
                Box::new(MseReader::new(r, None, rest)) as BoxAsyncReadVectored,
                Box::new(MseWriter::new(w, None)) as BoxAsyncWrite,
            )),
            other => bail!("MSE: peer chose method {other:#x}"),
        }
    })
    .await
    .context("MSE handshake timed out")?
}

/// Incoming side (B), after the first bytes (`first`, not a plaintext handshake) were read.
/// `info_hashes` are the torrents we serve. Returns the wrapped streams, whose first bytes are the
/// peer's BitTorrent handshake.
pub async fn respond(
    mut r: BoxAsyncReadVectored,
    mut w: BoxAsyncWrite,
    first: Vec<u8>,
    info_hashes: Vec<[u8; 20]>,
) -> anyhow::Result<(BoxAsyncReadVectored, BoxAsyncWrite)> {
    tokio::time::timeout(TIMEOUT, async {
        let mut have = first;
        let ya = take(&mut r, &mut have, 96).await?;
        let (xb, yb) = keypair();
        w.write_all(&[yb.as_slice(), &random_pad()].concat()).await?;
        let s = secret(&ya, &xb);

        // A sends PadA, then HASH('req1', S): find it.
        let req1 = sha1(&[b"req1", &s]);
        let mut rest = sync_on(&mut r, &mut have, &req1, MAX_PAD).await?;
        let x = take(&mut r, &mut rest, 20).await?;
        let req3 = sha1(&[b"req3", &s]);
        let skey = info_hashes
            .into_iter()
            .find(|ih| sha1(&[b"req2", ih]).iter().zip(req3).map(|(a, b)| a ^ b).eq(x.iter().copied()))
            .context("MSE: peer asked for a torrent we don't have")?;
        let mut dec = Rc4::new(&sha1(&[b"keyA", &s, &skey]));
        let mut enc = Rc4::new(&sha1(&[b"keyB", &s, &skey]));

        let mut head = take(&mut r, &mut rest, 14).await?;
        dec.apply(&mut head);
        if head[0..8] != VC {
            bail!("MSE: bad verification constant");
        }
        let provide = u32::from_be_bytes(head[8..12].try_into().unwrap());
        let pad_c = u16::from_be_bytes([head[12], head[13]]) as usize;
        if pad_c > MAX_PAD {
            bail!("MSE: bad padding length");
        }
        let mut pad = take(&mut r, &mut rest, pad_c).await?;
        dec.apply(&mut pad);
        let mut ia_len = take(&mut r, &mut rest, 2).await?;
        dec.apply(&mut ia_len);
        let mut ia = take(&mut r, &mut rest, u16::from_be_bytes([ia_len[0], ia_len[1]]) as usize).await?;
        dec.apply(&mut ia);

        let select = if provide & CRYPTO_RC4 != 0 {
            CRYPTO_RC4
        } else if provide & CRYPTO_PLAIN != 0 && mode() != Mode::Require {
            CRYPTO_PLAIN
        } else {
            bail!("MSE: no method in common");
        };
        let mut reply = [VC.as_slice(), &select.to_be_bytes(), &0u16.to_be_bytes()].concat();
        enc.apply(&mut reply);
        w.write_all(&reply).await?;

        // What follows the handshake bytes is the stream itself, in the chosen method.
        if select == CRYPTO_RC4 {
            dec.apply(&mut rest);
            ia.extend_from_slice(&rest);
            Ok::<_, anyhow::Error>((
                Box::new(MseReader::new(r, Some(dec), ia)) as BoxAsyncReadVectored,
                Box::new(MseWriter::new(w, Some(enc))) as BoxAsyncWrite,
            ))
        } else {
            ia.extend_from_slice(&rest);
            Ok((
                Box::new(MseReader::new(r, None, ia)) as BoxAsyncReadVectored,
                Box::new(MseWriter::new(w, None)) as BoxAsyncWrite,
            ))
        }
    })
    .await
    .context("MSE handshake timed out")?
}

/// Puts already-read bytes back in front of a stream (a plaintext handshake's first byte).
pub fn prefixed(r: BoxAsyncReadVectored, prefix: Vec<u8>) -> BoxAsyncReadVectored {
    Box::new(MseReader::new(r, None, prefix))
}
