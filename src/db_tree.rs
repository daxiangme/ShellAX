//! Connection tree of the database tool window: connections and databases / schemas are
//! opened and closed explicitly (nothing connects on its own), an open database shows its
//! object groups per engine (lazy, counted, filterable), plus saved queries and a backup entry.
use std::path::PathBuf;

use gpui::{anchored, deferred, div, prelude::*, px, AnyElement, ClickEvent, Context, MouseButton, MouseDownEvent, Pixels, Point, SharedString, Window};
use gpui_component::input::{InputEvent, InputState};
use ssh_term_proto::assets::{DbConn, DbKind};
use ssh_term_proto::db::{dialect, objtree::{self, Group}, saved};
use ssh_term_proto::i18n::{t, tf};

use crate::db_ui::{key_parts, Kids, LiveState, SEP};
use crate::db_view::{Intent, Target};
use crate::theme::{c, cur};
use crate::ui::{self, icon, IconName};
use crate::Root;

/// Separates a node key from an object-group id.
const GSEP: char = '\u{1e}';
/// 树每级缩进（行左内边距 + 层级 × INDENT）。
fn indent(depth: usize) -> Pixels { px(ui::ROW_PX + depth as f32 * ui::INDENT) }

/// 树的一行：共用列表行（悬停淡底、选中淡强调底），正文字号与行高取 token。
pub fn row(id: SharedString, depth: usize, selected: bool) -> gpui::Stateful<gpui::Div> {
    ui::list_row(id, depth, selected).flex_shrink_0().text_size(px(ui::fs_body())).line_height(px(ui::lh(ui::fs_body())))
}
/// 展开箭头槽位：固定 INDENT 宽，叶子行用空槽位对齐。
pub fn chev(open: bool, show: bool) -> gpui::Div {
    div().w(px(ui::INDENT)).flex_shrink_0().when(show, |d| d.child(icon(if open { IconName::ChevronDown } else { IconName::ChevronRight }, ui::IC_XS, c(cur().overlay1))))
}
fn label(s: impl Into<SharedString>, col: gpui::Hsla) -> gpui::Div {
    div().flex_1().min_w_0().whitespace_nowrap().overflow_hidden().text_ellipsis().text_color(col).child(s.into())
}
/// 树中的说明行（加载中、无对象等）：次要字号 + 次文本色。
fn note_row(s: &str, depth: usize) -> gpui::Div {
    div().min_h(px(ui::H_COMPACT)).pl(indent(depth) + px(ui::INDENT)).flex().items_center().text_size(px(ui::fs_small())).text_color(c(cur().subtext)).child(s.to_string())
}
fn err_row(s: &str, depth: usize) -> gpui::Div {
    div().pl(indent(depth) + px(ui::INDENT)).pr_2().py_1().text_size(px(ui::fs_small())).text_color(c(cur().error)).child(s.chars().take(300).collect::<String>())
}

pub fn group_key(node: &str, g: Group) -> String { format!("{node}{GSEP}{}", g.id()) }
/// (node key, group) of a group key.
pub fn split_group(k: &str) -> Option<(&str, Group)> { let (n, g) = k.split_once(GSEP)?; Some((n, Group::from_id(g)?)) }

pub fn group_icon(g: Group) -> IconName {
    match g {
        Group::Tables | Group::Collections => IconName::Table, Group::Views => IconName::Eye, Group::MatViews => IconName::Layers,
        Group::Functions => IconName::SquareFunction, Group::Procedures => IconName::Workflow, Group::Events => IconName::CalendarClock,
        Group::Triggers => IconName::Zap, Group::Sequences => IconName::ListOrdered, Group::Synonyms => IconName::Link,
        Group::Dictionaries => IconName::BookOpen, Group::GridFs => IconName::Boxes, Group::Indexes => IconName::KeyRound,
        Group::Queries => IconName::FileCode, Group::Backups => IconName::Archive,
    }
}

/// Saved-query directory for a node key (connection / database / schema).
pub fn saved_dir(node: &str) -> PathBuf {
    let (id, db, schema) = key_parts(node);
    let scope: Vec<&str> = db.iter().chain(schema.iter()).map(String::as_str).collect();
    saved::dir(&saved::root(), &id, &scope)
}

impl Root {
    fn db_kind(id: &str) -> Option<DbKind> { Self::db_conn(id).map(|c| c.kind) }

    /// Does `node` (a database or schema key) carry object groups directly?
    fn holds_groups(node: &str) -> bool {
        let (id, db, schema) = key_parts(node);
        let Some(k) = Self::db_kind(&id) else { return false };
        db.is_some() && (schema.is_some() || !k.has_schemas())
    }

    /// Fetch databases of a connection or schemas of a database.
    pub(crate) fn db_load_kids(&mut self, key: &str, cx: &mut Context<Self>) {
        let (id, db, _) = key_parts(key);
        if let Some(m) = self.db_mongo(&id) {
            self.dbp.kids.insert(key.into(), Kids::Loading);
            let key = key.to_string();
            let h = self.rt.spawn(async move { m.databases().await });
            cx.spawn(async move |this, cx| {
                let r = h.await.map_err(anyhow::Error::from).and_then(|r| r);
                let _ = this.update(cx, |this, cx| { this.dbp.kids.insert(key, match r { Ok(v) => Kids::Ready(v), Err(e) => Kids::Failed(format!("{e:#}")) }); cx.notify(); });
            }).detach();
            cx.notify();
            return;
        }
        let Some(live) = self.db_ready(&id) else { return };
        self.dbp.kids.insert(key.into(), Kids::Loading);
        let key = key.to_string();
        let h = self.rt.spawn(async move {
            match db { None => live.driver.databases().await, Some(db) => live.driver.schemas(&db).await }
        });
        cx.spawn(async move |this, cx| {
            let r = h.await.map_err(anyhow::Error::from).and_then(|r| r);
            let _ = this.update(cx, |this, cx| { this.dbp.kids.insert(key, match r { Ok(v) => Kids::Ready(v), Err(e) => Kids::Failed(format!("{e:#}")) }); cx.notify(); });
        }).detach();
        cx.notify();
    }

    /// Load the catalog members of one object group.
    fn db_load_group(&mut self, gk: &str, cx: &mut Context<Self>) {
        let Some((node, g)) = split_group(gk) else { return };
        if !g.from_catalog() { return; }
        let (id, db, schema) = key_parts(node);
        if let Some(m) = self.db_mongo(&id) {
            let Some(db) = db else { return };
            self.dbp.members.insert(gk.into(), Kids::Loading);
            let gk = gk.to_string();
            let h = self.rt.spawn(async move { m.tree_group(&db, g.id()).await });
            cx.spawn(async move |this, cx| {
                let r = h.await.map_err(anyhow::Error::from).and_then(|r| r);
                let _ = this.update(cx, |this, cx| { this.dbp.members.insert(gk, match r { Ok(v) => Kids::Ready(v), Err(e) => Kids::Failed(format!("{e:#}")) }); cx.notify(); });
            }).detach();
            return;
        }
        let Some(live) = self.db_ready(&id) else { return };
        let db = db.unwrap_or_default();
        let Some(sql) = objtree::list_sql(live.driver.family(), g, &db, schema.as_deref()) else { self.dbp.members.insert(gk.into(), Kids::Ready(vec![])); return };
        self.dbp.members.insert(gk.into(), Kids::Loading);
        let gk = gk.to_string();
        let h = self.rt.spawn(async move {
            let rs = live.driver.execute(&db, &sql, true).await?;
            Ok::<_, anyhow::Error>(rs.first().map(|r| (0..r.rows.len()).filter_map(|i| r.cell(i, 0).map(str::to_string)).collect()).unwrap_or_default())
        });
        cx.spawn(async move |this, cx| {
            let r = h.await.map_err(anyhow::Error::from).and_then(|r| r);
            let _ = this.update(cx, |this, cx| { this.dbp.members.insert(gk, match r { Ok(v) => Kids::Ready(v), Err(e) => Kids::Failed(format!("{e:#}")) }); cx.notify(); });
        }).detach();
    }

    /// Open a connection (connects; the databases load when it is ready).
    pub(crate) fn db_open_conn(&mut self, id: &str, cx: &mut Context<Self>) {
        if matches!(self.dbp.live.get(id), Some(LiveState::Failed(_))) { self.dbp.live.remove(id); }
        self.dbp.open.insert(id.into());
        if self.db_up(id) { if !self.dbp.kids.contains_key(id) { self.db_load_kids(id, cx); } } else { self.db_connect(id, cx); }
        cx.notify();
    }

    /// Open a database / schema: marks it open, expands it and loads its schemas or object groups.
    pub(crate) fn db_open_node(&mut self, key: &str, cx: &mut Context<Self>) {
        self.dbp.opened.insert(key.into());
        self.dbp.open.insert(key.into());
        let (id, ..) = key_parts(key);
        let Some(k) = Self::db_kind(&id) else { return };
        if Self::holds_groups(key) {
            for g in objtree::groups(k) { let gk = group_key(key, *g); if !self.dbp.members.contains_key(&gk) { self.db_load_group(&gk, cx); } }
        } else if !self.dbp.kids.contains_key(key) { self.db_load_kids(key, cx); }
        cx.notify();
    }

    /// Close a database / schema: drops everything loaded below it and collapses it.
    pub(crate) fn db_close_node(&mut self, key: &str, cx: &mut Context<Self>) {
        let pre = format!("{key}{SEP}");
        let gpre = format!("{key}{GSEP}");
        let under = |k: &str| k == key || k.starts_with(&pre) || k.starts_with(&gpre);
        self.dbp.opened.retain(|k| !under(k));
        self.dbp.open.retain(|k| !under(k));
        self.dbp.members.retain(|k, _| !under(k));
        self.dbp.kids.retain(|k, _| !(k.starts_with(&pre) || (k == key && key_parts(key).1.is_some())));
        cx.notify();
    }

    fn db_toggle_expand(&mut self, key: &str, cx: &mut Context<Self>) {
        if !self.dbp.open.remove(key) {
            self.dbp.open.insert(key.into());
            if split_group(key).is_some() && !self.dbp.members.contains_key(key) { self.db_load_group(key, cx); }
        }
        cx.notify();
    }

    /// Refresh a node or group.
    pub(crate) fn db_refresh(&mut self, key: &str, cx: &mut Context<Self>) {
        if split_group(key).is_some() { self.dbp.members.remove(key); self.dbp.open.insert(key.into()); self.db_load_group(key, cx); cx.notify(); return; }
        let (id, db, _) = key_parts(key);
        if db.is_none() {
            if !self.db_up(&id) { self.db_open_conn(&id, cx); return; }
            self.dbp.kids.remove(key);
            self.db_load_kids(key, cx);
            return;
        }
        let was = self.dbp.opened.contains(key);
        self.db_close_node(key, cx);
        if was { self.db_open_node(key, cx); }
    }

    fn table_target(gk: &str, name: &str) -> Target {
        let (node, _) = split_group(gk).unwrap_or((gk, Group::Tables));
        let (id, db, schema) = key_parts(node);
        Target { conn: id, database: db.unwrap_or_default(), schema, table: Some(name.to_string()) }
    }

    fn db_open_object(&mut self, gk: &str, name: &str, intent: Option<Intent>, window: &mut Window, cx: &mut Context<Self>) {
        if let Some((node, g)) = split_group(gk) {
            let (id, db, _) = key_parts(node);
            if Self::db_kind(&id) == Some(DbKind::Mongodb) {
                let coll = if g == Group::GridFs { format!("{name}.files") } else { name.to_string() };
                let p = self.mongo_pane(&id, (db.unwrap_or_default(), coll, String::new(), String::new()), window, cx);
                self.push_pane(p, window, cx);
                return;
            }
        }
        let target = Self::table_target(gk, name);
        self.open_db_tab(target.clone(), String::new(), window, cx);
        if let Some(i) = intent {
            if let Some(v) = self.db_views(cx).into_iter().find(|v| v.read(cx).target == target) { v.update(cx, |v, cx| v.intent(i, cx)); }
        }
    }

    /// Query tab pre-filled with a statement for an object (view definition, …).
    fn db_query_with(&mut self, node: &str, sql: String, window: &mut Window, cx: &mut Context<Self>) {
        let (id, db, schema) = key_parts(node);
        self.open_db_tab(Target { conn: id, database: db.unwrap_or_default(), schema, table: None }, sql, window, cx);
    }

    fn db_truncate(&mut self, gk: &str, name: &str, cx: &mut Context<Self>) {
        let Some((node, _)) = split_group(gk) else { return };
        let (id, db, schema) = key_parts(node);
        let (Some(live), Some(conn)) = (self.db_ready(&id), Self::db_conn(&id)) else { return };
        let f = live.driver.family();
        let db = db.unwrap_or_default();
        let sql = objtree::truncate_sql(f, &dialect::qualified(f, &db, schema.as_deref(), name));
        let (log, cname, name) = (self.exec_log.clone(), conn.name.clone(), name.to_string());
        let h = self.rt.spawn(async move { live.driver.execute(&db, &sql, conn.read_only).await.map(|_| sql) });
        cx.spawn(async move |this, cx| {
            let r = h.await.map_err(anyhow::Error::from).and_then(|r| r);
            let _ = this.update(cx, |this, cx| {
                match r {
                    Ok(sql) => { crate::activity::log_push(&log, &cname, &sql, true, String::new()); this.toast(tf("dbt.truncated", &[("name", &name)]), cx); }
                    Err(e) => this.toast(tf("dbt.failed", &[("e", &format!("{e:#}"))]), cx),
                }
            });
        }).detach();
    }

    // ---- saved queries

    fn saved_ext(id: &str) -> &'static str { if Self::db_kind(id) == Some(DbKind::Mongodb) { "txt" } else { "sql" } }

    /// Open a saved query in a query tab (bound to its file for Ctrl+S).
    fn db_open_saved(&mut self, node: &str, name: &str, window: &mut Window, cx: &mut Context<Self>) {
        let (id, db, schema) = key_parts(node);
        if Self::db_kind(&id) == Some(DbKind::Mongodb) { self.open_mongo_tab(&id, window, cx); return; }
        let dir = saved_dir(node);
        let text = saved::read(&dir, name).unwrap_or_default();
        let file = saved::path(&dir, name, "sql");
        let db = db.or_else(|| Self::db_conn(&id).map(|c| c.database).filter(|d| !d.is_empty())).unwrap_or_default();
        let target = Target { conn: id, database: db, schema, table: None };
        self.open_db_tab(target, text, window, cx);
        if let Some(v) = self.db_views(cx).into_iter().last() { v.update(cx, |v, _| { v.saved_file = Some(file); v.title = name.to_string(); }); }
    }

    fn db_new_saved(&mut self, node: &str, window: &mut Window, cx: &mut Context<Self>) {
        let (id, ..) = key_parts(node);
        let dir = saved_dir(node);
        let name = saved::free_name(&dir, &t("dbt.new_query_name"));
        if let Err(e) = saved::write(&dir, &name, Self::saved_ext(&id), "") { self.toast(tf("dbt.failed", &[("e", &format!("{e:#}"))]), cx); return; }
        self.dbp.open.insert(group_key(node, Group::Queries));
        self.db_open_saved(node, &name, window, cx);
    }

    fn db_saved_op(&mut self, node: &str, name: &str, op: &str, window: &mut Window, cx: &mut Context<Self>) {
        let dir = saved_dir(node);
        let r = match op {
            "dup" => saved::duplicate(&dir, name).map(|_| ()),
            "del" => saved::delete(&dir, name),
            "rename" => {
                let st = cx.new(|cx| InputState::new(window, cx).default_value(name.to_string()));
                let gk = group_key(node, Group::Queries);
                self._subs.push(cx.subscribe_in(&st, window, |this, st, ev: &InputEvent, _w, cx| match ev {
                    InputEvent::PressEnter { .. } | InputEvent::Blur => {
                        let Some((gk, Some(old), _)) = this.dbp.edit.take() else { return };
                        let new = st.read(cx).value().trim().to_string();
                        if !new.is_empty() && new != old {
                            if let Some((node, _)) = split_group(&gk) { if let Err(e) = saved::rename(&saved_dir(node), &old, &new) { this.toast(tf("dbt.failed", &[("e", &format!("{e:#}"))]), cx); } }
                        }
                        cx.notify();
                    }
                    _ => {}
                }));
                st.update(cx, |s, cx| s.focus(window, cx));
                self.dbp.edit = Some((gk, Some(name.to_string()), st));
                Ok(())
            }
            _ => Ok(()),
        };
        if let Err(e) = r { self.toast(tf("dbt.failed", &[("e", &format!("{e:#}"))]), cx); }
        cx.notify();
    }

    fn db_filter_edit(&mut self, gk: &str, window: &mut Window, cx: &mut Context<Self>) {
        let cur_q = self.dbp.filters.get(gk).cloned().unwrap_or_default();
        let st = cx.new(|cx| InputState::new(window, cx).placeholder(t("dbt.filter_ph")).default_value(cur_q));
        self._subs.push(cx.subscribe_in(&st, window, |this, st, ev: &InputEvent, _w, cx| {
            let Some((gk, None, _)) = this.dbp.edit.clone() else { return };
            match ev {
                InputEvent::Change => { let q = st.read(cx).value().to_string(); if q.trim().is_empty() { this.dbp.filters.remove(&gk); } else { this.dbp.filters.insert(gk, q); } cx.notify(); }
                InputEvent::PressEnter { .. } | InputEvent::Blur => { this.dbp.edit = None; cx.notify(); }
                _ => {}
            }
        }));
        st.update(cx, |s, cx| s.focus(window, cx));
        self.dbp.open.insert(gk.into());
        if !self.dbp.members.contains_key(gk) { self.db_load_group(gk, cx); }
        self.dbp.edit = Some((gk.to_string(), None, st));
        cx.notify();
    }

    // ---- rendering

    pub(crate) fn db_conn_rows(&mut self, mut list: gpui::Stateful<gpui::Div>, d: &DbConn, depth: usize, cx: &mut Context<Self>) -> gpui::Stateful<gpui::Div> {
        let th = cur();
        let id = d.id.clone();
        let state = self.dbp.live.get(&id);
        let ready = matches!(state, Some(LiveState::Ready(_) | LiveState::Doc(_)));
        let open = ready && self.dbp.open.contains(&id);
        let dot = match state { Some(LiveState::Connecting) => Some(c(th.warning)), Some(LiveState::Failed(_)) => Some(c(th.error)), _ => None };
        let sel = self.dbp.selected.as_deref() == Some(id.as_str());
        let kind = d.kind;
        let glyph_col = if ready { crate::file_icon::db_tint(kind.id()) } else { c(th.overlay0) };
        let (k, k2) = (id.clone(), id.clone());
        let mut r = row(SharedString::from(format!("dbc:{id}")), depth, sel)
            .on_click(cx.listener(move |this, e: &ClickEvent, _w, cx| {
                this.dbp.selected = Some(k.clone());
                if this.db_up(&k) { this.db_toggle_expand(&k, cx); }
                else if e.click_count() >= 2 { this.db_open_conn(&k, cx); }
                cx.notify();
            }))
            .on_mouse_down(MouseButton::Right, cx.listener(move |this, e: &MouseDownEvent, _w, cx| { this.dbp.selected = Some(k2.clone()); this.dbp.arm = None; this.dbp.menu = Some((k2.clone(), e.position)); cx.notify(); }))
            .child(chev(open, true))
            .child(crate::file_icon::db_glyph(kind.id(), ui::IC_MD, glyph_col))
            .child(label(d.name.clone(), if ready { c(th.text) } else { c(th.subtext) }))
            .child(div().text_size(px(ui::fs_small())).text_color(c(th.subtext)).child(kind.label()));
        if !d.ssh_via.is_empty() { r = r.child(icon(IconName::Waypoints, ui::IC_XS, c(th.overlay1))); }
        if d.read_only { r = r.child(icon(IconName::Lock, ui::IC_XS, c(th.warning))); }
        if let Some(col) = dot { r = r.child(div().size(px(7.)).rounded_full().bg(col)); }
        list = list.child(r);
        if let Some(LiveState::Failed(e)) = state { if self.dbp.open.contains(&id) { return list.child(err_row(e, depth + 1)); } }
        if matches!(state, Some(LiveState::Connecting)) { return list.child(note_row(&t("db.connecting"), depth + 1)); }
        if !open { return list; }
        list = self.db_node_kids(list, &id, depth + 1, cx);
        // connection-level saved queries
        self.db_group_rows(list, &id, Group::Queries, depth + 1, cx)
    }

    /// Databases of a connection, or schemas of a database.
    fn db_node_kids(&mut self, mut list: gpui::Stateful<gpui::Div>, key: &str, depth: usize, cx: &mut Context<Self>) -> gpui::Stateful<gpui::Div> {
        let th = cur();
        let names = match self.dbp.kids.get(key) {
            None | Some(Kids::Loading) => return list.child(note_row(&t("db.loading"), depth)),
            Some(Kids::Failed(e)) => return list.child(err_row(e, depth)),
            Some(Kids::Ready(v)) => v.clone(),
        };
        if names.is_empty() { return list.child(note_row(&t("db.no_objects"), depth)); }
        let is_schema = key_parts(key).1.is_some();
        for name in names {
            let ck = format!("{key}{SEP}{name}");
            let opened = self.dbp.opened.contains(&ck);
            let open = opened && self.dbp.open.contains(&ck);
            let sel = self.dbp.selected.as_deref() == Some(ck.as_str());
            let ic = if is_schema { IconName::FolderTree } else { IconName::Database };
            let (k1, k2) = (ck.clone(), ck.clone());
            list = list.child(row(SharedString::from(format!("dbn:{ck}")), depth, sel)
                .on_click(cx.listener(move |this, e: &ClickEvent, _w, cx| {
                    this.dbp.selected = Some(k1.clone());
                    if this.dbp.opened.contains(&k1) { this.db_toggle_expand(&k1, cx); } else if e.click_count() >= 2 { this.db_open_node(&k1, cx); }
                    cx.notify();
                }))
                .on_mouse_down(MouseButton::Right, cx.listener(move |this, e: &MouseDownEvent, _w, cx| { this.dbp.selected = Some(k2.clone()); this.dbp.arm = None; this.dbp.menu = Some((k2.clone(), e.position)); cx.notify(); }))
                .child(chev(open, true))
                .child(icon(ic, ui::IC_MD, if opened { c(th.accent) } else { c(th.overlay1) }))
                .child(label(name.clone(), if opened { c(th.text) } else { c(th.subtext) })));
            if !open { continue; }
            if Self::holds_groups(&ck) {
                let kind = Self::db_kind(&key_parts(&ck).0).unwrap_or_default();
                for g in objtree::groups(kind) { list = self.db_group_rows(list, &ck, *g, depth + 1, cx); }
            } else {
                list = self.db_node_kids(list, &ck, depth + 1, cx);
            }
        }
        list
    }

    fn db_group_rows(&mut self, mut list: gpui::Stateful<gpui::Div>, node: &str, g: Group, depth: usize, cx: &mut Context<Self>) -> gpui::Stateful<gpui::Div> {
        let th = cur();
        let gk = group_key(node, g);
        let open = self.dbp.open.contains(&gk);
        let names: Option<Result<Vec<String>, String>> = match g {
            Group::Queries => Some(Ok(saved::list(&saved_dir(node)))),
            Group::Backups => Some(Ok(vec![])),
            _ => match self.dbp.members.get(&gk) { Some(Kids::Ready(v)) => Some(Ok(v.clone())), Some(Kids::Failed(e)) => Some(Err(e.clone())), _ => None },
        };
        let q = self.dbp.filters.get(&gk).cloned().unwrap_or_default();
        let count = match &names { Some(Ok(v)) if g != Group::Backups => { let n = objtree::filter(v, &q).len(); if q.is_empty() { n.to_string() } else { format!("{n}/{}", v.len()) } } _ => String::new() };
        let sel = self.dbp.selected.as_deref() == Some(gk.as_str());
        let (k1, k2) = (gk.clone(), gk.clone());
        list = list.child(row(SharedString::from(format!("dbg2:{gk}")), depth, sel)
            .on_click(cx.listener(move |this, _, _w, cx| { this.dbp.selected = Some(k1.clone()); this.db_toggle_expand(&k1, cx); }))
            .on_mouse_down(MouseButton::Right, cx.listener(move |this, e: &MouseDownEvent, _w, cx| { this.dbp.selected = Some(k2.clone()); this.dbp.arm = None; this.dbp.menu = Some((k2.clone(), e.position)); cx.notify(); }))
            .child(chev(open, true))
            .child(icon(group_icon(g), ui::IC_MD, c(th.overlay1)))
            .child(label(t(&g.label_key()), c(th.subtext)))
            .when(!q.is_empty(), |d| d.child(icon(IconName::Search, ui::IC_XS, c(th.accent))))
            .child(div().text_size(px(ui::fs_small())).text_color(c(th.subtext)).child(count)));
        if let Some((ek, None, st)) = self.dbp.edit.clone() { if ek == gk { list = list.child(div().pl(indent(depth + 1)).pr_2().py_1().child(ui::text_field(&st, false))); } }
        if !open { return list; }
        if g == Group::Backups { return list.child(note_row(&t("dbt.backup_soon"), depth + 1)); }
        let names = match names {
            None => return list.child(note_row(&t("db.loading"), depth + 1)),
            Some(Err(e)) => return list.child(err_row(&e, depth + 1)),
            Some(Ok(v)) => v,
        };
        let shown: Vec<String> = objtree::filter(&names, &q).into_iter().cloned().collect();
        if shown.is_empty() { return list.child(note_row(&t(if g == Group::Queries { "dbt.no_queries" } else { "db.no_objects" }), depth + 1)); }
        let renaming = match &self.dbp.edit { Some((ek, Some(old), st)) if *ek == gk => Some((old.clone(), st.clone())), _ => None };
        for name in shown {
            if let Some((old, st)) = &renaming { if *old == name { list = list.child(div().pl(indent(depth + 1)).pr_2().py_1().child(ui::text_field(st, false))); continue; } }
            let mk = format!("{gk}\u{1d}{name}");
            let sel = self.dbp.selected.as_deref() == Some(mk.as_str());
            let (k1, k2, gk1, n1, node1) = (mk.clone(), mk.clone(), gk.clone(), name.clone(), node.to_string());
            list = list.child(row(SharedString::from(format!("dbo:{mk}")), depth + 1, sel)
                .on_click(cx.listener(move |this, e: &ClickEvent, w, cx| {
                    this.dbp.selected = Some(k1.clone());
                    if e.click_count() >= 2 {
                        if g == Group::Queries { this.db_open_saved(&node1, &n1, w, cx); }
                        else if g.opens_data() || (matches!(g, Group::Collections | Group::GridFs) && Self::db_kind(&key_parts(&gk1).0) == Some(DbKind::Mongodb)) { this.db_open_object(&gk1, &n1, None, w, cx); }
                    }
                    cx.notify();
                }))
                .on_mouse_down(MouseButton::Right, cx.listener(move |this, e: &MouseDownEvent, _w, cx| { this.dbp.selected = Some(k2.clone()); this.dbp.arm = None; this.dbp.menu = Some((k2.clone(), e.position)); cx.notify(); }))
                .child(chev(false, false))
                .child(icon(group_icon(g), ui::IC_MD, c(match g { Group::Views | Group::MatViews => th.warning, Group::Queries => th.info, _ => th.accent })))
                .child(label(name, c(th.text))));
        }
        list
    }

    /// Right-click menu for any tree key (connection, database / schema, group, member).
    pub(crate) fn db_menu_el(&mut self, key: String, pos: Point<Pixels>, cx: &mut Context<Self>) -> AnyElement {
        let mut m = ui::popover().w(px(210.)).py_1().flex().flex_col();
        let close = |this: &mut Root| { this.dbp.menu = None; this.dbp.arm = None; };
        macro_rules! item {
            ($id:expr, $ic:expr, $label:expr, $danger:expr, |$this:ident, $w:ident, $cx:ident| $body:expr) => {
                m = m.child(ui::menu_item($id, Some($ic), $label, true, $danger).on_click(cx.listener(move |$this, _, $w, $cx| { close($this); $body; $cx.notify(); })));
            };
        }
        if let Some((gk, name)) = key.split_once('\u{1d}') {
            // member of a group
            let Some((node, g)) = split_group(gk) else { return div().into_any_element() };
            let (gk, name, node) = (gk.to_string(), name.to_string(), node.to_string());
            match g {
                Group::Queries => {
                    { let (n, nm) = (node.clone(), name.clone()); item!("dbm-qopen", IconName::SquareTerminal, t("dbt.open"), false, |this, w, cx| this.db_open_saved(&n, &nm, w, cx)); }
                    { let (n, nm) = (node.clone(), name.clone()); item!("dbm-qren", IconName::Pencil, t("dbt.rename"), false, |this, w, cx| this.db_saved_op(&n, &nm, "rename", w, cx)); }
                    { let (n, nm) = (node.clone(), name.clone()); item!("dbm-qdup", IconName::Copy, t("dbt.duplicate"), false, |this, w, cx| this.db_saved_op(&n, &nm, "dup", w, cx)); }
                    m = m.child(ui::menu_sep());
                    { let (n, nm) = (node.clone(), name.clone()); item!("dbm-qdel", IconName::Trash, t("dbt.delete"), true, |this, w, cx| this.db_saved_op(&n, &nm, "del", w, cx)); }
                }
                Group::Collections | Group::GridFs => {
                    { let (g2, nm) = (gk.clone(), name.clone()); item!("dbm-mopen", IconName::Table, t("dbt.open_data"), false, |this, w, cx| this.db_open_object(&g2, &nm, None, w, cx)); }
                }
                Group::Tables => {
                    { let (g2, nm) = (gk.clone(), name.clone()); item!("dbm-topen", IconName::Table, t("dbt.open_data"), false, |this, w, cx| this.db_open_object(&g2, &nm, None, w, cx)); }
                    { let (g2, nm) = (gk.clone(), name.clone()); item!("dbm-tdesign", IconName::PencilRuler, t("dbt.design"), false, |this, w, cx| this.db_open_object(&g2, &nm, Some(Intent::Design), w, cx)); }
                    { let (g2, nm) = (gk.clone(), name.clone()); item!("dbm-texport", IconName::Download, t("dbt.export"), false, |this, w, cx| this.db_open_object(&g2, &nm, Some(Intent::Export), w, cx)); }
                    m = m.child(ui::menu_sep());
                    let armed = self.dbp.arm.as_deref() == Some(key.as_str());
                    let (g2, nm, k2) = (gk.clone(), name.clone(), key.clone());
                    m = m.child(ui::menu_item("dbm-ttrunc", Some(IconName::Eraser), if armed { t("dbt.truncate_confirm") } else { t("dbt.truncate") }, true, true)
                        .on_click(cx.listener(move |this, _, _w, cx| {
                            if this.dbp.arm.as_deref() == Some(k2.as_str()) { close(this); this.db_truncate(&g2, &nm, cx); } else { this.dbp.arm = Some(k2.clone()); }
                            cx.notify();
                        })));
                }
                Group::Views if Self::db_kind(&key_parts(&node).0) == Some(DbKind::Mongodb) => {
                    { let (g2, nm) = (gk.clone(), name.clone()); item!("dbm-mvopen", IconName::Table, t("dbt.open_data"), false, |this, w, cx| this.db_open_object(&g2, &nm, None, w, cx)); }
                }
                Group::Views | Group::MatViews => {
                    { let (g2, nm) = (gk.clone(), name.clone()); item!("dbm-vopen", IconName::Table, t("dbt.open_data"), false, |this, w, cx| this.db_open_object(&g2, &nm, None, w, cx)); }
                    let (id, db, schema) = key_parts(&node);
                    let f = Self::db_kind(&id).map(|k| k.family());
                    if let Some(sql) = f.and_then(|f| objtree::view_definition_sql(f, db.as_deref().unwrap_or(""), schema.as_deref(), &name)) {
                        let n = node.clone();
                        item!("dbm-vdef", IconName::FileCode, t("dbt.view_def"), false, |this, w, cx| this.db_query_with(&n, sql.clone(), w, cx));
                    }
                }
                _ => {}
            }
            { let nm = name.clone(); item!("dbm-copyname", IconName::Copy, t("dbt.copy_name"), false, |_this, _w, cx| cx.write_to_clipboard(gpui::ClipboardItem::new_string(nm.clone()))); }
        } else if let Some((node, g)) = split_group(&key) {
            let node = node.to_string();
            if g == Group::Queries { let n = node.clone(); item!("dbm-qnew", IconName::FilePlus, t("dbt.new_saved"), false, |this, w, cx| this.db_new_saved(&n, w, cx)); }
            if g != Group::Backups {
                { let k = key.clone(); item!("dbm-grefresh", IconName::RefreshCw, t("db.refresh"), false, |this, _w, cx| this.db_refresh(&k, cx)); }
                { let k = key.clone(); item!("dbm-gfilter", IconName::Search, t("dbt.filter"), false, |this, w, cx| this.db_filter_edit(&k, w, cx)); }
                if self.dbp.filters.contains_key(&key) { let k = key.clone(); item!("dbm-gclear", IconName::X, t("dbt.filter_clear"), false, |this, _w, cx| { this.dbp.filters.remove(&k); }); }
            }
        } else {
            let (id, db, _) = key_parts(&key);
            if db.is_none() {
                let ready = self.db_up(&id);
                let busy = matches!(self.dbp.live.get(&id), Some(LiveState::Connecting));
                if ready || busy { let i2 = id.clone(); item!("dbm-close", IconName::Unplug, t("dbt.close_conn"), false, |this, _w, cx| this.db_disconnect(&i2, cx)); }
                else { let i2 = id.clone(); item!("dbm-open", IconName::Plug, t("dbt.open_conn"), false, |this, _w, cx| this.db_open_conn(&i2, cx)); }
            } else if self.dbp.opened.contains(&key) {
                let k = key.clone();
                item!("dbm-dclose", IconName::FolderClosed, t(if key_parts(&key).2.is_some() { "dbt.close_schema" } else { "dbt.close_db" }), false, |this, _w, cx| this.db_close_node(&k, cx));
            } else {
                let k = key.clone();
                item!("dbm-dopen", IconName::FolderOpen, t(if key_parts(&key).2.is_some() { "dbt.open_schema" } else { "dbt.open_db" }), false, |this, _w, cx| this.db_open_node(&k, cx));
            }
            m = m.child(ui::menu_sep());
            { let k = key.clone(); item!("dbm-query", IconName::SquareTerminal, t("db.new_query"), false, |this, w, cx| this.db_new_query(Some(k.clone()), w, cx)); }
            { let k = key.clone(); item!("dbm-qnew2", IconName::FilePlus, t("dbt.new_saved"), false, |this, w, cx| this.db_new_saved(&k, w, cx)); }
            if self.db_up(&id) { let k = key.clone(); item!("dbm-refresh", IconName::RefreshCw, t("db.refresh"), false, |this, _w, cx| this.db_refresh(&k, cx)); }
            if db.is_none() { let i2 = id.clone(); item!("dbm-edit", IconName::Pencil, t("db.edit"), false, |this, w, cx| this.open_db_dialog(Some(i2.clone()), w, cx)); }
        }
        deferred(anchored().position(pos).snap_to_window_with_margin(gpui::px(crate::ui::EDGE_MARGIN)).child(m.occlude().on_mouse_down_out(cx.listener(|this, _, _w, cx| { this.dbp.menu = None; this.dbp.arm = None; cx.notify(); })))).with_priority(2).into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn group_keys_roundtrip() {
        let node = format!("c1{SEP}app{SEP}public");
        let gk = group_key(&node, Group::MatViews);
        assert_eq!(split_group(&gk), Some((node.as_str(), Group::MatViews)));
        assert_eq!(key_parts(&gk).0, "c1");
        assert!(split_group("c1").is_none());
    }
}
