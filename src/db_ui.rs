//! Database module UI (v0.2 PRD §6): live connections (with optional SSH tunnel), the
//! connection tree tool window, and opening table / query tabs.
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use gpui::{anchored, deferred, div, prelude::*, px, AnyElement, Context, Entity, MouseButton, Pixels, Point, SharedString, Window};
use gpui_component::input::InputState;
use ssh_term_proto::assets::{self, DbConn, TreeRow};
use ssh_term_proto::db::{live::Live, ObjKind};
use ssh_term_proto::i18n::{t, tf};

use crate::activity::{log_push, panel_title};
use crate::config;
use crate::db_dialog::{DbDialog, DbDialogEvent};
use crate::db_view::{DbView, Target};
use crate::termview::TermView;
use crate::theme::{c, cur};
use crate::ui::{self, icon, IconName};
use crate::{Node, Overlay, Root, Tab};

pub enum LiveState { Connecting, Ready(Arc<Live>), Failed(String) }

#[derive(Clone, Copy, PartialEq)]
pub enum Kid { Database, Schema, Table, View }

pub enum Kids { Loading, Ready(Vec<(String, Kid)>), Failed(String) }

/// Tree keys: `<conn>`, `<conn>\x1f<db>`, `<conn>\x1f<db>\x1f<schema>`.
const SEP: char = '\u{1f}';

#[derive(Default)]
pub struct DbPanel {
    pub live: HashMap<String, LiveState>,
    kids: HashMap<String, Kids>,
    open: HashSet<String>,
    collapsed_groups: Vec<String>,
    selected: Option<String>,
    search: Option<Entity<InputState>>,
    menu: Option<(String, Point<Pixels>)>,
}

fn key_parts(key: &str) -> (String, Option<String>, Option<String>) {
    let mut it = key.split(SEP);
    (it.next().unwrap_or_default().to_string(), it.next().map(str::to_string), it.next().map(str::to_string))
}

impl Root {
    fn db_conn(id: &str) -> Option<DbConn> { config::get().db.iter().find(|d| d.id == id).cloned() }

    /// Connect (or reuse) `id`. Views waiting on it are updated when it settles.
    pub(crate) fn db_connect(&mut self, id: &str, cx: &mut Context<Self>) {
        if matches!(self.dbp.live.get(id), Some(LiveState::Connecting | LiveState::Ready(_))) { return; }
        let Some(conn) = Self::db_conn(id) else { return };
        if !conn.kind.driver_ready() { self.dbp.live.insert(id.into(), LiveState::Failed(tf("db.kind_unsupported", &[("kind", &conn.kind.label())]))); cx.notify(); return; }
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
        if let Some(LiveState::Ready(l)) = self.dbp.live.remove(id) { self.rt.spawn(async move { l.close().await; }); }
        self.dbp.kids.retain(|k, _| key_parts(k).0 != id);
        self.dbp.open.retain(|k| key_parts(k).0 != id);
        for v in self.db_views(cx) { if v.read(cx).target.conn == id { v.update(cx, |v, cx| v.set_live(Err(t("db.disconnected").into()), cx)); } }
        cx.notify();
    }

    fn db_ready(&self, id: &str) -> Option<Arc<Live>> { match self.dbp.live.get(id) { Some(LiveState::Ready(l)) => Some(l.clone()), _ => None } }

    /// All database panes in all tabs.
    pub(crate) fn db_views(&self, cx: &gpui::App) -> Vec<Entity<DbView>> {
        self.tabs.iter().flat_map(|t| t.layout.leaves()).filter_map(|p| p.read(cx).db.clone()).collect()
    }

    /// Fetch children of a tree node.
    fn db_load_kids(&mut self, key: &str, cx: &mut Context<Self>) {
        let (id, db, schema) = key_parts(key);
        let Some(live) = self.db_ready(&id) else { return };
        let has_schemas = live.driver.kind().has_schemas();
        self.dbp.kids.insert(key.into(), Kids::Loading);
        let key = key.to_string();
        let h = self.rt.spawn(async move {
            let d = &live.driver;
            Ok::<_, anyhow::Error>(match (db, schema) {
                (None, _) => d.databases().await?.into_iter().map(|n| (n, Kid::Database)).collect::<Vec<_>>(),
                (Some(db), None) if has_schemas => d.schemas(&db).await?.into_iter().map(|n| (n, Kid::Schema)).collect(),
                (Some(db), s) => {
                    let mut v: Vec<(String, Kid)> = d.tables(&db, s.as_deref()).await?.into_iter().map(|ti| (ti.name, if ti.kind == ObjKind::View { Kid::View } else { Kid::Table })).collect();
                    v.sort_by(|a, b| (a.1 == Kid::View).cmp(&(b.1 == Kid::View)).then_with(|| a.0.to_lowercase().cmp(&b.0.to_lowercase())));
                    v
                }
            })
        });
        cx.spawn(async move |this, cx| {
            let r = h.await.map_err(anyhow::Error::from).and_then(|r| r);
            let _ = this.update(cx, |this, cx| { this.dbp.kids.insert(key, match r { Ok(v) => Kids::Ready(v), Err(e) => Kids::Failed(format!("{e:#}")) }); cx.notify(); });
        }).detach();
        cx.notify();
    }

    fn db_toggle(&mut self, key: &str, cx: &mut Context<Self>) {
        self.dbp.selected = Some(key.into());
        if !self.dbp.open.remove(key) {
            self.dbp.open.insert(key.into());
            let (id, ..) = key_parts(key);
            if self.db_ready(&id).is_none() { self.db_connect(&id, cx); }
            else if !matches!(self.dbp.kids.get(key), Some(Kids::Ready(_) | Kids::Loading)) { self.db_load_kids(key, cx); }
        }
        cx.notify();
    }

    fn db_refresh(&mut self, key: &str, cx: &mut Context<Self>) {
        self.dbp.kids.retain(|k, _| k != key && !k.starts_with(&format!("{key}{SEP}")));
        self.dbp.open.insert(key.into());
        let (id, ..) = key_parts(key);
        if matches!(self.dbp.live.get(&id), Some(LiveState::Failed(_))) { self.dbp.live.remove(&id); }
        if self.db_ready(&id).is_none() { self.db_connect(&id, cx); } else { self.db_load_kids(key, cx); }
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
    fn db_new_query(&mut self, key: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        let key = key.or_else(|| self.dbp.selected.clone()).or_else(|| config::get().db.first().map(|d| d.id.clone()));
        let Some(key) = key else { self.open_db_dialog(None, window, cx); return };
        let (id, db, schema) = key_parts(&key);
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

    fn db_menu_el(&mut self, key: String, pos: Point<Pixels>, cx: &mut Context<Self>) -> AnyElement {
        let (id, db, schema) = key_parts(&key);
        let is_conn = db.is_none() && !key.starts_with("g:");
        let connected = self.db_ready(&id).is_some();
        let mut m = ui::popover().w(px(200.)).py_1().flex().flex_col();
        let k = key.clone();
        m = m.child(ui::menu_item("dbm-query", Some(IconName::SquareTerminal), t("db.new_query"), true, false).on_click(cx.listener(move |this, _, w, cx| { this.dbp.menu = None; this.db_new_query(Some(k.clone()), w, cx); })));
        let k = key.clone();
        m = m.child(ui::menu_item("dbm-refresh", Some(IconName::RefreshCw), t("db.refresh"), true, false).on_click(cx.listener(move |this, _, _w, cx| { this.dbp.menu = None; this.db_refresh(&k, cx); })));
        if is_conn {
            m = m.child(ui::menu_sep());
            let i2 = id.clone();
            m = m.child(if connected { ui::menu_item("dbm-dc", Some(IconName::Unplug), t("db.disconnect"), true, false).on_click(cx.listener(move |this, _, _w, cx| { this.dbp.menu = None; this.db_disconnect(&i2, cx); })) }
                else { ui::menu_item("dbm-c", Some(IconName::Plug), t("db.connect"), true, false).on_click(cx.listener(move |this, _, _w, cx| { this.dbp.menu = None; this.dbp.live.remove(&i2); this.dbp.open.insert(i2.clone()); this.db_connect(&i2, cx); })) });
            let i2 = id.clone();
            m = m.child(ui::menu_item("dbm-edit", Some(IconName::Pencil), t("db.edit"), true, false).on_click(cx.listener(move |this, _, w, cx| { this.dbp.menu = None; this.open_db_dialog(Some(i2.clone()), w, cx); })));
        }
        let _ = schema;
        deferred(anchored().position(pos).child(m.occlude().on_mouse_down_out(cx.listener(|this, _, _w, cx| { this.dbp.menu = None; cx.notify(); })))).with_priority(2).into_any_element()
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

        let act = |id: &'static str, ic: IconName| div().id(id).size(px(22.)).rounded(px(5.)).flex().items_center().justify_center().cursor_pointer().hover(|d| d.bg(c(cur().surface0))).child(icon(ic, 13., c(cur().overlay1)));
        let actions = div().flex().items_center().gap(px(2.))
            .child(act("db-add", IconName::Plus).on_click(cx.listener(|this, _, w, cx| this.open_db_dialog(None, w, cx))))
            .child(act("db-query", IconName::SquareTerminal).on_click(cx.listener(|this, _, w, cx| this.db_new_query(None, w, cx))))
            .child(act("db-collapse", IconName::ChevronsDownUp).on_click(cx.listener(|this, _, _w, cx| { this.dbp.open.clear(); cx.notify(); })))
            .into_any_element();
        let mut col = div().flex_1().min_h_0().flex().flex_col()
            .child(panel_title(t("tool.database"), Some(actions)))
            .child(ui::field_row().pb_2().child(ui::search_field(&search, false)));

        if dbs.is_empty() {
            col = col.child(div().p_3().flex().flex_col().gap_3().text_size(px(12.)).text_color(c(th.subtext)).child(t("db.empty"))
                .child(ui::button("db-empty-add", t("db.new_connection"), 1).on_click(cx.listener(|this, _, w, cx| this.open_db_dialog(None, w, cx)))));
            return col.into_any_element();
        }

        let item_groups: Vec<String> = dbs.iter().map(|d| d.group_path()).collect();
        let keep = |i: usize| q.is_empty() || { let d = &dbs[i]; d.name.to_lowercase().contains(&q) || d.host.to_lowercase().contains(&q) || d.database.to_lowercase().contains(&q) };
        let rows = assets::tree_rows(&s.groups, &self.dbp.collapsed_groups, &item_groups, &keep, false);

        let mut list = div().id("db-tree").flex_1().min_h_0().overflow_y_scroll().pb_2().flex().flex_col();
        let indent = |d: usize| px(8. + d as f32 * 14.);
        let row_base = |id: SharedString, depth: usize, selected: bool| div().id(id).h(px(26.)).pl(indent(depth)).pr_2().mx_1().rounded(px(6.)).flex().items_center().gap_1().cursor_pointer().flex_shrink_0()
            .text_size(px(12.)).bg(if selected { c(cur().surface0) } else { gpui::transparent_black() }).hover(|d| d.bg(c(cur().surface0).opacity(0.7)));
        let chevron = |open: bool, show: bool| div().w(px(12.)).flex_shrink_0().when(show, |d| d.child(icon(if open { IconName::ChevronDown } else { IconName::ChevronRight }, 11., c(cur().overlay0))));

        for r in rows {
            match r {
                TreeRow::Group { path, depth, count } => {
                    let open = !self.dbp.collapsed_groups.contains(&path);
                    let name = path.rsplit('/').next().unwrap_or(&path).to_string();
                    let p2 = path.clone();
                    list = list.child(row_base(SharedString::from(format!("dbg:{path}")), depth, false)
                        .on_click(cx.listener(move |this, _, _w, cx| { let g = &mut this.dbp.collapsed_groups; if let Some(i) = g.iter().position(|x| *x == p2) { g.remove(i); } else { g.push(p2.clone()); } cx.notify(); }))
                        .child(chevron(open, true)).child(icon(IconName::Folder, 13., c(th.overlay1)))
                        .child(div().flex_1().min_w_0().whitespace_nowrap().overflow_hidden().text_ellipsis().text_color(c(th.subtext)).child(name))
                        .child(div().text_size(px(11.)).text_color(c(th.overlay0)).child(count.to_string())));
                }
                TreeRow::Item { index, depth } => {
                    let d = &dbs[index];
                    list = self.db_conn_rows(list, d, depth, &row_base, &chevron, cx);
                }
            }
        }
        col = col.child(list);
        if let Some((k, pos)) = self.dbp.menu.clone() { col = col.child(self.db_menu_el(k, pos, cx)); }
        col.into_any_element()
    }

    #[allow(clippy::too_many_arguments)]
    fn db_conn_rows<R, C>(&mut self, mut list: gpui::Stateful<gpui::Div>, d: &DbConn, depth: usize, row_base: &R, chevron: &C, cx: &mut Context<Self>) -> gpui::Stateful<gpui::Div>
    where R: Fn(SharedString, usize, bool) -> gpui::Stateful<gpui::Div>, C: Fn(bool, bool) -> gpui::Div {
        let th = cur();
        let id = d.id.clone();
        let open = self.dbp.open.contains(&id);
        let (dot, status_tip) = match self.dbp.live.get(&id) {
            Some(LiveState::Ready(_)) => (Some(c(th.success)), None),
            Some(LiveState::Connecting) => (Some(c(th.warning)), None),
            Some(LiveState::Failed(e)) => (Some(c(th.error)), Some(e.clone())),
            None => (None, None),
        };
        let sel = self.dbp.selected.as_deref() == Some(id.as_str());
        let k = id.clone();
        let k2 = id.clone();
        let mut row = row_base(SharedString::from(format!("dbc:{id}")), depth, sel)
            .on_click(cx.listener(move |this, _, _w, cx| this.db_toggle(&k, cx)))
            .on_mouse_down(MouseButton::Right, cx.listener(move |this, e: &gpui::MouseDownEvent, _w, cx| { this.dbp.selected = Some(k2.clone()); this.dbp.menu = Some((k2.clone(), e.position)); cx.notify(); }))
            .child(chevron(open, true))
            .child(icon(IconName::Database, 13., c(if d.read_only { th.warning } else { th.accent })))
            .child(div().flex_1().min_w_0().whitespace_nowrap().overflow_hidden().text_ellipsis().text_color(c(th.text)).child(d.name.clone()))
            .child(div().text_size(px(10.)).text_color(c(th.overlay0)).child(d.kind.label()));
        if !d.ssh_via.is_empty() { row = row.child(icon(IconName::Waypoints, 11., c(th.overlay0))); }
        if let Some(col) = dot { row = row.child(div().size(px(6.)).rounded_full().bg(col)); }
        list = list.child(row);
        if !open { return list; }
        if let Some(e) = status_tip { return list.child(err_row(&e, depth + 1)); }
        if matches!(self.dbp.live.get(&id), Some(LiveState::Connecting)) { return list.child(note_row(t("db.connecting"), depth + 1)); }
        self.db_kid_rows(list, &id, depth + 1, row_base, chevron, cx)
    }

    fn db_kid_rows<R, C>(&mut self, mut list: gpui::Stateful<gpui::Div>, key: &str, depth: usize, row_base: &R, chevron: &C, cx: &mut Context<Self>) -> gpui::Stateful<gpui::Div>
    where R: Fn(SharedString, usize, bool) -> gpui::Stateful<gpui::Div>, C: Fn(bool, bool) -> gpui::Div {
        let th = cur();
        let kids = match self.dbp.kids.get(key) {
            None | Some(Kids::Loading) => return list.child(note_row(t("db.loading"), depth)),
            Some(Kids::Failed(e)) => return list.child(err_row(e, depth)),
            Some(Kids::Ready(v)) => v.clone(),
        };
        if kids.is_empty() { return list.child(note_row(t("db.no_objects"), depth)); }
        let (id, db, schema) = key_parts(key);
        for (name, kind) in kids {
            let child_key = format!("{key}{SEP}{name}");
            let sel = self.dbp.selected.as_deref() == Some(child_key.as_str());
            let (ic, col) = match kind { Kid::Database => (IconName::Database, th.overlay1), Kid::Schema => (IconName::FolderTree, th.overlay1), Kid::Table => (IconName::Table, th.accent), Kid::View => (IconName::Eye, th.warning) };
            let expandable = matches!(kind, Kid::Database | Kid::Schema);
            let open = expandable && self.dbp.open.contains(&child_key);
            let mut row = row_base(SharedString::from(format!("dbk:{child_key}")), depth, sel).child(chevron(open, expandable)).child(icon(ic, 13., c(col)))
                .child(div().flex_1().min_w_0().whitespace_nowrap().overflow_hidden().text_ellipsis().text_color(c(th.text)).child(name.clone()));
            let ck = child_key.clone();
            row = row.on_mouse_down(MouseButton::Right, cx.listener(move |this, e: &gpui::MouseDownEvent, _w, cx| { this.dbp.selected = Some(ck.clone()); this.dbp.menu = Some((ck.clone(), e.position)); cx.notify(); }));
            if expandable {
                let ck = child_key.clone();
                row = row.on_click(cx.listener(move |this, _, _w, cx| this.db_toggle(&ck, cx)));
            } else {
                let ck = child_key.clone();
                let target = Target { conn: id.clone(), database: db.clone().unwrap_or_default(), schema: schema.clone(), table: Some(name.clone()) };
                row = row.on_click(cx.listener(move |this, e: &gpui::ClickEvent, w, cx| {
                    this.dbp.selected = Some(ck.clone());
                    if e.click_count() >= 2 { this.open_db_tab(target.clone(), String::new(), w, cx); }
                    cx.notify();
                }));
            }
            list = list.child(row);
            if open { list = self.db_kid_rows(list, &child_key, depth + 1, row_base, chevron, cx); }
        }
        list
    }
}

fn note_row(s: &str, depth: usize) -> gpui::Div {
    div().h(px(24.)).pl(px(8. + depth as f32 * 14. + 14.)).flex().items_center().text_size(px(11.)).text_color(c(cur().overlay0)).child(s.to_string())
}
fn err_row(s: &str, depth: usize) -> gpui::Div {
    div().pl(px(8. + depth as f32 * 14. + 14.)).pr_2().py_1().text_size(px(11.)).text_color(c(cur().error)).child(s.chars().take(300).collect::<String>())
}

