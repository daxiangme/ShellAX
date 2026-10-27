//! 扩展管理：列出已安装与可用的扩展包，安装、更新、卸载，并显示下载进度。
//! 同一个实体同时嵌在工具面板与设置页里。
use std::collections::HashMap;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use gpui::{div, prelude::*, px, Context, EventEmitter, FontWeight, SharedString, Window};
use ssh_term_proto::ext::{self, Index, PackRow};
use ssh_term_proto::i18n::{t, tf};
use ssh_term_proto::updater::Progress;

use crate::theme::{c, cur};
use crate::ui::{self, icon, IconName};

pub enum ExtEvent { Changed }

#[derive(Default)]
struct Job { prog: Arc<Progress>, err: Option<String> }

pub struct ExtManager {
    index: Option<Index>,
    loading: bool,
    index_err: Option<String>,
    jobs: HashMap<String, Job>,
    running: Vec<String>,
    pending: Vec<String>,
    ticking: bool,
}

impl EventEmitter<ExtEvent> for ExtManager {}

fn human(n: u64) -> String {
    if n >= 1 << 20 { format!("{:.1} MB", n as f64 / (1u64 << 20) as f64) } else { format!("{} KB", n.div_ceil(1024)) }
}

impl ExtManager {
    pub fn new() -> Self {
        ExtManager { index: ext::cached_index(), loading: false, index_err: None, jobs: HashMap::new(), running: vec![], pending: vec![], ticking: false }
    }

    pub fn busy(&self, id: &str) -> bool { self.running.iter().any(|x| x == id) || self.pending.iter().any(|x| x == id) }

    /// 下载进度（0..=1），没有在下载时为 None。
    pub fn progress(&self, id: &str) -> Option<f32> {
        if !self.busy(id) { return None; }
        let p = &self.jobs.get(id)?.prog;
        let (d, tot) = (p.done.load(Ordering::Relaxed), p.total.load(Ordering::Relaxed));
        Some(if tot == 0 { 0. } else { (d as f32 / tot as f32).min(1.) })
    }

    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        if self.loading { return; }
        self.loading = true;
        self.index_err = None;
        cx.notify();
        let task = cx.background_executor().spawn(async move { ext::fetch_index() });
        cx.spawn(async move |this, cx| {
            let r = task.await;
            let _ = this.update(cx, |m, cx| {
                m.loading = false;
                match r { Ok(i) => m.index = Some(i), Err(e) => m.index_err = Some(e.to_string()) }
                for id in std::mem::take(&mut m.pending) { m.start(&id, cx); }
                cx.emit(ExtEvent::Changed);
                cx.notify();
            });
        }).detach();
    }

    /// 安装或更新。没有索引时先获取索引。
    pub fn install(&mut self, id: &str, cx: &mut Context<Self>) {
        if self.busy(id) { return; }
        if self.index.as_ref().is_some_and(|i| i.packs.iter().any(|p| p.id == id)) && !self.loading { self.start(id, cx); return; }
        self.pending.push(id.to_string());
        self.jobs.insert(id.to_string(), Job::default());
        self.refresh(cx);
        cx.notify();
    }

    fn start(&mut self, id: &str, cx: &mut Context<Self>) {
        let Some(p) = self.index.as_ref().and_then(|i| i.packs.iter().find(|p| p.id == id)).cloned() else {
            self.jobs.insert(id.to_string(), Job { err: Some(t("ext.err_not_in_index").to_string()), ..Default::default() });
            return;
        };
        let prog = Arc::new(Progress::default());
        self.jobs.insert(id.to_string(), Job { prog: prog.clone(), err: None });
        self.running.push(id.to_string());
        let id = id.to_string();
        let task = cx.background_executor().spawn(async move { ext::install(&p, &prog) });
        cx.spawn(async move |this, cx| {
            let r = task.await;
            let _ = this.update(cx, |m, cx| {
                m.running.retain(|x| *x != id);
                match r {
                    Ok(_) => { m.jobs.remove(&id); }
                    Err(e) => if let Some(j) = m.jobs.get_mut(&id) { j.err = Some(if e.to_string() == "cancelled" { t("ext.cancelled").to_string() } else { e.to_string() }); }
                }
                cx.emit(ExtEvent::Changed);
                cx.notify();
            });
        }).detach();
        self.tick(cx);
    }

    fn tick(&mut self, cx: &mut Context<Self>) {
        if self.ticking { return; }
        self.ticking = true;
        cx.spawn(async move |this, cx| loop {
            cx.background_executor().timer(Duration::from_millis(150)).await;
            let go = this.update(cx, |m, cx| { cx.notify(); let more = !m.running.is_empty(); if !more { m.ticking = false; } more }).unwrap_or(false);
            if !go { break; }
        }).detach();
    }

    fn cancel(&mut self, id: &str) {
        if let Some(j) = self.jobs.get(id) { j.prog.cancel.store(true, Ordering::Relaxed); }
        self.pending.retain(|x| x != id);
    }

    fn uninstall(&mut self, id: &str, cx: &mut Context<Self>) {
        if let Err(e) = ext::uninstall(id) { self.jobs.insert(id.to_string(), Job { err: Some(e.to_string()), ..Default::default() }); }
        cx.emit(ExtEvent::Changed);
        cx.notify();
    }

    fn row(&self, r: &PackRow, cx: &mut Context<Self>) -> gpui::AnyElement {
        let th = cur();
        let id = r.id.clone();
        let job = self.jobs.get(&r.id);
        let prog = self.progress(&r.id);
        let status = if let Some(p) = prog {
            let j = &job.unwrap().prog;
            let (d, tot) = (j.done.load(Ordering::Relaxed), j.total.load(Ordering::Relaxed));
            if tot > 0 { format!("{} · {} / {}", t("ext.downloading"), human(d), human(tot)) } else { let _ = p; t("ext.preparing").to_string() }
        } else if let Some(v) = &r.installed {
            if r.has_update() { tf("ext.update_to", &[("v", v), ("n", &r.latest.as_ref().unwrap().version)]) } else { tf("ext.installed_v", &[("v", v)]) }
        } else if let Some(l) = &r.latest {
            if l.size > 0 { format!("{} · {}", tf("ext.available_v", &[("v", &l.version)]), human(l.size)) } else { tf("ext.available_v", &[("v", &l.version)]) }
        } else { t("ext.not_installed").to_string() };
        let mut actions = div().flex().items_center().gap_1();
        let key = |s: &str| SharedString::from(format!("ext-{s}-{id}"));
        if prog.is_some() {
            let i = id.clone();
            actions = actions.child(ui::button(key("cancel"), t("ui.cancel"), 0).on_click(cx.listener(move |m, _, _w, cx| { m.cancel(&i); cx.notify(); })));
        } else {
            if r.installed.is_none() || r.has_update() {
                let i = id.clone();
                let label = if r.installed.is_some() { t("ext.update") } else { t("ext.download") };
                actions = actions.child(ui::button(key("get"), label, 1).on_click(cx.listener(move |m, _, _w, cx| m.install(&i, cx))));
            }
            if r.installed.is_some() {
                let i = id.clone();
                actions = actions.child(ui::button(key("rm"), t("ext.uninstall"), 3).on_click(cx.listener(move |m, _, _w, cx| m.uninstall(&i, cx))));
            }
        }
        let tools: Vec<String> = ext::KNOWN.iter().find(|k| k.0 == r.id).map(|k| k.2.iter().map(|x| t(&format!("tools.{x}")).to_string()).collect())
            .unwrap_or_else(|| ext::installed().into_iter().find(|i| i.manifest.id == r.id).map(|i| i.manifest.tools.into_iter().map(|t| t.name).collect()).unwrap_or_default());
        div().id(key("row")).px_3().py_2().flex().flex_col().gap_1().border_b_1().border_color(c(th.surface0))
            .child(div().flex().items_center().gap_2()
                .child(icon(if r.installed.is_some() { IconName::Package } else { IconName::CloudDownload }, 14., c(if r.installed.is_some() { th.accent } else { th.overlay1 })))
                .child(div().flex_1().min_w_0().flex().flex_col()
                    .child(div().text_size(px(ui::FS_SECONDARY)).font_weight(FontWeight::MEDIUM).text_color(c(th.text)).child(r.name.clone()))
                    .child(div().text_size(px(ui::FS_CAPTION)).text_color(c(th.overlay1)).child(status)))
                .child(actions))
            .when(!tools.is_empty(), |d| d.child(div().text_size(px(ui::FS_CAPTION)).text_color(c(th.subtext)).child(tools.join(" · "))))
            .when_some(prog, |d, p| d.child(div().h(px(4.)).w_full().rounded(px(2.)).bg(c(th.surface0))
                .child(div().h_full().rounded(px(2.)).bg(c(th.accent)).w(gpui::relative(p.max(0.02))))))
            .when_some(job.and_then(|j| j.err.clone()), |d, e| d.child(div().text_size(px(ui::FS_CAPTION)).text_color(c(th.error)).child(e)))
            .into_any_element()
    }
}

impl Render for ExtManager {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let th = cur();
        let inst = ext::installed();
        let rows = ext::rows(self.index.as_ref(), &inst);
        let mut list = crate::card_layout::sub_card().flex().flex_col();
        for r in &rows { list = list.child(self.row(r, cx)); }
        let note = if self.loading { Some((t("ext.checking").to_string(), th.overlay1)) }
            else if let Some(e) = &self.index_err { Some((tf("ext.index_failed", &[("e", e)]), th.error)) }
            else if self.index.is_none() { Some((t("ext.index_hint").to_string(), th.overlay1)) } else { None };
        div().flex().flex_col().gap_2()
            .child(div().flex().items_center().gap_2()
                .child(div().flex_1().text_size(px(ui::FS_CAPTION)).text_color(c(th.overlay1)).child(tf("ext.summary", &[("n", &inst.len()), ("dir", &ext::ext_dir().display())])))
                .child(ui::button("ext-refresh", t("ext.check"), 0).on_click(cx.listener(|m, _, _w, cx| m.refresh(cx)))))
            .when_some(note, |d, (s, col)| d.child(div().text_size(px(ui::FS_CAPTION)).text_color(c(col)).child(s)))
            .child(list)
    }
}
