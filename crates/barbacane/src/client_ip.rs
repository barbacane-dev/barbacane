//! The client address a request is attributed to.
//!
//! The TCP peer, unless it is a trusted proxy (`--trusted-proxies`): then the
//! address the proxy chain reports in `X-Forwarded-For` or `Forwarded`
//! (`--client-ip-header`), read right to left past the trusted hops.

use std::net::{IpAddr, SocketAddr};
use std::str::FromStr;

use ipnet::IpNet;

/// The header a trusted proxy chain reports the client in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ClientIpHeader {
    /// `X-Forwarded-For: client, proxy1, proxy2`.
    #[default]
    XForwardedFor,
    /// `Forwarded: for=client, for=proxy1` (RFC 7239).
    Forwarded,
}

impl ClientIpHeader {
    /// The header's name, lowercase.
    pub fn name(self) -> &'static str {
        match self {
            Self::XForwardedFor => "x-forwarded-for",
            Self::Forwarded => "forwarded",
        }
    }
}

impl FromStr for ClientIpHeader {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, String> {
        match value.to_ascii_lowercase().as_str() {
            "x-forwarded-for" => Ok(Self::XForwardedFor),
            "forwarded" => Ok(Self::Forwarded),
            _ => Err("expected x-forwarded-for or forwarded".to_string()),
        }
    }
}

/// The networks whose connections may report a client address.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TrustedProxies {
    networks: Vec<IpNet>,
    header: ClientIpHeader,
}

impl TrustedProxies {
    /// No trusted proxy: every request is attributed to its TCP peer.
    pub fn none() -> Self {
        Self::default()
    }

    /// `networks`, comma-separated addresses or CIDR ranges, reporting the
    /// client in `header`. An empty list trusts nothing.
    pub fn parse(networks: &str, header: ClientIpHeader) -> Result<Self, String> {
        Ok(networks.parse::<Self>()?.with_header(header))
    }

    /// The same networks, reporting the client in `header`.
    pub fn with_header(self, header: ClientIpHeader) -> Self {
        Self { header, ..self }
    }

    /// The header the client is read from.
    pub fn header(&self) -> ClientIpHeader {
        self.header
    }

    /// The trusted networks, comma-separated.
    pub fn networks(&self) -> String {
        self.networks
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(", ")
    }

    /// Whether any proxy is trusted.
    pub fn is_empty(&self) -> bool {
        self.networks.is_empty()
    }

    fn trusts(&self, ip: IpAddr) -> bool {
        let ip = ip.to_canonical();
        self.networks.iter().any(|net| net.contains(&ip))
    }

    /// The client address of a request from `peer` carrying `reported`, the
    /// values of the configured header in the order received.
    ///
    /// `peer` unless it is trusted. Behind a trusted peer, the reported hops are
    /// read from the right: the first one that is not trusted is the client,
    /// and the left-most when all are. A hop that is not an address (`unknown`,
    /// an obfuscated identifier) ends the walk at the trusted hop to its right.
    pub fn client_ip<'a>(&self, peer: IpAddr, reported: impl Iterator<Item = &'a str>) -> IpAddr {
        let mut client = peer.to_canonical();
        if !self.trusts(client) {
            return client;
        }
        let hops: Vec<&str> = reported
            .flat_map(|value| split_hops(value, self.header))
            .collect();
        for hop in hops.into_iter().rev() {
            let Some(ip) = hop_address(hop, self.header) else {
                break;
            };
            client = ip;
            if !self.trusts(ip) {
                break;
            }
        }
        client
    }
}

/// Comma-separated addresses or CIDR ranges, reporting the client in
/// `X-Forwarded-For`. An empty list trusts nothing.
impl FromStr for TrustedProxies {
    type Err = String;

    fn from_str(networks: &str) -> Result<Self, String> {
        let networks = networks
            .split(',')
            .map(str::trim)
            .filter(|entry| !entry.is_empty())
            .map(parse_network)
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Self {
            networks,
            header: ClientIpHeader::default(),
        })
    }
}

/// An address (`10.0.0.7`, `::1`), or a CIDR range (`10.0.0.0/8`). An
/// IPv4-mapped range (`::ffff:10.0.0.0/104`) is the IPv4 range it maps, as
/// peers are compared in canonical form.
fn parse_network(entry: &str) -> Result<IpNet, String> {
    if let Ok(net) = entry.parse::<IpNet>() {
        return Ok(match net {
            IpNet::V6(v6) => match v6.addr().to_ipv4_mapped() {
                Some(v4) => {
                    let prefix = v6.prefix_len().checked_sub(96).ok_or_else(|| {
                        format!("{entry} is wider than the IPv4-mapped range ::ffff:0:0/96")
                    })?;
                    IpNet::new(IpAddr::V4(v4), prefix).map_err(|e| format!("{entry}: {e}"))?
                }
                None => net,
            },
            IpNet::V4(_) => net,
        }
        .trunc());
    }
    entry
        .parse::<IpAddr>()
        .map(|ip| IpNet::from(ip.to_canonical()))
        .map_err(|_| format!("{entry} is not an IP address or a CIDR range"))
}

/// The hops of one header value: comma-separated, and for `Forwarded`, the
/// elements' `for` parameters.
fn split_hops(value: &str, header: ClientIpHeader) -> Vec<&str> {
    match header {
        ClientIpHeader::XForwardedFor => value.split(',').map(str::trim).collect(),
        ClientIpHeader::Forwarded => value
            .split(',')
            .map(|element| {
                element
                    .split(';')
                    .filter_map(|pair| pair.split_once('='))
                    .find(|(name, _)| name.trim().eq_ignore_ascii_case("for"))
                    .map_or("", |(_, node)| node.trim())
            })
            .collect(),
    }
}

/// The address a hop names: `192.0.2.7`, `192.0.2.7:4711`, `2001:db8::1`,
/// `[2001:db8::1]:4711`, quoted in `Forwarded`.
fn hop_address(hop: &str, header: ClientIpHeader) -> Option<IpAddr> {
    let hop = match header {
        ClientIpHeader::Forwarded => hop.trim_matches('"'),
        ClientIpHeader::XForwardedFor => hop,
    };
    hop.parse::<IpAddr>()
        .or_else(|_| hop.parse::<SocketAddr>().map(|addr| addr.ip()))
        .or_else(|_| {
            hop.strip_prefix('[')
                .and_then(|rest| rest.strip_suffix(']'))
                .ok_or(())
                .and_then(|inner| inner.parse::<IpAddr>().map_err(|_| ()))
        })
        .ok()
        .map(|ip| ip.to_canonical())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(value: &str) -> IpAddr {
        value.parse().unwrap()
    }

    fn xff(networks: &str) -> TrustedProxies {
        TrustedProxies::parse(networks, ClientIpHeader::XForwardedFor).unwrap()
    }

    fn resolve(proxies: &TrustedProxies, peer: &str, values: &[&str]) -> String {
        proxies
            .client_ip(ip(peer), values.iter().copied())
            .to_string()
    }

    #[test]
    fn without_trusted_proxies_the_peer_is_the_client() {
        let none = TrustedProxies::none();
        assert!(none.is_empty());
        assert_eq!(resolve(&none, "10.0.0.1", &["198.51.100.7"]), "10.0.0.1");
        assert_eq!(resolve(&none, "::ffff:10.0.0.1", &[]), "10.0.0.1");
    }

    #[test]
    fn an_untrusted_peer_cannot_report_a_client() {
        let proxies = xff("10.0.0.0/8");
        assert_eq!(
            resolve(&proxies, "203.0.113.9", &["198.51.100.7"]),
            "203.0.113.9"
        );
    }

    #[test]
    fn behind_trusted_proxies_the_rightmost_untrusted_hop_is_the_client() {
        let proxies = xff("10.0.0.0/8, 192.168.1.7");
        // A client-written hop on the left is not believed.
        assert_eq!(
            resolve(
                &proxies,
                "10.1.2.3",
                &["1.1.1.1, 198.51.100.7, 192.168.1.7"]
            ),
            "198.51.100.7"
        );
        // Several header lines are one list, in order.
        assert_eq!(
            resolve(
                &proxies,
                "10.1.2.3",
                &["1.1.1.1, 198.51.100.7", "192.168.1.7"]
            ),
            "198.51.100.7"
        );
        // Every hop trusted: the left-most.
        assert_eq!(
            resolve(&proxies, "10.1.2.3", &["10.9.9.9, 192.168.1.7"]),
            "10.9.9.9"
        );
        // No header: the peer.
        assert_eq!(resolve(&proxies, "10.1.2.3", &[]), "10.1.2.3");
    }

    #[test]
    fn a_hop_that_is_not_an_address_ends_the_walk_at_the_trusted_hop() {
        let proxies = xff("10.0.0.0/8");
        assert_eq!(
            resolve(&proxies, "10.1.2.3", &["198.51.100.7, unknown, 10.4.4.4"]),
            "10.4.4.4"
        );
        assert_eq!(resolve(&proxies, "10.1.2.3", &["garbage"]), "10.1.2.3");
        assert_eq!(resolve(&proxies, "10.1.2.3", &[""]), "10.1.2.3");
    }

    #[test]
    fn hops_with_ports_brackets_and_mapped_addresses_are_read() {
        let proxies = xff("10.0.0.0/8, 2001:db8:ffff::/48");
        assert_eq!(
            resolve(&proxies, "10.1.2.3", &["198.51.100.7:4711"]),
            "198.51.100.7"
        );
        assert_eq!(
            resolve(&proxies, "10.1.2.3", &["[2001:db8::1]:4711"]),
            "2001:db8::1"
        );
        assert_eq!(
            resolve(&proxies, "10.1.2.3", &["[2001:db8::1]"]),
            "2001:db8::1"
        );
        assert_eq!(
            resolve(&proxies, "2001:db8:ffff::5", &["::ffff:198.51.100.7"]),
            "198.51.100.7"
        );
        // A mapped peer is matched against an IPv4 range.
        assert_eq!(
            resolve(&proxies, "::ffff:10.1.2.3", &["198.51.100.7"]),
            "198.51.100.7"
        );
    }

    #[test]
    fn forwarded_elements_report_their_for_parameter() {
        let proxies = TrustedProxies::parse("10.0.0.0/8", ClientIpHeader::Forwarded).unwrap();
        assert_eq!(
            resolve(
                &proxies,
                "10.1.2.3",
                &[
                    r#"for=1.1.1.1, For="[2001:db8:cafe::17]:4711";proto=https, for=10.2.2.2;by=10.0.0.1"#
                ]
            ),
            "2001:db8:cafe::17"
        );
        assert_eq!(
            resolve(&proxies, "10.1.2.3", &["for=198.51.100.7;proto=http"]),
            "198.51.100.7"
        );
        // An obfuscated or unknown node ends the walk.
        assert_eq!(
            resolve(&proxies, "10.1.2.3", &["for=198.51.100.7, for=_hidden"]),
            "10.1.2.3"
        );
        // An element without `for` ends it too.
        assert_eq!(
            resolve(&proxies, "10.1.2.3", &["for=198.51.100.7, proto=https"]),
            "10.1.2.3"
        );
        // X-Forwarded-For is not read when Forwarded is configured.
        let xff_only = xff("10.0.0.0/8");
        assert_eq!(
            resolve(&xff_only, "10.1.2.3", &["for=198.51.100.7"]),
            "10.1.2.3"
        );
    }

    #[test]
    fn networks_parse_as_addresses_or_ranges() {
        let proxies = xff(" 10.0.0.0/8 ,192.168.1.7,::1, 2001:db8::/32,, ::ffff:172.16.0.0/112");
        for trusted in [
            "10.255.0.1",
            "192.168.1.7",
            "::1",
            "2001:db8::42",
            "172.16.9.9",
        ] {
            assert!(proxies.trusts(ip(trusted)), "{trusted}");
        }
        for untrusted in [
            "11.0.0.1",
            "192.168.1.8",
            "::2",
            "2001:db9::1",
            "172.17.0.1",
        ] {
            assert!(!proxies.trusts(ip(untrusted)), "{untrusted}");
        }
        // Host bits are ignored.
        assert!(xff("10.1.2.3/8").trusts(ip("10.200.0.1")));
        assert!(xff("").is_empty());

        for invalid in [
            "10.0.0.0/33",
            "example.com",
            "10.0.0",
            "10.0.0.0/8/9",
            "::ffff:0.0.0.0/80",
        ] {
            assert!(
                TrustedProxies::parse(invalid, ClientIpHeader::XForwardedFor).is_err(),
                "{invalid}"
            );
        }
    }

    #[test]
    fn the_header_is_named_either_way() {
        assert_eq!(
            "X-Forwarded-For".parse::<ClientIpHeader>(),
            Ok(ClientIpHeader::XForwardedFor)
        );
        assert_eq!(
            "forwarded".parse::<ClientIpHeader>(),
            Ok(ClientIpHeader::Forwarded)
        );
        assert!("x-real-ip".parse::<ClientIpHeader>().is_err());
    }
}
