//! Update check (general.check_updates / general.update_channel).
//! Default source: GitHub Releases of daxiangme/ShellAX (stable = latest release, beta = newest
//! release including pre-releases). A custom JSON manifest `{"version","url","notes","sha256"}`
//! can still be used via general.update_url / SHELLAX_UPDATE_URL. Check-and-notify only: the
//! user opens the download page; nothing is replaced silently.
use serde::Deserialize;
use std::time::Duration;

pub const CURRENT: &str = env!("CARGO_PKG_VERSION");
/// GitHub repository the releases come from.
pub const REPO: &str = "daxiangme/ShellAX";
/// Human-readable default source (shown in Settings).
pub const DEFAULT_SOURCE: &str = "GitHub Releases (daxiangme/ShellAX)";

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

fn get(url: &str) -> anyhow::Result<ureq::Response> {
    let agent = crate::proxy::ureq_builder(url, ureq::AgentBuilder::new().timeout(Duration::from_secs(10)))?.build();
    match agent.get(url).set("User-Agent", &format!("shellax/{CURRENT}")).set("Accept", "application/vnd.github+json").call() {
        Ok(r) => Ok(r),
        Err(ureq::Error::Status(403 | 429, r)) if r.header("x-ratelimit-remaining") == Some("0") || r.status() == 429 =>
            anyhow::bail!("GitHub API rate limit reached; try again later"),
        Err(ureq::Error::Status(404, _)) => anyhow::bail!("no release published yet"),
        Err(ureq::Error::Status(code, _)) => anyhow::bail!("HTTP {code}"),
        Err(ureq::Error::Transport(t)) => anyhow::bail!("network unavailable ({})", t.kind()),
    }
}

/// Ok(Some(manifest)) when a newer version exists (custom JSON manifest URL).
pub fn check(url: &str) -> anyhow::Result<Option<Manifest>> {
    let m: Manifest = get(url)?.into_json()?;
    Ok(is_newer(&m.version, CURRENT).then_some(m))
}

#[derive(Debug, Clone, Deserialize)]
pub struct Asset { pub name: String, pub browser_download_url: String }
#[derive(Debug, Clone, Deserialize)]
pub struct Release {
    pub tag_name: String,
    #[serde(default)] pub html_url: String,
    #[serde(default)] pub body: Option<String>,
    #[serde(default)] pub draft: bool,
    #[serde(default)] pub prerelease: bool,
    #[serde(default)] pub assets: Vec<Asset>,
}

/// Download asset for this platform: Windows x64 installer (.exe) then zip; macOS arm64 .dmg then zip.
pub fn pick_asset<'a>(assets: &'a [Asset], os: &str, arch: &str) -> Option<&'a Asset> {
    let lname = |a: &Asset| a.name.to_lowercase();
    let pref: &[&dyn Fn(&str) -> bool] = match (os, arch) {
        ("windows", _) => &[&|n: &str| n.ends_with(".exe") && n.contains("setup"), &|n: &str| n.ends_with(".zip") && (n.contains("x64") || n.contains("windows"))],
        ("macos", "aarch64") => &[&|n: &str| n.ends_with(".dmg") && (n.contains("aarch64") || n.contains("arm64") || n.contains("macos")), &|n: &str| n.ends_with(".zip") && (n.contains("aarch64") || n.contains("arm64") || n.contains("macos"))],
        _ => &[],
    };
    pref.iter().find_map(|f| assets.iter().find(|a| f(&lname(a))))
}

/// Turn a release into a manifest when it is newer than this build.
pub fn from_release(r: &Release, os: &str, arch: &str) -> Option<Manifest> {
    if r.draft || !is_newer(&r.tag_name, CURRENT) { return None; }
    let url = pick_asset(&r.assets, os, arch).map(|a| a.browser_download_url.clone()).unwrap_or_else(|| r.html_url.clone());
    Some(Manifest { version: r.tag_name.trim_start_matches('v').to_string(), url, notes: r.body.clone().unwrap_or_default(), sha256: String::new() })
}

/// GitHub Releases: stable = /releases/latest, beta = newest entry of /releases (pre-releases included).
pub fn check_github(channel: &str) -> anyhow::Result<Option<Manifest>> {
    let base = format!("https://api.github.com/repos/{REPO}");
    let rel: Release = if channel == "beta" {
        let all: Vec<Release> = get(&format!("{base}/releases?per_page=10"))?.into_json()?;
        match all.into_iter().find(|r| !r.draft) { Some(r) => r, None => return Ok(None) }
    } else {
        get(&format!("{base}/releases/latest"))?.into_json()?
    };
    Ok(from_release(&rel, std::env::consts::OS, std::env::consts::ARCH))
}

/// Check the configured source: a custom manifest URL when given, else GitHub Releases.
pub fn check_source(custom_url: Option<&str>, channel: &str) -> anyhow::Result<Option<Manifest>> {
    match custom_url { Some(u) => check(&url_for(u, channel)), None => check_github(channel) }
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
        let a = |n: &str| Asset { name: n.into(), browser_download_url: format!("https://x/{n}") };
        let assets = vec![a("ShellAX-macos-aarch64.dmg"), a("ShellAX-portable-x64.zip"), a("ShellAX-Setup-x64.exe")];
        assert_eq!(pick_asset(&assets, "windows", "x86_64").unwrap().name, "ShellAX-Setup-x64.exe");
        assert_eq!(pick_asset(&assets, "macos", "aarch64").unwrap().name, "ShellAX-macos-aarch64.dmg");
        assert!(pick_asset(&assets, "linux", "x86_64").is_none());
        assert_eq!(url_for("x/{channel}", "weird"), "x/stable");
    }
}
