//! Homebrew 中间页：可更新表、已安装表、单个服务或软件的详情。
use gpui::{
    div, prelude::*, px, App, Context, Entity, EventEmitter, FontWeight, ScrollHandle,
    SharedString, Subscription, Window,
};
use gpui_component::input::{InputEvent, InputState};
use ssh_term_proto::brew::{self, Kind, Op, Package, Service, SvcAct};
use ssh_term_proto::i18n::{t, tf};

use crate::brew_ui::{BrewHub, Page};
use crate::theme::{c, cur};
use crate::ui::{self, IconName};

#[derive(Clone, Debug)]
pub enum BrewEvent {
    Open(Page, Option<String>),
}

pub struct BrewView {
    pub install: bool,
    page: Page,
    selected: Option<String>,
    hub: Entity<BrewHub>,
    ask: Option<Op>,
    find: Option<Entity<InputState>>,
    searched: bool,
    /// 0 全部，1 配方，2 应用。
    kind_filter: u8,
    log_open: bool,
    log_seen: usize,
    scroll: ScrollHandle,
    _subs: Vec<Subscription>,
}

impl EventEmitter<BrewEvent> for BrewView {}

impl BrewView {
    /// 一个标签只看一种内容：可更新、全部已安装，或某一个软件。
    pub fn open(hub: Entity<BrewHub>, page: Page, selected: Option<String>, cx: &mut Context<Self>) -> Self {
        Self::build(hub, false, page, selected, None, cx)
    }

    pub fn same(&self, install: bool, page: Page, id: Option<&str>) -> bool {
        if self.install != install {
            return false;
        }
        if install {
            return true;
        }
        match page {
            Page::Detail => self.page == Page::Detail && self.selected.as_deref() == id,
            other => self.page == other,
        }
    }

    pub fn page(&self) -> Page {
        self.page
    }

    pub fn selected_id(&self) -> Option<String> {
        self.selected.clone()
    }

    /// 安装落地页：搜索配方或应用，再安装。
    pub fn install(hub: Entity<BrewHub>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let find = cx.new(|cx| InputState::new(window, cx).placeholder(t("brew.install_ph")));
        let mut me = Self::build(hub, true, Page::Outdated, None, Some(find.clone()), cx);
        me._subs.push(
            cx.subscribe_in(&find, window, |this, _, ev: &InputEvent, _, cx| {
                if matches!(ev, InputEvent::PressEnter { .. }) {
                    this.run_install_search(cx);
                }
            }),
        );
        me
    }

    fn build(
        hub: Entity<BrewHub>,
        install: bool,
        page: Page,
        selected: Option<String>,
        find: Option<Entity<InputState>>,
        cx: &mut Context<Self>,
    ) -> Self {
        let scroll = ScrollHandle::new();
        let sub = cx.observe(&hub, move |this, hub, cx| {
            let n = hub.read(cx).log.len();
            if n != this.log_seen {
                this.log_seen = n;
                if this.log_open {
                    this.scroll.scroll_to_bottom();
                }
            }
            cx.notify();
        });
        Self {
            install,
            page,
            selected,
            hub,
            ask: None,
            find,
            searched: false,
            kind_filter: 0,
            log_open: false,
            log_seen: 0,
            scroll,
            _subs: vec![sub],
        }
    }

    /// 标签上的名字，和当前页标题一致。
    pub fn tab_label(&self, cx: &App) -> String {
        if self.install {
            return t("brew.install").into();
        }
        let h = self.hub.read(cx);
        match self.page {
            Page::Outdated => tf(
                "brew.nav_outdated",
                &[("n", &h.packages.iter().filter(|p| p.outdated).count())],
            ),
            Page::All => t("brew.nav_all").into(),
            Page::Detail => self
                .selected
                .as_ref()
                .and_then(|id| {
                    h.packages
                        .iter()
                        .find(|p| &p.id == id)
                        .map(|p| p.title.clone())
                        .or_else(|| id.strip_prefix("service:").map(|s| s.to_string()))
                })
                .filter(|n| !n.is_empty())
                .unwrap_or_else(|| t("brew.outdated").to_string()),
        }
    }

    fn run_install_search(&mut self, cx: &mut Context<Self>) {
        let Some(find) = self.find.clone() else {
            return;
        };
        let q = find.read(cx).value().trim().to_string();
        if q.is_empty() {
            return;
        }
        self.searched = true;
        self.hub
            .update(cx, |h, cx| h.enqueue(Op::Search { query: q }, cx));
    }

    pub fn crumbs(&self, cx: &App) -> String {
        if self.install {
            return t("brew.install").into();
        }
        format!("{} › {}", t("tool.brew"), self.tab_label(cx))
    }

    fn act(&mut self, op: Op, cx: &mut Context<Self>) {
        if op.needs_confirm() {
            self.ask = Some(op);
            cx.notify();
        } else {
            self.ask = None;
            self.hub.update(cx, |h, cx| h.enqueue(op, cx));
        }
    }

    fn op_btn(
        &self,
        id: impl Into<gpui::ElementId>,
        label: impl Into<SharedString>,
        kind: u8,
        op: Op,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        ui::button_sm(id, label, kind)
            .on_click(cx.listener(move |this, _, _, cx| this.act(op.clone(), cx)))
    }
}

impl gpui::Render for BrewView {
    fn render(&mut self, _window: &mut gpui::Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.install {
            return self.install_page(cx);
        }
        let th = cur();
        let (
            missing,
            loaded,
            error,
            packages,
            services,
            log,
            current,
            queued,
            query,
        ) = {
            let h = self.hub.read(cx);
            (
                h.brew.is_none(),
                h.loaded,
                h.error.clone(),
                h.packages.clone(),
                h.services.clone(),
                h.log.clone(),
                h.current.clone(),
                h.queue.len(),
                h.query.clone(),
            )
        };
        let page = self.page;
        let selected = self.selected.clone();
        let status = crate::brew_ui::running_text(current.as_ref(), queued);
        let q = if page == Page::Detail {
            String::new()
        } else {
            query.trim().to_lowercase()
        };
        let pkg = selected
            .as_ref()
            .and_then(|id| packages.iter().find(|p| &p.id == id).cloned());
        let svc = pkg
            .as_ref()
            .and_then(|p| services.iter().find(|s| s.name == p.name).cloned())
            .or_else(|| {
                selected
                    .as_deref()
                    .and_then(|id| id.strip_prefix("service:"))
                    .and_then(|n| services.iter().find(|s| s.name == n).cloned())
            });
        let detail = page == Page::Detail;
        let searching = !q.is_empty() && !detail;
        let outdated_mode = !searching && !detail && page != Page::All;
        let rows = table_rows(
            &packages,
            outdated_mode && !searching,
            searching,
            self.kind_filter,
            &q,
        );
        let outdated_n = packages.iter().filter(|p| p.outdated).count();

        let title = if searching {
            tf("brew.search_n", &[("n", &rows.len())])
        } else if detail {
            pkg.as_ref()
                .map(|p| p.title.clone())
                .or_else(|| svc.as_ref().map(|s| s.name.clone()))
                .unwrap_or_default()
        } else if outdated_mode {
            tf("brew.nav_outdated", &[("n", &outdated_n)])
        } else {
            t("brew.nav_all").to_string()
        };

        let mut bar = ui::toolbar();
        if detail {
            bar = bar.child(
                ui::icon_button(
                    "brew-back",
                    IconName::ChevronLeft,
                    ui::IconBtn::Md,
                    t("brew.back"),
                )
                .on_click(cx.listener(|_, _, _, cx| {
                    cx.emit(BrewEvent::Open(Page::Outdated, None));
                })),
            );
        }
        bar = bar.child(
            div()
                .min_w_0()
                .whitespace_nowrap()
                .overflow_hidden()
                .text_ellipsis()
                .text_size(px(ui::fs_body()))
                .font_weight(FontWeight::SEMIBOLD)
                .child(title),
        );
        if let Some(s) = status.clone() {
            bar = bar.child(
                div()
                    .flex_1()
                    .min_w_0()
                    .whitespace_nowrap()
                    .overflow_hidden()
                    .text_ellipsis()
                    .text_size(px(ui::fs_small()))
                    .text_color(c(th.subtext))
                    .child(s),
            );
        } else {
            bar = bar.child(div().flex_1());
        }
        if outdated_mode {
            bar = bar
                .child(self.op_btn(
                    "brew-upgrade-all",
                    t("brew.upgrade_all"),
                    if outdated_n > 0 { 1 } else { 0 },
                    Op::UpgradeAll,
                    cx,
                ))
                .child(self.op_btn("brew-update", t("brew.update"), 3, Op::Update, cx))
                .child(self.op_btn("brew-cleanup", t("brew.cleanup"), 3, Op::Cleanup, cx));
        }
        bar =
            bar.child(
                ui::icon_button(
                    "brew-do-refresh",
                    IconName::RefreshCw,
                    ui::IconBtn::Md,
                    t("brew.refresh"),
                )
                .on_click(cx.listener(|this, _, _, cx| {
                    this.hub.update(cx, |h, cx| h.enqueue(Op::Refresh, cx))
                })),
            );

        let mut col = div()
            .flex_1()
            .size_full()
            .flex()
            .flex_col()
            .bg(c(th.base))
            .child(bar);
        if let Some(e) = error {
            col = col.child(ui::notice(ui::Notice::Error, e).mt(px(ui::SP_2)));
        }
        if let Some(op) = self.ask.clone() {
            col = col.child(self.confirm_bar(&op, cx));
        }

        if missing {
            col = col.child(
                ui::empty_state(
                    IconName::Beer,
                    t("brew.missing"),
                    Some(t("brew.missing_desc").into()),
                    vec![],
                    true,
                )
                .flex_none(),
            );
        } else if !loaded && packages.is_empty() {
            col = col.child(
                ui::empty_state(IconName::Beer, t("brew.loading"), None, vec![], true).flex_none(),
            );
        } else if detail {
            let mut body = div()
                .id("brew-detail")
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .px(px(ui::SP_4))
                .py(px(ui::SP_3))
                .flex()
                .flex_col()
                .gap(px(ui::SP_3));
            if let Some(p) = pkg.as_ref() {
                body = body.child(self.detail(p, svc.as_ref(), cx));
            } else if let Some(s) = svc.as_ref() {
                body = body.child(self.service_only(s, cx));
            } else {
                body = body.child(
                    ui::empty_state(IconName::Beer, t("brew.gone"), None, vec![], true).flex_none(),
                );
            }
            col = col.child(body);
        } else {
            let empty = if searching || self.kind_filter != 0 {
                t("brew.no_match")
            } else if outdated_mode {
                t("brew.uptodate")
            } else {
                t("brew.no_match")
            };
            col = col
                .child(self.kind_chips(cx))
                .child(self.table(&rows, empty, cx));
        }
        col.child(self.log_panel(&log, status.as_deref(), cx))
    }
}

impl BrewView {
    fn install_page(&mut self, cx: &mut Context<Self>) -> gpui::Div {
        let th = cur();
        let (hits, packages, search_error, log, current, queued, error) = {
            let h = self.hub.read(cx);
            (
                h.hits.clone(),
                h.packages.clone(),
                h.search_error.clone(),
                h.log.clone(),
                h.current.clone(),
                h.queue.len(),
                h.error.clone(),
            )
        };
        let status = crate::brew_ui::running_text(current.as_ref(), queued);
        let find = self.find.clone();
        let mut bar = ui::toolbar().child(
            div()
                .text_size(px(ui::fs_body()))
                .font_weight(FontWeight::SEMIBOLD)
                .child(t("brew.install").to_string()),
        );
        if let Some(s) = status.clone() {
            bar = bar.child(
                div()
                    .flex_1()
                    .min_w_0()
                    .whitespace_nowrap()
                    .overflow_hidden()
                    .text_ellipsis()
                    .text_size(px(ui::fs_small()))
                    .text_color(c(th.subtext))
                    .child(s),
            );
        } else {
            bar = bar.child(div().flex_1());
        }
        bar = bar.child(
            ui::button_sm("brew-install-go", t("brew.install_go"), 1)
                .on_click(cx.listener(|this, _, _, cx| this.run_install_search(cx))),
        );
        let mut col = div()
            .flex_1()
            .size_full()
            .flex()
            .flex_col()
            .bg(c(th.base))
            .child(bar);
        if let Some(e) = search_error.or(error) {
            col = col.child(ui::notice(ui::Notice::Error, e).mt(px(ui::SP_2)));
        }
        if let Some(op) = self.ask.clone() {
            col = col.child(self.confirm_bar(&op, cx));
        }
        let mut body = div()
            .id("brew-install")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .px(px(ui::SP_4))
            .py(px(ui::SP_3))
            .flex()
            .flex_col()
            .gap(px(ui::SP_3));
        body = body.child(
            div()
                .text_size(px(ui::fs_small()))
                .text_color(c(th.subtext))
                .whitespace_normal()
                .child(t("brew.install_hint").to_string()),
        );
        if let Some(find) = find {
            body = body.child(ui::search_field(&find, false));
        }
        if hits.is_empty() {
            let msg = if self.searched {
                t("brew.search_none")
            } else {
                t("brew.install_empty")
            };
            body =
                body.child(ui::empty_state(IconName::Search, msg, None, vec![], true).flex_none());
        }
        let shown = hits.iter().take(50);
        let more = hits.len().saturating_sub(50);
        for hit in shown {
            let name = hit.name.clone();
            let cask = hit.cask;
            let already = packages
                .iter()
                .any(|p| p.name == hit.name || p.cmd == hit.name);
            let kind = if cask {
                t("brew.casks")
            } else {
                t("brew.formulae")
            };
            let mut row = div()
                .id(SharedString::from(format!("brew-hit-{}", hit.name)))
                .min_h(px(ui::row_h()))
                .flex()
                .items_center()
                .gap(px(ui::SP_2))
                .border_b_1()
                .border_color(crate::theme::divider())
                .child(ui::row_title(hit.name.clone()).flex_1())
                .child(ui::row_meta(kind.to_string()));
            if already {
                row = row.child(ui::row_meta(t("brew.installed_already").to_string()));
            } else {
                let id = SharedString::from(format!("brew-in-{}", hit.name));
                row = row.child(
                    ui::button_sm(id, t("brew.install"), 1).on_click(cx.listener(
                        move |this, _, _, cx| {
                            this.act(
                                Op::Install {
                                    name: name.clone(),
                                    cask,
                                },
                                cx,
                            );
                        },
                    )),
                );
            }
            body = body.child(row);
        }
        if more > 0 {
            body = body.child(
                div()
                    .text_size(px(ui::fs_small()))
                    .text_color(c(th.overlay1))
                    .child(tf("brew.search_n", &[("n", &hits.len())])),
            );
        }
        col.child(body)
            .child(self.log_panel(&log, status.as_deref(), cx))
    }

    fn confirm_bar(&self, op: &Op, cx: &mut Context<Self>) -> gpui::Div {
        let th = cur();
        div()
            .mx(px(ui::SP_3))
            .mt(px(ui::SP_2))
            .px(px(ui::SP_3))
            .py(px(ui::SP_2))
            .rounded(px(ui::R_MD))
            .bg(c(th.warning).opacity(0.12))
            .flex()
            .items_center()
            .gap(px(ui::SP_2))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_size(px(ui::fs_small()))
                    .text_color(c(th.text))
                    .whitespace_normal()
                    .child(confirm_text(op)),
            )
            .child(
                ui::button_sm("brew-ask-ok", t("brew.confirm"), 2).on_click(cx.listener(
                    |this, _, _, cx| {
                        if let Some(op) = this.ask.take() {
                            this.hub.update(cx, |h, cx| h.enqueue(op, cx));
                        }
                    },
                )),
            )
            .child(
                ui::button_sm("brew-ask-no", t("brew.cancel"), 0).on_click(cx.listener(
                    |this, _, _, cx| {
                        this.ask = None;
                        cx.notify();
                    },
                )),
            )
    }

    fn kind_chips(&self, cx: &mut Context<Self>) -> gpui::Div {
        let chip = |id, label, on, v: u8, cx: &mut Context<Self>| {
            ui::chip(id, label, on).on_click(cx.listener(move |this, _, _, cx| {
                this.kind_filter = v;
                cx.notify();
            }))
        };
        div()
            .flex_shrink_0()
            .px(px(ui::SP_3))
            .py(px(ui::SP_2))
            .flex()
            .gap(px(ui::SP_2))
            .child(chip(
                "brew-k-all",
                t("brew.kind_all").to_string(),
                self.kind_filter == 0,
                0,
                cx,
            ))
            .child(chip(
                "brew-k-f",
                t("brew.formulae").to_string(),
                self.kind_filter == 1,
                1,
                cx,
            ))
            .child(chip(
                "brew-k-c",
                t("brew.casks").to_string(),
                self.kind_filter == 2,
                2,
                cx,
            ))
    }

    fn table(&self, rows: &[&Package], empty: &str, cx: &mut Context<Self>) -> gpui::Div {
        let th = cur();
        let head = ui::table_header()
            .px(px(ui::SP_3))
            .child(div().flex_1().child(t("brew.col_name").to_string()))
            .child(div().w(px(56.)).child(t("brew.col_kind").to_string()))
            .child(div().w(px(168.)).child(t("brew.version").to_string()))
            .child(div().w(px(76.)));
        let mut body = div()
            .id("brew-table")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .flex()
            .flex_col();
        if rows.is_empty() {
            body = body.child(
                ui::empty_state(IconName::Beer, empty.to_string(), None, vec![], true).flex_none(),
            );
        }
        for p in rows {
            let id = p.id.clone();
            let up = p.outdated;
            let cmd = p.cmd.clone();
            let cask = p.kind == Kind::Cask;
            let ver = brew::version_line(&p.installed, &p.latest, p.outdated);
            let kind = if p.kind == Kind::Formula {
                t("brew.formulae")
            } else {
                t("brew.casks")
            };
            let mut row = div()
                .id(SharedString::from(format!("brew-row-{}", p.id)))
                .min_h(px(ui::row_h()))
                .px(px(ui::SP_3))
                .flex()
                .items_center()
                .gap(px(ui::SP_2))
                .cursor_pointer()
                .border_b_1()
                .border_color(crate::theme::divider())
                .hover(|s| s.bg(crate::theme::hover_bg()))
                .on_click(cx.listener(move |_, _, _, cx| {
                    cx.emit(BrewEvent::Open(Page::Detail, Some(id.clone())));
                }))
                .child(ui::row_title(p.title.clone()).flex_1())
                .child(
                    div()
                        .w(px(56.))
                        .flex_shrink_0()
                        .child(ui::row_meta(kind.to_string())),
                )
                .child(div().w(px(168.)).flex_shrink_0().child(
                    ui::row_meta(ver).text_color(if up { c(th.warning) } else { c(th.subtext) }),
                ));
            if up {
                let bid = SharedString::from(format!("brew-up-{}", p.id));
                row = row.child(div().w(px(76.)).flex_shrink_0().flex().justify_end().child(
                    ui::button_sm(bid, t("brew.upgrade"), 1).on_click(cx.listener(
                        move |this, _, _, cx| {
                            cx.stop_propagation();
                            this.act(
                                Op::Upgrade {
                                    name: cmd.clone(),
                                    cask,
                                },
                                cx,
                            );
                        },
                    )),
                ));
            } else {
                row = row.child(div().w(px(76.)).flex_shrink_0());
            }
            body = body.child(row);
        }
        div()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .child(head)
            .child(body)
    }

    fn detail(&self, p: &Package, svc: Option<&Service>, cx: &mut Context<Self>) -> gpui::Div {
        let th = cur();
        let mut head = div()
            .flex()
            .items_center()
            .gap(px(ui::SP_2))
            .flex_wrap()
            .child(
                div()
                    .text_size(px(ui::fs_title()))
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(p.title.clone()),
            )
            .child(ui::tag(
                if p.kind == Kind::Formula {
                    t("brew.formulae")
                } else {
                    t("brew.casks")
                }
                .to_string(),
                c(th.overlay1),
            ));
        if p.outdated {
            head = head.child(ui::tag(t("brew.outdated_badge").to_string(), c(th.warning)));
        }
        if p.pinned {
            head = head.child(ui::tag(t("brew.pinned").to_string(), c(th.accent)));
        }
        if let Some(s) = svc {
            head = head.child(ui::tag(
                t(brew::service_label_key(&s.status)).to_string(),
                svc_color(&s.status),
            ));
        }

        let mut actions = div().flex().flex_wrap().gap(px(ui::SP_2));
        if let Some(s) = svc {
            actions = actions
                .child(self.op_btn(
                    "brew-svc-start",
                    t("brew.svc_start"),
                    if s.status == "started" { 0 } else { 1 },
                    Op::Service {
                        name: s.name.clone(),
                        act: SvcAct::Start,
                    },
                    cx,
                ))
                .child(self.op_btn(
                    "brew-svc-stop",
                    t("brew.svc_stop"),
                    0,
                    Op::Service {
                        name: s.name.clone(),
                        act: SvcAct::Stop,
                    },
                    cx,
                ))
                .child(self.op_btn(
                    "brew-svc-restart",
                    t("brew.svc_restart"),
                    0,
                    Op::Service {
                        name: s.name.clone(),
                        act: SvcAct::Restart,
                    },
                    cx,
                ));
        }
        if p.outdated {
            actions = actions.child(self.op_btn(
                "brew-one-up",
                t("brew.upgrade"),
                1,
                Op::Upgrade {
                    name: p.cmd.clone(),
                    cask: p.kind == Kind::Cask,
                },
                cx,
            ));
        }
        actions = actions.child(self.op_btn(
            "brew-one-rm",
            t("brew.uninstall"),
            2,
            Op::Uninstall {
                name: p.cmd.clone(),
                cask: p.kind == Kind::Cask,
            },
            cx,
        ));
        if p.kind == Kind::Formula {
            let (label, unpin) = if p.pinned {
                (t("brew.unpin"), true)
            } else {
                (t("brew.pin"), false)
            };
            actions = actions.child(self.op_btn(
                if unpin { "brew-unpin" } else { "brew-pin" },
                label,
                0,
                Op::Pin {
                    name: p.cmd.clone(),
                    unpin,
                },
                cx,
            ));
        }

        let mut block = div().flex().flex_col().gap(px(ui::SP_3)).child(head);
        if !p.desc.is_empty() {
            block = block.child(
                div()
                    .text_size(px(ui::fs_body()))
                    .text_color(c(th.text))
                    .whitespace_normal()
                    .child(p.desc.clone()),
            );
        }
        block = block.child(actions);
        block = block.child(kv(
            t("brew.version"),
            brew::version_line(&p.installed, &p.latest, p.outdated),
        ));
        block = block.child(kv(t("brew.homepage"), blank(&p.homepage)));
        block = block.child(kv(t("brew.tap"), blank(&p.tap)));
        block = block.child(kv(t("brew.deps"), blank(&p.dependencies.join(", "))));
        if let Some(s) = svc {
            let who = if s.user.is_empty() {
                t(brew::service_label_key(&s.status)).to_string()
            } else {
                format!("{} · {}", t(brew::service_label_key(&s.status)), s.user)
            };
            block = block.child(kv(t("brew.svc_status"), who));
        }
        if !p.caveats.is_empty() {
            block = block.child(kv(t("brew.caveats"), p.caveats.clone()));
        }
        block
    }

    fn service_only(&self, s: &Service, cx: &mut Context<Self>) -> gpui::Div {
        let th = cur();
        div()
            .flex()
            .flex_col()
            .gap(px(ui::SP_3))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(ui::SP_2))
                    .child(
                        div()
                            .text_size(px(ui::fs_title()))
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(s.name.clone()),
                    )
                    .child(ui::tag(
                        t(brew::service_label_key(&s.status)).to_string(),
                        svc_color(&s.status),
                    )),
            )
            .child(
                div()
                    .text_size(px(ui::fs_small()))
                    .text_color(c(th.subtext))
                    .child(t("brew.service_only").to_string()),
            )
            .child(
                div()
                    .flex()
                    .gap(px(ui::SP_2))
                    .child(self.op_btn(
                        "brew-only-start",
                        t("brew.svc_start"),
                        1,
                        Op::Service {
                            name: s.name.clone(),
                            act: SvcAct::Start,
                        },
                        cx,
                    ))
                    .child(self.op_btn(
                        "brew-only-stop",
                        t("brew.svc_stop"),
                        0,
                        Op::Service {
                            name: s.name.clone(),
                            act: SvcAct::Stop,
                        },
                        cx,
                    ))
                    .child(self.op_btn(
                        "brew-only-restart",
                        t("brew.svc_restart"),
                        0,
                        Op::Service {
                            name: s.name.clone(),
                            act: SvcAct::Restart,
                        },
                        cx,
                    )),
            )
            .when(!s.user.is_empty(), |d| {
                d.child(kv(t("brew.user"), s.user.clone()))
            })
    }

    fn log_panel(&self, log: &[String], status: Option<&str>, cx: &mut Context<Self>) -> gpui::Div {
        let th = cur();
        let open = self.log_open;
        let last = log
            .last()
            .cloned()
            .or_else(|| status.map(|s| s.to_string()))
            .unwrap_or_else(|| t("brew.output_empty").to_string());
        let mut head = div()
            .h(px(ui::H_COMPACT + 4.))
            .px(px(ui::SP_3))
            .flex_shrink_0()
            .flex()
            .items_center()
            .gap(px(ui::SP_2))
            .border_t_1()
            .border_color(crate::theme::divider())
            .child(
                ui::icon_button(
                    "brew-log-toggle",
                    if open {
                        IconName::ChevronDown
                    } else {
                        IconName::ChevronRight
                    },
                    ui::IconBtn::Sm,
                    if open {
                        t("brew.collapse")
                    } else {
                        t("brew.expand")
                    },
                )
                .on_click(cx.listener(|this, _, _, cx| {
                    this.log_open = !this.log_open;
                    cx.notify();
                })),
            )
            .child(
                div()
                    .text_size(px(ui::fs_small()))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(c(th.subtext))
                    .child(t("brew.output").to_string()),
            );
        if !open {
            head = head.child(
                div()
                    .flex_1()
                    .min_w_0()
                    .whitespace_nowrap()
                    .overflow_hidden()
                    .text_ellipsis()
                    .font_family(ui::mono_family())
                    .text_size(px(ui::fs_small()))
                    .text_color(c(th.overlay1))
                    .child(last),
            );
        } else {
            head = head.child(div().flex_1());
        }
        head = head.child(
            ui::icon_button(
                "brew-log-clear",
                IconName::Trash,
                ui::IconBtn::Sm,
                t("brew.clear_log"),
            )
            .on_click(cx.listener(|this, _, _, cx| this.hub.update(cx, |h, cx| h.clear_log(cx)))),
        );
        let mut panel = div().flex_shrink_0().flex().flex_col().child(head);
        if open {
            let mut lines = div()
                .id("brew-log")
                .track_scroll(&self.scroll)
                .h(px(160.))
                .overflow_y_scroll()
                .px(px(ui::SP_3))
                .py(px(ui::SP_2))
                .flex()
                .flex_col()
                .gap(px(2.));
            if log.is_empty() {
                lines = lines.child(
                    div()
                        .text_size(px(ui::fs_small()))
                        .text_color(c(th.overlay1))
                        .child(t("brew.output_empty").to_string()),
                );
            }
            for (i, line) in log.iter().enumerate() {
                let cmd = line.starts_with("$ ");
                lines = lines.child(
                    div()
                        .id(("brew-line", i))
                        .font_family(ui::mono_family())
                        .text_size(px(ui::fs_mono()))
                        .whitespace_normal()
                        .text_color(if cmd { c(th.text) } else { c(th.subtext) })
                        .child(line.clone()),
                );
            }
            panel = panel.child(lines);
        }
        panel
    }
}

fn table_rows<'a>(
    packages: &'a [Package],
    only_outdated: bool,
    searching: bool,
    kind: u8,
    q: &str,
) -> Vec<&'a Package> {
    packages
        .iter()
        .filter(|p| {
            if only_outdated && !p.outdated {
                return false;
            }
            if kind == 1 && p.kind != Kind::Formula {
                return false;
            }
            if kind == 2 && p.kind != Kind::Cask {
                return false;
            }
            if searching && !hit(p, q) {
                return false;
            }
            true
        })
        .collect()
}

fn hit(p: &Package, q: &str) -> bool {
    format!("{} {} {} {}", p.name, p.title, p.desc, p.installed)
        .to_lowercase()
        .contains(q)
}

fn confirm_text(op: &Op) -> String {
    match op {
        Op::Uninstall { name, .. } => tf("brew.confirm_uninstall", &[("name", name)]),
        Op::UpgradeAll => t("brew.confirm_upgrade_all").into(),
        Op::Cleanup => t("brew.confirm_cleanup").into(),
        Op::Install { name, .. } => tf("brew.confirm_install", &[("name", name)]),
        _ => t("brew.confirm").into(),
    }
}

fn kv(label: &str, value: String) -> gpui::Div {
    let th = cur();
    div()
        .flex()
        .gap(px(ui::SP_3))
        .child(
            div()
                .w(px(72.))
                .flex_shrink_0()
                .text_size(px(ui::fs_small()))
                .text_color(c(th.overlay1))
                .child(label.to_string()),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .text_size(px(ui::fs_small()))
                .text_color(c(th.text))
                .whitespace_normal()
                .child(value),
        )
}

fn blank(s: &str) -> String {
    if s.trim().is_empty() {
        t("brew.none").into()
    } else {
        s.to_string()
    }
}

fn svc_color(status: &str) -> gpui::Hsla {
    let th = cur();
    match status {
        "started" => c(th.success),
        "error" | "exited" => c(th.error),
        _ => c(th.overlay1),
    }
}
