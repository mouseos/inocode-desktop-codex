//! The supervisor (reconnect loop) and the session state machine.

use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};
use super::*;

// --------------------------------------------------------------- supervisor

pub(super) enum End {
    Quit,
    Switch,
    Lost(String),
}

pub(super) fn transport_name(cfg: &Config) -> String {
    if cfg.use_stdio { "stdio" } else { "WS" }.to_string()
}

pub(super) fn supervise(mut cfg: Config, rx: Receiver<Command>, on_event: impl Fn(Event)) {
    let mut active_thread: Option<String> = None;
    let mut settings = Settings::default();
    let mut attempt: u32 = 0;
    loop {
        on_event(Event::Transport(transport_name(&cfg)));
        let status = if attempt == 0 { "接続中…".to_string() } else { format!("再接続中 ({attempt})…") };
        on_event(Event::Status(status, Level::Info));

        let end = match Conn::open(&cfg) {
            Ok(conn) => {
                let mut session = Session::new(conn, &mut active_thread, &mut settings, &on_event);
                let end = session.run(&rx, &mut cfg);
                if session.ready {
                    attempt = 0;
                }
                end
            }
            Err(e) => End::Lost(e),
        };
        match end {
            End::Quit => return,
            End::Switch => {
                attempt = 0;
                continue;
            }
            End::Lost(reason) => {
                attempt += 1;
                let delay = Duration::from_secs((1u64 << attempt.min(4)).min(10));
                on_event(Event::Status(
                    format!("未接続: {reason} ({}秒後に再接続)", delay.as_secs()),
                    Level::Error,
                ));
                on_event(Event::TurnDone { interrupted: false });
                let deadline = Instant::now() + delay;
                while Instant::now() < deadline {
                    match rx.recv_timeout(Duration::from_millis(100)) {
                        Ok(Command::SwitchTransport) => {
                            cfg.use_stdio = !cfg.use_stdio;
                            attempt = 0;
                            break;
                        }
                        Ok(Command::Quit) => return,
                        Ok(Command::Reconnect) => {
                            attempt = 0;
                            break;
                        }
                        Ok(Command::Prompt(_)) => on_event(Event::Error("未接続のため送信できません".into())),
                        Ok(Command::Rpc { tag, method, params }) => {
                            if !mock_rpc(&on_event, &tag, &method, &params) {
                                on_event(Event::Rpc { tag, result: Err("未接続です".into()) });
                            }
                        }
                        Ok(_) => {}
                        Err(mpsc::RecvTimeoutError::Timeout) => {}
                        Err(mpsc::RecvTimeoutError::Disconnected) => return,
                    }
                }
            }
        }
    }
}

// ------------------------------------------------------------------ session

#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) enum Pending {
    Init,
    ThreadList,
    ThreadStart,
    ThreadResume,
    TurnStart,
    /// The thread that makes the title of another (ephemeral), and its one turn.
    TitleThread,
    TitleTurn,
    /// thread/name/set of a made title: a failure leaves the name as it was, quietly.
    TitleName,
    Models,
    Requirements,
    Config,
    SettingsUpdate,
    Daybreak,
    Account,
    RateLimits,
    Login,
    Other,
}

pub(super) struct Session<'a, F: Fn(Event)> {
    pub(super) conn: Conn,
    pub(super) on_event: &'a F,
    pub(super) active_thread: &'a mut Option<String>,
    pub(super) settings: &'a mut Settings,
    /// Settings already pushed to the thread (only changes are sent).
    pub(super) sent: Settings,
    pub(super) settings_update_unsupported: bool,
    pub(super) next_id: u64,
    pub(super) pending: HashMap<u64, Pending>,
    /// Tags of Command::Rpc requests in flight, by request id.
    pub(super) rpc_tags: HashMap<u64, String>,
    pub(super) turn_id: Option<String>,
    pub(super) delta: String,
    pub(super) last_flush: Instant,
    pub(super) approvals: HashMap<String, (Value, String, Value)>,
    /// `changes` of the file-change items seen so far, by item id (an approval shows them).
    pub(super) file_changes: HashMap<String, Value>,
    /// Working folder of the active thread as the server reports it (sent with every turn).
    pub(super) thread_cwd: Option<String>,
    pub(super) msg_streamed: bool,
    pub(super) login_pending: bool,
    /// When model/list was last requested (the desktop client treats it as stale after 5 minutes).
    pub(super) models_asked: Instant,
    pub(super) ready: bool,
    /// The thread that has no title yet (its first message makes one).
    pub(super) untitled: Option<String>,
    /// Threads a title was asked for in this connection.
    pub(super) titled: std::collections::HashSet<String>,
    pub(super) title: Option<TitleJob>,
    /// The refusals of the automatic approval review (notification params), newest last.
    pub(super) denials: Vec<Value>,
    /// The turn that was running when a message was sent (a turn/start answered with that very turn
    /// was added to it).
    pub(super) turn_at_send: Option<String>,
}

/// A title being made by a short-lived thread of a light model (Codex Desktop: generateTitle).
pub(super) struct TitleJob {
    pub(super) target: String,
    pub(super) text: String,
    pub(super) thread: Option<String>,
    pub(super) buf: String,
}

impl<'a, F: Fn(Event)> Session<'a, F> {
    pub(super) fn new(
        conn: Conn,
        active_thread: &'a mut Option<String>,
        settings: &'a mut Settings,
        on_event: &'a F,
    ) -> Self {
        Session {
            conn,
            on_event,
            active_thread,
            sent: settings.clone(),
            settings,
            settings_update_unsupported: false,
            next_id: 1,
            pending: HashMap::new(),
            rpc_tags: HashMap::new(),
            turn_id: None,
            delta: String::new(),
            last_flush: Instant::now(),
            approvals: HashMap::new(),
            file_changes: HashMap::new(),
            thread_cwd: None,
            msg_streamed: false,
            login_pending: false,
            models_asked: Instant::now(),
            ready: false,
            untitled: None,
            titled: Default::default(),
            title: None,
            denials: Vec::new(),
            turn_at_send: None,
        }
    }

    pub(super) fn request(&mut self, method: &str, params: Value, kind: Pending) -> Result<(), String> {
        // The one place every turn passes: without a chosen model the server's own default (the
        // costly one) would be used, so no turn starts.
        // (The turn that makes a title runs in a thread of its own model.)
        if method == "turn/start" && kind != Pending::TitleTurn && params.get("model").and_then(Value::as_str).map_or(true, str::is_empty) {
            (self.on_event)(Event::Error("モデルを選んでください".into()));
            return Ok(());
        }
        let id = self.next_id;
        self.next_id += 1;
        self.pending.insert(id, kind);
        // INOCODE_GUI_RPC_LOG=<file>: what is asked of the server, to read what a run sent.
        if let Ok(path) = std::env::var("INOCODE_GUI_RPC_LOG") {
            use std::io::Write;
            if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
                let _ = writeln!(f, "{method} {}", params.to_string().chars().take(1500).collect::<String>());
            }
        }
        // Like the desktop client, leave "params" out when there are none.
        if params.is_null() {
            self.conn.send(json!({ "id": id, "method": method }))
        } else {
            self.conn.send(json!({ "id": id, "method": method, "params": params }))
        }
    }

    pub(super) fn flush(&mut self) {
        if !self.delta.is_empty() {
            (self.on_event)(Event::Delta(std::mem::take(&mut self.delta)));
        }
        self.last_flush = Instant::now();
    }

    pub(super) fn run(&mut self, rx: &Receiver<Command>, cfg: &mut Config) -> End {
        match self.run_inner(rx, cfg) {
            Ok(end) => end,
            Err(e) => End::Lost(e),
        }
    }

    pub(super) fn run_inner(&mut self, rx: &Receiver<Command>, cfg: &mut Config) -> Result<End, String> {
        self.request(
            "initialize",
            json!({ "clientInfo": {
                "name": "inocode_desktop",
                "title": "InoCode Desktop",
                "version": env!("CARGO_PKG_VERSION"),
            }, "capabilities": { "experimentalApi": true } }),
            Pending::Init,
        )?;

        loop {
            loop {
                match rx.try_recv() {
                    Ok(cmd) => {
                        if let Some(end) = self.command(cmd, cfg)? {
                            return Ok(end);
                        }
                    }
                    Err(mpsc::TryRecvError::Empty) => break,
                    Err(mpsc::TryRecvError::Disconnected) => return Ok(End::Quit),
                }
            }

            if let Some(text) = self.conn.recv()? {
                if let Ok(value) = serde_json::from_str::<Value>(&text) {
                    self.handle(&value)?;
                }
            }

            if !self.delta.is_empty() && self.last_flush.elapsed() >= FLUSH_INTERVAL {
                self.flush();
            }

            // Newly released models show up without a restart: ask again when stale.
            if self.ready && self.models_asked.elapsed() >= MODELS_STALE {
                self.refresh_models()?;
            }
        }
    }

    pub(super) fn command(&mut self, cmd: Command, cfg: &mut Config) -> Result<Option<End>, String> {
        match cmd {
            Command::SwitchTransport => {
                cfg.use_stdio = !cfg.use_stdio;
                return Ok(Some(End::Switch));
            }
            Command::Quit => return Ok(Some(End::Quit)),
            Command::Rpc { tag, method, params } => {
                if mock_rpc(self.on_event, &tag, &method, &params) {
                    return Ok(None);
                }
                let id = self.next_id;
                self.rpc_tags.insert(id, tag);
                self.request(&method, params, Pending::Other)?;
            }
            Command::Prompt(text) => {
                let Some(tid) = self.active_thread.clone() else {
                    (self.on_event)(Event::Error("スレッドがまだ開始されていません".into()));
                    return Ok(None);
                };
                let mut params = json!({ "threadId": tid, "input": [{ "type": "text", "text": text, "text_elements": [] }] });
                self.turn_params(&mut params);
                self.turn_at_send = self.turn_id.clone();
                self.request("turn/start", params, Pending::TurnStart)?;
                self.make_title(&tid, &text)?;
            }
            Command::PromptWith(text, extra) => {
                let Some(tid) = self.active_thread.clone() else {
                    (self.on_event)(Event::Error("スレッドがまだ開始されていません".into()));
                    return Ok(None);
                };
                let mut input = vec![json!({ "type": "text", "text": text, "text_elements": [] })];
                input.extend(extra);
                let mut params = json!({ "threadId": tid, "input": input });
                self.turn_params(&mut params);
                self.turn_at_send = self.turn_id.clone();
                self.request("turn/start", params, Pending::TurnStart)?;
                self.make_title(&tid, &text)?;
            }
            Command::NewThread => {
                let params = thread_start_params(self.settings);
                self.request("thread/start", params, Pending::ThreadStart)?;
            }
            Command::NewThreadIn(cwd) => {
                self.settings.cwd = Some(cwd);
                let params = thread_start_params(self.settings);
                self.request("thread/start", params, Pending::ThreadStart)?;
            }
            Command::SetSettings(s) => {
                // What the server told us stays; the screen only sends its own choices.
                let keep = (
                    self.settings.server_policy.take(),
                    self.settings.server_sandbox.take(),
                    self.settings.allowed_policies.take(),
                    self.settings.allowed_sandboxes.take(),
                    self.settings.summary.take(),
                );
                *self.settings = s;
                (
                    self.settings.server_policy,
                    self.settings.server_sandbox,
                    self.settings.allowed_policies,
                    self.settings.allowed_sandboxes,
                    self.settings.summary,
                ) = keep;
                self.push_settings()?;
            }
            Command::Reconnect => return Ok(Some(End::Switch)),
            Command::SetDaybreak(on) => {
                self.settings.daybreak = on;
                if let Some(tid) = self.active_thread.clone() {
                    self.request(
                        "thread/metadata/update",
                        json!({ "threadId": tid, "daybreakEnabled": on }),
                        Pending::Daybreak,
                    )?;
                }
            }
            Command::Login => {
                // One ChatGPT login at a time, as in the desktop client.
                if !self.login_pending {
                    self.login_pending = true;
                    self.request("account/login/start", json!({ "type": "chatgpt" }), Pending::Login)?;
                }
            }
            Command::OpenThread(id) => {
                self.request("thread/resume", json!({ "threadId": id }), Pending::ThreadResume)?;
            }
            Command::Interrupt => {
                if let (Some(tid), Some(turn)) = (self.active_thread.clone(), self.turn_id.clone()) {
                    self.request("turn/interrupt", json!({ "threadId": tid, "turnId": turn }), Pending::Other)?;
                }
            }
            Command::Steer { key, text } => {
                let tag = format!("prompt.steer:{key}");
                match (self.active_thread.clone(), self.turn_id.clone()) {
                    (Some(tid), Some(turn)) => {
                        let input = json!([{ "type": "text", "text": text, "text_elements": [] }]);
                        // The answer (or the error) goes to the screen: a message that was not
                        // taken must not be lost.
                        let id = self.next_id;
                        self.rpc_tags.insert(id, tag);
                        self.request(
                            "turn/steer",
                            json!({ "threadId": tid, "expectedTurnId": turn, "input": input }),
                            Pending::Other,
                        )?;
                    }
                    _ => (self.on_event)(Event::Rpc { tag, result: Err("実行中のターンがありません".into()) }),
                }
            }
            Command::ApproveDenied => {
                let tag = "slash.approve".to_string();
                match (self.active_thread.clone(), self.denials.pop()) {
                    (Some(tid), Some(p)) => {
                        let id = self.next_id;
                        self.rpc_tags.insert(id, tag);
                        // By the review's id: the server kept the refused event (a server without this call answers
        // "method not found" and the error is shown).
                        self.request("thread/approveGuardianDeniedActionById", approve_by_id_params(&tid, &p), Pending::Other)?;
                    }
                    _ => (self.on_event)(Event::Rpc { tag, result: Err("承認できる拒否はありません".into()) }),
                }
            }
            Command::Answer { key, answers } => {
                if let Some((id, method, params)) = self.approvals.remove(&key) {
                    if method == "mcpServer/elicitation/request" {
                        let result = approval::elicitation_result(&params, &answers);
                        self.conn.send(json!({ "id": id, "result": result }))?;
                        (self.on_event)(Event::ApprovalResolved(key));
                        return Ok(None);
                    }
                    let mut map = serde_json::Map::new();
                    for (qid, list) in answers {
                        map.insert(qid, json!({ "answers": list }));
                    }
                    self.conn.send(json!({ "id": id, "result": { "answers": map } }))?;
                    (self.on_event)(Event::ApprovalResolved(key));
                }
            }
            Command::Approve { key, decision } => {
                if let Ok(path) = std::env::var("INOCODE_GUI_RPC_LOG") {
                    use std::io::Write;
                    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
                        let _ = writeln!(f, "approve key={key} decision={decision:?} known={} pending={:?}", self.approvals.contains_key(&key), self.approvals.keys().collect::<Vec<_>>());
                    }
                }
                if let Some((id, method, params)) = self.approvals.remove(&key) {
                    let result = approval_result(&method, &params, decision);
                    self.conn.send(json!({ "id": id, "result": result }))?;
                    (self.on_event)(Event::ApprovalResolved(key));
                }
            }
        }
        Ok(None)
    }

    /// Fields that go with every turn: model, effort, speed, the permissions (approval policy and
    /// sandbox, so that a write does not depend on the folder's trust setting) and the folder.
    fn turn_params(&self, params: &mut Value) {
        turn_overrides(self.settings, true, params);
        if let Some(cwd) = &self.thread_cwd {
            params["cwd"] = json!(cwd);
        }
    }

    /// Push changed choices to the thread (thread/settings/update), as the desktop client does.
    pub(super) fn push_settings(&mut self) -> Result<(), String> {
        let Some(tid) = self.active_thread.clone() else { return Ok(()) };
        if self.settings_update_unsupported {
            return Ok(());
        }
        let new = self.settings.clone();
        let old = self.sent.clone();
        let mut p = json!({ "threadId": tid });
        let mut any = false;
        if new.model != old.model {
            if let Some(m) = &new.model {
                p["model"] = json!(m);
                any = true;
            }
        }
        if new.effort != old.effort {
            if let Some(e) = &new.effort {
                p["effort"] = json!(e);
                any = true;
            }
        }
        if new.service_tier != old.service_tier {
            if let Some(t) = &new.service_tier {
                p["serviceTier"] = match t {
                    Some(id) => json!(id),
                    None => Value::Null,
                };
                any = true;
            }
        }
        if new.permission(false) != old.permission(false) {
            let (policy, sandbox, reviewer) = new.permission(false);
            if let Some(v) = policy {
                p["approvalPolicy"] = json!(v);
                any = true;
            }
            if let Some(v) = sandbox.as_deref().and_then(sandbox_policy) {
                p["sandboxPolicy"] = v;
                any = true;
            }
            if let Some(v) = reviewer {
                p["approvalsReviewer"] = json!(v);
                any = true;
            }
        }
        self.sent = new;
        if any {
            self.request("thread/settings/update", p, Pending::SettingsUpdate)?;
        }
        Ok(())
    }

    pub(super) fn refresh_models(&mut self) -> Result<(), String> {
        self.models_asked = Instant::now();
        self.request(
            "model/list",
            json!({ "includeHidden": true, "cursor": null, "limit": 100 }),
            Pending::Models,
        )
    }

    pub(super) fn list_threads(&mut self) -> Result<(), String> {
        self.request("thread/list", json!({ "limit": 50, "modelProviders": [] }), Pending::ThreadList)
    }

    pub(super) fn handle(&mut self, value: &Value) -> Result<(), String> {
        let id = value.get("id");
        let method = value.get("method").and_then(Value::as_str);

        // Request from the server.
        if let (Some(id), Some(method)) = (id, method) {
            return self.server_request(id.clone(), method, value.get("params").cloned().unwrap_or(Value::Null));
        }
        // Response to one of ours.
        if let Some(id) = id.and_then(Value::as_u64) {
            if let Some(tag) = self.rpc_tags.remove(&id) {
                self.pending.remove(&id);
                let result = match value.get("error") {
                    Some(err) => Err(err.get("message").and_then(Value::as_str).unwrap_or("error").to_string()),
                    None => Ok(value.get("result").cloned().unwrap_or(Value::Null)),
                };
                (self.on_event)(Event::Rpc { tag, result });
                return Ok(());
            }
            let kind = self.pending.remove(&id).unwrap_or(Pending::Other);
            return self.response(kind, value);
        }
        if let Some(method) = method {
            self.notification(method, value.get("params").unwrap_or(&Value::Null));
        }
        Ok(())
    }

    pub(super) fn response(&mut self, kind: Pending, value: &Value) -> Result<(), String> {
        if let Some(err) = value.get("error") {
            let message = err.get("message").and_then(Value::as_str).unwrap_or("error").to_string();
            match kind {
                // Listing is optional; an older server may not know the method.
                Pending::ThreadList | Pending::Other | Pending::Models | Pending::RateLimits | Pending::Account => {}
                Pending::Init => return Err(format!("initialize が失敗しました: {message}")),
                Pending::Login => {
                    self.login_pending = false;
                    self.report_error(&message);
                }
                Pending::SettingsUpdate => {
                    // Like the desktop client: remember that the server lacks the method and
                    // send the choices with every turn instead.
                    let m = message.to_ascii_lowercase();
                    if m.contains("method not found") || m.contains("unknown variant") || m.contains("unsupported") {
                        self.settings_update_unsupported = true;
                    } else {
                        self.report_error(&message);
                    }
                }
                // Not available on this server: go on without it.
                Pending::Requirements => return self.request("config/read", json!({ "includeLayers": false }), Pending::Config),
                Pending::Config => return self.begin_thread(),
                Pending::Daybreak => {}
                // A new session that never got a message is not saved: after the server restarted
                // it is gone. Start a new one instead of showing the error.
                Pending::ThreadResume if message.contains("no rollout found") || message.contains("thread not loaded") => {
                    *self.active_thread = None;
                    self.untitled = None;
                    self.turn_id = None;
                    let params = thread_start_params(self.settings);
                    return self.request("thread/start", params, Pending::ThreadStart);
                }
                // No title: the first words of the message stay as it.
                Pending::TitleThread | Pending::TitleTurn | Pending::TitleName => {
                    super::notify::rpc_log(&format!("title error: {message}"));
                    self.title = None;
                }
                _ => self.report_error(&message),
            }
            return Ok(());
        }
        let result = value.get("result").unwrap_or(&Value::Null);
        match kind {
            Pending::Init => {
                self.conn.send(json!({ "method": "initialized" }))?;
                self.ready = true;
                self.refresh_models()?;
                self.request("account/read", json!({ "refreshToken": false }), Pending::Account)?;
                self.request("account/rateLimits/read", Value::Null, Pending::RateLimits)?;
                self.request("configRequirements/read", Value::Null, Pending::Requirements)?;
                self.list_threads()?;
                // Requirements, then the configuration, then the thread: its permissions and
                // folder are sent explicitly, so they have to be known first.
            }
            Pending::ThreadList => (self.on_event)(Event::Threads(parse_threads(result))),
            Pending::ThreadStart => {
                let tid = result
                    .pointer("/thread/id")
                    .and_then(Value::as_str)
                    .ok_or("thread/start の応答に thread.id がありません")?;
                *self.active_thread = Some(tid.to_string());
                self.untitled = Some(tid.to_string());
                self.turn_id = None;
                self.thread_cwd = result.get("cwd").and_then(Value::as_str).filter(|c| !c.is_empty()).map(str::to_string);
                (self.on_event)(Event::ThreadActive { id: tid.to_string(), title: String::new() });
                (self.on_event)(applied(result));
                (self.on_event)(Event::History(Vec::new()));
                (self.on_event)(Event::Status("接続済み".into(), Level::Ok));
                self.list_threads()?;
            }
            Pending::ThreadResume => {
                let thread = result.get("thread").unwrap_or(&Value::Null);
                if let Some(tid) = thread.get("id").and_then(Value::as_str) {
                    *self.active_thread = Some(tid.to_string());
                    self.untitled = thread
                        .get("name")
                        .and_then(Value::as_str)
                        .map(str::trim)
                        .filter(|n| !n.is_empty())
                        .is_none()
                        .then(|| tid.to_string());
                    self.turn_id = None;
                    (self.on_event)(Event::ThreadActive { id: tid.to_string(), title: thread_title(thread) });
                }
                self.thread_cwd = result.get("cwd").and_then(Value::as_str).filter(|c| !c.is_empty()).map(str::to_string);
                (self.on_event)(applied(result));
                (self.on_event)(Event::History(history_from_thread(thread)));
                (self.on_event)(Event::Status("接続済み".into(), Level::Ok));
                // A turn that is still running (the thread was opened from elsewhere): it can be stopped.
                if let Some(turn) = running_turn(thread) {
                    self.turn_id = Some(turn);
                    (self.on_event)(Event::TurnStarted);
                }
            }
            Pending::Models => {
                let list = parse_models(result);
                self.settings.programs = list.clone();
                (self.on_event)(Event::Models(list));
            }
            Pending::Account => {
                let (logged_in, label) = parse_account(result);
                (self.on_event)(Event::Account { logged_in, label });
            }
            Pending::RateLimits => {
                if let Some(text) = rate_limit_text(result.get("rateLimits").unwrap_or(&Value::Null)) {
                    (self.on_event)(Event::RateLimits(text));
                }
            }
            Pending::Requirements => {
                let req = result.get("requirements").unwrap_or(&Value::Null);
                let list = |key: &str| {
                    req.get(key).and_then(Value::as_array).map(|a| {
                        a.iter().filter_map(Value::as_str).map(str::to_string).collect::<Vec<_>>()
                    })
                };
                self.settings.allowed_policies = list("allowedApprovalPolicies");
                self.settings.allowed_sandboxes = list("allowedSandboxModes");
                (self.on_event)(Event::Requirements {
                    policies: list("allowedApprovalPolicies"),
                    sandboxes: list("allowedSandboxModes"),
                });
                self.request("config/read", json!({ "includeLayers": false }), Pending::Config)?;
            }
            Pending::Config => {
                let cfg = result.get("config").unwrap_or(&Value::Null);
                let text = |k: &str| cfg.get(k).and_then(Value::as_str).map(str::to_string);
                self.settings.server_policy = text("approval_policy");
                self.settings.server_sandbox = text("sandbox_mode");
                self.settings.summary = text("model_reasoning_summary");
                self.begin_thread()?;
            }
            Pending::SettingsUpdate => {}
            Pending::Daybreak => {
                if let Some(on) = result.pointer("/thread/daybreakEnabled").and_then(Value::as_bool) {
                    self.settings.daybreak = on;
                    (self.on_event)(Event::Daybreak(on));
                }
            }
            Pending::Login => {
                if let Some(url) = result.get("authUrl").and_then(Value::as_str) {
                    open_url(url);
                    (self.on_event)(Event::Status("ブラウザでログインしてください".into(), Level::Warn));
                }
            }
            Pending::TurnStart => {
                // The turn is known from the answer, not only from the notification (a message
                // sent in between would not find it).
                if let Some(turn) = result.pointer("/turn/id").and_then(Value::as_str) {
                    // The answer names the turn that was already running: the message was added to it
                    // (steer), and the mode of that turn stays.
                    if self.turn_at_send.take().as_deref() == Some(turn) {
                        (self.on_event)(Event::Note { id: format!("steered:{turn}"), text: "前のターンが実行中のため、そのターンに追加されました".into(), detail: String::new() });
                    }
                    if self.turn_id.is_none() {
                        self.turn_id = Some(turn.to_string());
                    }
                }
            }
            Pending::TitleThread => {
                let thread = result.pointer("/thread/id").and_then(Value::as_str).map(str::to_string);
                let text = self.title.as_ref().map(|j| j.text.clone());
                match (thread, text) {
                    (Some(t), Some(text)) => {
                        if let Some(j) = self.title.as_mut() {
                            j.thread = Some(t.clone());
                        }
                        let prompt = format!(
                            "次の依頼の内容を表す、短いタイトルを付けてください。依頼と同じ言語で、20 文字ほど。記号や引用符は付けず、\
                             title だけを JSON で返してください。\n\n依頼:\n{text}"
                        );
                        let params = json!({
                            "threadId": t,
                            "input": [{ "type": "text", "text": prompt, "text_elements": [] }],
                            "outputSchema": {
                                "type": "object",
                                "properties": { "title": { "type": "string" } },
                                "required": ["title"],
                                "additionalProperties": false
                            }
                        });
                        self.request("turn/start", params, Pending::TitleTurn)?;
                    }
                    _ => self.title = None,
                }
            }
            Pending::TitleTurn | Pending::TitleName | Pending::Other => {}
        }
        Ok(())
    }

    /// Open the thread that was active (after a reconnect) or start a new one.
    pub(super) fn begin_thread(&mut self) -> Result<(), String> {
        match self.active_thread.clone() {
            Some(tid) => self.request("thread/resume", json!({ "threadId": tid }), Pending::ThreadResume),
            None => {
                let params = thread_start_params(self.settings);
                self.request("thread/start", params, Pending::ThreadStart)
            }
        }
    }

    pub(super) fn report_error(&mut self, message: &str) {
        if is_auth_error(message) {
            (self.on_event)(Event::Status("認証が必要です (401)".into(), Level::Error));
        }
        (self.on_event)(Event::Error(message.to_string()));
    }

}

