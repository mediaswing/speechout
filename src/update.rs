//! Checking GitHub for a newer release.
//!
//! The only request is for the latest release's version number, over HTTPS
//! to GitHub's API, without following redirects. Nothing about the user or
//! this computer is sent beyond what any web request carries. The app never
//! downloads or installs anything by itself: it says a new version is out and
//! links to the releases page, which the user opens in their web browser.
//! Checking when the app starts can be turned off on the Settings tab.

use std::sync::OnceLock;
use std::time::Duration;

use anyhow::{Context, bail};
use serde_json::Value;

/// Where people download new versions. The link the app shows is always this
/// one, never an address from the reply.
pub const RELEASES_URL: &str = "https://github.com/mediaswing/speechout/releases/latest";
/// The newest published release that isn't a draft or a pre-release.
const LATEST_API: &str = "https://api.github.com/repos/mediaswing/speechout/releases/latest";
/// A release's details are a few kilobytes, so this leaves plenty of room.
const MAX_REPLY_BYTES: u64 = 1024 * 1024;

/// A version number: major, minor and patch, as in `5.0.0`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Version(u32, u32, u32);

impl Version {
    /// Reads `5.0.0` or `v5.0.0`. Anything else, such as a pre-release like
    /// `5.1.0-beta`, isn't a version this app offers.
    pub fn parse(text: &str) -> Option<Self> {
        let text = text.trim();
        let text = text.strip_prefix(['v', 'V']).unwrap_or(text);
        let mut parts = text.split('.').map(|part| {
            // u32's own parsing would take a leading "+".
            if part.is_empty() || part.len() > 9 || !part.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            part.parse::<u32>().ok()
        });
        let version = Version(parts.next()??, parts.next()??, parts.next()??);
        parts.next().is_none().then_some(version)
    }

    /// The version that is running.
    pub fn current() -> Self {
        Self::parse(env!("CARGO_PKG_VERSION")).expect("Cargo.toml has a three-part version")
    }
}

impl std::fmt::Display for Version {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(f, "{}.{}.{}", self.0, self.1, self.2)
    }
}

fn agent() -> &'static ureq::Agent {
    static AGENT: OnceLock<ureq::Agent> = OnceLock::new();
    AGENT.get_or_init(|| {
        ureq::Agent::config_builder()
            .https_only(true)
            .max_redirects(0)
            .http_status_as_error(false)
            .timeout_connect(Some(Duration::from_secs(15)))
            .timeout_global(Some(Duration::from_secs(30)))
            // GitHub's API refuses requests without one.
            .user_agent(concat!("commonspeech/", env!("CARGO_PKG_VERSION")))
            .build()
            .into()
    })
}

/// Asks GitHub for the latest release's version. Blocks, so it runs on a
/// background thread.
pub fn latest() -> anyhow::Result<Version> {
    let mut reply = agent()
        .get(LATEST_API)
        .header("Accept", "application/vnd.github+json")
        .call()
        .context("GitHub could not be reached. Check your internet connection")?;
    let status = reply.status().as_u16();
    match status {
        200..=299 => {}
        404 => bail!("no release has been published on GitHub yet"),
        403 | 429 => bail!("GitHub has had too many requests from this network. Try again later"),
        _ => bail!("GitHub refused the request (HTTP error {status})"),
    }
    let body: Value = reply
        .body_mut()
        .with_config()
        .limit(MAX_REPLY_BYTES)
        .read_json()
        .context("GitHub sent a reply that could not be understood")?;
    version_of(&body)
}

fn version_of(release: &Value) -> anyhow::Result<Version> {
    release["tag_name"]
        .as_str()
        .and_then(Version::parse)
        .context("the latest release on GitHub has a version number this app doesn't understand")
}

/// The latest version, if it is newer than this one.
pub fn newer(latest: Version) -> Option<Version> {
    (latest > Version::current()).then_some(latest)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn reads_version_numbers() {
        assert_eq!(Version::parse("5.0.0"), Some(Version(5, 0, 0)));
        assert_eq!(Version::parse("v5.10.2"), Some(Version(5, 10, 2)));
        assert_eq!(Version::parse(" V1.2.3 "), Some(Version(1, 2, 3)));
        for bad in ["", "5", "5.0", "5.0.0.1", "5.0.0-beta", "+5.0.0", "5.+0.0", "5..0", "v", "five.0.0", "9999999999.0.0"] {
            assert_eq!(Version::parse(bad), None, "{bad}");
        }
    }

    #[test]
    fn this_version_can_be_read() {
        assert_eq!(Version::current().to_string(), env!("CARGO_PKG_VERSION"));
    }

    #[test]
    fn versions_compare_by_number_not_text() {
        assert!(Version(5, 10, 0) > Version(5, 9, 9));
        assert!(Version(6, 0, 0) > Version(5, 99, 99));
        let current = Version::current();
        assert_eq!(newer(current), None);
        assert_eq!(newer(Version(0, 0, 1)), None);
        let next = Version(current.0, current.1, current.2 + 1);
        assert_eq!(newer(next), Some(next));
    }

    #[test]
    fn reads_the_version_from_a_release() {
        assert_eq!(version_of(&json!({"tag_name": "v5.1.0", "html_url": "https://example.com"})).unwrap(), Version(5, 1, 0));
        assert!(version_of(&json!({"tag_name": "v5.1.0-rc1"})).is_err());
        assert!(version_of(&json!({})).is_err());
    }

    #[test]
    fn the_releases_page_is_on_github_over_https() {
        assert!(RELEASES_URL.starts_with("https://github.com/mediaswing/speechout/"));
        assert!(LATEST_API.starts_with("https://api.github.com/repos/mediaswing/speechout/"));
    }
}
