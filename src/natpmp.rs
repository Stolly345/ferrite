//! NAT-PMP (RFC 6886): asks the router to forward Ferrite's listening port, for routers that speak
//! NAT-PMP instead of (or as well as) UPnP — Apple routers, OpenWrt, pfSense, many others. Maps TCP
//! and UDP (µTP), renews at half the lifetime, and removes the mapping on quit.

use anyhow::{Context, Result, bail};
use std::net::{Ipv4Addr, SocketAddr};
use std::time::Duration;
use tokio::net::UdpSocket;

pub const PORT: u16 = 5351;
const LIFETIME: u32 = 7200;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Proto {
    Udp = 1,
    Tcp = 2,
}

/// A mapping request: version 0, opcode, two reserved bytes, ports, lifetime (0 deletes).
pub fn request(proto: Proto, internal: u16, external: u16, lifetime: u32) -> [u8; 12] {
    let mut b = [0u8; 12];
    b[1] = proto as u8;
    b[4..6].copy_from_slice(&internal.to_be_bytes());
    b[6..8].copy_from_slice(&external.to_be_bytes());
    b[8..12].copy_from_slice(&lifetime.to_be_bytes());
    b
}

#[derive(Debug, PartialEq)]
pub struct Mapping {
    pub internal: u16,
    pub external: u16,
    pub lifetime: u32,
}

/// Reads a mapping response: version 0, opcode 128 + proto, result code, epoch, ports, lifetime.
pub fn parse(proto: Proto, b: &[u8]) -> Result<Mapping> {
    if b.len() < 16 || b[0] != 0 || b[1] != 128 + proto as u8 {
        bail!("not a NAT-PMP mapping response");
    }
    let code = u16::from_be_bytes([b[2], b[3]]);
    if code != 0 {
        let why = match code {
            1 => "unsupported version",
            2 => "refused by the router",
            3 => "router has no network",
            4 => "router is out of resources",
            5 => "unsupported request",
            _ => "unknown error",
        };
        bail!("NAT-PMP error {code}: {why}");
    }
    Ok(Mapping {
        internal: u16::from_be_bytes([b[8], b[9]]),
        external: u16::from_be_bytes([b[10], b[11]]),
        lifetime: u32::from_be_bytes([b[12], b[13], b[14], b[15]]),
    })
}

/// The default IPv4 gateway, from the kernel's routing table (`/proc/net/route` format).
pub fn default_gateway(route_table: &str) -> Option<Ipv4Addr> {
    route_table.lines().skip(1).find_map(|line| {
        let f: Vec<&str> = line.split_whitespace().collect();
        if f.len() > 2 && f[1] == "00000000" && f[2] != "00000000" {
            let g = u32::from_str_radix(f[2], 16).ok()?;
            Some(Ipv4Addr::from(g.to_le_bytes()))
        } else {
            None
        }
    })
}

/// One request with the RFC's retry schedule (250 ms, doubling, a few tries).
pub async fn map(router: SocketAddr, proto: Proto, port: u16, lifetime: u32) -> Result<Mapping> {
    let sock = UdpSocket::bind("0.0.0.0:0").await?;
    sock.connect(router).await?;
    let req = request(proto, port, port, lifetime);
    let mut wait = Duration::from_millis(250);
    for _ in 0..4 {
        sock.send(&req).await?;
        let mut buf = [0u8; 16];
        if let Ok(Ok(n)) = tokio::time::timeout(wait, sock.recv(&mut buf)).await {
            return parse(proto, &buf[..n]);
        }
        wait *= 2;
    }
    bail!("no NAT-PMP answer from {router}")
}

/// Keeps `port` forwarded on the default gateway until `stop` fires, then removes the mapping.
pub async fn keep_mapped(port: u16, stop: tokio_util::sync::CancellationToken) -> Result<()> {
    let table = std::fs::read_to_string("/proc/net/route").context("no routing table")?;
    let gw = default_gateway(&table).context("no default gateway")?;
    let router = SocketAddr::from((gw, PORT));
    loop {
        let mut renew = LIFETIME / 2;
        for proto in [Proto::Tcp, Proto::Udp] {
            let m = map(router, proto, port, LIFETIME).await?;
            renew = renew.min(m.lifetime / 2).max(60);
        }
        tokio::select! {
            _ = stop.cancelled() => break,
            _ = tokio::time::sleep(Duration::from_secs(renew as u64)) => {}
        }
    }
    for proto in [Proto::Tcp, Proto::Udp] {
        let _ = map(router, proto, port, 0).await;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packets() {
        let r = request(Proto::Tcp, 51413, 51413, 7200);
        assert_eq!(r, [0, 2, 0, 0, 0xc8, 0xd5, 0xc8, 0xd5, 0, 0, 0x1c, 0x20]);
        let mut resp = [0u8; 16];
        resp[1] = 130;
        resp[8..10].copy_from_slice(&51413u16.to_be_bytes());
        resp[10..12].copy_from_slice(&40000u16.to_be_bytes());
        resp[12..16].copy_from_slice(&3600u32.to_be_bytes());
        assert_eq!(parse(Proto::Tcp, &resp).unwrap(), Mapping { internal: 51413, external: 40000, lifetime: 3600 });
        resp[3] = 2;
        assert!(parse(Proto::Tcp, &resp).unwrap_err().to_string().contains("refused"));
        assert!(parse(Proto::Udp, &resp).is_err());
    }

    #[test]
    fn gateway_from_route_table() {
        let table = "Iface\tDestination\tGateway \tFlags\n\
                     enp5s0\t0000A8C0\t00000000\t0001\n\
                     enp5s0\t00000000\t0101A8C0\t0003\n";
        assert_eq!(default_gateway(table), Some(Ipv4Addr::new(192, 168, 1, 1)));
    }

    /// A pretend router on localhost answers; the client maps and reads the external port.
    #[tokio::test]
    async fn talks_to_a_router() {
        let router = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let addr = router.local_addr().unwrap();
        tokio::spawn(async move {
            let mut buf = [0u8; 12];
            let (_, from) = router.recv_from(&mut buf).await.unwrap();
            let mut resp = [0u8; 16];
            resp[1] = 128 + buf[1];
            resp[8..10].copy_from_slice(&buf[4..6]);
            resp[10..12].copy_from_slice(&41000u16.to_be_bytes());
            resp[12..16].copy_from_slice(&buf[8..12]);
            router.send_to(&resp, from).await.unwrap();
        });
        let m = map(addr, Proto::Udp, 51413, 7200).await.unwrap();
        assert_eq!(m, Mapping { internal: 51413, external: 41000, lifetime: 7200 });
    }
}
