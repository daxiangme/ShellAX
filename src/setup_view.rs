//! First-run setup. Shown once, before the main window, when the app has no config yet.
//!
//! One decision per page, applied the moment it is chosen so the window restyles under the user,
//! and a marker file written when they finish or skip so it never comes back. Every choice is one
//! the settings page can change later; skipping keeps the defaults.

use std::sync::Arc;

#[cfg(windows)]
windows_link::link!("kernel32.dll" "system" fn GetUserDefaultUILanguage() -> u16);

use gpui::{div, prelude::*, px, AnimationExt, AnyElement, App, Context, Entity, EventEmitter, FontWeight, Render, Window};
use gpui_component::input::{Input, InputState};
use gpui_component::Sizable;

use crate::theme::{c, cur, display_name, Appearance, Theme};
use crate::ui::{self, icon, IconName};
use ssh_term_proto::i18n::{t, tf};

const STEPS: usize = 4;

pub struct SetupView {
    step: usize,
    /// Language the user picked, or the one the system suggested.
    language: String,
    /// `system`, or a concrete theme name.
    theme: String,
    /// Terminal font size in points. Follows the detected terminal when it names one.
    font_size: f32,
    /// Monospace family the terminal will use.
    font_family: String,
    /// Where `font_family` came from, when it was detected rather than picked from the list.
    font_source: Option<String>,
    /// Monospace families installed on this machine, detected one first.
    fonts: Vec<String>,
    /// The font list stays hidden until the user asks to change the detected one.
    font_picking: bool,
    /// The search field stays hidden until its icon is clicked.
    font_search_open: bool,
    /// Which way the last step change went, so the new page slides in from that side.
    forward: bool,
    font_search: Entity<InputState>,
    themes: Vec<Arc<Theme>>,
}

pub enum SetupEvent {
    /// Close this window and continue. `true` when the user finished, so the choices were saved;
    /// `false` for skip or close, which leaves the defaults.
    Done(bool),
}

impl EventEmitter<SetupEvent> for SetupView {}

impl SetupView {
    pub fn language(&self) -> &str { &self.language }

    pub fn new(themes: Vec<Arc<Theme>>, installed: &[String], font_search: Entity<InputState>) -> Self {
        let (family, size, source, fonts) = terminal_fonts(installed);
        Self { step: 0, language: suggested_language(), theme: "system".into(), font_size: size, font_family: family, font_source: source, fonts, font_picking: false, font_search_open: false, forward: true, font_search, themes }
    }

    fn choose_language(&mut self, lang: &str, cx: &mut Context<Self>) {
        self.language = lang.into();
        // The window restyles its text immediately; the config is written at the end.
        ssh_term_proto::i18n::set_language(lang);
        cx.notify();
    }

    fn choose_theme(&mut self, theme: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.theme = theme.into();
        self.apply(window, cx);
        cx.notify();
    }

    fn choose_font(&mut self, family: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.font_family = family.into();
        // Picking closes the list again: the page returns to showing just the chosen font.
        self.font_picking = false;
        self.font_search_open = false;
        self.apply(window, cx);
        cx.notify();
    }

    /// Restyle the window from the choices so far, without writing config.toml. Skipping or
    /// closing therefore leaves nothing behind; only finishing persists them.
    fn apply(&self, window: &mut Window, cx: &mut App) {
        ssh_term_proto::i18n::set_language(&self.language);
        let mut s = (*crate::config::get()).clone();
        s.general.language = self.language.clone();
        s.appearance.theme = self.theme.clone();
        s.font.family = self.font_family.clone();
        s.font.size = self.font_size;
        crate::config::install(s);
        let th = resolve_theme(&self.themes, &self.theme, window);
        crate::theme::set_current(th.clone());
        crate::theme::set_scheme(Arc::new(th.to_scheme()));
        crate::sync_component_theme(&th, cx);
    }

    fn finish(&mut self, completed: bool, cx: &mut Context<Self>) {
        if completed {
            let language = self.language.clone();
            let theme = self.theme.clone();
            let family = self.font_family.clone();
            let size = self.font_size;
            crate::config::update(|s| {
                s.general.language = language;
                s.appearance.theme = theme;
                s.font.family = family;
                s.font.size = size;
            });
        }
        mark_done();
        cx.emit(SetupEvent::Done(completed));
    }

    fn page_title(&self) -> &'static str {
        match self.step {
            0 => t("setup.welcome_title"),
            1 => t("setup.look_title"),
            2 => t("setup.font_title"),
            _ => t("setup.done_title"),
        }
    }

    fn page_body(&self) -> &'static str {
        match self.step {
            0 => t("setup.welcome_body"),
            1 => t("setup.look_body"),
            2 => t("setup.font_body"),
            _ => t("setup.done_body"),
        }
    }
}

impl Render for SetupView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let th = cur();
        let step = self.step;
        // Subscribed once: typing in the font search re-renders the filtered list.
        cx.subscribe(&self.font_search, |_, _, _: &gpui_component::input::InputEvent, cx| cx.notify()).detach();

        let mut progress = div().flex().items_center().gap(px(6.));
        for i in 0..STEPS {
            let on = i <= step;
            progress = progress.child(div().w(px(if i == step { 28. } else { 16. })).h(px(3.)).rounded_full()
                .bg(if on { c(th.accent) } else { c(th.surface2) }));
        }

        // A fixed height, not one that hugs the content. Each step has a different amount of
        // content, and with the page centered in the window that difference slid the title and the
        // buttons up and down on every step.
        let mut page = div().w(px(560.)).h(px(440.)).flex().flex_col().gap(px(18.));
        page = page.child(progress)
            .child(div().min_h(px(72.)).flex_shrink_0().flex().flex_col().gap(px(6.))
                .child(div().text_size(px(20.)).font_weight(FontWeight::SEMIBOLD).text_color(c(th.text)).child(self.page_title()))
                .child(div().text_size(px(13.)).text_color(c(th.subtext)).line_height(px(20.)).child(self.page_body())));

        // The look page opens on "follow the system", which was never applied, so paint it now
        // rather than waiting for the row to be clicked.
        if step == 1 { self.apply(window, cx); }
        let body = match step {
            0 => self.welcome(cx),
            1 => self.look(window, cx),
            2 => self.terminal(window, cx),
            _ => self.summary(),
        };
        // The body takes the leftover space, so the footer stays put even when a step has little
        // content. It does not scroll itself: the font step pins its sample and scrolls only the
        // list, and a scrolling container here would carry the sample away with it.
        // The step number is part of the animation id, so each change starts a fresh slide instead of
        // continuing the previous one. Forward enters from the right, back from the left.
        let forward = self.forward;
        let body = div().id(("setup-step", step)).flex_1().min_h_0().flex().flex_col().justify_center().child(body)
            .with_animation(("setup-slide", step), gpui::Animation::new(std::time::Duration::from_millis(220)).with_easing(gpui::ease_out_quint()),
                move |d, t| d.occlude().opacity(t).left(px((1. - t) * if forward { 28. } else { -28. })));
        page = page.child(div().flex_1().min_h_0().relative().overflow_hidden().child(body));

        page = page.child(self.footer(cx));

        div().size_full().flex().flex_col().bg(c(th.base)).font_family(crate::theme::ui_font()).text_color(c(th.text))
            .child(self.titlebar(window))
            .child(div().flex_1().flex().items_center().justify_center().child(page))
            .children(crate::chrome::resize_edges(window))
    }
}

impl SetupView {
    fn titlebar(&self, window: &mut Window) -> gpui::AnyElement {
        let th = cur();
        let mut bar = crate::chrome::drag_region(div().id("setup-title")).h(px(crate::chrome::TITLE_H)).flex_shrink_0().flex().items_center()
            .bg(c(th.crust));
        if crate::chrome::style() == crate::chrome::Style::Mac {
            bar = bar.child(div().w(px(crate::chrome::LIGHTS_W)));
        } else {
            bar = bar.pl_3();
        }
        bar = bar.child(div().text_size(px(13.)).font_weight(FontWeight::MEDIUM).text_color(c(th.subtext)).child("ShellAX"))
            .child(crate::chrome::drag_spacer("setup-drag"));
        // The window close handler opens the main window and records that setup was dismissed, so
        // the button only has to close this window. Quitting here would take the app down with it.
        if let Some(wc) = crate::chrome::window_controls("setup-winctl", window, |w, _| w.remove_window()) {
            bar = bar.child(wc);
        }
        bar.into_any_element()
    }

    /// A prompt line, the thing the app actually is. The font and size chosen later redraw it.
    fn welcome(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let th = cur();
        let font = crate::config::get().font.family.clone();
        let prompt = div().p(px(16.)).rounded(px(10.)).bg(c(th.crust)).border_1().border_color(c(th.surface1).opacity(0.7))
            .flex().flex_col().gap(px(8.)).font_family(gpui::SharedString::from(font)).text_size(px(self.font_size))
            .child(div().flex().items_center().gap(px(8.))
                .child(div().text_color(c(th.success)).child("shellax@local"))
                .child(div().text_color(c(th.overlay1)).child(":"))
                .child(div().text_color(c(th.accent)).child("~"))
                .child(div().text_color(c(th.text)).child("$")))
            .child(div().text_size(px(12.)).font_family(crate::theme::ui_font()).text_color(c(th.overlay1)).child(t("setup.welcome_hint")));
        let zh = self.language == "zh-CN";
        div().flex().flex_col().gap(px(12.))
            .child(prompt)
            .child(div().flex().gap(px(8.))
                .child(self.lang_choice("setup-lang-zh", zh, "简体中文", cx.listener(|this, _, _w, cx| this.choose_language("zh-CN", cx))))
                .child(self.lang_choice("setup-lang-en", !zh, "English", cx.listener(|this, _, _w, cx| this.choose_language("en", cx)))))
            .into_any_element()
    }

    fn lang_choice(&self, id: &'static str, selected: bool, label: &str, on_click: impl Fn(&gpui::ClickEvent, &mut Window, &mut gpui::App) + 'static) -> AnyElement {
        let th = cur();
        div().id(id).flex_1().h(px(44.)).rounded(px(10.)).cursor_pointer().flex().items_center().justify_center().gap(px(8.))
            .border_1().border_color(if selected { c(th.accent) } else { c(th.surface1) })
            .bg(if selected { c(th.accent).opacity(0.1) } else { c(th.mantle) })
            .text_size(px(13.)).font_weight(FontWeight::MEDIUM).text_color(c(th.text))
            .hover(|d| d.border_color(c(cur().accent).opacity(0.6)))
            .on_click(on_click)
            .when(selected, |d| d.child(icon(IconName::Check, 14., c(th.accent))))
            .child(label.to_string())
            .into_any_element()
    }

    fn look(&mut self, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let dark = matches!(window.appearance(), gpui::WindowAppearance::Dark | gpui::WindowAppearance::VibrantDark);
        let mut options = div().flex().flex_col().gap(px(8.));
        options = options.child(self.choice("setup-look-system", self.theme == "system", IconName::Monitor, t("setup.follow_system"),
            if dark { t("setup.follow_system_dark") } else { t("setup.follow_system_light") },
            cx.listener(|this, _, w, cx| this.choose_theme("system", w, cx))));
        // One light and one dark theme stand in for the catalog; the rest stay reachable from
        // Settings, and listing all of them would push the page past the window.
        for theme in representative_themes(&self.themes) {
            let name = theme.name.clone();
            let selected = self.theme == name;
            let label = display_name(&name);
            let icon_name = if theme.appearance == Appearance::Dark { IconName::Moon } else { IconName::Sun };
            let id: &'static str = Box::leak(format!("setup-look-{name}").into_boxed_str());
            options = options.child(self.choice(id, selected, icon_name, &label, "",
                cx.listener(move |this, _, w, cx| this.choose_theme(&name, w, cx))));
        }
        options.into_any_element()
    }

    fn terminal(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let th = cur();
        let family = self.font_family.clone();
        let sample = div().p(px(14.)).rounded(px(10.)).bg(c(th.crust)).border_1().border_color(c(th.surface1).opacity(0.7))
            .font_family(gpui::SharedString::from(family.clone())).text_size(px(self.font_size)).text_color(c(th.text))
            .child(format!("ls  cd  git status   {}", t("setup.sample_cjk")));

        // The sample is drawn with the same cell-width assumption the terminal makes, so a family
        // that is not actually monospaced shows itself here instead of only after setup.
        let usable = monospace(window, &family, self.font_size);
        let source = self.font_source.clone().unwrap_or_default();
        let detected = self.fonts.first().is_some_and(|f| f.eq_ignore_ascii_case(&family)) && !source.is_empty();
        let origin = if detected { tf("setup.font_detected", &[("source", &source)]) } else { String::new() };

        // The sample stays put while the font list below it scrolls, so it is not part of that list.
        let mut page = div().flex_1().min_h_0().flex().flex_col().gap(px(12.)).child(sample)
            .when(!usable, |d| d.child(div().text_size(px(12.)).text_color(c(th.warning)).child(t("setup.font_not_mono"))));

        if self.font_picking {
            let query = self.font_search.read(cx).value().to_string();
            let needle = query.trim().to_lowercase();
            let mut list = div().id("setup-fonts").flex_1().min_h_0().overflow_y_scroll().flex().flex_col().gap(px(6.));
            let mut shown = 0usize;
            for (i, name) in self.fonts.clone().into_iter().enumerate() {
                if !needle.is_empty() && !name.to_lowercase().contains(&needle) { continue; }
                shown += 1;
                let from_terminal = i == 0 && !source.is_empty();
                let body = if from_terminal { origin.clone() } else { String::new() };
                let chosen = name.clone();
                list = list.child(self.choice(Box::leak(format!("setup-font-{i}").into_boxed_str()), name.eq_ignore_ascii_case(&family),
                    if from_terminal { IconName::Sparkles } else { IconName::Type }, &name, &body,
                    cx.listener(move |this, _, w, cx| this.choose_font(&chosen, w, cx))));
            }
            if shown == 0 {
                list = list.child(div().px(px(14.)).py(px(12.)).text_size(px(13.)).text_color(c(th.overlay1)).child(t("setup.font_none")));
            }
            // One element for both states, so the width animates between them instead of the field
            // popping in at full size. xsmall is the 20px input; the icon button is the same height.
            let open = self.font_search_open;
            let field = self.font_search.clone();
            // Set on render so the placeholder follows the language chosen on the first step.
            field.update(cx, |s, cx| s.set_placeholder(t("setup.font_search"), window, cx));
            let search = div().id("setup-font-search").h(px(32.)).rounded(px(6.)).flex().items_center().overflow_hidden().cursor_pointer()
                .border_1().border_color(if open { c(th.accent) } else { c(th.surface1) }).bg(c(th.mantle))
                .when(!open, |d| d.hover(|d| d.border_color(c(cur().accent).opacity(0.6))))
                .on_click(cx.listener(|this, _, w, cx| {
                    if this.font_search_open { return; }
                    this.font_search_open = true;
                    let fh = gpui_component::input::AnyInputState::from(this.font_search.clone()).focus_handle(cx);
                    w.focus(&fh, cx);
                    cx.notify();
                }))
                .child(div().flex_shrink_0().w(px(32.)).h_full().flex().items_center().justify_center().child(icon(IconName::Search, 14., c(th.overlay1))))
                .when(open, |d| d.child(div().flex_1().min_w_0().pr(px(6.)).child(Input::new(&field).small().appearance(false).cleanable(true))))
                .with_animation("setup-font-search-w", gpui::Animation::new(std::time::Duration::from_millis(180)).with_easing(gpui::ease_out_quint()),
                    move |d, t| d.w(px(32. + (if open { t } else { 1. - t }) * 188.)));
            page = page.child(div().flex().justify_end().child(search)).child(list);
        } else {
            page = page.child(div().px(px(14.)).py(px(12.)).rounded(px(10.)).flex().items_center().gap(px(12.))
                .border_1().border_color(c(th.accent)).bg(c(th.accent).opacity(0.08))
                .child(div().size(px(30.)).flex_shrink_0().rounded(px(6.)).flex().items_center().justify_center().bg(crate::theme::selected_bg())
                    .child(icon(if detected { IconName::Sparkles } else { IconName::Type }, 15., c(th.accent))))
                .child(div().flex_1().min_w_0().flex().flex_col().gap(px(1.))
                    .child(div().text_size(px(13.)).font_weight(FontWeight::MEDIUM).text_color(c(th.text)).child(family.clone()))
                    .when(!origin.is_empty(), |d| d.child(div().text_size(px(12.)).text_color(c(th.overlay1)).child(origin))))
                .child(ui::button("setup-font-change", t("setup.font_change"), 0)
                    .on_click(cx.listener(|this, _, _w, cx| { this.font_picking = true; cx.notify(); }))));
        }
        page.into_any_element()
    }

    fn summary(&self) -> AnyElement {
        let th = cur();
        let theme = if self.theme == "system" { t("setup.follow_system").to_string() } else { display_name(&self.theme) };
        let lang = if self.language == "zh-CN" { t("setup.lang_zh") } else { t("setup.lang_en") };
        let rows = [
            (IconName::Languages, t("setup.summary_language"), lang.to_string()),
            (IconName::Palette, t("setup.summary_look"), theme),
            (IconName::SquareTerminal, t("setup.summary_terminal"), self.font_family.clone()),
        ];
        let mut card = div().rounded(px(10.)).bg(c(th.mantle)).border_1().border_color(c(th.surface1).opacity(0.6)).flex().flex_col().overflow_hidden();
        for (i, (ic, label, value)) in rows.into_iter().enumerate() {
            if i > 0 { card = card.child(div().mx(px(14.)).h(px(1.)).bg(c(th.surface1).opacity(0.6))); }
            card = card.child(div().px(px(14.)).py(px(12.)).flex().items_center().gap(px(12.))
                .child(icon(ic, 15., c(th.accent)))
                .child(div().w(px(120.)).text_size(px(13.)).text_color(c(th.subtext)).child(label))
                .child(div().flex_1().text_size(px(13.)).font_weight(FontWeight::MEDIUM).text_color(c(th.text)).child(value)));
        }
        card.into_any_element()
    }

    fn choice(&self, id: &'static str, selected: bool, ic: IconName, title: &str, body: &str, on_click: impl Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static) -> AnyElement {
        let th = cur();
        div().id(id).w_full().px(px(14.)).py(px(12.)).rounded(px(10.)).cursor_pointer().flex().items_center().gap(px(12.))
            .border_1().border_color(if selected { c(th.accent) } else { c(th.surface1) })
            .bg(if selected { c(th.accent).opacity(0.08) } else { c(th.mantle) })
            .hover(|d| d.border_color(c(cur().accent).opacity(0.6)))
            .on_click(on_click)
            .child(div().size(px(30.)).flex_shrink_0().rounded(px(6.)).flex().items_center().justify_center()
                .bg(if selected { c(th.accent).opacity(0.16) } else { c(th.surface0) })
                .child(icon(ic, 15., if selected { c(th.accent) } else { c(th.overlay1) })))
            .child(div().flex_1().min_w_0().flex().flex_col().gap(px(1.))
                .child(div().text_size(px(13.)).font_weight(FontWeight::MEDIUM).text_color(c(th.text)).child(title.to_string()))
                .when(!body.is_empty(), |d| d.child(div().text_size(px(12.)).text_color(c(th.overlay1)).child(body.to_string()))))
            .when(selected, |d| d.child(icon(IconName::Check, 15., c(th.accent))))
            .into_any_element()
    }

    fn footer(&self, cx: &mut Context<Self>) -> AnyElement {
        let step = self.step;
        let last = step + 1 == STEPS;
        div().flex().items_center().justify_between().pt(px(4.))
            .child(if step == 0 {
                ui::button("setup-skip", t("setup.skip"), 3).on_click(cx.listener(|this, _, _w, cx| this.finish(false, cx))).into_any_element()
            } else {
                ui::button("setup-back", t("setup.back"), 0).on_click(cx.listener(|this, _, _w, cx| { this.forward = false; this.step -= 1; cx.notify(); })).into_any_element()
            })
            .child(div().flex().items_center().gap(px(8.))
                .when(step > 0 && !last, |d| d.child(ui::button("setup-skip-late", t("setup.skip"), 3).on_click(cx.listener(|this, _, _w, cx| this.finish(false, cx)))))
                .child(ui::button("setup-next", if last { t("setup.start") } else { t("setup.next") }, 1)
                    .on_click(cx.listener(move |this, _, _w, cx| if last { this.finish(true, cx); } else { this.forward = true; this.step += 1; cx.notify(); }))))
            .into_any_element()
    }
}

/// The font the terminal page offers first, then the other monospace families on this machine.
///
/// The first entry is whatever terminal the user already configured (Windows Terminal, then VS Code,
/// then an installed Nerd Font), so confirming setup keeps it. Its size is kept too; with nothing
/// detected the page falls back to the platform default at 14pt.
fn terminal_fonts(installed: &[String]) -> (String, f32, Option<String>, Vec<String>) {
    let detected = ssh_term_proto::platform::detect_system_terminal_font(installed);
    let fallback = crate::theme::pick_mono_family(&[], installed).unwrap_or_else(|| crate::config::Font::default().family);
    let family = detected.as_ref().map(|d| d.family.clone()).unwrap_or(fallback);
    let size = detected.as_ref().and_then(|d| d.size).filter(|s| (8.0..=32.0).contains(s)).unwrap_or(14.0);
    let mut fonts = vec![family.clone()];
    // The bundled Symbols Nerd Font Mono is registered as an installed font but holds only icons,
    // so it can never be a terminal font. Offering it is what garbled the grid on machines that
    // have no terminal font of their own configured.
    let mut rest: Vec<String> = installed.iter().filter(|f| looks_mono(f) && !icon_font(f) && !f.eq_ignore_ascii_case(&family)).cloned().collect();
    rest.sort();
    rest.dedup();
    fonts.extend(rest.into_iter().take(24));
    (family, size, detected.map(|d| d.source), fonts)
}

/// Name-based guess used to build the list. The sample then checks the chosen one for real, since a
/// name can claim to be mono and still not be.
fn icon_font(name: &str) -> bool { name.to_lowercase().contains("symbols") }

fn looks_mono(name: &str) -> bool {
    let l = name.to_lowercase();
    l.contains("mono") || l.contains("nerd font") || l.contains("code") || l.contains("consolas")
        || l.contains("courier") || l.contains("cascadia") || l.contains("jetbrains") || l.contains("hack")
        || l.contains("fira") || l.contains("menlo") || l.ends_with(" nf") || l.ends_with(" nfm")
}

fn monospace(window: &Window, family: &str, size: f32) -> bool {
    let ts = window.text_system();
    let fid = ts.resolve_font(&gpui::font(gpui::SharedString::from(family.to_string())));
    let advance = |ch: char| ts.advance(fid, px(size), ch).ok().map(|a| f32::from(a.width)).unwrap_or(0.0);
    let (i, m, zero) = (advance('i'), advance('M'), advance('0'));
    i > 0.0 && (i - m).abs() < 0.05 && (m - zero).abs() < 0.05
}

/// The two themes offered on the look page: the product's own light and dark, falling back to
/// whatever light and dark themes are installed.
fn representative_themes(themes: &[Arc<Theme>]) -> Vec<Arc<Theme>> {
    let mut out = Vec::new();
    for (preferred, appearance) in [("ShellAX Daylight", Appearance::Light), ("ShellAX Midnight", Appearance::Dark)] {
        let found = themes.iter().find(|t| t.name == preferred).or_else(|| themes.iter().find(|t| t.appearance == appearance));
        if let Some(t) = found { if !out.iter().any(|e: &Arc<Theme>| e.name == t.name) { out.push(t.clone()); } }
    }
    out
}

/// The theme setup settled on, read back after it closed. `None` unless the user finished, so
/// skipping or closing the window leaves the theme that was already applied.
pub fn committed_theme(completed: bool, themes: &[Arc<Theme>], cx: &App) -> Option<Arc<Theme>> {
    if !completed { return None; }
    let choice = crate::config::get().appearance.theme.clone();
    if choice == "system" {
        let dark = matches!(cx.window_appearance(), gpui::WindowAppearance::Dark | gpui::WindowAppearance::VibrantDark);
        let want = if dark { Appearance::Dark } else { Appearance::Light };
        let preferred = if dark { "ShellAX Midnight" } else { "ShellAX Daylight" };
        return themes.iter().find(|t| t.name == preferred).or_else(|| themes.iter().find(|t| t.appearance == want)).cloned().or_else(|| themes.first().cloned());
    }
    themes.iter().find(|t| t.name.eq_ignore_ascii_case(&choice)).cloned()
}

/// The theme a choice names: "system" follows the window, anything else is matched by name.
fn resolve_theme(themes: &[Arc<Theme>], choice: &str, window: &Window) -> Arc<Theme> {
    let by_name = |n: &str| themes.iter().find(|t| t.name.eq_ignore_ascii_case(n)).cloned();
    if choice != "system" {
        if let Some(t) = by_name(choice) { return t; }
    }
    let dark = matches!(window.appearance(), gpui::WindowAppearance::Dark | gpui::WindowAppearance::VibrantDark);
    let want = if dark { Appearance::Dark } else { Appearance::Light };
    let preferred = if dark { "ShellAX Midnight" } else { "ShellAX Daylight" };
    by_name(preferred).or_else(|| themes.iter().find(|t| t.appearance == want).cloned()).unwrap_or_else(|| themes[0].clone())
}

/// Whether this profile still needs the first-run setup.
///
/// An existing config.toml means the user already has settings, possibly migrated from an older
/// version, and should not be walked through it again. `SHELLAX_SETUP=1` forces it for testing and
/// `SHELLAX_NO_SETUP=1` suppresses it; when both are set the suppression wins.
pub fn needed() -> bool {
    if std::env::var_os("SHELLAX_NO_SETUP").is_some() { return false; }
    if std::env::var_os("SHELLAX_SETUP").is_some() { return true; }
    !crate::config::config_path().exists() && !marker_path().exists()
}

fn marker_path() -> std::path::PathBuf {
    // Beside config.toml, so SHELLAX_CONFIG (tests, portable installs) keeps the marker with it
    // instead of writing into the real profile directory.
    crate::config::config_path().parent().map(|d| d.join("setup-done")).unwrap_or_else(|| ssh_term_proto::platform::config_dir().join("setup-done"))
}

/// Records that setup has been shown, however it ended. Public so the window close path, which
/// bypasses the buttons, marks it too.
pub fn mark_seen() { mark_done() }

fn mark_done() {
    let p = marker_path();
    if let Some(d) = p.parent() { let _ = std::fs::create_dir_all(d); }
    let _ = std::fs::write(p, b"");
}

/// Language to offer first. Windows has no POSIX locale variables, so the UI language comes from
/// the system; elsewhere the existing LC_*/LANG detection is used.
pub fn suggested_language() -> String {
    #[cfg(windows)]
    {
        // 0x04 is the primary language identifier for Chinese, shared by zh-CN, zh-TW and zh-HK.
        let lang = unsafe { GetUserDefaultUILanguage() };
        if lang & 0x3ff == 0x04 { return "zh-CN".into(); }
        return "en".into();
    }
    #[cfg(not(windows))]
    { if ssh_term_proto::i18n::system_lang() == ssh_term_proto::i18n::Lang::ZhCn { "zh-CN".into() } else { "en".into() } }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Point config at a throwaway directory so the checks never touch the real profile.
    fn isolated(dir: &std::path::Path) {
        std::env::set_var("SHELLAX_CONFIG", dir.join("config.toml"));
        std::env::remove_var("SHELLAX_SETUP");
        std::env::remove_var("SHELLAX_NO_SETUP");
    }

    #[test]
    fn setup_runs_once_until_finished_or_configured() {
        let dir = std::env::temp_dir().join(format!("shellax-setup-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        isolated(&dir);
        assert!(needed(), "a profile with nothing in it still needs setup");

        mark_done();
        assert!(!needed(), "finishing or skipping writes the marker");

        std::fs::write(dir.join("config.toml"), b"whatever").unwrap();
        std::fs::remove_file(marker_path()).unwrap();
        assert!(!needed(), "an existing config.toml, even from an older version, is not a first run");

        std::env::set_var("SHELLAX_SETUP", "1");
        assert!(needed(), "SHELLAX_SETUP forces it for testing");
        std::env::set_var("SHELLAX_NO_SETUP", "1");
        assert!(!needed(), "SHELLAX_NO_SETUP wins, so a script can suppress it");

        std::env::remove_var("SHELLAX_CONFIG");
        std::env::remove_var("SHELLAX_SETUP");
        std::env::remove_var("SHELLAX_NO_SETUP");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
