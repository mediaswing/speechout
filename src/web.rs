//! Downloads a web page for the Web Reader. Only the page itself is fetched,
//! directly from the site: no images, scripts or other files, no cookies and
//! no referrer, and nothing goes through another service. So the site sees an
//! anonymous visit, and the page's trackers and adverts never run.

use anyhow::{Context, bail};
use std::net::{IpAddr, ToSocketAddrs};
use std::time::Duration;
use url::{Host, Url};

/// Pages larger than this are refused. A long article with all its markup is
/// a few hundred kilobytes.
const MAX_PAGE_BYTES: u64 = 10 * 1024 * 1024;
/// Most redirects followed, such as from "http://" or a short link.
const MAX_REDIRECTS: usize = 5;

/// A downloaded page, before its text is taken out.
pub struct Page {
    /// Where the page ended up, after any redirects.
    pub url: Url,
    pub html: Vec<u8>,
    /// The character set the site says the page is in, such as "utf-8".
    pub charset: Option<String>,
}

/// The address as typed, made into an https:// one. "variety.com/…" gains
/// "https://", and "http://" becomes "https://", since pages are only fetched
/// securely. Where it leads is checked when the page is fetched.
pub fn parse_address(text: &str) -> anyhow::Result<Url> {
    let text = text.trim();
    if text.is_empty() {
        bail!("type or paste the address of a web page");
    }
    let lower = text.to_ascii_lowercase();
    let with_scheme = if lower.starts_with("https://") {
        text.to_owned()
    } else if lower.starts_with("http://") {
        format!("https://{}", &text["http://".len()..])
    } else if text.contains("://") {
        bail!("only web addresses starting with https:// can be read");
    } else {
        format!("https://{text}")
    };
    Url::parse(&with_scheme).ok().filter(|u| u.host().is_some()).context("that is not a web address")
}

/// Pages are only fetched securely, from the internet, without a user name or
/// password in the address.
fn check_url(url: &Url) -> anyhow::Result<()> {
    if url.scheme() != "https" {
        bail!("the page tried to move to an address that isn't secure (https)");
    }
    if !url.username().is_empty() || url.password().is_some() {
        bail!("web addresses with a user name or password in them can't be read");
    }
    let host = url.host().context("that is not a web address")?;
    let addresses: Vec<IpAddr> = match host {
        Host::Ipv4(ip) => vec![IpAddr::V4(ip)],
        Host::Ipv6(ip) => vec![IpAddr::V6(ip)],
        Host::Domain(name) => (name, url.port_or_known_default().unwrap_or(443))
            .to_socket_addrs()
            .with_context(|| format!("could not find {name}. Check the address and your internet connection"))?
            .map(|a| a.ip())
            .collect(),
    };
    // Otherwise a link could reach things on this computer or network,
    // such as Ollama or a router's settings page.
    if addresses.is_empty() || !addresses.iter().all(is_public) {
        bail!("only pages on the internet can be read, not ones on this computer or your local network");
    }
    Ok(())
}

/// Whether `ip` is an internet address, rather than one on this computer,
/// the local network or reserved for something else.
fn is_public(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            let [a, b, ..] = v4.octets();
            !(v4.is_private()
                || v4.is_loopback()
                || v4.is_link_local()
                || v4.is_unspecified()
                || v4.is_broadcast()
                || v4.is_multicast()
                || v4.is_documentation()
                // "This network", which can reach this computer.
                || a == 0
                // Shared address space, used inside internet providers' networks.
                || (a == 100 && (64..128).contains(&b))
                // Reserved for future use.
                || a >= 240)
        }
        IpAddr::V6(v6) => {
            let s = v6.segments();
            // An IPv4 address written in IPv6 form, directly, through NAT64
            // or through 6to4.
            let embedded = if let Some(v4) = v6.to_ipv4_mapped() {
                Some(v4)
            } else if s[..6] == [0x64, 0xff9b, 0, 0, 0, 0] {
                Some(std::net::Ipv4Addr::from((u32::from(s[6]) << 16) | u32::from(s[7])))
            } else if s[0] == 0x2002 {
                Some(std::net::Ipv4Addr::from((u32::from(s[1]) << 16) | u32::from(s[2])))
            } else {
                None
            };
            if let Some(v4) = embedded {
                return is_public(&IpAddr::V4(v4));
            }
            let first = s[0];
            !(v6.is_loopback()
                || v6.is_unspecified()
                || v6.is_multicast()
                // Unique local addresses, the IPv6 version of a private network.
                || (first & 0xfe00) == 0xfc00
                // Link-local addresses.
                || (first & 0xffc0) == 0xfe80)
        }
    }
}

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .https_only(true)
        .http_status_as_error(false)
        // Redirects are followed here instead, so each address is checked.
        .max_redirects(0)
        .timeout_global(Some(Duration::from_secs(20)))
        .user_agent(concat!("speechout/", env!("CARGO_PKG_VERSION")))
        .build()
        .into()
}

/// Downloads the page at `url`, following redirects.
pub fn fetch(url: Url) -> anyhow::Result<Page> {
    let agent = agent();
    let mut url = url;
    for _ in 0..=MAX_REDIRECTS {
        check_url(&url)?;
        let mut resp = agent
            .get(url.as_str())
            .header("Accept", "text/html,application/xhtml+xml;q=0.9,*/*;q=0.1")
            .call()
            .context("could not reach the site. Check the address and your internet connection")?;
        let status = resp.status().as_u16();
        if (300..400).contains(&status) {
            let location = resp
                .headers()
                .get("location")
                .and_then(|l| l.to_str().ok())
                .context("the site sent a redirect without saying where to")?;
            url = url.join(location).context("the site redirected to an address that isn't valid")?;
            if url.scheme() == "http" {
                let _ = url.set_scheme("https");
            }
            continue;
        }
        match status {
            200 => {}
            401 | 403 => bail!("the site refused to send the page (HTTP error {status}). It may only allow web browsers, or need you to sign in"),
            404 | 410 => bail!("the site says the page doesn't exist (HTTP error {status}). Check the address"),
            429 => bail!("the site is getting too many requests (HTTP error 429). Try again later"),
            _ => bail!("the site returned HTTP error {status}"),
        }
        let content_type = resp
            .headers()
            .get("content-type")
            .and_then(|c| c.to_str().ok())
            .unwrap_or_default()
            .to_ascii_lowercase();
        let mime = content_type.split(';').next().unwrap_or_default().trim();
        // A missing type is let through, since the extractor will soon say
        // whether there is an article.
        if !mime.is_empty() && mime != "text/html" && mime != "application/xhtml+xml" {
            bail!("the address is for a file, not a web page. Download it and choose it with Choose File instead");
        }
        let charset = content_type
            .split(';')
            .find_map(|p| p.trim().strip_prefix("charset="))
            .map(|c| c.trim_matches('"').to_owned());
        let html = resp
            .body_mut()
            .with_config()
            .limit(MAX_PAGE_BYTES)
            .read_to_vec()
            .context("the page could not be downloaded, or is too large")?;
        return Ok(Page { url, html, charset });
    }
    bail!("the site redirected too many times")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn address(text: &str) -> String {
        parse_address(text).map(|u| u.to_string()).unwrap_or_else(|e| format!("error: {e}"))
    }

    #[test]
    fn addresses_are_made_secure() {
        assert_eq!(address("https://1.1.1.1/news"), "https://1.1.1.1/news");
        assert_eq!(address("  http://1.1.1.1/news "), "https://1.1.1.1/news");
        assert_eq!(address("1.1.1.1/news"), "https://1.1.1.1/news");
        assert!(address("ftp://1.1.1.1/file").starts_with("error: only web addresses starting with https://"));
        assert!(address("").starts_with("error: type or paste"));
        assert!(address("https://").starts_with("error: that is not a web address"));
    }

    fn checked(text: &str) -> String {
        check_url(&parse_address(text).unwrap()).map(|()| "ok".into()).unwrap_or_else(|e| format!("error: {e}"))
    }

    #[test]
    fn local_addresses_are_refused() {
        for text in [
            "127.0.0.1:11434",
            "https://localhost/",
            "192.168.1.1",
            "10.0.0.5",
            "172.16.0.1",
            "169.254.169.254",
            "100.64.0.1",
            "0.0.0.0",
            "0.1.2.3",
            "[64:ff9b::7f00:1]",
            "[2002:c0a8:101::]",
            "[::1]",
            "[fd00::1]",
            "[fe80::1]",
            "[::ffff:192.168.0.1]",
        ] {
            assert!(checked(text).starts_with("error: only pages on the internet"), "{text} was allowed");
        }
    }

    #[test]
    fn sign_in_details_are_refused() {
        assert!(checked("https://user:secret@1.1.1.1/").starts_with("error: web addresses with a user name"));
    }

    #[test]
    fn public_addresses_are_allowed() {
        assert_eq!(checked("1.1.1.1/news"), "ok");
        assert!(is_public(&"1.1.1.1".parse().unwrap()));
        assert!(is_public(&"2606:4700::1111".parse().unwrap()));
    }

    /// Fetches a real page; run with `cargo test -- --ignored`.
    #[test]
    #[ignore]
    fn fetches_a_page() {
        let page = fetch(parse_address("https://www.bbc.co.uk/news").unwrap()).unwrap();
        assert!(!page.html.is_empty());
    }
}
