//! Database tab content (v0.2 PRD §6.4/§6.5): a table browser with server-side paging,
//! filtering, sorting and staged editing (plus structure and DDL views), or a SQL editor with
//! completion, highlighting, history and result tabs. Lives inside a layout leaf
//! (`TermView.db`) so tabs, splits and layout restore work as for terminals.
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::rc::Rc;
use std::sync::Arc;
use std::time::Instant;

use gpui::{div, prelude::*, px, App, Context, Entity, FontWeight, SharedString, WeakEntity, Window};
use gpui_component::input::{Editor, EditorState, InputEvent, InputState};
use gpui_component::table::{Column, ColumnSort, DataTable, TableDelegate, TableEvent, TableState};
use ssh_term_proto::db::{self, dialect, edit, export, history, live::Live, split, CancelSlot, ColumnInfo, ResultSet, RunOpts};
use ssh_term_proto::i18n::{t, tf};
use ssh_term_proto::session_store::PaneSave;

use crate::activity::{log_push, ExecLog};
use crate::theme::{c, cur};
use crate::ui::{self, icon, IconName};

/// Rows/columns behind a grid. `owner` pages and sorts through the view.
pub struct Grid {
    cols: Vec<ColumnInfo>,
    widths: Vec<f32>,
    rows: Vec<Vec<Option<String>>>,
    more: bool,
    busy: bool,
    server_sort: bool,
    sort: Option<(usize, bool)>,
    owner: WeakEntity<DbView>,
    /// Staged edits of loaded rows: (row, column) -> new value.
    edits: HashMap<(usize, usize), Option<String>>,
    /// Loaded rows marked for deletion.
    deleted: BTreeSet<usize>,
    /// New rows (shown after the loaded ones): column -> value for the cells the user set.
    added: Vec<HashMap<usize, Option<String>>>,
}

#[derive(Clone, Copy, PartialEq)]
enum Mark { Plain, Edited, Added, Default, Deleted }

impl Grid {
    fn new(owner: WeakEntity<DbView>, server_sort: bool) -> Self {
        Self { cols: vec![], widths: vec![], rows: vec![], more: false, busy: false, server_sort, sort: None, owner, edits: HashMap::new(), deleted: BTreeSet::new(), added: vec![] }
    }
    /// Value shown at (row, col) with its staging state.
    fn shown(&self, row: usize, col: usize) -> (Option<String>, Mark) {
        let n = self.rows.len();
        if row >= n {
            return match self.added.get(row - n).and_then(|m| m.get(&col)) { Some(v) => (v.clone(), Mark::Added), None => (None, Mark::Default) };
        }
        let orig = self.rows.get(row).and_then(|r| r.get(col)).cloned().flatten();
        if self.deleted.contains(&row) { return (orig, Mark::Deleted); }
        match self.edits.get(&(row, col)) { Some(v) => (v.clone(), Mark::Edited), None => (orig, Mark::Plain) }
    }
    fn pending(&self) -> usize { self.edits.keys().map(|k| k.0).collect::<BTreeSet<_>>().len() + self.deleted.len() + self.added.len() }
    fn clear_staged(&mut self) { self.edits.clear(); self.deleted.clear(); self.added.clear(); }
    fn set(&mut self, cols: Vec<ColumnInfo>, rows: Vec<Vec<Option<String>>>) {
        self.widths = col_widths(&cols, &rows);
        self.cols = cols;
        self.rows = rows;
    }
}

/// Width per column from the header and the first rows: ~7px per char, 72..360.
fn col_widths(cols: &[ColumnInfo], rows: &[Vec<Option<String>>]) -> Vec<f32> {
    cols.iter().enumerate().map(|(i, col)| {
        let data = rows.iter().take(100).map(|r| r.get(i).and_then(|v| v.as_deref()).map_or(4, |s| s.chars().take(60).count())).max().unwrap_or(0);
        let chars = data.max(col.name.chars().count() + 2);
        (chars as f32 * 7.0 + 24.0).clamp(72.0, 360.0)
    }).collect()
}

/// One-line cell text: NULL marker, newlines/tabs flattened, long values cut.
fn cell_display(v: Option<&str>) -> String {
    match v {
        None => "NULL".into(),
        Some(s) => { let mut o: String = s.chars().take(500).map(|ch| if ch == '\n' || ch == '\r' || ch == '\t' { ' ' } else { ch }).collect(); if s.chars().count() > 500 { o.push('…'); } o }
    }
}

impl TableDelegate for Grid {
    fn columns_count(&self, _: &App) -> usize { self.cols.len() }
    fn rows_count(&self, _: &App) -> usize { self.rows.len() + self.added.len() }
    fn column(&self, i: usize, _: &App) -> Column {
        let ci = &self.cols[i];
        let mut col = Column::new(SharedString::from(format!("c{i}")), SharedString::from(ci.name.clone()))
            .width(px(self.widths.get(i).copied().unwrap_or(120.))).resizable(true).sortable();
        if let Some((s, desc)) = self.sort { if s == i { col = if desc { col.descending() } else { col.ascending() }; } }
        col
    }
    fn perform_sort(&mut self, col: usize, sort: ColumnSort, _w: &mut Window, cx: &mut Context<TableState<Self>>) {
        self.sort = match sort { ColumnSort::Ascending => Some((col, false)), ColumnSort::Descending => Some((col, true)), ColumnSort::Default => None };
        if self.server_sort {
            let (o, s) = (self.owner.clone(), self.sort);
            cx.spawn(async move |_, cx| { let _ = o.update(cx, |v, cx| v.sort_changed(s, cx)); }).detach();
        } else {
            // ad-hoc result: sort what is loaded, NULLs first, numbers numerically
            let key = |v: &Option<String>| v.as_deref().map(|s| (s.parse::<f64>().ok(), s.to_string()));
            match self.sort {
                Some((i, desc)) => self.rows.sort_by(|a, b| { let o = key(&a[i]).partial_cmp(&key(&b[i])).unwrap_or(std::cmp::Ordering::Equal); if desc { o.reverse() } else { o } }),
                None => {}
            }
        }
    }
    fn render_td(&mut self, row: usize, col: usize, _w: &mut Window, _cx: &mut Context<TableState<Self>>) -> impl IntoElement {
        let th = cur();
        let (v, mark) = self.shown(row, col);
        let text = if mark == Mark::Default { t("db.default_value").to_string() } else { cell_display(v.as_deref()) };
        div().w_full().h_full().flex().items_center().whitespace_nowrap().overflow_hidden().text_ellipsis().text_size(px(12.))
            .when(v.is_none(), |d| d.italic().text_color(c(th.overlay0)))
            .when(mark == Mark::Edited, |d| d.bg(c(th.warning).opacity(0.22)))
            .when(mark == Mark::Added || mark == Mark::Default, |d| d.bg(c(th.success).opacity(0.14)))
            .when(mark == Mark::Deleted, |d| d.line_through().text_color(c(th.error)).bg(c(th.error).opacity(0.10)))
            .child(text)
    }
    fn cell_text(&self, row: usize, col: usize, _: &App) -> String {
        self.shown(row, col).0.unwrap_or_else(|| "NULL".into())
    }
    fn render_empty(&mut self, _w: &mut Window, _cx: &mut Context<TableState<Self>>) -> impl IntoElement {
        div().size_full().flex().items_center().justify_center().text_size(px(12.)).text_color(c(cur().overlay0)).child(if self.busy { t("db.loading") } else { t("db.no_rows") })
    }
    fn loading(&self, _: &App) -> bool { self.busy && self.rows.is_empty() }
    fn has_more(&self, _: &App) -> bool { self.more && !self.busy }
    fn load_more(&mut self, _w: &mut Window, cx: &mut Context<TableState<Self>>) {
        self.busy = true;
        let o = self.owner.clone();
        cx.spawn(async move |_, cx| { let _ = o.update(cx, |v, cx| v.load_page(false, cx)); }).detach();
    }
}

#[derive(Clone, Copy, PartialEq)]
pub enum Sub { Data, Structure, Ddl }

#[derive(Clone, Copy, PartialEq)]
enum Pop { Export, History, Saved }

/// Something waiting for the user's OK.
enum Ask {
    /// User SQL with a dangerous / production write; `stmt` is the one shown.
    Run { sql: String, stmt: String },
    /// Staged grid changes.
    Commit(Vec<String>),
    /// Rows from a file.
    Import { stmts: Vec<String>, rows: usize, file: String },
}

#[derive(Clone)]
pub struct EditorResize;
impl Render for EditorResize { fn render(&mut self, _w: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement { div() } }

const EDITOR_H: f32 = 220.;

/// What a pane shows.
#[derive(Clone, Debug, PartialEq)]
pub struct Target { pub conn: String, pub database: String, pub schema: Option<String>, pub table: Option<String> }

pub struct DbView {
    pub target: Target,
    pub title: String,
    conn_name: String,
    read_only: bool,
    production: bool,
    live: Option<Arc<Live>>,
    conn_err: Option<String>,
    rt: tokio::runtime::Handle,
    log: ExecLog,
    grid: Entity<TableState<Grid>>,
    // table mode
    where_in: Entity<InputState>,
    applied_where: String,
    total: Option<u64>,
    sub: Sub,
    /// Columns with key info (table mode), for editing and the structure view.
    table_cols: Option<Result<Vec<ColumnInfo>, String>>,
    ddl: Option<Result<String, String>>,
    ddl_ed: Entity<EditorState>,
    /// DDL text waiting to be put into `ddl_ed` (needs a window, so done at render).
    pending_ddl: Option<String>,
    cell_in: Entity<InputState>,
    sel: Option<(usize, usize)>,
    edit_note: Option<String>,
    // query mode
    editor: Entity<EditorState>,
    editor_h: f32,
    comp: crate::db_editor::SharedComp,
    hist: history::Store,
    hist_in: Entity<InputState>,
    save_in: Entity<InputState>,
    results: Vec<ResultSet>,
    result_ix: usize,
    pop: Option<Pop>,
    ask: Option<Ask>,
    status: String,
    error: Option<String>,
    busy: bool,
    /// Bumped per request; late answers of an older request are dropped.
    gen: u64,
    /// The running query's cancel handle (query mode).
    slot: Option<Arc<CancelSlot>>,
    _subs: Vec<gpui::Subscription>,
}

impl DbView {
    pub fn new(target: Target, sql: String, rt: tokio::runtime::Handle, log: ExecLog, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let me = cx.entity().downgrade();
        let table_mode = target.table.is_some();
        let grid = cx.new(|cx| TableState::new(Grid::new(me.clone(), table_mode), window, cx).col_movable(false).cell_selectable(table_mode));
        let where_in = cx.new(|cx| InputState::new(window, cx).placeholder(t("db.where_placeholder")));
        let comp: crate::db_editor::SharedComp = Default::default();
        let editor = cx.new(|cx| {
            let mut e = EditorState::new(window, cx).language("sql").line_number(true).placeholder(t("db.sql_placeholder")).default_value(sql);
            crate::db_editor::install_highlighter(&mut e, cx);
            e.lsp_mut().completion_provider = Some(Rc::new(crate::db_editor::SqlCompletion(comp.clone())));
            e
        });
        let ddl_ed = cx.new(|cx| { let mut e = EditorState::new(window, cx).language("sql").line_number(true); crate::db_editor::install_highlighter(&mut e, cx); e });
        let cell_in = cx.new(|cx| InputState::new(window, cx).placeholder("NULL"));
        let hist_in = cx.new(|cx| InputState::new(window, cx).placeholder(t("db.history_search")));
        let save_in = cx.new(|cx| InputState::new(window, cx).placeholder(t("db.saved_name")));
        let mut subs = vec![];
        subs.push(cx.subscribe_in(&where_in, window, |this: &mut Self, _, ev: &InputEvent, _w, cx| { if let InputEvent::PressEnter { .. } = ev { this.apply_where(cx); } }));
        subs.push(cx.subscribe_in(&cell_in, window, |this: &mut Self, st, ev: &InputEvent, _w, cx| { if let InputEvent::PressEnter { .. } = ev { let v = st.read(cx).value().to_string(); this.stage(Some(v), cx); } }));
        subs.push(cx.subscribe_in(&save_in, window, |this: &mut Self, _, ev: &InputEvent, w, cx| { if let InputEvent::PressEnter { .. } = ev { this.save_query(w, cx); } }));
        subs.push(cx.subscribe_in(&hist_in, window, |_this: &mut Self, _, ev: &InputEvent, _w, cx| { if let InputEvent::Change = ev { cx.notify(); } }));
        subs.push(cx.subscribe_in(&grid, window, |this: &mut Self, _, ev: &TableEvent, w, cx| match ev {
            TableEvent::SelectCell(r, col) => this.select_cell(*r, *col, w, cx),
            TableEvent::DoubleClickedCell(r, col) => { this.select_cell(*r, *col, w, cx); this.cell_in.update(cx, |s, cx| s.focus(w, cx)); }
            _ => {}
        }));
        let (conn_name, read_only, production) = conn_meta(&target.conn);
        let mut me = Self { title: String::new(), target, conn_name, read_only, production, live: None, conn_err: None, rt, log, grid, where_in, applied_where: String::new(), total: None,
            sub: Sub::Data, table_cols: None, ddl: None, ddl_ed, pending_ddl: None, cell_in, sel: None, edit_note: None,
            editor, editor_h: EDITOR_H, comp, hist: history::Store::load(), hist_in, save_in, results: vec![], result_ix: 0, pop: None, ask: None,
            status: String::new(), error: None, busy: false, gen: 0, slot: None, _subs: subs };
        me.title = me.make_title();
        me
    }

    fn make_title(&self) -> String {
        match &self.target.table {
            Some(tb) => tb.clone(),
            None => format!("{} · {}", t("db.query"), if self.target.database.is_empty() { self.conn_name.clone() } else { self.target.database.clone() }),
        }
    }
    pub fn table_mode(&self) -> bool { self.target.table.is_some() }

    /// Breadcrumb for the status bar: connection › database › schema › table.
    pub fn crumbs(&self) -> String {
        let mut v = vec![self.conn_name.clone()];
        if !self.target.database.is_empty() { v.push(self.target.database.clone()); }
        if let Some(s) = &self.target.schema { v.push(s.clone()); }
        if let Some(tb) = &self.target.table { v.push(tb.clone()); }
        v.join(" › ")
    }

    pub fn save(&self, cx: &App) -> PaneSave {
        let sql = if self.table_mode() { String::new() } else { self.editor.read(cx).value().to_string() };
        PaneSave::Db { conn: self.target.conn.clone(), database: self.target.database.clone(), schema: self.target.schema.clone(), table: self.target.table.clone(), sql }
    }

    /// Connection ready (or failed). Table mode loads the first page right away.
    pub fn set_live(&mut self, live: Result<Arc<Live>, String>, cx: &mut Context<Self>) {
        let (n, ro, prod) = conn_meta(&self.target.conn);
        (self.conn_name, self.read_only, self.production) = (n, ro, prod);
        match live {
            Ok(l) => {
                let fresh = self.live.as_ref().is_none_or(|o| !Arc::ptr_eq(o, &l));
                self.live = Some(l.clone());
                self.conn_err = None;
                if fresh {
                    {
                        let mut cc = self.comp.borrow_mut();
                        cc.live = Some(l.clone());
                        cc.rt = Some(self.rt.clone());
                        cc.database = self.target.database.clone();
                        cc.schema = self.target.schema.clone();
                        cc.asked.clear();
                        cc.cache = Default::default();
                    }
                    if self.table_mode() { self.table_cols = None; self.ddl = None; self.load_meta(cx); self.load_page(true, cx); } else { self.load_tables(cx); }
                }
            }
            Err(e) => { self.live = None; self.conn_err = Some(e); self.busy = false; }
        }
        cx.notify();
    }

    // ---- table mode
    fn apply_where(&mut self, cx: &mut Context<Self>) {
        self.applied_where = self.where_in.read(cx).value().trim().to_string();
        self.load_page(true, cx);
    }
    fn sort_changed(&mut self, _s: Option<(usize, bool)>, cx: &mut Context<Self>) { self.load_page(true, cx); }

    /// Load the first page (`reset`) or the next one.
    pub fn load_page(&mut self, reset: bool, cx: &mut Context<Self>) {
        let (Some(live), Some(table)) = (self.live.clone(), self.target.table.clone()) else { return };
        if self.busy && !reset { return; }
        self.gen += 1;
        let gen = self.gen;
        let (offset, order) = self.grid.update(cx, |s, cx| {
            let d = s.delegate_mut();
            d.busy = true;
            if reset { d.rows.clear(); d.more = false; d.clear_staged(); }
            let order = d.sort.and_then(|(i, desc)| d.cols.get(i).map(|c| (c.name.clone(), desc)));
            let n = d.rows.len();
            cx.notify();
            (n, order)
        });
        if reset { self.total = None; self.sel = None; self.edit_note = None; self.grid.update(cx, |s, cx| s.scroll_to_row(0, cx)); }
        self.busy = true;
        self.error = None;
        let (db, schema, where_) = (self.target.database.clone(), self.target.schema.clone(), self.applied_where.clone());
        let started = Instant::now();
        let h = self.rt.spawn(async move { live.driver.page(&db, schema.as_deref(), &table, &where_, order.as_ref().map(|(c, d)| (c.as_str(), *d)), offset).await });
        cx.spawn(async move |this, cx| {
            let r = h.await.map_err(anyhow::Error::from).and_then(|r| r);
            let _ = this.update(cx, |v, cx| {
                if v.gen != gen { return; }
                v.busy = false;
                let ms = started.elapsed().as_millis();
                match r {
                    Ok((rs, total)) => {
                        if let Some(n) = total { v.total = Some(n); }
                        let got = rs.rows.len();
                        v.grid.update(cx, |s, cx| {
                            let d = s.delegate_mut();
                            if offset == 0 { d.set(rs.columns, rs.rows); } else { d.rows.extend(rs.rows); }
                            d.more = got >= db::PAGE;
                            d.busy = false;
                            s.refresh(cx);
                        });
                        let loaded = v.grid.read(cx).delegate().rows.len();
                        v.status = match v.total { Some(n) => tf("db.loaded_of", &[("n", &loaded), ("total", &n), ("ms", &ms)]), None => tf("db.loaded", &[("n", &loaded), ("ms", &ms)]) };
                    }
                    Err(e) => {
                        v.grid.update(cx, |s, cx| { s.delegate_mut().busy = false; s.delegate_mut().more = false; s.refresh(cx); });
                        v.error = Some(format!("{e:#}"));
                        log_push(&v.log, &v.conn_name, &format!("SELECT … {}", v.target.table.clone().unwrap_or_default()), false, format!("{e:#}"));
                    }
                }
                cx.notify();
            });
        }).detach();
        cx.notify();
    }

    // ---- query mode
    /// The SQL to run: the selection, else the statement under the cursor (`all` = whole text).
    fn pick_sql(&self, all: bool, cx: &App) -> String {
        let ed = self.editor.read(cx);
        let text = ed.value().to_string();
        let sel = ed.selected_range();
        if !all && sel.start < sel.end { if let Some(s) = text.get(sel.clone()) { return s.to_string(); } }
        if all { return text; }
        split::at_cursor(&text, ed.cursor()).map(|s| s.text).unwrap_or(text)
    }

    pub fn run_sql(&mut self, all: bool, cx: &mut Context<Self>) {
        let sql = self.pick_sql(all, cx);
        if sql.trim().is_empty() { return; }
        let stmts = split::split(&sql);
        let danger = stmts.iter().find(|s| split::classify(&s.text) == split::Class::Dangerous);
        let write = stmts.iter().find(|s| split::classify(&s.text) != split::Class::Read);
        let ask = match (danger, write) { (Some(s), _) => Some(s.text.clone()), (None, Some(s)) if self.production => Some(s.text.clone()), _ => None };
        if let (Some(stmt), false) = (ask, self.read_only) { self.ask = Some(Ask::Run { sql, stmt }); cx.notify(); return; }
        self.execute(sql, cx);
    }

    fn execute(&mut self, sql: String, cx: &mut Context<Self>) {
        let Some(live) = self.live.clone() else { self.error = Some(self.conn_err.clone().unwrap_or_else(|| t("db.not_connected").into())); cx.notify(); return };
        self.ask = None;
        self.gen += 1;
        let gen = self.gen;
        self.busy = true;
        self.error = None;
        self.status = t("db.running").into();
        let (db, ro) = (self.target.database.clone(), self.read_only);
        let started = Instant::now();
        let s2 = sql.clone();
        let slot = Arc::new(CancelSlot::default());
        self.slot = Some(slot.clone());
        let opts = RunOpts { schema: self.target.schema.clone(), transaction: false, cancel: Some(slot) };
        let h = self.rt.spawn(async move { live.driver.execute_with(&db, &s2, ro, &opts).await });
        cx.spawn(async move |this, cx| {
            let r = h.await.map_err(anyhow::Error::from).and_then(|r| r);
            let _ = this.update(cx, |v, cx| {
                if v.gen != gen { return; }
                v.busy = false;
                v.slot = None;
                let ms = started.elapsed().as_millis();
                match r {
                    Ok(sets) => {
                        for rs in &sets {
                            let detail = match (&rs.error, rs.returns_rows) { (Some(e), _) => e.clone(), (None, true) => tf("db.rows_ms", &[("n", &rs.rows.len()), ("ms", &rs.elapsed.as_millis())]), (None, false) => tf("db.affected_ms", &[("n", &rs.affected), ("ms", &rs.elapsed.as_millis())]) };
                            log_push(&v.log, &v.conn_name, &one_line(&rs.sql), rs.error.is_none(), detail);
                        }
                        let failed = sets.iter().find_map(|r| r.error.clone());
                        v.record(&sql, failed.is_none(), ms as u64);
                        v.status = tf("db.done_n", &[("n", &sets.len()), ("ms", &ms)]);
                        v.error = failed;
                        // first failing set, else the last one that returned rows, else the last
                        v.result_ix = sets.iter().position(|r| r.error.is_some()).or_else(|| sets.iter().rposition(|r| r.returns_rows)).unwrap_or(sets.len().saturating_sub(1));
                        v.results = sets;
                        v.show_result(cx);
                    }
                    Err(e) => {
                        v.record(&sql, false, ms as u64);
                        log_push(&v.log, &v.conn_name, &one_line(&sql), false, format!("{e:#}"));
                        v.error = Some(format!("{e:#}"));
                        v.status.clear();
                    }
                }
                cx.notify();
            });
        }).detach();
        cx.notify();
    }

    fn show_result(&mut self, cx: &mut Context<Self>) {
        let rs = self.results.get(self.result_ix).cloned().unwrap_or_default();
        self.grid.update(cx, |s, cx| { let d = s.delegate_mut(); d.sort = None; d.more = false; d.busy = false; d.set(rs.columns, rs.rows); s.refresh(cx); });
    }

    /// Stop the running query on the server; the request then ends with a cancelled error.
    /// When nothing could be stopped (no session yet, or the cancel failed) the UI just stops
    /// waiting.
    fn cancel(&mut self, cx: &mut Context<Self>) {
        if let (Some(slot), Some(live)) = (self.slot.clone(), self.live.clone()) {
            self.status = t("db.cancelling").into();
            let db = self.target.database.clone();
            let gen = self.gen;
            let h = self.rt.spawn(async move { live.driver.cancel(&db, &slot).await });
            cx.spawn(async move |this, cx| {
                let ok = matches!(h.await, Ok(Ok(true)));
                if !ok { let _ = this.update(cx, |v, cx| if v.gen == gen && v.busy { v.drop_wait(cx) }); }
            }).detach();
            cx.notify();
            return;
        }
        self.drop_wait(cx);
    }

    fn drop_wait(&mut self, cx: &mut Context<Self>) {
        self.slot = None;
        self.gen += 1;
        self.busy = false;
        self.status = t("db.cancelled").into();
        self.grid.update(cx, |s, cx| { s.delegate_mut().busy = false; s.refresh(cx); });
        cx.notify();
    }

    // ---- table metadata
    fn load_meta(&mut self, cx: &mut Context<Self>) {
        let (Some(live), Some(table)) = (self.live.clone(), self.target.table.clone()) else { return };
        let (db, schema) = (self.target.database.clone(), self.target.schema.clone());
        let h = self.rt.spawn(async move { live.driver.columns(&db, schema.as_deref(), &table).await });
        cx.spawn(async move |this, cx| {
            let r = h.await.map_err(anyhow::Error::from).and_then(|r| r);
            let _ = this.update(cx, |v, cx| { v.table_cols = Some(r.map_err(|e| format!("{e:#}"))); cx.notify(); });
        }).detach();
    }

    fn load_ddl(&mut self, cx: &mut Context<Self>) {
        let (Some(live), Some(table)) = (self.live.clone(), self.target.table.clone()) else { return };
        let (db, schema) = (self.target.database.clone(), self.target.schema.clone());
        self.ddl = Some(Ok(String::new()));
        let h = self.rt.spawn(async move { live.driver.ddl(&db, schema.as_deref(), &table).await });
        cx.spawn(async move |this, cx| {
            let r = h.await.map_err(anyhow::Error::from).and_then(|r| r);
            let _ = this.update(cx, |v, cx| {
                if let Ok(s) = &r { v.pending_ddl = Some(s.clone()); }
                v.ddl = Some(r.map_err(|e| format!("{e:#}")));
                cx.notify();
            });
        }).detach();
    }

    /// Table names for completion (query mode).
    fn load_tables(&mut self, cx: &mut Context<Self>) {
        let Some(live) = self.live.clone() else { return };
        let (db, schema) = (self.target.database.clone(), self.target.schema.clone());
        let h = self.rt.spawn(async move { live.driver.tables(&db, schema.as_deref()).await });
        let comp = self.comp.clone();
        cx.spawn(async move |_, _cx| {
            if let Ok(Ok(v)) = h.await { comp.borrow_mut().cache.tables = v.into_iter().map(|t| t.name).collect(); }
        }).detach();
    }

    fn set_sub(&mut self, sub: Sub, cx: &mut Context<Self>) {
        self.sub = sub;
        if sub == Sub::Ddl && self.ddl.as_ref().is_none_or(|d| d.is_err()) { self.load_ddl(cx); }
        if sub == Sub::Structure && self.table_cols.as_ref().is_none_or(|d| d.is_err()) { self.load_meta(cx); }
        cx.notify();
    }

    // ---- staged editing
    /// Why the grid cannot be edited, if it cannot.
    fn edit_block(&self, cx: &App) -> Option<String> {
        if self.read_only { return Some(t("db.edit_read_only").into()); }
        let cols = match &self.table_cols { None => return Some(t("db.loading").into()), Some(Err(e)) => return Some(e.clone()), Some(Ok(c)) => c };
        if self.pk_idx(cols, cx).is_none() { return Some(t("db.edit_no_pk").into()); }
        None
    }

    /// Grid column indexes of the primary key (matched by name).
    fn pk_idx(&self, cols: &[ColumnInfo], cx: &App) -> Option<Vec<usize>> {
        let g = &self.grid.read(cx).delegate().cols;
        let names: Vec<&str> = cols.iter().filter(|c| c.primary).map(|c| c.name.as_str()).collect();
        if names.is_empty() { return None; }
        names.iter().map(|n| g.iter().position(|c| &c.name == n)).collect()
    }

    fn select_cell(&mut self, r: usize, col: usize, w: &mut Window, cx: &mut Context<Self>) {
        if !self.table_mode() { return; }
        self.sel = Some((r, col));
        let v = self.grid.read(cx).delegate().shown(r, col).0;
        self.cell_in.update(cx, |s, cx| s.set_value(v.unwrap_or_default(), w, cx));
        cx.notify();
    }

    fn stage(&mut self, v: Option<String>, cx: &mut Context<Self>) {
        if let Some(why) = self.edit_block(cx) { self.edit_note = Some(why); cx.notify(); return; }
        let Some((r, col)) = self.sel else { return };
        let name = self.grid.read(cx).delegate().cols.get(col).map(|c| c.name.clone()).unwrap_or_default();
        let ty = self.table_cols.as_ref().and_then(|c| c.as_ref().ok()).and_then(|c| c.iter().find(|x| x.name == name)).map(|c| c.type_name.clone()).unwrap_or_default();
        if !edit::editable_type(&ty) { self.edit_note = Some(t("db.edit_binary").into()); cx.notify(); return; }
        self.edit_note = None;
        self.grid.update(cx, |s, cx| {
            let d = s.delegate_mut();
            let n = d.rows.len();
            if r >= n { if let Some(m) = d.added.get_mut(r - n) { m.insert(col, v); } }
            else if !d.deleted.contains(&r) {
                let orig = d.rows.get(r).and_then(|x| x.get(col)).cloned().flatten();
                if orig == v { d.edits.remove(&(r, col)); } else { d.edits.insert((r, col), v); }
            }
            s.refresh(cx);
        });
        cx.notify();
    }

    fn add_row(&mut self, cx: &mut Context<Self>) {
        if let Some(why) = self.edit_block(cx) { self.edit_note = Some(why); cx.notify(); return; }
        let r = self.grid.update(cx, |s, cx| { let d = s.delegate_mut(); d.added.push(HashMap::new()); let r = d.rows.len() + d.added.len() - 1; s.refresh(cx); r });
        self.grid.update(cx, |s, cx| { s.scroll_to_row(r, cx); s.set_selected_cell(r, 0, cx); });
        self.sel = Some((r, 0));
        cx.notify();
    }

    /// Mark the selected row deleted (a new row is just dropped); again to unmark.
    fn delete_row(&mut self, cx: &mut Context<Self>) {
        if let Some(why) = self.edit_block(cx) { self.edit_note = Some(why); cx.notify(); return; }
        let Some((r, _)) = self.sel else { return };
        self.grid.update(cx, |s, cx| {
            let d = s.delegate_mut();
            let n = d.rows.len();
            if r >= n { if r - n < d.added.len() { d.added.remove(r - n); } }
            else if !d.deleted.remove(&r) { d.deleted.insert(r); d.edits.retain(|k, _| k.0 != r); }
            s.refresh(cx);
        });
        cx.notify();
    }

    fn rollback(&mut self, cx: &mut Context<Self>) {
        self.grid.update(cx, |s, cx| { s.delegate_mut().clear_staged(); s.refresh(cx); });
        self.edit_note = None;
        cx.notify();
    }

    /// Staged changes as statements, in delete → update → insert order.
    fn staged_sql(&self, cx: &App) -> Result<Vec<String>, String> {
        let live = self.live.as_ref().ok_or_else(|| t("db.not_connected").to_string())?;
        let cols = self.table_cols.as_ref().and_then(|c| c.as_ref().ok()).ok_or_else(|| t("db.loading").to_string())?;
        let pk = self.pk_idx(cols, cx).ok_or_else(|| t("db.edit_no_pk").to_string())?;
        let f = live.driver.family();
        let q = dialect::qualified(f, &self.target.database, self.target.schema.as_deref(), self.target.table.as_deref().unwrap_or_default());
        let d = self.grid.read(cx).delegate();
        let key = |r: usize| edit::key_of(&d.cols, &pk, &d.rows[r]);
        let mut out = vec![];
        for &r in &d.deleted { out.push(edit::to_sql(f, &q, &edit::Change::Delete { key: key(r) })); }
        let mut by_row: BTreeMap<usize, Vec<(usize, Option<String>)>> = BTreeMap::new();
        for ((r, col), v) in &d.edits { by_row.entry(*r).or_default().push((*col, v.clone())); }
        for (r, mut set) in by_row {
            if d.deleted.contains(&r) { continue; }
            set.sort_by_key(|x| x.0);
            let set = set.into_iter().map(|(col, v)| (d.cols[col].name.clone(), v)).collect();
            out.push(edit::to_sql(f, &q, &edit::Change::Update { key: key(r), set }));
        }
        for m in &d.added {
            let mut values: Vec<(usize, Option<String>)> = m.iter().map(|(k, v)| (*k, v.clone())).collect();
            values.sort_by_key(|x| x.0);
            out.push(edit::to_sql(f, &q, &edit::Change::Insert { values: values.into_iter().map(|(col, v)| (d.cols[col].name.clone(), v)).collect() }));
        }
        Ok(out)
    }

    fn commit(&mut self, cx: &mut Context<Self>) {
        match self.staged_sql(cx) {
            Err(e) => { self.edit_note = Some(e); cx.notify(); }
            Ok(v) if v.is_empty() => {}
            Ok(v) if self.production => { self.ask = Some(Ask::Commit(v)); cx.notify(); }
            Ok(v) => self.apply(v, cx),
        }
    }

    /// Run statements in one transaction, then reload the table.
    fn apply(&mut self, stmts: Vec<String>, cx: &mut Context<Self>) {
        let Some(live) = self.live.clone() else { return };
        self.ask = None;
        self.busy = true;
        self.edit_note = None;
        self.status = t("db.committing").into();
        let (db, schema) = (self.target.database.clone(), self.target.schema.clone());
        let n = stmts.len();
        let summary = one_line(&stmts.join("; "));
        let started = Instant::now();
        let h = self.rt.spawn(async move { live.driver.apply(&db, schema.as_deref(), stmts).await });
        cx.spawn(async move |this, cx| {
            let r = h.await.map_err(anyhow::Error::from).and_then(|r| r);
            let _ = this.update(cx, |v, cx| {
                v.busy = false;
                let ms = started.elapsed().as_millis();
                match r {
                    Ok(rows) => {
                        log_push(&v.log, &v.conn_name, &summary, true, tf("db.affected_ms", &[("n", &rows), ("ms", &ms)]));
                        v.load_page(true, cx);
                        v.status = tf("db.committed", &[("n", &n), ("rows", &rows)]);
                    }
                    Err(e) => {
                        log_push(&v.log, &v.conn_name, &summary, false, format!("{e:#}"));
                        v.edit_note = Some(tf("db.commit_failed", &[("e", &format!("{e:#}"))]));
                        v.status.clear();
                    }
                }
                cx.notify();
            });
        }).detach();
        cx.notify();
    }

    // ---- export / import
    fn export(&mut self, fmt: export::Format, cx: &mut Context<Self>) {
        self.pop = None;
        let f = self.live.as_ref().map(|l| l.driver.family()).unwrap_or(ssh_term_proto::assets::Family::Sqlite);
        let (cols, rows) = { let d = self.grid.read(cx).delegate(); (d.cols.clone(), d.rows.clone()) };
        if cols.is_empty() { self.status = t("db.export_empty").into(); cx.notify(); return; }
        let base = self.target.table.clone().unwrap_or_else(|| "result".into());
        let q = match &self.target.table { Some(tb) => dialect::qualified(f, &self.target.database, self.target.schema.as_deref(), tb), None => dialect::quote_ident(f, "result") };
        let text = export::export(fmt, f, &q, &cols, &rows);
        let n = rows.len();
        let dir = home_dir();
        let rx = cx.prompt_for_new_path(&dir, Some(&format!("{base}.{}", fmt.ext())));
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(path))) = rx.await else { return };
            let r = std::fs::write(&path, text);
            let _ = this.update(cx, |v, cx| {
                v.status = match r { Ok(()) => tf("db.exported", &[("n", &n), ("path", &path.display())]), Err(e) => tf("db.export_failed", &[("e", &e)]) };
                cx.notify();
            });
        }).detach();
    }

    fn import(&mut self, cx: &mut Context<Self>) {
        self.pop = None;
        if let Some(why) = if self.read_only { Some(t("db.edit_read_only").to_string()) } else { None } { self.edit_note = Some(why); cx.notify(); return; }
        let rx = cx.prompt_for_paths(gpui::PathPromptOptions { files: true, directories: false, multiple: false, prompt: None });
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(paths))) = rx.await else { return };
            let Some(path) = paths.into_iter().next() else { return };
            let _ = this.update(cx, |v, cx| { v.import_file(&path, cx); cx.notify(); });
        }).detach();
    }

    fn import_file(&mut self, path: &std::path::Path, cx: &mut Context<Self>) {
        let fail = |v: &mut Self, e: String| v.edit_note = Some(tf("db.import_failed", &[("e", &e)]));
        let (Some(live), Some(table)) = (self.live.clone(), self.target.table.clone()) else { return };
        let text = match std::fs::read_to_string(path) { Ok(s) => s, Err(e) => return fail(self, e.to_string()) };
        let file = path.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
        let f = live.driver.family();
        let q = dialect::qualified(f, &self.target.database, self.target.schema.as_deref(), &table);
        let parsed = match export::Format::from_path(path) {
            Some(export::Format::Csv) => export::parse_csv(&text),
            Some(export::Format::Json) => export::parse_json(&text),
            Some(export::Format::Sql) => {
                let stmts: Vec<String> = split::split(&text).into_iter().map(|s| s.text).collect();
                let n = stmts.len();
                self.ask = Some(Ask::Import { stmts, rows: n, file });
                return;
            }
            None => return fail(self, t("db.import_format").into()),
        };
        let (header, rows) = match parsed { Ok(x) => x, Err(e) => return fail(self, format!("{e:#}")) };
        // map file columns onto the table's (case-insensitive)
        let cols: Vec<String> = match &self.table_cols { Some(Ok(c)) => c.iter().map(|c| c.name.clone()).collect(), _ => self.grid.read(cx).delegate().cols.iter().map(|c| c.name.clone()).collect() };
        let mut real = vec![];
        for h in &header {
            match cols.iter().find(|c| c.eq_ignore_ascii_case(h)) { Some(c) => real.push(c.clone()), None => return fail(self, tf("db.import_unknown_col", &[("col", h)])) }
        }
        let n = rows.len();
        let stmts = export::insert_statements(f, &q, &real, &rows, 200);
        self.ask = Some(Ask::Import { stmts, rows: n, file });
    }

    // ---- history / saved queries
    fn record(&mut self, sql: &str, ok: bool, ms: u64) {
        let mut s = history::Store::load();
        s.record(history::Entry { sql: sql.trim().to_string(), conn: self.target.conn.clone(), database: self.target.database.clone(), at: chrono::Utc::now().timestamp(), ok, ms });
        let _ = s.save();
        self.hist = s;
    }

    fn save_query(&mut self, w: &mut Window, cx: &mut Context<Self>) {
        let name = self.save_in.read(cx).value().trim().to_string();
        let sql = self.editor.read(cx).value().to_string();
        if name.is_empty() || sql.trim().is_empty() { return; }
        let mut s = history::Store::load();
        s.save_query(history::Saved { name, sql, conn: self.target.conn.clone() });
        let _ = s.save();
        self.hist = s;
        self.save_in.update(cx, |s, cx| s.set_value("", w, cx));
        self.status = t("db.saved_ok").into();
        cx.notify();
    }

    fn delete_saved(&mut self, name: &str, cx: &mut Context<Self>) {
        let mut s = history::Store::load();
        s.remove_saved(name);
        let _ = s.save();
        self.hist = s;
        cx.notify();
    }

    fn load_sql(&mut self, sql: String, w: &mut Window, cx: &mut Context<Self>) {
        self.pop = None;
        self.editor.update(cx, |s, cx| { s.set_value(sql, w, cx); s.focus(w, cx); });
        cx.notify();
    }

    fn toggle_pop(&mut self, p: Pop, cx: &mut Context<Self>) {
        self.pop = if self.pop == Some(p) { None } else { Some(p) };
        if self.pop.is_some() { self.hist = history::Store::load(); }
        cx.notify();
    }

    // ---- render
    fn toolbar_btn(id: &'static str, ic: IconName, label: Option<String>, tip: &'static str) -> gpui::Stateful<gpui::Div> {
        let th = cur();
        let _ = tip;
        div().id(id).h(px(26.)).px_2().rounded(px(6.)).flex().items_center().gap_1().cursor_pointer().flex_shrink_0()
            .text_size(px(12.)).text_color(c(th.subtext)).hover(|d| d.bg(c(cur().surface0))).active(|d| d.bg(c(cur().surface1)))
            .child(icon(ic, 13., c(th.subtext))).children(label)
    }

    fn header(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let th = cur();
        let mut row = div().h(px(36.)).px_2().flex().items_center().gap_1().flex_shrink_0().border_b_1().border_color(c(th.surface0)).bg(c(th.mantle));
        row = row.child(div().px_1().flex().items_center().gap_1().max_w(px(300.)).min_w_0()
            .child(icon(if self.table_mode() { IconName::Table } else { IconName::Database }, 13., c(th.accent)))
            .child(div().min_w_0().whitespace_nowrap().overflow_hidden().text_ellipsis().text_size(px(12.)).text_color(c(th.subtext)).child(self.crumbs())));
        if self.read_only { row = row.child(ui::tag(t("db.read_only"), c(th.warning))); }
        if self.production { row = row.child(ui::tag(t("tag.production"), c(th.error))); }
        row = row.child(div().w(px(8.)));
        if self.table_mode() {
            let me = cx.entity().downgrade();
            let subs = [("data".to_string(), t("db.sub_data").to_string()), ("structure".to_string(), t("db.sub_structure").to_string()), ("ddl".to_string(), "DDL".to_string())];
            let cur_sub = match self.sub { Sub::Data => "data", Sub::Structure => "structure", Sub::Ddl => "ddl" };
            row = row.child(div().flex_shrink_0().child(ui::segmented("dbv-sub", &subs, cur_sub, move |v, _w, cx| {
                let sub = match v.as_str() { "structure" => Sub::Structure, "ddl" => Sub::Ddl, _ => Sub::Data };
                let _ = me.update(cx, |this, cx| this.set_sub(sub, cx));
            })));
            row = row.child(div().w(px(6.)));
            if self.sub == Sub::Data {
                row = row.child(Self::toolbar_btn("dbv-refresh", IconName::RefreshCw, None, "").on_click(cx.listener(|v, _, _w, cx| v.load_page(true, cx))))
                    .child(div().text_size(px(11.)).font_weight(FontWeight::SEMIBOLD).text_color(c(th.overlay1)).child("WHERE"))
                    .child(div().flex_1().min_w(px(120.)).child(ui::text_field(&self.where_in, false)))
                    .child(ui::button("dbv-apply", t("db.apply"), 0).on_click(cx.listener(|v, _, _w, cx| v.apply_where(cx))));
                if !self.applied_where.is_empty() {
                    row = row.child(ui::button("dbv-clear", t("db.clear_filter"), 3).on_click(cx.listener(|v, _, window, cx| {
                        v.where_in.update(cx, |s, cx| s.set_value("", window, cx));
                        v.apply_where(cx);
                    })));
                }
                row = row.child(Self::toolbar_btn("dbv-export", IconName::ArrowDownToLine, Some(t("db.export").into()), "").on_click(cx.listener(|v, _, _w, cx| v.toggle_pop(Pop::Export, cx))))
                    .child(Self::toolbar_btn("dbv-import", IconName::ArrowUpFromLine, Some(t("db.import").into()), "").on_click(cx.listener(|v, _, _w, cx| v.import(cx))));
            } else {
                row = row.child(div().flex_1()).child(Self::toolbar_btn("dbv-meta-refresh", IconName::RefreshCw, None, "").on_click(cx.listener(|v, _, _w, cx| { if v.sub == Sub::Ddl { v.load_ddl(cx) } else { v.table_cols = None; v.load_meta(cx) } cx.notify(); })));
                if self.sub == Sub::Ddl {
                    row = row.child(Self::toolbar_btn("dbv-ddl-copy", IconName::Copy, Some(t("db.copy").into()), "").on_click(cx.listener(|v, _, _w, cx| {
                        if let Some(Ok(s)) = &v.ddl { cx.write_to_clipboard(gpui::ClipboardItem::new_string(s.clone())); v.status = t("db.copied").into(); cx.notify(); }
                    })));
                }
            }
        } else {
            row = row.child(if self.busy { Self::toolbar_btn("dbv-stop", IconName::Square, Some(t("db.stop").into()), "").on_click(cx.listener(|v, _, _w, cx| v.cancel(cx))) }
                    else { Self::toolbar_btn("dbv-run", IconName::Play, Some(t("db.run").into()), "").on_click(cx.listener(|v, _, _w, cx| v.run_sql(false, cx))) })
                .child(Self::toolbar_btn("dbv-run-all", IconName::ListVideo, Some(t("db.run_all").into()), "").on_click(cx.listener(|v, _, _w, cx| v.run_sql(true, cx))))
                .child(Self::toolbar_btn("dbv-history", IconName::Clock, Some(t("db.history").into()), "").on_click(cx.listener(|v, _, _w, cx| v.toggle_pop(Pop::History, cx))))
                .child(Self::toolbar_btn("dbv-saved", IconName::Bookmark, Some(t("db.saved").into()), "").on_click(cx.listener(|v, _, _w, cx| v.toggle_pop(Pop::Saved, cx))))
                .child(Self::toolbar_btn("dbv-export", IconName::ArrowDownToLine, Some(t("db.export").into()), "").on_click(cx.listener(|v, _, _w, cx| v.toggle_pop(Pop::Export, cx))))
                .child(div().flex_1())
                .child(div().text_size(px(11.)).text_color(c(th.overlay0)).whitespace_nowrap().child(t("db.run_hint")));
        }
        row
    }

    /// Cell editor and staging actions under the table toolbar.
    fn edit_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let th = cur();
        let block = self.edit_block(cx);
        let pending = self.grid.read(cx).delegate().pending();
        let col_name = self.sel.and_then(|(_, col)| self.grid.read(cx).delegate().cols.get(col).map(|c| c.name.clone()));
        let mut row = div().h(px(34.)).px_2().flex().items_center().gap_1().flex_shrink_0().border_b_1().border_color(c(th.surface0));
        row = row.child(div().min_w(px(60.)).max_w(px(180.)).px_1().whitespace_nowrap().overflow_hidden().text_ellipsis().text_size(px(11.)).font_weight(FontWeight::SEMIBOLD).text_color(c(th.overlay1))
            .child(col_name.unwrap_or_else(|| t("db.select_cell").to_string())));
        if block.is_none() {
            row = row.child(div().flex_1().min_w(px(120.)).child(ui::text_field(&self.cell_in, false)))
                .child(ui::button("dbv-cell-set", t("db.cell_set"), 0).on_click(cx.listener(|v, _, _w, cx| { let s = v.cell_in.read(cx).value().to_string(); v.stage(Some(s), cx); })))
                .child(ui::button("dbv-cell-null", "NULL", 3).on_click(cx.listener(|v, _, _w, cx| v.stage(None, cx))))
                .child(div().w(px(6.)))
                .child(Self::toolbar_btn("dbv-add-row", IconName::Plus, Some(t("db.add_row").into()), "").on_click(cx.listener(|v, _, _w, cx| v.add_row(cx))))
                .child(Self::toolbar_btn("dbv-del-row", IconName::Minus, Some(t("db.delete_row").into()), "").on_click(cx.listener(|v, _, _w, cx| v.delete_row(cx))));
        } else {
            row = row.child(div().flex_1().min_w_0().whitespace_nowrap().overflow_hidden().text_ellipsis().text_size(px(11.)).text_color(c(th.overlay0)).child(block.unwrap_or_default()));
        }
        if pending > 0 {
            row = row.child(ui::button("dbv-rollback", t("db.rollback"), 0).on_click(cx.listener(|v, _, _w, cx| v.rollback(cx))))
                .child(ui::button("dbv-commit", tf("db.commit_n", &[("n", &pending)]), 1).on_click(cx.listener(|v, _, _w, cx| v.commit(cx))));
        }
        let note = self.edit_note.clone().map(|n| div().px_3().py_1().flex_shrink_0().text_size(px(11.)).text_color(c(th.error)).child(n));
        div().flex().flex_col().flex_shrink_0().child(row).children(note)
    }

    fn structure_el(&self) -> gpui::AnyElement {
        let th = cur();
        let cols = match &self.table_cols {
            None => return div().flex_1().p_3().text_size(px(12.)).text_color(c(th.overlay1)).child(t("db.loading")).into_any_element(),
            Some(Err(e)) => return div().flex_1().p_3().text_size(px(12.)).text_color(c(th.error)).child(e.clone()).into_any_element(),
            Some(Ok(c)) => c,
        };
        let widths = [36., 180., 200., 60., 50., 180.];
        let cell = |w: f32, s: String, color| div().w(px(w)).flex_shrink_0().px_2().whitespace_nowrap().overflow_hidden().text_ellipsis().text_color(color).child(s);
        let mut list = div().id("dbv-structure").flex_1().min_h_0().overflow_y_scroll().flex().flex_col().text_size(px(12.));
        let head = [t("db.col_index"), t("db.col_name"), t("db.col_type"), t("db.col_nullable"), t("db.col_pk"), t("db.col_default")];
        let mut hr = div().h(px(28.)).flex().items_center().border_b_1().border_color(c(th.surface0)).bg(c(th.mantle)).font_weight(FontWeight::SEMIBOLD);
        for (i, h) in head.iter().enumerate() { hr = hr.child(cell(widths[i], h.to_string(), c(th.overlay1))); }
        hr = hr.child(div().flex_1().px_2().text_color(c(th.overlay1)).child(t("db.col_comment")));
        list = list.child(hr);
        for (i, col) in cols.iter().enumerate() {
            list = list.child(div().h(px(26.)).flex().items_center().border_b_1().border_color(c(th.surface0).opacity(0.5))
                .child(cell(widths[0], (i + 1).to_string(), c(th.overlay0)))
                .child(cell(widths[1], col.name.clone(), c(th.text)))
                .child(cell(widths[2], col.type_name.clone(), c(th.ansi[6])))
                .child(cell(widths[3], match col.nullable { Some(true) => "YES".into(), Some(false) => "NO".into(), None => String::new() }, c(th.subtext)))
                .child(cell(widths[4], if col.primary { "PK".into() } else { String::new() }, c(th.warning)))
                .child(cell(widths[5], col.default.clone().unwrap_or_default(), c(th.subtext)))
                .child(div().flex_1().min_w_0().px_2().whitespace_nowrap().overflow_hidden().text_ellipsis().text_color(c(th.overlay1)).child(col.comment.clone())));
        }
        list.into_any_element()
    }

    fn ddl_el(&self) -> gpui::AnyElement {
        let th = cur();
        match &self.ddl {
            Some(Err(e)) => div().flex_1().p_3().text_size(px(12.)).text_color(c(th.error)).child(e.clone()).into_any_element(),
            Some(Ok(s)) if s.is_empty() => div().flex_1().p_3().text_size(px(12.)).text_color(c(th.overlay1)).child(t("db.loading")).into_any_element(),
            _ => div().flex_1().min_h_0().child(Editor::new(&self.ddl_ed).h_full().bordered(false)).into_any_element(),
        }
    }

    fn pop_el(&self, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        let p = self.pop?;
        let th = cur();
        let mut card = ui::popover().absolute().top(px(38.)).right(px(8.)).occlude().p_1().flex().flex_col().gap_1()
            .on_mouse_down_out(cx.listener(|v, _, _w, cx| { v.pop = None; cx.notify(); }));
        match p {
            Pop::Export => {
                for (i, (fmt, label)) in [(export::Format::Csv, "CSV"), (export::Format::Json, "JSON"), (export::Format::Sql, "SQL INSERT")].into_iter().enumerate() {
                    card = card.child(ui::menu_item(("dbv-exp", i), None, label, true, false).on_click(cx.listener(move |v, _, _w, cx| v.export(fmt, cx))));
                }
                card = card.child(div().px_2().pb_1().text_size(px(11.)).text_color(c(th.overlay0)).child(t("db.export_note")));
            }
            Pop::History => {
                card = card.w(px(480.)).max_h(px(420.)).child(div().p_1().child(ui::search_field(&self.hist_in, false)));
                let q = self.hist_in.read(cx).value().to_lowercase();
                let mut list = div().id("dbv-hist").flex().flex_col().overflow_y_scroll().max_h(px(360.));
                let mut n = 0;
                for (i, e) in self.hist.history.iter().filter(|e| e.conn == self.target.conn && (q.is_empty() || e.sql.to_lowercase().contains(&q))).take(200).enumerate() {
                    n += 1;
                    let sql = e.sql.clone();
                    let when = chrono::DateTime::from_timestamp(e.at, 0).map(|d| d.with_timezone(&chrono::Local).format("%m-%d %H:%M").to_string()).unwrap_or_default();
                    list = list.child(div().id(("dbv-h", i)).px_2().py_1().rounded(px(6.)).cursor_pointer().hover(|d| d.bg(c(cur().surface0)))
                        .on_click(cx.listener(move |v, _, w, cx| v.load_sql(sql.clone(), w, cx)))
                        .child(div().font_family("monospace").text_size(px(12.)).text_color(if e.ok { c(th.text) } else { c(th.error) }).whitespace_nowrap().overflow_hidden().text_ellipsis().child(one_line(&e.sql)))
                        .child(div().text_size(px(10.)).text_color(c(th.overlay0)).child(format!("{when} · {} ms{}", e.ms, if e.database.is_empty() { String::new() } else { format!(" · {}", e.database) }))));
                }
                if n == 0 { list = list.child(div().p_3().text_size(px(12.)).text_color(c(th.overlay0)).child(t("db.history_empty"))); }
                card = card.child(list);
            }
            Pop::Saved => {
                card = card.w(px(480.)).max_h(px(420.))
                    .child(div().p_1().flex().gap_1().child(div().flex_1().child(ui::text_field(&self.save_in, false)))
                        .child(ui::button("dbv-save-q", t("db.save_current"), 1).on_click(cx.listener(|v, _, w, cx| v.save_query(w, cx)))));
                let mut list = div().id("dbv-saved-list").flex().flex_col().overflow_y_scroll().max_h(px(360.));
                for (i, sq) in self.hist.saved.iter().enumerate() {
                    let (sql, name) = (sq.sql.clone(), sq.name.clone());
                    list = list.child(div().id(("dbv-s", i)).px_2().py_1().rounded(px(6.)).flex().items_center().gap_2().cursor_pointer().hover(|d| d.bg(c(cur().surface0)))
                        .on_click(cx.listener(move |v, _, w, cx| v.load_sql(sql.clone(), w, cx)))
                        .child(div().flex_1().min_w_0().flex().flex_col()
                            .child(div().text_size(px(12.)).font_weight(FontWeight::MEDIUM).text_color(c(th.text)).child(sq.name.clone()))
                            .child(div().font_family("monospace").text_size(px(11.)).text_color(c(th.overlay1)).whitespace_nowrap().overflow_hidden().text_ellipsis().child(one_line(&sq.sql))))
                        .child(div().id(("dbv-s-del", i)).px_1().rounded(px(4.)).text_size(px(12.)).text_color(c(th.overlay0)).hover(|d| d.text_color(c(cur().error)))
                            .on_click(cx.listener(move |v, _, _w, cx| { cx.stop_propagation(); v.delete_saved(&name, cx); })).child("×")));
                }
                if self.hist.saved.is_empty() { list = list.child(div().p_3().text_size(px(12.)).text_color(c(th.overlay0)).child(t("db.saved_empty"))); }
                card = card.child(list);
            }
        }
        Some(card)
    }

    fn footer(&self) -> impl IntoElement {
        let th = cur();
        let mut f = div().h(px(26.)).px_3().flex().items_center().gap_2().flex_shrink_0().border_t_1().border_color(c(th.surface0)).bg(c(th.mantle)).text_size(px(11.)).text_color(c(th.overlay1));
        if self.busy { f = f.child(icon(IconName::LoaderCircle, 12., c(th.accent))); }
        f = f.child(div().flex_1().min_w_0().whitespace_nowrap().overflow_hidden().text_ellipsis().child(self.status.clone()));
        if let Some(l) = &self.live { if !l.version.is_empty() { f = f.child(div().whitespace_nowrap().child(l.version.chars().take(40).collect::<String>())); } if let Some(via) = &l.via { f = f.child(div().whitespace_nowrap().child(tf("db.via_host", &[("host", via)]))); } }
        f
    }

    fn result_tabs(&self, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        if self.results.len() < 2 { return None; }
        let th = cur();
        let mut row = div().h(px(28.)).px_2().flex().items_center().gap_1().flex_shrink_0().border_b_1().border_color(c(th.surface0)).id("dbv-rtabs").overflow_x_scroll();
        for (i, rs) in self.results.iter().enumerate() {
            let active = i == self.result_ix;
            let label = match (&rs.error, rs.returns_rows) { (Some(_), _) => tf("db.result_err", &[("n", &(i + 1))]), (None, true) => tf("db.result_rows", &[("n", &(i + 1)), ("rows", &rs.rows.len())]), (None, false) => tf("db.result_affected", &[("n", &(i + 1)), ("rows", &rs.affected)]) };
            row = row.child(div().id(("dbv-rt", i)).h(px(22.)).px_2().rounded(px(5.)).flex().items_center().cursor_pointer().text_size(px(11.)).flex_shrink_0()
                .bg(if active { c(th.surface0) } else { gpui::transparent_black() })
                .text_color(if rs.error.is_some() { c(th.error) } else if active { c(th.text) } else { c(th.overlay1) })
                .hover(|d| d.bg(c(cur().surface0)))
                .on_click(cx.listener(move |v, _, _w, cx| { v.result_ix = i; v.show_result(cx); cx.notify(); }))
                .child(label));
        }
        Some(row)
    }

    fn result_body(&self) -> gpui::AnyElement {
        let th = cur();
        let msg = |s: String, err: bool| div().flex_1().p_3().text_size(px(12.)).text_color(if err { c(th.error) } else { c(th.subtext) }).child(s).into_any_element();
        if let Some(e) = &self.conn_err { return msg(e.clone(), true); }
        match self.results.get(self.result_ix) {
            Some(rs) if rs.error.is_some() => msg(rs.error.clone().unwrap_or_default(), true),
            Some(rs) if !rs.returns_rows => msg(tf("db.affected_ms", &[("n", &rs.affected), ("ms", &rs.elapsed.as_millis())]), false),
            Some(rs) => {
                let note = rs.truncated.then(|| div().px_3().py_1().text_size(px(11.)).text_color(c(th.warning)).child(tf("db.truncated", &[("n", &db::MAX_ROWS)])));
                div().flex_1().min_h_0().flex().flex_col().children(note).child(div().flex_1().min_h_0().child(DataTable::new(&self.grid).stripe(true).bordered(false))).into_any_element()
            }
            None => match &self.error { Some(e) => msg(e.clone(), true), None => msg(t("db.query_hint").into(), false) },
        }
    }

    fn confirm_el(&self, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        let ask = self.ask.as_ref()?;
        let th = cur();
        let (msg, preview, ok_label) = match ask {
            Ask::Run { stmt, .. } => ((if self.production { t("db.confirm_prod") } else { t("db.confirm_danger") }).to_string(), stmt.clone(), t("db.confirm_run").to_string()),
            Ask::Commit(v) => (tf("db.confirm_commit", &[("n", &v.len())]), v.join(";\n"), t("db.commit").to_string()),
            Ask::Import { stmts, rows, file } => (tf("db.confirm_import", &[("n", rows), ("file", file), ("table", &self.target.table.clone().unwrap_or_default())]), stmts.first().cloned().unwrap_or_default(), t("db.import").to_string()),
        };
        let preview: String = preview.chars().take(1200).collect();
        Some(ui::modal(ui::card(520.)
            .child(div().p_5().flex().flex_col().gap_3()
                .child(div().flex().items_center().gap_2().child(icon(IconName::TriangleAlert, 16., c(th.warning))).child(div().text_size(px(13.)).font_weight(FontWeight::SEMIBOLD).text_color(c(th.text)).child(t("db.confirm_title"))))
                .child(div().text_size(px(12.)).text_color(c(th.subtext)).child(msg))
                .child(div().id("dbv-cf-pre").max_h(px(240.)).overflow_y_scroll().p_2().rounded(px(6.)).bg(c(th.mantle)).font_family("monospace").text_size(px(12.)).text_color(c(th.text)).child(preview)))
            .child(div().px_5().py_3().flex().justify_end().gap_2().border_t_1().border_color(c(th.surface0))
                .child(ui::button("dbv-cf-no", t("ui.cancel"), 0).on_click(cx.listener(|v, _, _w, cx| { v.ask = None; cx.notify(); })))
                .child(ui::button("dbv-cf-yes", ok_label, 2).on_click(cx.listener(|v, _, _w, cx| {
                    match v.ask.take() {
                        Some(Ask::Run { sql, .. }) => v.execute(sql, cx),
                        Some(Ask::Commit(stmts)) | Some(Ask::Import { stmts, .. }) => v.apply(stmts, cx),
                        None => {}
                    }
                }))))))
    }
}

fn home_dir() -> std::path::PathBuf {
    std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")).map(Into::into).unwrap_or_else(|| ".".into())
}

fn one_line(sql: &str) -> String {
    let s: String = sql.split_whitespace().collect::<Vec<_>>().join(" ");
    if s.chars().count() > 160 { format!("{}…", s.chars().take(160).collect::<String>()) } else { s }
}

/// Name, read-only flag and production flag of a saved connection.
pub fn conn_meta(id: &str) -> (String, bool, bool) {
    let s = crate::config::get();
    match s.db.iter().find(|d| d.id == id) {
        Some(d) => (d.name.clone(), d.read_only, ssh_term_proto::assets::db_is_production(d, &s.groups, &s.tags.production)),
        None => (t("db.missing_conn").into(), true, false),
    }
}

impl Render for DbView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if let Some(s) = self.pending_ddl.take() { self.ddl_ed.update(cx, |e, cx| e.set_value(s, window, cx)); }
        let th = cur();
        let mut root = div().id("db-view").size_full().relative().flex().flex_col().bg(c(th.base)).text_color(c(th.text));
        root = root.child(self.header(cx));
        if self.table_mode() {
            let body: gpui::AnyElement = match self.sub {
                Sub::Structure => self.structure_el(),
                Sub::Ddl => self.ddl_el(),
                Sub::Data => match (&self.conn_err, &self.error) {
                    (Some(e), _) | (None, Some(e)) => div().flex_1().p_3().text_size(px(12.)).text_color(c(th.error)).child(e.clone()).into_any_element(),
                    _ if self.live.is_none() => div().flex_1().p_3().text_size(px(12.)).text_color(c(th.overlay1)).child(t("db.connecting")).into_any_element(),
                    _ => div().flex_1().min_h_0().flex().flex_col().child(self.edit_bar(cx)).child(div().flex_1().min_h_0().child(DataTable::new(&self.grid).stripe(true).bordered(false))).into_any_element(),
                },
            };
            root = root.child(body);
        } else {
            let h = self.editor_h;
            let editor = div().h(px(h)).flex_shrink_0()
                .capture_key_down(cx.listener(|v, e: &gpui::KeyDownEvent, _w, cx| {
                    let k = &e.keystroke;
                    if k.key == "enter" && (k.modifiers.control || k.modifiers.platform) { v.run_sql(k.modifiers.shift, cx); cx.stop_propagation(); }
                }))
                .child(Editor::new(&self.editor).h(px(h)).bordered(false));
            let handle = div().id("dbv-split").h(px(5.)).w_full().flex_shrink_0().cursor_row_resize().border_t_1().border_color(c(th.surface0))
                .hover(|d| d.bg(c(cur().accent).opacity(0.35)))
                .on_drag(EditorResize, |d: &EditorResize, _, _, cx| cx.new(|_| d.clone()))
                .on_click(cx.listener(|v, ev: &gpui::ClickEvent, _w, cx| if ev.click_count() >= 2 { v.editor_h = EDITOR_H; cx.notify(); }));
            let body = div().id("dbv-qbody").flex_1().min_h_0().flex().flex_col()
                .on_drag_move(cx.listener(|v, ev: &gpui::DragMoveEvent<EditorResize>, _w, cx| {
                    let total = f32::from(ev.bounds.size.height);
                    let h = (f32::from(ev.event.position.y - ev.bounds.top())).clamp(60., (total - 80.).max(60.));
                    if (h - v.editor_h).abs() >= 1.0 { v.editor_h = h; cx.notify(); }
                }))
                .child(editor).child(handle).children(self.result_tabs(cx)).child(self.result_body());
            root = root.child(body);
        }
        root = root.child(self.footer());
        root.children(self.pop_el(cx)).children(self.confirm_el(cx))
    }
}
