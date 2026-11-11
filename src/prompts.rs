//! Questions from the agent, the follow-up queue while a turn runs, the error banner and compaction.
//!
//! Official: item/tool/requestUserInput, mcpServer/elicitation/request, turn/steer,
//! thread/queue/* (experimental), thread/compact/start, the `error` notification.
//! Claude Desktop: "Claude is asking: …" with Skip / Submit, Queued messages with Send now /
//! Edit in composer / Remove from queue, recovery buttons on failed turns, Compact this session
//! (c96d752d0, cf74bc599, c227f4d5e, c3e34355f).

use crate::state::*;
use crate::*;
use client::QuestionInfo;
use serde_json::{json, Value};

#[derive(Default)]
struct Data {
    qkey: String,
    items: Vec<QuestionInfo>,
    idx: usize,
    answers: Vec<(String, Vec<String>)>,
    last_prompt: String,
    /// (queued submission id, text) of the active thread.
    queue: Vec<(String, String)>,
    /// Messages sent while a turn runs that the server has not taken yet, or did not take. They
    /// stay on the screen (with a retry) until they are taken, edited or removed.
    outbox: Vec<Out>,
    next_out: u64,
}

struct Out {
    id: String,
    client_id: String,
    text: String,
    extra: Vec<Value>,
    failed: bool,
}

thread_local! {
    /// Codex: follow-up behavior "steer" (desktop.followUpQueueMode); the default is the queue.
    static FOLLOW_STEER: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static HANDLE: RefCell<Option<Rc<client::Handle>>> = const { RefCell::new(None) };
    static DATA: RefCell<Data> = RefCell::new(Data::default());
}

fn rpc(tag: &str, method: &str, params: Value) {
    if let Some(h) = HANDLE.with(|c| c.borrow().clone()) {
        h.send(Command::Rpc { tag: tag.to_string(), method: method.to_string(), params });
    }
}

fn active() -> Option<String> {
    STATE.with(|s| Some(s.borrow().active.clone()).filter(|a| !a.is_empty()))
}

fn input(text: &str) -> Value {
    json!([{ "type": "text", "text": text, "text_elements": [] }])
}

fn show_question(app: &AppWindow) {
    let s = app.global::<PromptState>();
    DATA.with(|d| {
        let d = d.borrow();
        match d.items.get(d.idx) {
            Some(q) => {
                s.set_question_header(q.header.as_str().into());
                s.set_question_text(q.question.as_str().into());
                s.set_question_options(strings(q.options.clone()));
                s.set_question_secret(q.secret);
                s.set_question_progress(if d.items.len() > 1 { format!("{} / {}", d.idx + 1, d.items.len()).into() } else { "".into() });
                s.set_question_open(true);
            }
            None => s.set_question_open(false),
        }
    });
}

/// A request for answers arrived.
pub(crate) fn on_questions(app: &AppWindow, key: String, items: Vec<QuestionInfo>) {
    DATA.with(|d| {
        let mut d = d.borrow_mut();
        d.qkey = key;
        d.items = items;
        d.idx = 0;
        d.answers.clear();
    });
    show_question(app);
}

/// The request was resolved (here or by the server).
pub(crate) fn on_resolved(app: &AppWindow, key: &str) {
    let mine = DATA.with(|d| d.borrow().qkey == key);
    if mine {
        DATA.with(|d| d.borrow_mut().qkey.clear());
        app.global::<PromptState>().set_question_open(false);
    }
}

fn record(app: &AppWindow, answer: Option<String>) {
    let done = DATA.with(|d| {
        let mut d = d.borrow_mut();
        let Some(q) = d.items.get(d.idx) else { return None };
        let id = q.id.clone();
        d.answers.push((id, answer.into_iter().collect()));
        d.idx += 1;
        if d.idx >= d.items.len() {
            Some((d.qkey.clone(), std::mem::take(&mut d.answers)))
        } else {
            None
        }
    });
    match done {
        Some((key, answers)) => {
            if let Some(h) = HANDLE.with(|c| c.borrow().clone()) {
                h.send(Command::Answer { key, answers });
            }
        }
        None => show_question(app),
    }
}

pub(crate) fn clear_banner(app: &AppWindow) {
    app.global::<PromptState>().set_banner_level(0);
}

pub(crate) fn show_banner(app: &AppWindow, text: &str, level: i32) {
    let s = app.global::<PromptState>();
    s.set_banner_text(text.lines().next().unwrap_or(text).into());
    s.set_banner_level(level);
}

pub(crate) fn set_follow_steer(on: bool) {
    FOLLOW_STEER.with(|f| f.set(on));
}

/// Called by the composer: send now, or hand to the running turn (steer or queue).
pub(crate) fn submit(app: &AppWindow, h: &client::Handle, text: String) {
    DATA.with(|d| d.borrow_mut().last_prompt = text.clone());
    let extra = crate::attach::take_inputs(app);
    if app.global::<AppState>().get_busy() {
        let id = DATA.with(|d| {
            let mut d = d.borrow_mut();
            d.next_out += 1;
            let id = format!("out-{}", d.next_out);
            d.outbox.push(Out { id: id.clone(), client_id: format!("ino-{}", now_nanos()), text: text.clone(), extra, failed: false });
            id
        });
        set_queue_model(app);
        send_busy(app, h, &id);
    } else if extra.is_empty() {
        h.send(Command::Prompt(text));
    } else {
        h.send(Command::PromptWith(text, extra));
    }
}

/// Hand an outbox message to the running turn: into it at once (steer), or behind it (queue).
fn send_busy(app: &AppWindow, h: &client::Handle, id: &str) {
    let Some((text, client_id, extra)) =
        DATA.with(|d| d.borrow().outbox.iter().find(|o| o.id == id).map(|o| (o.text.clone(), o.client_id.clone(), o.extra.clone())))
    else {
        return;
    };
    if FOLLOW_STEER.with(|f| f.get()) && extra.is_empty() {
        h.send(Command::Steer { key: id.to_string(), text });
    } else if let Some(tid) = active() {
        let mut inputs = input(&text);
        if let Some(a) = inputs.as_array_mut() {
            a.extend(extra);
        }
        h.send(Command::Rpc {
            tag: format!("prompt.queue.add:{id}"),
            method: "thread/queue/add".into(),
            params: json!({ "threadId": tid, "input": inputs, "clientUserMessageId": client_id }),
        });
    } else {
        on_sent(app, id, &Err("スレッドがまだ開始されていません".into()));
    }
}

fn set_failed(id: &str, failed: bool) {
    DATA.with(|d| {
        if let Some(o) = d.borrow_mut().outbox.iter_mut().find(|o| o.id == id) {
            o.failed = failed;
        }
    });
}

fn take_out(id: &str) -> Option<Out> {
    DATA.with(|d| {
        let mut d = d.borrow_mut();
        let i = d.outbox.iter().position(|o| o.id == id)?;
        Some(d.outbox.remove(i))
    })
}

fn now_nanos() -> u128 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0)
}

fn list_queue() {
    if let Some(tid) = active() {
        rpc("prompt.queue.list", "thread/queue/list", json!({ "threadId": tid }));
    }
}

fn set_queue_model(app: &AppWindow) {
    let rows: Vec<QueueRow> = DATA.with(|d| {
        let d = d.borrow();
        let queued = d.queue.iter().map(|(id, text)| QueueRow { id: id.as_str().into(), text: text.as_str().into(), state: "".into() });
        let out = d.outbox.iter().map(|o| QueueRow {
            id: o.id.as_str().into(),
            text: o.text.as_str().into(),
            state: if o.failed { "failed" } else { "sending" }.into(),
        });
        queued.chain(out).collect()
    });
    app.global::<PromptState>().set_queue(ModelRc::from(Rc::new(VecModel::from(rows))));
}

pub(crate) fn register(app: &AppWindow, handle: &Rc<client::Handle>) {
    HANDLE.with(|c| *c.borrow_mut() = Some(handle.clone()));
    let weak = app.as_weak();
    app.global::<PromptState>().on_answer(move |text| {
        if let Some(app) = weak.upgrade() {
            record(&app, Some(text.to_string()));
        }
    });
    let weak = app.as_weak();
    app.global::<PromptState>().on_skip_question(move || {
        if let Some(app) = weak.upgrade() {
            record(&app, None);
        }
    });
    let weak = app.as_weak();
    let h = handle.clone();
    app.global::<PromptState>().on_queue_act(move |kind, id| {
        let Some(app) = weak.upgrade() else { return };
        // A message that the server has not taken (yet): it is kept here until it is taken.
        if id.starts_with("out-") {
            let Some(o) = take_out(&id) else { return };
            match kind.as_str() {
                "retry" => {
                    if app.global::<AppState>().get_busy() {
                        DATA.with(|d| d.borrow_mut().outbox.push(Out { failed: false, ..o }));
                        set_queue_model(&app);
                        send_busy(&app, &h, &id);
                        return;
                    }
                    // The turn ended in between: it starts a turn of its own.
                    crate::view::messages_with(&app, |m| crate::view::push(m, "", "user", &o.text));
                    if o.extra.is_empty() {
                        h.send(Command::Prompt(o.text));
                    } else {
                        h.send(Command::PromptWith(o.text, o.extra));
                    }
                }
                "edit" => app.set_prompt(o.text.into()),
                _ => {}
            }
            set_queue_model(&app);
            return;
        }
        let Some(tid) = active() else { return };
        let text = DATA.with(|d| d.borrow().queue.iter().find(|(q, _)| *q == id.as_str()).map(|(_, t)| t.clone()));
        let Some(text) = text else { return };
        match kind.as_str() {
            // The queued message goes out of the queue only when the running turn took it.
            "now" => {
                h.send(Command::Steer { key: format!("now.{id}"), text });
                return;
            }
            "edit" => app.set_prompt(text.into()),
            _ => {}
        }
        rpc("prompt.queue.delete", "thread/queue/delete", json!({ "threadId": tid, "queuedSubmissionId": id.to_string() }));
    });
    let weak = app.as_weak();
    let h = handle.clone();
    app.global::<PromptState>().on_banner_retry(move || {
        let Some(app) = weak.upgrade() else { return };
        clear_banner(&app);
        // A running turn takes no new turn: its messages are retried on their own rows.
        if app.global::<AppState>().get_busy() {
            return;
        }
        let text = DATA.with(|d| d.borrow().last_prompt.clone());
        if !text.is_empty() {
            h.send(Command::Prompt(text));
        }
    });
    let weak = app.as_weak();
    app.global::<PromptState>().on_banner_close(move || {
        if let Some(app) = weak.upgrade() {
            clear_banner(&app);
        }
    });
    let weak = app.as_weak();
    app.global::<PromptState>().on_compact(move || {
        let Some(app) = weak.upgrade() else { return };
        if let Some(tid) = active() {
            app.global::<PromptState>().set_compacting(true);
            rpc("prompt.compact", "thread/compact/start", json!({ "threadId": tid }));
        }
    });
}

/// The answer to a message handed to the running turn.
fn on_sent(app: &AppWindow, id: &str, result: &Result<Value, String>) {
    match result {
        Ok(_) => {
            if let Some(o) = take_out(id) {
                // Into the running turn: it shows in the chat now. (A queued one shows when it starts.)
                if FOLLOW_STEER.with(|f| f.get()) && o.extra.is_empty() {
                    crate::view::messages_with(app, |m| crate::view::push(m, "", "user", &o.text));
                    app.invoke_scroll_bottom();
                }
            }
            list_queue();
        }
        Err(_) => set_failed(id, true),
    }
    set_queue_model(app);
}

pub(crate) fn on_rpc(app: &AppWindow, tag: &str, result: &Result<Value, String>) {
    if let Some(id) = tag.strip_prefix("prompt.queue.add:") {
        return on_sent(app, id, result);
    }
    if let Some(key) = tag.strip_prefix("prompt.steer:") {
        // "Send now" on a queued message: out of the queue once taken.
        if let Some(qid) = key.strip_prefix("now.") {
            match result {
                Ok(_) => {
                    let text = DATA.with(|d| d.borrow().queue.iter().find(|(q, _)| q == qid).map(|(_, t)| t.clone()));
                    if let (Some(text), Some(tid)) = (text, active()) {
                        crate::view::messages_with(app, |m| crate::view::push(m, "", "user", &text));
                        rpc("prompt.queue.delete", "thread/queue/delete", json!({ "threadId": tid, "queuedSubmissionId": qid }));
                    }
                }
                Err(e) => show_banner(app, &format!("送信できませんでした: {e}"), 1),
            }
            return;
        }
        return on_sent(app, key, result);
    }
    match tag {
        "prompt.queue.list" => {
            if let Ok(v) = result {
                let rows: Vec<(String, String)> = v
                    .get("data")
                    .and_then(Value::as_array)
                    .map(|a| {
                        a.iter()
                            .map(|q| {
                                let text = q.pointer("/input/0/text").and_then(Value::as_str).unwrap_or("").to_string();
                                (q.get("id").and_then(Value::as_str).unwrap_or("").to_string(), text)
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                DATA.with(|d| d.borrow_mut().queue = rows);
                set_queue_model(app);
            }
        }
        "prompt.queue.delete" => match result {
            Ok(_) => list_queue(),
            Err(e) => show_banner(app, &format!("キューを更新できません: {e}"), 1),
        },
        "prompt.compact" => {
            app.global::<PromptState>().set_compacting(false);
            if let Err(e) = result {
                show_banner(app, &format!("圧縮できません: {e}"), 1);
            }
        }
        _ => {}
    }
}

pub(crate) fn on_notify(method: &str) {
    if method == "thread/queue/changed" {
        list_queue();
    }
}

/// The turn ended: start the next queued message (a stopped turn leaves the queue alone).
pub(crate) fn on_turn_done(app: &AppWindow, thread: &str, interrupted: bool) {
    let first = DATA.with(|d| d.borrow().queue.first().cloned());
    if let Some((_, text)) = first.filter(|_| !interrupted && !thread.is_empty()) {
        crate::view::messages_with(app, |m| crate::view::push(m, "", "user", &text));
        rpc("prompt.queue.start", "thread/queue/start", json!({ "threadId": thread }));
    }
}

/// The snapshot entry INOCODE_GUI_SEND=1: messages sent while a turn runs, in the three states a
/// mock server's answers lead to (taken, not taken, sent again), made through the same functions
/// that the server's answers go through.
pub(crate) fn demo(app: &AppWindow) {
    FOLLOW_STEER.with(|f| f.set(true));
    app.global::<AppState>().set_busy(true);
    let add = |text: &str| {
        DATA.with(|d| {
            let mut d = d.borrow_mut();
            d.next_out += 1;
            let id = format!("out-{}", d.next_out);
            d.outbox.push(Out { id: id.clone(), client_id: String::new(), text: text.to_string(), extra: Vec::new(), failed: false });
            id
        })
    };
    crate::view::messages_with(app, |m| crate::view::push(m, "", "user", "テストを全部直して"));
    let ok = add("あわせて README も更新して");
    on_sent(app, &ok, &Ok(json!({ "turnId": "t1" })));
    let failed = add("さっきの修正は、元に戻して");
    on_sent(app, &failed, &Err("no active turn to steer".into()));
    add("もう一度送ったメッセージ");
    set_queue_model(app);
}
