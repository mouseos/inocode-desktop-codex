//! 設定の「MCP サーバー」の中身: 一覧(状態・認証・ツールの数)、有効・無効、追加、アンインストール、
//! OAuth の認証。構造と文言は Codex Desktop の設定(settings.mcp.*)に合わせる。
//!
//! サーバー: mcpServerStatus/list、config/value/write(mcp_servers.<名前> と .enabled。null で消える)、
//! config/mcpServer/reload、mcpServer/oauth/login と通知 mcpServer/oauthLogin/completed、
//! 通知 mcpServer/startupStatus/updated。

use crate::settings_page::{open_url, refresh_mcp, row, rpc};
use crate::{AppState, AppWindow, SRow};
use serde_json::{json, Map, Value};
use slint::{ComponentHandle, ModelRc, SharedString, VecModel};
use std::cell::RefCell;
use std::rc::Rc;

#[derive(Default)]
struct Form {
    /// 0 STDIO, 1 ストリーミング可能な HTTP
    kind: usize,
    name: String,
    command: String,
    args: String,
    env: String,
    url: String,
    bearer: String,
}

#[derive(Default)]
struct Data {
    form: Form,
    /// サーバー名 -> 認証の開始中
    logging_in: Vec<String>,
    /// config.toml の mcp_servers.<名前>.enabled(書いていなければ有効)
    enabled: Vec<(String, bool)>,
}

thread_local! {
    static D: RefCell<Data> = RefCell::new(Data::default());
}

fn s<'a>(v: &'a Value, k: &str) -> &'a str {
    v.get(k).and_then(Value::as_str).unwrap_or("")
}

fn runtime_label(v: &str) -> &str {
    match v.to_ascii_lowercase().as_str() {
        "connected" => "接続済み",
        "starting" => "起動中",
        "notstarted" => "未起動",
        "authenticationrequired" => "認証が必要",
        "failed" => "失敗",
        "cancelled" => "取り消し",
        "disabled" => "無効",
        _ => "",
    }
}

fn auth_label(v: &str) -> &str {
    match v.to_ascii_lowercase().as_str() {
        "oauth" => "OAuth",
        "bearertoken" => "Bearer",
        "notloggedin" => "未ログイン",
        _ => "",
    }
}

fn strings(v: Vec<&str>) -> ModelRc<SharedString> {
    ModelRc::from(Rc::new(VecModel::from(v.into_iter().map(SharedString::from).collect::<Vec<_>>())))
}

/// 画面の行: サーバーの一覧と、追加のフォーム。v は mcpServerStatus/list の応答。
pub fn rows(v: &Value) -> Vec<SRow> {
    let mut out: Vec<SRow> = vec![row("", "サーバー", "", "", 6, false)];
    let mut servers: Vec<&Value> = v.get("data").and_then(Value::as_array).into_iter().flatten().collect();
    servers.sort_by(|a, b| s(a, "name").cmp(s(b, "name")));
    let logging = D.with(|d| d.borrow().logging_in.clone());
    if servers.is_empty() {
        out.push(row("", "接続中の MCP サーバーはありません", "", "", 0, false));
    }
    for sv in servers {
        let name = s(sv, "name");
        let runtime = s(sv, "runtimeStatus");
        let auth = s(sv, "authStatus");
        let mut parts: Vec<String> = Vec::new();
        let r = runtime_label(runtime);
        if !r.is_empty() {
            parts.push(r.to_string());
        }
        let tools = sv.get("tools").and_then(Value::as_object).map(Map::len).unwrap_or(0);
        if tools > 0 {
            parts.push(format!("ツール {tools} 個"));
        }
        let a = auth_label(auth);
        if !a.is_empty() {
            parts.push(a.to_string());
        }
        if sv.get("toolsError").map(|e| e.is_string()).unwrap_or(false) {
            parts.push("ツールを取得できませんでした".into());
        }
        // 有効かどうかは設定の値(スレッドの接続の状態は、スレッドが無いと null)
        let configured = D.with(|d| d.borrow().enabled.iter().find(|(n, _)| n == name).map(|(_, e)| *e));
        let on = configured.unwrap_or(!runtime.eq_ignore_ascii_case("disabled"));
        out.push(row(&format!("mcpen:{name}"), name, &parts.join(" · "), "", 1, on));
        let needs_login = auth.eq_ignore_ascii_case("notLoggedIn") || runtime.eq_ignore_ascii_case("authenticationRequired");
        if needs_login {
            let busy = logging.iter().any(|n| n == name);
            out.push(row(&format!("mcplogin:{name}"), "", "", if busy { "開始中…" } else { "認証する" }, 2, false));
        }
        out.push(row(&format!("mcpdel:{name}"), "", "", "アンインストールする", 2, false));
    }

    out.push(row("", "サーバーを追加する", "", "", 6, false));
    let f = D.with(|d| {
        let d = d.borrow();
        (d.form.kind, d.form.name.clone(), d.form.command.clone(), d.form.args.clone(), d.form.env.clone(), d.form.url.clone(), d.form.bearer.clone())
    });
    let (kind, name, command, args, env, url, bearer) = f;
    let mut t = row("mcpf:type", "タイプ", "", if kind == 0 { "STDIO" } else { "ストリーミング可能な HTTP" }, 7, false);
    t.opts = strings(vec!["STDIO", "ストリーミング可能な HTTP"]);
    t.cur = kind as i32;
    out.push(t);
    out.push(row("mcpf:name", "名前", "", &name, 5, false));
    if kind == 0 {
        out.push(row("mcpf:command", "起動用コマンド", "", &command, 5, false));
        out.push(row("mcpf:args", "引数", "", &args, 5, false));
        out.push(row("mcpf:env", "環境変数", "", &env, 5, false));
    } else {
        out.push(row("mcpf:url", "URL", "", &url, 5, false));
        out.push(row("mcpf:bearer", "Bearer トークン環境変数", "", &bearer, 5, false));
    }
    out.push(row("mcp-add", "", "", "追加する", 2, false));
    out.push(row("", "設定", "", "", 6, false));
    out.push(row("mcp-reload", "設定を再読み込み", "", "再読み込み", 2, false));
    out.push(row("open-config", "config.toml", "", "ファイルを開く", 2, false));
    out
}

fn valid_name(n: &str) -> bool {
    !n.is_empty() && n.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/// 空白で区切る(引用符で囲んだ部分は 1 つ)。
fn split_words(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    let mut has = false;
    for c in text.chars() {
        match quote {
            Some(q) if c == q => quote = None,
            Some(_) => cur.push(c),
            None if c == '"' || c == '\'' => {
                quote = Some(c);
                has = true;
            }
            None if c.is_whitespace() => {
                if has || !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                    has = false;
                }
            }
            None => cur.push(c),
        }
    }
    if has || !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// 追加のフォームから、mcp_servers.<名前> に書く値を作る。足りないときは理由。
fn form_value() -> Result<(String, Value), String> {
    D.with(|d| {
        let d = d.borrow();
        let f = &d.form;
        let name = f.name.trim().to_string();
        if !valid_name(&name) {
            return Err("名前は英数字、ハイフン、アンダースコアで入力してください".into());
        }
        if f.kind == 0 {
            let command = f.command.trim();
            if command.is_empty() {
                return Err("起動用コマンドを入力してください".into());
            }
            let mut v = Map::new();
            v.insert("command".into(), json!(command));
            let args = split_words(&f.args);
            if !args.is_empty() {
                v.insert("args".into(), json!(args));
            }
            let mut env = Map::new();
            for w in split_words(&f.env) {
                match w.split_once('=') {
                    Some((k, val)) if !k.is_empty() => {
                        env.insert(k.to_string(), json!(val));
                    }
                    _ => return Err(format!("環境変数は KEY=VALUE の形で入力してください: {w}")),
                }
            }
            if !env.is_empty() {
                v.insert("env".into(), Value::Object(env));
            }
            Ok((name, Value::Object(v)))
        } else {
            let url = f.url.trim();
            if !(url.starts_with("http://") || url.starts_with("https://")) {
                return Err("URL は http:// または https:// で入力してください".into());
            }
            let mut v = Map::new();
            v.insert("url".into(), json!(url));
            if !f.bearer.trim().is_empty() {
                v.insert("bearer_token_env_var".into(), json!(f.bearer.trim()));
            }
            Ok((name, Value::Object(v)))
        }
    })
}

fn status(app: &AppWindow, text: &str, level: i32) {
    app.global::<AppState>().set_status(text.into());
    app.global::<AppState>().set_status_level(level);
}

fn write(key: &str, value: Value) {
    rpc("spage.mcp.cfg", "config/value/write", json!({ "keyPath": key, "value": value, "mergeStrategy": "replace" }));
}

/// ボタンの行。使ったら true。
pub fn action(app: &AppWindow, id: &str) -> bool {
    if id == "mcp-add" {
        match form_value() {
            Ok((name, value)) => rpc(
                "spage.mcp.add",
                "config/value/write",
                json!({ "keyPath": format!("mcp_servers.{name}"), "value": value, "mergeStrategy": "replace" }),
            ),
            Err(e) => status(app, &e, 2),
        }
        return true;
    }
    if let Some(name) = id.strip_prefix("mcpdel:") {
        write(&format!("mcp_servers.{name}"), Value::Null);
        return true;
    }
    if let Some(name) = id.strip_prefix("mcplogin:") {
        D.with(|d| {
            let mut d = d.borrow_mut();
            if !d.logging_in.iter().any(|n| n == name) {
                d.logging_in.push(name.to_string());
            }
        });
        rpc("spage.mcp.login", "mcpServer/oauth/login", json!({ "name": name }));
        refresh_mcp(app);
        return true;
    }
    false
}

/// スイッチの行(有効・無効)。
pub fn toggle(id: &str, on: bool) -> bool {
    let Some(name) = id.strip_prefix("mcpen:") else { return false };
    rpc("spage.mcp.cfg", "config/value/write", json!({ "keyPath": format!("mcp_servers.{name}.enabled"), "value": on, "mergeStrategy": "replace" }));
    true
}

/// タイプの選択。
pub fn choose(app: &AppWindow, id: &str, index: usize) -> bool {
    if id != "mcpf:type" {
        return false;
    }
    D.with(|d| d.borrow_mut().form.kind = index.min(1));
    refresh_mcp(app);
    true
}

/// 入力欄の文字(入力のたびに行は作り直さない)。
pub fn text(id: &str, value: &str) -> bool {
    let Some(field) = id.strip_prefix("mcpf:") else { return false };
    D.with(|d| {
        let mut d = d.borrow_mut();
        let f = &mut d.form;
        match field {
            "name" => f.name = value.to_string(),
            "command" => f.command = value.to_string(),
            "args" => f.args = value.to_string(),
            "env" => f.env = value.to_string(),
            "url" => f.url = value.to_string(),
            "bearer" => f.bearer = value.to_string(),
            _ => {}
        }
    });
    true
}

/// 画面を開く・読み直す: 設定(有効か)を読んでから、サーバーの一覧を引く。
pub fn start() {
    rpc("spage.mcp.conf", "config/read", json!({ "includeLayers": false }));
}

/// この画面の RPC の応答。使ったら true。
pub fn on_rpc(app: &AppWindow, tag: &str, result: &Result<Value, String>) -> bool {
    match tag {
        "spage.mcp.conf" => {
            if let Ok(v) = result {
                let servers = v.get("config").and_then(|c| c.get("mcp_servers")).and_then(Value::as_object);
                let list: Vec<(String, bool)> = servers
                    .map(|m| m.iter().map(|(n, c)| (n.clone(), c.get("enabled").and_then(Value::as_bool).unwrap_or(true))).collect())
                    .unwrap_or_default();
                D.with(|d| d.borrow_mut().enabled = list);
            }
            rpc("spage.mcp", "mcpServerStatus/list", json!({}));
            let _ = app;
            true
        }
        "spage.mcp.cfg" | "spage.mcp.add" => {
            match result {
                Ok(_) => {
                    // an added server leaves the form empty; the list is read again after the reload
                    if tag == "spage.mcp.add" {
                        clear_form();
                    }
                    rpc("spage.mcp.reload", "config/mcpServer/reload", json!({}));
                }
                Err(e) => status(app, &format!("MCP サーバーを保存できませんでした: {}", crate::errmsg::short(e)), 2),
            }
            true
        }
        "spage.mcp.login" => {
            match result {
                Ok(v) => {
                    let url = s(v, "authorizationUrl");
                    if url.starts_with("http://") || url.starts_with("https://") {
                        open_url(url);
                    }
                }
                Err(e) => {
                    D.with(|d| d.borrow_mut().logging_in.clear());
                    status(app, &format!("認証を開始できませんでした: {}", crate::errmsg::short(e)), 2);
                    refresh_mcp(app);
                }
            }
            true
        }
        _ => false,
    }
}

/// 通知: 起動の状態、OAuth の完了。
pub fn on_notify(app: &AppWindow, method: &str, params: &Value) {
    match method {
        "mcpServer/startupStatus/updated" => refresh_mcp(app),
        "mcpServer/oauthLogin/completed" => {
            let name = s(params, "name").to_string();
            D.with(|d| d.borrow_mut().logging_in.retain(|n| *n != name));
            if !params.get("success").and_then(Value::as_bool).unwrap_or(false) {
                let why = s(params, "error");
                status(app, &format!("{name} の認証に失敗しました{}", if why.is_empty() { String::new() } else { format!(": {why}") }), 2);
            }
            rpc("spage.mcp.reload", "config/mcpServer/reload", json!({}));
        }
        _ => {}
    }
}

/// 追加の後、フォームを空にする(書き込みが成功したとき)。
pub fn clear_form() {
    D.with(|d| d.borrow_mut().form = Form::default());
}
