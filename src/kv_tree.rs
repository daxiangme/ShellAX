//! Redis connection tree in the left tool window: connections are opened explicitly, each
//! shows db0..dbN (key counts from INFO keyspace); an opened database lists its keys folded
//! into folders by the connection's separator, plus saved commands (查询).
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use gpui::{anchored, deferred, div, prelude::*, px, AnyElement, ClickEvent, Context, Entity, MouseButton, MouseDownEvent, Pixels, Point, SharedString, Window};
use gpui_component::input::{InputEvent, InputState};
use ssh_term_proto::assets::RedisConn;
use ssh_term_proto::db::saved;
use ssh_term_proto::i18n::{t, tf};
use ssh_term_proto::kv::{tree, Cursor, Kv};

use crate::config;
use crate::db_tree::{chev, row};
use crate::theme::{c, cur};
use crate::ui::{self, icon, IconName};
use crate::Root;

/// Keys loaded per opened database (SCAN stops here).
pub const KEY_LIMIT: usize = 5000;
const SEP: char = '\u{1f}';

pub enum KvLive { Connecting, Ready { kv: Arc<Kv>, dbs: u32, counts: HashMap<u32, u64> }, Failed(String) }
pub enum KvDb { Loading, Ready { kv: Arc<Kv>, keys: Vec<String>, more: bool }, Failed(String) }

#[derive(Default)]
pub struct KvPanel {
    pub live: HashMap<String, KvLive>,
    /// Opened databases by `<id>\x1f<n>`.
    pub dbs: HashMap<String, KvDb>,
    /// Expanded nodes: connections, databases, folders (`<db key>\x1f<path>`), `<id>\x1fq`.
    pub open: HashSet<String>,
    pub menu: Option<(String, Point<Pixels>)>,
    pub selected: Option<String>,
    /// Saved command being renamed: (connection id, old name, input).
    pub edit: Option<(String, String, Entity<InputState>)>,
}

fn db_key(id: &str, n: u32) -> String { format!("{id}{SEP}{n}") }
fn saved_dir(id: &str) -> std::path::PathBuf { saved::dir(&saved::root(), id, &[]) }

impl Root {
    fn kv_conn(id: &str) -> Option<RedisConn> { config::get().redis.iter().find(|r| r.id == id).cloned() }

    fn kv_open_with(&self, conn: RedisConn) -> tokio::task::JoinHandle<anyhow::Result<Kv>> {
        let via = if conn.mode == "standalone" || conn.mode.is_empty() { self.via_for(&conn.ssh_via) } else { Ok(None) };
        let pw = crate::secret_store().get(&conn.password_secret()).map(|x| x.0);
        self.rt.spawn(async move { let via = via.map_err(anyhow::Error::msg)?; Kv::open(&conn, pw, via).await })
    }

    pub(crate) fn kv_open_conn(&mut self, id: &str, cx: &mut Context<Self>) {
        if matches!(self.kvp.live.get(id), Some(KvLive::Connecting | KvLive::Ready { .. })) { self.kvp.open.insert(id.into()); cx.notify(); return; }
        let Some(conn) = Self::kv_conn(id) else { return };
        self.kvp.live.insert(id.into(), KvLive::Connecting);
        self.kvp.open.insert(id.into());
        let h = self.kv_open_with(conn);
        let rt = self.rt.handle().clone();
        let id = id.to_string();
        cx.spawn(async move |this, cx| {
            let r = h.await.map_err(anyhow::Error::from).and_then(|r| r);
            let r = match r {
                Ok(kv) => { let kv = Arc::new(kv); let k2 = kv.clone(); rt.spawn(async move { (k2.database_count().await, k2.keyspace().await) }).await.map(|(n, ks)| (kv, n, ks)).map_err(anyhow::Error::from) }
                Err(e) => Err(e),
            };
            let _ = this.update(cx, |this, cx| {
                this.kvp.live.insert(id.clone(), match r { Ok((kv, dbs, ks)) => KvLive::Ready { kv, dbs, counts: ks.into_iter().collect() }, Err(e) => KvLive::Failed(format!("{e:#}")) });
                cx.notify();
            });
        }).detach();
        cx.notify();
    }

    pub(crate) fn kv_close_conn(&mut self, id: &str, cx: &mut Context<Self>) {
        let pre = format!("{id}{SEP}");
        let mut kvs: Vec<Arc<Kv>> = vec![];
        if let Some(KvLive::Ready { kv, .. }) = self.kvp.live.remove(id) { kvs.push(kv); }
        for (k, d) in self.kvp.dbs.iter() { if k.starts_with(&pre) { if let KvDb::Ready { kv, .. } = d { kvs.push(kv.clone()); } } }
        self.kvp.dbs.retain(|k, _| !k.starts_with(&pre));
        self.kvp.open.retain(|k| k != id && !k.starts_with(&pre));
        self.rt.spawn(async move { for kv in kvs { kv.close().await; } });
        cx.notify();
    }

    fn kv_open_db(&mut self, id: &str, n: u32, cx: &mut Context<Self>) {
        let k = db_key(id, n);
        self.kvp.open.insert(k.clone());
        if matches!(self.kvp.dbs.get(&k), Some(KvDb::Loading | KvDb::Ready { .. })) { cx.notify(); return; }
        let Some(mut conn) = Self::kv_conn(id) else { return };
        conn.db = n;
        self.kvp.dbs.insert(k.clone(), KvDb::Loading);
        let h = self.kv_open_with(conn);
        let rt = self.rt.handle().clone();
        cx.spawn(async move |this, cx| {
            let r = h.await.map_err(anyhow::Error::from).and_then(|r| r);
            let r = match r {
                Ok(kv) => {
                    let kv = Arc::new(kv);
                    let k2 = kv.clone();
                    rt.spawn(async move {
                        let (mut keys, mut cur) = (vec![], Cursor::default());
                        while !cur.done && keys.len() < KEY_LIMIT { let (b, c) = k2.scan(cur, "*", None).await?; keys.extend(b); cur = c; }
                        keys.sort();
                        anyhow::Ok((keys, !cur.done))
                    }).await.map_err(anyhow::Error::from).and_then(|r| r).map(|(keys, more)| KvDb::Ready { kv, keys, more })
                }
                Err(e) => Err(e),
            };
            let _ = this.update(cx, |this, cx| { this.kvp.dbs.insert(k, r.unwrap_or_else(|e| KvDb::Failed(format!("{e:#}")))); cx.notify(); });
        }).detach();
        cx.notify();
    }

    fn kv_close_db(&mut self, id: &str, n: u32, cx: &mut Context<Self>) {
        let k = db_key(id, n);
        let pre = format!("{k}{SEP}");
        if let Some(KvDb::Ready { kv, .. }) = self.kvp.dbs.remove(&k) { self.rt.spawn(async move { kv.close().await }); }
        self.kvp.open.retain(|o| *o != k && !o.starts_with(&pre));
        cx.notify();
    }

    fn kv_toggle(&mut self, k: &str, cx: &mut Context<Self>) { if !self.kvp.open.remove(k) { self.kvp.open.insert(k.into()); } cx.notify(); }

    /// Redis tab on database `n`, optionally selecting `key`.
    fn kv_open_tab(&mut self, id: &str, n: Option<u32>, key: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        let p = self.redis_pane_on(id, n, key, window, cx);
        self.push_pane(p, window, cx);
    }

    fn kv_open_saved(&mut self, id: &str, name: &str, window: &mut Window, cx: &mut Context<Self>) {
        let text = saved::read(&saved_dir(id), name).unwrap_or_default();
        self.kv_open_tab(id, None, None, window, cx);
        if let Some(crate::termview::Embed::Kv(v)) = self.tabs.get(self.active).and_then(|t| t.focused.read(cx).embed.clone()) {
            v.update(cx, |v, cx| v.insert_console(&text, false, window, cx));
        }
    }

    fn kv_saved_op(&mut self, id: &str, name: Option<&str>, op: &str, window: &mut Window, cx: &mut Context<Self>) {
        let dir = saved_dir(id);
        let r: anyhow::Result<()> = match (op, name) {
            ("new", _) => { let n = saved::free_name(&dir, &t("dbt.new_query_name")); self.kvp.open.insert(format!("{id}{SEP}q")); saved::write(&dir, &n, "txt", "").map(|_| ()) }
            ("dup", Some(n)) => saved::duplicate(&dir, n).map(|_| ()),
            ("del", Some(n)) => saved::delete(&dir, n),
            ("rename", Some(n)) => {
                let st = cx.new(|cx| InputState::new(window, cx).default_value(n.to_string()));
                self._subs.push(cx.subscribe_in(&st, window, |this, st, ev: &InputEvent, _w, cx| {
                    if !matches!(ev, InputEvent::PressEnter { .. } | InputEvent::Blur) { return; }
                    let Some((id, old, _)) = this.kvp.edit.take() else { return };
                    let new = st.read(cx).value().trim().to_string();
                    if !new.is_empty() && new != old { if let Err(e) = saved::rename(&saved_dir(&id), &old, &new) { this.toast(tf("dbt.failed", &[("e", &format!("{e:#}"))]), cx); } }
                    cx.notify();
                }));
                st.update(cx, |s, cx| s.focus(window, cx));
                self.kvp.edit = Some((id.to_string(), n.to_string(), st));
                Ok(())
            }
            _ => Ok(()),
        };
        if let Err(e) = r { self.toast(tf("dbt.failed", &[("e", &format!("{e:#}"))]), cx); }
        cx.notify();
    }

    fn kv_menu_to(&mut self, k: String, e: &MouseDownEvent, cx: &mut Context<Self>) { self.kvp.selected = Some(k.clone()); self.kvp.menu = Some((k, e.position)); cx.notify(); }

    /// Tree rows of all Redis connections (grouped by group path).
    pub(crate) fn kv_tree_list(&mut self, cx: &mut Context<Self>) -> gpui::Stateful<gpui::Div> {
        let mut conns = config::get().redis.clone();
        conns.sort_by(|a, b| (a.group_path(), a.name.to_lowercase()).cmp(&(b.group_path(), b.name.to_lowercase())));
        let mut list = div().id("kv-tree").flex_1().min_h_0().overflow_y_scroll().pb_2().flex().flex_col();
        let mut last: Option<String> = None;
        for conn in conns {
            let g = conn.group_path();
            if last.as_ref() != Some(&g) && !g.is_empty() {
                list = list.child(ui::section_header(SharedString::from(format!("kvg:{g}")), g.clone(), None, None));
            }
            last = Some(g);
            list = self.kv_conn_rows(list, &conn, cx);
        }
        list
    }

    fn kv_conn_rows(&mut self, mut list: gpui::Stateful<gpui::Div>, conn: &RedisConn, cx: &mut Context<Self>) -> gpui::Stateful<gpui::Div> {
        let th = cur();
        let id = conn.id.clone();
        let state = self.kvp.live.get(&id);
        let ready = matches!(state, Some(KvLive::Ready { .. }));
        let open = self.kvp.open.contains(&id);
        let dot = match state { Some(KvLive::Connecting) => Some(c(th.warning)), Some(KvLive::Failed(_)) => Some(c(th.error)), _ => None };
        let prod = Self::production_of(&conn.tags, &conn.group_path());
        let (k1, k2) = (id.clone(), id.clone());
        let mut r = row(SharedString::from(format!("kvc:{id}")), 0, self.kvp.selected.as_deref() == Some(id.as_str()))
            .on_click(cx.listener(move |this, e: &ClickEvent, _w, cx| {
                this.kvp.selected = Some(k1.clone());
                if matches!(this.kvp.live.get(&k1), Some(KvLive::Ready { .. })) { this.kv_toggle(&k1, cx); } else if e.click_count() >= 2 { this.kv_open_conn(&k1, cx); }
                cx.notify();
            }))
            .on_mouse_down(MouseButton::Right, cx.listener(move |this, e: &MouseDownEvent, _w, cx| this.kv_menu_to(k2.clone(), e, cx)))
            .child(chev(open && ready, true))
            .child(crate::file_icon::db_glyph("redis", ui::IC_MD, if ready { crate::file_icon::db_tint("redis") } else { c(th.overlay0) }))
            .child(div().flex_1().min_w_0().whitespace_nowrap().overflow_hidden().text_ellipsis().text_color(if ready { c(th.text) } else { c(th.subtext) }).child(conn.name.clone()))
            .child(div().text_size(px(ui::fs_small())).text_color(c(th.subtext)).whitespace_nowrap().child(conn.address()));
        if prod { r = r.child(div().size(px(7.)).rounded_full().bg(c(th.error))); }
        if conn.read_only { r = r.child(icon(IconName::Lock, ui::IC_XS, c(th.warning))); }
        if let Some(col) = dot { r = r.child(div().size(px(7.)).rounded_full().bg(col)); }
        list = list.child(r);
        let note = |s: String, depth: usize, err: bool| div().min_h(px(ui::H_COMPACT)).pl(px(ui::ROW_PX + (depth + 1) as f32 * ui::INDENT)).pr_2().flex().items_center().text_size(px(ui::fs_small())).text_color(if err { c(cur().error) } else { c(cur().subtext) }).child(s);
        match state {
            Some(KvLive::Connecting) => return list.child(note(t("db.connecting").to_string(), 1, false)),
            Some(KvLive::Failed(e)) => { let e = e.chars().take(300).collect(); return if open { list.child(note(e, 1, true)) } else { list }; }
            None => return list,
            _ => {}
        }
        if !open { return list; }
        let Some(KvLive::Ready { dbs, counts, .. }) = self.kvp.live.get(&id) else { return list };
        let (dbs, counts) = (*dbs, counts.clone());
        for n in 0..dbs {
            let dk = db_key(&id, n);
            let opened = self.kvp.dbs.contains_key(&dk);
            let dopen = opened && self.kvp.open.contains(&dk);
            let (d1, d2, i1) = (dk.clone(), dk.clone(), id.clone());
            list = list.child(row(SharedString::from(format!("kvd:{dk}")), 1, self.kvp.selected.as_deref() == Some(dk.as_str()))
                .on_click(cx.listener(move |this, e: &ClickEvent, _w, cx| {
                    this.kvp.selected = Some(d1.clone());
                    if this.kvp.dbs.contains_key(&d1) { this.kv_toggle(&d1, cx); } else if e.click_count() >= 2 { this.kv_open_db(&i1, n, cx); }
                    cx.notify();
                }))
                .on_mouse_down(MouseButton::Right, cx.listener(move |this, e: &MouseDownEvent, _w, cx| this.kv_menu_to(d2.clone(), e, cx)))
                .child(chev(dopen, true))
                .child(icon(IconName::Database, ui::IC_MD, if opened { c(th.accent) } else { c(th.overlay1) }))
                .child(div().flex_1().text_color(if opened { c(th.text) } else { c(th.subtext) }).child(format!("db{n}")))
                .child(div().text_size(px(ui::fs_small())).text_color(c(th.subtext)).child(counts.get(&n).map(|c| c.to_string()).unwrap_or_default())));
            if !dopen { continue; }
            match self.kvp.dbs.get(&dk) {
                Some(KvDb::Loading) | None => { list = list.child(note(t("db.loading").to_string(), 2, false)); }
                Some(KvDb::Failed(e)) => { list = list.child(note(e.chars().take(300).collect(), 2, true)); }
                Some(KvDb::Ready { keys, more, .. }) => {
                    let more = *more;
                    if keys.is_empty() { list = list.child(note(t("db.no_objects").to_string(), 2, false)); continue; }
                    let root = tree::Node::build(keys.iter().map(String::as_str), conn.sep());
                    let pre = format!("{dk}{SEP}");
                    let open_paths: HashSet<String> = self.kvp.open.iter().filter_map(|o| o.strip_prefix(&pre).map(str::to_string)).collect();
                    for r in tree::rows(&root, conn.sep(), &open_paths) {
                        list = match r {
                            tree::Row::Dir { path, name, depth, count, open } => {
                                let fk = format!("{pre}{path}");
                                let f1 = fk.clone();
                                list.child(row(SharedString::from(format!("kvf:{fk}")), depth + 2, false)
                                    .on_click(cx.listener(move |this, _, _w, cx| this.kv_toggle(&f1, cx)))
                                    .child(chev(open, true)).child(icon(if open { IconName::FolderOpen } else { IconName::Folder }, ui::IC_MD, c(th.overlay1)))
                                    .child(div().flex_1().min_w_0().whitespace_nowrap().overflow_hidden().text_ellipsis().text_color(c(th.subtext)).child(name))
                                    .child(div().text_size(px(ui::fs_small())).text_color(c(th.subtext)).child(count.to_string())))
                            }
                            tree::Row::Key { key, name, depth } => {
                                let kk = format!("{pre}\u{1d}{key}");
                                let (i2, key2, s1) = (id.clone(), key.clone(), kk.clone());
                                list.child(row(SharedString::from(format!("kvk:{kk}")), depth + 2, self.kvp.selected.as_deref() == Some(kk.as_str()))
                                    .on_click(cx.listener(move |this, e: &ClickEvent, w, cx| { this.kvp.selected = Some(s1.clone()); if e.click_count() >= 2 { this.kv_open_tab(&i2, Some(n), Some(key2.clone()), w, cx); } cx.notify(); }))
                                    .child(chev(false, false)).child(icon(IconName::KeyRound, ui::IC_MD, c(th.accent)))
                                    .child(div().flex_1().min_w_0().whitespace_nowrap().overflow_hidden().text_ellipsis().text_color(c(th.text)).child(name)))
                            }
                        };
                    }
                    if more { list = list.child(note(tf("kvt.more", &[("n", &KEY_LIMIT)]), 2, false)); }
                }
            }
        }
        // saved commands
        let qk = format!("{id}{SEP}q");
        let qopen = self.kvp.open.contains(&qk);
        let names = saved::list(&saved_dir(&id));
        let (q1, q2) = (qk.clone(), qk.clone());
        list = list.child(row(SharedString::from(format!("kvq:{qk}")), 1, false)
            .on_click(cx.listener(move |this, _, _w, cx| this.kv_toggle(&q1, cx)))
            .on_mouse_down(MouseButton::Right, cx.listener(move |this, e: &MouseDownEvent, _w, cx| this.kv_menu_to(q2.clone(), e, cx)))
            .child(chev(qopen, true)).child(icon(IconName::FileCode, ui::IC_MD, c(th.overlay1)))
            .child(div().flex_1().text_color(c(th.subtext)).child(t("dbt.g_queries")))
            .child(div().text_size(px(ui::fs_small())).text_color(c(th.subtext)).child(names.len().to_string())));
        if qopen {
            if names.is_empty() { list = list.child(note(t("dbt.no_queries").to_string(), 2, false)); }
            for name in names {
                if let Some((eid, old, st)) = &self.kvp.edit { if *eid == id && *old == name { list = list.child(div().pl(px(ui::ROW_PX + 2. * ui::INDENT)).pr_2().py_1().child(ui::text_field(st, false))); continue; } }
                let mk = format!("{qk}\u{1d}{name}");
                let (i2, n2, m2, m3) = (id.clone(), name.clone(), mk.clone(), mk.clone());
                list = list.child(row(SharedString::from(format!("kvs:{mk}")), 2, self.kvp.selected.as_deref() == Some(mk.as_str()))
                    .on_click(cx.listener(move |this, e: &ClickEvent, w, cx| { this.kvp.selected = Some(m2.clone()); if e.click_count() >= 2 { this.kv_open_saved(&i2, &n2, w, cx); } cx.notify(); }))
                    .on_mouse_down(MouseButton::Right, cx.listener(move |this, e: &MouseDownEvent, _w, cx| this.kv_menu_to(m3.clone(), e, cx)))
                    .child(chev(false, false)).child(icon(IconName::FileCode, ui::IC_MD, c(th.info)))
                    .child(div().flex_1().min_w_0().whitespace_nowrap().overflow_hidden().text_ellipsis().text_color(c(th.text)).child(name)));
            }
        }
        list
    }

    pub(crate) fn kv_menu_el(&mut self, key: String, pos: Point<Pixels>, cx: &mut Context<Self>) -> AnyElement {
        let mut m = ui::popover().w(px(200.)).py_1().flex().flex_col();
        macro_rules! item {
            ($id:expr, $ic:expr, $label:expr, $danger:expr, |$this:ident, $w:ident, $cx:ident| $body:expr) => {
                m = m.child(ui::menu_item($id, Some($ic), $label, true, $danger).on_click(cx.listener(move |$this, _, $w, $cx| { $this.kvp.menu = None; $body; $cx.notify(); })));
            };
        }
        let mut parts = key.splitn(2, SEP);
        let id = parts.next().unwrap_or_default().to_string();
        match parts.next() {
            None => {
                let live = matches!(self.kvp.live.get(&id), Some(KvLive::Ready { .. } | KvLive::Connecting));
                if live { let i = id.clone(); item!("kvm-close", IconName::Unplug, t("dbt.close_conn"), false, |this, _w, cx| this.kv_close_conn(&i, cx)); }
                else { let i = id.clone(); item!("kvm-open", IconName::Plug, t("dbt.open_conn"), false, |this, _w, cx| { this.kvp.live.remove(&i); this.kv_open_conn(&i, cx) }); }
                m = m.child(ui::menu_sep());
                { let i = id.clone(); item!("kvm-tab", IconName::SquareTerminal, t("kvt.open_tab"), false, |this, w, cx| this.kv_open_tab(&i, None, None, w, cx)); }
                { let i = id.clone(); item!("kvm-qnew", IconName::FilePlus, t("dbt.new_saved"), false, |this, w, cx| this.kv_saved_op(&i, None, "new", w, cx)); }
                { let i = id.clone(); item!("kvm-edit", IconName::Pencil, t("db.edit"), false, |this, w, cx| this.open_asset_dialog(crate::kv_ui::AssetKind::Redis, Some(i.clone()), w, cx)); }
            }
            Some("q") => { let i = id.clone(); item!("kvm-qnew", IconName::FilePlus, t("dbt.new_saved"), false, |this, w, cx| this.kv_saved_op(&i, None, "new", w, cx)); }
            Some(rest) if rest.starts_with("q\u{1d}") => {
                let name = rest["q\u{1d}".len()..].to_string();
                { let (i, n) = (id.clone(), name.clone()); item!("kvm-qopen", IconName::SquareTerminal, t("dbt.open"), false, |this, w, cx| this.kv_open_saved(&i, &n, w, cx)); }
                { let (i, n) = (id.clone(), name.clone()); item!("kvm-qren", IconName::Pencil, t("dbt.rename"), false, |this, w, cx| this.kv_saved_op(&i, Some(&n), "rename", w, cx)); }
                { let (i, n) = (id.clone(), name.clone()); item!("kvm-qdup", IconName::Copy, t("dbt.duplicate"), false, |this, w, cx| this.kv_saved_op(&i, Some(&n), "dup", w, cx)); }
                m = m.child(ui::menu_sep());
                { let (i, n) = (id.clone(), name.clone()); item!("kvm-qdel", IconName::Trash, t("dbt.delete"), true, |this, w, cx| this.kv_saved_op(&i, Some(&n), "del", w, cx)); }
            }
            Some(n) => {
                let Ok(n) = n.parse::<u32>() else { return div().into_any_element() };
                if self.kvp.dbs.contains_key(&db_key(&id, n)) {
                    { let i = id.clone(); item!("kvm-dclose", IconName::FolderClosed, t("dbt.close_db"), false, |this, _w, cx| this.kv_close_db(&i, n, cx)); }
                    { let i = id.clone(); item!("kvm-drefresh", IconName::RefreshCw, t("db.refresh"), false, |this, _w, cx| { this.kv_close_db(&i, n, cx); this.kv_open_db(&i, n, cx) }); }
                } else { let i = id.clone(); item!("kvm-dopen", IconName::FolderOpen, t("dbt.open_db"), false, |this, _w, cx| this.kv_open_db(&i, n, cx)); }
                { let i = id.clone(); item!("kvm-dtab", IconName::SquareTerminal, t("kvt.open_tab"), false, |this, w, cx| this.kv_open_tab(&i, Some(n), None, w, cx)); }
            }
        }
        deferred(anchored().position(pos).child(m.occlude().on_mouse_down_out(cx.listener(|this, _, _w, cx| { this.kvp.menu = None; cx.notify(); })))).with_priority(2).into_any_element()
    }
}
