//! Database tab content (v0.2 PRD §6.4/§6.5): a read-only table browser with server-side
//! paging, filtering and sorting, or a SQL editor with result tabs. Lives inside a layout
//! leaf (`TermView.db`) so tabs, splits and layout restore work as for terminals.
use std::sync::Arc;
use std::time::Instant;

use gpui::{div, prelude::*, px, App, Context, Entity, FontWeight, SharedString, WeakEntity, Window};
use gpui_component::input::{Editor, EditorState, InputEvent, InputState};
use gpui_component::table::{Column, ColumnSort, DataTable, TableDelegate, TableState};
use ssh_term_proto::db::{self, live::Live, split, CancelSlot, ColumnInfo, ResultSet, RunOpts};
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
}

impl Grid {
    fn new(owner: WeakEntity<DbView>, server_sort: bool) -> Self {
        Self { cols: vec![], widths: vec![], rows: vec![], more: false, busy: false, server_sort, sort: None, owner }
    }
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
    fn rows_count(&self, _: &App) -> usize { self.rows.len() }
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
        let v = self.rows.get(row).and_then(|r| r.get(col)).and_then(|v| v.as_deref());
        div().w_full().whitespace_nowrap().overflow_hidden().text_ellipsis().text_size(px(12.))
            .when(v.is_none(), |d| d.italic().text_color(c(th.overlay0)))
            .child(cell_display(v))
    }
    fn cell_text(&self, row: usize, col: usize, _: &App) -> String {
        self.rows.get(row).and_then(|r| r.get(col)).map(|v| v.clone().unwrap_or_else(|| "NULL".into())).unwrap_or_default()
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
    // query mode
    editor: Entity<EditorState>,
    results: Vec<ResultSet>,
    result_ix: usize,
    confirm: Option<(String, String)>,
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
        let grid = cx.new(|cx| TableState::new(Grid::new(me.clone(), table_mode), window, cx).col_movable(false));
        let where_in = cx.new(|cx| InputState::new(window, cx).placeholder(t("db.where_placeholder")));
        let editor = cx.new(|cx| EditorState::new(window, cx).language("sql").line_number(true).placeholder(t("db.sql_placeholder")).default_value(sql));
        let mut subs = vec![];
        subs.push(cx.subscribe_in(&where_in, window, |this: &mut Self, _, ev: &InputEvent, _w, cx| { if let InputEvent::PressEnter { .. } = ev { this.apply_where(cx); } }));
        let (conn_name, read_only, production) = conn_meta(&target.conn);
        let mut me = Self { title: String::new(), target, conn_name, read_only, production, live: None, conn_err: None, rt, log, grid, where_in, applied_where: String::new(), total: None,
            editor, results: vec![], result_ix: 0, confirm: None, status: String::new(), error: None, busy: false, gen: 0, slot: None, _subs: subs };
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
                self.live = Some(l);
                self.conn_err = None;
                if fresh && self.table_mode() { self.load_page(true, cx); }
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
            if reset { d.rows.clear(); d.more = false; }
            let order = d.sort.and_then(|(i, desc)| d.cols.get(i).map(|c| (c.name.clone(), desc)));
            let n = d.rows.len();
            cx.notify();
            (n, order)
        });
        if reset { self.total = None; self.grid.update(cx, |s, cx| s.scroll_to_row(0, cx)); }
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
        if let (Some(stmt), false) = (ask, self.read_only) { self.confirm = Some((sql, stmt)); cx.notify(); return; }
        self.execute(sql, cx);
    }

    fn execute(&mut self, sql: String, cx: &mut Context<Self>) {
        let Some(live) = self.live.clone() else { self.error = Some(self.conn_err.clone().unwrap_or_else(|| t("db.not_connected").into())); cx.notify(); return };
        self.confirm = None;
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
                        v.status = tf("db.done_n", &[("n", &sets.len()), ("ms", &ms)]);
                        v.error = failed;
                        // first failing set, else the last one that returned rows, else the last
                        v.result_ix = sets.iter().position(|r| r.error.is_some()).or_else(|| sets.iter().rposition(|r| r.returns_rows)).unwrap_or(sets.len().saturating_sub(1));
                        v.results = sets;
                        v.show_result(cx);
                    }
                    Err(e) => {
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
        row = row.child(div().px_1().flex().items_center().gap_1().max_w(px(360.)).min_w_0()
            .child(icon(if self.table_mode() { IconName::Table } else { IconName::Database }, 13., c(th.accent)))
            .child(div().min_w_0().whitespace_nowrap().overflow_hidden().text_ellipsis().text_size(px(12.)).text_color(c(th.subtext)).child(self.crumbs())));
        if self.read_only { row = row.child(ui::tag(t("db.read_only"), c(th.warning))); }
        if self.production { row = row.child(ui::tag(t("tag.production"), c(th.error))); }
        row = row.child(div().w(px(8.)));
        if self.table_mode() {
            row = row.child(Self::toolbar_btn("dbv-refresh", IconName::RefreshCw, None, "").on_click(cx.listener(|v, _, _w, cx| v.load_page(true, cx))))
                .child(div().w(px(6.)))
                .child(div().text_size(px(11.)).font_weight(FontWeight::SEMIBOLD).text_color(c(th.overlay1)).child("WHERE"))
                .child(div().flex_1().min_w(px(120.)).child(ui::text_field(&self.where_in, false)))
                .child(ui::button("dbv-apply", t("db.apply"), 0).on_click(cx.listener(|v, _, _w, cx| v.apply_where(cx))));
            if !self.applied_where.is_empty() {
                row = row.child(ui::button("dbv-clear", t("db.clear_filter"), 3).on_click(cx.listener(|v, _, window, cx| {
                    v.where_in.update(cx, |s, cx| s.set_value("", window, cx));
                    v.apply_where(cx);
                })));
            }
        } else {
            row = row.child(if self.busy { Self::toolbar_btn("dbv-stop", IconName::Square, Some(t("db.stop").into()), "").on_click(cx.listener(|v, _, _w, cx| v.cancel(cx))) }
                    else { Self::toolbar_btn("dbv-run", IconName::Play, Some(t("db.run").into()), "").on_click(cx.listener(|v, _, _w, cx| v.run_sql(false, cx))) })
                .child(Self::toolbar_btn("dbv-run-all", IconName::ListVideo, Some(t("db.run_all").into()), "").on_click(cx.listener(|v, _, _w, cx| v.run_sql(true, cx))))
                .child(div().flex_1())
                .child(div().text_size(px(11.)).text_color(c(th.overlay0)).child(t("db.run_hint")));
        }
        row
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
        let (_, stmt) = self.confirm.clone()?;
        let th = cur();
        Some(ui::modal(ui::card(460.)
            .child(div().p_5().flex().flex_col().gap_3()
                .child(div().flex().items_center().gap_2().child(icon(IconName::TriangleAlert, 16., c(th.warning))).child(div().text_size(px(13.)).font_weight(FontWeight::SEMIBOLD).text_color(c(th.text)).child(t("db.confirm_title"))))
                .child(div().text_size(px(12.)).text_color(c(th.subtext)).child(if self.production { t("db.confirm_prod") } else { t("db.confirm_danger") }))
                .child(div().p_2().rounded(px(6.)).bg(c(th.mantle)).font_family("monospace").text_size(px(12.)).text_color(c(th.text)).child(stmt.chars().take(400).collect::<String>())))
            .child(div().px_5().py_3().flex().justify_end().gap_2().border_t_1().border_color(c(th.surface0))
                .child(ui::button("dbv-cf-no", t("ui.cancel"), 0).on_click(cx.listener(|v, _, _w, cx| { v.confirm = None; cx.notify(); })))
                .child(ui::button("dbv-cf-yes", t("db.confirm_run"), 2).on_click(cx.listener(|v, _, _w, cx| { if let Some((sql, _)) = v.confirm.take() { v.execute(sql, cx); } }))))))
    }
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
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let th = cur();
        let mut root = div().id("db-view").size_full().relative().flex().flex_col().bg(c(th.base)).text_color(c(th.text));
        root = root.child(self.header(cx));
        if self.table_mode() {
            let body: gpui::AnyElement = match (&self.conn_err, &self.error) {
                (Some(e), _) | (None, Some(e)) => div().flex_1().p_3().text_size(px(12.)).text_color(c(th.error)).child(e.clone()).into_any_element(),
                _ if self.live.is_none() => div().flex_1().p_3().text_size(px(12.)).text_color(c(th.overlay1)).child(t("db.connecting")).into_any_element(),
                _ => div().flex_1().min_h_0().child(DataTable::new(&self.grid).stripe(true).bordered(false)).into_any_element(),
            };
            root = root.child(body);
        } else {
            let editor = div().h(px(220.)).flex_shrink_0().border_b_1().border_color(c(th.surface0))
                .capture_key_down(cx.listener(|v, e: &gpui::KeyDownEvent, _w, cx| {
                    let k = &e.keystroke;
                    if k.key == "enter" && (k.modifiers.control || k.modifiers.platform) { v.run_sql(k.modifiers.shift, cx); cx.stop_propagation(); }
                }))
                .child(Editor::new(&self.editor).h(px(220.)).bordered(false));
            root = root.child(editor).children(self.result_tabs(cx)).child(self.result_body());
        }
        root = root.child(self.footer());
        root.children(self.confirm_el(cx))
    }
}
