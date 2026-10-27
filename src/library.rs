//! Right-hand docks for saved commands and command history. Same card as the AI assistant.
use gpui::{anchored, deferred, div, prelude::*, px, AnyElement, ClipboardItem, Context, Entity, FontWeight, Pixels, Point, SharedString, Window};
use gpui_component::input::{InputState, Textarea, TextareaState};
use ssh_term_proto::i18n::{t, tf};
use ssh_term_proto::snippets::Snippet;
use ssh_term_proto::suggest::HistItem;

use crate::theme::{c, cur};
use crate::ui::{self, IconName};
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

/// 行内「更多」菜单针对的那一行。
#[derive(Clone)]
pub(crate) enum LibMenu {
    Snippet { id: String, name: String, group: String, note: String, body: String },
    History { key: String, cmd: String },
}

impl LibMenu {
    /// 菜单归属：片段卡片为 true，历史卡片为 false。
    fn owner_is_snippets(&self) -> bool { matches!(self, LibMenu::Snippet { .. }) }
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
        let mut actions: Vec<AnyElement> = vec![];
        let mut body = div().flex_1().min_h_0().flex().flex_col();
        body = body.child(ui::field_row().pb(px(ui::SP_2)).child(ui::search_field(&search, false)));
        if !ready {
            body = body.child(ui::notice(ui::Notice::Info, t("lib.need_terminal")).mb(px(ui::SP_2)));
        }
        if snippets_mode {
            let mut all = ssh_term_proto::snippets::load();
            all.retain(|s| s.module == module);
            body = body.child(self.snippet_groups(&all, &group, cx));
            if let Some(ed) = editor {
                body = body.child(self.snippet_editor(&ed, cx));
            } else if let Some(v) = vars {
                body = body.child(self.var_form(&v, cx));
            } else if let Some(cfn) = confirm {
                body = body.child(self.confirm_form(&cfn, cx));
            } else {
                actions.push(ui::icon_button("lib-new", IconName::Plus, ui::IconBtn::Md, t("lib.new")).on_click(cx.listener(|this, _, w, cx| {
                    this.open_lib_editor(None, String::new(), String::new(), String::new(), String::new(), w, cx);
                })).into_any_element());
                body = body.child(self.snippet_list(&all, &q, &group, module, cx));
            }
        } else {
            let opts = [("this".to_string(), t("lib.this_host").to_string()), ("all".to_string(), t("lib.all_hosts").to_string())];
            let me = cx.entity();
            body = body.child(div().w_full().flex_shrink_0().px(px(ui::LIST_INSET)).pb(px(ui::SP_2))
                .child(ui::segmented_fill("lib-host", &opts, if hist_all { "all" } else { "this" }, move |v, _w, cx| { me.update(cx, |this, cx| { this.lib_hist_all = v == "all"; cx.notify(); }); })));
            if let Some(v) = vars {
                body = body.child(self.var_form(&v, cx));
            } else if let Some(cfn) = confirm {
                body = body.child(self.confirm_form(&cfn, cx));
            } else {
                actions.push(ui::icon_button_state("lib-clear-host", IconName::Trash, ui::IconBtn::Md, t("lib.clear_host"), false, true)
                    .on_click(cx.listener(|this, _, w, cx| this.clear_current_history(w, cx))).into_any_element());
                let rows = self.history_rows(window, cx);
                body = body.child(self.history_list(&rows, &q, hist_all, cx));
            }
        }
        if !notice.is_empty() {
            body = body.child(ui::notice(ui::Notice::Warn, notice).my(px(ui::SP_2)));
        }
        // 菜单只由发起它的卡片渲染，避免两张卡片同时打开时重复渲染同 ID 的菜单
        if let Some((target, pos)) = self.lib_menu.clone().filter(|(t, _)| t.owner_is_snippets() == snippets_mode) {
            body = body.child(self.lib_menu_el(target, pos, cx));
        }
        let (id, title, sub, ic) = if snippets_mode { ("snip-dock", t("lib.snippets"), Some(SharedString::from(module_label(module))), IconName::Code) } else { ("hist-dock", t("lib.history"), None, IconName::Clock) };
        self.dock_frame(id, title.into(), sub, ic, actions, body, cx)
    }

    fn snippet_groups(&mut self, all: &[Snippet], group: &str, cx: &mut Context<Self>) -> gpui::Stateful<gpui::Div> {
        let mut names: Vec<String> = all.iter().map(|s| s.group.clone()).filter(|g| !g.is_empty()).collect();
        names.sort();
        names.dedup();
        let ungrouped = all.iter().any(|s| s.group.is_empty());
        // 单行不换行，超出横向滚动
        let mut row = div().id("lib-groups").w_full().flex_shrink_0().px(px(ui::LIST_INSET)).pb(px(ui::SP_2)).flex().flex_nowrap().gap(px(ui::SP_1)).overflow_x_scroll()
            .child(ui::chip("lib-g-all", t("lib.all_groups"), group.is_empty()).on_click(cx.listener(|this, _, _, cx| { this.lib_group.clear(); cx.notify(); })));
        for (i, name) in names.iter().enumerate() {
            let n = name.clone();
            let on = group == name;
            row = row.child(ui::chip(("lib-g", i), name.clone(), on).on_click(cx.listener(move |this, _, _, cx| { this.lib_group = n.clone(); cx.notify(); })));
        }
        if ungrouped {
            row = row.child(ui::chip("lib-g-none", t("lib.ungrouped"), group == UNGROUPED).on_click(cx.listener(|this, _, _, cx| { this.lib_group = UNGROUPED.into(); cx.notify(); })));
        }
        row
    }

    fn snippet_list(&mut self, all: &[Snippet], q: &str, group: &str, module: &'static str, cx: &mut Context<Self>) -> AnyElement {
        let th = cur();
        let shown: Vec<Snippet> = all.iter().filter(|s| snip_visible(s, q, group)).cloned().collect();
        if all.is_empty() {
            return ui::empty_state(IconName::Code, t("lib.empty_snippets_title"), Some(t("lib.empty_snippets_desc").into()), vec![
                ui::button("lib-new-empty", t("lib.new"), 1).on_click(cx.listener(|this, _, w, cx| {
                    this.open_lib_editor(None, String::new(), String::new(), String::new(), String::new(), w, cx);
                })).into_any_element(),
                ui::button("lib-starters", t("lib.add_examples"), 0).on_click(cx.listener(move |this, _, _w, cx| this.add_starters(module, cx))).into_any_element(),
            ], false).into_any_element();
        }
        if shown.is_empty() {
            return ui::empty_state(IconName::Search, t("ui.no_matching_commands"), None, vec![], false).into_any_element();
        }
        let mut list = div().id("lib-snip-list").flex_1().min_h_0().overflow_y_scroll().pb(px(ui::SP_2)).flex().flex_col();
        for (i, s) in shown.iter().enumerate() {
            let body = s.body.clone();
            let group_name = s.group.clone();
            let preview: String = s.body.lines().find(|l| !l.trim().is_empty()).unwrap_or("").trim().chars().take(80).collect();
            let run_body = body.clone();
            let ins_body = body.clone();
            let copy_body = body.clone();
            let menu = LibMenu::Snippet { id: s.id.clone(), name: s.name.clone(), group: group_name.clone(), note: s.note.clone(), body };
            let gid = SharedString::from(format!("lib-sn-{i}"));
            list = list.child(ui::list_row(("lib-sn", i), 0, false).min_h(px(ui::row2_h())).group(gid.clone())
                .on_click(cx.listener(move |this, _, w, cx| this.start_snippet(run_body.clone(), true, w, cx)))
                .child(div().flex_1().min_w_0().flex().flex_col()
                    .child(div().flex().items_center().gap(px(ui::SP_1))
                        .child(ui::row_title(s.name.clone()).font_weight(FontWeight::MEDIUM))
                        .when(!group_name.is_empty(), |d| d.child(div().flex_shrink_0().child(ui::tag(group_name.clone(), c(th.subtext))))))
                    .child(self.mono_line(preview, c(th.subtext))))
                .child(ui::row_actions(gid)
                    .child(self.act_btn(("lib-ins", i), IconName::CornerDownLeft, t("lib.insert")).on_click(cx.listener(move |this, _, w, cx| { cx.stop_propagation(); this.start_snippet(ins_body.clone(), false, w, cx); })))
                    .child(self.act_btn(("lib-cp", i), IconName::Copy, t("lib.copy")).on_click(cx.listener(move |this, _, _, cx| { cx.stop_propagation(); this.copy_lib(copy_body.clone(), cx); })))
                    .child(self.more_btn(("lib-more", i), menu, cx))));
        }
        list.into_any_element()
    }

    fn snippet_editor(&mut self, ed: &LibEditor, cx: &mut Context<Self>) -> AnyElement {
        let th = cur();
        let label = |s: &'static str| div().text_size(px(ui::fs_small())).text_color(c(th.subtext)).child(t(s));
        let field = |name: &'static str, input: &Entity<InputState>| div().flex().flex_col().gap(px(ui::SP_1)).child(label(name)).child(ui::text_field(input, false));
        div().id("lib-editor").flex_1().min_h_0().overflow_y_scroll().px(px(ui::PANEL_PX)).py(px(ui::SP_3)).flex().flex_col().gap(px(ui::SP_3))
            .child(field("lib.name", &ed.name))
            .child(field("lib.group", &ed.group))
            .child(field("lib.note", &ed.note))
            .child(div().flex().flex_col().gap(px(ui::SP_1))
                .child(label("lib.body"))
                .child(Textarea::new(&ed.body).min_h(px(120.)).font_family(ui::mono_family()).text_size(px(ui::fs_mono()))))
            .child(div().pt(px(ui::SP_1)).flex().gap(px(ui::SP_2)).justify_end()
                .child(ui::button("lib-ed-cancel", t("ui.cancel"), 0).on_click(cx.listener(|this, _, _, cx| { this.lib_editor = None; cx.notify(); })))
                .child(ui::button("lib-ed-save", t("lib.save"), 1).on_click(cx.listener(|this, _, _, cx| this.save_lib_editor(cx)))))
            .into_any_element()
    }

    fn var_form(&mut self, v: &LibVars, cx: &mut Context<Self>) -> AnyElement {
        let th = cur();
        let mut col = div().id("lib-vars").flex_1().min_h_0().overflow_y_scroll().px(px(ui::PANEL_PX)).py(px(ui::SP_3)).flex().flex_col().gap(px(ui::SP_3))
            .child(div().text_size(px(ui::fs_body())).font_weight(FontWeight::MEDIUM).text_color(c(th.text)).child(t("lib.fill_title")));
        for (name, inp) in v.names.iter().zip(v.inputs.iter()) {
            col = col.child(div().flex().flex_col().gap(px(ui::SP_1))
                .child(div().text_size(px(ui::fs_small())).text_color(c(th.subtext)).child(name.clone()))
                .child(ui::text_field(inp, false)));
        }
        col.child(div().flex().gap(px(ui::SP_2)).justify_end()
            .child(ui::button("lib-var-cancel", t("ui.cancel"), 0).on_click(cx.listener(|this, _, _, cx| { this.lib_vars = None; cx.notify(); })))
            .child(ui::button("lib-var-ok", if v.exec { t("lib.run") } else { t("lib.insert") }, 1).on_click(cx.listener(|this, _, w, cx| this.submit_vars(w, cx)))))
            .into_any_element()
    }

    fn confirm_form(&mut self, cfn: &LibConfirm, cx: &mut Context<Self>) -> AnyElement {
        let th = cur();
        let mut col = div().id("lib-confirm").flex_1().min_h_0().overflow_y_scroll().px(px(ui::PANEL_PX)).py(px(ui::SP_3)).flex().flex_col().gap(px(ui::SP_3))
            .child(div().text_size(px(ui::fs_body())).font_weight(FontWeight::MEDIUM).text_color(c(th.text)).child(t("lib.confirm_title")));
        for (i, reason) in cfn.reasons.iter().enumerate() {
            col = col.child(ui::notice(ui::Notice::Warn, reason.clone()).mx_0().id(("lib-ask", i)));
        }
        col.child(div().flex().gap(px(ui::SP_2)).justify_end()
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
        if rows.is_empty() {
            return ui::empty_state(IconName::Clock, t("lib.empty_history_title"), None, vec![], false).into_any_element();
        }
        if shown.is_empty() {
            return ui::empty_state(IconName::Search, t("ui.no_matching_commands"), None, vec![], false).into_any_element();
        }
        let mut list = div().id("lib-hist-list").flex_1().min_h_0().overflow_y_scroll().pb(px(ui::SP_2)).flex().flex_col();
        for (i, r) in shown.iter().enumerate() {
            let cmd = r.cmd.clone();
            let run = cmd.clone();
            let ins = cmd.clone();
            let copy = cmd.clone();
            let menu = LibMenu::History { key: r.key.clone(), cmd: cmd.clone() };
            let meta = match (show_host, r.ts) {
                (true, Some(ts)) => format!("{} · {}", r.key, fmt_ts(ts)),
                (true, None) => r.key.clone(),
                (false, Some(ts)) => fmt_ts(ts),
                (false, None) => String::new(),
            };
            let gid = SharedString::from(format!("lib-h-{i}"));
            list = list.child(ui::list_row(("lib-h", i), 0, false).min_h(px(ui::row2_h())).group(gid.clone())
                .on_click(cx.listener(move |this, _, w, cx| this.start_snippet(run.clone(), true, w, cx)))
                .child(div().flex_1().min_w_0().flex().flex_col()
                    .child(self.mono_line(cmd, c(th.text)))
                    .when(!meta.is_empty(), |d| d.child(ui::row_meta(meta))))
                .child(ui::row_actions(gid)
                    .child(self.act_btn(("lib-hi", i), IconName::CornerDownLeft, t("lib.insert")).on_click(cx.listener(move |this, _, w, cx| { cx.stop_propagation(); this.start_snippet(ins.clone(), false, w, cx); })))
                    .child(self.act_btn(("lib-hc", i), IconName::Copy, t("lib.copy")).on_click(cx.listener(move |this, _, _, cx| { cx.stop_propagation(); this.copy_lib(copy.clone(), cx); })))
                    .child(self.more_btn(("lib-hm", i), menu, cx))));
        }
        list.into_any_element()
    }

    /// 单行省略的等宽文本（命令预览、历史命令）。
    fn mono_line(&self, text: impl Into<SharedString>, color: gpui::Hsla) -> gpui::Div {
        div().min_w_0().whitespace_nowrap().overflow_hidden().text_ellipsis().font_family(ui::mono_family()).text_size(px(ui::fs_mono())).line_height(px(ui::lh(ui::fs_mono()))).text_color(color).child(text.into())
    }

    /// 行内图标按钮：按下不触发整行点击。
    fn act_btn(&self, id: impl Into<gpui::ElementId>, ic: IconName, tip: impl Into<SharedString>) -> gpui::Stateful<gpui::Div> {
        ui::icon_button(id, ic, ui::IconBtn::Sm, tip).on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
    }

    /// 「更多」按钮：在按钮下方弹出该行的小菜单。
    fn more_btn(&self, id: impl Into<gpui::ElementId>, target: LibMenu, cx: &mut Context<Self>) -> gpui::Stateful<gpui::Div> {
        self.act_btn(id, IconName::Ellipsis, t("lib.more")).on_click(cx.listener(move |this, e: &gpui::ClickEvent, _, cx| {
            cx.stop_propagation();
            this.lib_menu = Some((target.clone(), e.position() + Point::new(px(-8.), px(14.))));
            cx.notify();
        }))
    }

    /// 「更多」菜单：常用命令 = 编辑 / 复制一份 / 删除；历史 = 存为常用命令 / 删除。
    fn lib_menu_el(&mut self, target: LibMenu, pos: Point<Pixels>, cx: &mut Context<Self>) -> AnyElement {
        let mut m = ui::popover().py_1().flex().flex_col();
        let close = |this: &mut Root| { this.lib_menu = None; };
        match target {
            LibMenu::Snippet { id, name, group, note, body } => {
                let (dup, del) = (id.clone(), id.clone());
                m = m.child(ui::menu_item("lib-m-edit", Some(IconName::Pencil), t("lib.edit"), true, false).on_click(cx.listener(move |this, _, w, cx| {
                    close(this);
                    this.open_lib_editor(Some(id.clone()), name.clone(), group.clone(), note.clone(), body.clone(), w, cx);
                })))
                .child(ui::menu_item("lib-m-dup", Some(IconName::CopyPlus), t("lib.duplicate"), true, false).on_click(cx.listener(move |this, _, _, cx| {
                    close(this);
                    match ssh_term_proto::snippets::duplicate(&dup) { Ok(_) => this.lib_notice = t("lib.saved").into(), Err(e) => this.lib_notice = e }
                    cx.notify();
                })))
                .child(ui::menu_sep())
                .child(ui::menu_item("lib-m-del", Some(IconName::Trash), t("lib.delete"), true, true).on_click(cx.listener(move |this, _, _, cx| {
                    close(this);
                    if let Err(e) = ssh_term_proto::snippets::delete(&del) { this.lib_notice = e; } else { this.lib_notice.clear(); }
                    cx.notify();
                })));
            }
            LibMenu::History { key, cmd } => {
                let save = cmd.clone();
                m = m.child(ui::menu_item("lib-m-save", Some(IconName::Bookmark), t("lib.save_snippet"), true, false).on_click(cx.listener(move |this, _, w, cx| {
                    close(this);
                    this.save_hist_as_snippet(save.clone(), w, cx);
                })))
                .child(ui::menu_sep())
                .child(ui::menu_item("lib-m-forget", Some(IconName::Trash), t("lib.delete"), true, true).on_click(cx.listener(move |this, _, _, cx| {
                    close(this);
                    this.forget_history(&key, &cmd, cx);
                })));
            }
        }
        deferred(anchored().position(pos).child(m.occlude().on_mouse_down_out(cx.listener(|this, _, _, cx| { this.lib_menu = None; cx.notify(); })))).with_priority(2).into_any_element()
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
