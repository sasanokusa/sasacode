//! Which addresses a fetched page may come from, and a GET that follows redirects itself so that every
//! hop is judged (scheme, name resolution, internal address) and connected to the address that was
//! judged. Shared by web_fetch and web_search.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::time::Duration;

use futures::future::BoxFuture;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use crate::rustxng_install::no_redirects;
use crate::web_fetch::send_within;

/// An IPv6 address as its eight 16-bit words, `::` filled in with zeros; nothing for anything else.
/// A zone (`fe80::1%eth0`) is accepted as Node's `isIP` accepts it, and does not count.
fn ipv6_words(ip: &str) -> Option<[u16; 8]> {
    let v = ip.to_lowercase();
    let addr = v.split_once('%').map_or(v.as_str(), |(a, _)| a);
    addr.parse::<Ipv6Addr>().ok().map(|a| a.segments())
}

/// An IPv4 address that is not on the public internet: this network, private, shared (CGNAT), loopback,
/// link-local (cloud metadata), IETF protocol assignments, benchmarking, documentation, and everything
/// from 224.0.0.0 up (multicast, reserved, broadcast).
fn private_v4([a, b, c, _]: [u8; 4]) -> bool {
    a == 0
        || a == 10
        || a == 127
        || a >= 224
        || (a == 100 && (64..=127).contains(&b))
        || (a == 169 && b == 254)
        || (a == 172 && (16..=31).contains(&b))
        || (a == 192 && b == 168)
        || (a == 192 && b == 0 && (c == 0 || c == 2))
        || (a == 198 && (b == 18 || b == 19))
        || (a == 198 && b == 51 && c == 100)
        || (a == 203 && b == 0 && c == 113)
}

/// An IPv6 address that is not on the public internet. An address that carries an IPv4 is judged by
/// that IPv4: a connection to it ends up there.
fn private_v6(w: [u16; 8]) -> bool {
    let v4 = |hi: u16, lo: u16| [(hi >> 8) as u8, (hi & 0xff) as u8, (lo >> 8) as u8, (lo & 0xff) as u8];
    // An IPv4-mapped (`::ffff:`) address, or the older IPv4-compatible one (all zero; `::` and `::1`
    // come out as 0.0.0.x): both are judged as the IPv4 they carry, written in dotted decimal
    // (`::ffff:127.0.0.1`) or in hexadecimal (`::ffff:7f00:1`, which the URL parser keeps as written).
    if w[..5].iter().all(|x| *x == 0) && (w[5] == 0xffff || w[5] == 0) {
        return private_v4(v4(w[6], w[7]));
    }
    // IPv4-translated (`::ffff:0:a.b.c.d`, RFC 2765).
    if w[..4].iter().all(|x| *x == 0) && w[4] == 0xffff && w[5] == 0 {
        return private_v4(v4(w[6], w[7]));
    }
    // NAT64's well-known prefix (64:ff9b::/96, RFC 6052): a NAT64 gateway connects to the IPv4 in the
    // last 32 bits, and DNS64 answers a name with such an address (#83).
    if w[0] == 0x64 && w[1] == 0xff9b && w[2..6].iter().all(|x| *x == 0) {
        return private_v4(v4(w[6], w[7]));
    }
    // NAT64's local-use prefix (64:ff9b:1::/48, RFC 8215), whose IPv4 are the site's own.
    if w[0] == 0x64 && w[1] == 0xff9b && w[2] == 1 {
        return true;
    }
    // 6to4 (2002::/16) carries its IPv4 in bits 16-47.
    if w[0] == 0x2002 {
        return private_v4(v4(w[1], w[2]));
    }
    // Teredo (2001::/32) carries the server's IPv4 in bits 32-63 and the client's, inverted, in the
    // last 32 bits: both must be public.
    if w[0] == 0x2001 && w[1] == 0 {
        return private_v4(v4(w[2], w[3])) || private_v4(v4(!w[6], !w[7]));
    }
    // Documentation (2001:db8::/32, and 3fff::/20 of RFC 9637), discard-only (100::/64).
    if (w[0] == 0x2001 && w[1] == 0x0db8)
        || (w[0] == 0x3fff && w[1] & 0xf000 == 0)
        || (w[0] == 0x0100 && w[1..4].iter().all(|x| *x == 0))
    {
        return true;
    }
    // Unique local (fc00::/7), link-local (fe80::/10), site-local (fec0::/10, deprecated but routed
    // inside some sites), multicast (ff00::/8).
    w[0] & 0xfe00 == 0xfc00 || w[0] & 0xffc0 == 0xfe80 || w[0] & 0xffc0 == 0xfec0 || w[0] >> 8 == 0xff
}

/// Loopback, private, link-local (cloud metadata), shared, unspecified, multicast and the like: an
/// address a page is not to be read from unless the user named it. Text that is not an address counts.
pub fn private_address(ip: &str) -> bool {
    if let Ok(v4) = ip.parse::<Ipv4Addr>() {
        return private_v4(v4.octets());
    }
    ipv6_words(ip).is_none_or(private_v6)
}

/// [`private_address`] of an address already parsed.
pub(crate) fn internal_ip(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(a) => private_v4(a.octets()),
        IpAddr::V6(a) => private_v6(a.segments()),
    }
}

/// How a name is resolved and which resolved addresses are refused: the system and [`internal_ip`] in
/// use, other functions in tests.
pub(crate) struct Route {
    pub lookup: fn(String) -> BoxFuture<'static, Result<Vec<IpAddr>, String>>,
    pub internal: fn(&IpAddr) -> bool,
}

fn system_lookup(host: String) -> BoxFuture<'static, Result<Vec<IpAddr>, String>> {
    Box::pin(async move {
        tokio::net::lookup_host((host.as_str(), 0))
            .await
            .map(|found| found.map(|a| a.ip()).collect())
            .map_err(|e| e.to_string())
    })
}

pub(crate) const SYSTEM: Route = Route {
    lookup: system_lookup,
    internal: internal_ip,
};

/// Which hops of a GET are judged.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Check {
    /// Every URL, the first included: the address came from somewhere the user did not read.
    All,
    /// The redirects only: the first URL is what the user was asked about.
    Redirects,
    /// None: the user chose this host (an intranet search server), the scheme is still checked.
    Never,
}

/// How long a GET may take.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Limit {
    /// All hops together, from the first request's start.
    Total(Duration),
    /// Each hop from its own start; the caller's body read runs on the last hop's clock.
    PerHop(Duration),
}

impl Limit {
    fn length(self) -> Duration {
        match self {
            Limit::Total(d) | Limit::PerHop(d) => d,
        }
    }
}

/// Why a GET did not complete.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GetError {
    pub message: String,
    /// The URL was refused (scheme or address, or too many redirects), not a failed connection.
    pub refused: bool,
}

impl GetError {
    fn failed(message: impl Into<String>) -> GetError {
        GetError {
            message: message.into(),
            refused: false,
        }
    }
}

/// What a GET ended with: the response, the URL it answers, and where the time began.
pub(crate) struct Followed {
    pub res: reqwest::Response,
    pub url: url::Url,
    /// The URL asked for first, when the answer came after redirects.
    pub redirected_from: Option<url::Url>,
    /// When the request that answered started (the first one, under [`Limit::Total`]): the caller's
    /// body read runs on the same clock.
    pub started: Instant,
}

const NOT_PUBLIC: &str = "not a public address";

/// Resolve and judge a URL's host. The addresses to connect to: empty for an IP literal, which needs no
/// pinning.
async fn vet(url: &url::Url, route: &Route) -> Result<Vec<IpAddr>, String> {
    let domain = match url.host() {
        None => return Err("no host".into()),
        Some(url::Host::Ipv4(a)) => return refuse_if_internal(route, vec![IpAddr::V4(a)]).map(|_| Vec::new()),
        Some(url::Host::Ipv6(a)) => return refuse_if_internal(route, vec![IpAddr::V6(a)]).map(|_| Vec::new()),
        Some(url::Host::Domain(d)) => d,
    };
    let bare = domain.trim_end_matches('.');
    if bare == "localhost" || bare.ends_with(".localhost") {
        return Err(NOT_PUBLIC.into());
    }
    let ips = (route.lookup)(domain.to_string()).await?;
    refuse_if_internal(route, ips.clone())?;
    Ok(ips)
}

fn refuse_if_internal(route: &Route, ips: Vec<IpAddr>) -> Result<(), String> {
    if ips.is_empty() || ips.iter().any(route.internal) {
        return Err(NOT_PUBLIC.into());
    }
    Ok(())
}

/// A client that reports redirects and connects a name only to `ips`, the addresses that were judged:
/// the name is not resolved again between the check and the connection.
fn pinned_client(url: &url::Url, ips: &[IpAddr]) -> Result<reqwest::Client, String> {
    let Some(url::Host::Domain(domain)) = url.host() else {
        return Ok(no_redirects().clone());
    };
    let addrs: Vec<SocketAddr> = ips.iter().map(|ip| SocketAddr::new(*ip, 0)).collect();
    sasacode_ai::http::builder()
        .redirect(reqwest::redirect::Policy::none())
        .resolve_to_addrs(domain, &addrs)
        .build()
        .map_err(|e| e.to_string())
}

/// GET `first` and follow redirects (at most `max_redirects`), judging hops as `check` says and
/// connecting each judged hop to the address that was judged. All of it, the name resolution
/// included, runs under `signal` and `limit`.
///
/// Where a redirect stays on the first URL's origin (scheme, host and port), `Check::Redirects` does not
/// judge it: it is the place the first URL already reached, and nothing new can be seen there.
///
/// Behind an HTTP proxy (reqwest reads the environment's) the proxy resolves the name and makes the
/// connection, so the pinning here does not apply; the check of the resolved address still runs.
pub(crate) async fn get_following(
    first: url::Url,
    check: Check,
    max_redirects: usize,
    route: &Route,
    headers: &[(&str, &str)],
    signal: &CancellationToken,
    limit: Limit,
) -> Result<Followed, GetError> {
    let mut started = Instant::now();
    let mut deadline = started + limit.length();
    let mut next = first.clone();
    let mut hops = 0;
    loop {
        if hops > 0 && matches!(limit, Limit::PerHop(_)) {
            started = Instant::now();
            deadline = started + limit.length();
        }
        let refused = |why: String| GetError {
            message: if hops == 0 {
                why
            } else {
                format!("redirect to {next} refused: {why}")
            },
            refused: true,
        };
        if next.scheme() != "http" && next.scheme() != "https" {
            return Err(refused(format!("not http(s): {}:", next.scheme())));
        }
        let judged = match check {
            Check::All => true,
            Check::Redirects => hops > 0 && next.origin() != first.origin(),
            Check::Never => false,
        };
        let client = if judged {
            let vetted = tokio::select! {
                _ = signal.cancelled() => return Err(GetError::failed("The operation was aborted.")),
                _ = tokio::time::sleep_until(deadline) => return Err(GetError::failed("The operation timed out.")),
                r = vet(&next, route) => r,
            };
            let ips = vetted.map_err(|e| {
                // An internal address is a refused URL; a name that did not resolve is a failed fetch.
                if e == NOT_PUBLIC {
                    refused(e)
                } else if hops == 0 {
                    GetError::failed(e)
                } else {
                    GetError::failed(format!("redirect to {next} failed: {e}"))
                }
            })?;
            pinned_client(&next, &ips).map_err(GetError::failed)?
        } else {
            no_redirects().clone()
        };
        let mut req = client.get(next.clone());
        for (name, value) in headers {
            req = req.header(*name, *value);
        }
        let res = send_within(req, signal, deadline.saturating_duration_since(Instant::now()))
            .await
            .map_err(GetError::failed)?;
        let location = if res.status().is_redirection() {
            res.headers()
                .get(reqwest::header::LOCATION)
                .and_then(|v| v.to_str().ok())
                .map(str::to_string)
        } else {
            None
        };
        let Some(location) = location else {
            return Ok(Followed {
                res,
                redirected_from: (hops > 0).then_some(first),
                url: next,
                started,
            });
        };
        drop(res);
        if hops >= max_redirects {
            return Err(GetError {
                message: "too many redirects".into(),
                refused: true,
            });
        }
        hops += 1;
        next = next
            .join(&location)
            .map_err(|_| GetError::failed(format!("Invalid redirect URL: {location}")))?;
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    use super::*;

    #[test]
    fn private_address_loopback_private_link_local_and_the_like_ipv4_and_ipv6() {
        let internal = [
            "127.0.0.1",
            "10.1.2.3",
            "172.16.0.1",
            "172.31.255.255",
            "192.168.1.1",
            "169.254.169.254",
            "100.64.0.1",
            "0.0.0.0",
            "::1",
            "fd00::1",
            "fe80::1",
            "::ffff:127.0.0.1",
            // The same addresses written in hexadecimal: the URL parser keeps them as written, so they
            // must be judged by the IPv4 they mean (#55).
            "::ffff:7f00:1",
            "::ffff:7f00:0001",
            "0:0:0:0:0:ffff:7f00:1",
            "::ffff:a00:1",
            "::ffff:a9fe:a9fe",
            "::ffff:6440:0",
            "::ffff:c0a8:1",
            "::ffff:0:0",
        ];
        for ip in internal {
            assert_eq!((ip, private_address(ip)), (ip, true));
        }
        for ip in [
            "8.8.8.8",
            "172.32.0.1",
            "1.1.1.1",
            "2606:4700::1111",
            "::ffff:8.8.8.8",
            "::ffff:100:63",
            "::ffff:a9fd:1",
            "::ffff:c0a7:1",
        ] {
            assert_eq!((ip, private_address(ip)), (ip, false));
        }
        // NAT64 (#83): the well-known prefix is judged by the IPv4 it carries, the local-use one is the
        // site's own; 6to4 likewise.
        for ip in [
            "64:ff9b::127.0.0.1",
            "64:ff9b::7f00:1",
            "64:ff9b::a9fe:a9fe",
            "64:ff9b::c0a8:101",
            "64:ff9b:1::8.8.8.8",
            "2002:7f00:1::1",
            "2002:a00:1::",
        ] {
            assert_eq!((ip, private_address(ip)), (ip, true));
        }
        for ip in [
            "64:ff9b::8.8.8.8",
            "64:ff9b::808:808",
            "2002:808:808::1",
            "64:ff9b:2::7f00:1",
        ] {
            assert_eq!((ip, private_address(ip)), (ip, false));
        }
    }

    #[test]
    fn private_address_accepts_a_zone_and_judges_upper_case() {
        assert!(private_address("fe80::1%eth0"));
        assert!(private_address("::FFFF:7F00:1"));
        assert!(!private_address("2606:4700::1111%en0"));
    }

    #[test]
    fn private_address_ipv4_range_edges() {
        let (internal, public): (&[&str], &[&str]) = (
            &[
                "0.0.0.0",
                "0.255.255.255",
                "10.0.0.0",
                "10.255.255.255",
                "100.64.0.0",
                "100.127.255.255",
                "127.255.255.255",
                "169.254.0.0",
                "169.254.255.255",
                "172.16.0.0",
                "172.31.255.255",
                "192.0.0.0",
                "192.0.0.255",
                "192.0.2.0",
                "192.0.2.255",
                "192.168.0.0",
                "192.168.255.255",
                "198.18.0.0",
                "198.19.255.255",
                "198.51.100.0",
                "198.51.100.255",
                "203.0.113.0",
                "203.0.113.255",
                "224.0.0.0",
                "239.255.255.255",
                "240.0.0.1",
                "255.255.255.255",
            ],
            &[
                "1.0.0.0",
                "9.255.255.255",
                "11.0.0.0",
                "100.63.255.255",
                "100.128.0.0",
                "126.255.255.255",
                "128.0.0.0",
                "169.253.255.255",
                "169.255.0.0",
                "172.15.255.255",
                "172.32.0.0",
                "192.0.1.0",
                "192.0.3.0",
                "192.167.255.255",
                "192.169.0.0",
                "198.17.255.255",
                "198.20.0.0",
                "198.51.99.255",
                "198.51.101.0",
                "203.0.112.255",
                "203.0.114.0",
                "223.255.255.255",
            ],
        );
        for ip in internal {
            assert_eq!((ip, private_address(ip)), (ip, true));
            assert!(internal_ip(&ip.parse().expect("an address")), "{ip}");
        }
        for ip in public {
            assert_eq!((ip, private_address(ip)), (ip, false));
            assert!(!internal_ip(&ip.parse().expect("an address")), "{ip}");
        }
    }

    #[test]
    fn private_address_ipv6_range_edges() {
        let internal = [
            "::",
            "::1",
            "fc00::",
            "fdff:ffff:ffff:ffff:ffff:ffff:ffff:ffff",
            "fe80::",
            "febf:ffff:ffff:ffff:ffff:ffff:ffff:ffff",
            // site-local, deprecated
            "fec0::",
            "fec0::1",
            "feff:ffff:ffff:ffff:ffff:ffff:ffff:ffff",
            "ff00::",
            "ff02::1",
            "2001:db8::",
            "2001:db8:ffff::1",
            "3fff::",
            "3fff:fff:ffff::1",
            "100::1",
            // Teredo, judged by the server's IPv4 and by the client's (inverted in the last 32 bits)
            "2001:0:7f00:1::1",
            "2001:0:808:808:0:0:80ff:fffe", // public server, client 127.0.0.1
            "2001:0:a00:1:0:0:808:808",     // server 10.0.0.1, client 247.247.247.247
            "2001::",
            // IPv4-translated, IPv4-mapped in hexadecimal, 6to4
            "::ffff:0:127.0.0.1",
            "::ffff:7f00:1",
            "2002:c0a8:1::",
            "2002:a9fe:a9fe::",
        ];
        for ip in internal {
            assert_eq!((ip, private_address(ip)), (ip, true));
            assert!(internal_ip(&ip.parse().expect("an address")), "{ip}");
        }
        let public = [
            "2000::1",
            "2001:1::1",
            "2001:db7:ffff::1",
            "2001:db9::1",
            "2606:4700::1111",
            "3ffe::1",
            "3fff:1000::1",
            "fbff:ffff::1",
            "fe7f::1",
            "ff::1",
            // Teredo with a public server and a public client (8.8.8.8 is f7f7:f7f7 inverted)
            "2001:0:808:808:0:0:f7f7:f7f7",
            "2001:0:101:101:0:0:fefe:fefe",
            "::ffff:0:8.8.8.8",
            "2002:808:808::1",
            "64:ff9b::808:808",
        ];
        for ip in public {
            assert_eq!((ip, private_address(ip)), (ip, false));
            assert!(!internal_ip(&ip.parse().expect("an address")), "{ip}");
        }
    }

    #[test]
    fn private_address_counts_text_that_is_not_an_address() {
        assert!(private_address("not-an-address"));
        assert!(private_address(""));
    }

    // --- the hop-by-hop GET ---

    /// A server on 127.0.0.1 that answers by path and counts the requests it got. A path that is a key
    /// of `redirects` answers 302 to its value; any other, 200 with the body `ok:<port><path>`.
    struct Listener {
        port: u16,
        hits: Arc<AtomicUsize>,
    }

    async fn listen(redirects: Vec<(&'static str, String)>) -> Listener {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let port = listener.local_addr().expect("addr").port();
        let hits = Arc::new(AtomicUsize::new(0));
        let counted = hits.clone();
        tokio::spawn(async move {
            while let Ok((mut socket, _)) = listener.accept().await {
                counted.fetch_add(1, Ordering::SeqCst);
                let redirects = redirects.clone();
                tokio::spawn(async move {
                    let mut buf = vec![0u8; 4096];
                    let mut head = Vec::new();
                    while !head.windows(4).any(|w| w == b"\r\n\r\n") {
                        match socket.read(&mut buf).await {
                            Ok(0) | Err(_) => return,
                            Ok(n) => head.extend_from_slice(&buf[..n]),
                        }
                    }
                    let head = String::from_utf8_lossy(&head).into_owned();
                    let path = head.split_whitespace().nth(1).unwrap_or("/").to_string();
                    let reply = match redirects.iter().find(|(p, _)| *p == path) {
                        Some((_, to)) => format!(
                            "HTTP/1.1 302 Found\r\nlocation: {to}\r\ncontent-length: 0\r\nconnection: close\r\n\r\n"
                        ),
                        None => {
                            let body = format!("ok:{port}{path}");
                            format!(
                                "HTTP/1.1 200 OK\r\ncontent-type: text/plain\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                                body.len()
                            )
                        }
                    };
                    let _ = socket.write_all(reply.as_bytes()).await;
                    let _ = socket.shutdown().await;
                });
            }
        });
        Listener { port, hits }
    }

    impl Listener {
        fn hits(&self) -> usize {
            self.hits.load(Ordering::SeqCst)
        }
    }

    fn parse(s: &str) -> url::Url {
        url::Url::parse(s).expect("a URL")
    }

    /// What a GET answered: status and body, the URL, and where it began if redirected.
    struct Answer {
        text: String,
        url: String,
        from: Option<String>,
    }

    async fn get(first: &str, check: Check, max: usize, route: &Route) -> Result<Answer, GetError> {
        let signal = CancellationToken::new();
        let followed = get_following(
            parse(first),
            check,
            max,
            route,
            &[],
            &signal,
            Limit::Total(Duration::from_secs(10)),
        )
        .await?;
        let (url, from) = (
            followed.url.to_string(),
            followed.redirected_from.map(|u| u.to_string()),
        );
        let status = followed.res.status().as_u16();
        let body = followed.res.text().await.expect("body");
        Ok(Answer {
            text: format!("{status} {body}"),
            url,
            from,
        })
    }

    /// Every name resolves to this machine; which addresses count as internal is the test's choice.
    fn loopback_lookup(_host: String) -> BoxFuture<'static, Result<Vec<IpAddr>, String>> {
        Box::pin(async { Ok(vec![IpAddr::V4(Ipv4Addr::LOCALHOST)]) })
    }

    fn nothing_internal(_: &IpAddr) -> bool {
        false
    }

    const OPEN_LOOPBACK: Route = Route {
        lookup: loopback_lookup,
        internal: nothing_internal,
    };

    #[tokio::test]
    async fn a_redirect_to_an_internal_address_is_refused_before_anything_connects() {
        let target = listen(vec![]).await;
        let origin = listen(vec![("/", format!("http://127.0.0.1:{}/secret", target.port))]).await;
        let err = get(
            &format!("http://127.0.0.1:{}/", origin.port),
            Check::Redirects,
            10,
            &SYSTEM,
        )
        .await
        .err()
        .expect("refused");
        assert!(err.refused, "{err:?}");
        assert_eq!(
            err.message,
            format!(
                "redirect to http://127.0.0.1:{}/secret refused: not a public address",
                target.port
            )
        );
        assert_eq!((origin.hits(), target.hits()), (1, 0));
    }

    #[tokio::test]
    async fn a_redirect_to_a_name_that_resolves_to_an_internal_address_is_refused_too() {
        fn resolves_to_loopback(host: String) -> BoxFuture<'static, Result<Vec<IpAddr>, String>> {
            loopback_lookup(host)
        }
        let by_name = Route {
            lookup: resolves_to_loopback,
            internal: internal_ip,
        };
        let target = listen(vec![]).await;
        let origin = listen(vec![
            ("/by-name", format!("http://metadata.example:{}/x", target.port)),
            ("/localhost", format!("http://localhost:{}/x", target.port)),
            ("/sub", format!("http://app.localhost.:{}/x", target.port)),
            ("/mapped", format!("http://[::ffff:7f00:1]:{}/x", target.port)),
        ])
        .await;
        for path in ["/by-name", "/localhost", "/sub", "/mapped"] {
            let err = get(
                &format!("http://127.0.0.1:{}{path}", origin.port),
                Check::Redirects,
                10,
                &by_name,
            )
            .await
            .err()
            .unwrap_or_else(|| panic!("{path} was fetched"));
            assert!(
                err.refused && err.message.ends_with("refused: not a public address"),
                "{path}: {err:?}"
            );
        }
        assert_eq!(target.hits(), 0);
    }

    #[tokio::test]
    async fn the_first_url_is_fetched_when_only_redirects_are_judged() {
        let origin = listen(vec![]).await;
        let answer = get(
            &format!("http://127.0.0.1:{}/page", origin.port),
            Check::Redirects,
            10,
            &SYSTEM,
        )
        .await
        .expect("fetched");
        assert_eq!(answer.text, format!("200 ok:{}/page", origin.port));
        assert!(answer.from.is_none());
        // Judged as well, the same URL is refused without a request.
        let again = listen(vec![]).await;
        let err = get(&format!("http://127.0.0.1:{}/", again.port), Check::All, 10, &SYSTEM)
            .await
            .err()
            .expect("refused");
        assert_eq!(err.message, "not a public address");
        assert_eq!(again.hits(), 0);
        // And never judged, the redirects are followed to this machine.
        let target = listen(vec![]).await;
        let hop = listen(vec![("/", format!("http://127.0.0.1:{}/in", target.port))]).await;
        let answer = get(&format!("http://127.0.0.1:{}/", hop.port), Check::Never, 10, &SYSTEM)
            .await
            .expect("fetched");
        assert_eq!(answer.text, format!("200 ok:{}/in", target.port));
    }

    #[tokio::test]
    async fn a_redirect_that_stays_on_the_first_origin_is_not_judged_but_any_other_origin_is() {
        let other = listen(vec![]).await;
        let site = listen(vec![
            ("/", "/login".into()),
            ("/other-port", format!("http://127.0.0.1:{}/x", other.port)),
            ("/other-host", format!("http://localhost:{}/x", other.port)),
        ])
        .await;
        let base = format!("http://127.0.0.1:{}", site.port);
        let answer = get(&format!("{base}/"), Check::Redirects, 10, &SYSTEM)
            .await
            .expect("fetched");
        assert_eq!(answer.text, format!("200 ok:{}/login", site.port));
        assert_eq!(answer.from, Some(format!("{base}/")));
        // Another port, or another name for this machine, is another origin: judged, refused.
        for path in ["/other-port", "/other-host"] {
            let err = get(&format!("{base}{path}"), Check::Redirects, 10, &SYSTEM)
                .await
                .err()
                .unwrap_or_else(|| panic!("{path} was fetched"));
            assert!(
                err.refused && err.message.ends_with("refused: not a public address"),
                "{path}: {err:?}"
            );
        }
        assert_eq!(other.hits(), 0);
        // Judged from the first URL on, nothing is the same origin as an approved one.
        let err = get(&format!("{base}/"), Check::All, 10, &SYSTEM)
            .await
            .err()
            .expect("refused");
        assert_eq!(err.message, "not a public address");
    }

    #[tokio::test]
    async fn a_judged_name_is_connected_to_the_address_that_was_judged() {
        // `pinned.test` cannot be resolved by the system: the connection works only because it goes to
        // the address the lookup gave, with no second lookup in between.
        let target = listen(vec![]).await;
        let page = format!("http://pinned.test:{}/page", target.port);
        let answer = get(&page, Check::All, 10, &OPEN_LOOPBACK).await.expect("fetched");
        assert_eq!(answer.text, format!("200 ok:{}/page", target.port));
        assert_eq!(answer.url, page);
        // Without a judged hop the same name does not resolve.
        let err = get(&page, Check::Never, 10, &OPEN_LOOPBACK)
            .await
            .err()
            .expect("not resolved");
        assert!(!err.refused, "{err:?}");
    }

    #[tokio::test]
    async fn every_hop_is_judged_and_the_final_url_and_where_it_began_are_reported() {
        fn lookup(host: String) -> BoxFuture<'static, Result<Vec<IpAddr>, String>> {
            Box::pin(async move {
                match host.as_str() {
                    "ok.test" => Ok(vec![IpAddr::V4(Ipv4Addr::LOCALHOST)]),
                    "inside.test" => Ok(vec![IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1))]),
                    _ => Err("no such host".into()),
                }
            })
        }
        // The loopback stands in for the public internet here; the real classifier judges the rest.
        fn internal(ip: &IpAddr) -> bool {
            *ip != IpAddr::V4(Ipv4Addr::LOCALHOST) && internal_ip(ip)
        }
        let route = Route { lookup, internal };
        let unreachable = listen(vec![]).await;
        let site = listen(vec![]).await;
        let origin = listen(vec![
            ("/a", format!("http://ok.test:{}/b", site.port)),
            ("/c", format!("http://inside.test:{}/x", unreachable.port)),
            ("/d", format!("http://gone.test:{}/x", unreachable.port)),
            ("/e", "ftp://ok.test/x".into()),
            ("/loop", "/loop".into()),
        ])
        .await;
        let first = format!("http://ok.test:{}", origin.port);
        let answer = get(&format!("{first}/a"), Check::All, 10, &route)
            .await
            .expect("fetched");
        assert_eq!(answer.text, format!("200 ok:{}/b", site.port));
        assert_eq!(answer.url, format!("http://ok.test:{}/b", site.port));
        assert_eq!(answer.from, Some(format!("{first}/a")));
        let refused = get(&format!("{first}/c"), Check::All, 10, &route)
            .await
            .err()
            .expect("refused");
        assert!(
            refused.refused && refused.message.ends_with("refused: not a public address"),
            "{refused:?}"
        );
        let unresolved = get(&format!("{first}/d"), Check::All, 10, &route)
            .await
            .err()
            .expect("failed");
        assert!(
            !unresolved.refused && unresolved.message.ends_with("failed: no such host"),
            "{unresolved:?}"
        );
        let scheme = get(&format!("{first}/e"), Check::All, 10, &route)
            .await
            .err()
            .expect("refused");
        assert_eq!(scheme.message, "redirect to ftp://ok.test/x refused: not http(s): ftp:");
        let looped = get(&format!("{first}/loop"), Check::All, 3, &route)
            .await
            .err()
            .expect("refused");
        assert_eq!(looped.message, "too many redirects");
        assert_eq!(unreachable.hits(), 0);
    }
}
