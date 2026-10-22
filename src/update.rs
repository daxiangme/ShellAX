//! Update check (general.check_updates / general.update_channel).
//! Fetches a small JSON manifest `{"version","url","notes","sha256"}` for the channel and compares
//! versions. Check-and-notify only: downloading/installing (with minisign verification, PRD 5.x)
//! is left to the release pipeline; the prototype opens the download page.
use serde::Deserialize;
use std::time::Duration;

pub const CURRENT: &str = env!("CARGO_PKG_VERSION");
/// `{channel}` is replaced by `stable` / `beta`. Overridable with general.update_url / SHELLAX_UPDATE_URL.
pub const DEFAULT_URL: &str = "https://updates.shellax.dev/{channel}.json";

#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct Manifest {
    pub version: String,
    #[serde(default)] pub url: String,
    #[serde(default)] pub notes: String,
    #[serde(default)] pub sha256: String,
}

fn parts(v: &str) -> (Vec<u64>, String) {
    let v = v.trim().trim_start_matches('v');
    let (main, pre) = v.split_once('-').map(|(a, b)| (a, b.to_string())).unwrap_or((v, String::new()));
    (main.split('.').map(|x| x.parse().unwrap_or(0)).collect(), pre)
}

/// Semver-ish: numeric components, a pre-release (`-beta.1`) sorts before the release.
pub fn is_newer(candidate: &str, current: &str) -> bool {
    let (a, ap) = parts(candidate);
    let (b, bp) = parts(current);
    let n = a.len().max(b.len());
    for i in 0..n {
        let (x, y) = (a.get(i).copied().unwrap_or(0), b.get(i).copied().unwrap_or(0));
        if x != y { return x > y; }
    }
    match (ap.is_empty(), bp.is_empty()) { (true, false) => true, (false, true) => false, _ => ap > bp }
}

pub fn url_for(template: &str, channel: &str) -> String {
    let ch = if channel == "beta" { "beta" } else { "stable" };
    template.replace("{channel}", ch)
}

/// Ok(Some(manifest)) when a newer version exists.
pub fn check(url: &str) -> anyhow::Result<Option<Manifest>> {
    let agent = crate::proxy::ureq_builder(url, ureq::AgentBuilder::new().timeout(Duration::from_secs(10)))?.build();
    let m: Manifest = agent.get(url).set("User-Agent", &format!("shellax/{CURRENT}")).call()?.into_json()?;
    Ok(is_newer(&m.version, CURRENT).then_some(m))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn version_compare() {
        assert!(is_newer("0.2.0", "0.1.9"));
        assert!(is_newer("v1.0.0", "0.9.12"));
        assert!(is_newer("0.1.10", "0.1.9"));
        assert!(!is_newer("0.1.0", "0.1.0"));
        assert!(!is_newer("0.1.0-beta.1", "0.1.0"));
        assert!(is_newer("0.1.0", "0.1.0-beta.2"));
        assert!(is_newer("0.1.0-beta.2", "0.1.0-beta.1"));
        assert_eq!(url_for(DEFAULT_URL, "beta"), "https://updates.shellax.dev/beta.json");
        assert_eq!(url_for("x/{channel}", "weird"), "x/stable");
    }
}
