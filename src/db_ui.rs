//! Database module UI (v0.2 PRD §6): connection tree tool window, connection dialog.
use gpui::{div, prelude::*, px, AnyElement, Context, Window};
use ssh_term_proto::i18n::t;

use crate::theme::{c, cur};
use crate::Root;

impl Root {
    pub(crate) fn db_panel(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> AnyElement {
        div().flex_1().p_3().text_size(px(12.)).text_color(c(cur().subtext)).child(t("db.empty")).into_any_element()
    }
    pub(crate) fn open_db_dialog(&mut self, _id: Option<String>, _window: &mut Window, cx: &mut Context<Self>) { cx.notify(); }
}
