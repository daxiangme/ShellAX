//! Sidebar host manager (PRD 5.23): pinned local terminal, user-managed tree of groups and
//! connections (drag to reorder / move, collapse persisted), Finder color tags with a tag
//! filter section, "+" menu, right-click menus, ~/.ssh/config import, rename / delete dialogs.
use gpui::AnimationExt as _;
use gpui::{div, prelude::*, px, AnyElement, Context, Entity, FontWeight, SharedString, Window};
use gpui_component::input::{InputState};
use ssh_term_proto::hosts::HostEntry;
use ssh_term_proto::hosttree::{self, Row};
use ssh_term_proto::i18n::{t, tf};
use ssh_term_proto::sftp_mode::Mode as SftpMode;
use std::sync::atomic::Ordering;

use crate::actions::Action;
use crate::card_layout;
use crate::termview::Spec;
use crate::theme::{c, cur};
use crate::ui::{self, icon, IconName};
use crate::{chrome, config, HostDrag, Root};

pub(crate) fn action_tip(label: &str, binding: &str) -> String {
    if binding.is_empty() { return label.to_string(); }
    let keys = binding.split('-').map(|p| if p.len() == 1 { p.to_uppercase() } else {
        let mut c = p.chars();
        c.next().map(|f| f.to_uppercase().collect::<String>() + c.as_str()).unwrap_or_default()
    }).collect::<Vec<_>>().join("+");
    format!("{label}  ({keys})")
}

/// Command that enters `path`. Windows local shells are PowerShell; SSH shells are POSIX.
fn cd_command(path: &str, local_windows: bool) -> String {
    if local_windows {
        format!("Set-Location -LiteralPath '{}'", path.replace('\'', "''"))
    } else {
        format!("cd -- '{}'", path.replace('\'', "'\\''"))
    }
}
use ssh_term_proto::layout::Dir;

/// What a sidebar context menu is about.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum SbTarget { Host(String), Group(String), Local, Place(usize), Empty }
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum SbSub { Main, Move }
#[derive(Clone)]
pub(crate) struct SbMenu { pub target: SbTarget, pub pos: gpui::Point<gpui::Pixels>, pub sub: SbSub }

#[derive(Clone)]
pub(crate) enum PromptKind { NewGroup(String), RenameHost(String), RenameGroup(String), SavePlace { path: String, host: String, group: String }, PlacePath { host: String, group: String }, RenamePlace(usize) }
#[derive(Clone)]
pub(crate) enum SbModal {
    Prompt { kind: PromptKind, input: Entity<InputState> },
    DeleteHost(String),
    DeleteGroup(String),
    Import { cands: Vec<HostEntry>, checked: Vec<bool>, group: Entity<InputState> },
    /// Double-click on a sidebar entry: open another tab? (+ "don't ask again")
    #[allow(dead_code)]
    NewTab { spec: Spec, label: String, dont_ask: bool },
}

/// Dragging a group row.
/// Drag payload of the sidebar's right-edge resize handle.
#[derive(Clone)]
pub(crate) struct SbResize;
impl Render for SbResize { fn render(&mut self, _w: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement { div() } }

/// Drag payload of the AI dock's left-edge resize handle.
#[derive(Clone)]
pub(crate) struct AiResize;
impl Render for AiResize { fn render(&mut self, _w: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement { div() } }

#[derive(Clone)]
pub(crate) struct GroupDrag { pub path: String }
impl Render for GroupDrag {
    fn render(&mut self, _w: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let th = cur();
        div().px_3().py_1().rounded(px(ui::R_MD)).bg(c(th.base)).border_1().border_color(c(th.accent)).flex().items_center().gap_1()
            .child(icon(IconName::Folder, ui::IC_XS, c(th.accent))).text_size(px(ui::fs_body())).text_color(c(th.text)).child(hosttree::leaf_of(&self.path).to_string())
    }
}

impl Root {
    fn show_sb_menu(&mut self, menu: SbMenu) {
        self.sb_menu_at = std::time::Instant::now();
        self.sb_menu = Some(menu);
    }
    fn sb_save(&mut self, what: &str, detail: String, f: impl FnOnce(&mut config::Settings), cx: &mut Context<Self>) {
        let r = config::update(|s| { f(s); hosttree::ensure_groups(&s.hosts, &mut s.groups); });
        if let Some(e) = r { self.toast(e, cx); }
        self.log(what, detail);
        cx.notify();
    }
    /// Resolved SSH target of a config host.
    pub(crate) fn target_for(&self, name: &str) -> Option<ssh_term_proto::ssh::SshTarget> {
        self.host_targets().into_iter().find(|(h, _)| h.name == name).map(|(_, t)| t)
    }
    fn toggle_collapsed(&mut self, path: &str, cx: &mut Context<Self>) {
        let p = path.to_string();
        let r = config::update(|s| { if let Some(i) = s.sidebar.collapsed.iter().position(|c| *c == p) { s.sidebar.collapsed.remove(i); } else { s.sidebar.collapsed.push(p); } });
        if let Some(e) = r { self.toast(e, cx); }
        cx.notify();
    }
    /// New terminal for `spec` split off the focused pane (opens a tab if there is none).
    pub(crate) fn split_with(&mut self, spec: Spec, dir: Dir, window: &mut Window, cx: &mut Context<Self>) {
        if self.block_proxy(&spec, true, window, cx) { return; }
        let Some(target) = self.focused_pane(window, cx) else { return self.new_tab(spec, window, cx) };
        let pane = self.open_pane(spec, window, cx);
        let fh = pane.read(cx).focus.clone();
        if let Some(tab) = self.tabs.get_mut(self.active) { tab.layout.split(&target, pane.clone(), dir); tab.focused = pane; }
        window.focus(&fh, cx);
        cx.notify();
    }
    fn place_host(&self, spec: &Spec) -> String {
        match spec {
            Spec::Local => String::new(),
            Spec::Ssh(t) => self.host_targets().into_iter().find(|(_, x)| x.host == t.host && x.user == t.user && x.port == t.port).map(|(h, _)| h.name).unwrap_or_else(|| t.label()),
        }
    }
    fn menu_group(&self, target: &SbTarget) -> String {
        let s = config::get();
        match target {
            SbTarget::Group(p) => p.clone(),
            SbTarget::Host(n) => s.hosts.iter().find(|h| &h.name == n).and_then(|h| h.group.clone()).unwrap_or_default(),
            SbTarget::Place(i) => s.places.get(*i).map(|p| p.group.clone()).unwrap_or_default(),
            _ => String::new(),
        }
    }
    fn add_place(&mut self, group: String, window: &mut Window, cx: &mut Context<Self>) {
        self.prompt(PromptKind::PlacePath { host: String::new(), group }, String::new(), window, cx);
    }
    fn save_current_place(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(pane) = self.tabs.get(self.active).and_then(|t| t.layout.leaves().into_iter().find(|p| p.read(cx).focus.is_focused(window))).or_else(|| self.focused_pane(window, cx)) else {
            self.toast(t("place.need_terminal").into(), cx);
            return;
        };
        let view = pane.read(cx);
        if view.embedded() { self.toast(t("place.need_terminal").into(), cx); return; }
        let host = self.place_host(&view.spec);
        let group = if host.is_empty() { String::new() } else { config::get().hosts.iter().find(|h| h.name == host).and_then(|h| h.group.clone()).unwrap_or_default() };
        match view.shell_cwd().filter(|p| !p.is_empty()) {
            Some(path) => {
                let name = path.rsplit(['/', '\\']).next().unwrap_or(&path).to_string();
                self.prompt(PromptKind::SavePlace { path, host, group }, name, window, cx);
            }
            None => self.prompt(PromptKind::PlacePath { host, group }, String::new(), window, cx),
        }
    }
    fn open_place(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(place) = config::get().places.get(index).cloned() else { return };
        let spec = if place.host.is_empty() {
            Spec::Local
        } else if let Some(t) = self.target_for(&place.host) {
            Spec::Ssh(t)
        } else if self.default_target.as_ref().is_some_and(|t| t.label() == place.host) {
            Spec::Ssh(self.default_target.clone().unwrap())
        } else {
            self.toast(tf("place.missing_host", &[("name", &place.host)]), cx);
            return;
        };
        let local_win = place.host.is_empty() && cfg!(windows);
        let pane = self.open_pane(spec, window, cx);
        pane.update(cx, |v, cx| v.queue_cd(cd_command(&place.path, local_win), cx));
        let fh = pane.read(cx).focus.clone();
        self.tabs.push(crate::Tab { layout: ssh_term_proto::layout::Node::Leaf(pane.clone()), focused: pane, name: Some(place.name) });
        self.active = self.tabs.len() - 1;
        self.leave_settings();
        self.overlay = crate::Overlay::None;
        window.focus(&fh, cx);
        cx.notify();
    }
    fn prompt(&mut self, kind: PromptKind, value: String, window: &mut Window, cx: &mut Context<Self>) {
        let input = cx.new(|cx| InputState::new(window, cx).default_value(value));
        input.update(cx, |s, cx| s.focus(window, cx));
        self.sb_modal = Some(SbModal::Prompt { kind, input });
        cx.notify();
    }
    fn prompt_ok(&mut self, kind: PromptKind, val: String, window: &mut Window, cx: &mut Context<Self>) {
        let val = val.trim().to_string();
        if val.is_empty() { return; }
        match kind {
            PromptKind::NewGroup(parent) => {
                let mut made = String::new();
                self.sb_save("hosts.group_new", format!("parent={parent:?} name={val:?}"), |s| { made = hosttree::add_group(&mut s.groups, &parent, &val); }, cx);
                // make sure the new group is visible
                if !parent.is_empty() && config::get().sidebar.collapsed.contains(&parent) { self.toggle_collapsed(&parent, cx); }
            }
            PromptKind::RenameHost(old) => {
                if config::get().hosts.iter().any(|h| h.name == val && h.name != old) { return self.toast(tf("host.exists", &[("name", &val)]), cx); }
                self.sb_save("hosts.rename", format!("from={old:?} to={val:?}"), |s| { hosttree::rename_host(&mut s.hosts, &old, &val); }, cx);
            }
            PromptKind::RenameGroup(old) => {
                let mut ok = true;
                self.sb_save("hosts.group_rename", format!("from={old:?} to={val:?}"), |s| {
                    let (h, g) = (&mut s.hosts, &mut s.groups);
                    match hosttree::rename_group(h, g, &old, &val) {
                        Some(new) => for c in s.sidebar.collapsed.iter_mut() { if hosttree::under(c, &old) { *c = format!("{new}{}", &c[old.len()..]); } },
                        None => ok = false,
                    }
                }, cx);
                if !ok { self.toast(tf("sb.group_exists", &[("name", &val)]), cx); }
            }
            PromptKind::PlacePath { host, group } => {
                let name = val.rsplit(['/', '\\']).next().unwrap_or(&val).to_string();
                self.prompt(PromptKind::SavePlace { path: val, host, group }, name, window, cx);
                return;
            }
            PromptKind::SavePlace { path, host, group } => {
                self.sb_save("places.save", format!("name={val:?} path={path:?} host={host:?} group={group:?}"), |s| s.places.push(config::Place { name: val.clone(), path, host, group }), cx);
            }
            PromptKind::RenamePlace(i) => {
                self.sb_save("places.rename", format!("i={i} name={val:?}"), |s| { if let Some(p) = s.places.get_mut(i) { p.name = val.clone(); } }, cx);
            }
        }
        self.sb_modal = None;
    }
    fn connect_all(&mut self, path: &str, window: &mut Window, cx: &mut Context<Self>) {
        let s = config::get();
        let names: Vec<String> = hosttree::hosts_under(&s.hosts, path).into_iter().map(|i| s.hosts[i].name.clone()).collect();
        self.log("hosts.connect_all", format!("group={path:?} n={}", names.len()));
        for n in names { if let Some(t) = self.target_for(&n) { self.new_tab(Spec::Ssh(t), window, cx); } }
    }
    pub(crate) fn open_import(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let s = config::get();
        let cands = hosttree::importable(&s.hosts, &s.ssh_config_hosts());
        let checked = vec![true; cands.len()];
        let group = cx.new(|cx| InputState::new(window, cx).placeholder(t("sb.import_group_ph")));
        self.sb_modal = Some(SbModal::Import { cands, checked, group });
        cx.notify();
    }

    // ------------------------------------------------------------------ rows
    /// 实体图标 + 右上角连接状态点（灰空闲 / 绿已连接 / 琥珀呼吸连接中 / 红失败），生产环境另在右下加盾牌。
    fn entity_icon(ic: IconName, st: u8, anim_id: usize, shield: Option<gpui::Hsla>) -> gpui::Div {
        let th = cur();
        // 状态点描边用卡片底色，与岛融为一体
        let dot = div().absolute().top(px(-2.)).right(px(-2.)).size(px(7.)).rounded_full().border_1().border_color(crate::theme::card_bg())
            .bg(match st { 2 => c(th.success), 1 => c(th.warning), 3 => c(th.error), _ => c(th.overlay0) });
        let dot = if st == 1 && !crate::motion::reduced() {
            dot.with_animation(("host-dot", anim_id), gpui::Animation::new(crate::motion::ms(crate::motion::BREATHE_MS)).repeat().with_max_fps(30.).with_easing(|t| (t * std::f32::consts::PI).sin()), |d, t| d.opacity(0.35 + 0.65 * t)).into_any_element()
        } else { dot.into_any_element() };
        div().relative().size(px(ui::IC_MD)).flex_shrink_0()
            .child(icon(ic, ui::IC_MD, c(th.overlay1)))
            .child(dot)
            .children(shield.map(|col| div().absolute().bottom(px(-3.)).right(px(-4.)).child(icon(IconName::ShieldAlert, ui::IC_BADGE, col))))
    }

    fn sb_host_row(&self, idx: usize, h: &HostEntry, depth: usize, ctx: &RowCtx, cx: &mut Context<Self>) -> AnyElement {
        let s = config::get();
        let target = ctx.targets.iter().find(|(e, _)| e.name == h.name).map(|(_, t)| t.clone());
        let label = target.as_ref().map(|t| t.label()).unwrap_or_else(|| h.host.clone());
        let st = ctx.open.iter().filter(|(l, _)| *l == label).map(|(_, s)| *s).max_by_key(|s| match s { 2 => 3, 1 => 2, 3 => 1, _ => 0 }).unwrap_or(0);
        let sel = ctx.selected.as_deref() == Some(label.as_str());
        let prod = s.host_is_production(h);
        let os = target.as_ref().and_then(|t| ctx.host_os.get(&t.pool_key()).cloned());
        let detail = match os { Some(o) => format!("{label} · {}", ssh_term_proto::outline::badge(&o).1), None => label.clone() };
        let gid = SharedString::from(format!("hr-g-{idx}"));
        let name = h.name.clone();
        // 比同级分组多一级缩进（对齐分组的展开箭头槽位）
        let mut row = ui::list_row(("host", idx), depth + 1, sel).min_h(px(ui::row2_h())).group(gid.clone())
            // 一个信号一件事：单色服务器图标，右上角连接状态点，生产环境右下盾牌；标签最多 3 个色点（+N）
            .child(Self::entity_icon(IconName::Server, st, idx, prod.then(|| c(s.tags.rgb(&s.tags.production)))))
            .child(div().flex_1().min_w_0().flex().flex_col().overflow_hidden()
                .child(div().flex().items_center().gap(px(4.))
                    .child(ui::row_title(h.name.clone()))
                    .children(h.tags.iter().take(3).map(|t| div().size(px(6.)).flex_shrink_0().rounded_full().bg(c(s.tags.rgb(t)))))
                    .when(h.tags.len() > 3, |d| d.child(div().text_size(px(ui::fs_caption())).text_color(c(cur().subtext)).child(format!("+{}", h.tags.len() - 3)))))
                .child(ui::row_meta(detail.clone())));
        {
            // full name + user@host:port on hover (names are often truncated)
            let tip = format!("{}\n{}", h.name, label);
            row = row.tooltip(move |w, cx| gpui_component::tooltip::Tooltip::new(tip.clone()).build(w, cx));
        }
        {
            let n = name.clone();
            row = row.on_click(cx.listener(move |this, ev: &gpui::ClickEvent, w, cx| {
                match this.target_for(&n) { Some(t) => this.sb_activate(Spec::Ssh(t), n.clone(), ev.click_count() >= 2, w, cx), None => this.toast(tf("sb.cannot_resolve", &[("name", &n)]), cx) }
            }));
        }
        let n = name.clone();
        row = row.on_mouse_down(gpui::MouseButton::Right, cx.listener(move |this, ev: &gpui::MouseDownEvent, _, cx| { cx.stop_propagation(); this.show_sb_menu(SbMenu { target: SbTarget::Host(n.clone()), pos: ev.position, sub: SbSub::Main }); cx.notify(); }));
        // drag to reorder / move; dropping another host here places it before this one
        let d = HostDrag { name: name.clone() };
        row = row.on_drag(d, |d: &HostDrag, _, _, cx| cx.new(|_| d.clone()));
        let (before, grp) = (name.clone(), h.group.clone().unwrap_or_default());
        row = row.drag_over::<HostDrag>(|st, _, _, _| st.border_t_2().border_color(c(cur().accent)))
            .on_drop(cx.listener(move |this, d: &HostDrag, _w, cx| {
                if d.name == before { return; }
                let (n, g, b) = (d.name.clone(), grp.clone(), before.clone());
                this.sb_save("hosts.move", format!("host={n} group={g:?} before={b:?}"), |s| { hosttree::move_host(&mut s.hosts, &n, &g, Some(&b)); }, cx);
            }));
        let edit = h.clone();
        row = row.child(ui::row_actions(gid).when(sel, |d| d.visible())
            .child(ui::icon_button(("host-edit", idx), IconName::Pencil, ui::IconBtn::Sm, t("menu.edit"))
                .on_click(cx.listener(move |this, _, w, cx| { cx.stop_propagation(); this.open_host_dialog(Some(edit.clone()), None, w, cx); }))));
        row.into_any_element()
    }

    fn place_row(&self, idx: usize, p: &config::Place, depth: usize, cx: &mut Context<Self>) -> AnyElement {
        let sub = if p.host.is_empty() { p.path.clone() } else { format!("{} · {}", p.host, p.path) };
        ui::list_row(("place", idx), depth + 1, false).min_h(px(ui::row2_h()))
            .on_click(cx.listener(move |this, _, w, cx| this.open_place(idx, w, cx)))
            .on_mouse_down(gpui::MouseButton::Right, cx.listener(move |this, ev: &gpui::MouseDownEvent, _, cx| { cx.stop_propagation(); this.show_sb_menu(SbMenu { target: SbTarget::Place(idx), pos: ev.position, sub: SbSub::Main }); cx.notify(); }))
            .child(icon(IconName::Bookmark, ui::IC_MD, c(cur().overlay1)))
            .child(div().flex_1().min_w_0().flex().flex_col().overflow_hidden()
                .child(ui::row_title(p.name.clone()))
                .child(ui::row_meta(sub)))
            .into_any_element()
    }

    fn sb_group_row(&self, path: &str, name: &str, depth: usize, count: usize, collapsed: bool, cx: &mut Context<Self>) -> AnyElement {
        let th = cur();
        let s = config::get();
        let tags = s.groups.iter().find(|g| g.path == path).map(|g| g.tags.clone()).unwrap_or_default();
        let (p1, p2, p3, p4) = (path.to_string(), path.to_string(), path.to_string(), path.to_string());
        ui::list_row(SharedString::from(format!("grp-{path}")), depth, false)
            .on_click(cx.listener(move |this, _, _w, cx| this.toggle_collapsed(&p1, cx)))
            .on_mouse_down(gpui::MouseButton::Right, cx.listener(move |this, ev: &gpui::MouseDownEvent, _, cx| { cx.stop_propagation(); this.show_sb_menu(SbMenu { target: SbTarget::Group(p2.clone()), pos: ev.position, sub: SbSub::Main }); cx.notify(); }))
            .on_drag(GroupDrag { path: path.to_string() }, |d: &GroupDrag, _, _, cx| cx.new(|_| d.clone()))
            .drag_over::<HostDrag>(|st, _, _, _| st.bg(c(cur().accent).opacity(0.22)))
            .drag_over::<GroupDrag>(|st, _, _, _| st.bg(c(cur().accent).opacity(0.22)))
            .on_drop(cx.listener(move |this, d: &HostDrag, _w, cx| { let (n, g) = (d.name.clone(), p3.clone()); this.sb_save("hosts.move", format!("host={n} group={g:?}"), |s| { hosttree::move_host(&mut s.hosts, &n, &g, None); }, cx); }))
            .on_drop(cx.listener(move |this, d: &GroupDrag, _w, cx| {
                let (from, to) = (d.path.clone(), p4.clone());
                if hosttree::under(&to, &from) { return; }
                this.sb_save("hosts.group_move", format!("group={from:?} into={to:?}"), |s| { hosttree::move_group(&mut s.hosts, &mut s.groups, &from, &to, None); }, cx);
            }))
            .child(div().w(px(ui::INDENT)).flex_shrink_0().flex().items_center().child(icon(if collapsed { IconName::ChevronRight } else { IconName::ChevronDown }, ui::IC_XS, c(th.overlay1))))
            .child(icon(IconName::Folder, ui::IC_MD, c(th.overlay1)))
            .child(ui::row_title(name.to_string()).font_weight(FontWeight::MEDIUM))
            .children(ui::tag_dot(tags.first()))
            .child(div().flex_1())
            .child(div().text_size(px(ui::fs_caption())).text_color(c(th.overlay1)).child(count.to_string()))
            .into_any_element()
    }

    // --------------------------------------------------------------- sidebar
    pub(crate) fn sidebar(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let th = cur();
        let s = config::get();
        let selected = self.tabs.get(self.active).map(|t| match &t.focused.read(cx).spec { Spec::Local => "local".to_string(), Spec::Ssh(t) => t.label() });
        let open: Vec<(String, u8)> = self.tabs.iter().flat_map(|t| t.layout.leaves()).map(|p| {
            let v = p.read(cx);
            // 0 idle/closed, 1 connecting, 2 connected, 3 failed
            let st = match &v.session {
                Some(s) if s.conn_error.lock().unwrap().is_some() => 3u8,
                Some(s) if s.exited.load(Ordering::Acquire) => 0,
                Some(s) if s.connected.load(Ordering::Acquire) => 2,
                Some(_) => 1,
                None if !v.embedded() && v.error.is_some() => 3,
                None => 0,
            };
            (match &v.spec { Spec::Local => "local".to_string(), Spec::Ssh(t) => t.label() }, st)
        }).collect();
        let ctx = RowCtx { targets: self.host_targets(), open, selected, host_os: self.host_os.lock().unwrap().clone() };
        let qtext = self.host_search.read(cx).value().to_string();
        let mut q = hosttree::parse_query(&qtext, &s.tags.all());
        for t in &self.tag_filter { if !q.tags.contains(t) { q.tags.push(t.clone()); } }
        let searching = !q.words.is_empty() || !q.tags.is_empty();

        // ---- 标题行：模块名 + 右侧「+」（点击弹出添加菜单）；搜索框独占一行
        // 菜单展开期间不挂提示，避免遮住第一项
        let plus = ui::icon_button("new-host", IconName::Plus, ui::IconBtn::Md, if self.sb_menu.is_none() { t("sb.add") } else { "" })
            .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_click(cx.listener(|this, ev: &gpui::ClickEvent, _w, cx| {
                let p = ev.position();
                this.show_sb_menu(SbMenu { target: SbTarget::Empty, pos: gpui::point(p.x, p.y + px(14.)), sub: SbSub::Main });
                cx.notify();
            }));
        let mut col = self.sb_frame(cx);
        col = col.child(ui::panel_header(self.tool.label(), None, None, vec![plus.into_any_element()]))
            .child(ui::field_row().pb(px(ui::SP_2)).child(ui::search_field(&self.host_search, false)));

        // ---- list
        let mut list = div().id("host-list").flex_1().min_h_0().overflow_y_scroll().flex().flex_col().pb(px(ui::SP_2))
            .on_mouse_down(gpui::MouseButton::Right, cx.listener(|this, ev: &gpui::MouseDownEvent, _, cx| { cx.stop_propagation(); this.show_sb_menu(SbMenu { target: SbTarget::Empty, pos: ev.position, sub: SbSub::Main }); cx.notify(); }));
        // SFTP module: remote hosts only (no local terminal, no local folders)
        let remote_only = self.tool == crate::activity::Tool::Sftp;
        // pinned: local terminal (+ $SSH_HOST target in dev setups)
        if !remote_only {
            list = list.child(ui::section_header("sec-local", t("sb.local"), None, None));
            let sh = crate::shell_display_name();
            let local_sel = ctx.selected.as_deref() == Some("local");
            let local_on = ctx.open.iter().any(|(l, on)| l == "local" && *on == 2);
            list = list.child(ui::list_row("host-local", 0, local_sel).min_h(px(ui::row2_h()))
                .on_click(cx.listener(|this, ev: &gpui::ClickEvent, w, cx| this.sb_activate(Spec::Local, t("ui.local_terminal").to_string(), ev.click_count() >= 2, w, cx)))
                .on_mouse_down(gpui::MouseButton::Right, cx.listener(|this, ev: &gpui::MouseDownEvent, _, cx| { cx.stop_propagation(); this.show_sb_menu(SbMenu { target: SbTarget::Local, pos: ev.position, sub: SbSub::Main }); cx.notify(); }))
                .child(Self::entity_icon(IconName::Monitor, if local_on { 2 } else { 0 }, usize::MAX, None))
                .child(div().flex_1().min_w_0().flex().flex_col().overflow_hidden()
                    .child(ui::row_title(t("ui.local_terminal")))
                    .child(ui::row_meta(sh))));
        }
        if let Some(dt) = self.default_target.clone() {
            let l = dt.label();
            let on = ctx.open.iter().any(|(x, o)| *x == l && *o == 2);
            list = list.child(ui::list_row("host-env", 0, false).min_h(px(ui::row2_h()))
                .on_click(cx.listener(move |this, _, w, cx| this.new_tab(Spec::Ssh(dt.clone()), w, cx)))
                .child(Self::entity_icon(IconName::Server, if on { 2 } else { 0 }, usize::MAX - 1, None))
                .child(div().flex_1().min_w_0().flex().flex_col().overflow_hidden()
                    .child(ui::row_title(l))
                    .child(ui::row_meta(t("ui.ssh_host_environment_variable")))));
        }

        // 「连接」分区可折叠（只存内存）；拖到标题上移到顶层，逻辑不变
        let conn_fold = self.sb_fold.contains("sec-conn");
        let mut conn_head = ui::section_header("sec-conn", t("sb.connections"), Some(s.hosts.len()), Some(conn_fold)).mt(px(ui::SP_2))
            .on_click(cx.listener(|this, _, _w, cx| { if !this.sb_fold.remove("sec-conn") { this.sb_fold.insert("sec-conn"); } cx.notify(); }));
        // dropping on the section header moves to the top level
        conn_head = conn_head.drag_over::<HostDrag>(|st, _, _, _| st.bg(c(cur().accent).opacity(0.22)))
            .drag_over::<GroupDrag>(|st, _, _, _| st.bg(c(cur().accent).opacity(0.22)))
            .on_drop(cx.listener(|this, d: &HostDrag, _w, cx| { let n = d.name.clone(); this.sb_save("hosts.move", format!("host={n} group=\"\""), |s| { hosttree::move_host(&mut s.hosts, &n, "", None); }, cx); }))
            .on_drop(cx.listener(|this, d: &GroupDrag, _w, cx| { let p = d.path.clone(); this.sb_save("hosts.group_move", format!("group={p:?} into=\"\""), |s| { hosttree::move_group(&mut s.hosts, &mut s.groups, &p, "", None); }, cx); }));
        list = list.child(conn_head);
        let places = if remote_only { vec![] } else { s.places.clone() };
        let rows = hosttree::rows(&s.hosts, &s.groups, &s.sidebar.collapsed, &q);
        if conn_fold {
            // 折叠：不渲染连接列表
        } else if s.hosts.is_empty() && s.groups.is_empty() {
            list = list.child(ui::empty_state(IconName::Server, t("sb.empty_title"), Some(t("sb.empty_desc").into()), vec![
                ui::button("sb-empty-new", t("sb.new_connection"), 1).w_full().on_click(cx.listener(|this, _, w, cx| this.open_host_dialog(None, None, w, cx))).into_any_element(),
                ui::button("sb-empty-import", t("sb.import"), 0).w_full().on_click(cx.listener(|this, _, w, cx| this.open_import(w, cx))).into_any_element(),
            ], false));
        } else if rows.is_empty() {
            list = list.child(ui::empty_state(IconName::Search, t("ui.no_matching_hosts"), None, vec![], false));
        }
        if !conn_fold {
            for (i, p) in places.iter().enumerate().filter(|(_, p)| p.group.is_empty()) {
                list = list.child(self.place_row(i, p, 0, cx));
            }
            for r in rows {
                match r {
                    Row::Group { path, name, depth, count, collapsed } => {
                        let extra = places.iter().filter(|p| p.group == path).count();
                        list = list.child(self.sb_group_row(&path, &name, depth, count + extra, collapsed, cx));
                        if !collapsed {
                            for (i, p) in places.iter().enumerate().filter(|(_, p)| p.group == path) {
                                list = list.child(self.place_row(i, p, depth + 1, cx));
                            }
                        }
                    }
                    Row::Host { index, depth } => { let h = s.hosts[index].clone(); list = list.child(self.sb_host_row(index, &h, depth, &ctx, cx)); }
                }
            }
        }

        // "Tags" section: only tags in use (with counts); click to filter. Definitions live in Settings.
        let used: Vec<(String, String, usize)> = s.tags.all().into_iter().map(|(id, name)| {
            let n = s.hosts.iter().filter(|h| h.tags.contains(&id)).count() + s.groups.iter().filter(|g| g.tags.contains(&id)).count();
            (id, name, n)
        }).filter(|(_, _, n)| *n > 0).collect();
        let tags_fold = self.sb_fold.contains("sec-tags");
        if !used.is_empty() {
            list = list.child(ui::section_header("sec-tags", t("tag.tags"), None, Some(tags_fold)).mt(px(ui::SP_2))
                .on_click(cx.listener(|this, _, _w, cx| { if !this.sb_fold.remove("sec-tags") { this.sb_fold.insert("sec-tags"); } cx.notify(); })));
        }
        for (i, (color, name, n)) in used.into_iter().enumerate().filter(|_| !tags_fold) {
            let on = self.tag_filter.contains(&color);
            let cc = color.clone();
            let is_prod = color == s.tags.production;
            list = list.child(ui::list_row(("tagf", i), 0, on)
                .on_click(cx.listener(move |this, _, _w, cx| { if let Some(p) = this.tag_filter.iter().position(|t| *t == cc) { this.tag_filter.remove(p); } else { this.tag_filter.push(cc.clone()); } cx.notify(); }))
                .child(div().size(px(8.)).flex_shrink_0().rounded_full().bg(c(s.tags.rgb(&color))))
                .child(ui::row_title(name))
                .when(is_prod, |d| d.child(icon(IconName::ShieldAlert, ui::IC_XS, c(th.overlay1))))
                .child(div().flex_1())
                .child(div().flex_shrink_0().text_size(px(ui::fs_caption())).text_color(c(th.overlay1)).child(n.to_string())));
        }
        let _ = searching;
        col.child(list)
    }

    /// Left tool-window frame: fixed width, resize handle on the right edge (double click resets).
    pub(crate) fn sb_frame(&mut self, cx: &mut Context<Self>) -> gpui::Stateful<gpui::Div> {
        let col = card_layout::island("sidebar").relative().w(px(self.sb_width)).flex_shrink_0().h_full()
            .on_drag_move(cx.listener(|this, ev: &gpui::DragMoveEvent<SbResize>, _w, cx| {
                let w = (f32::from(ev.event.position.x - ev.bounds.left()) - 2.0).clamp(180.0, 480.0);
                if (w - this.sb_width).abs() >= 1.0 { this.sb_width = w; cx.notify(); }
            }));
        // The 4px gap to the right of the sidebar is the resize hit area.
        col.child(div().id("sb-resize").absolute().top_0().right(px(-4.)).w(px(4.)).h_full().cursor_col_resize()
            .hover(|d| d.bg(c(cur().accent).opacity(0.35)))
            .on_drag(SbResize, |d: &SbResize, _, _, cx| cx.new(|_| d.clone()))
            .on_click(cx.listener(|this, ev: &gpui::ClickEvent, _w, cx| if ev.click_count() >= 2 { this.sb_width = config::SIDEBAR_W; this.save_sb_width(); cx.notify(); })))
    }

    /// Session tools sit beside the terminal, not in the host list.
    pub(crate) fn tool_rail(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let th = cur();
        // Left activity strip owns the modules (terminal, SFTP, database, Redis, Docker, ports);
        // this right strip holds the tool windows that work on the focused session.
        let _ = th;
        let items: [(Action, &str, IconName); 6] = [
            (Action::OpenAi, "AI / Agent", IconName::Bot),
            (Action::OpenHostTools, t("hosttools.title"), IconName::Monitor),
            (Action::OpenSnippets, t("lib.snippets"), IconName::Code),
            (Action::OpenHistory, t("lib.history"), IconName::Clock),
            (Action::CommandPalette, t("ui.command_palette"), IconName::Command),
            (Action::OpenAbout, t("ui.help_about"), IconName::CircleQuestionMark),
        ];
        let n_agents = self.tabs.iter().flat_map(|t| t.layout.leaves()).filter(|p| p.read(cx).agent.is_some()).count();
        // same size, spacing and active style as the left activity strip
        let mut rail = div().id("tool-rail").w(px(crate::activity::STRIP_W)).flex_shrink_0().h_full().flex().flex_col().items_center().pt(px(4.)).pb(px(8.)).gap(px(4.));
        for (i, (a, label, ic)) in items.into_iter().enumerate() {
            let tip = action_tip(label, &a.binding());
            rail = rail.child(self.strip_btn(("rail", i), ic, tip, self.dock_is(a), a == Action::OpenAi && n_agents > 0)
                .on_click(cx.listener(move |this, _, w, cx| this.run(a, w, cx))));
        }
        rail = rail.child(div().flex_1()).child(div().w(px(16.)).h(px(1.)).flex_shrink_0().my(px(4.)).bg(crate::theme::divider()));
        let busy = self.transfers_active(cx);
        rail = rail
            .child(self.strip_btn("rail-transfers", IconName::ArrowDownUp, action_tip(t("tool.transfers"), &Action::ToggleBottomPanel.binding()), self.bottom_open && self.bottom_tab == crate::activity::Bottom::Transfers, busy)
                .on_click(cx.listener(|this, _, _w, cx| this.toggle_bottom(Some(crate::activity::Bottom::Transfers), cx))))
            .child(self.strip_btn("rail-audit", IconName::ScrollText, action_tip(t("ui.audit_log"), &Action::OpenAudit.binding()), false, false)
                .on_click(cx.listener(|this, _, w, cx| this.run(Action::OpenAudit, w, cx))))
            .child(self.strip_btn("rail-save", IconName::FolderPlus, t("place.save").to_string(), false, false)
                .on_click(cx.listener(|this, _, w, cx| this.save_current_place(w, cx))));
        rail
    }

    // ------------------------------------------------------------ context menu
    pub(crate) fn sb_menu_el(&self, m: SbMenu, cx: &mut Context<Self>) -> AnyElement {
        let th = cur();
        let s = config::get();
        let close = |this: &mut Root| this.sb_menu = None;
        let mut list = ui::popover().id("sb-menu").occlude().min_w(px(240.)).max_h(px(560.)).overflow_y_scroll().p(px(5.)).flex().flex_col()
            .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                if this.sb_menu_at.elapsed() < std::time::Duration::from_millis(250) { return; }
                this.sb_menu = None;
                cx.notify();
            }));
        let mut n = 0usize;
        let mut id = || { n += 1; ("sbm", n) };
        macro_rules! item { ($ic:expr, $label:expr, $f:expr) => {{ let f = $f; ui::menu_item(id(), $ic, $label, true, false).on_click(cx.listener(move |this, _, w, cx| { close(this); f(this, w, cx); cx.notify(); })) }} }
        macro_rules! danger { ($ic:expr, $label:expr, $f:expr) => {{ let f = $f; ui::menu_item(id(), $ic, $label, true, true).on_click(cx.listener(move |this, _, w, cx| { close(this); f(this, w, cx); cx.notify(); })) }} }
        let sftp_mode = SftpMode::parse(&s.session.sftp_mode);
        match (&m.target, m.sub) {
            (SbTarget::Host(name), SbSub::Move) => {
                list = list.child(ui::menu_label(t("sb.move_to")));
                let cur_g = s.hosts.iter().find(|h| h.name == *name).and_then(|h| h.group.clone()).unwrap_or_default();
                let mut dests: Vec<(String, String)> = vec![(String::new(), t("sb.top_level").to_string())];
                for g in &s.groups { dests.push((g.path.clone(), format!("{}{}", "    ".repeat(g.path.matches('/').count()), hosttree::leaf_of(&g.path)))); }
                for (path, label) in dests {
                    let (n2, p2) = (name.clone(), path.clone());
                    let here = path == cur_g;
                    list = list.child(ui::menu_item(id(), Some(if path.is_empty() { IconName::Layers } else { IconName::Folder }), label, !here, false)
                        .when(!here, |d| d.on_click(cx.listener(move |this, _, _w, cx| { close(this); let (n, g) = (n2.clone(), p2.clone()); this.sb_save("hosts.move", format!("host={n} group={g:?}"), |s| { hosttree::move_host(&mut s.hosts, &n, &g, None); }, cx); }))));
                }
            }
            (SbTarget::Host(name), _) => {
                let tg = self.target_for(name);
                let (a, b, c2, d) = (tg.clone(), tg.clone(), tg.clone(), tg.clone());
                list = list
                    .child(item!(Some(IconName::Plug), t("sb.connect"), move |this: &mut Root, w: &mut Window, cx: &mut Context<Root>| if let Some(t) = a.clone() { this.new_tab(Spec::Ssh(t), w, cx) }))
                    .child(item!(Some(IconName::Columns2), t("sb.open_split_right"), move |this: &mut Root, w: &mut Window, cx: &mut Context<Root>| if let Some(t) = b.clone() { this.split_with(Spec::Ssh(t), Dir::Vertical, w, cx) }))
                    .child(item!(Some(IconName::Rows2), t("sb.open_split_down"), move |this: &mut Root, w: &mut Window, cx: &mut Context<Root>| if let Some(t) = c2.clone() { this.split_with(Spec::Ssh(t), Dir::Horizontal, w, cx) }))
                    .child(item!(Some(IconName::FolderSync), t("sb.open_sftp"), move |this: &mut Root, w: &mut Window, cx: &mut Context<Root>| if let Some(t) = d.clone() { this.open_sftp_mode(Some(t), sftp_mode, w, cx) }))
                    .child(ui::menu_sep());
                let h = s.hosts.iter().find(|h| h.name == *name).cloned();
                let (e, r, du, mv, del) = (h.clone(), name.clone(), name.clone(), name.clone(), name.clone());
                list = list
                    .child(item!(Some(IconName::Pencil), t("sb.edit"), move |this: &mut Root, w: &mut Window, cx: &mut Context<Root>| this.open_host_dialog(e.clone(), None, w, cx)))
                    .child(item!(Some(IconName::TextCursorInput), t("sb.rename"), move |this: &mut Root, w: &mut Window, cx: &mut Context<Root>| this.prompt(PromptKind::RenameHost(r.clone()), r.clone(), w, cx)))
                    .child(item!(Some(IconName::CopyPlus), t("sb.duplicate"), move |this: &mut Root, _w: &mut Window, cx: &mut Context<Root>| { let n = du.clone(); this.sb_save("hosts.duplicate", format!("host={n}"), |s| { hosttree::duplicate_host(&mut s.hosts, &n); }, cx); }))
                    .child(ui::menu_item(id(), Some(IconName::FolderInput), t("sb.move_to"), true, false).child(div().flex_1()).child(icon(IconName::ChevronRight, ui::IC_XS, c(th.overlay1)))
                        .on_click(cx.listener(move |this, ev: &gpui::ClickEvent, _w, cx| { this.show_sb_menu(SbMenu { target: SbTarget::Host(mv.clone()), pos: ev.position(), sub: SbSub::Move }); cx.notify(); })));
                if let Some(h) = h {
                    let hn = h.name.clone();
                    list = list.child(div().px(px(10.)).py(px(6.)).flex().items_center().gap_2().child(div().text_size(px(ui::fs_small())).text_color(c(th.overlay1)).child(t("tag.tags")))
                        .child(ui::tag_picker("sbm-tags", &h.tags, &s.tags.all(), {
                            let me = cx.entity().downgrade();
                            move |color, _w, cx| { let n = hn.clone(); let _ = me.update(cx, |this, cx| this.sb_save("hosts.tag", format!("host={n} tag={color}"), |s| { if let Some(h) = s.hosts.iter_mut().find(|h| h.name == n) { hosttree::toggle(&mut h.tags, &color); } }, cx)); }
                        })));
                }
                list = list.child(ui::menu_sep())
                    .child(danger!(Some(IconName::Trash), t("sb.delete"), move |this: &mut Root, _w: &mut Window, _cx: &mut Context<Root>| this.sb_modal = Some(SbModal::DeleteHost(del.clone()))));
            }
            (SbTarget::Group(path), _) => {
                let (p1, p2, p3, p4, p5, p6) = (path.clone(), path.clone(), path.clone(), path.clone(), path.clone(), path.clone());
                list = list
                    .child(item!(Some(IconName::Plug), t("sb.connect_all"), move |this: &mut Root, w: &mut Window, cx: &mut Context<Root>| this.connect_all(&p1, w, cx)))
                    .child(ui::menu_sep())
                    .child(item!(Some(IconName::SquarePlus), t("sb.new_connection_here"), move |this: &mut Root, w: &mut Window, cx: &mut Context<Root>| this.open_host_dialog(None, Some(p2.clone()), w, cx)))
                    .child(item!(Some(IconName::FolderPlus), t("sb.new_subgroup"), move |this: &mut Root, w: &mut Window, cx: &mut Context<Root>| this.prompt(PromptKind::NewGroup(p3.clone()), String::new(), w, cx)))
                    .child(item!(Some(IconName::TextCursorInput), t("sb.rename"), move |this: &mut Root, w: &mut Window, cx: &mut Context<Root>| this.prompt(PromptKind::RenameGroup(p4.clone()), hosttree::leaf_of(&p4).to_string(), w, cx)));
                let tags = s.groups.iter().find(|g| g.path == *path).map(|g| g.tags.clone()).unwrap_or_default();
                list = list.child(div().px(px(10.)).py(px(6.)).flex().items_center().gap_2().child(div().text_size(px(ui::fs_small())).text_color(c(th.overlay1)).child(t("tag.tags")))
                    .child(ui::tag_picker("sbm-gtags", &tags, &s.tags.all(), {
                        let me = cx.entity().downgrade();
                        move |color, _w, cx| { let p = p5.clone(); let _ = me.update(cx, |this, cx| this.sb_save("hosts.group_tag", format!("group={p:?} tag={color}"), |s| { if let Some(g) = s.groups.iter_mut().find(|g| g.path == p) { hosttree::toggle(&mut g.tags, &color); } }, cx)); }
                    })))
                    .child(ui::menu_sep())
                    .child(danger!(Some(IconName::Trash), t("sb.delete_group"), move |this: &mut Root, _w: &mut Window, _cx: &mut Context<Root>| this.sb_modal = Some(SbModal::DeleteGroup(p6.clone()))));
            }
            (SbTarget::Local, _) => {
                list = list
                    .child(item!(Some(IconName::SquareTerminal), t("hostmenu.new_tab"), |this: &mut Root, w: &mut Window, cx: &mut Context<Root>| this.new_tab(Spec::Local, w, cx)))
                    .child(item!(Some(IconName::Columns2), t("sb.open_split_right"), |this: &mut Root, w: &mut Window, cx: &mut Context<Root>| this.split_with(Spec::Local, Dir::Vertical, w, cx)))
                    .child(item!(Some(IconName::Rows2), t("sb.open_split_down"), |this: &mut Root, w: &mut Window, cx: &mut Context<Root>| this.split_with(Spec::Local, Dir::Horizontal, w, cx)));
            }
            (SbTarget::Place(i), SbSub::Move) => {
                list = list.child(ui::menu_label(t("sb.move_to")));
                let cur_g = s.places.get(*i).map(|p| p.group.clone()).unwrap_or_default();
                let mut dests: Vec<(String, String)> = vec![(String::new(), t("sb.top_level").to_string())];
                for g in &s.groups { dests.push((g.path.clone(), format!("{}{}", "    ".repeat(g.path.matches('/').count()), hosttree::leaf_of(&g.path)))); }
                for (path, label) in dests {
                    let (i2, p2) = (*i, path.clone());
                    let here = path == cur_g;
                    list = list.child(ui::menu_item(id(), Some(if path.is_empty() { IconName::Layers } else { IconName::Folder }), label, !here, false)
                        .when(!here, |d| d.on_click(cx.listener(move |this, _, _w, cx| { close(this); let (idx, g) = (i2, p2.clone()); this.sb_save("places.move", format!("i={idx} group={g:?}"), |s| { if let Some(p) = s.places.get_mut(idx) { p.group = g; } }, cx); }))));
                }
            }
            (SbTarget::Place(i), _) => {
                let (i1, i2, mv) = (*i, *i, *i);
                let name = s.places.get(*i).map(|p| p.name.clone()).unwrap_or_default();
                list = list
                    .child(item!(Some(IconName::TextCursorInput), t("sb.rename"), move |this: &mut Root, w: &mut Window, cx: &mut Context<Root>| this.prompt(PromptKind::RenamePlace(i1), name.clone(), w, cx)))
                    .child(danger!(Some(IconName::Trash), t("place.delete"), move |this: &mut Root, _w: &mut Window, cx: &mut Context<Root>| { this.sb_save("places.delete", format!("i={i2}"), |s| { if i2 < s.places.len() { s.places.remove(i2); } }, cx); }))
                    .child(ui::menu_item(id(), Some(IconName::FolderInput), t("sb.move_to"), true, false).child(div().flex_1()).child(icon(IconName::ChevronRight, ui::IC_XS, c(th.overlay1)))
                        .on_click(cx.listener(move |this, ev: &gpui::ClickEvent, _w, cx| { this.show_sb_menu(SbMenu { target: SbTarget::Place(mv), pos: ev.position(), sub: SbSub::Move }); cx.notify(); })));
            }
            (SbTarget::Empty, _) => {
                list = list.child(item!(Some(IconName::Import), t("sb.import"), |this: &mut Root, w: &mut Window, cx: &mut Context<Root>| this.open_import(w, cx)));
            }
        }
        if !matches!((&m.target, m.sub), (SbTarget::Host(_), SbSub::Move) | (SbTarget::Place(_), SbSub::Move)) {
            let group = self.menu_group(&m.target);
            let (g1, g2, g3) = (group.clone(), group.clone(), group);
            if !matches!(m.target, SbTarget::Empty) { list = list.child(ui::menu_sep()); }
            list = list
                .child(item!(Some(IconName::FolderPlus), t("sb.new_group"), move |this: &mut Root, w: &mut Window, cx: &mut Context<Root>| this.prompt(PromptKind::NewGroup(g1.clone()), String::new(), w, cx)))
                .child(item!(Some(IconName::SquarePlus), t("sb.new_connection"), move |this: &mut Root, w: &mut Window, cx: &mut Context<Root>| this.open_host_dialog(None, (!g2.is_empty()).then(|| g2.clone()), w, cx)))
                .child(item!(Some(IconName::FolderPlus), t("place.add"), move |this: &mut Root, w: &mut Window, cx: &mut Context<Root>| this.add_place(g3.clone(), w, cx)));
        }
        gpui::deferred(gpui::anchored().position(m.pos).snap_to_window_with_margin(px(8.)).child(chrome::pop_in("sb-menu-pop", list))).with_priority(3).into_any_element()
    }

    // ------------------------------------------------------------------ modals
    pub(crate) fn sb_modal_el(&mut self, m: SbModal, cx: &mut Context<Self>) -> AnyElement {
        let th = cur();
        let note = |s: String| div().text_size(px(ui::fs_body())).text_color(c(th.subtext)).child(s);
        let cancel = ui::button("sbm-cancel", t("ui.cancel"), 0).on_click(cx.listener(|this, _, w, cx| { this.sb_modal = None; this.focus_active(w, cx); cx.notify(); }));
        let mut body = div().p(px(ui::SP_5)).flex().flex_col().gap(px(ui::SP_3));
        let hdr: (IconName, gpui::Hsla, String);
        let mut foot = ui::dialog_footer(None, vec![]);
        match m {
            SbModal::Prompt { kind, input } => {
                let head = match &kind {
                    PromptKind::NewGroup(p) if p.is_empty() => t("sb.new_group").to_string(),
                    PromptKind::NewGroup(p) => tf("sb.new_subgroup_in", &[("g", &hosttree::leaf_of(p))]),
                    PromptKind::RenameHost(_) | PromptKind::RenameGroup(_) | PromptKind::RenamePlace(_) => t("sb.rename").to_string(),
                    PromptKind::SavePlace { .. } => t("place.name_title").to_string(),
                    PromptKind::PlacePath { .. } => t("place.path_title").to_string(),
                };
                let (k1, i1, k2, i2) = (kind.clone(), input.clone(), kind, input.clone());
                hdr = (IconName::TextCursorInput, c(cur().accent), head);
                body = body
                    .child(div().on_key_down(cx.listener(move |this, ev: &gpui::KeyDownEvent, w, cx| if ev.keystroke.key == "enter" { let v = i1.read(cx).value().to_string(); this.prompt_ok(k1.clone(), v, w, cx); cx.notify(); })).child(ui::text_field(&input, false)));
                foot = foot.child(cancel)
                        .child(ui::button("sbm-ok", t("ui.save"), 1).on_click(cx.listener(move |this, _, w, cx| { let v = i2.read(cx).value().to_string(); this.prompt_ok(k2.clone(), v, w, cx); cx.notify(); })));
            }
            SbModal::NewTab { spec, label, dont_ask } => {
                let sp = spec.clone();
                hdr = (IconName::Plus, c(cur().accent), tf("sb.confirm_new_tab", &[("name", &label)]));
                body = body
                    .child(div().id("sbm-dontask").flex().items_center().gap_2().cursor_pointer()
                        .on_click(cx.listener(|this, _, _w, cx| { if let Some(SbModal::NewTab { dont_ask, .. }) = this.sb_modal.as_mut() { *dont_ask = !*dont_ask; } cx.notify(); }))
                        .child(div().size(px(16.)).rounded(px(ui::R_SM)).border_1().border_color(c(th.surface2)).flex().items_center().justify_center()
                            .when(dont_ask, |d| d.bg(c(th.accent)).child(icon(IconName::Check, ui::IC_XS, c(th.base)))))
                        .child(div().text_size(px(ui::fs_body())).text_color(c(th.text)).child(t("sb.dont_ask_again"))));
                foot = foot.child(cancel)
                        .child(ui::button("sbm-open", t("sb.open_new_tab"), 1).on_click(cx.listener(move |this, _, w, cx| {
                            if dont_ask { let _ = config::update(|s| s.session.confirm_new_tab = false); }
                            this.sb_modal = None;
                            this.new_tab(sp.clone(), w, cx);
                        })));
            }
            SbModal::DeleteHost(name) => {
                hdr = (IconName::Trash, c(cur().error), tf("sb.delete_host_title", &[("name", &name)]));
                body = body.child(note(t("sb.delete_host_body").into()));
                foot = foot.child(cancel)
                        .child(ui::button("sbm-del", t("ui.delete"), 2).on_click(cx.listener(move |this, _, _w, cx| { let n = name.clone(); this.sb_save("host.delete", n.clone(), |s| s.hosts.retain(|h| h.name != n), cx); this.sb_modal = None; })));
            }
            SbModal::DeleteGroup(path) => {
                let s = config::get();
                let n_hosts = hosttree::hosts_under(&s.hosts, &path).len();
                let n_groups = s.groups.iter().filter(|g| hosttree::under(&g.path, &path) && g.path != path).count();
                let (p1, p2) = (path.clone(), path.clone());
                hdr = (IconName::Trash, c(cur().error), tf("sb.delete_group_title", &[("name", &hosttree::leaf_of(&path))]));
                body = body
                    .child(note(tf("sb.delete_group_body", &[("h", &n_hosts), ("g", &n_groups)])));
                foot = foot.child(cancel)
                        .child(ui::button("sbm-keep", t("sb.delete_keep"), 1).on_click(cx.listener(move |this, _, _w, cx| { let p = p1.clone(); this.sb_save("hosts.group_delete", format!("group={p:?} keep_children=true"), |s| { hosttree::delete_group(&mut s.hosts, &mut s.groups, &p, true); s.sidebar.collapsed.retain(|c| *c != p); }, cx); this.sb_modal = None; })))
                        .when(n_hosts + n_groups > 0, |d| d.child(ui::button("sbm-all", t("sb.delete_all"), 2).on_click(cx.listener(move |this, _, _w, cx| { let p = p2.clone(); this.sb_save("hosts.group_delete", format!("group={p:?} keep_children=false"), |s| { hosttree::delete_group(&mut s.hosts, &mut s.groups, &p, false); s.sidebar.collapsed.retain(|c| !hosttree::under(c, &p)); }, cx); this.sb_modal = None; }))));
            }
            SbModal::Import { cands, checked, group } => {
                hdr = (IconName::Import, c(cur().accent), t("sb.import").to_string());
                body = body
                    .child(note(tf("sb.import_body", &[("path", &config::get().ssh.ssh_config)])));
                if cands.is_empty() { body = body.child(note(t("sb.import_none").into())); }
                let mut lst = div().id("imp-list").max_h(px(300.)).overflow_y_scroll().flex().flex_col().gap_1();
                for (i, h) in cands.iter().enumerate() {
                    let on = checked[i];
                    lst = lst.child(div().id(("imp", i)).px_2().py(px(5.)).rounded(px(ui::R_MD)).flex().items_center().gap_2().cursor_pointer().hover(|d| d.bg(crate::theme::hover_bg()))
                        .on_click(cx.listener(move |this, _, _w, cx| { if let Some(SbModal::Import { checked, .. }) = &mut this.sb_modal { checked[i] = !checked[i]; } cx.notify(); }))
                        .child(div().size(px(14.)).rounded(px(ui::R_SM)).border_1().border_color(c(th.overlay0)).flex().items_center().justify_center().when(on, |d| d.bg(c(th.accent)).border_color(c(th.accent)).child(icon(IconName::Check, ui::IC_XS, gpui::white()))))
                        .child(icon(IconName::Server, ui::IC_SM, c(th.accent)))
                        .child(div().text_size(px(ui::fs_body())).text_color(c(th.text)).child(h.name.clone()))
                        .child(div().text_size(px(ui::fs_small())).text_color(c(th.overlay0)).child(format!("{}{}{}", h.user.as_ref().map(|u| format!("{u}@")).unwrap_or_default(), h.host, h.port.map(|p| format!(":{p}")).unwrap_or_default()))));
                }
                let n = checked.iter().filter(|c| **c).count();
                let g2 = group.clone();
                body = body.child(lst)
                    .child(div().flex().items_center().gap_2().child(div().text_size(px(ui::fs_body())).text_color(c(th.subtext)).child(t("ui.group"))).child(div().flex_1().child(ui::text_field(&group, false))));
                foot = foot.child(cancel)
                        .child(ui::button("sbm-import", tf("sb.import_n", &[("n", &n)]), 1).on_click(cx.listener(move |this, _, _w, cx| {
                            let Some(SbModal::Import { cands, checked, .. }) = this.sb_modal.clone() else { return };
                            let chosen: Vec<HostEntry> = cands.iter().zip(&checked).filter(|(_, c)| **c).map(|(h, _)| h.clone()).collect();
                            let g = g2.read(cx).value().to_string();
                            let mut added = 0;
                            this.sb_save("hosts.import", format!("n={} group={g:?}", chosen.len()), |s| { added = hosttree::import(&mut s.hosts, &chosen, &g); }, cx);
                            this.toast(tf("sb.imported", &[("n", &added)]), cx);
                            this.sb_modal = None;
                        })));
            }
        }
        ui::modal(ui::card(480.).child(ui::dialog_header(Some(hdr.0), hdr.1, hdr.2, None, None)).child(body).child(foot)).into_any_element()
    }
}

pub(crate) struct RowCtx {
    targets: Vec<(HostEntry, ssh_term_proto::ssh::SshTarget)>,
    open: Vec<(String, u8)>,
    selected: Option<String>,
    host_os: std::collections::HashMap<String, String>,
}

impl Root {
    /// Sidebar click (PRD 5.24): single click focuses the first tab already showing this target
    /// (opens one if none); double click opens another tab, after a confirmation unless disabled.
    pub(crate) fn sb_activate(&mut self, spec: Spec, label: String, double: bool, window: &mut Window, cx: &mut Context<Self>) {
        // The first click of a double click already focused / opened the session; the second
        // click must not be treated as a request for another tab (that used to pop the
        // "open again?" dialog). Opening a second tab stays available from the context menu.
        if double { let _ = label; return; }
        // SFTP tool window: the same host tree, but a click opens the file browser
        if self.tool == crate::activity::Tool::Sftp {
            let mode = SftpMode::parse(&config::get().session.sftp_mode);
            let target = match spec { Spec::Ssh(t) => Some(t), Spec::Local => None };
            // the focused SFTP tab still choosing a host takes it right away; otherwise a new tab
            if let Some(t) = target.clone() {
                let sv = self.tabs.get(self.active).and_then(|tb| tb.focused.read(cx).sftp.clone());
                if let Some(sv) = sv { if sv.update(cx, |v, cx| v.connect_free_pane(t, cx)) { return; } }
            }
            return self.open_sftp_mode(target, mode, window, cx);
        }
        let same = |p: &Entity<crate::termview::TermView>, cx: &Context<Self>| {
            let v = p.read(cx);
            !v.embedded() && match (&v.spec, &spec) { (Spec::Local, Spec::Local) => true, (Spec::Ssh(a), Spec::Ssh(b)) => a.label() == b.label(), _ => false }
        };
        let hit = self.tabs.iter().enumerate().find_map(|(i, t)| t.layout.leaves().into_iter().find(|p| same(p, cx)).map(|p| (i, p)));
        match hit {
            Some((i, p)) => { self.active = i; self.leave_settings(); self.tabs[i].focused = p.clone(); let fh = p.read(cx).focus.clone(); window.focus(&fh, cx); cx.notify(); }
            None => self.new_tab(spec, window, cx),
        }
    }
    /// Persist the sidebar width when it differs from the saved value.
    pub(crate) fn save_sb_width(&mut self) {
        let w = self.sb_width.round();
        if (config::get().sidebar.width - w).abs() >= 1.0 { let _ = config::update(|s| s.sidebar.width = w); }
    }
    /// Persist the AI dock width when it differs from the saved value.
    pub(crate) fn save_ai_width(&mut self) {
        let w = self.ai_width.round();
        if (config::get().sidebar.ai_width - w).abs() >= 1.0 { let _ = config::update(|s| s.sidebar.ai_width = w); }
    }
}
