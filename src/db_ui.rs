//! Database module UI (v0.2 PRD §6): live connections (with optional SSH tunnel), the
//! connection tree tool window, and opening table / query tabs.
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use gpui::{div, prelude::*, px, AnyElement, Context, Entity, Pixels, Point, SharedString, Window};
use gpui_component::input::InputState;
use ssh_term_proto::assets::{self, DbConn, TreeRow};
use ssh_term_proto::db::live::Live;
use ssh_term_proto::i18n::{t, tf};

use crate::activity::log_push;
use crate::config;
use crate::db_dialog::{DbDialog, DbDialogEvent};
use crate::db_view::{DbView, Target};
use crate::termview::TermView;
use crate::theme::{c, cur};
use crate::ui::{self, icon, IconName};
use crate::{Node, Overlay, Root, Tab};

/// `Doc`: a MongoDB connection opened from the tree (it has no SQL driver).
pub enum LiveState { Connecting, Ready(Arc<Live>), Doc(Arc<ssh_term_proto::doc::Mongo>), Failed(String) }

pub enum Kids { Loading, Ready(Vec<String>), Failed(String) }

/// Tree keys: `<conn>`, `<conn>\x1f<db>`, `<conn>\x1f<db>\x1f<schema>`; an object group
/// under a node is `<node>\x1e<group id>` (see `db_tree`).
pub(crate) const SEP: char = '\u{1f}';

#[derive(Default)]
pub struct DbPanel {
    pub live: HashMap<String, LiveState>,
    /// Databases of a connection, schemas of a database.
    pub(crate) kids: HashMap<String, Kids>,
    /// Expanded nodes (connections, databases, schemas, object groups).
    pub(crate) open: HashSet<String>,
    /// Databases / schemas opened by the user (their object groups are loaded).
    pub(crate) opened: HashSet<String>,
    /// Members of an object group, by group key.
    pub(crate) members: HashMap<String, Kids>,
    /// Name filter per group key.
    pub(crate) filters: HashMap<String, String>,
    /// Inline editor: (group key, member being renamed or empty for a filter, input).
    pub(crate) edit: Option<(String, Option<String>, Entity<InputState>)>,
    /// Destructive menu entry waiting for a second click.
    pub(crate) arm: Option<String>,
    collapsed_groups: Vec<String>,
    pub(crate) selected: Option<String>,
    search: Option<Entity<InputState>>,
    pub(crate) menu: Option<(String, Point<Pixels>)>,
}

pub(crate) fn key_parts(key: &str) -> (String, Option<String>, Option<String>) {
    let mut it = key.split(SEP);
    (it.next().unwrap_or_default().to_string(), it.next().map(str::to_string), it.next().map(str::to_string))
}

impl Root {
    pub(crate) fn db_conn(id: &str) -> Option<DbConn> { config::get().db.iter().find(|d| d.id == id).cloned() }

    /// Connect (or reuse) `id`. Views waiting on it are updated when it settles.
    pub(crate) fn db_connect(&mut self, id: &str, cx: &mut Context<Self>) {
        if matches!(self.dbp.live.get(id), Some(LiveState::Connecting | LiveState::Ready(_) | LiveState::Doc(_))) { return; }
        let Some(conn) = Self::db_conn(id) else { return };
        if conn.kind == assets::DbKind::Mongodb { self.mongo_connect(conn, cx); return; }
        if !conn.kind.driver_ready() { self.dbp.live.insert(id.into(), LiveState::Failed(tf("db.kind_unsupported", &[("kind", &conn.kind.ui_name())]))); cx.notify(); return; }
        let password = crate::secret_store().get(&conn.password_secret()).map(|x| x.0);
        let via = match Some(conn.ssh_via.as_str()).filter(|s| !s.is_empty()) {
            Some(h) => match self.target_for(h) { Some(t) => Some((t, self.prompts.clone())), None => { self.dbp.live.insert(id.into(), LiveState::Failed(tf("db.via_missing", &[("host", &h)]))); cx.notify(); return; } },
            None => None,
        };
        self.dbp.live.insert(id.into(), LiveState::Connecting);
        let id = id.to_string();
        let name = conn.name.clone();
        let h = self.rt.spawn(async move { ssh_term_proto::db::live::open(&conn, password, via).await });
        cx.spawn(async move |this, cx| {
            let r = h.await.map_err(anyhow::Error::from).and_then(|r| r);
            let _ = this.update(cx, |this, cx| {
                let res = match r {
                    Ok(l) => { let l = Arc::new(l); log_push(&this.exec_log, &name, &t("db.connected"), true, l.version.clone()); this.dbp.live.insert(id.clone(), LiveState::Ready(l.clone())); Ok(l) }
                    Err(e) => { let m = format!("{e:#}"); log_push(&this.exec_log, &name, &t("db.connect_failed"), false, m.clone()); this.dbp.live.insert(id.clone(), LiveState::Failed(m.clone())); Err(m) }
                };
                if res.is_ok() && this.dbp.open.contains(&id) { this.db_load_kids(&id, cx); }
                for v in this.db_views(cx) { if v.read(cx).target.conn == id { let r2 = res.clone(); v.update(cx, |v, cx| v.set_live(r2, cx)); } }
                cx.notify();
            });
        }).detach();
        cx.notify();
    }

    pub(crate) fn db_disconnect(&mut self, id: &str, cx: &mut Context<Self>) {
        match self.dbp.live.remove(id) {
            Some(LiveState::Ready(l)) => { self.rt.spawn(async move { l.close().await; }); }
            Some(LiveState::Doc(m)) => { self.rt.spawn(async move { m.close().await; }); }
            _ => {}
        }
        self.dbp.kids.retain(|k, _| key_parts(k).0 != id);
        self.dbp.open.retain(|k| key_parts(k).0 != id);
        self.dbp.opened.retain(|k| key_parts(k).0 != id);
        self.dbp.members.retain(|k, _| key_parts(k).0 != id);
        for v in self.db_views(cx) { if v.read(cx).target.conn == id { v.update(cx, |v, cx| v.set_live(Err(t("db.disconnected").into()), cx)); } }
        cx.notify();
    }

    /// MongoDB connection for the tree: same states as the SQL ones, `Doc` once open.
    fn mongo_connect(&mut self, conn: DbConn, cx: &mut Context<Self>) {
        let id = conn.id.clone();
        let via = if conn.host.contains("://") { Ok(None) } else { self.via_for(&conn.ssh_via) };
        let via = match via { Ok(v) => v, Err(e) => { self.dbp.live.insert(id, LiveState::Failed(e)); cx.notify(); return; } };
        let password = crate::secret_store().get(&conn.password_secret()).map(|x| x.0);
        self.dbp.live.insert(id.clone(), LiveState::Connecting);
        let name = conn.name.clone();
        let h = self.rt.spawn(async move { ssh_term_proto::doc::Mongo::open(&conn, password, via).await });
        cx.spawn(async move |this, cx| {
            let r = h.await.map_err(anyhow::Error::from).and_then(|r| r);
            let _ = this.update(cx, |this, cx| {
                match r {
                    Ok(m) => { log_push(&this.exec_log, &name, &t("db.connected"), true, m.version.clone()); this.dbp.live.insert(id.clone(), LiveState::Doc(Arc::new(m))); if this.dbp.open.contains(&id) { this.db_load_kids(&id, cx); } }
                    Err(e) => { let m = format!("{e:#}"); log_push(&this.exec_log, &name, &t("db.connect_failed"), false, m.clone()); this.dbp.live.insert(id.clone(), LiveState::Failed(m)); }
                }
                cx.notify();
            });
        }).detach();
        cx.notify();
    }

    pub(crate) fn db_mongo(&self, id: &str) -> Option<Arc<ssh_term_proto::doc::Mongo>> { match self.dbp.live.get(id) { Some(LiveState::Doc(m)) => Some(m.clone()), _ => None } }
    /// Connected (SQL driver or MongoDB).
    pub(crate) fn db_up(&self, id: &str) -> bool { matches!(self.dbp.live.get(id), Some(LiveState::Ready(_) | LiveState::Doc(_))) }

    pub(crate) fn db_ready(&self, id: &str) -> Option<Arc<Live>> { match self.dbp.live.get(id) { Some(LiveState::Ready(l)) => Some(l.clone()), _ => None } }

    /// All database panes in all tabs.
    pub(crate) fn db_views(&self, cx: &gpui::App) -> Vec<Entity<DbView>> {
        self.tabs.iter().flat_map(|t| t.layout.leaves()).filter_map(|p| p.read(cx).db.clone()).collect()
    }

    /// Open a table browser or a query tab for `target` (connects when needed).
    pub(crate) fn open_db_tab(&mut self, target: Target, sql: String, window: &mut Window, cx: &mut Context<Self>) {
        // a table already open → focus it
        if target.table.is_some() {
            for (ti, tab) in self.tabs.iter().enumerate() {
                if let Some(p) = tab.layout.leaves().into_iter().find(|p| p.read(cx).db.as_ref().is_some_and(|v| v.read(cx).target == target)) {
                    self.active = ti;
                    self.tabs[ti].focused = p.clone();
                    self.leave_settings();
                    let fh = p.read(cx).focus.clone();
                    window.focus(&fh, cx);
                    cx.notify();
                    return;
                }
            }
        }
        let pane = self.db_pane(target, sql, window, cx);
        let fh = pane.read(cx).focus.clone();
        self.tabs.push(Tab { layout: Node::Leaf(pane.clone()), focused: pane, name: None });
        self.active = self.tabs.len() - 1;
        self.leave_settings();
        self.overlay = Overlay::None;
        window.focus(&fh, cx);
        cx.notify();
    }

    /// A layout leaf hosting a database view (new tab or layout restore).
    pub(crate) fn db_pane(&mut self, target: Target, sql: String, window: &mut Window, cx: &mut Context<Self>) -> Entity<TermView> {
        let conn = target.conn.clone();
        let (rt, log) = (self.rt.handle().clone(), self.exec_log.clone());
        let v = cx.new(|cx| DbView::new(target, sql, rt, log, window, cx));
        if matches!(self.dbp.live.get(&conn), Some(LiveState::Failed(_))) { self.dbp.live.remove(&conn); }
        match self.db_ready(&conn) {
            Some(l) => v.update(cx, |v, cx| v.set_live(Ok(l), cx)),
            None => self.db_connect(&conn, cx),
        }
        let (h, m) = (self.history_for("local"), self.manifests.clone());
        cx.new(|cx| TermView::new_db(v, h, m, window, cx))
    }

    /// Query tab for the selected tree node (its connection / database / schema).
    pub(crate) fn db_new_query(&mut self, key: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        let key = key.or_else(|| self.dbp.selected.clone()).or_else(|| config::get().db.first().map(|d| d.id.clone()));
        let Some(key) = key else { self.open_db_dialog(None, window, cx); return };
        let (id, db, schema) = key_parts(&key);
        if Self::db_conn(&id).is_some_and(|c| c.kind == ssh_term_proto::assets::DbKind::Mongodb) { self.open_mongo_tab(&id, window, cx); return; }
        let db = db.or_else(|| Self::db_conn(&id).map(|c| c.database).filter(|d| !d.is_empty())).unwrap_or_default();
        let db = if db.is_empty() { Self::db_conn(&id).filter(|c| c.kind.is_file()).map(|_| "main".to_string()).unwrap_or_default() } else { db };
        self.open_db_tab(Target { conn: id, database: db, schema, table: None }, String::new(), window, cx);
    }

    pub(crate) fn open_db_dialog(&mut self, id: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        let existing = id.as_deref().and_then(Self::db_conn);
        let group = if existing.is_none() { self.dbp.selected.as_deref().and_then(|k| k.strip_prefix("g:")).map(str::to_string) } else { None };
        let hosts: Vec<String> = config::get().all_hosts().iter().map(|h| h.name.clone()).collect();
        let (rt, prompts) = (self.rt.handle().clone(), self.prompts.clone());
        let targets: HashMap<String, ssh_term_proto::ssh::SshTarget> = self.host_targets().into_iter().map(|(h, t)| (h.name, t)).collect();
        let d = cx.new(|cx| DbDialog::new(existing, group, hosts, targets, prompts, rt, window, cx));
        self._subs.push(cx.subscribe_in(&d, window, |this, _, ev: &DbDialogEvent, window, cx| {
            match ev {
                DbDialogEvent::Saved(id) => {
                    // settings may have changed: drop the old connection so the next use reconnects
                    this.db_disconnect(id, cx);
                    this.log("db.save", id);
                    this.close_overlay(window, cx);
                }
                DbDialogEvent::Deleted(id) => { this.db_disconnect(id, cx); this.log("db.delete", id); this.close_overlay(window, cx); }
                DbDialogEvent::Close => this.close_overlay(window, cx),
            }
        }));
        self.overlay = Overlay::Db(d);
        cx.notify();
    }

    pub(crate) fn db_panel(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let th = cur();
        if self.dbp.search.is_none() {
            let s = cx.new(|cx| InputState::new(window, cx).placeholder(t("db.search")));
            self._subs.push(cx.observe(&s, |_, _, cx| cx.notify()));
            self.dbp.search = Some(s);
        }
        let search = self.dbp.search.clone().unwrap();
        let q = search.read(cx).value().trim().to_lowercase();
        let s = config::get();
        let dbs = s.db.clone();

        let actions = vec![
            ui::icon_button("db-add", IconName::Plus, ui::IconBtn::Md, t("db.new_connection")).on_click(cx.listener(|this, _, w, cx| this.open_db_dialog(None, w, cx))).into_any_element(),
            ui::icon_button("db-query", IconName::SquareTerminal, ui::IconBtn::Md, t("db.new_query")).on_click(cx.listener(|this, _, w, cx| this.db_new_query(None, w, cx))).into_any_element(),
            ui::icon_button("db-collapse", IconName::ChevronsDownUp, ui::IconBtn::Md, t("db.collapse_all")).on_click(cx.listener(|this, _, _w, cx| { this.dbp.open.clear(); cx.notify(); })).into_any_element(),
        ];
        let mut col = div().flex_1().min_h_0().flex().flex_col()
            .child(ui::panel_header(t("tool.database"), None, None, actions))
            .child(ui::field_row().pb(px(ui::SP_2)).child(ui::search_field(&search, false)));

        if dbs.is_empty() {
            col = col.child(ui::empty_state(IconName::Database, t("db.empty"), None, vec![
                ui::button("db-empty-add", t("db.new_connection"), 1).w_full().on_click(cx.listener(|this, _, w, cx| this.open_db_dialog(None, w, cx))).into_any_element(),
            ], false));
            return col.into_any_element();
        }

        let item_groups: Vec<String> = dbs.iter().map(|d| d.group_path()).collect();
        let keep = |i: usize| q.is_empty() || { let d = &dbs[i]; d.name.to_lowercase().contains(&q) || d.host.to_lowercase().contains(&q) || d.database.to_lowercase().contains(&q) };
        let rows = assets::tree_rows(&s.groups, &self.dbp.collapsed_groups, &item_groups, &keep, false);

        let mut list = div().id("db-tree").flex_1().min_h_0().overflow_y_scroll().pb(px(ui::SP_2)).flex().flex_col();
        use crate::db_tree::{chev as chevron, row as row_base};
        for r in rows {
            match r {
                TreeRow::Group { path, depth, count } => {
                    let open = !self.dbp.collapsed_groups.contains(&path);
                    let name = path.rsplit('/').next().unwrap_or(&path).to_string();
                    let p2 = path.clone();
                    list = list.child(row_base(SharedString::from(format!("dbg:{path}")), depth, false)
                        .on_click(cx.listener(move |this, _, _w, cx| { let g = &mut this.dbp.collapsed_groups; if let Some(i) = g.iter().position(|x| *x == p2) { g.remove(i); } else { g.push(p2.clone()); } cx.notify(); }))
                        .child(chevron(open, true)).child(icon(if open { IconName::FolderOpen } else { IconName::Folder }, ui::IC_MD, c(th.overlay1)))
                        .child(ui::row_title(name).font_weight(gpui::FontWeight::MEDIUM))
                        .child(div().flex_1())
                        .child(div().text_size(px(ui::fs_small())).text_color(c(th.subtext)).child(count.to_string())));
                }
                TreeRow::Item { index, depth } => {
                    let d = &dbs[index];
                    list = self.db_conn_rows(list, d, depth, cx);
                }
            }
        }
        col = col.child(list);
        if let Some((k, pos)) = self.dbp.menu.clone() { col = col.child(self.db_menu_el(k, pos, cx)); }
        col.into_any_element()
    }

}
