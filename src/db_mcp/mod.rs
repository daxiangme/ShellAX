//! Local database MCP server (phase 1+2).
//!
//! The process listens on 127.0.0.1 and requires a bearer token on every request.
//! Tool results never include connection secrets. Grants and the SQL gate live next
//! to the listener so they can be tested without the UI.

mod auth;
mod server;
mod sql;

pub use auth::{authorize, exposed, policy_for, resolve, ConnPolicy, Decision, Deny, Grant, Level};
pub use server::{start, AuditEvent, Backend, ColumnOut, ConfirmReq, ConnView, Limits, PublicConn, RunOut, Running, SetOut};
pub use sql::{analyze, count_sql, Analysis, Engine, Kind, SqlError, TableUse};

use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::assets::{DbKind, Family};

/// How long the UI confirmation may sit before the write is denied.
pub const CONFIRM_WAIT: Duration = Duration::from_secs(180);

/// `auto` keeps the first free port until the user refreshes it. `manual` uses the typed port.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PortMode { Auto, Manual }

impl Default for PortMode {
    fn default() -> Self { Self::Auto }
}

impl PortMode {
    pub fn is_auto(self) -> bool { self == Self::Auto }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct McpConfig {
    pub enabled: bool,
    /// Remembered port. `0` means auto has not assigned one yet.
    pub port: u16,
    #[serde(default)]
    pub port_mode: PortMode,
    pub max_rows: u32,
    pub timeout_secs: u64,
    /// The "MySQL and DM only" hint was closed.
    #[serde(default, skip_serializing_if = "is_false")]
    pub phase_dismissed: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub grants: Vec<Grant>,
}

fn is_false(v: &bool) -> bool { !*v }

impl Default for McpConfig {
    fn default() -> Self { Self { enabled: false, port: 0, port_mode: PortMode::Auto, max_rows: 500, timeout_secs: 30, phase_dismissed: false, grants: vec![] } }
}

impl McpConfig {
    pub fn is_default(&self) -> bool { self == &Self::default() }

    pub fn upsert(&mut self, conn: &str, database: &str, table: &str, level: Level) {
        if let Some(g) = self.find_mut(conn, database, table) {
            g.level = level;
            if table.is_empty() { g.auto = false; }
        } else {
            self.grants.push(Grant { conn: conn.into(), database: database.into(), schema: String::new(), table: table.into(), level, auto: false });
        }
    }

    pub fn set_auto(&mut self, conn: &str, database: &str, table: &str, auto: bool) {
        if table.is_empty() { return; }
        if let Some(g) = self.find_mut(conn, database, table) { g.auto = auto; }
        else { self.grants.push(Grant { conn: conn.into(), database: database.into(), schema: String::new(), table: table.into(), level: Level::Read, auto }); }
    }

    pub fn clear(&mut self, conn: &str, database: &str, table: &str) {
        self.grants.retain(|g| !(g.conn == conn && g.schema.is_empty() && g.database.eq_ignore_ascii_case(database) && g.table.eq_ignore_ascii_case(table)));
    }

    fn find_mut(&mut self, conn: &str, database: &str, table: &str) -> Option<&mut Grant> {
        self.grants.iter_mut().find(|g| g.conn == conn && g.schema.is_empty() && g.database.eq_ignore_ascii_case(database) && g.table.eq_ignore_ascii_case(table))
    }
}

pub fn supported(kind: DbKind) -> bool { matches!(kind.family(), Family::Mysql | Family::Dameng) }

pub fn engine_of(kind: DbKind) -> Option<Engine> {
    match kind.family() {
        Family::Mysql => Some(Engine::Mysql),
        Family::Dameng => Some(Engine::Dameng),
        _ => None,
    }
}

pub fn token_path(data_dir: &Path) -> PathBuf { data_dir.join("db-mcp-token") }

pub fn new_token() -> String {
    use rand::RngCore;
    let mut b = [0u8; 24];
    rand::thread_rng().fill_bytes(&mut b);
    b.iter().map(|x| format!("{x:02x}")).collect()
}

pub fn load_or_create_token(data_dir: &Path) -> anyhow::Result<String> {
    let p = token_path(data_dir);
    if let Ok(s) = std::fs::read_to_string(&p) {
        let s = s.trim().to_string();
        if !s.is_empty() { return Ok(s); }
    }
    let t = new_token();
    store_token(data_dir, &t)?;
    Ok(t)
}

pub fn store_token(data_dir: &Path, token: &str) -> anyhow::Result<()> {
    if let Some(d) = token_path(data_dir).parent() { std::fs::create_dir_all(d)?; }
    write_private(&token_path(data_dir), token.trim())?;
    Ok(())
}

fn write_private(p: &Path, text: &str) -> anyhow::Result<()> {
    let tmp = p.with_extension("tmp");
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        let mut f = std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(&tmp)?;
        f.write_all(text.as_bytes())?;
    }
    #[cfg(not(unix))]
    { std::fs::write(&tmp, text)?; }
    std::fs::rename(tmp, p)?;
    Ok(())
}

/// Drop obvious credentials from an error string before it reaches a client.
pub fn scrub(s: &str) -> String {
    let url = regex::Regex::new(r"(?i)(://[^:/\s]+:)[^@\s]+@").unwrap();
    let kv = regex::Regex::new(r"(?i)\b(password|passwd|pwd)\b(\s*[:=]\s*)\S+").unwrap();
    kv.replace_all(&url.replace_all(s, "$1***@"), "$1$2***").into_owned()
}

pub fn endpoint(port: u16) -> String { format!("http://127.0.0.1:{port}/mcp") }

/// Generic MCP client JSON. No database credentials.
pub fn mcp_json(port: u16, token: &str) -> String {
    format!(
        "{{\n  \"mcpServers\": {{\n    \"shellax-db\": {{\n      \"url\": \"{}\",\n      \"headers\": {{ \"Authorization\": \"Bearer {token}\" }}\n    }}\n  }}\n}}",
        endpoint(port)
    )
}

/// Chinese setup note for an external assistant. Tool names and client config files are part of the note.
pub fn access_prompt(port: u16, token: &str) -> String {
    let url = endpoint(port);
    let json = mcp_json(port, token);
    format!(
        "请把下面的数据库 MCP 服务加入你的 MCP 配置。这是本机 ShellAX 提供的接口，写操作会在 ShellAX 里再次确认。不要在对话里复述令牌。数据库账号和口令不会出现在工具返回里。\n\
         \n\
         服务地址（Streamable HTTP）：{url}\n\
         令牌：{token}\n\
         请求头：Authorization: Bearer {token}\n\
         \n\
         可用工具：list_connections、describe_schema、describe_table、query、execute、explain。\n\
         query 只读；execute 执行已授权的写入；explain 查看执行计划。\n\
         \n\
         通用配置：\n\
         ```json\n\
         {json}\n\
         ```\n\
         \n\
         Claude Code 写入项目或用户目录的 .mcp.json，内容是上面的 mcpServers。\n\
         Cursor 写入 ~/.cursor/mcp.json 或项目里的 .cursor/mcp.json，内容相同。\n\
         Codex 写入 ~/.codex/config.toml：\n\
         \n\
         ```toml\n\
         [mcp_servers.shellax-db]\n\
         url = \"{url}\"\n\
         http_headers = {{ Authorization = \"Bearer {token}\" }}\n\
         ```\n\
         \n\
         写好后重新打开会话，用 list_connections 确认能看到已授权的连接。"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_defaults_and_prompt_has_no_credentials() {
        let c: McpConfig = toml::from_str("enabled = true").unwrap();
        assert!(c.enabled && c.port == 0 && c.max_rows == 500 && c.timeout_secs == 30);
        let token = "abc123token";
        let prompt = access_prompt(9, token);
        let json = mcp_json(9, token);
        assert!(prompt.contains("http://127.0.0.1:9/mcp"));
        assert!(prompt.contains(token));
        assert!(prompt.contains("list_connections") && prompt.contains("execute"));
        assert!(prompt.contains("Claude Code") && prompt.contains("Cursor") && prompt.contains("Codex"));
        assert!(json.contains(token) && json.contains("mcpServers"));
        let low = prompt.to_lowercase();
        assert!(!low.contains("password") && !json.to_lowercase().contains("password"));
        assert_eq!(scrub("connect password=s3cret failed"), "connect password=*** failed");
        assert_eq!(scrub("mysql://root:s3cret@127.0.0.1/db"), "mysql://root:***@127.0.0.1/db");
    }

    #[test]
    fn token_roundtrip_replaces_the_old_value() {
        let dir = std::env::temp_dir().join(format!("shellax-mcp-tok-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let a = load_or_create_token(&dir).unwrap();
        let b = load_or_create_token(&dir).unwrap();
        assert_eq!(a, b);
        store_token(&dir, "next-token").unwrap();
        assert_eq!(load_or_create_token(&dir).unwrap(), "next-token");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
