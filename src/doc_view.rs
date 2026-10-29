//! MongoDB tab content: database / collection tree, a document browser with JSON filter,
//! sort, projection and paging, tree or JSON document views, edit / insert / delete,
//! indexes, a basic aggregation pipeline editor and JSON export.
use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::sync::Arc;

use gpui::{div, prelude::*, px, AnyElement, Context, Entity, FontWeight, SharedString, Window};
use gpui_component::input::{Editor, EditorState, InputEvent, InputState};
use mongodb::bson::{Bson, Document};
use ssh_term_proto::assets::DbConn;
use ssh_term_proto::doc::{self, IndexRow, Mongo};
use ssh_term_proto::i18n::{t, tf};
use ssh_term_proto::ssh::{Prompts, SshTarget};

use crate::activity::{log_push, ExecLog};
use crate::theme::{c, cur};
use crate::ui::{self, icon, mono, small, IconName};

#[derive(Clone, Copy, PartialEq)]
enum Sub { Docs, Indexes, Aggregate }
#[derive(Clone, Copy, PartialEq)]
enum Show { Tree, Json }

enum Ask { Replace(Bson, String), Insert(String), Delete(Bson), Aggregate(String) }

pub struct DocView {
    pub conn: DbConn,
    pub title: String,
    production: bool,
    mongo: Option<Arc<Mongo>>,
    conn_err: Option<String>,
    rt: tokio::runtime::Handle,
    log: ExecLog,
    // tree
    dbs: Vec<String>,
    colls: HashMap<String, Vec<String>>,
    open: HashSet<String>,
    db: String,
    coll: String,
    // query
    filter_in: Entity<InputState>,
    sort_in: Entity<InputState>,
    proj_in: Entity<InputState>,
    skip: u64,
    total: Option<u64>,
    docs: Vec<Document>,
    selected: Option<usize>,
    show: Show,
    /// None = viewing; Some(None) = new document; Some(Some(i)) = editing docs[i].
    editing: Option<Option<usize>>,
    doc_ed: Entity<EditorState>,
    pending_doc: Option<String>,
    sub: Sub,
    indexes: Vec<IndexRow>,
    pipe_ed: Entity<EditorState>,
    agg: Vec<Document>,
    agg_ms: Option<u128>,
    busy: bool,
    error: Option<String>,
    note: Option<String>,
    ask: Option<Ask>,
    collapsed: HashSet<String>,
    /// Filter / sort of the last query, kept for layout save.
    saved: (String, String),
    _subs: Vec<gpui::Subscription>,
}

const AGG_LIMIT: usize = 500;

/// Flattened rows of a document tree: (path, depth, key, value preview, has children).
pub fn tree_rows(d: &Document, collapsed: &HashSet<String>) -> Vec<(String, usize, String, String, bool)> {
    fn walk(b: &Bson, path: &str, key: &str, depth: usize, collapsed: &HashSet<String>, out: &mut Vec<(String, usize, String, String, bool)>) {
        match b {
            Bson::Document(d) => {
                out.push((path.to_string(), depth, key.to_string(), format!("{{ {} }}", d.len()), true));
                if !collapsed.contains(path) { for (k, v) in d { walk(v, &format!("{path}.{k}"), k, depth + 1, collapsed, out); } }
            }
            Bson::Array(a) => {
                out.push((path.to_string(), depth, key.to_string(), format!("[ {} ]", a.len()), true));
                if !collapsed.contains(path) { for (i, v) in a.iter().enumerate() { walk(v, &format!("{path}.{i}"), &i.to_string(), depth + 1, collapsed, out); } }
            }
            other => {
                let s = match other { Bson::String(s) => format!("\"{s}\""), Bson::ObjectId(o) => format!("ObjectId(\"{}\")", o.to_hex()), Bson::DateTime(d) => d.try_to_rfc3339_string().unwrap_or_default(), o => o.clone().into_relaxed_extjson().to_string() };
                out.push((path.to_string(), depth, key.to_string(), s, false));
            }
        }
    }
    let mut out = vec![];
    for (k, v) in d { walk(v, k, k, 0, collapsed, &mut out); }
    out
}

impl DocView {
    #[allow(clippy::too_many_arguments)]
    pub fn new(conn: DbConn, password: Option<String>, via: Result<Option<(SshTarget, Prompts)>, String>, production: bool, start: (String, String, String, String), rt: tokio::runtime::Handle, log: ExecLog, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let (db, coll, filter, sort) = start;
        let mk = |ph: &str, v: String, window: &mut Window, cx: &mut Context<Self>| { let ph = ph.to_string(); cx.new(|cx| InputState::new(window, cx).placeholder(ph).default_value(v)) };
        let filter_in = mk(r#"{ "status": "active" }"#, filter, window, cx);
        let sort_in = mk(r#"{ "_id": -1 }"#, sort, window, cx);
        let proj_in = mk(r#"{ "name": 1 }"#, String::new(), window, cx);
        let doc_ed = cx.new(|cx| EditorState::new(window, cx).language("json").line_number(true));
        let pipe_ed = cx.new(|cx| EditorState::new(window, cx).language("json").line_number(true).default_value("[\n  { \"$match\": {} },\n  { \"$limit\": 20 }\n]"));
        let mut subs = vec![];
        for i in [&filter_in, &sort_in, &proj_in] {
            subs.push(cx.subscribe_in(i, window, |this: &mut Self, _, ev: &InputEvent, _w, cx| { if let InputEvent::PressEnter { .. } = ev { this.skip = 0; this.find(cx); } }));
        }
        let mut me = Self { title: conn.name.clone(), production, mongo: None, conn_err: None, rt, log, dbs: vec![], colls: HashMap::new(), open: HashSet::new(), db, coll,
            filter_in, sort_in, proj_in, skip: 0, total: None, docs: vec![], selected: None, show: Show::Tree, editing: None, doc_ed, pending_doc: None, sub: Sub::Docs,
            indexes: vec![], pipe_ed, agg: vec![], agg_ms: None, busy: false, error: None, note: None, ask: None, collapsed: HashSet::new(), saved: (String::new(), String::new()), _subs: subs, conn };
        let via = match via { Ok(v) => v, Err(e) => { me.conn_err = Some(e); return me; } };
        let c = me.conn.clone();
        let h = me.rt.spawn(async move { Mongo::open(&c, password, via).await.map(Arc::new) });
        cx.spawn(async move |this, cx| {
            let r = h.await.map_err(anyhow::Error::from).and_then(|r| r);
            let _ = this.update(cx, |v, cx| {
                let what = format!("connect {}", v.conn.name);
                match r {
                    Ok(m) => {
                        log_push(&v.log, "mongo", &what, true, m.version.clone());
                        v.mongo = Some(m);
                        v.load_dbs(cx);
                        if !v.db.is_empty() { let d = v.db.clone(); v.open.insert(d.clone()); v.load_colls(d, cx); }
                        if !v.coll.is_empty() { v.find(cx); }
                    }
                    Err(e) => { log_push(&v.log, "mongo", &what, false, format!("{e:#}")); v.conn_err = Some(format!("{e:#}")); }
                }
                cx.notify();
            });
        }).detach();
        me
    }

    pub fn crumbs(&self) -> String {
        let mut s = self.conn.name.clone();
        for p in [&self.db, &self.coll] { if !p.is_empty() { s.push_str(" › "); s.push_str(p); } }
        s
    }
    pub fn save(&self) -> ssh_term_proto::session_store::PaneSave {
        // the filter bar text is read on save via `saved_query`
        ssh_term_proto::session_store::PaneSave::Mongo { conn: self.conn.id.clone(), database: self.db.clone(), collection: self.coll.clone(), filter: self.saved.0.clone(), sort: self.saved.1.clone() }
    }

    fn job<T: Send + 'static, F: Future<Output = anyhow::Result<T>> + Send + 'static>(&mut self, f: impl FnOnce(Arc<Mongo>) -> F, done: impl FnOnce(&mut Self, anyhow::Result<T>, &mut Context<Self>) + 'static, cx: &mut Context<Self>) {
        let Some(m) = self.mongo.clone() else { return };
        let h = self.rt.spawn(f(m));
        cx.spawn(async move |this, cx| {
            let r = h.await.map_err(anyhow::Error::from).and_then(|r| r);
            let _ = this.update(cx, |v, cx| { done(v, r, cx); cx.notify(); });
        }).detach();
    }
    fn fail(&mut self, e: anyhow::Error) { self.error = Some(format!("{e:#}")); }
    fn logged<T>(&self, what: &str, r: &anyhow::Result<T>) { log_push(&self.log, "mongo", what, r.is_ok(), r.as_ref().err().map(|e| format!("{e:#}")).unwrap_or_default()); }
    fn val(i: &Entity<InputState>, cx: &Context<Self>) -> String { i.read(cx).value().trim().to_string() }

    fn load_dbs(&mut self, cx: &mut Context<Self>) { self.job(|m| async move { m.databases().await }, |v, r, _| match r { Ok(d) => v.dbs = d, Err(e) => v.fail(e) }, cx); }
    fn load_colls(&mut self, db: String, cx: &mut Context<Self>) {
        let d2 = db.clone();
        self.job(move |m| async move { m.collections(&db).await }, move |v, r, _| match r { Ok(c) => { v.colls.insert(d2, c); } Err(e) => v.fail(e) }, cx);
    }
    fn pick(&mut self, db: String, coll: String, cx: &mut Context<Self>) {
        self.db = db; self.coll = coll; self.skip = 0; self.selected = None; self.editing = None; self.indexes.clear(); self.agg.clear();
        self.title = format!("{} · {}", self.conn.name, self.coll);
        match self.sub { Sub::Indexes => self.load_indexes(cx), _ => self.find(cx) }
    }

    fn find(&mut self, cx: &mut Context<Self>) {
        if self.coll.is_empty() { return; }
        let (db, coll, f, s, p, skip) = (self.db.clone(), self.coll.clone(), Self::val(&self.filter_in, cx), Self::val(&self.sort_in, cx), Self::val(&self.proj_in, cx), self.skip);
        self.saved = (f.clone(), s.clone());
        self.busy = true; self.error = None;
        self.job(move |m| async move { m.find(&db, &coll, &f, &s, &p, skip).await }, |v, r, _| {
            v.busy = false;
            match r { Ok(pg) => { if pg.total.is_some() { v.total = pg.total; } v.docs = pg.docs; v.selected = if v.docs.is_empty() { None } else { Some(0) }; v.editing = None; v.sync_editor(); } Err(e) => v.fail(e) }
        }, cx);
    }
    fn sync_editor(&mut self) { self.pending_doc = Some(self.selected.and_then(|i| self.docs.get(i)).map(doc::to_json).unwrap_or_default()); }
    fn load_indexes(&mut self, cx: &mut Context<Self>) {
        if self.coll.is_empty() { return; }
        let (db, coll) = (self.db.clone(), self.coll.clone());
        self.job(move |m| async move { m.indexes(&db, &coll).await }, |v, r, _| match r { Ok(i) => v.indexes = i, Err(e) => v.fail(e) }, cx);
    }

    fn writable(&mut self) -> bool { if self.conn.read_only { self.error = Some(t("db.err_read_only").into()); false } else { true } }
    /// Production asks before every change; otherwise only deletes ask.
    fn request(&mut self, a: Ask, cx: &mut Context<Self>) {
        if !matches!(a, Ask::Aggregate(_)) && !self.writable() { cx.notify(); return; }
        if self.production || matches!(a, Ask::Delete(_)) { self.ask = Some(a); } else { self.perform(a, cx); }
        cx.notify();
    }
    fn perform(&mut self, a: Ask, cx: &mut Context<Self>) {
        let (db, coll) = (self.db.clone(), self.coll.clone());
        match a {
            Ask::Replace(id, json) => self.job(move |m| async move { m.replace(&db, &coll, &id, &json).await }, |v, r, cx| { v.logged("replaceOne", &r); match r { Ok(()) => { v.note = Some(t("redis.saved").into()); v.find(cx); } Err(e) => v.fail(e) } }, cx),
            Ask::Insert(json) => self.job(move |m| async move { m.insert(&db, &coll, &json).await }, |v, r, cx| { v.logged("insertOne", &r); match r { Ok(id) => { v.note = Some(tf("mongo.inserted", &[("id", &id.into_relaxed_extjson().to_string())])); v.find(cx); } Err(e) => v.fail(e) } }, cx),
            Ask::Delete(id) => self.job(move |m| async move { m.delete(&db, &coll, &id).await }, |v, r, cx| { v.logged("deleteOne", &r); match r { Ok(n) => { v.note = Some(tf("mongo.deleted", &[("n", &n)])); v.find(cx); } Err(e) => v.fail(e) } }, cx),
            Ask::Aggregate(p) => {
                let t0 = std::time::Instant::now();
                self.busy = true;
                self.job(move |m| async move { m.aggregate(&db, &coll, &p, AGG_LIMIT).await }, move |v, r, _| { v.busy = false; v.logged("aggregate", &r); match r { Ok(d) => { v.agg = d; v.agg_ms = Some(t0.elapsed().as_millis()); } Err(e) => v.fail(e) } }, cx);
            }
        }
    }
    fn save_doc(&mut self, cx: &mut Context<Self>) {
        let json = self.doc_ed.read(cx).value().to_string();
        self.error = None;
        match self.editing {
            Some(None) => self.request(Ask::Insert(json), cx),
            Some(Some(i)) => {
                let Some(id) = self.docs.get(i).and_then(|d| d.get("_id")).cloned() else { self.error = Some(t("mongo.err_no_id").into()); return };
                self.request(Ask::Replace(id, json), cx)
            }
            None => {}
        }
    }
    fn run_agg(&mut self, cx: &mut Context<Self>) {
        let p = self.pipe_ed.read(cx).value().to_string();
        self.error = None;
        let writes = doc::parse_pipeline(&p).map(|v| v.iter().any(|s| s.contains_key("$out") || s.contains_key("$merge"))).unwrap_or(false);
        if writes { self.request(Ask::Aggregate(p), cx); } else { self.perform(Ask::Aggregate(p), cx); }
    }
    fn export(&mut self, cx: &mut Context<Self>) {
        if self.coll.is_empty() { return; }
        let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")).map(std::path::PathBuf::from).unwrap_or_else(|| ".".into());
        let rx = cx.prompt_for_new_path(&home, Some(&format!("{}.json", self.coll)));
        let (db, coll, f, s, p) = (self.db.clone(), self.coll.clone(), Self::val(&self.filter_in, cx), Self::val(&self.sort_in, cx), Self::val(&self.proj_in, cx));
        let (rt, mongo) = (self.rt.clone(), self.mongo.clone());
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(path))) = rx.await else { return };
            let Some(m) = mongo else { return };
            let p2 = path.clone();
            let r = rt.spawn(async move { m.export(&db, &coll, &f, &s, &p, &p2).await }).await.map_err(anyhow::Error::from).and_then(|r| r);
            let _ = this.update(cx, |v, cx| { v.logged("export", &r); match r { Ok(n) => v.note = Some(tf("mongo.exported", &[("n", &n), ("path", &path.display())])), Err(e) => v.fail(e) } cx.notify(); });
        }).detach();
    }
    pub fn close(&mut self) { if let Some(m) = self.mongo.take() { self.rt.spawn(async move { m.close().await }); } }
}

impl Drop for DocView { fn drop(&mut self) { self.close(); } }


impl DocView {
    fn tree_el(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let th = cur();
        let mut list = div().id("mg-tree").flex_1().min_h_0().overflow_y_scroll().py_1().flex().flex_col();
        for (i, d) in self.dbs.clone().into_iter().enumerate() {
            let open = self.open.contains(&d);
            let d2 = d.clone();
            list = list.child(div().id(("mg-db", i)).h(px(24.)).px_2().flex().items_center().gap_1().cursor_pointer().hover(|x| x.bg(crate::theme::hover_bg()))
                .on_click(cx.listener(move |v, _, _w, cx| { if !v.open.remove(&d2) { v.open.insert(d2.clone()); if !v.colls.contains_key(&d2) { v.load_colls(d2.clone(), cx); } } cx.notify(); }))
                .child(icon(if open { IconName::ChevronDown } else { IconName::ChevronRight }, ui::IC_XS, c(th.overlay1)))
                .child(icon(IconName::Database, ui::IC_SM, c(th.accent)))
                .child(div().text_size(px(ui::fs_body())).text_color(c(th.text)).child(d.clone())));
            if !open { continue; }
            match self.colls.get(&d) {
                None => list = list.child(div().pl(px(34.)).child(small(t("db.loading")))),
                Some(cs) => for (j, co) in cs.iter().enumerate() {
                    let sel = self.db == d && &self.coll == co;
                    let (dd, cc) = (d.clone(), co.clone());
                    list = list.child(div().id(SharedString::from(format!("mg-c-{i}-{j}"))).h(px(24.)).pl(px(30.)).pr_2().flex().items_center().gap_1().cursor_pointer()
                        .when(sel, |x| x.bg(crate::theme::selected_bg())).hover(|x| x.bg(crate::theme::hover_bg()))
                        .on_click(cx.listener(move |v, _, _w, cx| { v.pick(dd.clone(), cc.clone(), cx); cx.notify(); }))
                        .child(icon(IconName::Folder, ui::IC_XS, c(th.overlay1)))
                        .child(div().whitespace_nowrap().overflow_hidden().text_ellipsis().text_size(px(ui::fs_body())).text_color(c(th.text)).child(co.clone())));
                },
            }
        }
        div().w(px(240.)).flex_shrink_0().h_full().flex().flex_col().border_r_1().border_color(crate::theme::divider())
            .child(div().p_2().flex().gap_1().child(ui::button("mg-refresh", t("redis.refresh"), 0).on_click(cx.listener(|v, _, _w, cx| { v.colls.clear(); v.load_dbs(cx); for d in v.open.clone() { v.load_colls(d, cx); } cx.notify(); }))))
            .child(list).into_any_element()
    }

    fn docs_el(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let th = cur();
        let lbl = |s: &str| div().w(px(70.)).flex_shrink_0().text_size(px(ui::fs_small())).text_color(c(th.subtext)).child(s.to_string());
        let bar = div().p_2().flex().flex_col().gap_1().border_b_1().border_color(crate::theme::divider())
            .child(div().flex().items_center().gap_2().child(lbl(&t("mongo.filter"))).child(div().flex_1().child(ui::text_field(&self.filter_in, false)))
                .child(ui::button("mg-find", t("mongo.find"), 1).on_click(cx.listener(|v, _, _w, cx| { v.skip = 0; v.find(cx); cx.notify(); }))))
            .child(div().flex().items_center().gap_2().child(lbl(&t("mongo.sort"))).child(div().flex_1().child(ui::text_field(&self.sort_in, false)))
                .child(lbl(&t("mongo.projection"))).child(div().flex_1().child(ui::text_field(&self.proj_in, false))));
        let total = self.total.unwrap_or(0);
        let page = format!("{}–{} / {}", if self.docs.is_empty() { 0 } else { self.skip + 1 }, self.skip + self.docs.len() as u64, total);
        let can_prev = self.skip > 0;
        let can_next = self.skip + (self.docs.len() as u64) < total;
        let pager = div().px_2().py_1().flex().items_center().gap_2().border_b_1().border_color(crate::theme::divider())
            .child(small(page)).child(div().flex_1())
            .child(ui::button("mg-prev", "‹", 0).when(!can_prev, |b| b.opacity(0.4)).on_click(cx.listener(move |v, _, _w, cx| { if can_prev { v.skip = v.skip.saturating_sub(doc::PAGE); v.find(cx); } cx.notify(); })))
            .child(ui::button("mg-next", "›", 0).when(!can_next, |b| b.opacity(0.4)).on_click(cx.listener(move |v, _, _w, cx| { if can_next { v.skip += doc::PAGE; v.find(cx); } cx.notify(); })))
            .child(ui::button("mg-new", t("mongo.new_doc"), 0).on_click(cx.listener(|v, _, _w, cx| { v.editing = Some(None); v.show = Show::Json; v.pending_doc = Some("{\n  \n}".into()); cx.notify(); })))
            .child(ui::button("mg-export", t("mongo.export"), 0).on_click(cx.listener(|v, _, _w, cx| { v.export(cx); cx.notify(); })));
        let mut list = div().id("mg-docs").flex_1().min_h_0().overflow_y_scroll().flex().flex_col();
        if self.docs.is_empty() { list = list.child(div().p_3().child(small(if self.busy { t("db.loading") } else { t("db.no_rows") }))); }
        for (i, d) in self.docs.iter().enumerate() {
            let sel = self.selected == Some(i);
            list = list.child(div().id(("mg-d", i)).min_h(px(40.)).px_2().py_1().flex().flex_col().border_b_1().border_color(crate::theme::divider()).cursor_pointer()
                .when(sel, |x| x.bg(crate::theme::selected_bg())).hover(|x| x.bg(crate::theme::hover_bg()))
                .on_click(cx.listener(move |v, _, _w, cx| { v.selected = Some(i); v.editing = None; v.sync_editor(); cx.notify(); }))
                .child(mono(doc::id_text(d)).text_color(c(th.accent)))
                .child(div().whitespace_nowrap().overflow_hidden().text_ellipsis().child(small(doc::preview(d, 300)))));
        }
        let left = div().w(px(380.)).flex_shrink_0().h_full().flex().flex_col().border_r_1().border_color(crate::theme::divider()).child(list);
        let right = self.doc_detail_el(cx);
        div().flex_1().min_h_0().flex().flex_col().child(bar).child(pager).child(div().flex_1().min_h_0().flex().child(left).child(right)).into_any_element()
    }

    fn doc_detail_el(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let th = cur();
        let editing = self.editing.is_some();
        let has = self.selected.is_some() || editing;
        if !has { return div().flex_1().flex().items_center().justify_center().child(small(t("mongo.pick_doc"))).into_any_element(); }
        let modes: Vec<(String, String)> = vec![("tree".into(), t("mongo.tree").into()), ("json".into(), "JSON".into())];
        let me = cx.entity().downgrade();
        let mut head = div().px_2().py_1().flex().items_center().gap_2().border_b_1().border_color(crate::theme::divider());
        if !editing {
            head = head.child(ui::segmented("mg-show", &modes, if self.show == Show::Tree { "tree" } else { "json" }, move |s, _w, cx| { let _ = me.update(cx, |v, cx| { v.show = if s == "tree" { Show::Tree } else { Show::Json }; cx.notify(); }); }))
                .child(div().flex_1())
                .child(ui::button("mg-edit", t("mongo.edit"), 0).on_click(cx.listener(|v, _, _w, cx| { if v.writable() { v.editing = Some(v.selected); v.show = Show::Json; v.sync_editor(); } cx.notify(); })))
                .child(ui::button("mg-del", t("ui.delete"), 2).on_click(cx.listener(|v, _, _w, cx| {
                    if let Some(id) = v.selected.and_then(|i| v.docs.get(i)).and_then(|d| d.get("_id")).cloned() { v.request(Ask::Delete(id), cx); }
                })));
        } else {
            head = head.child(small(if self.editing == Some(None) { t("mongo.new_doc") } else { t("mongo.editing") })).child(div().flex_1())
                .child(ui::button("mg-cancel", t("ui.cancel"), 0).on_click(cx.listener(|v, _, _w, cx| { v.editing = None; v.sync_editor(); cx.notify(); })))
                .child(ui::button("mg-save", t("ui.save"), 1).on_click(cx.listener(|v, _, _w, cx| { v.save_doc(cx); cx.notify(); })));
        }
        let body: AnyElement = if self.show == Show::Json || editing {
            div().flex_1().min_h_0().child(Editor::new(&self.doc_ed).h_full()).into_any_element()
        } else {
            let d = self.selected.and_then(|i| self.docs.get(i)).cloned().unwrap_or_default();
            let mut list = div().id("mg-tree-doc").flex_1().min_h_0().overflow_y_scroll().p_2().flex().flex_col();
            for (n, (path, depth, key, val, kids)) in tree_rows(&d, &self.collapsed).into_iter().enumerate() {
                let open = !self.collapsed.contains(&path);
                let p = path.clone();
                list = list.child(div().id(("mg-tr", n)).min_h(px(22.)).pl(px(4. + depth as f32 * 16.)).flex().items_center().gap_1().when(kids, |x| x.cursor_pointer())
                    .on_click(cx.listener(move |v, _, _w, cx| { if kids && !v.collapsed.remove(&p) { v.collapsed.insert(p.clone()); } cx.notify(); }))
                    .child(div().w(px(12.)).when(kids, |x| x.child(icon(if open { IconName::ChevronDown } else { IconName::ChevronRight }, ui::IC_XS, c(th.overlay1)))))
                    .child(mono(key).text_color(c(th.accent)))
                    .child(div().flex_1().min_w_0().whitespace_nowrap().overflow_hidden().text_ellipsis().child(mono(val).text_color(if kids { c(th.overlay0) } else { c(th.text) }))));
            }
            list.into_any_element()
        };
        div().flex_1().min_w_0().h_full().flex().flex_col().child(head).child(body).into_any_element()
    }

    fn indexes_el(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let mut list = div().id("mg-idx").flex_1().min_h_0().overflow_y_scroll().flex().flex_col();
        list = list.child(ui::table_header().px_3().gap_3()
            .child(div().w(px(220.)).child(t("ui.name"))).child(div().flex_1().child(t("mongo.keys"))).child(div().w(px(160.)).child(t("mongo.options"))));
        for ix in &self.indexes {
            let mut opts = vec![];
            if ix.unique { opts.push(t("mongo.unique").to_string()); }
            if let Some(s) = ix.ttl { opts.push(format!("TTL {s}s")); }
            list = list.child(div().min_h(px(28.)).px_3().flex().items_center().gap_3().border_b_1().border_color(crate::theme::divider())
                .child(div().w(px(220.)).child(mono(ix.name.clone()))).child(div().flex_1().min_w_0().child(mono(ix.keys.clone()))).child(div().w(px(160.)).child(small(opts.join(", ")))));
        }
        div().flex_1().min_h_0().flex().flex_col()
            .child(div().p_2().child(ui::button("mg-idx-r", t("redis.refresh"), 0).on_click(cx.listener(|v, _, _w, cx| { v.load_indexes(cx); cx.notify(); }))))
            .child(list).into_any_element()
    }

    fn agg_el(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let mut out = div().id("mg-agg").flex_1().min_h_0().overflow_y_scroll().p_2().flex().flex_col().gap_2().bg(crate::theme::sub_bg());
        for d in &self.agg { out = out.child(mono(doc::to_json(d)).whitespace_normal()); }
        let info = match self.agg_ms { Some(ms) => tf("mongo.agg_result", &[("n", &self.agg.len()), ("ms", &ms)]), None => t("mongo.agg_hint").to_string() };
        div().flex_1().min_h_0().flex().flex_col()
            .child(div().h(px(200.)).flex_shrink_0().child(Editor::new(&self.pipe_ed).h_full()))
            .child(div().p_2().flex().items_center().gap_2().border_y_1().border_color(crate::theme::divider())
                .child(ui::button("mg-agg-run", if self.busy { t("db.loading") } else { t("mongo.run") }, 1).on_click(cx.listener(|v, _, _w, cx| { v.run_agg(cx); cx.notify(); })))
                .child(small(info)))
            .child(out).into_any_element()
    }

    fn ask_el(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let ask = self.ask.as_ref()?;
        let th = cur();
        let (msg, pre) = match ask {
            Ask::Replace(id, j) => (tf("mongo.confirm_replace", &[("id", &id.clone().into_relaxed_extjson().to_string())]), j.clone()),
            Ask::Insert(j) => (t("mongo.confirm_insert").to_string(), j.clone()),
            Ask::Delete(id) => (tf("mongo.confirm_delete", &[("id", &id.clone().into_relaxed_extjson().to_string())]), String::new()),
            Ask::Aggregate(p) => (t("mongo.confirm_agg").to_string(), p.clone()),
        };
        let pre: String = pre.chars().take(4000).collect();
        let prod = self.production.then(|| ui::tag(t("tag.production"), c(th.error)));
        Some(ui::modal(ui::card(520.)
            .child(ui::dialog_header(Some(IconName::TriangleAlert), c(cur().warning), t("db.confirm_title"), None, None).children(prod))
            .child(div().p(px(ui::SP_5)).flex().flex_col().gap(px(ui::SP_3))
                .child(small(msg))
                .when(!pre.is_empty(), |d| d.child(div().id("mg-cf-pre").max_h(px(300.)).overflow_y_scroll().p_2().rounded(px(ui::R_MD)).bg(crate::theme::sub_bg()).child(mono(pre).whitespace_normal()))))
            .child(ui::dialog_footer(None, vec![])
                .child(ui::button("mg-cf-no", t("ui.cancel"), 0).on_click(cx.listener(|v, _, _w, cx| { v.ask = None; cx.notify(); })))
                .child(ui::button("mg-cf-yes", t("db.confirm_run"), 2).on_click(cx.listener(|v, _, _w, cx| { if let Some(a) = v.ask.take() { v.perform(a, cx); } cx.notify(); }))))).into_any_element())
    }
}

impl Render for DocView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let th = cur();
        if let Some(s) = self.pending_doc.take() { self.doc_ed.update(cx, |e, cx| e.set_value(s, window, cx)); }
        let mut head = ui::toolbar()
            .child(icon(IconName::Database, ui::IC_SM, c(th.success)))
            .child(div().text_size(px(ui::fs_body())).font_weight(FontWeight::SEMIBOLD).text_color(c(th.text)).child(self.crumbs()))
            .child(small(self.mongo.as_ref().map(|m| format!("MongoDB {}{}", m.version, m.via.as_ref().map(|v| format!(" ⇢ {v}")).unwrap_or_default())).unwrap_or_default()));
        if self.production { head = head.child(ui::tag(t("tag.production"), c(th.error))); }
        if self.conn.read_only { head = head.child(ui::tag(t("db.read_only"), c(th.warning))); }
        head = head.child(div().flex_1());
        let subs: Vec<(String, String)> = vec![("d".into(), t("mongo.documents").into()), ("i".into(), t("mongo.indexes").into()), ("a".into(), t("mongo.aggregate").into())];
        let cur_sub = match self.sub { Sub::Docs => "d", Sub::Indexes => "i", Sub::Aggregate => "a" };
        let me = cx.entity().downgrade();
        head = head.child(ui::segmented("mg-sub", &subs, cur_sub, move |s, _w, cx| {
            let sub = match s.as_str() { "i" => Sub::Indexes, "a" => Sub::Aggregate, _ => Sub::Docs };
            let _ = me.update(cx, |v, cx| { v.sub = sub; if sub == Sub::Indexes { v.load_indexes(cx); } cx.notify(); });
        }));
        let body: AnyElement = if let Some(e) = &self.conn_err {
            div().flex_1().p_5().flex().flex_col().gap_2().child(ui::notice(ui::Notice::Error, t("ui.connection_failed")).mx_0()).child(mono(e.clone()).whitespace_normal()).into_any_element()
        } else if self.mongo.is_none() {
            div().flex_1().flex().items_center().justify_center().child(small(t("ui.connecting"))).into_any_element()
        } else {
            let tree = self.tree_el(cx);
            let main: AnyElement = if self.coll.is_empty() { div().flex_1().flex().items_center().justify_center().child(small(t("mongo.pick_coll"))).into_any_element() } else {
                match self.sub { Sub::Docs => self.docs_el(cx), Sub::Indexes => self.indexes_el(cx), Sub::Aggregate => self.agg_el(cx) }
            };
            div().flex_1().min_h_0().flex().child(tree).child(div().flex_1().min_w_0().flex().flex_col().child(main)).into_any_element()
        };
        let err = self.error.clone().map(|e| ui::notice(ui::Notice::Error, e).mt(px(ui::SP_2)));
        let note = self.note.clone().map(|n| div().mx_3().mt_1().text_size(px(ui::fs_body())).text_color(c(th.success)).child(n));
        let ask = self.ask_el(cx);
        div().size_full().flex().flex_col().relative().bg(c(th.base)).child(head).children(err).children(note).child(body).children(ask)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn doc_tree_rows() {
        let d = doc::parse_doc(r#"{"a":1,"b":{"c":"x","d":[1,2]}}"#).unwrap();
        let r = tree_rows(&d, &HashSet::new());
        assert_eq!(r.iter().map(|x| (x.0.as_str(), x.1)).collect::<Vec<_>>(), [("a", 0), ("b", 1 - 1), ("b.c", 1), ("b.d", 1), ("b.d.0", 2), ("b.d.1", 2)]);
        assert_eq!(r[2].3, "\"x\"");
        let closed: HashSet<String> = ["b".to_string()].into();
        assert_eq!(tree_rows(&d, &closed).len(), 2);
    }
}
