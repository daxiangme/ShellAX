//! Right-hand card for the local database MCP server.
//!
//! The listener is optional. Grants and schema browsing work while it is off.
//! Write confirmation is a dialog on the main window, same family as the approval card.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use gpui::{anchored, deferred, div, prelude::*, px, AnyElement, Context, Entity, Pixels, Point, SharedString, Window};
use gpui_component::input::InputState;
use ssh_term_proto::assets::{self, DbConn};
use ssh_term_proto::db_mcp::{self, Backend, Level};
use ssh_term_proto::i18n::{t, tf};

use crate::mcp_backend::{Ask, McpBackend};
use crate::theme::{c, cur};
use crate::ui::{self, IconName};
use crate::{config, secondary, Root};

const SEP: char = '\u{1f}';

enum Kid { Loading, Ready(Vec<String>), Err(String) }

#[derive(Clone)]
enum McpTarget {
    Conn { id: String, read_only: bool },
    Db { conn: String, database: String, read_only: bool },
    Table { conn: String, database: String, table: String, read_only: bool },
}

impl McpTarget {
    fn parts(&self) -> (&str, &str, &str, bool) {
        match self {
            Self::Conn { id, read_only } => (id, "", "", *read_only),
            Self::Db { conn, database, read_only } => (conn, database, "", *read_only),
            Self::Table { conn, database, table, read_only } => (conn, database, table, *read_only),
        }
    }
    fn expandable(&self) -> bool { !matches!(self, Self::Table { .. }) }
    fn load_key(&self) -> String {
        let (conn, db, _, _) = self.parts();
        if db.is_empty() { conn.to_string() } else { format!("{conn}{SEP}{db}") }
    }
}

struct McpRow {
    depth: usize,
    title: String,
    kind: String,
    level: Option<Level>,
    inherited: bool,
    auto: bool,
    production: bool,
    open: bool,
    target: Option<McpTarget>,
}

pub(crate) struct McpState {
    pub server: Option<Arc<db_mcp::Running>>,
    pub backend: Arc<McpBackend>,
    pub show_token: bool,
    pub token: String,
    /// `true` keeps the remembered port until the user refreshes it.
    pub port_auto: bool,
    pub port_in: Entity<InputState>,
    pub rows_in: Entity<InputState>,
    pub secs_in: Entity<InputState>,
    /// Timeout and row limit stay folded until the user opens them.
    pub limits_open: bool,
    pub open: HashSet<String>,
    kids: HashMap<String, Kid>,
    menu: Option<(McpTarget, Point<Pixels>)>,
    pub confirms: Vec<Ask>,
}

impl McpState {
    pub(crate) fn new(backend: Arc<McpBackend>, window: &mut Window, cx: &mut Context<Root>) -> Self {
        let cfg = config::get().db_mcp.clone();
        let port_auto = cfg.port_mode.is_auto();
        let port_text = if cfg.port == 0 { String::new() } else { cfg.port.to_string() };
        Self {
            server: None,
            backend,
            show_token: false,
            token: String::new(),
            port_auto,
            port_in: cx.new(|cx| InputState::new(window, cx).placeholder("1-65535").default_value(port_text)),
            rows_in: cx.new(|cx| InputState::new(window, cx).placeholder("500").default_value(cfg.max_rows.to_string())),
            secs_in: cx.new(|cx| InputState::new(window, cx).placeholder("30").default_value(cfg.timeout_secs.to_string())),
            limits_open: false,
            open: HashSet::new(),
            kids: HashMap::new(),
            menu: None,
            confirms: vec![],
        }
    }
}

fn split_key(key: &str) -> (String, String) {
    match key.split_once(SEP) {
        Some((c, d)) => (c.to_string(), d.to_string()),
        None => (key.to_string(), String::new()),
    }
}

fn mask_token(token: &str) -> String {
    if token.chars().count() <= 8 { return "••••••••".into(); }
    let head: String = token.chars().take(4).collect();
    let tail: String = token.chars().rev().take(4).collect::<String>().chars().rev().collect();
    format!("{head}••••{tail}")
}

fn policy_for_conn(c: &DbConn) -> db_mcp::ConnPolicy {
    let s = config::get();
    let production = assets::db_is_production(c, &s.groups, &s.tags.production);
    db_mcp::policy_for(&s.db_mcp.grants, &c.id, c.read_only, production)
}

fn explicit(conn: &str, database: &str, table: &str) -> bool {
    config::get().db_mcp.grants.iter().any(|g| g.conn == conn && g.schema.is_empty() && g.database.eq_ignore_ascii_case(database) && g.table.eq_ignore_ascii_case(table))
}

fn level_color(level: Level) -> gpui::Hsla {
    let th = cur();
    match level {
        Level::Hidden => c(th.overlay1),
        Level::Read => c(th.info),
        Level::Write => c(th.warning),
        Level::Ddl => c(th.error),
    }
}

impl Root {
    pub(crate) fn mcp_sync_token(&mut self) {
        if let Some(s) = &self.mcp.server { self.mcp.token = s.token(); return; }
        self.mcp.token = db_mcp::load_or_create_token(&crate::data_dir()).unwrap_or_default();
    }

    pub(crate) fn mcp_listen(&mut self, cx: &mut Context<Self>) {
        if secondary() { return; }
        let cfg = config::get().db_mcp.clone();
        if !cfg.enabled {
            self.mcp.server = None;
            self.mcp_sync_token();
            return;
        }
        let token = match db_mcp::load_or_create_token(&crate::data_dir()) {
            Ok(t) => t,
            Err(e) => {
                self.mcp.server = None;
                self.toast(tf("mcp.token_failed", &[("e", &e)]), cx);
                return;
            }
        };
        match db_mcp::start(cfg.port, token, self.mcp.backend.clone() as Arc<dyn db_mcp::Backend>) {
            Ok(running) => {
                let port = running.port();
                if port != cfg.port {
                    if let Some(e) = config::update(|s| s.db_mcp.port = port) { self.toast(e, cx); }
                }
                self.mcp.server = Some(Arc::new(running));
                self.mcp_sync_token();
                self.log("db.mcp", format!("listening {port}"));
            }
            Err(e) => {
                self.mcp.server = None;
                self.toast(tf("mcp.start_failed", &[("e", &e)]), cx);
            }
        }
    }

    fn mcp_toggle_enabled(&mut self, cx: &mut Context<Self>) {
        if secondary() { self.toast(t("mcp.secondary").into(), cx); return; }
        let on = self.mcp.server.is_none();
        if let Some(e) = config::update(|s| s.db_mcp.enabled = on) { self.toast(e, cx); return; }
        if on { self.mcp_listen(cx); } else { self.mcp.server = None; self.mcp_sync_token(); }
        cx.notify();
    }

    fn mcp_copy(&mut self, json_only: bool, cx: &mut Context<Self>) {
        if secondary() { self.toast(t("mcp.secondary").into(), cx); return; }
        let Some(srv) = self.mcp.server.clone() else { self.toast(t("mcp.need_running").into(), cx); return; };
        let text = if json_only { db_mcp::mcp_json(srv.port(), &srv.token()) } else { db_mcp::access_prompt(srv.port(), &srv.token()) };
        cx.write_to_clipboard(gpui::ClipboardItem::new_string(text));
        self.toast(t("mcp.copied").into(), cx);
    }

    fn mcp_copy_token(&mut self, cx: &mut Context<Self>) {
        if self.mcp.token.is_empty() { self.mcp_sync_token(); }
        cx.write_to_clipboard(gpui::ClipboardItem::new_string(self.mcp.token.clone()));
        self.toast(t("mcp.copied").into(), cx);
    }

    fn mcp_regen(&mut self, cx: &mut Context<Self>) {
        if secondary() { self.toast(t("mcp.secondary").into(), cx); return; }
        let token = db_mcp::new_token();
        if let Err(e) = db_mcp::store_token(&crate::data_dir(), &token) {
            self.toast(tf("mcp.token_failed", &[("e", &e)]), cx);
            return;
        }
        if let Some(s) = &self.mcp.server { s.set_token(token.clone()); }
        self.mcp.token = token;
        self.toast(t("mcp.regen_done").into(), cx);
        cx.notify();
    }

    /// 记住手动填写的端口。服务已在监听时按这个端口重新绑定，失败则仍停在原端口。
    fn mcp_save_port(&mut self, port: u16, cx: &mut Context<Self>) {
        if port == 0 || port == config::get().db_mcp.port { cx.notify(); return; }
        let prev = config::get().db_mcp.port;
        if let Some(e) = config::update(|s| s.db_mcp.port = port) { self.toast(e, cx); return; }
        if secondary() { self.toast(t("mcp.secondary").into(), cx); return; }
        if self.mcp.server.is_some() {
            self.mcp.server = None;
            self.mcp_listen(cx);
            if self.mcp.server.is_none() {
                let _ = config::update(|s| s.db_mcp.port = prev);
            }
        }
        cx.notify();
    }

    fn mcp_set_port_mode(&mut self, auto: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.mcp.port_auto == auto { return; }
        self.mcp.port_auto = auto;
        let mode = if auto { db_mcp::PortMode::Auto } else { db_mcp::PortMode::Manual };
        if let Some(e) = config::update(|s| s.db_mcp.port_mode = mode) { self.toast(e, cx); return; }
        if !auto {
            let saved = config::get().db_mcp.port;
            let live = self.mcp.server.as_ref().map(|s| s.port()).unwrap_or(0);
            let p = if saved > 0 { saved } else { live };
            let cur = self.mcp.port_in.read(cx).value().trim().to_string();
            if (cur.is_empty() || cur == "0") && p > 0 {
                self.mcp.port_in.update(cx, |s, cx| s.set_value(p.to_string(), window, cx));
            }
        }
        cx.notify();
    }

    fn mcp_refresh_port(&mut self, cx: &mut Context<Self>) {
        if !self.mcp.port_auto { return; }
        if secondary() { self.toast(t("mcp.secondary").into(), cx); return; }
        let enabled = config::get().db_mcp.enabled;
        self.mcp.server = None;
        let token = match db_mcp::load_or_create_token(&crate::data_dir()) {
            Ok(t) => t,
            Err(e) => {
                self.toast(tf("mcp.token_failed", &[("e", &e)]), cx);
                if enabled { self.mcp_listen(cx); }
                return;
            }
        };
        match db_mcp::start(0, token, self.mcp.backend.clone() as Arc<dyn db_mcp::Backend>) {
            Ok(running) => {
                let port = running.port();
                if let Some(e) = config::update(|s| s.db_mcp.port = port) { self.toast(e, cx); }
                if enabled {
                    self.mcp.server = Some(Arc::new(running));
                    self.mcp_sync_token();
                }
                self.toast(t("mcp.port_refreshed").into(), cx);
                cx.notify();
            }
            Err(e) => {
                self.toast(tf("mcp.start_failed", &[("e", &e)]), cx);
                if enabled { self.mcp_listen(cx); }
            }
        }
    }

    fn mcp_copy_addr(&mut self, cx: &mut Context<Self>) {
        let port = self.mcp.server.as_ref().map(|s| s.port()).filter(|p| *p > 0).or_else(|| {
            let p = config::get().db_mcp.port;
            (p > 0).then_some(p)
        });
        let Some(port) = port else { self.toast(t("mcp.need_running").into(), cx); return; };
        cx.write_to_clipboard(gpui::ClipboardItem::new_string(db_mcp::endpoint(port)));
        self.toast(t("mcp.copied").into(), cx);
    }

    /// 手动端口在回车时立刻生效；离开输入框时只接受已经填好的合法端口。
    pub(crate) fn mcp_commit_port(&mut self, strict: bool, cx: &mut Context<Self>) {
        if self.mcp.port_auto { return; }
        match self.mcp.port_in.read(cx).value().trim().parse::<u16>() {
            Ok(p) if p > 0 => self.mcp_save_port(p, cx),
            _ if strict => self.toast(t("mcp.port_invalid").into(), cx),
            _ => {}
        }
    }

    fn mcp_apply(&mut self, cx: &mut Context<Self>) {
        let cfg = config::get().db_mcp.clone();
        let rows = self.mcp.rows_in.read(cx).value().trim().parse::<u32>().unwrap_or(cfg.max_rows).clamp(1, 10_000);
        let secs = self.mcp.secs_in.read(cx).value().trim().parse::<u64>().unwrap_or(cfg.timeout_secs).clamp(1, 300);
        if let Some(e) = config::update(|s| { s.db_mcp.max_rows = rows; s.db_mcp.timeout_secs = secs; }) {
            self.toast(e, cx);
            return;
        }
        self.toast(t("mcp.applied").into(), cx);
    }

    fn mcp_dismiss_phase(&mut self, cx: &mut Context<Self>) {
        if let Some(e) = config::update(|s| s.db_mcp.phase_dismissed = true) { self.toast(e, cx); return; }
        cx.notify();
    }

    fn mcp_load(&mut self, key: String, cx: &mut Context<Self>) {
        self.mcp.kids.insert(key.clone(), Kid::Loading);
        let backend = self.mcp.backend.clone();
        cx.spawn(async move |this, cx| {
            let key2 = key.clone();
            let res = cx.background_executor().spawn(async move {
                let (conn, db) = split_key(&key2);
                if db.is_empty() { backend.databases(&conn) } else { backend.tables(&conn, &db) }
            }).await;
            let kid = match res { Ok(v) => Kid::Ready(v), Err(e) => Kid::Err(e) };
            let _ = this.update(cx, |r, cx| { r.mcp.kids.insert(key, kid); cx.notify(); });
        }).detach();
    }

    fn mcp_toggle_node(&mut self, key: String, cx: &mut Context<Self>) {
        if !self.mcp.open.insert(key.clone()) { self.mcp.open.remove(&key); }
        else if !self.mcp.kids.contains_key(&key) { self.mcp_load(key, cx); }
        cx.notify();
    }

    fn mcp_set_level(&mut self, target: &McpTarget, level: Level, cx: &mut Context<Self>) {
        let (conn, db, table, read_only) = target.parts();
        if read_only && level > Level::Read {
            self.toast(t("mcp.read_only_cap").into(), cx);
            return;
        }
        let (conn, db, table) = (conn.to_string(), db.to_string(), table.to_string());
        if let Some(e) = config::update(move |s| s.db_mcp.upsert(&conn, &db, &table, level)) { self.toast(e, cx); }
        cx.notify();
    }

    fn mcp_cycle(&mut self, target: &McpTarget, cx: &mut Context<Self>) {
        let (conn, db, table, read_only) = target.parts();
        let Some(c) = config::get().db.iter().find(|c| c.id == conn).cloned() else { return };
        let (lvl, _) = db_mcp::resolve(&policy_for_conn(&c), db, table);
        let next = if read_only { if lvl == Level::Hidden { Level::Read } else { Level::Hidden } } else { lvl.cycle() };
        self.mcp_set_level(target, next, cx);
    }

    fn mcp_inherit(&mut self, target: &McpTarget, cx: &mut Context<Self>) {
        let (conn, db, table, _) = target.parts();
        if db.is_empty() && table.is_empty() { return; }
        let (conn, db, table) = (conn.to_string(), db.to_string(), table.to_string());
        if let Some(e) = config::update(move |s| s.db_mcp.clear(&conn, &db, &table)) { self.toast(e, cx); }
        cx.notify();
    }

    fn mcp_toggle_auto(&mut self, target: &McpTarget, cx: &mut Context<Self>) {
        let (conn, db, table, read_only) = target.parts();
        if table.is_empty() { return; }
        if read_only { self.toast(t("mcp.read_only_cap").into(), cx); return; }
        let on = config::get().db_mcp.grants.iter().any(|g| g.conn == conn && g.schema.is_empty() && g.database.eq_ignore_ascii_case(db) && g.table.eq_ignore_ascii_case(table) && g.auto);
        let (conn, db, table) = (conn.to_string(), db.to_string(), table.to_string());
        if let Some(e) = config::update(move |s| s.db_mcp.set_auto(&conn, &db, &table, !on)) { self.toast(e, cx); }
        cx.notify();
    }

    fn mcp_reload(&mut self, target: &McpTarget, cx: &mut Context<Self>) {
        let key = target.load_key();
        let prefix = format!("{key}{SEP}");
        self.mcp.kids.retain(|k, _| k != &key && !k.starts_with(&prefix));
        if self.mcp.open.contains(&key) { self.mcp_load(key, cx); }
        cx.notify();
    }

    fn mcp_rows(&self) -> Vec<McpRow> {
        let cfg = config::get();
        let mut rows = Vec::new();
        for c in cfg.db.iter().filter(|c| db_mcp::supported(c.kind)) {
            let policy = policy_for_conn(c);
            let (level, _) = db_mcp::resolve(&policy, "", "");
            let open = self.mcp.open.contains(&c.id);
            rows.push(McpRow {
                depth: 0, title: c.name.clone(), kind: c.kind.label().into(), level: Some(level),
                inherited: !explicit(&c.id, "", ""), auto: false,
                production: assets::db_is_production(c, &cfg.groups, &cfg.tags.production),
                open, target: Some(McpTarget::Conn { id: c.id.clone(), read_only: c.read_only }),
            });
            if !open { continue; }
            self.push_kids(&mut rows, &c.id, "", 1, c.read_only, &policy);
        }
        rows
    }

    fn push_kids(&self, rows: &mut Vec<McpRow>, conn: &str, database: &str, depth: usize, read_only: bool, policy: &db_mcp::ConnPolicy) {
        let key = if database.is_empty() { conn.to_string() } else { format!("{conn}{SEP}{database}") };
        match self.mcp.kids.get(&key) {
            None | Some(Kid::Loading) => rows.push(McpRow { depth, title: t("mcp.loading").into(), kind: String::new(), level: None, inherited: false, auto: false, production: false, open: false, target: None }),
            Some(Kid::Err(e)) => rows.push(McpRow { depth, title: e.clone(), kind: String::new(), level: None, inherited: false, auto: false, production: false, open: false, target: None }),
            Some(Kid::Ready(names)) => {
                for name in names {
                    if database.is_empty() {
                        let child = format!("{conn}{SEP}{name}");
                        let (level, _) = db_mcp::resolve(policy, name, "");
                        let open = self.mcp.open.contains(&child);
                        rows.push(McpRow {
                            depth, title: name.clone(), kind: String::new(), level: Some(level),
                            inherited: !explicit(conn, name, ""), auto: false, production: false, open,
                            target: Some(McpTarget::Db { conn: conn.into(), database: name.clone(), read_only }),
                        });
                        if open { self.push_kids(rows, conn, name, depth + 1, read_only, policy); }
                    } else {
                        let (level, auto) = db_mcp::resolve(policy, database, name);
                        rows.push(McpRow {
                            depth, title: name.clone(), kind: String::new(), level: Some(level),
                            inherited: !explicit(conn, database, name), auto, production: false, open: false,
                            target: Some(McpTarget::Table { conn: conn.into(), database: database.into(), table: name.clone(), read_only }),
                        });
                    }
                }
            }
        }
    }

    pub(crate) fn mcp_dock(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let th = cur();
        let listening = self.mcp.server.as_ref().map(|s| s.port());
        let sub = Some(t(if listening.is_some() { "mcp.on" } else { "mcp.off" }).into());
        let actions: Vec<AnyElement> = vec![
            ui::icon_button_state("mcp-power", IconName::Power, ui::IconBtn::Md, if listening.is_some() { t("mcp.disable") } else { t("mcp.enable") }, listening.is_some(), false)
                .on_click(cx.listener(|this, _, _, cx| this.mcp_toggle_enabled(cx))).into_any_element(),
            ui::icon_button("mcp-prompt", IconName::ClipboardCopy, ui::IconBtn::Md, t("mcp.copy_prompt"))
                .on_click(cx.listener(|this, _, _, cx| this.mcp_copy(false, cx))).into_any_element(),
            ui::icon_button("mcp-json", IconName::Braces, ui::IconBtn::Md, t("mcp.copy_json"))
                .on_click(cx.listener(|this, _, _, cx| this.mcp_copy(true, cx))).into_any_element(),
            ui::icon_button("mcp-regen", IconName::RefreshCw, ui::IconBtn::Md, t("mcp.regen"))
                .on_click(cx.listener(|this, _, _, cx| this.mcp_regen(cx))).into_any_element(),
        ];
        let shown = if self.mcp.show_token { self.mcp.token.clone() } else { mask_token(&self.mcp.token) };
        let port_auto = self.mcp.port_auto;
        let port_modes = vec![("auto".to_string(), t("mcp.port_auto").to_string()), ("manual".to_string(), t("mcp.port_manual").to_string())];
        let port_mode = if port_auto { "auto" } else { "manual" };
        let me = cx.entity().downgrade();
        let port_in = self.mcp.port_in.clone();
        let rows_in = self.mcp.rows_in.clone();
        let secs_in = self.mcp.secs_in.clone();
        let limits_open = self.mcp.limits_open;
        let phase_on = !config::get().db_mcp.phase_dismissed;
        let saved_port = config::get().db_mcp.port;
        let port_num = listening.or_else(|| (saved_port > 0).then_some(saved_port));
        let addr_live = listening.is_some();
        let label = |text: &str| div().text_size(px(ui::fs_small())).text_color(c(th.subtext)).child(text.to_string());
        let field = |name: &str, input: &Entity<InputState>| div().flex_1().min_w_0().flex().flex_col().gap(px(ui::SP_1)).child(label(name)).child(ui::text_field(input, false));
        let rows = self.mcp_rows();
        let mut list = div().id("mcp-list").flex_1().min_h_0().overflow_y_scroll().pb(px(ui::SP_2)).flex().flex_col();
        if rows.is_empty() {
            list = list.child(ui::empty_state(IconName::Database, t("mcp.empty_title"), Some(t("mcp.empty_desc").into()), vec![], false));
        }
        for (i, row) in rows.iter().enumerate() {
            if row.target.is_none() {
                list = list.child(div().pl(px(ui::ROW_PX + ui::INDENT * row.depth as f32)).pr(px(ui::SP_2)).py(px(ui::SP_1)).child(ui::row_meta(row.title.clone())));
                continue;
            }
            let target = row.target.clone().unwrap();
            let key = target.load_key();
            let expandable = target.expandable();
            let gid = SharedString::from(format!("mcp-g-{i}"));
            let open = row.open;
            let cycle = target.clone();
            let more = target.clone();
            list = list.child(ui::list_row(("mcp-row", i), row.depth, false).group(gid.clone())
                .on_click(cx.listener(move |this, _, _, cx| {
                    if expandable { this.mcp_toggle_node(key.clone(), cx); }
                }))
                .child(div().flex_shrink_0().child(ui::icon(if !expandable { IconName::Table } else if open { IconName::ChevronDown } else { IconName::ChevronRight }, ui::IC_XS, c(th.overlay1))))
                .child(div().flex_1().min_w_0().flex().items_center().gap(px(ui::SP_1))
                    .child(ui::row_title(row.title.clone()))
                    .when(!row.kind.is_empty(), |d| d.child(ui::tag(row.kind.clone(), c(th.subtext))))
                    .when(row.production, |d| d.child(ui::tag(t("mcp.prod"), c(th.warning))))
                    .when_some(row.level, |d, lvl| d.child(ui::tag(t(lvl.label_key()), level_color(lvl))))
                    .when(row.inherited && row.level.is_some(), |d| d.child(ui::tag(t("mcp.inherit"), c(th.overlay1))))
                    .when(row.auto, |d| d.child(ui::tag(t("mcp.auto"), c(th.warning)))))
                .child(ui::row_actions(gid)
                    .child(ui::icon_button(("mcp-cycle", i), IconName::ArrowRightLeft, ui::IconBtn::Sm, t("mcp.cycle"))
                        .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .on_click(cx.listener(move |this, _, _, cx| { cx.stop_propagation(); this.mcp_cycle(&cycle, cx); })))
                    .child(ui::icon_button(("mcp-more", i), IconName::Ellipsis, ui::IconBtn::Sm, t("mcp.more"))
                        .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .on_click(cx.listener(move |this, e: &gpui::ClickEvent, _, cx| {
                            cx.stop_propagation();
                            this.mcp.menu = Some((more.clone(), e.position() + Point::new(px(-8.), px(14.))));
                            cx.notify();
                        })))));
        }
        let mut body = div().flex_1().min_h_0().flex().flex_col().pt(px(ui::SP_2))
            .when(secondary(), |d| d.child(ui::notice(ui::Notice::Info, t("mcp.secondary")).mb(px(ui::SP_2))))
            .child(div().px(px(ui::PANEL_PX)).pb(px(ui::SP_2)).flex().flex_col().gap(px(ui::SP_2))
                .child(div().flex().items_center().gap(px(ui::SP_2))
                    .child(div().flex_shrink_0().child(label(&t("mcp.token"))))
                    .child(div().flex_1().min_w_0().whitespace_nowrap().overflow_hidden().text_ellipsis().font_family(ui::mono_family()).text_size(px(ui::fs_mono())).line_height(px(ui::lh(ui::fs_mono()))).text_color(c(th.text)).child(shown))
                    .child(ui::icon_button("mcp-eye", if self.mcp.show_token { IconName::EyeOff } else { IconName::Eye }, ui::IconBtn::Sm, if self.mcp.show_token { t("mcp.hide_token") } else { t("mcp.show_token") })
                        .on_click(cx.listener(|this, _, _, cx| { this.mcp.show_token = !this.mcp.show_token; cx.notify(); })))
                    .child(ui::icon_button("mcp-tok-copy", IconName::Copy, ui::IconBtn::Sm, t("mcp.copy_token"))
                        .on_click(cx.listener(|this, _, _, cx| this.mcp_copy_token(cx)))))
                .child(div().h(px(ui::H_DEFAULT)).px(px(ui::SP_2)).rounded(px(ui::R_MD)).bg(crate::theme::sub_bg()).flex().items_center().gap(px(ui::SP_1))
                    .child(div().flex_1().min_w_0().overflow_hidden().text_ellipsis()
                        .when_some(port_num, |d, p| d.font_family(ui::mono_family()).text_size(px(ui::fs_mono())).line_height(px(ui::lh(ui::fs_mono()))).text_color(if addr_live { c(th.text) } else { c(th.subtext) }).child(format!("127.0.0.1:{p}")))
                        .when(port_num.is_none(), |d| d.text_size(px(ui::fs_small())).text_color(c(th.overlay1)).child(t("mcp.addr_pending"))))
                    .when(port_num.is_some(), |d| d.child(ui::icon_button("mcp-addr-copy", IconName::Copy, ui::IconBtn::Sm, t("mcp.copy_addr"))
                        .on_click(cx.listener(|this, _, _, cx| this.mcp_copy_addr(cx)))))
                    .when(port_auto && port_num.is_some(), |d| d.child(ui::icon_button("mcp-port-refresh", IconName::RefreshCw, ui::IconBtn::Sm, t("mcp.refresh_port"))
                        .on_click(cx.listener(|this, _, _, cx| this.mcp_refresh_port(cx))))))
                .child(ui::segmented_fill("mcp-port", &port_modes, port_mode, move |v, w, cx| {
                    let _ = me.update(cx, |this, cx| this.mcp_set_port_mode(v == "auto", w, cx));
                }))
                .when(!port_auto, |d| d.child(ui::text_field(&port_in, false)))
                .child(div().id("mcp-limits").h(px(ui::H_COMPACT)).flex().items_center().gap(px(ui::SP_1)).cursor_pointer().rounded(px(ui::R_SM))
                    .hover(|d| d.bg(crate::theme::hover_bg()))
                    .on_click(cx.listener(|this, _, _, cx| { this.mcp.limits_open = !this.mcp.limits_open; cx.notify(); }))
                    .child(ui::icon(if limits_open { IconName::ChevronDown } else { IconName::ChevronRight }, ui::IC_XS, c(th.overlay1)))
                    .child(label(&t("mcp.limits"))))
                .when(limits_open, |d| d.child(div().flex().flex_col().gap(px(ui::SP_2))
                    .child(div().flex().gap(px(ui::SP_2))
                        .child(field(&t("mcp.timeout"), &secs_in))
                        .child(field(&t("mcp.max_rows"), &rows_in)))
                    .child(div().flex().justify_end().child(ui::button("mcp-apply", t("mcp.apply"), 1).on_click(cx.listener(|this, _, _, cx| this.mcp_apply(cx))))))))
            .when(phase_on, |d| d.child(div().mx(px(ui::LIST_INSET)).mb(px(ui::SP_2)).pl(px(ui::ROW_PX)).pr(px(ui::SP_1)).py(px(4.)).rounded(px(ui::R_MD))
                .bg(c(th.info).opacity(0.10)).flex().items_center().gap(px(ui::SP_1))
                .child(div().flex_1().min_w_0().text_size(px(ui::fs_small())).text_color(c(th.info)).child(t("mcp.phase")))
                .child(ui::icon_button("mcp-phase-x", IconName::X, ui::IconBtn::Sm, t("ui.close"))
                    .on_click(cx.listener(|this, _, _, cx| this.mcp_dismiss_phase(cx))))))
            .child(list);
        if let Some((target, pos)) = self.mcp.menu.clone() {
            body = body.child(self.mcp_menu_el(target, pos, cx));
        }
        self.dock_frame("mcp-dock", t("mcp.title").into(), sub, IconName::Waypoints, actions, body, cx)
    }

    fn mcp_menu_el(&mut self, target: McpTarget, pos: Point<Pixels>, cx: &mut Context<Self>) -> AnyElement {
        let (conn, db, table, read_only) = target.parts();
        let is_table = !table.is_empty();
        let is_conn = db.is_empty() && table.is_empty();
        let auto_on = is_table && config::get().db_mcp.grants.iter().any(|g| g.conn == conn && g.schema.is_empty() && g.database.eq_ignore_ascii_case(db) && g.table.eq_ignore_ascii_case(table) && g.auto);
        let mut m = ui::popover().py_1().flex().flex_col();
        for (i, level) in [Level::Hidden, Level::Read, Level::Write, Level::Ddl].into_iter().enumerate() {
            let item = target.clone();
            let enabled = !(read_only && level > Level::Read);
            m = m.child(ui::menu_item(("mcp-lv", i), None, t(level.label_key()), enabled, false).on_click(cx.listener(move |this, _, _, cx| {
                this.mcp.menu = None;
                this.mcp_set_level(&item, level, cx);
            })));
        }
        if !is_conn {
            let item = target.clone();
            m = m.child(ui::menu_sep()).child(ui::menu_item("mcp-inherit", Some(IconName::Undo2), t("mcp.inherit"), true, false).on_click(cx.listener(move |this, _, _, cx| {
                this.mcp.menu = None;
                this.mcp_inherit(&item, cx);
            })));
        }
        if is_table {
            let item = target.clone();
            m = m.child(ui::menu_item("mcp-auto", Some(IconName::Zap), if auto_on { t("mcp.auto_off") } else { t("mcp.auto") }, !read_only, false).on_click(cx.listener(move |this, _, _, cx| {
                this.mcp.menu = None;
                this.mcp_toggle_auto(&item, cx);
            })));
        }
        if target.expandable() {
            let item = target.clone();
            m = m.child(ui::menu_sep()).child(ui::menu_item("mcp-reload", Some(IconName::RefreshCw), t("mcp.reload"), true, false).on_click(cx.listener(move |this, _, _, cx| {
                this.mcp.menu = None;
                this.mcp_reload(&item, cx);
            })));
        }
        deferred(anchored().position(pos).snap_to_window_with_margin(gpui::px(crate::ui::EDGE_MARGIN)).child(m.occlude().on_mouse_down_out(cx.listener(|this, _, _, cx| { this.mcp.menu = None; cx.notify(); })))).with_priority(2).into_any_element()
    }

    pub(crate) fn mcp_answer(&mut self, allow: bool, cx: &mut Context<Self>) {
        if self.mcp.confirms.is_empty() { return; }
        let ask = self.mcp.confirms.remove(0);
        let _ = ask.reply.send(allow);
        cx.notify();
    }

    pub(crate) fn mcp_confirm_el(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let ask = self.mcp.confirms.first()?;
        let left = ask.deadline.saturating_duration_since(std::time::Instant::now()).as_secs();
        let n = self.mcp.confirms.len();
        let sql: String = ask.req.sql.chars().take(4000).collect();
        let who = tf("mcp.confirm_who", &[("client", &ask.req.client), ("conn", &ask.req.conn_name)]);
        let estimate = match ask.req.estimate {
            Some(rows) => tf("mcp.estimate_rows", &[("n", &rows)]),
            None => t("mcp.estimate_unknown").into(),
        };
        let th = cur();
        Some(ui::modal(ui::card(600.)
            .child(ui::dialog_header(Some(IconName::TriangleAlert), c(cur().warning), t("mcp.confirm_title"), None, None)
                .child(div().text_size(px(ui::fs_small())).text_color(c(th.overlay1)).child(tf("mcp.confirm_left", &[("n", &left), ("q", &n)]))))
            .child(div().p(px(ui::SP_5)).flex().flex_col().gap(px(ui::SP_3))
                .child(div().text_size(px(ui::fs_body())).text_color(c(th.subtext)).child(who))
                .child(div().id("mcp-sql").max_h(px(160.)).overflow_y_scroll().p(px(ui::SP_3)).rounded(px(ui::R_MD)).bg(crate::theme::sub_bg()).border_1().border_color(crate::theme::divider())
                    .font_family(ui::mono_family()).text_size(px(ui::fs_mono())).text_color(c(th.text)).child(sql))
                .child(div().text_size(px(ui::fs_small())).text_color(c(th.overlay1)).child(estimate)))
            .child(ui::dialog_footer(None, vec![])
                .child(ui::button("mcp-deny", t("mcp.confirm_deny"), 0).on_click(cx.listener(|this, _, _, cx| this.mcp_answer(false, cx))))
                .child(ui::button("mcp-allow", t("mcp.confirm_allow"), 1).on_click(cx.listener(|this, _, _, cx| this.mcp_answer(true, cx))))))
            .into_any_element())
    }

    pub(crate) fn toggle_mcp_dock(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        if self.dock_mcp { self.close_dock("mcp-dock", _window, cx); return; }
        self.dock_mcp = true;
        if self.mcp.token.is_empty() { self.mcp_sync_token(); }
        self.open_dock_exclusive("mcp-dock");
        cx.notify();
    }
}
