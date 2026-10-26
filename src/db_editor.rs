//! SQL editor helpers: token highlighting from the crate's own SQL lexer, and completion of
//! keywords, tables and columns from the connection's schema (columns load on demand).
use std::cell::RefCell;
use std::ops::Range;
use std::rc::Rc;
use std::sync::Arc;

use gpui::{App, Context, FontStyle, HighlightStyle, SharedString, Task, Window};
use gpui_component::input::{CompletionProvider, EditorState, FoldRange, HighlightStyleResolver, InputEdit, InputHighlighter, Rope};
use lsp_types::{CompletionContext, CompletionItem, CompletionItemKind, CompletionResponse};
use ssh_term_proto::db::{complete, lex, live::Live};

use crate::theme::{c, cur};

#[derive(Default)]
pub struct SqlHighlighter { toks: Vec<(Range<usize>, lex::Tok)> }

fn tok_style(t: lex::Tok) -> HighlightStyle {
    let th = cur();
    let color = |x: u32| HighlightStyle { color: Some(c(x)), ..Default::default() };
    match t {
        lex::Tok::Keyword => HighlightStyle { color: Some(c(th.ansi[13])), font_weight: Some(gpui::FontWeight::SEMIBOLD), ..Default::default() },
        lex::Tok::String => color(th.ansi[2]),
        lex::Tok::Number => color(th.ansi[3]),
        lex::Tok::Quoted => color(th.ansi[6]),
        lex::Tok::Comment => HighlightStyle { color: Some(c(th.overlay0)), font_style: Some(FontStyle::Italic), ..Default::default() },
        lex::Tok::Operator => color(th.subtext),
        _ => HighlightStyle::default(),
    }
}

impl InputHighlighter for SqlHighlighter {
    fn language(&self) -> SharedString { "sql".into() }
    fn update(&mut self, _edit: Option<InputEdit>, text: &Rope, _folding: bool, _w: &mut Window, _cx: &mut Context<EditorState>) {
        self.toks = lex::tokens(&text.to_string());
    }
    fn styles(&self, range: &Range<usize>, _resolver: &dyn HighlightStyleResolver) -> Vec<(Range<usize>, HighlightStyle)> {
        let mut out: Vec<(Range<usize>, HighlightStyle)> = vec![];
        let mut at = range.start;
        let first = self.toks.partition_point(|(r, _)| r.end <= range.start);
        for (r, t) in &self.toks[first..] {
            if r.start >= range.end { break; }
            let (s, e) = (r.start.max(at), r.end.min(range.end));
            if s > at { out.push((at..s, HighlightStyle::default())); }
            if e > s { out.push((s..e, tok_style(*t))); at = e; }
        }
        if at < range.end { out.push((at..range.end, HighlightStyle::default())); }
        out
    }
    fn fold_ranges(&self, _text: &Rope) -> Vec<FoldRange> { vec![] }
}

pub fn install_highlighter(ed: &mut EditorState, cx: &mut Context<EditorState>) {
    ed.set_highlighter_factory(Rc::new(|lang: &str| (lang == "sql").then(|| Box::new(SqlHighlighter::default()) as Box<dyn InputHighlighter>)), cx);
}

/// What completion needs to know about the editor's connection; updated by the view.
#[derive(Default)]
pub struct CompCtx {
    pub live: Option<Arc<Live>>,
    pub rt: Option<tokio::runtime::Handle>,
    pub database: String,
    pub schema: Option<String>,
    pub cache: complete::Schema,
    /// Tables whose columns were requested (loaded or failed), so each is asked once.
    pub asked: std::collections::HashSet<String>,
}

pub type SharedComp = Rc<RefCell<CompCtx>>;

pub struct SqlCompletion(pub SharedComp);

fn to_items(items: Vec<complete::Item>, prefix: &str) -> CompletionResponse {
    let lower = !prefix.is_empty() && prefix.chars().all(|c| !c.is_ascii_uppercase());
    CompletionResponse::Array(items.into_iter().map(|i| {
        let label = if i.kind == complete::Kind::Keyword && lower { i.label.to_lowercase() } else { i.label };
        CompletionItem {
            label,
            kind: Some(match i.kind { complete::Kind::Keyword => CompletionItemKind::KEYWORD, complete::Kind::Table => CompletionItemKind::STRUCT, complete::Kind::Column => CompletionItemKind::FIELD }),
            detail: (!i.detail.is_empty()).then_some(i.detail),
            ..Default::default()
        }
    }).collect())
}

impl CompletionProvider for SqlCompletion {
    fn completions(&self, text: &Rope, offset: usize, _trigger: CompletionContext, _w: &mut Window, cx: &mut App) -> Task<anyhow::Result<CompletionResponse>> {
        let text = text.to_string();
        let offset = offset.min(text.len());
        let (prefix, qual) = complete::word_at(&text, offset);
        let stmt = &text[lex::statement_at(&text, offset)];
        // tables whose columns are needed and not asked yet
        let mut want: Vec<String> = complete::table_refs(stmt).into_iter().filter(|(t, a)| qual.as_ref().is_none_or(|q| a.as_deref().is_some_and(|a| a.eq_ignore_ascii_case(q)) || t.eq_ignore_ascii_case(q))).map(|(t, _)| t).collect();
        if let Some(q) = &qual { if want.is_empty() { want.push(q.clone()); } }
        let shared = self.0.clone();
        let (live, rt, db, schema) = {
            let mut s = shared.borrow_mut();
            want.retain(|t| { let k = t.to_lowercase(); s.cache.tables.iter().any(|x| x.eq_ignore_ascii_case(t)) && s.asked.insert(k) });
            (s.live.clone(), s.rt.clone(), s.database.clone(), s.schema.clone())
        };
        cx.spawn(async move |_cx| {
            if let (Some(live), Some(rt), false) = (live, rt, want.is_empty()) {
                let h = rt.spawn(async move {
                    let mut out = vec![];
                    for t in want { if let Ok(cols) = live.driver.columns(&db, schema.as_deref(), &t).await { out.push((t, cols)); } }
                    out
                });
                if let Ok(got) = h.await {
                    let mut s = shared.borrow_mut();
                    for (t, cols) in got { s.cache.columns.insert(t.to_lowercase(), cols.into_iter().map(|c| c.name).collect()); }
                }
            }
            let items = complete::suggest(&text, offset, &shared.borrow().cache);
            Ok(to_items(items, &prefix))
        })
    }

    fn is_completion_trigger(&self, _offset: usize, new_text: &str, _cx: &mut App) -> bool {
        new_text.chars().last().is_some_and(|c| c.is_alphanumeric() || c == '_' || c == '.')
    }
}
