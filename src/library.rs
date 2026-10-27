//! Right-hand docks for saved commands and command history. Same card as the AI assistant.
use gpui::{div, prelude::*, px, AnyElement, ClipboardItem, Context, Entity, FontWeight, SharedString, Window};
use gpui_component::input::{InputState, Textarea, TextareaState};
use ssh_term_proto::i18n::{t, tf};
use ssh_term_proto::snippets::Snippet;
use ssh_term_proto::suggest::HistItem;

use crate::theme::{c, cur};
use crate::ui::{self, icon, IconName};
use crate::{data_dir, Root};

/// Filter value for snippets that have no group. Not a name a user can type.
const UNGROUPED: &str = "\u{1}";

#[derive(Clone)]
pub(crate) struct LibEditor {
    pub id: Option<String>,
    pub name: Entity<InputState>,
    pub group: Entity<InputState>,
    pub note: Entity<InputState>,
    pub body: Entity<TextareaState>,
}

#[derive(Clone)]
pub(crate) struct LibVars {
    pub body: String,
    pub exec: bool,
    pub names: Vec<String>,
    pub inputs: Vec<Entity<InputState>>,
}

#[derive(Clone)]
pub(crate) struct LibConfirm {
    pub lines: Vec<String>,
    pub reasons: Vec<String>,
}

struct HistRow {
    key: String,
    cmd: String,
    ts: Option<i64>,
    ord: u32,
}

fn fmt_ts(ts: i64) -> String {
    chrono::DateTime::from_timestamp(ts, 0)
        .map(|d| d.with_timezone(&chrono::Local).format("%Y-%m-%d %H:%M").to_string())
        .unwrap_or_default()
}

fn chip(id: impl Into<gpui::ElementId>, label: impl Into<SharedString>, on: bool) -> gpui::Stateful<gpui::Div> {
    let th = cur();
    div().id(id).h(px(24.)).px_2().rounded_full().flex().items_center().cursor_pointer().text_size(px(11.))
        .bg(if on { c(th.accent).opacity(0.16) } else { c(th.surface0) })
        .text_color(if on { c(th.accent) } else { c(th.subtext) })
        .hover(|d| d.bg(c(cur().surface1)))
        .child(label.into())
}

impl Root {
    /// One card per mode: saved commands (`snippets_mode`) or history. Both can be open; the shared
    /// editor / variable / confirm forms show in the snippets card, or in history when that one is closed.
    pub(crate) fn library_dock(&mut self, snippets_mode: bool, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let search = if snippets_mode { self.lib_search.clone() } else { self.hist_search.clone() };
        let owns_forms = snippets_mode || !self.dock_snip;
        let q = search.read(cx).value().trim().to_lowercase();
        let group = self.lib_group.clone();
        let hist_all = self.lib_hist_all;
        let editor = self.lib_editor.clone().filter(|_| owns_forms);
        let vars = self.lib_vars.clone().filter(|_| owns_forms);
        let confirm = self.lib_confirm.clone().filter(|_| owns_forms);
        let notice = if owns_forms { self.lib_notice.clone() } else { String::new() };
        let module = self.snippet_module();
        let ready = module != "terminal" || self.focused_pane(window, cx).is_some_and(|p| p.read(cx).ready());
        let th = cur();
        let mut body = div().flex_1().min_h_0().flex().flex_col();
        body = body.child(ui::field_row().pt_2().child(ui::search_field(&search, false)));
        if !ready {
            body = body.child(div().px_3().pt_2().text_size(px(12.)).text_color(c(th.overlay1)).child(t("lib.need_terminal")));
        }
        if snippets_mode {
            let mut all = ssh_term_proto::snippets::load();
            all.retain(|s| s.module == module);
            body = body.child(div().px_3().pt_2().text_size(px(11.)).text_color(c(th.overlay1)).child(tf("lib.module_scope", &[("module", &crate::library::module_label(module))])));
            body = body.child(self.snippet_groups(&all, &group, cx));
            if let Some(ed) = editor {
                body = body.child(self.snippet_editor(&ed, cx));
            } else if let Some(v) = vars {
                body = body.child(self.var_form(&v, cx));
            } else if let Some(cfn) = confirm {
                body = body.child(self.confirm_form(&cfn, cx));
            } else {
                let empty = all.is_empty();
                body = body.child(self.snippet_list(&all, &q, &group, cx));
                body = body.child(div().px_3().py_2().flex_shrink_0().flex().gap_2().child(
                    ui::button("lib-new", t("lib.new"), 1).on_click(cx.listener(|this, _, w, cx| {
                        this.open_lib_editor(None, String::new(), String::new(), String::new(), String::new(), w, cx);
                    })),
                ).when(empty, |d| d.child(ui::button("lib-starters", t("lib.add_examples"), 0).on_click(cx.listener(move |this, _, _w, cx| this.add_starters(module, cx))))));
            }
        } else {
            body = body.child(div().px_3().pt_2().flex().gap_1()
                .child(chip("lib-host-this", t("lib.this_host"), !hist_all).on_click(cx.listener(|this, _, _, cx| { this.lib_hist_all = false; cx.notify(); })))
                .child(chip("lib-host-all", t("lib.all_hosts"), hist_all).on_click(cx.listener(|this, _, _, cx| { this.lib_hist_all = true; cx.notify(); }))));
            if let Some(v) = vars {
                body = body.child(self.var_form(&v, cx));
            } else if let Some(cfn) = confirm {
                body = body.child(self.confirm_form(&cfn, cx));
            } else {
                let rows = self.history_rows(window, cx);
                body = body.child(self.history_list(&rows, &q, hist_all, cx));
                body = body.child(div().px_3().py_2().flex_shrink_0().child(
                    ui::button("lib-clear-host", t("lib.clear_host"), 2).on_click(cx.listener(|this, _, w, cx| this.clear_current_history(w, cx))),
                ));
            }
        }
        if !notice.is_empty() {
            body = body.child(div().px_3().pb_2().text_size(px(12.)).text_color(c(th.warning)).child(notice));
        }
        let (id, title, ic) = if snippets_mode { ("snip-dock", t("lib.snippets"), IconName::Code) } else { ("hist-dock", t("lib.history"), IconName::Clock) };
        self.dock_frame(id, title.into(), ic, body, cx)
    }

    fn snippet_groups(&mut self, all: &[Snippet], group: &str, cx: &mut Context<Self>) -> gpui::Div {
        let mut names: Vec<String> = all.iter().map(|s| s.group.clone()).filter(|g| !g.is_empty()).collect();
        names.sort();
        names.dedup();
        let ungrouped = all.iter().any(|s| s.group.is_empty());
        let mut row = div().px_3().pt_2().flex().flex_wrap().gap_1()
            .child(chip("lib-g-all", t("lib.all_groups"), group.is_empty()).on_click(cx.listener(|this, _, _, cx| { this.lib_group.clear(); cx.notify(); })));
        for (i, name) in names.iter().enumerate() {
            let n = name.clone();
            let on = group == name;
            row = row.child(chip(("lib-g", i), name.clone(), on).on_click(cx.listener(move |this, _, _, cx| { this.lib_group = n.clone(); cx.notify(); })));
        }
        if ungrouped {
            row = row.child(chip("lib-g-none", t("lib.ungrouped"), group == UNGROUPED).on_click(cx.listener(|this, _, _, cx| { this.lib_group = UNGROUPED.into(); cx.notify(); })));
        }
        row
    }

    fn snippet_list(&mut self, all: &[Snippet], q: &str, group: &str, cx: &mut Context<Self>) -> AnyElement {
        let th = cur();
        let shown: Vec<Snippet> = all.iter().filter(|s| snip_visible(s, q, group)).cloned().collect();
        let mut list = div().id("lib-snip-list").flex_1().min_h_0().overflow_y_scroll().px_2().py_1().flex().flex_col().gap(px(2.));
        if shown.is_empty() {
            list = list.child(div().p_4().text_size(px(12.)).text_color(c(th.overlay1)).child(t("lib.empty_snippets")));
        }
        for (i, s) in shown.iter().enumerate() {
            let body = s.body.clone();
            let id = s.id.clone();
            let name = s.name.clone();
            let group_name = s.group.clone();
            let note = s.note.clone();
            let preview: String = s.body.lines().find(|l| !l.trim().is_empty()).unwrap_or("").trim().chars().take(80).collect();
            let run_body = body.clone();
            let ins_body = body.clone();
            let copy_body = body.clone();
            let edit = (id.clone(), name, group_name.clone(), note, body.clone());
            let del = id.clone();
            let dup = id.clone();
            list = list.child(div().id(("lib-sn", i)).px_2().py_1().rounded(px(6.)).flex().items_center().gap_1().cursor_pointer()
                .hover(|d| d.bg(c(cur().surface0)))
                .on_click(cx.listener(move |this, _, w, cx| this.start_snippet(run_body.clone(), true, w, cx)))
                .child(div().flex_1().min_w_0().flex().flex_col().gap(px(1.))
                    .child(div().flex().items_center().gap_1()
                        .child(div().text_size(px(13.)).font_weight(FontWeight::MEDIUM).text_color(c(th.text)).overflow_hidden().text_ellipsis().whitespace_nowrap().child(s.name.clone()))
                        .when(!group_name.is_empty(), |d| d.child(div().text_size(px(11.)).text_color(c(th.overlay0)).child(group_name.clone()))))
                    .child(div().text_size(px(11.)).text_color(c(th.subtext)).overflow_hidden().text_ellipsis().whitespace_nowrap().child(preview)))
                .child(self.row_icon(("lib-ins", i), IconName::CornerDownLeft, t("lib.insert").into(), cx).on_click(cx.listener(move |this, _, w, cx| { cx.stop_propagation(); this.start_snippet(ins_body.clone(), false, w, cx); })))
                .child(self.row_icon(("lib-cp", i), IconName::Copy, t("lib.copy").into(), cx).on_click(cx.listener(move |this, _, _, cx| { cx.stop_propagation(); this.copy_lib(copy_body.clone(), cx); })))
                .child(self.row_icon(("lib-ed", i), IconName::Pencil, t("lib.edit").into(), cx).on_click(cx.listener(move |this, _, w, cx| {
                    cx.stop_propagation();
                    let (id, name, group, note, body) = edit.clone();
                    this.open_lib_editor(Some(id), name, group, note, body, w, cx);
                })))
                .child(self.row_icon(("lib-dup", i), IconName::CopyPlus, t("lib.duplicate").into(), cx).on_click(cx.listener(move |this, _, _, cx| {
                    cx.stop_propagation();
                    match ssh_term_proto::snippets::duplicate(&dup) { Ok(_) => this.lib_notice = t("lib.saved").into(), Err(e) => this.lib_notice = e }
                    cx.notify();
                })))
                .child(self.row_icon(("lib-del", i), IconName::Trash, t("lib.delete").into(), cx).on_click(cx.listener(move |this, _, _, cx| {
                    cx.stop_propagation();
                    if let Err(e) = ssh_term_proto::snippets::delete(&del) { this.lib_notice = e; } else { this.lib_notice.clear(); }
                    cx.notify();
                }))));
        }
        list.into_any_element()
    }

    fn snippet_editor(&mut self, ed: &LibEditor, cx: &mut Context<Self>) -> AnyElement {
        let th = cur();
        // Full-size controls: the dock is narrow, so a 24px compact input reads as cramped next to
        // the editor's own 12px labels.
        let field = |label: &'static str, input: &Entity<InputState>| {
            div().flex().flex_col().gap(px(4.))
                .child(div().text_size(px(12.)).text_color(c(th.overlay1)).child(t(label)))
                .child(ui::text_field(input, false))
        };
        div().id("lib-editor").flex_1().min_h_0().overflow_y_scroll().px_3().py_3().flex().flex_col().gap_3()
            .child(field("lib.name", &ed.name))
            .child(field("lib.group", &ed.group))
            .child(field("lib.note", &ed.note))
            .child(div().flex().flex_col().gap(px(4.))
                .child(div().text_size(px(12.)).text_color(c(th.overlay1)).child(t("lib.body")))
                .child(Textarea::new(&ed.body)))
            .child(div().pt_1().flex().gap_2().justify_end()
                .child(ui::button("lib-ed-cancel", t("ui.cancel"), 0).on_click(cx.listener(|this, _, _, cx| { this.lib_editor = None; cx.notify(); })))
                .child(ui::button("lib-ed-save", t("lib.save"), 1).on_click(cx.listener(|this, _, _, cx| this.save_lib_editor(cx)))))
            .into_any_element()
    }

    fn var_form(&mut self, v: &LibVars, cx: &mut Context<Self>) -> AnyElement {
        let th = cur();
        let mut col = div().id("lib-vars").flex_1().min_h_0().overflow_y_scroll().px_3().py_2().flex().flex_col().gap_2()
            .child(div().text_size(px(12.)).font_weight(FontWeight::MEDIUM).text_color(c(th.text)).child(t("lib.fill_title")));
        for (i, (name, inp)) in v.names.iter().zip(v.inputs.iter()).enumerate() {
            col = col.child(div().flex().flex_col().gap(px(2.))
                .child(div().text_size(px(11.)).text_color(c(th.overlay1)).child(name.clone()))
                .child(ui::text_field(inp, false)));
            let _ = i;
        }
        col.child(div().flex().gap_2().justify_end()
            .child(ui::button("lib-var-cancel", t("ui.cancel"), 0).on_click(cx.listener(|this, _, _, cx| { this.lib_vars = None; cx.notify(); })))
            .child(ui::button("lib-var-ok", if v.exec { t("lib.run") } else { t("lib.insert") }, 1).on_click(cx.listener(|this, _, w, cx| this.submit_vars(w, cx)))))
            .into_any_element()
    }

    fn confirm_form(&mut self, cfn: &LibConfirm, cx: &mut Context<Self>) -> AnyElement {
        let th = cur();
        let mut col = div().id("lib-confirm").flex_1().min_h_0().overflow_y_scroll().px_3().py_2().flex().flex_col().gap_1()
            .child(div().text_size(px(12.)).font_weight(FontWeight::MEDIUM).text_color(c(th.text)).child(t("lib.confirm_title")));
        for (i, reason) in cfn.reasons.iter().enumerate() {
            col = col.child(div().id(("lib-ask", i)).text_size(px(12.)).text_color(c(th.subtext)).child(reason.clone()));
        }
        col.child(div().pt_2().flex().gap_2().justify_end()
            .child(ui::button("lib-ask-no", t("ui.cancel"), 0).on_click(cx.listener(|this, _, _, cx| { this.lib_confirm = None; cx.notify(); })))
            .child(ui::button("lib-ask-yes", t("lib.confirm_ok"), 1).on_click(cx.listener(|this, _, w, cx| this.confirm_script(w, cx)))))
            .into_any_element()
    }

    fn history_list(&mut self, rows: &[HistRow], q: &str, show_host: bool, cx: &mut Context<Self>) -> AnyElement {
        let th = cur();
        let shown: Vec<&HistRow> = rows.iter().filter(|r| {
            if q.is_empty() { return true; }
            r.cmd.to_lowercase().contains(q) || r.key.to_lowercase().contains(q)
        }).take(300).collect();
        let mut list = div().id("lib-hist-list").flex_1().min_h_0().overflow_y_scroll().px_2().py_1().flex().flex_col().gap(px(2.));
        if shown.is_empty() {
            list = list.child(div().p_4().text_size(px(12.)).text_color(c(th.overlay1)).child(t("lib.empty_history")));
        }
        for (i, r) in shown.iter().enumerate() {
            let cmd = r.cmd.clone();
            let key = r.key.clone();
            let run = cmd.clone();
            let ins = cmd.clone();
            let copy = cmd.clone();
            let save = cmd.clone();
            let forget = (key.clone(), cmd.clone());
            let meta = match (show_host, r.ts) {
                (true, Some(ts)) => format!("{} · {}", r.key, fmt_ts(ts)),
                (true, None) => r.key.clone(),
                (false, Some(ts)) => fmt_ts(ts),
                (false, None) => String::new(),
            };
            list = list.child(div().id(("lib-h", i)).px_2().py_1().rounded(px(6.)).flex().items_center().gap_1().cursor_pointer()
                .hover(|d| d.bg(c(cur().surface0)))
                .on_click(cx.listener(move |this, _, w, cx| this.start_snippet(run.clone(), true, w, cx)))
                .child(div().flex_1().min_w_0().flex().flex_col().gap(px(1.))
                    .child(div().text_size(px(13.)).text_color(c(th.text)).overflow_hidden().text_ellipsis().whitespace_nowrap().child(cmd.clone()))
                    .when(!meta.is_empty(), |d| d.child(div().text_size(px(11.)).text_color(c(th.overlay0)).overflow_hidden().text_ellipsis().whitespace_nowrap().child(meta))))
                .child(self.row_icon(("lib-hi", i), IconName::CornerDownLeft, t("lib.insert").into(), cx).on_click(cx.listener(move |this, _, w, cx| { cx.stop_propagation(); this.start_snippet(ins.clone(), false, w, cx); })))
                .child(self.row_icon(("lib-hc", i), IconName::Copy, t("lib.copy").into(), cx).on_click(cx.listener(move |this, _, _, cx| { cx.stop_propagation(); this.copy_lib(copy.clone(), cx); })))
                .child(self.row_icon(("lib-hs", i), IconName::Bookmark, t("lib.save_snippet").into(), cx).on_click(cx.listener(move |this, _, w, cx| { cx.stop_propagation(); this.save_hist_as_snippet(save.clone(), w, cx); })))
                .child(self.row_icon(("lib-hd", i), IconName::Trash, t("lib.delete").into(), cx).on_click(cx.listener(move |this, _, _, cx| {
                    cx.stop_propagation();
                    let (key, cmd) = forget.clone();
                    this.forget_history(&key, &cmd, cx);
                }))));
        }
        list.into_any_element()
    }

    fn row_icon(&self, id: impl Into<gpui::ElementId>, ic: IconName, tip: SharedString, _cx: &mut Context<Self>) -> gpui::Stateful<gpui::Div> {
        let th = cur();
        div().id(id).size(px(22.)).flex_shrink_0().rounded(px(4.)).flex().items_center().justify_center().cursor_pointer()
            .hover(|d| d.bg(c(cur().surface1)))
            .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .tooltip(move |w, cx| gpui_component::tooltip::Tooltip::new(tip.clone()).build(w, cx))
            .child(icon(ic, 13., c(th.overlay1)))
    }

    fn history_rows(&self, window: &Window, cx: &gpui::App) -> Vec<HistRow> {
        let current = self.focused_pane(window, cx).map(|p| (p.read(cx).spec.history_key(), p.read(cx).history.clone()));
        let mut rows = vec![];
        let mut ord = 0u32;
        let push = |key: String, items: &[HistItem], rows: &mut Vec<HistRow>, ord: &mut u32| {
            for it in items {
                rows.push(HistRow { key: key.clone(), cmd: it.cmd.clone(), ts: it.ts, ord: *ord });
                *ord += 1;
            }
        };
        if self.lib_hist_all {
            let mut keys: Vec<String> = self.histories.keys().cloned().collect();
            if let Ok(rd) = std::fs::read_dir(data_dir().join("history")) {
                for e in rd.flatten() {
                    let path = e.path();
                    if path.extension().and_then(|x| x.to_str()) != Some("txt") { continue; }
                    if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                        if !keys.iter().any(|k| k == stem || ssh_term_proto::suggest::history_stem(k) == stem) { keys.push(stem.to_string()); }
                    }
                }
            }
            for key in keys {
                if let Some(h) = self.histories.get(&key) {
                    push(key, &h.lock().unwrap().items().to_vec(), &mut rows, &mut ord);
                } else if current.as_ref().is_some_and(|(k, _)| k == &key) {
                    let items = current.as_ref().unwrap().1.lock().unwrap().items().to_vec();
                    push(key, &items, &mut rows, &mut ord);
                } else {
                    let items = ssh_term_proto::suggest::History::open(ssh_term_proto::suggest::history_path(&data_dir(), &key)).items().to_vec();
                    push(key, &items, &mut rows, &mut ord);
                }
            }
        } else if let Some((key, h)) = current {
            let items = h.lock().unwrap().items().to_vec();
            push(key, &items, &mut rows, &mut ord);
        }
        rows.sort_by(|a, b| b.ts.unwrap_or(0).cmp(&a.ts.unwrap_or(0)).then_with(|| b.ord.cmp(&a.ord)));
        rows
    }

    pub(crate) fn open_lib_editor(&mut self, id: Option<String>, name: String, group: String, note: String, body: String, window: &mut Window, cx: &mut Context<Self>) {
        let name_in = cx.new(|cx| InputState::new(window, cx).placeholder(t("lib.name")).default_value(name));
        let group_in = cx.new(|cx| InputState::new(window, cx).placeholder(t("lib.group")).default_value(group));
        let note_in = cx.new(|cx| InputState::new(window, cx).placeholder(t("lib.note")).default_value(note));
        let body_in = cx.new(|cx| TextareaState::new(window, cx).placeholder(t("lib.body")).auto_grow(3, 8));
        let text = body.clone();
        body_in.update(cx, |s, cx| s.set_value(text, window, cx));
        self.lib_editor = Some(LibEditor { id, name: name_in, group: group_in, note: note_in, body: body_in });
        self.lib_vars = None;
        self.lib_confirm = None;
        self.lib_notice.clear();
        cx.notify();
    }

    fn save_lib_editor(&mut self, cx: &mut Context<Self>) {
        let Some(ed) = self.lib_editor.clone() else { return };
        let name = ed.name.read(cx).value().trim().to_string();
        let body = ed.body.read(cx).value().to_string();
        if name.is_empty() || body.trim().is_empty() {
            self.lib_notice = t("lib.need_name").into();
            cx.notify();
            return;
        }
        // an edited snippet keeps its module; a new one joins the current module
        let module = ed.id.as_ref().and_then(|id| ssh_term_proto::snippets::load().into_iter().find(|s| &s.id == id)).map(|s| s.module).unwrap_or_else(|| self.snippet_module().to_string());
        let item = Snippet {
            module,
            id: ed.id.clone().unwrap_or_else(ssh_term_proto::snippets::new_id),
            name,
            group: ed.group.read(cx).value().trim().to_string(),
            note: ed.note.read(cx).value().trim().to_string(),
            body,
        };
        match ssh_term_proto::snippets::upsert(item) {
            Ok(()) => { self.lib_editor = None; self.lib_notice = t("lib.saved").into(); }
            Err(e) => self.lib_notice = e,
        }
        cx.notify();
    }

    fn start_snippet(&mut self, body: String, exec: bool, window: &mut Window, cx: &mut Context<Self>) {
        let names = ssh_term_proto::snippets::placeholder_names(&body);
        if !names.is_empty() {
            let inputs = names.iter().map(|n| {
                let ph = n.clone();
                cx.new(|cx| InputState::new(window, cx).placeholder(ph))
            }).collect();
            self.lib_vars = Some(LibVars { body, exec, names, inputs });
            self.lib_confirm = None;
            self.lib_notice.clear();
            cx.notify();
            return;
        }
        self.apply_filled(body, exec, window, cx);
    }

    fn submit_vars(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(vars) = self.lib_vars.take() else { return };
        let mut pairs = vec![];
        let mut missing = vec![];
        for (name, inp) in vars.names.iter().zip(vars.inputs.iter()) {
            let val = inp.read(cx).value().to_string();
            if val.trim().is_empty() { missing.push(name.clone()); } else { pairs.push((name.clone(), val)); }
        }
        if !missing.is_empty() {
            self.lib_notice = tf("lib.fill_empty", &[("name", &missing.join(", "))]);
            self.lib_vars = Some(vars);
            cx.notify();
            return;
        }
        match ssh_term_proto::snippets::fill(&vars.body, &pairs) {
            Ok(body) => self.apply_filled(body, vars.exec, window, cx),
            Err(names) => {
                self.lib_notice = tf("lib.fill_empty", &[("name", &names.join(", "))]);
                self.lib_vars = Some(vars);
                cx.notify();
            }
        }
    }

    /// Example snippets for a module that has none yet.
    fn add_starters(&mut self, module: &'static str, cx: &mut Context<Self>) {
        for (name, group, body) in ssh_term_proto::snippets::starters(module) {
            let item = Snippet { id: ssh_term_proto::snippets::new_id(), name: name.to_string(), group: group.to_string(), note: String::new(), body: body.to_string(), module: module.into() };
            if let Err(e) = ssh_term_proto::snippets::upsert(item) { self.lib_notice = e; break; }
        }
        cx.notify();
    }

    /// Insert / run into the current module's own target: SQL editor, Redis console, or (for
    /// terminal, Docker container shells) the terminal. With no target the text is copied.
    fn apply_module(&mut self, body: &str, exec: bool, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let module = self.snippet_module();
        let pane = if self.active_in_ws(cx) { self.focused_pane(window, cx) } else { None };
        if let Some(p) = &pane {
            if let Some(dv) = p.read(cx).db.clone() { dv.update(cx, |v, cx| v.insert_text(body, window, cx)); self.lib_notice.clear(); cx.notify(); return true; }
            if let Some(crate::termview::Embed::Kv(kv)) = p.read(cx).embed.clone() { kv.update(cx, |v, cx| v.insert_console(body, exec, window, cx)); self.lib_notice.clear(); cx.notify(); return true; }
            if p.read(cx).ready() { return false; }
        }
        if module == "terminal" { return false; }
        cx.write_to_clipboard(gpui::ClipboardItem::new_string(body.to_string()));
        self.lib_notice = t("lib.copied_no_target").into();
        cx.notify();
        true
    }

    fn apply_filled(&mut self, body: String, exec: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.lib_vars = None;
        if self.apply_module(&body, exec, window, cx) { return; }
        let Some(p) = self.focused_pane(window, cx).filter(|p| p.read(cx).ready()) else {
            self.lib_notice = t("lib.need_terminal").into();
            cx.notify();
            return;
        };
        if !exec {
            let lines = ssh_term_proto::snippets::script_lines(&body);
            let multiline = lines.len() > 1 || body.contains('\n');
            if !multiline {
                let Some(cmd) = lines.first().cloned().filter(|s| !s.trim().is_empty()) else {
                    self.lib_notice = t("lib.need_name").into();
                    cx.notify();
                    return;
                };
                p.update(cx, |v, cx| v.insert_command(&cmd, cx));
            } else if !p.update(cx, |v, cx| v.insert_block(&body, cx)) {
                self.lib_notice = t("lib.insert_blocked").into();
                cx.notify();
                return;
            }
            self.lib_notice.clear();
            self.focus_active(window, cx);
            cx.notify();
            return;
        }
        let lines = ssh_term_proto::snippets::script_lines(&body);
        if lines.is_empty() { self.lib_notice = t("lib.need_name").into(); cx.notify(); return; }
        match p.read(cx).gate_script(&lines) {
            crate::termview::ScriptGate::Deny { line, why } => { self.lib_notice = tf("lib.denied", &[("line", &line), ("why", &why)]); cx.notify(); }
            crate::termview::ScriptGate::Ask(reasons) => { self.lib_confirm = Some(LibConfirm { lines, reasons }); self.lib_notice.clear(); cx.notify(); }
            crate::termview::ScriptGate::Allow => {
                p.update(cx, |v, cx| v.run_script_lines(&lines, cx));
                self.lib_confirm = None;
                self.lib_notice.clear();
                self.focus_active(window, cx);
                cx.notify();
            }
        }
    }

    fn confirm_script(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(pending) = self.lib_confirm.take() else { return };
        let Some(p) = self.focused_pane(window, cx).filter(|p| p.read(cx).ready()) else {
            self.lib_notice = t("lib.need_terminal").into();
            cx.notify();
            return;
        };
        p.update(cx, |v, cx| v.run_script_lines(&pending.lines, cx));
        self.focus_active(window, cx);
        cx.notify();
    }

    fn copy_lib(&mut self, text: String, cx: &mut Context<Self>) {
        cx.write_to_clipboard(ClipboardItem::new_string(text));
        self.lib_notice = t("lib.copied").into();
        cx.notify();
    }

    fn save_hist_as_snippet(&mut self, cmd: String, window: &mut Window, cx: &mut Context<Self>) {
        self.dock_snip = true;
        self.dock_min.remove("snip-dock");
        let name: String = cmd.trim().chars().take(40).collect();
        self.open_lib_editor(None, name, String::new(), String::new(), cmd, window, cx);
    }

    fn forget_history(&mut self, key: &str, cmd: &str, cx: &mut Context<Self>) {
        if let Some(h) = self.histories.get(key) { h.lock().unwrap().remove(cmd); }
        else {
            let mut h = ssh_term_proto::suggest::History::open(ssh_term_proto::suggest::history_path(&data_dir(), key));
            h.remove(cmd);
        }
        for p in self.tabs.iter().flat_map(|t| t.layout.leaves()) {
            if p.read(cx).spec.history_key() == key { p.read(cx).history.lock().unwrap().remove(cmd); }
        }
        cx.notify();
    }

    fn clear_current_history(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(p) = self.focused_pane(window, cx) else { self.lib_notice = t("lib.no_host").into(); cx.notify(); return; };
        let key = p.read(cx).spec.history_key();
        if let Some(h) = self.histories.get(&key) { h.lock().unwrap().clear(); }
        else {
            let mut h = ssh_term_proto::suggest::History::open(ssh_term_proto::suggest::history_path(&data_dir(), &key));
            h.clear();
        }
        for pane in self.tabs.iter().flat_map(|t| t.layout.leaves()) {
            if pane.read(cx).spec.history_key() == key { pane.read(cx).history.lock().unwrap().clear(); }
        }
        self.lib_notice.clear();
        cx.notify();
    }
}

fn snip_visible(s: &Snippet, q: &str, group: &str) -> bool {
    if group == UNGROUPED && !s.group.is_empty() { return false; }
    if !group.is_empty() && group != UNGROUPED && s.group != group { return false; }
    if q.is_empty() { return true; }
    format!("{} {} {} {}", s.name, s.group, s.note, s.body).to_lowercase().contains(q)
}

/// Label of a snippet module for the tool window header.
pub(crate) fn module_label(m: &str) -> String {
    match m { "sftp" => t("tool.sftp"), "database" => t("tool.database"), "redis" => t("tool.redis"), "docker" => t("tool.docker"), _ => t("lib.module_terminal") }.to_string()
}
