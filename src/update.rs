//! Checks GitHub for a newer release. Only the public "latest release"
//! endpoint is contacted; nothing about the user or their files is sent.

use anyhow::Context;
use serde_json::Value;
use std::time::Duration;

const LATEST_RELEASE: &str = "https://api.github.com/repos/mediaswing/speechout/releases/latest";
/// Release pages must be on the project's own GitHub page.
const RELEASES_PAGE: &str = "https://github.com/mediaswing/speechout/releases";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Release {
    pub version: String,
    pub url: String,
}

/// Parses "v1.2.3" or "1.2.3-beta" into comparable numbers.
fn parse_version(text: &str) -> Option<(u64, u64, u64)> {
    let core = text.trim().trim_start_matches(['v', 'V']).split(['-', '+']).next()?;
    let mut parts = core.split('.').map(|p| p.parse::<u64>());
    let major = parts.next()?.ok()?;
    let minor = parts.next().unwrap_or(Ok(0)).ok()?;
    let patch = parts.next().unwrap_or(Ok(0)).ok()?;
    Some((major, minor, patch))
}

pub fn is_newer(candidate: &str, current: &str) -> bool {
    matches!((parse_version(candidate), parse_version(current)), (Some(a), Some(b)) if a > b)
}

/// Returns the latest release if it is newer than this copy of the app.
pub fn check() -> anyhow::Result<Option<Release>> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .https_only(true)
        .http_status_as_error(false)
        .timeout_global(Some(Duration::from_secs(15)))
        .user_agent(concat!("speechout/", env!("CARGO_PKG_VERSION")))
        .build()
        .into();
    let mut resp = agent
        .get(LATEST_RELEASE)
        .header("Accept", "application/vnd.github+json")
        .call()
        .context("could not reach GitHub to check for updates")?;
    match resp.status().as_u16() {
        200 => {}
        404 => return Ok(None), // No releases published yet.
        code => anyhow::bail!("GitHub returned HTTP error {code} when checking for updates"),
    }
    let body: Value = resp.body_mut().with_config().limit(1024 * 1024).read_json()?;
    let tag = body["tag_name"].as_str().context("GitHub sent an unexpected reply")?;
    if !is_newer(tag, env!("CARGO_PKG_VERSION")) {
        return Ok(None);
    }
    // Only ever open the project's own release page, whatever the reply says.
    let url = body["html_url"]
        .as_str()
        .filter(|u| u.starts_with(&format!("{RELEASES_PAGE}/")))
        .map_or_else(|| format!("{RELEASES_PAGE}/latest"), str::to_owned);
    let version = tag.trim_start_matches(['v', 'V']).to_owned();
    Ok(Some(Release { version, url }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compares_versions() {
        assert!(is_newer("v1.0.1", "1.0.0"));
        assert!(is_newer("v1.10.0", "1.9.9"));
        assert!(is_newer("2", "1.9.9"));
        assert!(!is_newer("v1.0.0", "1.0.0"));
        assert!(!is_newer("v0.9.0", "1.0.0"));
        assert!(!is_newer("nonsense", "1.0.0"));
        assert!(!is_newer("v1.0.0-beta", "1.0.0"));
    }

    /// Talks to the real GitHub API; run with `cargo test -- --ignored`.
    #[test]
    #[ignore]
    fn checks_github() {
        check().unwrap();
    }
}
