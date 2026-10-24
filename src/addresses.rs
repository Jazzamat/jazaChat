use libp2p::{multiaddr::Protocol, Multiaddr, PeerId};
use std::{
    io,
    net::{IpAddr, SocketAddr},
};

pub fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message.into())
}

pub fn socket_address(address: SocketAddr) -> Multiaddr {
    let mut result = Multiaddr::empty();
    result.push(match address.ip() {
        IpAddr::V4(ip) => Protocol::Ip4(ip),
        IpAddr::V6(ip) => Protocol::Ip6(ip),
    });
    result.push(Protocol::Tcp(address.port()));
    result
}

// Exact allowlist: circuit, WebSocket, proxy and all other transports are rejected.
pub fn direct_address(address: &Multiaddr) -> io::Result<(SocketAddr, Option<PeerId>)> {
    let mut parts = address.iter();
    let ip = match parts.next() {
        Some(Protocol::Ip4(ip)) => IpAddr::V4(ip),
        Some(Protocol::Ip6(ip)) => IpAddr::V6(ip),
        _ => return Err(invalid("Use a literal IPv4 or IPv6 TCP address")),
    };
    let port = match parts.next() {
        Some(Protocol::Tcp(port)) if port != 0 => port,
        _ => return Err(invalid("A nonzero TCP port is required")),
    };
    let peer = match parts.next() {
        Some(Protocol::P2p(peer)) => Some(peer),
        None => None,
        _ => {
            return Err(invalid(
                "Only direct TCP addresses are allowed; relays are disabled",
            ))
        }
    };
    if parts.next().is_some() || ip.is_unspecified() || ip.is_multicast() {
        return Err(invalid("Only reachable direct TCP addresses are allowed"));
    }
    Ok((SocketAddr::new(ip, port), peer))
}

pub fn invitation(value: &str) -> io::Result<(Multiaddr, PeerId)> {
    let address: Multiaddr = value
        .parse()
        .map_err(|_| invalid("Use the full /ip6/.../tcp/.../p2p/... or /ip4/... invitation"))?;
    let (_, peer) = direct_address(&address)?;
    Ok((
        address,
        peer.ok_or_else(|| invalid("Invitation must include /p2p/PEER_ID to verify its owner"))?,
    ))
}

pub fn public_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => {
            let [a, b, _, _] = ip.octets();
            !ip.is_private()
                && !ip.is_loopback()
                && !ip.is_link_local()
                && !ip.is_broadcast()
                && !ip.is_documentation()
                && !ip.is_unspecified()
                && !ip.is_multicast()
                && a != 0
                && a < 240
                && !(a == 100 && (64..=127).contains(&b))
                && !(a == 198 && (b == 18 || b == 19))
        }
        IpAddr::V6(ip) => {
            // Global unicast only; exclude documentation and IPv4-mapped addresses.
            let s = ip.segments();
            s[0] & 0xe000 == 0x2000
                && !(s[0] == 0x2001 && (s[1] < 0x0200 || s[1] == 0x0db8))
                && !(s[0] == 0x3fff && s[1] & 0xf000 == 0)
        }
    }
}

#[derive(Clone, Copy)]
pub enum InviteKind {
    Ipv6,
    Ipv4,
    Local,
}

pub fn invitation_matches(address: &Multiaddr, kind: InviteKind) -> bool {
    direct_address(address).is_ok_and(|(socket, _)| match kind {
        InviteKind::Ipv6 => socket.is_ipv6() && public_ip(socket.ip()),
        InviteKind::Ipv4 => socket.is_ipv4() && public_ip(socket.ip()),
        InviteKind::Local => {
            !public_ip(socket.ip())
                && match socket.ip() {
                    IpAddr::V4(ip) => ip.is_loopback() || ip.is_private(),
                    IpAddr::V6(ip) => ip.is_loopback() || ip.is_unique_local(),
                }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ipv6_invites_exclude_local_ipv4_and_documentation_addresses() {
        for ip in [
            "127.0.0.1",
            "172.18.0.1",
            "192.168.1.10",
            "8.8.8.8",
            "::1",
            "fe80::1",
            "fd00::1",
            "2001:db8::1",
            "3fff::1",
            "2001:2::1",
        ] {
            let address = socket_address(SocketAddr::new(ip.parse().unwrap(), 45874));
            assert!(!invitation_matches(&address, InviteKind::Ipv6), "{ip}");
        }
        let address: Multiaddr = "/ip6/2606:4700::1111/tcp/45874".parse().unwrap();
        assert!(invitation_matches(&address, InviteKind::Ipv6));
        assert!(!invitation_matches(&address, InviteKind::Local));
        assert!(invitation_matches(
            &"/ip4/192.168.1.10/tcp/45874".parse().unwrap(),
            InviteKind::Local
        ));
        assert!(invitation_matches(
            &"/ip4/8.8.8.8/tcp/45874".parse().unwrap(),
            InviteKind::Ipv4
        ));
    }
    #[test]
    fn relay_and_unpinned_addresses_are_rejected() {
        let peer = PeerId::random();
        let direct = format!("/ip4/1.2.3.4/tcp/8080/p2p/{peer}");
        assert!(invitation(&direct).is_ok());
        assert!(invitation(&format!("{direct}/p2p-circuit/p2p/{}", PeerId::random())).is_err());
        assert!(invitation("/ip4/1.2.3.4/tcp/8080").is_err());
        assert!(invitation(&format!("/ip6/::1/tcp/8080/p2p/{peer}")).is_ok());
        assert!(invitation(&format!("/ip4/0.0.0.0/tcp/8080/p2p/{peer}")).is_err());
    }
    #[test]
    fn public_routes_exclude_lan_and_carrier_nat() {
        for ip in [
            "127.0.0.1",
            "192.168.1.1",
            "100.64.0.1",
            "169.254.169.254",
            "::1",
            "fe80::1",
            "fd00::1",
        ] {
            assert!(!public_ip(ip.parse().unwrap()));
        }
    }
}
