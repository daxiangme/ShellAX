//! ssh-term-proto v4 UI: host sidebar (config + ~/.ssh/config), tabs with split panes,
//! host-key confirmation, agent status, themes. Terminal grid painted by our own elements.
mod config;
mod termview;
mod theme;

use std::collections::HashMap;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};

use gpui::{
    div, prelude::*, px, size, AnyElement, App, Bounds, Context, Entity, FontWeight, Hsla, KeyDownEvent, SharedString,
    Window, WindowAppearance, WindowBounds, WindowOptions,
};
use ssh_term_proto::agent::{self, AgentState, Manifest};
use ssh_term_proto::hosts::{self, HostEntry};
use ssh_term_proto::layout::{auto_direction, Dir, Node};
use ssh_term_proto::ssh::{self, HostKeyDecision, HostKeyPrompt, SshTarget};
use ssh_term_proto::suggest::History;
use ssh_term_proto::{local, term::Size};
use termview::{Spec, TermView};
use theme::{c, cur, Appearance, Theme};

static FONT: std::sync::OnceLock<config::FontConfig> = std::sync::OnceLock::new();
pub fn fcfg() -> &'static config::FontConfig { FONT.get_or_init(config::FontConfig::platform_default) }
pub fn font_size() -> f32 { fcfg().size }
pub fn line_h() -> f32 { (fcfg().size * fcfg().line_height).round() }

// ---------------------------------------------------------------- app shell
struct Tab { layout: Node<Entity<TermView>>, focused: Entity<TermView> }

#[derive(Clone, Copy, PartialEq)]
enum ThemeMode { System, Fixed(usize) }

struct Root {
    rt: Arc<tokio::runtime::Runtime>,
    hosts: Vec<HostEntry>,
    default_target: SshTarget,
    known_hosts: std::path::PathBuf,
    tabs: Vec<Tab>,
    active: usize,
    themes: Vec<Arc<Theme>>,
    mode: ThemeMode,
    histories: HashMap<String, Arc<Mutex<History>>>,
    manifests: Arc<Vec<Manifest>>,
    hk_tx: flume::Sender<HostKeyPrompt>,
    hk_pending: Vec<HostKeyPrompt>,
    _subs: Vec<gpui::Subscription>,
}

fn data_dir() -> std::path::PathBuf {
    std::env::var_os("XDG_DATA_HOME").map(Into::into)
        .unwrap_or_else(|| std::path::PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".local/share"))
        .join("agentshell")
}

fn env_target(known_hosts: &std::path::Path) -> SshTarget {
    let e = HostEntry {
        name: "env".into(),
        host: std::env::var("SSH_HOST").unwrap_or_else(|_| "127.0.0.1".into()),
        port: std::env::var("SSH_PORT").ok().and_then(|p| p.parse().ok()),
        user: std::env::var("SSH_USER").ok(),
        identity_file: std::env::var("SSH_KEY").ok(),
        source: "env",
    };
    hosts::to_target(&e, known_hosts)
}

impl Root {
    fn history_for(&mut self, key: &str) -> Arc<Mutex<History>> {
        self.histories.entry(key.to_string()).or_insert_with(|| {
            // persisted per host: ~/.local/share/agentshell/history/<key>.txt
            let fname: String = key.chars().map(|c| if c.is_ascii_alphanumeric() || "@.-_".contains(c) { c } else { '_' }).collect();
            Arc::new(Mutex::new(History::open(data_dir().join("history").join(format!("{fname}.txt")))))
        }).clone()
    }
    fn open_pane(&mut self, spec: Spec, window: &mut Window, cx: &mut Context<Self>) -> Entity<TermView> {
        let init = Size { cols: 80, rows: 24 };
        let h = self.history_for(&spec.history_key());
        let m = self.manifests.clone();
        match spec {
            Spec::Local => {
                let sh = std::env::var("SHELL").unwrap_or_else(|_| "bash".into());
                let title = format!("本地 · {}", sh.rsplit('/').next().unwrap_or("sh"));
                cx.new(|cx| TermView::new(&title, Spec::Local, local::spawn_local(init, None), h, m, window, cx))
            }
            Spec::Ssh(t) => {
                let title = t.label();
                let s = ssh::spawn_ssh(self.rt.handle(), t.clone(), init, self.hk_tx.clone());
                cx.new(|cx| TermView::new(&title, Spec::Ssh(t), Ok(s), h, m, window, cx))
            }
        }
    }
    fn new_tab(&mut self, spec: Spec, window: &mut Window, cx: &mut Context<Self>) {
        let pane = self.open_pane(spec, window, cx);
        let fh = pane.read(cx).focus.clone();
        self.tabs.push(Tab { layout: Node::Leaf(pane.clone()), focused: pane });
        self.active = self.tabs.len() - 1;
        window.focus(&fh, cx);
        cx.notify();
    }
    fn focused_pane(&self, window: &Window, cx: &App) -> Option<Entity<TermView>> {
        let tab = self.tabs.get(self.active)?;
        tab.layout.leaves().into_iter().find(|p| p.read(cx).focus.is_focused(window)).or_else(|| Some(tab.focused.clone()))
    }
    /// Split the focused pane; `dir` None = zellij's automatic rule (split along the longer side).
    fn split(&mut self, dir: Option<Dir>, window: &mut Window, cx: &mut Context<Self>) {
        let Some(target) = self.focused_pane(window, cx) else { return };
        let (spec, sz) = { let v = target.read(cx); (v.spec.clone(), v.session.as_ref().map(|s| s.size())) };
        let dir = dir.or_else(|| sz.and_then(|s| auto_direction(s.rows, s.cols))).unwrap_or(Dir::Vertical);
        let pane = self.open_pane(spec, window, cx);
        let fh = pane.read(cx).focus.clone();
        if let Some(tab) = self.tabs.get_mut(self.active) {
            tab.layout.split(&target, pane.clone(), dir);
            tab.focused = pane;
        }
        window.focus(&fh, cx);
        cx.notify();
    }
    fn close_pane(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(target) = self.focused_pane(window, cx) else { return };
        let Some(tab) = self.tabs.get_mut(self.active) else { return };
        if !tab.layout.remove(&target) {
            if self.tabs.len() > 1 { self.tabs.remove(self.active); self.active = self.active.min(self.tabs.len() - 1); }
        } else {
            tab.focused = tab.layout.leaves()[0].clone();
        }
        if let Some(tab) = self.tabs.get(self.active) { let fh = tab.focused.read(cx).focus.clone(); window.focus(&fh, cx); }
        cx.notify();
    }
    fn apply_theme(&self, window: &Window) {
        let idx = match self.mode {
            ThemeMode::Fixed(i) => i,
            ThemeMode::System => {
                let want = match window.appearance() { WindowAppearance::Dark | WindowAppearance::VibrantDark => Appearance::Dark, _ => Appearance::Light };
                self.themes.iter().position(|t| t.appearance == want).unwrap_or(0)
            }
        };
        theme::set_current(self.themes[idx].clone());
    }
    fn cycle_theme(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let cur_idx = self.themes.iter().position(|t| t.name == cur().name).unwrap_or(0);
        self.mode = ThemeMode::Fixed((cur_idx + 1) % self.themes.len());
        self.apply_theme(window);
        self.refresh_all(cx);
    }
    fn refresh_all(&self, cx: &mut Context<Self>) {
        // theme changes must invalidate cached grid layers too: re-notify everything
        cx.refresh_windows();
        cx.notify();
    }
    fn answer_hostkey(&mut self, d: HostKeyDecision, cx: &mut Context<Self>) {
        if !self.hk_pending.is_empty() { let p = self.hk_pending.remove(0); let _ = p.reply.send(d); }
        cx.notify();
    }
}

fn icon_badge(letter: &str, color: Hsla) -> impl IntoElement {
    div().size(px(26.)).rounded_md().flex().items_center().justify_center()
        .bg(color.opacity(0.18)).text_color(color).text_size(px(12.)).font_weight(FontWeight::BOLD).child(letter.to_string())
}

fn render_node(node: &Node<Entity<TermView>>, th: &Theme) -> AnyElement {
    match node {
        Node::Leaf(p) => div().flex_1().size_full().flex().overflow_hidden().child(p.clone()).into_any_element(),
        Node::Split { dir, a, b } => {
            let d = div().flex_1().size_full().flex().overflow_hidden();
            let (d, divider) = match dir {
                Dir::Vertical => (d.flex_row(), div().w(px(1.)).h_full().bg(c(th.surface1))),
                Dir::Horizontal => (d.flex_col(), div().h(px(1.)).w_full().bg(c(th.surface1))),
            };
            d.child(render_node(a, th)).child(divider).child(render_node(b, th)).into_any_element()
        }
    }
}

fn state_color(st: AgentState, th: &Theme) -> Hsla {
    match st { AgentState::Working => c(th.warning), AgentState::Blocked => c(th.error), AgentState::Idle => c(th.success), AgentState::Unknown => c(th.overlay1) }
}

impl Render for Root {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let th = cur();
        let open_labels: Vec<(String, bool)> = self.tabs.iter().flat_map(|t| t.layout.leaves()).map(|p| {
            let v = p.read(cx);
            let label = match &v.spec { Spec::Local => "local".to_string(), Spec::Ssh(t) => t.label() };
            (label, v.session.as_ref().is_some_and(|s| s.connected.load(Ordering::Acquire)))
        }).collect();
        let online = |label: &str| open_labels.iter().any(|(l, on)| l == label && *on);

        let mut sidebar = div().w(px(232.)).h_full().flex().flex_col().bg(c(th.crust)).border_r_1().border_color(c(th.surface0))
            .child(div().h(px(44.)).px_4().flex().items_center().gap_2()
                .child(div().size(px(10.)).rounded_full().bg(c(th.accent)))
                .child(div().text_color(c(th.text)).font_weight(FontWeight::SEMIBOLD).text_size(px(13.)).child("AgentShell")))
            .child(div().px_4().pt_2().pb_1().text_size(px(10.5)).text_color(c(th.overlay0)).child("主机"));
        let sh = std::env::var("SHELL").unwrap_or_else(|_| "bash".into());
        let mut entries: Vec<(SharedString, SharedString, Option<SshTarget>, &'static str)> = vec![
            ("本地".into(), format!("{} · portable-pty", sh.rsplit('/').next().unwrap_or("sh")).into(), None, "L"),
        ];
        let dt = self.default_target.clone();
        entries.push((dt.label().into(), "环境变量 · russh".into(), Some(dt), "S"));
        for h in &self.hosts {
            let t = hosts::to_target(h, &self.known_hosts);
            let src = if h.source == "config" { "config.toml" } else { "~/.ssh/config" };
            entries.push((h.name.clone().into(), format!("{} · {src}", t.label()).into(), Some(t), "S"));
        }
        for (i, (name, detail, target, letter)) in entries.into_iter().enumerate() {
            let accent = if target.is_none() { c(th.info) } else { c(th.warning) };
            let label = target.as_ref().map(|t| t.label()).unwrap_or_else(|| "local".into());
            let dot = if online(&label) { c(th.success) } else { c(th.surface2) };
            let spec = match target { Some(t) => Spec::Ssh(t), None => Spec::Local };
            sidebar = sidebar.child(
                div().id(("host", i)).mx_2().px_2().py(px(6.)).rounded_lg().flex().items_center().gap_3().cursor_pointer()
                    .hover(|s| s.bg(c(cur().surface0)))
                    .on_click(cx.listener(move |this, _, w, cx| this.new_tab(spec.clone(), w, cx)))
                    .child(icon_badge(letter, accent))
                    .child(div().flex_1().flex().flex_col().overflow_hidden()
                        .child(div().text_size(px(12.5)).text_color(c(th.text)).child(name))
                        .child(div().text_size(px(10.5)).text_color(c(th.overlay0)).child(detail)))
                    .child(div().size(px(8.)).rounded_full().bg(dot)),
            );
        }
        sidebar = sidebar.child(div().flex_1()).child(
            div().m_3().p_3().rounded_lg().bg(c(th.mantle)).border_1().border_color(c(th.surface0)).text_size(px(11.)).text_color(c(th.subtext))
                .flex().flex_col().gap_1()
                .child("Ctrl+Shift+D 分屏 · W 关闭")
                .child("Ctrl+Shift+C / V 复制粘贴")
                .child("Shift+PgUp / 滚轮 回看")
                .child("Ctrl+Shift+T 切换主题")
                .child("→ / Tab 接受灰色建议"));

        let mut tabbar = div().h(px(40.)).px_2().flex().items_center().gap_1().bg(c(th.crust)).border_b_1().border_color(c(th.surface0));
        for (i, t) in self.tabs.iter().enumerate() {
            let active = i == self.active;
            let panes = t.layout.leaves();
            let title: SharedString = {
                let first = panes[0].read(cx).title.clone();
                if panes.len() > 1 { format!("{first} +{}", panes.len() - 1).into() } else { first }
            };
            // most urgent agent state among the tab's panes: Blocked > Working > Idle
            let agent = panes.iter().filter_map(|p| p.read(cx).agent_label())
                .max_by_key(|(_, st)| match st { AgentState::Blocked => 3, AgentState::Working => 2, AgentState::Idle => 1, _ => 0 });
            let mut tabel = div().id(("tab", i)).h(px(30.)).pl_3().pr_1().rounded_lg().flex().items_center().gap_2().cursor_pointer()
                .bg(if active { c(th.base) } else { c(th.crust) })
                .border_1().border_color(if active { c(th.surface0) } else { c(th.crust) })
                .hover(|s| s.bg(c(cur().mantle)))
                .on_click(cx.listener(move |this, _, _w, cx| { this.active = i; cx.notify(); }))
                .child(div().size(px(6.)).rounded_full().bg(if active { c(th.accent) } else { c(th.surface2) }))
                .child(div().text_size(px(12.)).text_color(if active { c(th.text) } else { c(th.overlay1) }).child(title));
            if let Some((name, st)) = agent {
                let col = state_color(st, &th);
                tabel = tabel.child(div().px(px(6.)).rounded_md().bg(col.opacity(0.18)).text_color(col).text_size(px(11.)).child(format!("● {name} {}", st.label_zh())));
            }
            tabel = tabel.child(div().id(("close", i)).size(px(20.)).rounded_md().flex().items_center().justify_center()
                .text_size(px(13.)).text_color(c(th.overlay0)).hover(|s| s.bg(c(cur().surface1)).text_color(c(cur().text)))
                .on_click(cx.listener(move |this, _, _w, cx| {
                    cx.stop_propagation();
                    if this.tabs.len() > 1 { this.tabs.remove(i); this.active = this.active.min(this.tabs.len() - 1); cx.notify(); }
                }))
                .child("×"));
            tabbar = tabbar.child(tabel);
        }
        tabbar = tabbar.child(
            div().id("new-tab").size(px(28.)).rounded_lg().flex().items_center().justify_center().cursor_pointer()
                .text_color(c(th.overlay1)).text_size(px(16.)).hover(|s| s.bg(c(cur().surface0)).text_color(c(cur().text)))
                .on_click(cx.listener(|this, _, w, cx| this.new_tab(Spec::Local, w, cx)))
                .child("+"),
        ).child(div().flex_1())
        .child(div().id("split-auto").px_2().h(px(26.)).rounded_md().flex().items_center().cursor_pointer().text_size(px(11.5)).text_color(c(th.overlay1))
            .hover(|s| s.bg(c(cur().surface0))).on_click(cx.listener(|this, _, w, cx| this.split(Some(Dir::Vertical), w, cx))).child("⫿ 左右分屏"))
        .child(div().id("split-down").px_2().h(px(26.)).rounded_md().flex().items_center().cursor_pointer().text_size(px(11.5)).text_color(c(th.overlay1))
            .hover(|s| s.bg(c(cur().surface0))).on_click(cx.listener(|this, _, w, cx| this.split(Some(Dir::Horizontal), w, cx))).child("☰ 上下分屏"))
        .child(div().id("close-pane").px_2().h(px(26.)).rounded_md().flex().items_center().cursor_pointer().text_size(px(11.5)).text_color(c(th.overlay1))
            .hover(|s| s.bg(c(cur().surface0))).on_click(cx.listener(|this, _, w, cx| this.close_pane(w, cx))).child("✕ 关闭分屏"));

        let panes = match self.tabs.get(self.active) {
            Some(tab) => div().flex_1().flex().overflow_hidden().child(render_node(&tab.layout, &th)),
            None => div().flex_1(),
        };

        let (status, scolor, dims, title, hist) = match self.focused_pane(window, cx) {
            Some(p) => {
                let v = p.read(cx);
                let (st, col) = v.status(&th);
                let d = v.session.as_ref().map(|s| { let z = s.size(); format!("{}×{}", z.cols, z.rows) }).unwrap_or_default();
                let n = v.history.lock().unwrap().len();
                (st, col, d, v.title.clone(), format!("历史 {n}"))
            }
            None => ("—", c(th.overlay0), String::new(), SharedString::from(""), String::new()),
        };
        let theme_label = match self.mode { ThemeMode::System => format!("◐ {}（跟随系统）", th.name), ThemeMode::Fixed(_) => format!("◐ {}", th.name) };
        let statusbar = div().h(px(26.)).px_3().flex().items_center().gap_4().bg(c(th.crust)).border_t_1().border_color(c(th.surface0))
            .text_size(px(11.)).text_color(c(th.subtext))
            .child(div().flex().items_center().gap_2().child(div().size(px(7.)).rounded_full().bg(scolor)).child(status))
            .child(title)
            .child(div().flex_1())
            .child(hist)
            .child(dims)
            .child("UTF-8")
            .child(div().id("theme-btn").px_2().rounded_md().cursor_pointer().hover(|s| s.bg(c(cur().surface0)))
                .on_click(cx.listener(|this, _, w, cx| this.cycle_theme(w, cx))).child(theme_label))
            .child(div().id("theme-sys").px_2().rounded_md().cursor_pointer().hover(|s| s.bg(c(cur().surface0)))
                .on_click(cx.listener(|this, _, w, cx| { this.mode = ThemeMode::System; this.apply_theme(w); this.refresh_all(cx); }))
                .child("系统"))
            .child(div().px_2().py(px(1.)).rounded_md().bg(c(th.accent).opacity(0.18)).text_color(c(th.accent)).font_weight(FontWeight::SEMIBOLD).child("AI: Ask 模式"));

        let mut root = div().size_full().relative().flex().flex_row().bg(c(th.crust)).font_family(SharedString::from("Inter"))
            .capture_key_down(cx.listener(|this, ev: &KeyDownEvent, w, cx| {
                let m = &ev.keystroke.modifiers;
                if m.control && m.shift && !m.alt {
                    match ev.keystroke.key.to_ascii_lowercase().as_str() {
                        "t" => this.cycle_theme(w, cx),
                        "d" => this.split(None, w, cx),
                        "w" => this.close_pane(w, cx),
                        _ => return,
                    }
                    cx.stop_propagation();
                }
            }))
            .child(sidebar)
            .child(div().flex_1().h_full().flex().flex_col().child(tabbar).child(panes).child(statusbar));

        // host key confirmation (first connection / TOFU)
        if let Some(p) = self.hk_pending.first() {
            let btn = |id: &'static str, label: &'static str, bg: Hsla, fg: Hsla| div().id(id).px_3().py(px(6.)).rounded_md().cursor_pointer().bg(bg).text_color(fg).text_size(px(12.5)).child(label);
            root = root.child(
                div().absolute().top_0().left_0().size_full().flex().items_center().justify_center().bg(gpui::black().opacity(0.45))
                    .child(div().w(px(560.)).p_5().rounded_xl().bg(c(th.base)).border_1().border_color(c(th.surface1)).flex().flex_col().gap_3()
                        .child(div().text_size(px(15.)).font_weight(FontWeight::SEMIBOLD).text_color(c(th.text)).child("首次连接：确认主机密钥"))
                        .child(div().text_size(px(12.5)).text_color(c(th.subtext)).child(format!("{}:{} 不在 known_hosts（{}）中。", p.host, p.port, self.known_hosts.display())))
                        .child(div().p_3().rounded_md().bg(c(th.mantle)).flex().flex_col().gap_1().text_size(px(12.)).text_color(c(th.text))
                            .child(format!("算法：{}", p.algorithm))
                            .child(div().font_family(SharedString::from(fcfg().family.clone())).child(format!("指纹：{}", p.fingerprint))))
                        .child(div().text_size(px(11.5)).text_color(c(th.overlay1)).child("请通过其他渠道核对指纹。接受并保存后，以后密钥变化会被拒绝连接。"))
                        .child(div().flex().gap_2().justify_end()
                            .child(btn("hk-reject", "拒绝", c(th.surface0), c(th.text)).on_click(cx.listener(|this, _, _w, cx| this.answer_hostkey(HostKeyDecision::Reject, cx))))
                            .child(btn("hk-once", "仅本次", c(th.surface0), c(th.text)).on_click(cx.listener(|this, _, _w, cx| this.answer_hostkey(HostKeyDecision::AcceptOnce, cx))))
                            .child(btn("hk-save", "接受并保存", c(th.accent), c(th.base)).on_click(cx.listener(|this, _, _w, cx| this.answer_hostkey(HostKeyDecision::AcceptAndSave, cx)))))),
            );
        }
        root
    }
}

fn main() {
    let cfg = config::load();
    let _ = FONT.set(cfg.font.clone());
    if std::env::var("PERF_LOG").is_ok() {
        std::thread::spawn(|| {
            use termview::{CURSOR_PAINTS, GRID_PAINTS, ROWS_BUILT};
            loop {
                std::thread::sleep(std::time::Duration::from_secs(1));
                let (g, r, c) = (GRID_PAINTS.swap(0, Ordering::Relaxed), ROWS_BUILT.swap(0, Ordering::Relaxed), CURSOR_PAINTS.swap(0, Ordering::Relaxed));
                if g + r + c > 0 { eprintln!("[perf] grid_paints/s={g} rows_built/s={r} cursor_paints/s={c}"); }
            }
        });
    }
    let rt = Arc::new(tokio::runtime::Builder::new_multi_thread().enable_all().worker_threads(2).build().unwrap());
    let themes = theme::load_all();
    let mode = match cfg.theme.as_deref() {
        None | Some("system") => ThemeMode::System,
        Some(name) => themes.iter().position(|t| t.name.eq_ignore_ascii_case(name)).map(ThemeMode::Fixed).unwrap_or(ThemeMode::System),
    };
    let default_target = env_target(&cfg.known_hosts);
    let no_ssh = std::env::var("NO_SSH").is_ok();
    gpui_platform::application().run(move |cx: &mut App| {
        let bounds = Bounds::centered(None, size(px(1440.), px(860.)), cx);
        cx.open_window(
            WindowOptions { window_bounds: Some(WindowBounds::Windowed(bounds)), ..Default::default() },
            move |window, cx| {
                cx.new(|cx| {
                    let (hk_tx, hk_rx) = flume::unbounded::<HostKeyPrompt>();
                    let mut root = Root {
                        rt, hosts: cfg.hosts.clone(), default_target, known_hosts: cfg.known_hosts.clone(),
                        tabs: vec![], active: 0, themes, mode, histories: HashMap::new(),
                        manifests: Arc::new(agent::bundled()), hk_tx, hk_pending: vec![], _subs: vec![],
                    };
                    root.apply_theme(window);
                    root._subs.push(cx.observe_window_appearance(window, |this: &mut Root, w, cx| {
                        if this.mode == ThemeMode::System { this.apply_theme(w); this.refresh_all(cx); }
                    }));
                    // host-key prompts from SSH connections -> modal
                    cx.spawn(async move |this, cx| {
                        while let Ok(p) = hk_rx.recv_async().await {
                            if this.update(cx, |r, cx| { r.hk_pending.push(p); cx.notify(); }).is_err() { break; }
                        }
                    }).detach();
                    let l = root.open_pane(Spec::Local, window, cx);
                    let mut layout = Node::Leaf(l.clone());
                    if !no_ssh {
                        let r = root.open_pane(Spec::Ssh(root.default_target.clone()), window, cx);
                        layout.split(&l, r, Dir::Vertical);
                    }
                    let fh = l.read(cx).focus.clone();
                    root.tabs.push(Tab { layout, focused: l });
                    window.focus(&fh, cx);
                    root
                })
            },
        ).unwrap();
        cx.activate(true);
    });
}
