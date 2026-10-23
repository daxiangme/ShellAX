//! Update check (general.check_updates / general.update_channel).
//! Default source: GitHub Releases of daxiangme/ShellAX (stable = latest release, beta = newest
//! release including pre-releases). A custom JSON manifest `{"version","url","notes","sha256"}`
//! can still be used via general.update_url / SHELLAX_UPDATE_URL. Downloading and installing is
//! in `updater` and only happens after the user asks for it (or turns on auto download).
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
            anyhow::bail!("{}", crate::i18n::t("update.rate_limited")),
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

/// 本平台安装包的固定文件名（与 pick_asset 的优先项一致）。
fn asset_name(os: &str, arch: &str) -> Option<&'static str> {
    match (os, arch) {
        ("windows", _) => Some("ShellAX-Setup-x64.exe"),
        ("macos", "aarch64") => Some("ShellAX-macos-aarch64.dmg"),
        _ => None,
    }
}

/// 从 `.../releases/tag/vX.Y.Z` 形式的地址里取出标签。
fn tag_from_url(u: &str) -> Option<String> {
    let i = u.find("/releases/tag/")? + "/releases/tag/".len();
    let tag: String = u[i..].chars().take_while(|ch| !matches!(ch, '"' | '?' | '#' | '/' | '<' | ' ')).collect();
    (!tag.is_empty()).then_some(tag)
}

/// 由标签直接拼出更新信息（不走 API）：安装包为 releases/download/<tag>/<文件名>，说明为 release 网页。
fn manifest_from_tag(tag: &str, os: &str, arch: &str) -> Option<Manifest> {
    if !is_newer(tag, CURRENT) { return None; }
    let page = format!("https://github.com/{REPO}/releases/tag/{tag}");
    let url = asset_name(os, arch).map(|n| format!("https://github.com/{REPO}/releases/download/{tag}/{n}")).unwrap_or_else(|| page.clone());
    Some(Manifest { version: tag.trim_start_matches('v').to_string(), url, notes: page, sha256: String::new() })
}

fn web_agent(url: &str, redirects: u32) -> anyhow::Result<ureq::Agent> {
    Ok(crate::proxy::ureq_builder(url, ureq::AgentBuilder::new().timeout(Duration::from_secs(10)).redirects(redirects))?.build())
}

/// 网页请求的错误说明（保留状态码或底层原因，便于定位）。
fn web_err(step: &str, e: ureq::Error) -> String {
    match e {
        ureq::Error::Status(code, _) => format!("{step}: HTTP {code}"),
        ureq::Error::Transport(t) => format!("{step}: {t}"),
    }
}

/// 网页请求（不带 API 的 Accept 头）。`redirects` 为 0 时 3xx 原样返回。
fn web_get(url: &str, redirects: u32, step: &str) -> Result<ureq::Response, String> {
    let agent = web_agent(url, redirects).map_err(|e| format!("{step}: {e}"))?;
    agent.get(url).set("User-Agent", &format!("shellax/{CURRENT}")).set("Accept", "text/html,application/xhtml+xml,*/*").call().map_err(|e| web_err(step, e))
}

/// 正式版：/releases/latest 会 302 到 /releases/tag/<最新正式版>，不受 API 限流。
/// 依次尝试：读 Location → 跟随重定向取最终地址或页面内容 → releases.atom 第一条；全部失败时返回每一步的原因。
fn latest_tag_web() -> anyhow::Result<String> {
    let url = format!("https://github.com/{REPO}/releases/latest");
    let mut errs: Vec<String> = Vec::new();
    match web_get(&url, 0, "redirect") {
        Ok(r) => match r.header("location").and_then(tag_from_url) {
            Some(t) => return Ok(t),
            None => errs.push(format!("redirect: HTTP {} location={:?}", r.status(), r.header("location"))),
        },
        Err(e) => errs.push(e),
    }
    match web_get(&url, 5, "page") {
        Ok(r) => {
            if let Some(t) = tag_from_url(r.get_url()) { return Ok(t); }
            let final_url = r.get_url().to_string();
            match r.into_string() {
                Ok(body) => match tag_from_url(&body) {
                    Some(t) => return Ok(t),
                    None => errs.push(format!("page: no release tag ({final_url})")),
                },
                Err(e) => errs.push(format!("page: {e}")),
            }
        }
        Err(e) => errs.push(e),
    }
    match latest_stable_from_atom() {
        Ok(t) => return Ok(t),
        Err(e) => errs.push(e),
    }
    anyhow::bail!("{}", errs.join("; "))
}

fn atom_body() -> Result<String, String> {
    let url = format!("https://github.com/{REPO}/releases.atom");
    web_get(&url, 5, "atom")?.into_string().map_err(|e| format!("atom: {e}"))
}

/// releases.atom 里第一条不含预发布后缀（-beta 等）的标签。
fn latest_stable_from_atom() -> Result<String, String> {
    let body = atom_body()?;
    body.split("<entry>").skip(1).filter_map(tag_from_url).find(|t| !t.contains('-')).ok_or_else(|| "atom: no stable release in feed".to_string())
}

/// 测试版：releases.atom 的第一条即最新发布（含预发布），不受 API 限流。
fn latest_tag_atom() -> anyhow::Result<String> {
    let body = atom_body().map_err(|e| anyhow::anyhow!(e))?;
    let entry = body.find("<entry>").map(|i| &body[i..]).ok_or_else(|| anyhow::anyhow!("atom: no release published yet"))?;
    tag_from_url(entry).ok_or_else(|| anyhow::anyhow!("atom: no release tag in feed"))
}

/// GitHub：先用网页重定向 / Atom feed 取标签，都失败再退回 API（API 还能带上更新说明）。
/// 两条路都失败时同时给出两边的原因。
pub fn check_github_web_first(channel: &str) -> anyhow::Result<Option<Manifest>> {
    let (os, arch) = (std::env::consts::OS, std::env::consts::ARCH);
    let tag = if channel == "beta" { latest_tag_atom() } else { latest_tag_web() };
    match tag {
        Ok(t) => Ok(manifest_from_tag(&t, os, arch)),
        Err(web) => check_github(channel).map_err(|api| {
            anyhow::anyhow!("{}", crate::i18n::tf("update.both_failed", &[("web", &web), ("api", &api)]))
        }),
    }
}

/// Check the configured source: a custom manifest URL when given, else GitHub Releases.
pub fn check_source(custom_url: Option<&str>, channel: &str) -> anyhow::Result<Option<Manifest>> {
    match custom_url { Some(u) => check(&url_for(u, channel)), None => check_github_web_first(channel) }
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
        assert_eq!(tag_from_url("https://github.com/daxiangme/ShellAX/releases/tag/v0.1.12").as_deref(), Some("v0.1.12"));
        assert_eq!(tag_from_url(r#"<link href="https://github.com/a/b/releases/tag/v1.0.0-beta.1"/>"#).as_deref(), Some("v1.0.0-beta.1"));
    }
}
#[cfg(test)]
mod live {
    use super::*;
    /// 联网验证网页取标签（默认忽略）：cargo test --lib update::live -- --ignored --nocapture
    #[test]
    #[ignore]
    fn web_and_atom() {
        // 可用 SHELLAX_TEST_PROXY 指定代理验证代理路径
        if let Ok(u) = std::env::var("SHELLAX_TEST_PROXY") { *crate::proxy::URL.lock().unwrap() = u; }
        if std::env::var_os("SHELLAX_TEST_BOTH").is_some() { eprintln!("both={:?}", check_github_web_first("stable").map(|m| m.map(|m| m.version))); }
        let w = latest_tag_web();
        let a = latest_tag_atom();
        eprintln!("web={w:?} atom={a:?}");
        assert!(w.unwrap().starts_with('v'));
        assert!(a.unwrap().starts_with('v'));
    }
}
