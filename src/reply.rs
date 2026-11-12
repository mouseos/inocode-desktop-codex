//! Replying to a message by its number. A message of the user or the assistant has a number
//! `#<turn>.<item>` that the server gives (inocode/messages/resolve); "reply" puts a chip over
//! the composer, and the next message starts with one line `[返信 #12.3「SQLite の並行…」]` per
//! chip. A message that starts with such lines is shown with chips for them.

use crate::state::STATE;
use crate::view::messages_with;
use crate::*;
use serde_json::{json, Value};
use slint::Model;
use std::cell::RefCell;
use std::collections::VecDeque;

/// How many characters of the message are in the marker of a reply.
const HINT_CHARS: usize = 20;
/// How many numbers are asked for at the same time.
const IN_FLIGHT: usize = 6;

/// What is known of a quoted message.
#[derive(Clone, Debug)]
struct Resolved {
    /// 0 readable, 1 not found, 2 cannot be opened, 3 out of reach
    state: i32,
    role: String,
    text: String,
}

thread_local! {
    static HANDLE: RefCell<Option<Rc<client::Handle>>> = const { RefCell::new(None) };
    /// The quotes that are ready.
    static CHIPS: RefCell<Vec<Ref>> = const { RefCell::new(Vec::new()) };
    /// What the server said of a quoted message (inocode/messages/resolveMany), by thread|number|range.
    static RESOLVED: RefCell<std::collections::HashMap<String, Resolved>> = RefCell::new(std::collections::HashMap::new());
    /// The keys that were asked and not answered yet.
    static ASKED_KEYS: RefCell<std::collections::HashSet<String>> = RefCell::new(std::collections::HashSet::new());
    /// A jump to a message of another thread, made when that thread is open and numbered.
    static PENDING_JUMP: RefCell<Option<String>> = const { RefCell::new(None) };
    /// Messages whose number is still to be asked: (turn id, item index).
    static QUEUE: RefCell<VecDeque<(String, i32)>> = const { RefCell::new(VecDeque::new()) };
    static ASKED: RefCell<usize> = const { RefCell::new(0) };
    static DEMO_TIMER: RefCell<Option<slint::Timer>> = const { RefCell::new(None) };
}

// ------------------------------------------------------------------ pure helpers (tested)

/// The marker of a message: its start, whitespace as one space, 20 characters.
pub(crate) fn hint_of(text: &str) -> String {
    let flat: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let flat: String = flat.chars().filter(|c| !matches!(c, '「' | '」' | '[' | ']')).collect();
    let mut chars = flat.chars();
    let head: String = chars.by_ref().take(HINT_CHARS).collect();
    if chars.next().is_some() {
        format!("{head}…")
    } else {
        head
    }
}

/// A quoted message: the number, the marker (the start of it), the thread it is in (empty: this one;
/// for a message between agents: the thread of the sender) and the part of the text that is
/// quoted ("40-95", in characters; empty: all of it).
#[derive(Clone, Debug, PartialEq, Default)]
pub(crate) struct Ref {
    pub(crate) id: String,
    pub(crate) hint: String,
    pub(crate) thread: String,
    pub(crate) range: String,
}

/// One line of the message: `[引用 #12.3 thread:<thread> 40-95]` (the thread and the range only when
/// there are some). The old `[返信 #12.3「…」]` is still read.
pub(crate) fn ref_line(r: &Ref) -> String {
    let mut out = format!("[引用 {}", r.id);
    if !r.thread.is_empty() {
        out.push_str(&format!(" thread:{}", r.thread));
    }
    if !r.range.is_empty() {
        out.push_str(&format!(" {}", r.range));
    }
    out.push(']');
    out
}

/// The lines of quotes at the start of a message and the text after them.
pub(crate) fn parse_ref_lines(text: &str) -> (Vec<Ref>, String) {
    let mut refs = Vec::new();
    let mut rest = text;
    loop {
        let (line, after) = match rest.split_once('\n') {
            Some((l, a)) => (l, a),
            None => (rest, ""),
        };
        match parse_ref_line(line.trim_end_matches('\r')) {
            Some(r) => {
                refs.push(r);
                rest = after;
                if after.is_empty() {
                    break;
                }
            }
            None => break,
        }
    }
    (refs, rest.to_string())
}

/// "#12.3" or "#12.@3" at the start of `s`: the number and what follows it.
fn take_number(s: &str) -> Option<(String, &str)> {
    let body = s.strip_prefix('#')?;
    let (turn, tail) = body.split_once('.')?;
    let (at, tail) = match tail.strip_prefix('@') {
        Some(t) => ("@", t),
        None => ("", tail),
    };
    let item_len = tail.chars().take_while(|c| c.is_ascii_digit()).count();
    let (item, tail) = tail.split_at(item_len);
    if turn.is_empty() || item.is_empty() || !turn.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    Some((format!("#{turn}.{at}{item}"), tail))
}

fn parse_ref_line(line: &str) -> Option<Ref> {
    if let Some(body) = line.strip_prefix("[引用 ") {
        let inner = body.strip_suffix(']')?;
        let (id, tail) = take_number(inner)?;
        let mut thread = String::new();
        let mut range = String::new();
        for word in tail.split_whitespace() {
            if let Some(t) = word.strip_prefix("thread:") {
                if t.is_empty() {
                    return None;
                }
                thread = t.to_string();
            } else if let Some((a, b)) = word.split_once('-') {
                if a.is_empty() || b.is_empty() || !a.chars().all(|c| c.is_ascii_digit()) || !b.chars().all(|c| c.is_ascii_digit()) {
                    return None;
                }
                range = word.to_string();
            } else {
                return None;
            }
        }
        if !tail.is_empty() && !tail.starts_with(' ') {
            return None;
        }
        return Some(Ref { id, hint: String::new(), thread, range });
    }
    let body = line.strip_prefix("[返信 ")?;
    let (id, tail) = take_number(body)?;
    let inner = tail.strip_suffix(']')?;
    // The marker is optional in the lines of the agents: `[返信 #12.3 thread:abc]`.
    let (hint, after) = match inner.strip_prefix('「') {
        Some(rest) => {
            let close = rest.rfind('」')?;
            (&rest[..close], &rest[close + '」'.len_utf8()..])
        }
        None => ("", inner),
    };
    let thread = match after {
        "" => "",
        a => {
            let t = a.strip_prefix(" thread:")?;
            if t.is_empty() || t.contains(char::is_whitespace) {
                return None;
            }
            t
        }
    };
    Some(Ref { id, hint: hint.to_string(), thread: thread.to_string(), range: String::new() })
}

/// The text to send: the lines of the quotes, then the text.
pub(crate) fn with_refs(chips: &[Ref], text: &str) -> String {
    if chips.is_empty() {
        return text.to_string();
    }
    let mut out: Vec<String> = chips.iter().map(ref_line).collect();
    out.push(text.to_string());
    out.join("\n")
}

/// The link of a message: `inocode://thread/<thread>#12.3`.
pub(crate) fn link_of(thread: &str, id: &str) -> String {
    format!("inocode://thread/{thread}#{}", id.trim_start_matches('#'))
}

/// Links in a text (pasted): the text without them and the quotes they stand for.
pub(crate) fn take_links(text: &str) -> Option<(String, Vec<Ref>)> {
    const START: &str = "inocode://thread/";
    if !text.contains(START) {
        return None;
    }
    let mut refs = Vec::new();
    let mut out = String::new();
    let mut rest = text;
    while let Some(at) = rest.find(START) {
        out.push_str(&rest[..at]);
        let tail = &rest[at + START.len()..];
        let thread_len = tail.chars().take_while(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_').count();
        let (thread, after) = tail.split_at(thread_len);
        let parsed = after.strip_prefix('#').and_then(|n| take_number(&format!("#{n}")).map(|(id, t)| (id, n.len() + 1 - t.len() - 0, t.len())));
        match parsed {
            Some((id, _, tail_len)) if !thread.is_empty() => {
                refs.push(Ref { id, hint: String::new(), thread: thread.to_string(), range: String::new() });
                rest = &after[after.len() - tail_len..];
            }
            _ => {
                out.push_str(START);
                rest = tail;
            }
        }
    }
    out.push_str(rest);
    if refs.is_empty() {
        return None;
    }
    Some((out.trim().to_string(), refs))
}

/// The lines of an excerpt for a card: at most three lines, each cut at `width` characters.
pub(crate) fn excerpt_lines(text: &str) -> Vec<String> {
    const WIDTH: usize = 64;
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .take(3)
        .map(|l| {
            let mut it = l.chars();
            let head: String = it.by_ref().take(WIDTH).collect();
            if it.next().is_some() { format!("{head}…") } else { head }
        })
        .collect()
}

/// Character offsets of a byte range of `text`.
pub(crate) fn char_range(text: &str, a: usize, b: usize) -> (usize, usize) {
    let a = a.min(text.len());
    let b = b.min(text.len());
    (text.get(..a).map_or(0, |t| t.chars().count()), text.get(..b).map_or(0, |t| t.chars().count()))
}

/// The characters [start, end) of a text.
pub(crate) fn slice_chars(text: &str, start: usize, end: usize) -> String {
    text.chars().skip(start).take(end.saturating_sub(start)).collect()
}

/// For each row of the screen (role, text): the message of the thread (role, text) it is, in
/// order, or None. A row that the thread does not have (a status line) is skipped.
pub(crate) fn align(rows: &[(String, String)], thread: &[(String, String)]) -> Vec<Option<usize>> {
    let mut at = 0;
    rows.iter()
        .map(|(role, text)| {
            let found = (at..thread.len()).find(|j| thread[*j].0 == *role && thread[*j].1 == *text);
            if let Some(j) = found {
                at = j + 1;
            }
            found
        })
        .collect()
}

// ------------------------------------------------------------------ screen

fn key_of(r: &Ref, active: &str) -> String {
    let thread = if r.thread.is_empty() { active } else { r.thread.as_str() };
    format!("{thread}|{}|{}", r.id, r.range)
}

/// The card of a quote: what the server said, else what is known of it (the marker).
fn card_of(r: &Ref, active: &str) -> ReplyRef {
    let key = key_of(r, active);
    let resolved = RESOLVED.with(|c| c.borrow().get(&key).cloned());
    let (state, role, text) = match &resolved {
        Some(x) => (x.state, x.role.as_str(), x.text.as_str()),
        None => (0, "", r.hint.as_str()),
    };
    let speaker = match role {
        "user" => "自分",
        "assistant" => "AI",
        _ => "",
    };
    let other = !r.thread.is_empty() && r.thread != active;
    let thread_name = if other {
        STATE.with(|s| s.borrow().threads.iter().find(|t| t.id == r.thread).map(|t| t.title.clone()).unwrap_or_default())
    } else {
        String::new()
    };
    let mut head: Vec<&str> = Vec::new();
    for part in [speaker, r.id.as_str(), thread_name.as_str()] {
        if !part.is_empty() {
            head.push(part);
        }
    }
    let lines: Vec<SharedString> = match state {
        1 => vec!["メッセージが見つかりません".into()],
        2 => vec!["このメッセージは開けません".into()],
        3 => vec!["メッセージが見つかりません".into()],
        _ => excerpt_lines(text).into_iter().map(Into::into).collect(),
    };
    ReplyRef {
        id: r.id.as_str().into(),
        hint: r.hint.as_str().into(),
        thread: if other { r.thread.as_str().into() } else { "".into() },
        head: head.join(" · ").into(),
        lines: ModelRc::from(Rc::new(VecModel::from(lines))),
        // 0: can be pressed, 1: dim and not pressable
        dim: state != 0,
    }
}

/// Ask what the cards need (once for each, a batch at a time).
fn ensure_resolved(refs: &[Ref]) {
    let Some(h) = HANDLE.with(|c| c.borrow().clone()) else { return };
    let active = STATE.with(|s| s.borrow().active.clone());
    if active.is_empty() {
        return;
    }
    let mut asks: Vec<Value> = Vec::new();
    let mut keys: Vec<String> = Vec::new();
    for r in refs {
        // A message between agents has the number of the sender's thread: not asked here.
        if r.id.contains('@') {
            continue;
        }
        let key = key_of(r, &active);
        if RESOLVED.with(|c| c.borrow().contains_key(&key)) || !ASKED_KEYS.with(|k| k.borrow_mut().insert(key.clone())) {
            continue;
        }
        let mut ask = json!({ "id": r.id });
        if !r.thread.is_empty() {
            ask["threadId"] = json!(r.thread);
            ask["userAttached"] = json!(true);
        }
        if let Some((a, b)) = r.range.split_once('-') {
            if let (Ok(a), Ok(b)) = (a.parse::<i64>(), b.parse::<i64>()) {
                ask["range"] = json!([a, b]);
            }
        }
        asks.push(ask);
        keys.push(key);
    }
    if asks.is_empty() {
        return;
    }
    h.send(Command::Rpc {
        tag: format!("quote.many|{}", keys.join("\u{1}")),
        method: "inocode/messages/resolveMany".into(),
        params: json!({ "callerThreadId": active, "refs": asks, "maxChars": 600 }),
    });
}

fn render_chips(app: &AppWindow) {
    let active = STATE.with(|s| s.borrow().active.clone());
    let chips = CHIPS.with(|c| c.borrow().clone());
    ensure_resolved(&chips);
    let rows: Vec<ReplyRef> = chips.iter().map(|r| card_of(r, &active)).collect();
    app.global::<ReplyState>().set_chips(ModelRc::from(Rc::new(VecModel::from(rows))));
}

/// The cards in the messages of the chat are drawn again (what the server said came).
fn refresh_rows(app: &AppWindow) {
    let active = STATE.with(|s| s.borrow().active.clone());
    messages_with(app, |model| {
        for i in 0..model.row_count() {
            let Some(mut m) = model.row_data(i) else { continue };
            if m.role != "user" || m.refs.row_count() == 0 {
                continue;
            }
            let (refs, _) = parse_ref_lines(&m.text);
            let rows: Vec<ReplyRef> = refs.iter().map(|r| card_of(r, &active)).collect();
            m.refs = ModelRc::from(Rc::new(VecModel::from(rows)));
            model.set_row_data(i, m);
        }
    });
}

fn status(app: &AppWindow, text: &str) {
    app.global::<AppState>().set_status(text.into());
    app.global::<AppState>().set_status_level(2);
}

/// What the screen needs of a message: the lines of quotes of a user message become cards.
pub(crate) fn decorate(m: &mut Message) {
    if m.role != "user" {
        return;
    }
    let (refs, body) = parse_ref_lines(&m.text);
    if refs.is_empty() {
        return;
    }
    let active = STATE.with(|s| s.borrow().active.clone());
    let rows: Vec<ReplyRef> = refs.iter().map(|r| card_of(r, &active)).collect();
    m.refs = ModelRc::from(Rc::new(VecModel::from(rows)));
    m.shown = body.into();
    ensure_resolved(&refs);
}

/// The text of the composer with the lines of the quotes in front; the cards are used up.
pub(crate) fn compose(app: &AppWindow, text: String) -> String {
    let chips = CHIPS.with(|c| std::mem::take(&mut *c.borrow_mut()));
    render_chips(app);
    with_refs(&chips, &text)
}

pub(crate) fn add_ref(app: &AppWindow, r: Ref) {
    CHIPS.with(|c| {
        let mut c = c.borrow_mut();
        if !c.iter().any(|x| x.id == r.id && x.thread == r.thread && x.range == r.range) {
            c.push(r);
        }
    });
    render_chips(app);
}

/// A quote by its parts (the team log uses it).
pub(crate) fn add_chip(app: &AppWindow, id: &str, hint: &str, thread: &str) {
    add_ref(app, Ref { id: id.to_string(), hint: hint.to_string(), thread: thread.to_string(), range: String::new() });
}

/// A link was pasted in the composer: the cards of the links, and the text without them.
pub(crate) fn pasted_links(app: &AppWindow, text: &str) -> bool {
    let Some((rest, refs)) = take_links(text) else { return false };
    let active = STATE.with(|s| s.borrow().active.clone());
    for mut r in refs {
        if r.thread == active {
            r.thread = active.clone();
        }
        add_ref(app, r);
    }
    app.set_prompt(rest.into());
    true
}

/// "Quote" under a message (the whole message, or `part` of its text, in bytes).
fn reply_to(app: &AppWindow, index: usize, part: Option<(usize, usize)>) {
    let model = app.global::<AppState>().get_messages();
    let Some(m) = model.row_data(index) else { return };
    if m.role != "user" && m.role != "assistant" {
        return;
    }
    let text = m.text.to_string();
    let body = if m.role == "user" { parse_ref_lines(&text).1 } else { text };
    let (source, range) = match part {
        Some((a, b)) => {
            let (ca, cb) = char_range(&body, a, b);
            (slice_chars(&body, ca, cb), format!("{ca}-{cb}"))
        }
        None => (body, String::new()),
    };
    let thread = STATE.with(|s| s.borrow().active.clone());
    let hint = hint_of(&source);
    if !m.ref_id.is_empty() {
        // The card shows what is quoted at once (the server confirms it).
        RESOLVED.with(|c| {
            c.borrow_mut().insert(
                format!("{thread}|{}|{range}", m.ref_id),
                Resolved { state: 0, role: m.role.to_string(), text: source.clone() },
            )
        });
        add_ref(app, Ref { id: m.ref_id.to_string(), hint, thread, range });
        return;
    }
    // Not named yet: ask now.
    if m.turn_id.is_empty() || thread.is_empty() {
        status(app, "この発言の番号をまだ取得できません");
        return;
    }
    if let Some(h) = HANDLE.with(|c| c.borrow().clone()) {
        let _ = &source;
        h.send(Command::Rpc {
            tag: format!("reply.add|{}|{}|{hint}|{range}|{}", m.turn_id, m.item_index, m.role),
            method: "inocode/messages/resolve".into(),
            params: json!({ "threadId": thread, "turnId": m.turn_id.as_str(), "itemIndex": m.item_index }),
        });
    }
}

fn number_of(v: &Value) -> Option<String> {
    let p = v.get("response").unwrap_or(v);
    Some(format!("#{}.{}", p.get("turnNumber")?.as_i64()?, p.get("itemNumber")?.as_i64()?))
}

/// Ask for the numbers of the messages that have none yet (a few at a time).
fn pump() {
    let Some(h) = HANDLE.with(|c| c.borrow().clone()) else { return };
    let thread = STATE.with(|s| s.borrow().active.clone());
    if thread.is_empty() {
        QUEUE.with(|q| q.borrow_mut().clear());
        return;
    }
    while ASKED.with(|a| *a.borrow()) < IN_FLIGHT {
        let Some((turn, item)) = QUEUE.with(|q| q.borrow_mut().pop_front()) else { break };
        ASKED.with(|a| *a.borrow_mut() += 1);
        h.send(Command::Rpc {
            tag: format!("reply.num|{turn}|{item}"),
            method: "inocode/messages/resolve".into(),
            params: json!({ "threadId": thread, "turnId": turn, "itemIndex": item }),
        });
    }
}

/// The conversation was replaced (history): number the messages.
pub(crate) fn number_all(app: &AppWindow) {
    QUEUE.with(|q| q.borrow_mut().clear());
    ASKED.with(|a| *a.borrow_mut() = 0);
    let model = app.global::<AppState>().get_messages();
    let jobs: Vec<(String, i32)> = (0..model.row_count())
        .filter_map(|i| model.row_data(i))
        .filter(|m| (m.role == "user" || m.role == "assistant") && m.ref_id.is_empty() && !m.turn_id.is_empty())
        .map(|m| (m.turn_id.to_string(), m.item_index))
        .collect();
    QUEUE.with(|q| q.borrow_mut().extend(jobs));
    pump();
}

/// A turn ended: the rows that were added while it ran learn where they sit in the thread, and
/// get their numbers.
pub(crate) fn after_turn(app: &AppWindow) {
    let thread = STATE.with(|s| s.borrow().active.clone());
    if thread.is_empty() {
        return;
    }
    let _ = app;
    if let Some(h) = HANDLE.with(|c| c.borrow().clone()) {
        h.send(Command::Rpc { tag: "reply.read".into(), method: "thread/read".into(), params: json!({ "threadId": thread, "includeTurns": true }) });
    }
}

pub(crate) fn on_rpc(app: &AppWindow, tag: &str, result: &Result<Value, String>) {
    if tag == "reply.read" {
        let Ok(v) = result else { return };
        let hist = client::history_from_thread(v.get("thread").unwrap_or(&Value::Null));
        let thread: Vec<(String, String)> = hist.iter().map(|m| (m.role.clone(), m.text.clone())).collect();
        messages_with(app, |model| {
            let rows: Vec<(String, String)> = (0..model.row_count())
                .map(|i| model.row_data(i).map(|m| (m.role.to_string(), m.text.to_string())).unwrap_or_default())
                .collect();
            let found = align(&rows, &thread);
            for (i, j) in found.into_iter().enumerate() {
                let Some(j) = j else { continue };
                let Some(mut m) = model.row_data(i) else { continue };
                if (m.role == "user" || m.role == "assistant") && m.turn_id.is_empty() {
                    m.turn_id = hist[j].turn_id.as_str().into();
                    m.item_index = hist[j].item_index;
                    model.set_row_data(i, m);
                }
            }
        });
        number_all(app);
        return;
    }
    if let Some(rest) = tag.strip_prefix("reply.num|") {
        ASKED.with(|a| {
            let mut a = a.borrow_mut();
            *a = a.saturating_sub(1);
        });
        if let (Ok(v), Some((turn, item))) = (result, rest.split_once('|')) {
            if let (Some(id), Ok(item)) = (number_of(v), item.parse::<i32>()) {
                messages_with(app, |model| {
                    for i in 0..model.row_count() {
                        if let Some(mut m) = model.row_data(i) {
                            if m.turn_id.as_str() == turn && m.item_index == item && m.ref_id.as_str() != id {
                                m.ref_id = id.as_str().into();
                                model.set_row_data(i, m);
                            }
                        }
                    }
                });
            }
        }
        pump();
        try_pending_jump(app);
        return;
    }
    if let Some(rest) = tag.strip_prefix("reply.add|") {
        let mut parts = rest.splitn(5, '|');
        let _turn = parts.next();
        let _item = parts.next();
        let hint = parts.next().unwrap_or("").to_string();
        let range = parts.next().unwrap_or("").to_string();
        match result {
            Ok(v) => match number_of(v) {
                Some(id) => {
                    let thread = STATE.with(|s| s.borrow().active.clone());
                    add_ref(app, Ref { id, hint, thread, range });
                }
                None => status(app, "この発言の番号を取得できません"),
            },
            Err(e) => status(app, &format!("この発言の番号を取得できません: {e}")),
        }
        return;
    }
    if let Some(keys) = tag.strip_prefix("quote.many|") {
        let keys: Vec<&str> = keys.split('\u{1}').collect();
        let items = result.as_ref().ok().and_then(|v| v.get("items")).and_then(Value::as_array).cloned().unwrap_or_default();
        for (i, key) in keys.iter().enumerate() {
            ASKED_KEYS.with(|k| k.borrow_mut().remove(*key));
            let Some(it) = items.get(i) else { continue };
            let state = match it.get("state").and_then(Value::as_str) {
                Some("ok") => 0,
                Some("cannotOpen") => 2,
                Some("outOfScope") => 3,
                _ => 1,
            };
            let text = it.get("text").and_then(Value::as_str).unwrap_or("").to_string();
            let role = it.get("role").and_then(Value::as_str).unwrap_or("").to_string();
            RESOLVED.with(|c| c.borrow_mut().insert(key.to_string(), Resolved { state, role, text }));
        }
        refresh_rows(app);
        render_chips(app);
    }
}

/// Scroll to the message with this number in the chat that is open.
fn jump_in_chat(app: &AppWindow, id: &str) -> bool {
    let model = app.global::<AppState>().get_messages();
    match (0..model.row_count()).find(|i| model.row_data(*i).map(|m| m.ref_id.as_str() == id).unwrap_or(false)) {
        Some(i) => {
            app.invoke_scroll_to(i as i32);
            true
        }
        None => false,
    }
}

/// The numbers of a thread that was opened for a jump came: move to the message.
fn try_pending_jump(app: &AppWindow) {
    let Some(id) = PENDING_JUMP.with(|p| p.borrow().clone()) else { return };
    if jump_in_chat(app, &id) {
        PENDING_JUMP.with(|p| *p.borrow_mut() = None);
    }
}

pub(crate) fn register(app: &AppWindow, handle: &Rc<client::Handle>) {
    HANDLE.with(|c| *c.borrow_mut() = Some(handle.clone()));
    if std::env::var("INOCODE_GUI_DEMO").map(|v| v == "reply").unwrap_or(false) {
        // Mock only: a conversation with numbers, so that the screen can be looked at.
        let weak = app.as_weak();
        let timer = slint::Timer::default();
        timer.start(slint::TimerMode::SingleShot, std::time::Duration::from_secs(6), move || {
            let Some(app) = weak.upgrade() else { return };
            messages_with(&app, |model| {
                let mut rows = vec![
                    Message { role: "user".into(), text: "SQLite の並行書き込みはどうなりますか".into(), ref_id: "#1.1".into(), ..Default::default() },
                    Message { role: "assistant".into(), text: "WAL を使うと、読み取りと書き込みが同時にできます。

書き込みは 1 つずつです。".into(), ref_id: "#1.2".into(), turn_id: "t1".into(), item_index: 1, ..Default::default() },
                    Message { role: "user".into(), text: "[返信 #1.2「WAL を使うと、読み取りと書き込みが同時にできます。 書き込みは 1…」]
ありがとう。書き込みが重いときは？".into(), ref_id: "#2.1".into(), ..Default::default() },
                ];
                for m in rows.iter_mut() {
                    decorate(m);
                    if m.role == "assistant" {
                        m.blocks = crate::chat_md::blocks_of(&m.text);
                    }
                }
                for m in rows {
                    model.push(m);
                }
            });
        });
        DEMO_TIMER.with(|t| *t.borrow_mut() = Some(timer));
    }
    let weak = app.as_weak();
    app.global::<ReplyState>().on_remove(move |i| {
        CHIPS.with(|c| {
            let mut c = c.borrow_mut();
            if (i as usize) < c.len() {
                c.remove(i as usize);
            }
        });
        if let Some(app) = weak.upgrade() {
            render_chips(&app);
        }
    });
    let weak = app.as_weak();
    app.global::<ReplyState>().on_jump(move |thread, id| {
        let Some(app) = weak.upgrade() else { return };
        let active = STATE.with(|s| s.borrow().active.clone());
        if !thread.is_empty() && thread.as_str() != active {
            // Another session: open it, then move to the message.
            PENDING_JUMP.with(|p| *p.borrow_mut() = Some(id.to_string()));
            app.global::<AppState>().invoke_open_thread(thread);
            return;
        }
        jump_in_chat(&app, id.as_str());
    });
    let weak = app.as_weak();
    app.global::<MessageActions>().on_reply_selection(move |index, start, end| {
        if let Some(app) = weak.upgrade() {
            reply_to(&app, index.max(0) as usize, Some((start.max(0) as usize, end.max(0) as usize)));
        }
    });
}

/// "reply" and "copy-id" of the buttons under a message. True when it was one of them.
pub(crate) fn act(app: &AppWindow, kind: &str, index: usize) -> bool {
    match kind {
        "reply" => reply_to(app, index, None),
        // Under a plan: leave plan mode and go on with it.
        "run-plan" => {
            app.invoke_submit("/plan off".into());
            app.invoke_submit("計画を実行してください。実行を始めてよいです(前のターンの「まだ実行しない」は解除します)。".into());
        }
        "copy-link" => {
            let model = app.global::<AppState>().get_messages();
            let thread = STATE.with(|s| s.borrow().active.clone());
            if let Some(m) = model.row_data(index).filter(|m| !m.ref_id.is_empty() && !thread.is_empty()) {
                if let Err(e) = arboard::Clipboard::new().and_then(|mut c| c.set_text(link_of(&thread, m.ref_id.as_str()))) {
                    status(app, &format!("コピーできませんでした: {e}"));
                }
            }
        }
        "copy-id" => {
            let model = app.global::<AppState>().get_messages();
            if let Some(m) = model.row_data(index) {
                if let Err(e) = arboard::Clipboard::new().and_then(|mut c| c.set_text(m.ref_id.to_string())) {
                    status(app, &format!("コピーできませんでした: {e}"));
                }
            }
        }
        _ => return false,
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_marker_is_the_start_of_the_message_on_one_line() {
        assert_eq!(hint_of("SQLite の並行書き込みについて、WAL を使うと読み取りが止まりません"), "SQLite の並行書き込みについて、W…");
        assert_eq!(hint_of("a\n\n  b\tc"), "a b c");
        assert_eq!(hint_of("短い"), "短い");
        // the characters of the format are not in a marker
        assert_eq!(hint_of("「引用」 [x]"), "引用 x");
        assert_eq!(hint_of(&"あ".repeat(20)), "あ".repeat(20));
        assert_eq!(hint_of(&"あ".repeat(21)), format!("{}…", "あ".repeat(20)));
    }

    fn r(id: &str, hint: &str, thread: &str, range: &str) -> Ref {
        Ref { id: id.into(), hint: hint.into(), thread: thread.into(), range: range.into() }
    }

    #[test]
    fn quote_lines_are_written_and_read_back() {
        let chips = vec![r("#12.3", "", "thr-1", "40-95"), r("#2.1", "", "thr-1", ""), r("#4.@2", "", "thr_sender", "")];
        let sent = with_refs(&chips, "ありがとう\n続き");
        assert_eq!(sent, "[引用 #12.3 thread:thr-1 40-95]\n[引用 #2.1 thread:thr-1]\n[引用 #4.@2 thread:thr_sender]\nありがとう\n続き");
        let (refs, body) = parse_ref_lines(&sent);
        assert_eq!(refs, chips);
        assert_eq!(body, "ありがとう\n続き");
        assert_eq!(with_refs(&[], "text"), "text");
        assert_eq!(ref_line(&r("#1.1", "", "", "")), "[引用 #1.1]");
        // no quote lines
        assert_eq!(parse_ref_lines("hello\n[引用 #1.1]"), (Vec::<Ref>::new(), "hello\n[引用 #1.1]".to_string()));
        assert_eq!(parse_ref_lines("[引用 #1.1]").1, "");
        // something that only looks like one
        assert!(parse_ref_line("[引用 #a.1]").is_none());
        assert!(parse_ref_line("[引用 #1.]").is_none());
        assert!(parse_ref_line("[引用 #1.1 x]").is_none());
        assert!(parse_ref_line("[引用 #1.1 thread:]").is_none());
        assert!(parse_ref_line("[引用 #1.1 40-]").is_none());
        assert!(parse_ref_line("[引用 #1.1x]").is_none());
    }

    #[test]
    fn the_old_reply_lines_are_still_read() {
        assert_eq!(parse_ref_line("[返信 #10.22「a」b」]"), Some(r("#10.22", "a」b", "", "")));
        assert_eq!(parse_ref_line("[返信 #3.@1「依頼」 thread:abc-1]"), Some(r("#3.@1", "依頼", "abc-1", "")));
        assert!(parse_ref_line("[返信 #a.1「x」]").is_none());
        assert!(parse_ref_line("[返信 #1.「x」]").is_none());
        assert!(parse_ref_line("[返信 #1.1 x]").is_none());
        assert!(parse_ref_line("[返信 #3.@「x」]").is_none());
        assert!(parse_ref_line("[返信 #3.@1「x」 thread:]").is_none());
        assert!(parse_ref_line("[返信 #3.@1「x」 thread:a b]").is_none());
        assert!(parse_ref_line("[返信 #3.@1「x」 other]").is_none());
        assert_eq!(parse_ref_line("[返信 #12.3 thread:abc]"), Some(r("#12.3", "", "abc", "")));
        assert_eq!(parse_ref_line("[返信 #12.@2]"), Some(r("#12.@2", "", "", "")));
        assert!(parse_ref_line("[返信 #12.3 abc]").is_none());
    }

    #[test]
    fn a_message_has_a_link_and_a_pasted_link_becomes_a_card() {
        assert_eq!(link_of("thr-1", "#12.3"), "inocode://thread/thr-1#12.3");
        let (rest, refs) = take_links("見て inocode://thread/019db3ea-fe9e#1.2 と inocode://thread/abc#3.1 です").unwrap();
        assert_eq!(rest, "見て  と  です");
        assert_eq!(refs, vec![r("#1.2", "", "019db3ea-fe9e", ""), r("#3.1", "", "abc", "")]);
        assert_eq!(take_links("inocode://thread/abc#4.2").unwrap().0, "");
        // not a link of a message
        assert!(take_links("inocode://thread/abc").is_none());
        assert!(take_links("inocode://thread/#1.1").is_none());
        assert!(take_links("plain text").is_none());
    }

    #[test]
    fn a_card_shows_three_lines_and_a_range_counts_characters() {
        assert_eq!(excerpt_lines("a\n\nb\nc\nd"), vec!["a", "b", "c"]);
        assert_eq!(excerpt_lines(&"あ".repeat(70))[0], format!("{}…", "あ".repeat(64)));
        let text = "ab日本語cd";
        // bytes 2..11 are the three Japanese characters
        assert_eq!(char_range(text, 2, 11), (2, 5));
        assert_eq!(slice_chars(text, 2, 5), "日本語");
    }

    #[test]
    fn rows_of_the_screen_find_their_place_in_the_thread() {
        let row = |r: &str, t: &str| (r.to_string(), t.to_string());
        let thread = vec![row("user", "q1"), row("assistant", "a1"), row("user", "q2"), row("assistant", "a2")];
        // a status line in between, and the last answer not in the thread yet
        let rows = vec![row("user", "q1"), row("assistant", "a1"), row("tool", "status"), row("user", "q2"), row("assistant", "a2"), row("assistant", "new")];
        assert_eq!(align(&rows, &thread), vec![Some(0), Some(1), None, Some(2), Some(3), None]);
        // the same text twice: in order
        let twice = vec![row("user", "ok"), row("assistant", "x"), row("user", "ok")];
        let th = vec![row("user", "ok"), row("assistant", "x"), row("user", "ok")];
        assert_eq!(align(&twice, &th), vec![Some(0), Some(1), Some(2)]);
    }

    #[test]
    fn the_server_gives_the_number() {
        let v = json!({ "turnNumber": 12, "itemNumber": 3, "text": "x" });
        assert_eq!(number_of(&v), Some("#12.3".to_string()));
        assert_eq!(number_of(&json!({ "response": { "turnNumber": 1, "itemNumber": 2 } })), Some("#1.2".to_string()));
        assert_eq!(number_of(&json!({})), None);
    }
}
