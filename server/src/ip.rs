//! IP address and CIDR matching for the trusted-proxy setting.
//!
//! std-only containment checks so the gateway can decide which peers may
//! vouch for a client address in forwarding headers. No new crate is pulled
//! in for this; the checks are plain integer comparisons.

use std::net::IpAddr;

/// An IP network in CIDR notation (`10.0.0.0/8`, `2001:db8::/32`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IpCidr {
    /// Network address as a big integer (the low 32 bits hold an IPv4 address).
    network: u128,
    /// Leading network bits: 0–32 for IPv4, 0–128 for IPv6.
    prefix_len: u8,
    /// `true` for an IPv4 network. Families never match across.
    ipv4: bool,
}

impl IpCidr {
    /// Parses `network/prefix`; a bare address is a single-host network.
    /// Fails naming the problem when the address or prefix length is invalid.
    pub fn parse(value: &str) -> Result<Self, String> {
        let (addr, prefix) = value.split_once('/').unwrap_or((value, ""));
        let addr: IpAddr = addr
            .trim()
            .parse()
            .map_err(|_| format!("invalid IP address {addr:?}"))?;
        let (network, ipv4, width) = match addr {
            IpAddr::V4(v4) => (u128::from(u32::from(v4)), true, 32u8),
            IpAddr::V6(v6) => (u128::from(v6), false, 128u8),
        };
        let prefix = prefix.trim();
        let prefix_len = if prefix.is_empty() {
            width
        } else {
            prefix
                .parse::<u8>()
                .map_err(|_| format!("invalid prefix length {prefix:?}"))?
        };
        if prefix_len > width {
            return Err(format!("prefix length {prefix_len} exceeds {width} bits"));
        }
        Ok(Self {
            network,
            prefix_len,
            ipv4,
        })
    }

    /// True when `ip` falls inside the network. An IPv4-mapped IPv6 address
    /// (`::ffff:10.0.0.1`) is compared as the IPv4 address it embeds.
    pub fn contains(&self, ip: IpAddr) -> bool {
        let ip = unmap_ipv4(ip);
        let value = match (self.ipv4, ip) {
            (true, IpAddr::V4(v4)) => u128::from(u32::from(v4)),
            (false, IpAddr::V6(v6)) => u128::from(v6),
            _ => return false,
        };
        let shift = u32::from(if self.ipv4 { 32 } else { 128 } - self.prefix_len);
        // A /0 shifts by the full width, which `checked_shr` reports as too
        // wide; every address of the family is inside such a network.
        value.checked_shr(shift).unwrap_or(0) == self.network.checked_shr(shift).unwrap_or(0)
    }
}

/// IPv4-mapped IPv6 (`::ffff:a.b.c.d`) reduces to the IPv4 address it embeds.
pub fn unmap_ipv4(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V6(v6) => v6
            .to_ipv4_mapped()
            .map(IpAddr::V4)
            .unwrap_or(IpAddr::V6(v6)),
        v4 => v4,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn addr(value: &str) -> IpAddr {
        value.parse().expect("test address")
    }

    #[test]
    fn parses_and_matches_ipv4_cidrs() {
        let net = IpCidr::parse("10.0.0.0/8").expect("valid cidr");
        assert!(net.contains(addr("10.0.0.0")));
        assert!(net.contains(addr("10.255.255.255")));
        assert!(!net.contains(addr("11.0.0.1")));
        assert!(!net.contains(addr("9.255.255.255")));

        let host = IpCidr::parse("127.0.0.1/32").expect("valid cidr");
        assert!(host.contains(addr("127.0.0.1")));
        assert!(!host.contains(addr("127.0.0.2")));

        // A bare address is a single-host network.
        let bare = IpCidr::parse("127.0.0.1").expect("valid cidr");
        assert_eq!(bare, host);
    }

    #[test]
    fn parses_and_matches_ipv6_cidrs() {
        let net = IpCidr::parse("2001:db8::/32").expect("valid cidr");
        assert!(net.contains(addr("2001:db8::1")));
        assert!(net.contains(addr("2001:db8:ffff::")));
        assert!(!net.contains(addr("2001:db9::1")));

        let loopback = IpCidr::parse("::1/128").expect("valid cidr");
        assert!(loopback.contains(addr("::1")));
        assert!(!loopback.contains(addr("::2")));
    }

    #[test]
    fn rejects_malformed_cidrs() {
        for bad in [
            "not-an-ip",
            "10.0.0.0/33",
            "10.0.0.0/abc",
            "10.0.0.0/-1",
            "::1/129",
            "10.0.0.256/8",
            "",
        ] {
            assert!(IpCidr::parse(bad).is_err(), "{bad:?} must not parse");
        }
    }

    #[test]
    fn families_never_match_across() {
        let v4 = IpCidr::parse("0.0.0.0/0").expect("valid cidr");
        assert!(v4.contains(addr("8.8.8.8")));
        assert!(!v4.contains(addr("::1")));

        let v6 = IpCidr::parse("::/0").expect("valid cidr");
        assert!(v6.contains(addr("2001:db8::1")));
        assert!(!v6.contains(addr("8.8.8.8")));
    }

    #[test]
    fn ipv4_mapped_ipv6_matches_ipv4_cidrs() {
        let net = IpCidr::parse("10.0.0.0/8").expect("valid cidr");
        assert!(net.contains(addr("::ffff:10.0.0.1")));
        assert!(!net.contains(addr("::ffff:11.0.0.1")));
    }
}
