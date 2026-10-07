//! Update check: asks GitHub for the latest published release and compares versions.
//!
//! One HTTPS GET to the public GitHub API at startup (when enabled in settings); nothing about
//! the user or their settings is sent. No release yet, offline, or rate-limited: silently nothing.

use serde::Deserialize;
use std::time::Duration;

pub const REPO: &str = "FlameDevil1/VoiceChangerProject";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Update {
    pub version: String,
    pub url: String,
}

#[derive(Deserialize)]
struct Release {
    tag_name: String,
    html_url: String,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
}

/// `Some` if a newer release than `current` is published. Blocking (call off the UI thread).
pub fn check(current: &str) -> Result<Option<Update>, String> {
    use ureq::tls::{TlsConfig, TlsProvider};
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .tls_config(TlsConfig::builder().provider(TlsProvider::NativeTls).build())
        .timeout_global(Some(Duration::from_secs(8)))
        .http_status_as_error(false)
        .build()
        .into();
    let mut resp = agent
        .get(format!("https://api.github.com/repos/{REPO}/releases/latest"))
        .header("User-Agent", concat!("VoiceChanger/", env!("CARGO_PKG_VERSION")))
        .header("Accept", "application/vnd.github+json")
        .call()
        .map_err(|e| e.to_string())?;
    match resp.status().as_u16() {
        200 => {}
        404 => return Ok(None), // no releases published yet
        s => return Err(format!("GitHub answered {s}")),
    }
    let rel: Release = resp.body_mut().read_json().map_err(|e| e.to_string())?;
    Ok(newer(current, &rel)
        .then(|| Update { version: rel.tag_name.trim_start_matches('v').to_string(), url: rel.html_url }))
}

fn newer(current: &str, rel: &Release) -> bool {
    !rel.draft && !rel.prerelease && parse(&rel.tag_name) > parse(current)
}

/// "v1.2.3" / "1.2" -> (1, 2, 3); anything unparsable counts as 0.
fn parse(v: &str) -> (u32, u32, u32) {
    let mut it = v.trim().trim_start_matches('v').split(['.', '-', '+']).map(|p| p.parse().unwrap_or(0));
    (it.next().unwrap_or(0), it.next().unwrap_or(0), it.next().unwrap_or(0))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rel(tag: &str) -> Release {
        Release { tag_name: tag.into(), html_url: String::new(), draft: false, prerelease: false }
    }

    #[test]
    fn version_comparison() {
        assert!(newer("0.1.0", &rel("v0.2.0")));
        assert!(newer("0.1.9", &rel("v0.1.10")), "numeric, not text, comparison");
        assert!(!newer("0.2.0", &rel("v0.2.0")));
        assert!(!newer("1.0.0", &rel("v0.9.9")));
        assert!(!newer("0.1.0", &Release { prerelease: true, ..rel("v9.0.0") }));
        assert_eq!(parse("v1.2"), (1, 2, 0));
    }
}
