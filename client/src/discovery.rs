//! Local-link discovery. Chat traffic still travels directly over TCP.
use crate::network::Network;
use serde::{Deserialize, Serialize};
use socket2::{Domain, Protocol, Socket, Type};
use std::{
    io,
    net::{Ipv4Addr, SocketAddr, SocketAddrV4, UdpSocket},
    sync::Arc,
    thread,
    time::{Duration, Instant},
};

const GROUP: Ipv4Addr = Ipv4Addr::new(239, 255, 74, 67);
const PORT: u16 = 45873;
const PROTOCOL: &str = "jazachat-lan-v1";

#[derive(Serialize, Deserialize)]
struct Announcement {
    protocol: String,
    identity: String,
    port: u16,
}

fn decode(bytes: &[u8], source: SocketAddr) -> Option<(String, SocketAddr)> {
    let announcement: Announcement = serde_json::from_slice(bytes).ok()?;
    if announcement.protocol != PROTOCOL
        || announcement.port == 0
        || announcement.identity.is_empty()
        || announcement.identity.len() > 128
        || !source.is_ipv4()
        || source.ip().is_unspecified()
        || source.ip().is_multicast()
    {
        return None;
    }
    // Use the packet source, never an address supplied by an announcement.
    Some((
        announcement.identity,
        SocketAddr::new(source.ip(), announcement.port),
    ))
}

pub fn start(network: &Arc<Network>) -> io::Result<()> {
    if !network.address.is_ipv4() || !network.address.ip().is_unspecified() {
        return Err(io::Error::new(io::ErrorKind::InvalidInput,
            "Auto-discovery requires --listen 0.0.0.0:port; use --no-discovery for a specific interface"));
    }
    let socket = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
    socket.set_reuse_address(true)?;
    socket.bind(&SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, PORT).into())?;
    let socket: UdpSocket = socket.into();
    socket.join_multicast_v4(&GROUP, &Ipv4Addr::UNSPECIFIED)?;
    socket.set_multicast_ttl_v4(1)?;
    socket.set_multicast_loop_v4(true)?;
    socket.set_read_timeout(Some(Duration::from_millis(200)))?;
    let announcement = serde_json::to_vec(&Announcement {
        protocol: PROTOCOL.into(),
        identity: network.identity.clone(),
        port: network.address.port(),
    })?;
    // Report an unusable multicast route at startup instead of silently waiting.
    socket.send_to(&announcement, (GROUP, PORT))?;
    let weak = Arc::downgrade(network);
    thread::spawn(move || {
        let mut last_sent = Instant::now();
        let mut bytes = [0u8; 1024];
        loop {
            let Some(network) = weak.upgrade() else {
                break;
            };
            if last_sent.elapsed() >= Duration::from_secs(1) {
                let _ = socket.send_to(&announcement, (GROUP, PORT));
                last_sent = Instant::now();
            }
            match socket.recv_from(&mut bytes) {
                Ok((len, source)) => {
                    if let Some((identity, address)) = decode(&bytes[..len], source) {
                        network.connect_discovered(identity, address);
                    }
                }
                Err(e)
                    if matches!(
                        e.kind(),
                        io::ErrorKind::WouldBlock
                            | io::ErrorKind::TimedOut
                            | io::ErrorKind::Interrupted
                    ) => {}
                Err(_) => break,
            }
        }
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::network::Event;

    #[test]
    fn validates_announcements() {
        let source = "192.168.1.10:45873".parse().unwrap();
        let valid = br#"{"protocol":"jazachat-lan-v1","identity":"alice","port":1234}"#;
        assert_eq!(
            decode(valid, source),
            Some(("alice".into(), "192.168.1.10:1234".parse().unwrap()))
        );
        assert!(decode(b"not json", source).is_none());
        assert!(decode(
            br#"{"protocol":"other","identity":"a","port":1234}"#,
            source
        )
        .is_none());
        assert!(decode(
            br#"{"protocol":"jazachat-lan-v1","identity":"a","port":0}"#,
            source
        )
        .is_none());
    }

    #[test]
    fn discovers_three_clients_without_addresses_and_reconnects() {
        let (a, _arx) = Network::bind("0.0.0.0:0").unwrap();
        let (b, brx) = Network::bind("0.0.0.0:0").unwrap();
        let (c, crx) = Network::bind("0.0.0.0:0").unwrap();
        for peer in [&a, &b, &c] {
            start(peer).unwrap();
        }
        let deadline = Instant::now() + Duration::from_secs(8);
        while [&a, &b, &c].iter().any(|peer| peer.count() != 2) {
            assert!(Instant::now() < deadline, "automatic mesh did not form");
            thread::sleep(Duration::from_millis(50));
        }
        a.send("alice", "automatically connected");
        for rx in [&brx, &crx] {
            loop {
                if let Event::Message(message) = rx.recv_timeout(Duration::from_secs(2)).unwrap() {
                    assert_eq!(message.text, "automatically connected");
                    break;
                }
            }
        }
        // A broken link should recover from the next announcement.
        a.disconnect_all_for_test();
        let deadline = Instant::now() + Duration::from_secs(8);
        loop {
            if [&a, &b, &c].iter().all(|peer| peer.count() == 2) {
                break;
            }
            assert!(Instant::now() < deadline, "automatic reconnect failed");
            thread::sleep(Duration::from_millis(50));
        }
        c.send("carol", "after reconnect");
        loop {
            if let Event::Message(message) = brx.recv_timeout(Duration::from_secs(2)).unwrap() {
                assert_eq!(message.text, "after reconnect");
                break;
            }
        }
        thread::sleep(Duration::from_millis(1200));
        for peer in [&a, &b, &c] {
            assert_eq!(peer.count(), 2, "duplicate automatic connection");
        }
    }
}
