//! Redis tab content: key tree with SCAN paging, per-type viewer/editor, TTL / rename /
//! delete, a console, server info and slow log. Lives in a layout leaf like a database view.
use std::collections::HashSet;
use std::future::Future;
use std::sync::Arc;

use gpui::{div, prelude::*, px, AnyElement, Context, Entity, FontWeight, Window};
use gpui_component::input::{EditorState, InputEvent, InputState};
use ssh_term_proto::assets::RedisConn;
use ssh_term_proto::i18n::{t, tf};
use ssh_term_proto::kv::{self, cmd::{self, Risk}, tree, Content, Cursor, KeyMeta, Kv};
use ssh_term_proto::ssh::{Prompts, SshTarget};

use crate::activity::{log_push, ExecLog};
use crate::theme::{c, cur};
use crate::ui::{self, icon, mono, small, IconName};

const KEY_CAP: usize = 100_000;
const ROW_CAP: usize = 3000;
pub const KINDS: [&str; 6] = ["string", "hash", "list", "set", "zset", "stream"];

#[derive(Clone, Copy, PartialEq)]
pub enum Sub { Keys, Console, Info, Slowlog }

enum Ask { Console(String), Delete(Vec<String>), Write(String) }

pub struct KvView {
    pub conn: RedisConn,
    pub title: String,
    production: bool,
    kv: Option<Arc<Kv>>,
    conn_err: Option<String>,
    rt: tokio::runtime::Handle,
    log: ExecLog,
    sub: Sub,
    // keys
    keys: Vec<String>,
    cursor: Cursor,
    scanning: bool,
    open: HashSet<String>,
    rows: Vec<tree::Row>,
    total: Option<u64>,
    filter_in: Entity<InputState>,
    applied_filter: String,
    kind_filter: Option<&'static str>,
    selected: Option<String>,
    meta: Option<KeyMeta>,
    content: Option<Content>,
    loading: bool,
    error: Option<String>,
    note: Option<String>,
    // editors
    value_ed: Entity<EditorState>,
    pending_value: Option<String>,
    binary: bool,
    item_a: Entity<InputState>,
    item_b: Entity<InputState>,
    ttl_in: Entity<InputState>,
    rename_in: Entity<InputState>,
    // new key
    new_open: bool,
    new_kind: String,
    new_key: Entity<InputState>,
    new_a: Entity<InputState>,
    new_b: Entity<InputState>,
    // bulk delete
    bulk_in: Entity<InputState>,
    // console
    console_in: Entity<InputState>,
    console_out: Vec<(String, String)>,
    // info / slowlog
    info: Vec<(String, String, String)>,
    slow: Vec<kv::SlowEntry>,
    ask: Option<Ask>,
    _subs: Vec<gpui::Subscription>,
}

fn input(ph: &str, window: &mut Window, cx: &mut Context<KvView>) -> Entity<InputState> {
    let ph = ph.to_string();
    cx.new(|cx| InputState::new(window, cx).placeholder(ph))
}
fn val(i: &Entity<InputState>, cx: &App) -> String { i.read(cx).value().trim().to_string() }
use gpui::App;

/// TTL in ms as text: "∞" (none), "—" (missing), or d/h/m/s.
pub fn ttl_text(ms: i64) -> String {
    if ms == -1 { return "∞".into(); }
    if ms < 0 { return "—".into(); }
    let s = ms / 1000;
    match s { s if s >= 86400 => format!("{}d {}h", s / 86400, s % 86400 / 3600), s if s >= 3600 => format!("{}h {}m", s / 3600, s % 3600 / 60), s if s >= 60 => format!("{}m {}s", s / 60, s % 60), s => format!("{s}s") }
}

/// "a=1 b=2" → [(a,1),(b,2)] for XADD.
pub fn parse_fields(s: &str) -> Vec<(String, String)> {
    cmd::split_line(s).unwrap_or_default().into_iter().filter_map(|p| p.split_once('=').map(|(a, b)| (a.to_string(), b.to_string()))).collect()
}

impl KvView {
    pub fn new(conn: RedisConn, password: Option<String>, via: Result<Option<(SshTarget, Prompts)>, String>, select: Option<String>, production: bool, rt: tokio::runtime::Handle, log: ExecLog, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let filter_in = cx.new(|cx| InputState::new(window, cx).placeholder(t("redis.filter_placeholder")));
        let console_in = cx.new(|cx| InputState::new(window, cx).placeholder(t("redis.console_placeholder")));
        let value_ed = cx.new(|cx| EditorState::new(window, cx).line_number(true));
        let mut subs = vec![];
        subs.push(cx.subscribe_in(&filter_in, window, |this: &mut Self, _, ev: &InputEvent, _w, cx| { if let InputEvent::PressEnter { .. } = ev { this.rescan(cx); } }));
        subs.push(cx.subscribe_in(&console_in, window, |this: &mut Self, st, ev: &InputEvent, w, cx| {
            if let InputEvent::PressEnter { .. } = ev { let line = st.read(cx).value().trim().to_string(); st.update(cx, |s, cx| s.set_value("", w, cx)); this.console(line, cx); }
        }));
        let me = Self {
            title: conn.name.clone(), production, kv: None, conn_err: None, rt, log, sub: Sub::Keys,
            keys: vec![], cursor: Cursor::default(), scanning: false, open: HashSet::new(), rows: vec![], total: None,
            filter_in, applied_filter: String::new(), kind_filter: None, selected: None, meta: None, content: None, loading: false, error: None, note: None,
            value_ed, pending_value: None, binary: false,
            item_a: input(&t("redis.field"), window, cx), item_b: input(&t("redis.value"), window, cx),
            ttl_in: input(&t("redis.ttl_placeholder"), window, cx), rename_in: input(&t("redis.new_name"), window, cx),
            new_open: false, new_kind: "string".into(), new_key: input(&t("redis.key_name"), window, cx), new_a: input(&t("redis.value"), window, cx), new_b: input("", window, cx),
            bulk_in: input(&t("redis.bulk_placeholder"), window, cx),
            console_in, console_out: vec![], info: vec![], slow: vec![], ask: None, _subs: subs, conn,
        };
        let mut me = me;
        let via = match via { Ok(v) => v, Err(e) => { me.conn_err = Some(e); return me; } };
        let c = me.conn.clone();
        let h = me.rt.spawn(async move { Kv::open(&c, password, via).await.map(Arc::new) });
        cx.spawn(async move |this, cx| {
            let r = h.await.map_err(anyhow::Error::from).and_then(|r| r);
            let _ = this.update(cx, |v, cx| {
                match r {
                    Ok(kv) => { log_push(&v.log, "redis", &format!("connect {}", v.conn.address()), true, kv.version.clone()); v.kv = Some(kv); v.rescan(cx); if let Some(k) = select.clone() { v.select(k, cx); } }
                    Err(e) => { log_push(&v.log, "redis", &format!("connect {}", v.conn.address()), false, format!("{e:#}")); v.conn_err = Some(format!("{e:#}")); }
                }
                cx.notify();
            });
        }).detach();
        me
    }

    pub fn save(&self) -> ssh_term_proto::session_store::PaneSave { ssh_term_proto::session_store::PaneSave::Redis { conn: self.conn.id.clone(), key: self.selected.clone() } }
    pub fn crumbs(&self) -> String {
        let mut s = format!("{} › db{}", self.conn.name, self.conn.db);
        if let Some(k) = &self.selected { s.push_str(" › "); s.push_str(k); }
        s
    }

    /// Run `f` on the runtime; `done` gets the result on the UI thread.
    fn job<T: Send + 'static, F: Future<Output = anyhow::Result<T>> + Send + 'static>(&mut self, f: impl FnOnce(Arc<Kv>) -> F, done: impl FnOnce(&mut Self, anyhow::Result<T>, &mut Context<Self>) + 'static, cx: &mut Context<Self>) {
        let Some(kv) = self.kv.clone() else { return };
        let h = self.rt.spawn(f(kv));
        cx.spawn(async move |this, cx| {
            let r = h.await.map_err(anyhow::Error::from).and_then(|r| r);
            let _ = this.update(cx, |v, cx| { done(v, r, cx); cx.notify(); });
        }).detach();
    }
    fn fail(&mut self, e: anyhow::Error) { self.error = Some(format!("{e:#}")); }
    fn logged(&self, what: &str, r: &anyhow::Result<()>) { log_push(&self.log, "redis", what, r.is_ok(), r.as_ref().err().map(|e| format!("{e:#}")).unwrap_or_default()); }
    fn writable(&mut self) -> bool { if self.conn.read_only { self.error = Some(t("redis.read_only").into()); false } else { true } }

    // ---- key list
    pub fn rescan(&mut self, cx: &mut Context<Self>) {
        self.keys.clear();
        self.cursor = Cursor::default();
        self.applied_filter = val(&self.filter_in, cx);
        self.rebuild();
        self.job(|kv| async move { kv.dbsize().await }, |v, r, _| v.total = r.ok(), cx);
        self.scan_more(cx);
    }
    fn scan_more(&mut self, cx: &mut Context<Self>) {
        if self.scanning || self.cursor.done { return; }
        self.scanning = true;
        let (cur0, pat, kind) = (self.cursor, self.applied_filter.clone(), self.kind_filter);
        self.job(move |kv| async move {
            // fill at least one screen: keep scanning while batches come back sparse
            let (mut out, mut cur) = (vec![], cur0);
            for _ in 0..20 { let (k, c) = kv.scan(cur, &pat, kind).await?; out.extend(k); cur = c; if cur.done || out.len() >= 500 { break; } }
            Ok((out, cur))
        }, |v, r, _| {
            v.scanning = false;
            match r {
                Ok((k, c)) => { let seen: HashSet<&String> = v.keys.iter().collect(); let new: Vec<String> = k.into_iter().filter(|x| !seen.contains(x)).collect(); v.keys.extend(new); v.keys.truncate(KEY_CAP); v.cursor = c; if v.keys.len() >= KEY_CAP { v.cursor.done = true; } v.rebuild(); }
                Err(e) => v.fail(e),
            }
        }, cx);
    }
    fn rebuild(&mut self) {
        let root = tree::Node::build(self.keys.iter().map(String::as_str), self.conn.sep());
        self.rows = tree::rows(&root, self.conn.sep(), &self.open);
    }

    fn select(&mut self, key: String, cx: &mut Context<Self>) {
        self.selected = Some(key.clone());
        self.error = None;
        self.note = None;
        self.loading = true;
        self.content = None;
        self.job(move |kv| async move {
            let m = kv.meta(&key).await?;
            let content = if m.kind == "none" { None } else { Some(kv.read(&key, &m.kind, 0, None, "").await?) };
            Ok((m, content))
        }, |v, r, _| {
            v.loading = false;
            match r {
                Ok((m, content)) => {
                    if m.kind == "none" { v.error = Some(tf("redis.err_no_key", &[("key", &m.key)])); v.meta = None; return; }
                    if let Some(Content::Str { value, .. }) = &content {
                        let s = String::from_utf8(value.clone());
                        v.binary = s.is_err();
                        v.pending_value = Some(match s { Ok(s) => s, Err(_) => kv::hex_dump(value) });
                    }
                    v.meta = Some(m);
                    v.content = content;
                }
                Err(e) => v.fail(e),
            }
        }, cx);
    }
    fn reload(&mut self, cx: &mut Context<Self>) { if let Some(k) = self.selected.clone() { self.select(k, cx); } }
    fn more_items(&mut self, cx: &mut Context<Self>) {
        let (Some(k), Some(m)) = (self.selected.clone(), self.meta.clone()) else { return };
        let (from, from_id) = match &self.content { Some(Content::Pairs { next: Some(n), .. }) => (*n, None), Some(Content::Stream { next: Some(id), .. }) => (0, Some(id.clone())), _ => return };
        self.job(move |kv| async move { kv.read(&k, &m.kind, from, from_id.as_deref(), "").await }, |v, r, _| match (r, v.content.as_mut()) {
            (Ok(Content::Pairs { items, next }), Some(Content::Pairs { items: have, next: n })) => { have.extend(items); *n = next; }
            (Ok(Content::Stream { entries, next, .. }), Some(Content::Stream { entries: have, next: n, .. })) => { have.extend(entries); *n = next; }
            (Err(e), _) => v.fail(e),
            _ => {}
        }, cx);
    }

    // ---- edits
    fn after_write(&mut self, what: String, r: anyhow::Result<()>, cx: &mut Context<Self>) {
        self.logged(&what, &r);
        match r { Ok(()) => { self.note = Some(t("redis.saved").into()); self.reload(cx); } Err(e) => self.fail(e) }
    }
    /// Production connections confirm every write; others run it straight away.
    fn guarded_write(&mut self, what: String, cx: &mut Context<Self>) {
        if !self.writable() { return; }
        if self.production { self.ask = Some(Ask::Write(what)); } else { self.do_write(&what, cx); }
        cx.notify();
    }
    fn do_write(&mut self, what: &str, cx: &mut Context<Self>) {
        let Some(key) = self.selected.clone() else { return };
        let (a, b) = (val(&self.item_a, cx), self.item_b.read(cx).value().to_string());
        let kind = self.meta.as_ref().map(|m| m.kind.clone()).unwrap_or_default();
        let w = what.to_string();
        match what {
            "save_string" => {
                let text = self.value_ed.read(cx).value().to_string();
                if self.binary { self.error = Some(t("redis.binary_readonly").into()); return; }
                self.job(move |kv| async move { kv.set_string(&key, text.as_bytes(), true).await }, move |v, r, cx| v.after_write(format!("SET {w}"), r, cx), cx);
            }
            "ttl" => {
                let raw = val(&self.ttl_in, cx);
                let secs = if raw.is_empty() || raw == "-1" { None } else { match raw.parse::<i64>() { Ok(s) => Some(s), Err(_) => { self.error = Some(t("redis.err_ttl").into()); return; } } };
                self.job(move |kv| async move { kv.set_ttl(&key, secs).await }, move |v, r, cx| v.after_write(format!("EXPIRE {raw}"), r, cx), cx);
            }
            "rename" => {
                let to = val(&self.rename_in, cx);
                if to.is_empty() { return; }
                let to2 = to.clone();
                self.job(move |kv| async move { kv.rename(&key, &to).await }, move |v, r, cx| {
                    v.logged(&format!("RENAMENX → {to2}"), &r);
                    match r { Ok(()) => { if let Some(k) = v.keys.iter_mut().find(|k| Some(&**k) == v.selected.as_ref()) { *k = to2.clone(); } v.rebuild(); v.select(to2, cx); } Err(e) => v.fail(e) }
                }, cx);
            }
            "item_set" => {
                if a.is_empty() && kind != "stream" { return; }
                self.job(move |kv| async move {
                    match kind.as_str() {
                        "hash" => kv.hset(&key, &a, &b).await,
                        "list" => match a.parse::<i64>() { Ok(i) => kv.lset(&key, i, &b).await, Err(_) => kv.lpush(&key, &a, true).await },
                        "set" => kv.sadd(&key, &a).await,
                        "zset" => kv.zadd(&key, &a, b.trim().parse().map_err(|_| anyhow::anyhow!(t("redis.err_score").to_string()))?).await,
                        "stream" => { let f = parse_fields(&b); anyhow::ensure!(!f.is_empty(), t("redis.err_fields").to_string()); kv.xadd(&key, &f).await.map(|_| ()) }
                        _ => Ok(()),
                    }
                }, move |v, r, cx| v.after_write(w, r, cx), cx);
            }
            "push_tail" | "push_head" => {
                let tail = what == "push_tail";
                self.job(move |kv| async move { kv.lpush(&key, &b, tail).await }, move |v, r, cx| v.after_write(w, r, cx), cx);
            }
            other if other.starts_with("item_del:") => {
                let item = other["item_del:".len()..].to_string();
                self.job(move |kv| async move {
                    match kind.as_str() {
                        "hash" => kv.hdel(&key, &item).await,
                        "list" => kv.ldel(&key, item.parse().unwrap_or(0)).await,
                        "set" => kv.srem(&key, &item).await,
                        "zset" => kv.zrem(&key, &item).await,
                        "stream" => kv.xdel(&key, &item).await,
                        _ => Ok(()),
                    }
                }, move |v, r, cx| v.after_write(w, r, cx), cx);
            }
            "create" => {
                let (key, kind, a, b) = (val(&self.new_key, cx), self.new_kind.clone(), self.new_a.read(cx).value().to_string(), self.new_b.read(cx).value().to_string());
                if key.is_empty() { return; }
                let k2 = key.clone();
                self.job(move |kv| async move { kv.create(&key, &kind, &a, &b).await }, move |v, r, cx| {
                    v.logged(&format!("create {k2}"), &r);
                    match r { Ok(()) => { v.new_open = false; if !v.keys.contains(&k2) { v.keys.push(k2.clone()); v.rebuild(); } v.select(k2, cx); } Err(e) => v.fail(e) }
                }, cx);
            }
            _ => {}
        }
    }
    fn ask_delete_key(&mut self, cx: &mut Context<Self>) {
        if !self.writable() { return; }
        if let Some(k) = self.selected.clone() { self.ask = Some(Ask::Delete(vec![k])); }
        cx.notify();
    }
    fn ask_bulk(&mut self, cx: &mut Context<Self>) {
        if !self.writable() { return; }
        let pat = val(&self.bulk_in, cx);
        if pat.is_empty() { return; }
        self.job(move |kv| async move { kv.matching(&pat, 100_000).await }, |v, r, _| match r {
            Ok(keys) if keys.is_empty() => v.note = Some(t("redis.none_match").into()),
            Ok(keys) => v.ask = Some(Ask::Delete(keys)),
            Err(e) => v.fail(e),
        }, cx);
    }
    fn delete(&mut self, keys: Vec<String>, cx: &mut Context<Self>) {
        let n = keys.len();
        self.job(move |kv| async move { kv.delete(&keys).await.map(|d| (d, keys)) }, move |v, r, _| {
            log_push(&v.log, "redis", &format!("UNLINK ×{n}"), r.is_ok(), r.as_ref().err().map(|e| format!("{e:#}")).unwrap_or_default());
            match r {
                Ok((d, keys)) => {
                    let gone: HashSet<String> = keys.into_iter().collect();
                    v.keys.retain(|k| !gone.contains(k));
                    if v.selected.as_ref().is_some_and(|s| gone.contains(s)) { v.selected = None; v.meta = None; v.content = None; }
                    v.rebuild();
                    v.note = Some(tf("redis.deleted_n", &[("n", &d)]));
                    if let Some(t) = v.total.as_mut() { *t = t.saturating_sub(d); }
                }
                Err(e) => v.fail(e),
            }
        }, cx);
    }

    // ---- console
    /// Snippet from the snippets tool window: into the console line; `run` sends it too.
    pub fn insert_console(&mut self, text: &str, run: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.sub = Sub::Console;
        let line = text.trim().to_string();
        if run { self.console_in.update(cx, |s, cx| s.set_value("", window, cx)); self.console(line, cx); }
        else { self.console_in.update(cx, |s, cx| { s.set_value(line, window, cx); s.focus(window, cx); }); }
        cx.notify();
    }
    fn console(&mut self, line: String, cx: &mut Context<Self>) {
        if line.is_empty() { return; }
        let args = match cmd::split_line(&line) { Ok(a) => a, Err(e) => { self.console_out.push((line, format!("(error) {e}"))); cx.notify(); return; } };
        let risk = cmd::classify(&args);
        if risk != Risk::Read && self.conn.read_only { self.console_out.push((line, format!("(error) {}", t("redis.read_only")))); cx.notify(); return; }
        if risk == Risk::Dangerous || (risk == Risk::Write && self.production) { self.ask = Some(Ask::Console(line)); cx.notify(); return; }
        self.run_console(line, cx);
    }
    fn run_console(&mut self, line: String, cx: &mut Context<Self>) {
        let l2 = line.clone();
        self.job(move |kv| async move { kv.console(&line).await }, move |v, r, _| {
            let out = r.unwrap_or_else(|e| format!("(error) {e:#}"));
            log_push(&v.log, "redis", &l2, !out.starts_with("(error)"), String::new());
            v.console_out.push((l2, out));
            if v.console_out.len() > 500 { v.console_out.remove(0); }
        }, cx);
    }
    fn load_info(&mut self, cx: &mut Context<Self>) { self.job(|kv| async move { kv.info().await }, |v, r, _| match r { Ok(i) => v.info = i, Err(e) => v.fail(e) }, cx); }
    fn load_slow(&mut self, cx: &mut Context<Self>) { self.job(|kv| async move { kv.slowlog(128).await }, |v, r, _| match r { Ok(s) => v.slow = s, Err(e) => v.fail(e) }, cx); }
    fn set_sub(&mut self, s: Sub, cx: &mut Context<Self>) {
        self.sub = s;
        match s { Sub::Info => self.load_info(cx), Sub::Slowlog => self.load_slow(cx), _ => {} }
        cx.notify();
    }

    pub fn ai_label(&self) -> String { format!("Redis · {}", self.conn.name) }
    /// Assistant context: INFO excerpts, key count and a typed key sample; `deep` adds memory of
    /// sampled keys, access frequency (LFU only) and the slow log. Read-only, SCAN-based.
    pub fn ai_context(&self, deep: bool) -> Option<tokio::task::JoinHandle<String>> {
        let kv = self.kv.clone()?;
        let head = format!("Redis connection {}{}\n", self.conn.name, if self.production { " (PRODUCTION)" } else { "" });
        Some(self.rt.spawn(async move {
            let mut out = head;
            const KEEP: [&str; 22] = ["redis_version", "redis_mode", "uptime_in_days", "connected_clients", "blocked_clients", "used_memory_human", "used_memory_peak_human",
                "maxmemory_human", "maxmemory_policy", "mem_fragmentation_ratio", "evicted_keys", "expired_keys", "keyspace_hits", "keyspace_misses", "instantaneous_ops_per_sec",
                "total_commands_processed", "rejected_connections", "role", "connected_slaves", "aof_enabled", "rdb_last_bgsave_status", "lazyfree_pending_objects"];
            if let Ok(info) = kv.info().await {
                for (_, k, v) in info.iter().filter(|(_, k, _)| KEEP.contains(&k.as_str()) || k.starts_with("db")) { out.push_str(&format!("{k}: {v}\n")); }
            }
            if let Ok(n) = kv.dbsize().await { out.push_str(&format!("keys: {n}\n")); }
            let mut keys = vec![];
            let mut cur = ssh_term_proto::kv::Cursor::default();
            for _ in 0..20 { match kv.scan(cur, "*", None).await { Ok((k, c)) => { keys.extend(k); cur = c; if cur.done || keys.len() >= 200 { break; } } Err(_) => break } }
            keys.truncate(200);
            let mut kinds: std::collections::BTreeMap<String, usize> = Default::default();
            let mut sample = String::new();
            let mut mem: Vec<(u64, String, String)> = vec![];
            for k in &keys {
                let ty = kv.raw(&["TYPE".into(), k.clone()]).await.ok().and_then(|v| ssh_term_proto::kv::cmd::scalar(&v)).unwrap_or_default();
                *kinds.entry(ty.clone()).or_default() += 1;
                if sample.len() < 4000 { sample.push_str(&format!("{k} ({ty})\n")); }
                if deep { if let Some(b) = kv.raw(&["MEMORY".into(), "USAGE".into(), k.clone()]).await.ok().and_then(|v| ssh_term_proto::kv::cmd::scalar(&v)).and_then(|s| s.parse::<u64>().ok()) { mem.push((b, k.clone(), ty)); } }
            }
            out.push_str(&format!("Type counts in a sample of {} keys: {:?}\nSample keys:\n{sample}", keys.len(), kinds));
            if deep {
                mem.sort_by(|a, b| b.0.cmp(&a.0));
                out.push_str("Largest sampled keys (MEMORY USAGE bytes):\n");
                for (b, k, ty) in mem.iter().take(15) { out.push_str(&format!("{k} ({ty}) {b}\n")); }
                let mut freq = String::new();
                for (_, k, _) in mem.iter().take(20) {
                    match kv.raw(&["OBJECT".into(), "FREQ".into(), k.clone()]).await { Ok(v) => freq.push_str(&format!("{k} {}\n", ssh_term_proto::kv::cmd::scalar(&v).unwrap_or_default())), Err(_) => { freq = "unavailable (needs an LFU maxmemory-policy)\n".into(); break; } }
                }
                out.push_str(&format!("Access frequency (OBJECT FREQ):\n{freq}"));
                if let Ok(sl) = kv.slowlog(15).await {
                    out.push_str("Slow log (µs, command):\n");
                    for e in sl { out.push_str(&format!("{} {}\n", e.micros, e.command.chars().take(200).collect::<String>())); }
                }
            }
            crate::ai_ctx::cap(out, 16_000)
        }))
    }

    pub fn close(&mut self) { if let Some(kv) = self.kv.take() { self.rt.spawn(async move { kv.close().await }); } }
}

impl Drop for KvView { fn drop(&mut self) { self.close(); } }

// ---------------- render

impl KvView {
    fn tree_el(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let th = cur();
        let mut list = div().id("kv-keys").flex_1().min_h_0().overflow_y_scroll().flex().flex_col().py_1();
        for (i, r) in self.rows.iter().take(ROW_CAP).enumerate() {
            let el = match r {
                tree::Row::Dir { path, name, depth, count, open } => {
                    let p = path.clone();
                    div().id(("kv-r", i)).h(px(24.)).pl(px(8. + *depth as f32 * 14.)).pr_2().flex().items_center().gap_1().cursor_pointer().hover(|d| d.bg(crate::theme::hover_bg()))
                        .on_click(cx.listener(move |v, _, _w, cx| { if !v.open.remove(&p) { v.open.insert(p.clone()); } v.rebuild(); cx.notify(); }))
                        .child(icon(if *open { IconName::ChevronDown } else { IconName::ChevronRight }, ui::IC_XS, c(th.overlay1)))
                        .child(icon(IconName::Folder, ui::IC_SM, c(th.accent)))
                        .child(div().flex_1().min_w_0().whitespace_nowrap().overflow_hidden().text_ellipsis().text_size(px(ui::fs_body())).text_color(c(th.text)).child(name.clone()))
                        .child(div().text_size(px(ui::fs_small())).text_color(c(th.overlay0)).child(count.to_string()))
                }
                tree::Row::Key { key, name, depth } => {
                    let k = key.clone();
                    let sel = self.selected.as_ref() == Some(key);
                    div().id(("kv-r", i)).h(px(24.)).pl(px(22. + *depth as f32 * 14.)).pr_2().flex().items_center().gap_1().cursor_pointer()
                        .when(sel, |d| d.bg(crate::theme::selected_bg())).hover(|d| d.bg(crate::theme::hover_bg()))
                        .on_click(cx.listener(move |v, _, _w, cx| { v.select(k.clone(), cx); cx.notify(); }))
                        .child(icon(IconName::Key, ui::IC_XS, c(th.overlay1)))
                        .child(div().flex_1().min_w_0().whitespace_nowrap().overflow_hidden().text_ellipsis().text_size(px(ui::fs_body())).text_color(c(th.text)).child(name.clone()))
                }
            };
            list = list.child(el);
        }
        if self.rows.len() > ROW_CAP { list = list.child(div().p_2().child(small(tf("redis.rows_capped", &[("n", &ROW_CAP)])))); }
        let loaded = tf("redis.loaded", &[("n", &self.keys.len()), ("total", &self.total.map(|t| t.to_string()).unwrap_or("?".into()))]);
        let kinds: Vec<(String, String)> = std::iter::once(("*".to_string(), t("redis.all_types").to_string())).chain(KINDS.iter().map(|k| (k.to_string(), k.to_string()))).collect();
        let me = cx.entity().downgrade();
        div().w(px(300.)).flex_shrink_0().h_full().flex().flex_col().border_r_1().border_color(crate::theme::divider())
            .child(div().p_2().flex().flex_col().gap_2()
                .child(ui::search_field(&self.filter_in, false))
                .child(div().id("kv-kinds").overflow_x_scroll().child(ui::segmented("kv-kind", &kinds, self.kind_filter.unwrap_or("*"), move |s, _w, cx| {
                    let _ = me.update(cx, |v, cx| { v.kind_filter = KINDS.iter().copied().find(|k| *k == s); v.rescan(cx); cx.notify(); });
                })))
                .child(div().flex().gap_1()
                    .child(ui::button("kv-refresh", t("redis.refresh"), 0).on_click(cx.listener(|v, _, _w, cx| { v.rescan(cx); cx.notify(); })))
                    .child(ui::button("kv-new", t("redis.new_key"), 0).on_click(cx.listener(|v, _, _w, cx| { v.new_open = !v.new_open; cx.notify(); })))))
            .child(list)
            .child(div().p_2().flex().items_center().gap_2().border_t_1().border_color(crate::theme::divider())
                .child(div().flex_1().child(small(loaded)))
                .when(!self.cursor.done, |d| d.child(ui::button("kv-more", if self.scanning { t("db.loading") } else { t("redis.scan_more") }, 0).on_click(cx.listener(|v, _, _w, cx| { v.scan_more(cx); cx.notify(); })))))
            .into_any_element()
    }

    fn new_key_el(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let th = cur();
        let kinds: Vec<(String, String)> = KINDS.iter().map(|k| (k.to_string(), k.to_string())).collect();
        let me = cx.entity().downgrade();
        let (la, lb) = match self.new_kind.as_str() { "hash" => (t("redis.field"), Some(t("redis.value"))), "zset" => (t("redis.member"), Some(t("redis.score"))), "stream" => (t("redis.field"), Some(t("redis.value"))), "string" => (t("redis.value"), None), _ => (t("redis.member"), None) };
        div().p_3().m_3().rounded(px(ui::R_SUB)).bg(crate::theme::sub_bg()).border_1().border_color(crate::theme::divider()).flex().flex_col().gap_2()
            .child(div().text_size(px(ui::fs_body())).font_weight(FontWeight::SEMIBOLD).text_color(c(th.text)).child(t("redis.new_key")))
            .child(ui::segmented("kv-nk", &kinds, &self.new_kind, move |s, _w, cx| { let _ = me.update(cx, |v, cx| { v.new_kind = s; cx.notify(); }); }))
            .child(ui::text_field(&self.new_key, false))
            .child(div().flex().gap_2().child(div().flex_1().child(small(la)).child(ui::text_field(&self.new_a, false)))
                .children(lb.map(|l| div().flex_1().child(small(l)).child(ui::text_field(&self.new_b, false)))))
            .child(div().flex().justify_end().gap_2()
                .child(ui::button("kv-nk-no", t("ui.cancel"), 0).on_click(cx.listener(|v, _, _w, cx| { v.new_open = false; cx.notify(); })))
                .child(ui::button("kv-nk-ok", t("redis.create"), 1).on_click(cx.listener(|v, _, _w, cx| { if v.writable() { v.do_write("create", cx); } cx.notify(); }))))
            .into_any_element()
    }

    fn detail_el(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let th = cur();
        let mut col = div().flex_1().min_w_0().h_full().flex().flex_col();
        if self.new_open { col = col.child(self.new_key_el(cx)); }
        if let Some(e) = &self.error { col = col.child(ui::notice(ui::Notice::Error, e.clone()).mt(px(ui::SP_2))); }
        if let Some(n) = &self.note { col = col.child(div().mx_3().mt_2().text_size(px(ui::fs_body())).text_color(c(th.success)).child(n.clone())); }
        let Some(m) = self.meta.clone() else {
            let hint = if self.loading { t("db.loading") } else { t("redis.pick_key") };
            return col.child(div().flex_1().flex().items_center().justify_center().child(small(hint)))
                .child(div().p_3().flex().gap_2().items_center().border_t_1().border_color(crate::theme::divider())
                    .child(div().flex_1().child(ui::text_field(&self.bulk_in, false)))
                    .child(ui::button("kv-bulk", t("redis.bulk_delete"), 2).on_click(cx.listener(|v, _, _w, cx| { v.ask_bulk(cx); cx.notify(); }))))
                .into_any_element();
        };
        // header: key, type, ttl, size
        let info = format!("{} · TTL {} · {}{}{}", m.kind, ttl_text(m.ttl_ms), m.len.map(|l| tf("redis.len", &[("n", &l)])).unwrap_or_default(),
            m.bytes.map(|b| format!(" · {}", ssh_term_proto::dock::human_size(b as i64))).unwrap_or_default(), if m.encoding.is_empty() { String::new() } else { format!(" · {}", m.encoding) });
        col = col.child(div().px_3().pt_3().flex().flex_col().gap_1()
            .child(div().font_family(ui::mono_family()).text_size(px(ui::fs_body())).font_weight(FontWeight::SEMIBOLD).text_color(c(th.text)).whitespace_normal().child(m.key.clone()))
            .child(small(info)));
        col = col.child(div().px_3().py_2().flex().items_center().gap_2().border_b_1().border_color(crate::theme::divider())
            .child(div().w(px(120.)).child(ui::text_field(&self.ttl_in, false)))
            .child(ui::button("kv-ttl", t("redis.set_ttl"), 0).on_click(cx.listener(|v, _, _w, cx| v.guarded_write("ttl".into(), cx))))
            .child(div().w(px(200.)).child(ui::text_field(&self.rename_in, false)))
            .child(ui::button("kv-ren", t("redis.rename"), 0).on_click(cx.listener(|v, _, _w, cx| v.guarded_write("rename".into(), cx))))
            .child(div().flex_1())
            .child(ui::button("kv-reload", t("redis.refresh"), 0).on_click(cx.listener(|v, _, _w, cx| { v.reload(cx); cx.notify(); })))
            .child(ui::button("kv-del", t("ui.delete"), 2).on_click(cx.listener(|v, _, _w, cx| v.ask_delete_key(cx)))));
        let body: AnyElement = match self.content.clone() {
            Some(Content::Str { full_len, .. }) => {
                let cut = full_len > kv::STR_MAX;
                div().flex_1().min_h_0().flex().flex_col().p_3().gap_2()
                    .when(cut, |d| d.child(div().text_size(px(ui::fs_small())).text_color(c(th.warning)).child(tf("redis.cut", &[("n", &full_len)]))))
                    .when(self.binary, |d| d.child(div().text_size(px(ui::fs_small())).text_color(c(th.warning)).child(t("redis.binary_readonly"))))
                    .child(div().flex_1().min_h_0().child(gpui_component::input::Editor::new(&self.value_ed).h_full()))
                    .child(div().flex().gap_2().justify_end()
                        .child(ui::button("kv-json", t("redis.format_json"), 0).on_click(cx.listener(|v, _, _w, cx| {
                            let s = v.value_ed.read(cx).value().to_string();
                            match kv::pretty_json(&s) { Some(p) => v.pending_value = Some(p), None => v.error = Some(t("redis.not_json").into()) }
                            cx.notify();
                        })))
                        .when(!cut && !self.binary, |d| d.child(ui::button("kv-save", t("ui.save"), 1).on_click(cx.listener(|v, _, _w, cx| v.guarded_write("save_string".into(), cx))))))
                    .into_any_element()
            }
            Some(Content::Pairs { items, next }) => self.items_el(&m.kind, items.into_iter().map(|(a, b)| (a, b)).collect(), next.is_some(), cx),
            Some(Content::Stream { entries, next, groups }) => {
                let rows: Vec<(String, String)> = entries.into_iter().map(|(id, f)| (id, f.iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>().join("  "))).collect();
                let g = (!groups.is_empty()).then(|| small(tf("redis.groups", &[("g", &groups.iter().map(|g| format!("{} (pending {}, consumers {}, last {})", g.0, g.1, g.2, g.3)).collect::<Vec<_>>().join("; "))])).px_3());
                div().flex_1().min_h_0().flex().flex_col().children(g).child(self.items_el("stream", rows, next.is_some(), cx)).into_any_element()
            }
            None => div().flex_1().into_any_element(),
        };
        col.child(body).into_any_element()
    }

    fn items_el(&mut self, kind: &str, items: Vec<(String, String)>, more: bool, cx: &mut Context<Self>) -> AnyElement {
        let th = cur();
        let (ha, hb) = match kind { "hash" => (t("redis.field"), t("redis.value")), "list" => (t("redis.index"), t("redis.value")), "zset" => (t("redis.member"), t("redis.score")), "stream" => ("ID".into(), t("redis.fields")), _ => (t("redis.member"), "".into()) };
        let mut list = div().id("kv-items").flex_1().min_h_0().overflow_y_scroll().flex().flex_col();
        list = list.child(ui::table_header().px_3().gap_3()
            .child(div().w(px(220.)).child(ha)).child(div().flex_1().child(hb)));
        let kind_s = kind.to_string();
        for (i, (a, b)) in items.into_iter().enumerate() {
            let (a2, b2, del) = (a.clone(), b.clone(), a.clone());
            let k = kind_s.clone();
            list = list.child(div().id(("kv-it", i)).min_h(px(26.)).px_3().flex().items_center().gap_3().border_b_1().border_color(crate::theme::divider()).cursor_pointer().hover(|d| d.bg(crate::theme::hover_bg()))
                .on_click(cx.listener(move |v, _, w, cx| {
                    // load into the edit row
                    let (x, y) = if k == "set" { (a2.clone(), String::new()) } else if k == "stream" { (String::new(), b2.clone()) } else { (a2.clone(), b2.clone()) };
                    v.item_a.update(cx, |s, cx| s.set_value(x, w, cx));
                    v.item_b.update(cx, |s, cx| s.set_value(y, w, cx));
                }))
                .child(div().w(px(220.)).flex_shrink_0().whitespace_nowrap().overflow_hidden().text_ellipsis().child(mono(a)))
                .child(div().flex_1().min_w_0().whitespace_nowrap().overflow_hidden().text_ellipsis().child(mono(b)))
                .child(div().id(("kv-it-del", i)).cursor_pointer().child(icon(IconName::Trash, ui::IC_XS, c(th.overlay1)))
                    .on_click(cx.listener(move |v, _, _w, cx| { cx.stop_propagation(); v.guarded_write(format!("item_del:{del}"), cx); }))));
        }
        if more { list = list.child(div().p_2().child(ui::button("kv-it-more", t("redis.load_more"), 0).on_click(cx.listener(|v, _, _w, cx| { v.more_items(cx); cx.notify(); })))); }
        let edit_a = kind != "stream";
        let edit_b = kind != "set";
        let mut bar = div().p_3().flex().items_center().gap_2().border_t_1().border_color(crate::theme::divider());
        if edit_a { bar = bar.child(div().w(px(220.)).child(ui::text_field(&self.item_a, false))); }
        if edit_b { bar = bar.child(div().flex_1().child(ui::text_field(&self.item_b, false))); } else { bar = bar.child(div().flex_1()); }
        bar = bar.child(ui::button("kv-it-set", if kind == "stream" { t("redis.xadd") } else { t("redis.set_item") }, 1).on_click(cx.listener(|v, _, _w, cx| v.guarded_write("item_set".into(), cx))));
        if kind == "list" {
            bar = bar.child(ui::button("kv-push-h", t("redis.push_head"), 0).on_click(cx.listener(|v, _, _w, cx| v.guarded_write("push_head".into(), cx))))
                .child(ui::button("kv-push-t", t("redis.push_tail"), 0).on_click(cx.listener(|v, _, _w, cx| v.guarded_write("push_tail".into(), cx))));
        }
        let hint = match kind { "list" => Some(t("redis.list_hint")), "stream" => Some(t("redis.stream_hint")), _ => None };
        div().flex_1().min_h_0().flex().flex_col().child(list).child(bar).children(hint.map(|h| small(h).px_3().pb_2())).into_any_element()
    }

    fn console_el(&mut self, _cx: &mut Context<Self>) -> AnyElement {
        let th = cur();
        let mut out = div().id("kv-console").flex_1().min_h_0().overflow_y_scroll().p_3().flex().flex_col().gap_2().bg(crate::theme::sub_bg());
        for (l, o) in &self.console_out {
            out = out.child(div().flex().flex_col()
                .child(mono(format!("{}> {l}", self.conn.address())).text_color(c(th.accent)))
                .child(mono(o.clone()).whitespace_normal().text_color(if o.starts_with("(error)") { c(th.error) } else { c(th.text) })));
        }
        div().flex_1().min_h_0().flex().flex_col().child(out)
            .child(div().p_2().border_t_1().border_color(crate::theme::divider()).child(ui::text_field(&self.console_in, false)))
            .child(small(t("redis.console_hint")).px_3().pb_2())
            .into_any_element()
    }

    fn info_el(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let th = cur();
        let mut list = div().id("kv-info").flex_1().min_h_0().overflow_y_scroll().p_3().flex().flex_col();
        let mut last = String::new();
        for (sec, k, v) in &self.info {
            if *sec != last { last = sec.clone(); list = list.child(div().mt_2().mb_1().text_size(px(ui::fs_body())).font_weight(FontWeight::SEMIBOLD).text_color(c(th.accent)).child(sec.clone())); }
            list = list.child(div().flex().gap_3().child(div().w(px(260.)).flex_shrink_0().child(mono(k.clone()).text_color(c(th.subtext)))).child(div().flex_1().min_w_0().whitespace_normal().child(mono(v.clone()))));
        }
        div().flex_1().min_h_0().flex().flex_col()
            .child(div().p_2().flex().gap_2().child(ui::button("kv-info-r", t("redis.refresh"), 0).on_click(cx.listener(|v, _, _w, cx| { v.load_info(cx); cx.notify(); }))))
            .child(list).into_any_element()
    }

    fn slow_el(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let th = cur();
        let mut list = div().id("kv-slow").flex_1().min_h_0().overflow_y_scroll().p_3().flex().flex_col().gap_1();
        if self.slow.is_empty() { list = list.child(small(t("redis.slow_empty"))); }
        for e in &self.slow {
            let at = chrono::DateTime::from_timestamp(e.at, 0).map(|d| d.with_timezone(&chrono::Local).format("%m-%d %H:%M:%S").to_string()).unwrap_or_default();
            list = list.child(div().flex().gap_3().border_b_1().border_color(crate::theme::divider()).py_1()
                .child(div().w(px(110.)).flex_shrink_0().child(mono(at).text_color(c(th.subtext))))
                .child(div().w(px(90.)).flex_shrink_0().child(mono(format!("{:.2} ms", e.micros as f64 / 1000.)).text_color(c(th.warning))))
                .child(div().flex_1().min_w_0().whitespace_normal().child(mono(e.command.clone())))
                .child(div().w(px(150.)).flex_shrink_0().child(mono(e.client.clone()).text_color(c(th.overlay1)))));
        }
        div().flex_1().min_h_0().flex().flex_col()
            .child(div().p_2().flex().gap_2().child(ui::button("kv-slow-r", t("redis.refresh"), 0).on_click(cx.listener(|v, _, _w, cx| { v.load_slow(cx); cx.notify(); }))))
            .child(list).into_any_element()
    }

    fn ask_el(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let ask = self.ask.as_ref()?;
        let th = cur();
        let (msg, preview) = match ask {
            Ask::Console(l) => (if self.production { t("redis.confirm_prod") } else { t("redis.confirm_danger") }.to_string(), l.clone()),
            Ask::Delete(keys) => (tf("redis.confirm_delete", &[("n", &keys.len())]), keys.iter().take(50).cloned().collect::<Vec<_>>().join("\n") + if keys.len() > 50 { "\n…" } else { "" }),
            Ask::Write(w) => (t("redis.confirm_prod").to_string(), format!("{} · {}", self.selected.clone().unwrap_or_default(), w)),
        };
        let prod = self.production.then(|| ui::tag(t("tag.production"), c(th.error)));
        Some(ui::modal(ui::card(520.)
            .child(ui::dialog_header(Some(IconName::TriangleAlert), c(cur().warning), t("db.confirm_title"), None, prod.map(|p| p.into_any_element())))
            .child(div().p(px(ui::SP_5)).flex().flex_col().gap(px(ui::SP_3))
                .child(small(msg))
                .child(div().id("kv-cf-pre").max_h(px(300.)).overflow_y_scroll().p_2().rounded(px(ui::R_MD)).bg(crate::theme::sub_bg()).child(mono(preview).whitespace_normal())))
            .child(ui::dialog_footer(None, vec![
                ui::button("kv-cf-no", t("ui.cancel"), 0).on_click(cx.listener(|v, _, _w, cx| { v.ask = None; cx.notify(); })).into_any_element(),
                ui::button("kv-cf-yes", t("db.confirm_run"), 2).on_click(cx.listener(|v, _, _w, cx| {
                    match v.ask.take() {
                        Some(Ask::Console(l)) => v.run_console(l, cx),
                        Some(Ask::Delete(k)) => v.delete(k, cx),
                        Some(Ask::Write(w)) => v.do_write(&w, cx),
                        None => {}
                    }
                    cx.notify();
                })).into_any_element(),
            ]))).into_any_element())
    }
}

impl Render for KvView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let th = cur();
        if let Some(s) = self.pending_value.take() { self.value_ed.update(cx, |e, cx| e.set_value(s, window, cx)); }
        let mut head = ui::toolbar()
            .child(icon(IconName::Database, ui::IC_SM, c(th.overlay1)))
            .child(div().text_size(px(ui::fs_body())).font_weight(FontWeight::SEMIBOLD).text_color(c(th.text)).child(self.conn.name.clone()))
            .child(small(format!("{} · db{}{}", self.conn.address(), self.conn.db, self.kv.as_ref().map(|k| format!(" · {} {}", k.mode, k.version)).unwrap_or_default())));
        if self.production { head = head.child(ui::tag(t("tag.production"), c(th.error))); }
        if self.conn.read_only { head = head.child(ui::tag(t("db.read_only"), c(th.warning))); }
        head = head.child(div().flex_1());
        let subs: Vec<(String, String)> = [("keys", t("redis.tab_keys")), ("console", t("redis.tab_console")), ("info", t("redis.tab_info")), ("slow", t("redis.tab_slowlog"))].iter().map(|(a, b)| (a.to_string(), b.to_string())).collect();
        let cur_sub = match self.sub { Sub::Keys => "keys", Sub::Console => "console", Sub::Info => "info", Sub::Slowlog => "slow" };
        let me = cx.entity().downgrade();
        head = head.child(ui::segmented("kv-sub", &subs, cur_sub, move |s, _w, cx| {
            let sub = match s.as_str() { "console" => Sub::Console, "info" => Sub::Info, "slow" => Sub::Slowlog, _ => Sub::Keys };
            let _ = me.update(cx, |v, cx| v.set_sub(sub, cx));
        }));
        let body: AnyElement = if let Some(e) = &self.conn_err {
            div().flex_1().p_5().flex().flex_col().gap_2().child(ui::notice(ui::Notice::Error, t("ui.connection_failed")).mx_0()).child(mono(e.clone()).whitespace_normal()).into_any_element()
        } else if self.kv.is_none() {
            div().flex_1().flex().items_center().justify_center().child(small(t("ui.connecting"))).into_any_element()
        } else {
            match self.sub {
                Sub::Keys => { let tr = self.tree_el(cx); let d = self.detail_el(cx); div().flex_1().min_h_0().flex().child(tr).child(d).into_any_element() }
                Sub::Console => self.console_el(cx),
                Sub::Info => self.info_el(cx),
                Sub::Slowlog => self.slow_el(cx),
            }
        };
        let ask = self.ask_el(cx);
        div().size_full().flex().flex_col().relative().bg(c(th.base)).child(head).child(body).children(ask)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ttl_and_fields() {
        assert_eq!((ttl_text(-1).as_str(), ttl_text(-2).as_str(), ttl_text(5_000).as_str(), ttl_text(3_725_000).as_str(), ttl_text(90_000_000).as_str()), ("∞", "—", "5s", "1h 2m", "1d 1h"));
        assert_eq!(parse_fields("a=1 'b c'=x y"), vec![("a".into(), "1".into()), ("b c".into(), "x".into())]);
    }
}
