//! Right-click menu rendering + per-host tool probing (PRD 5.20). The menu model comes from
//! `ssh_term_proto::ctxmenu`; this module draws it as a floating, window-anchored popup.
use std::sync::OnceLock;
use std::time::Duration;

use gpui::{anchored, deferred, div, prelude::*, px, AnyElement, Context, FontWeight, Pixels, Point, SharedString, Window};
use ssh_term_proto::ctxmenu::{self, Menu, Target, Tools};
use ssh_term_proto::i18n::{t, tf};
use ssh_term_proto::ssh::{self, SshTarget};

use crate::config;
use crate::theme::{c, cur};
use crate::ui;

static RT: OnceLock<(tokio::runtime::Handle, ssh::Prompts)> = OnceLock::new();
/// Called once at startup so views can probe remote hosts through the shared connection pool.
pub fn init(rt: tokio::runtime::Handle, prompts: ssh::Prompts) { let _ = RT.set((rt, prompts)); }

pub fn host_key(target: Option<&SshTarget>) -> String { target.map(|t| t.pool_key()).unwrap_or_else(|| "local".into()) }

/// Start a tool probe for this host unless cached / running. The receiver fires when done.
pub fn ensure_probe(target: Option<SshTarget>) -> Option<flume::Receiver<()>> {
    let key = host_key(target.as_ref());
    if !ctxmenu::begin_probe(&key) { return None; }
    let (tx, rx) = flume::bounded(1);
    match target {
        None => { std::thread::spawn(move || { ctxmenu::store_tools(&key, ctxmenu::probe_local()); let _ = tx.send(()); }); }
        Some(tg) => {
            let (rt, p) = RT.get()?.clone();
            rt.spawn(async move {
                let r = async { let (c, _) = ssh::connect_shared(&tg, &p).await?; c.exec(&ctxmenu::probe_command(), Duration::from_secs(15)).await }.await;
                match r {
                    Ok((_, out)) => ctxmenu::store_tools(&key, ctxmenu::parse_probe(&String::from_utf8_lossy(&out))),
                    Err(e) => eprintln!("ctxmenu: tool probe on {key} failed: {e:#}"),
                }
                let _ = tx.send(());
            });
        }
    }
    Some(rx)
}

/// An open menu: model, where it was opened, what it is about.
pub struct MenuState {
    pub menu: Menu,
    pub pos: Point<Pixels>,
    pub target: Target,
    pub host_key: String,
    pub note: Option<String>,
    pub errors: usize,
    pub opened: std::time::Instant,
}

pub fn build(target: Target, place: ctxmenu::Place, host: &str, host_key: String, pos: Point<Pixels>, note: Option<String>) -> MenuState {
    let tools: Option<Tools> = ctxmenu::cached_tools(&host_key);
    let (custom, errs) = ctxmenu::load_custom();
    let lang_en = ssh_term_proto::i18n::current() == ssh_term_proto::i18n::Lang::En;
    let menu = ctxmenu::build(&target, &ctxmenu::Ctx { host, place, tools: tools.as_ref(), custom: &custom, lang_en });
    MenuState { menu, pos, target, host_key, note, errors: errs.len(), opened: std::time::Instant::now() }
}
impl MenuState {
    /// Rebuild after a tool probe finished (same target and place).
    pub fn rebuilt(&self, place: ctxmenu::Place, host: &str) -> MenuState {
        let mut next = build(self.target.clone(), place, host, self.host_key.clone(), self.pos, self.note.clone());
        next.opened = self.opened;
        next
    }
}

/// Draw the menu. `pick(view, index)` runs for enabled items; `close(view)` on outside click.
pub fn render<V: 'static>(st: &MenuState, cx: &mut Context<V>, pick: fn(&mut V, usize, &mut Window, &mut Context<V>), close: fn(&mut V, &mut Context<V>)) -> AnyElement {
    let th = cur();
    let mono = SharedString::from(config::get().font.family.clone());
    let probing = ctxmenu::cached_tools(&st.host_key).is_none();
    let mut list = div().id("ctxmenu-list").max_h(px(560.)).overflow_y_scroll().flex().flex_col().py_1();
    let mut last_group = None;
    for (i, it) in st.menu.items.iter().enumerate() {
        if last_group.is_some_and(|g| g != it.group) { list = list.child(ui::menu_sep()); }
        last_group = Some(it.group);
        let enabled = it.enabled();
        let mut row = div().id(("ctxmenu-item", i)).px(px(10.)).py(px(4.)).flex().flex_col().gap(px(1.)).rounded(px(ui::R_MD)).mx(px(ui::LIST_INSET))
            .child(div().flex().items_center().gap_2().text_size(px(ui::fs_body())).text_color(if enabled { c(th.text) } else { c(th.overlay0) })
                .child(div().flex_1().min_w_0().whitespace_nowrap().overflow_hidden().text_ellipsis().child(it.label.clone()))
                .children(it.custom.then(|| div().px_1().rounded(px(ui::R_SM)).text_size(px(ui::fs_caption())).text_color(c(th.accent)).border_1().border_color(c(th.accent).opacity(0.5)).child("custom")))
                .children((it.unknown && probing).then(|| div().text_size(px(ui::fs_small())).text_color(c(th.overlay0)).child(t("cm.probing")))));
        if let Some(p) = it.preview() {
            row = row.child(div().text_size(px(ui::fs_mono())).font_family(mono.clone()).text_color(if enabled { c(th.accent) } else { c(th.overlay0) }).whitespace_nowrap().overflow_hidden().text_ellipsis().child(format!("$ {p}")));
        }
        if !enabled {
            let tool = it.missing.join(", ");
            row = row.child(div().text_size(px(ui::fs_small())).text_color(c(th.warning)).child(tf("cm.missing", &[("tool", &tool), ("cmd", &it.hint.clone().unwrap_or_default())])));
        } else {
            row = row.cursor_pointer().hover(|d| d.bg(crate::theme::menu_sel_bg())).on_click(cx.listener(move |v, _, w, cx| pick(v, i, w, cx)));
        }
        list = list.child(row);
    }
    let opened = st.opened;
    let has_insert = st.menu.items.iter().any(|i| matches!(i.act, ssh_term_proto::ctxmenu::Act::Insert(_)));
    let menu = crate::ui::popover().id("ctxmenu").occlude().w(px(280.)).flex().flex_col().overflow_hidden()
        .on_mouse_down_out(cx.listener(move |v, _, _, cx| {
            if opened.elapsed() < Duration::from_millis(250) { return; }
            close(v, cx);
        }))
        .child(div().px_3().pt_2().pb_1().flex().flex_col().gap(px(2.)).border_b_1().border_color(crate::theme::divider())
            .child(div().text_size(px(ui::fs_small())).font_weight(FontWeight::SEMIBOLD).text_color(c(th.text)).whitespace_nowrap().overflow_hidden().text_ellipsis().child(st.menu.header.clone()))
            .children(st.note.clone().map(|n| div().text_size(px(ui::fs_small())).text_color(c(th.info)).child(n)))
            .children((st.errors > 0).then(|| div().text_size(px(ui::fs_small())).text_color(c(th.error)).child(tf("cm.custom_errors", &[("n", &st.errors)])))))
        .child(list)
        .when(has_insert, |d| d.child(div().px_3().py_1().border_t_1().border_color(crate::theme::divider()).text_size(px(ui::fs_small())).text_color(c(th.overlay1)).whitespace_nowrap().overflow_hidden().text_ellipsis().child(t("cm.insert_hint"))));
    deferred(anchored().position(st.pos).snap_to_window_with_margin(px(8.)).child(crate::chrome::pop_in("ctxmenu-pop", menu))).with_priority(10).into_any_element()
}
