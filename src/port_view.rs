//! Port forwarding manager: saved forwards (data_dir/forwards.toml), add/start/stop/delete,
//! live state and counters, auto-start when an SSH session to that host opens.
use std::sync::atomic::Ordering;
use std::sync::Arc;

use gpui::{
    deferred, div, prelude::*, px, ClipboardItem, Context, Entity, FontWeight, SharedString, Window,
};
use gpui_component::input::InputState;
use gpui_component::switch::Switch;
use ssh_term_proto::forward::{self, Kind, Running, Spec, State};
use ssh_term_proto::i18n::{t, tf};
use ssh_term_proto::ssh::{self, SshTarget};

use crate::theme::{c, cur};
use crate::ui::{self, icon, IconName};

pub enum PortToast {
    Toast(crate::ToastKind, String),
}
impl gpui::EventEmitter<PortToast> for PortView {}

pub struct PortView {
    rt: tokio::runtime::Handle,
    prompts: ssh::Prompts,
    pub target: Option<SshTarget>,
    pub items: Vec<Arc<Running>>,
    pub host_filter: Option<String>,
    kind: Kind,
    auto: bool,
    fields: [Entity<InputState>; 4],
    host_menu_open: bool,
    ticking: bool,
}

fn store_path() -> std::path::PathBuf {
    crate::data_dir().join("forwards.toml")
}

impl PortView {
    pub fn new(
        rt: tokio::runtime::Handle,
        prompts: ssh::Prompts,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let mk = |ph: &str, v: &str, window: &mut Window, cx: &mut Context<Self>| {
            let (ph, v) = (ph.to_string(), v.to_string());
            cx.new(|cx| InputState::new(window, cx).placeholder(ph).default_value(v))
        };
        let fields = [
            mk("127.0.0.1", "127.0.0.1", window, cx),
            mk("8080", "", window, cx),
            mk("localhost", "localhost", window, cx),
            mk("80", "", window, cx),
        ];
        let items = forward::load(&store_path())
            .into_iter()
            .map(Running::new)
            .collect();
        let default_tg = crate::config::resolved_hosts()
            .first()
            .map(|(_, tg)| tg.clone());
        Self {
            rt,
            prompts,
            target: default_tg,
            items,
            host_filter: None,
            kind: Kind::L,
            auto: false,
            fields,
            host_menu_open: false,
            ticking: false,
        }
    }

    pub fn ensure_default_target(&mut self) {
        if self.target.is_none() {
            self.target = crate::config::resolved_hosts()
                .first()
                .map(|(_, tg)| tg.clone());
        }
    }

    pub fn resolve_target(&self, host_or_label: &str) -> Option<SshTarget> {
        if let Some(tg) = &self.target {
            if tg.label() == host_or_label {
                return Some(tg.clone());
            }
        }
        crate::config::resolved_hosts()
            .iter()
            .find(|(he, tg)| he.name == host_or_label || tg.label() == host_or_label)
            .map(|(_, tg)| tg.clone())
    }

    pub fn active_count(&self) -> usize {
        self.items.iter().filter(|r| r.is_on()).count()
    }

    pub fn tab_title(&self) -> String {
        let on = self.active_count();
        if on > 0 {
            format!("{} ({on})", t("tool.ports"))
        } else {
            t("tool.ports").to_string()
        }
    }

    pub fn crumbs(&self) -> String {
        tf(
            "ux.pf_active_summary",
            &[("on", &self.active_count()), ("total", &self.items.len())],
        )
    }

    pub fn start_rule(&mut self, r: Arc<Running>, cx: &mut Context<Self>) {
        if let Some(tg) = self.resolve_target(&r.spec.host) {
            self.start(r, tg, cx);
        } else {
            cx.emit(PortToast::Toast(
                crate::ToastKind::Error,
                tf("pf.open_host_first", &[("h", &r.spec.host)]),
            ));
        }
    }

    pub fn stop_rule(&mut self, r: Arc<Running>, cx: &mut Context<Self>) {
        self.stop(r, cx);
    }

    pub fn stop_all(&mut self, cx: &mut Context<Self>) {
        let running: Vec<_> = self.items.iter().filter(|r| r.is_on()).cloned().collect();
        for r in running {
            self.stop(r, cx);
        }
    }

    fn apply_preset(
        &mut self,
        kind: Kind,
        bind_addr: &str,
        bind_port: &str,
        dest_host: &str,
        dest_port: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.kind = kind;
        let vals = [bind_addr, bind_port, dest_host, dest_port];
        for (i, v) in vals.iter().enumerate() {
            let s = (*v).to_string();
            self.fields[i].update(cx, |inp, cx| inp.set_value(s, window, cx));
        }
        cx.notify();
    }

    fn save(&self) {
        let _ = forward::save(
            &store_path(),
            &self
                .items
                .iter()
                .map(|r| r.spec.clone())
                .collect::<Vec<_>>(),
        );
    }
    fn val(&self, i: usize, cx: &Context<Self>) -> String {
        self.fields[i].read(cx).value().trim().to_string()
    }

    fn add(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.ensure_default_target();
        let Some(tg) = self.target.clone() else {
            cx.emit(PortToast::Toast(
                crate::ToastKind::Error,
                t("pf.need_ssh").into(),
            ));
            return;
        };
        let port = |s: String| s.parse::<u16>().ok();
        let bp = self.val(1, cx);
        let spec = Spec {
            host: tg.label(),
            kind: self.kind,
            bind_addr: self.val(0, cx),
            bind_port: if bp.is_empty() && self.kind == Kind::R {
                0
            } else {
                port(bp).unwrap_or(0)
            },
            dest_host: self.val(2, cx),
            dest_port: port(self.val(3, cx)).unwrap_or(0),
            auto_start: self.auto,
        };
        if let Err(e) = spec.validate() {
            cx.emit(PortToast::Toast(crate::ToastKind::Error, format!("{e}")));
            return;
        }
        for i in [1, 3] {
            self.fields[i].update(cx, |s, cx| s.set_value("", window, cx));
        }
        let r = Running::new(spec);
        self.items.push(r.clone());
        self.save();
        self.start(r, tg, cx);
    }
    pub fn start(&mut self, r: Arc<Running>, tg: SshTarget, cx: &mut Context<Self>) {
        let p = self.prompts.clone();
        *r.state.lock().unwrap() = State::Starting;
        self.rt.spawn(async move {
            match ssh::connect_shared(&tg, &p).await {
                Ok((conn, _)) => {
                    let _ = r.start(conn).await;
                }
                Err(e) => *r.state.lock().unwrap() = State::Error(format!("{e:#}")),
            }
        });
        self.tick(cx);
    }
    fn stop(&mut self, r: Arc<Running>, cx: &mut Context<Self>) {
        self.rt.spawn(async move { r.stop().await });
        self.tick(cx);
    }
    /// Called when an SSH session to `tg` opens: start its auto-start forwards.
    pub fn auto_start(&mut self, tg: &SshTarget, cx: &mut Context<Self>) {
        if self.target.is_none() {
            self.target = Some(tg.clone());
        }
        let label = tg.label();
        let todo: Vec<_> = self
            .items
            .iter()
            .filter(|r| r.spec.auto_start && r.spec.host == label && !r.is_on())
            .cloned()
            .collect();
        for r in todo {
            self.start(r, tg.clone(), cx);
        }
    }
    fn tick(&mut self, cx: &mut Context<Self>) {
        if self.ticking {
            return;
        }
        self.ticking = true;
        cx.spawn(async move |this, cx| {
            for _ in 0..20 {
                cx.background_executor()
                    .timer(std::time::Duration::from_millis(250))
                    .await;
                if this.update(cx, |_, cx| cx.notify()).is_err() {
                    return;
                }
            }
            let _ = this.update(cx, |v, _| v.ticking = false);
        })
        .detach();
    }
}

pub fn listen_addr_of(r: &Running, st: &State) -> String {
    let port = match st {
        State::Running { port } => *port,
        _ => r.spec.bind_port,
    };
    let addr = if r.spec.bind_addr.trim().is_empty() {
        "127.0.0.1"
    } else {
        r.spec.bind_addr.trim()
    };
    if r.spec.kind == Kind::D {
        format!("socks5://{addr}:{port}")
    } else {
        format!("{addr}:{port}")
    }
}

fn human(n: u64) -> String {
    if n < 1024 {
        format!("{n} B")
    } else if n < 1 << 20 {
        format!("{:.1} KB", n as f64 / 1024.0)
    } else {
        format!("{:.1} MB", n as f64 / 1048576.0)
    }
}

impl Render for PortView {
    fn render(&mut self, _w: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.ensure_default_target();
        let th = cur();
        let resolved = crate::config::resolved_hosts();
        let cur_host_display = if let Some(tg) = &self.target {
            let lbl = tg.label();
            if let Some((he, _)) = resolved.iter().find(|(_, t)| t.label() == lbl) {
                if he.name != lbl {
                    format!("{} ({lbl})", he.name)
                } else {
                    lbl
                }
            } else {
                lbl
            }
        } else {
            t("pf.need_ssh").to_string()
        };

        let mut host_picker = div().relative().child(
            div()
                .id("pf-host-btn")
                .h(px(ui::H_DEFAULT))
                .px(px(ui::SP_2))
                .rounded(px(ui::R_MD))
                .bg(crate::theme::input_bg())
                .border_1()
                .border_color(if self.host_menu_open {
                    c(th.accent)
                } else {
                    crate::theme::divider()
                })
                .hover(|s| s.bg(crate::theme::hover_bg()))
                .cursor_pointer()
                .flex()
                .items_center()
                .gap(px(6.))
                .on_click(cx.listener(|v, _, _, cx| {
                    v.host_menu_open = !v.host_menu_open;
                    cx.notify();
                }))
                .child(icon(IconName::Server, ui::IC_XS, c(th.accent)))
                .child(
                    div()
                        .text_size(px(ui::fs_small()))
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(c(th.text))
                        .max_w(px(260.))
                        .overflow_hidden()
                        .text_ellipsis()
                        .whitespace_nowrap()
                        .child(cur_host_display),
                )
                .child(icon(IconName::ChevronDown, ui::IC_XS, c(th.overlay1))),
        );

        if self.host_menu_open && !resolved.is_empty() {
            let sel_lbl = self.target.as_ref().map(|t| t.label()).unwrap_or_default();
            let mut menu = ui::popover()
                .absolute()
                .top(px(ui::H_DEFAULT + 4.))
                .left_0()
                .w(px(280.))
                .max_h(px(280.))
                .p_1()
                .id("pf-host-pop")
                .overflow_y_scroll()
                .flex()
                .flex_col();
            for (idx, (he, tg)) in resolved.iter().enumerate() {
                let lbl = tg.label();
                let is_sel = lbl == sel_lbl;
                let tg_cl = tg.clone();
                menu = menu.child(
                    div()
                        .id(("pf-host-opt", idx))
                        .px(px(ui::SP_2))
                        .py(px(6.))
                        .rounded(px(ui::R_SM))
                        .cursor_pointer()
                        .bg(if is_sel {
                            crate::theme::menu_sel_bg()
                        } else {
                            gpui::transparent_black()
                        })
                        .hover(|s| s.bg(crate::theme::hover_bg()))
                        .flex()
                        .items_center()
                        .gap_2()
                        .on_click(cx.listener(move |v, _, _, cx| {
                            v.target = Some(tg_cl.clone());
                            v.host_menu_open = false;
                            cx.notify();
                        }))
                        .child(icon(
                            IconName::Server,
                            ui::IC_XS,
                            if is_sel { c(th.accent) } else { c(th.overlay1) },
                        ))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .flex()
                                .flex_col()
                                .child(
                                    div()
                                        .text_size(px(ui::fs_small()))
                                        .font_weight(FontWeight::MEDIUM)
                                        .text_color(c(th.text))
                                        .overflow_hidden()
                                        .text_ellipsis()
                                        .whitespace_nowrap()
                                        .child(he.name.clone()),
                                )
                                .child(
                                    div()
                                        .text_size(px(ui::fs_caption()))
                                        .text_color(c(th.overlay1))
                                        .overflow_hidden()
                                        .text_ellipsis()
                                        .whitespace_nowrap()
                                        .child(lbl),
                                ),
                        )
                        .when(is_sel, |d| {
                            d.child(icon(IconName::Check, ui::IC_XS, c(th.accent)))
                        }),
                );
            }
            host_picker = host_picker.child(deferred(menu).with_priority(3));
        }

        let labels = match self.kind {
            Kind::L => [
                t("ui.local_address"),
                t("ui.local_port"),
                t("ui.remote_host"),
                t("ui.remote_port"),
            ],
            Kind::R => [
                t("pf.remote_bind"),
                t("pf.remote_port_0"),
                t("pf.local_dest"),
                t("pf.local_dest_port"),
            ],
            Kind::D => [t("ui.local_address"), t("ui.local_port"), "", ""],
        };
        let mut inputs = div().flex().flex_wrap().items_end().gap_3();
        for (i, l) in labels.iter().enumerate() {
            if l.is_empty() {
                continue;
            }
            let width = if i == 1 || i == 3 { 128. } else { 176. };
            inputs = inputs.child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .w(px(width))
                    .child(
                        div()
                            .text_size(px(ui::fs_small()))
                            .text_color(c(th.overlay1))
                            .child(l.to_string()),
                    )
                    .child(ui::text_field(&self.fields[i], false)),
            );
        }
        inputs = inputs
            .child(
                div()
                    .h(px(ui::H_DEFAULT))
                    .flex()
                    .items_center()
                    .child(
                        Switch::new("pf-auto")
                            .checked(self.auto)
                            .label(t("pf.auto_start"))
                            .on_click(cx.listener(|v, on: &bool, _, cx| {
                                v.auto = *on;
                                cx.notify()
                            })),
                    ),
            )
            .child(
                ui::button("pf-add", t("ui.add_and_start"), 1)
                    .on_click(cx.listener(|v, _, w, cx| v.add(w, cx))),
            );

        let kinds = vec![
            ("L".to_string(), t("ui.local_l").to_string()),
            ("R".to_string(), t("ui.remote_r").to_string()),
            ("D".to_string(), t("ui.dynamic_socks_d").to_string()),
        ];
        let sel = match self.kind {
            Kind::L => "L",
            Kind::R => "R",
            Kind::D => "D",
        };
        let me = cx.entity().downgrade();

        let presets: [(&'static str, &'static str, Kind, &'static str, &'static str, &'static str, &'static str); 5] = [
            ("pf-pre-web", t("ux.pf_preset_web"), Kind::L, "127.0.0.1", "8080", "localhost", "80"),
            ("pf-pre-mysql", t("ux.pf_preset_mysql"), Kind::L, "127.0.0.1", "3306", "127.0.0.1", "3306"),
            ("pf-pre-pg", t("ux.pf_preset_pg"), Kind::L, "127.0.0.1", "5432", "127.0.0.1", "5432"),
            ("pf-pre-redis", t("ux.pf_preset_redis"), Kind::L, "127.0.0.1", "6379", "127.0.0.1", "6379"),
            ("pf-pre-socks", t("ux.pf_preset_socks"), Kind::D, "127.0.0.1", "1080", "localhost", "80"),
        ];
        let mut preset_row = div().flex().flex_wrap().items_center().gap(px(6.));
        for (pid, plabel, pk, ba, bp, dh, dp) in presets {
            preset_row = preset_row.child(
                ui::chip(pid, plabel, false).on_click(cx.listener(move |v, _, w, cx| {
                    v.apply_preset(pk, ba, bp, dh, dp, w, cx);
                })),
            );
        }

        let form = div()
            .p_4()
            .mx_4()
            .mt_3()
            .mb_3()
            .rounded(px(ui::R_SUB))
            .bg(crate::theme::sub_bg())
            .border_1()
            .border_color(crate::theme::divider())
            .flex()
            .flex_col()
            .gap_3()
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap_3()
                    .child(
                        div()
                            .text_size(px(ui::fs_body()))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(c(th.text))
                            .child(t("ui.new_forward")),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(6.))
                            .child(
                                div()
                                    .text_size(px(ui::fs_small()))
                                    .text_color(c(th.overlay1))
                                    .child(t("ux.pf_host")),
                            )
                            .child(host_picker),
                    )
                    .child(div().flex_1())
                    .child(ui::segmented("pf-kind", &kinds, sel, move |v, _, cx| {
                        let _ = me.update(cx, |p, cx| {
                            p.kind = match v.as_str() {
                                "R" => Kind::R,
                                "D" => Kind::D,
                                _ => Kind::L,
                            };
                            cx.notify()
                        });
                    })),
            )
            .child(preset_row)
            .child(inputs);

        // Distinct hosts across saved rules for quick host filtering
        let mut distinct_hosts: Vec<String> = Vec::new();
        for r in &self.items {
            if !distinct_hosts.iter().any(|h| h == &r.spec.host) {
                distinct_hosts.push(r.spec.host.clone());
            }
        }
        let show_host_chips = distinct_hosts.len() >= 2 || self.host_filter.is_some();
        let mut filter_bar = div().mx_4().mb_2().flex().flex_wrap().items_center().gap(px(6.));
        if show_host_chips {
            let all_on = self.host_filter.is_none();
            filter_bar = filter_bar.child(
                ui::chip("pf-hf-all", t("ux.pf_all_hosts"), all_on).on_click(cx.listener(
                    |v, _, _, cx| {
                        v.host_filter = None;
                        cx.notify();
                    },
                )),
            );
            for (hi, hname) in distinct_hosts.iter().enumerate() {
                let on = self.host_filter.as_deref() == Some(hname.as_str());
                let h_cl = hname.clone();
                filter_bar = filter_bar.child(
                    ui::chip(SharedString::from(format!("pf-hf-{hi}")), hname.clone(), on)
                        .on_click(cx.listener(move |v, _, _, cx| {
                            v.host_filter = Some(h_cl.clone());
                            cx.notify();
                        })),
                );
            }
        }

        let visible_items: Vec<(usize, Arc<Running>)> = self
            .items
            .iter()
            .cloned()
            .enumerate()
            .filter(|(_, r)| {
                self.host_filter
                    .as_deref()
                    .is_none_or(|hf| r.spec.host == hf)
            })
            .collect();

        let mut list = div()
            .id("pf-list")
            .mx_4()
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .rounded(px(ui::R_SUB))
            .border_1()
            .border_color(crate::theme::divider());
        if visible_items.is_empty() {
            list = list.child(ui::empty_state(
                IconName::ArrowLeftRight,
                t("ui.no_port_forwards"),
                Some(t("ui.forwards_will_be_saved_per").into()),
                vec![],
                true,
            ));
        }
        for (row_idx, (i, r)) in visible_items.into_iter().enumerate() {
            let st = r.state();
            let (label, col) = match &st {
                State::Stopped => (t("pf.stopped").to_string(), c(th.overlay1)),
                State::Starting => (t("pf.starting").to_string(), c(th.info)),
                State::Running { port } => (tf("pf.running", &[("p", port)]), c(th.success)),
                State::Error(_) => (t("pf.error").to_string(), c(th.error)),
            };
            let on = r.is_on();
            let (r1, r2) = (r.clone(), r.clone());
            let listen_addr = listen_addr_of(&r, &st);
            let stats = tf(
                "pf.stats",
                &[
                    ("a", &r.stats.active.load(Ordering::Relaxed)),
                    ("t", &r.stats.total.load(Ordering::Relaxed)),
                    ("b", &human(r.stats.bytes.load(Ordering::Relaxed))),
                ],
            );
            list = list.child(
                div()
                    .px_3()
                    .py(px(8.))
                    .flex()
                    .items_center()
                    .gap_3()
                    .hover(|s| s.bg(crate::theme::hover_bg()))
                    .when(row_idx > 0, |d| {
                        d.border_t_1().border_color(crate::theme::divider())
                    })
                    .text_size(px(ui::fs_body()))
                    .text_color(c(th.text))
                    .child(icon(IconName::ArrowLeftRight, ui::IC_SM, col))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .child(div().font_weight(FontWeight::MEDIUM).child(r.spec.describe()))
                            .child(diagram(
                                &r.spec,
                                match &st {
                                    State::Running { port } => Some(*port),
                                    _ => None,
                                },
                                on && matches!(st, State::Running { .. }),
                            ))
                            .child(
                                div()
                                    .text_size(px(ui::fs_small()))
                                    .text_color(c(th.overlay1))
                                    .child(format!(
                                        "{} · {}{}",
                                        r.spec.host,
                                        stats,
                                        if r.spec.auto_start {
                                            format!(" · {}", t("pf.auto_start"))
                                        } else {
                                            String::new()
                                        }
                                    )),
                            )
                            .when_some(
                                match &st {
                                    State::Error(e) => Some(e.clone()),
                                    _ => None,
                                },
                                |d, e| {
                                    d.child(
                                        ui::notice(ui::Notice::Error, e).mx_0().mt(px(ui::SP_1)),
                                    )
                                },
                            ),
                    )
                    .child(ui::tag(label, col))
                    .child(
                        ui::icon_button(
                            ("pf-copy", i),
                            IconName::Copy,
                            ui::IconBtn::Sm,
                            t("ux.pf_copy_addr"),
                        )
                        .on_click(cx.listener(move |_, _, _, cx| {
                            cx.write_to_clipboard(ClipboardItem::new_string(listen_addr.clone()));
                            cx.emit(PortToast::Toast(
                                crate::ToastKind::Success,
                                tf("ux.pf_copied_addr", &[("addr", &listen_addr)]),
                            ));
                        })),
                    )
                    .child(if on {
                        ui::button_sm(("pf-stop", i), t("pf.stop"), 0)
                            .on_click(cx.listener(move |v, _, _, cx| v.stop_rule(r1.clone(), cx)))
                    } else {
                        ui::button_sm(("pf-start", i), t("pf.start"), 1)
                            .on_click(cx.listener(move |v, _, _, cx| v.start_rule(r1.clone(), cx)))
                    })
                    .child(
                        ui::button_sm(("pf-del", i), t("sftp.delete"), 2).on_click(cx.listener(
                            move |v, _, _, cx| {
                                let r = r2.clone();
                                v.stop(r.clone(), cx);
                                v.items.retain(|x| !Arc::ptr_eq(x, &r));
                                v.save();
                                cx.notify()
                            },
                        )),
                    ),
            );
        }

        let active_n = self.active_count();
        let summary = self.crumbs();
        let mut bar = ui::toolbar()
            .child(icon(IconName::ArrowLeftRight, ui::IC_SM, c(th.accent)))
            .child(
                div()
                    .text_size(px(ui::fs_body()))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(c(th.text))
                    .child(t("ui.port_forwarding")),
            )
            .child(ui::tag(
                summary,
                if active_n > 0 {
                    c(th.success)
                } else {
                    c(th.overlay1)
                },
            ))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_size(px(ui::fs_small()))
                    .text_color(c(th.overlay1))
                    .overflow_hidden()
                    .text_ellipsis()
                    .whitespace_nowrap()
                    .child(t("pf.subtitle")),
            );
        if active_n > 0 {
            bar = bar.child(
                ui::button_sm("pf-stop-all", t("ux.pf_stop_all"), 2)
                    .on_click(cx.listener(|v, _, _, cx| v.stop_all(cx))),
            );
        }

        div()
            .flex_1()
            .size_full()
            .flex()
            .flex_col()
            .child(bar)
            .child(form)
            .when(show_host_chips, |d| d.child(filter_bar))
            .child(list)
            .child(div().h(px(12.)))
            .into_any_element()
    }
}

/// Port-forward diagram: [listener] ══ssh tunnel══ [ssh server] ──► [destination].
fn diagram(
    spec: &ssh_term_proto::forward::Spec,
    port: Option<u16>,
    live: bool,
) -> impl IntoElement {
    let th = cur();
    let (nodes, _) = spec.diagram(port);
    let role = |r: &str| match r {
        "local" => t("pf.node_local"),
        "remote" => t("pf.node_remote"),
        "ssh" => t("pf.node_ssh"),
        "remote-side" => t("pf.node_remote_side"),
        "local-side" => t("pf.node_local_side"),
        _ => t("pf.node_any"),
    };
    let col = |i: usize| match i {
        0 => c(th.accent),
        1 => c(th.warning),
        _ => c(th.success),
    };
    let boxn = |i: usize, title: &str, addr: &str| {
        div()
            .px_2()
            .py(px(3.))
            .rounded(px(ui::R_MD))
            .border_1()
            .border_color(col(i))
            .bg(col(i).opacity(0.08))
            .flex()
            .flex_col()
            .child(
                div()
                    .text_size(px(ui::fs_small()))
                    .text_color(col(i))
                    .child(role(title).to_string()),
            )
            .child(
                div()
                    .text_size(px(ui::fs_body()))
                    .text_color(c(th.text))
                    .child(addr.to_string()),
            )
    };
    let line = if live { c(th.success) } else { c(th.overlay0) };
    let arrow = |label: String, thick: bool| {
        div()
            .flex()
            .flex_col()
            .items_center()
            .min_w(px(90.))
            .child(
                div()
                    .text_size(px(ui::fs_small()))
                    .text_color(c(th.overlay1))
                    .child(label),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .w_full()
                    .child(div().flex_1().h(px(if thick { 3. } else { 1.5 })).bg(line))
                    .child(
                        div()
                            .text_size(px(ui::fs_small()))
                            .text_color(line)
                            .child("▶"),
                    ),
            )
    };
    let dir_label = match spec.kind {
        ssh_term_proto::forward::Kind::R => t("pf.tunnel_back").to_string(),
        _ => t("pf.tunnel").to_string(),
    };
    div()
        .mt_1()
        .flex()
        .flex_wrap()
        .items_center()
        .gap_1()
        .child(boxn(0, &nodes[0].1, &nodes[0].2))
        .child(arrow(dir_label, true))
        .child(boxn(1, &nodes[1].1, &nodes[1].2))
        .child(arrow(t("pf.then").to_string(), false))
        .child(boxn(2, &nodes[2].1, &nodes[2].2))
}
