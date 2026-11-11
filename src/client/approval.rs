//! What an approval card shows: the full command, the diff of a file change, the extra
//! permissions that are asked for. Pure functions over the request's parameters (tested).

use super::{ApprovalInfo, QuestionInfo};
use serde_json::{json, Value};

/// Line kinds of `ApprovalInfo::lines`.
pub const PLAIN: i32 = 0;
pub const ADDED: i32 = 1;
pub const REMOVED: i32 = 2;
pub const HEADER: i32 = 3;
pub const NOTE: i32 = 4;

/// A diff is cut after this many lines (the card scrolls; the chat row has the whole text).
const MAX_LINES: usize = 600;

fn text<'a>(v: &'a Value, k: &str) -> Option<&'a str> {
    v.get(k).and_then(Value::as_str).filter(|s| !s.is_empty())
}

/// The permission profile of an approval (network, file system) as readable lines.
pub fn permission_lines(profile: &Value) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(n) = profile.get("network").filter(|n| n.is_object()) {
        if n.get("enabled").and_then(Value::as_bool).unwrap_or(true) {
            out.push("ネットワーク: 許可".to_string());
        }
    }
    if let Some(fs) = profile.get("fileSystem").filter(|f| f.is_object()) {
        let list = |k: &str| -> Vec<String> {
            fs.get(k).and_then(Value::as_array).map(|a| a.iter().filter_map(Value::as_str).map(str::to_string).collect()).unwrap_or_default()
        };
        for p in list("read") {
            out.push(format!("読み取り: {p}"));
        }
        for p in list("write") {
            out.push(format!("書き込み: {p}"));
        }
        if let Some(entries) = fs.get("entries").and_then(Value::as_array) {
            for e in entries {
                let path = e.pointer("/path").map(|p| p.as_str().map(str::to_string).unwrap_or_else(|| p.to_string())).unwrap_or_default();
                let access = e.get("access").and_then(Value::as_str).unwrap_or("");
                if !path.is_empty() {
                    out.push(format!("{access} {path}").trim().to_string());
                }
            }
        }
    }
    out
}

/// One file change (`{path, kind: {type}, diff}`) as card lines; returns (adds, removes).
fn change_lines(change: &Value, out: &mut Vec<(String, i32)>) -> (usize, usize) {
    let kind = change.pointer("/kind/type").and_then(Value::as_str).unwrap_or("update");
    let label = match kind {
        "add" => "追加",
        "delete" => "削除",
        _ => "変更",
    };
    let path = text(change, "path").unwrap_or("");
    out.push((format!("{label} {path}"), HEADER));
    let diff = change.get("diff").and_then(Value::as_str).unwrap_or("");
    let (mut adds, mut removes) = (0, 0);
    for line in diff.lines() {
        let k = match kind {
            "add" => ADDED,
            "delete" => REMOVED,
            _ if line.starts_with("+++") || line.starts_with("---") || line.starts_with("diff --git") || line.starts_with("index ") || line.starts_with("@@") => NOTE,
            _ if line.starts_with('+') => ADDED,
            _ if line.starts_with('-') => REMOVED,
            _ => PLAIN,
        };
        match k {
            ADDED => adds += 1,
            REMOVED => removes += 1,
            _ => {}
        }
        out.push((line.to_string(), k));
    }
    (adds, removes)
}

/// The card of one server request. `changes` is the `changes` array of the file-change item
/// the request is about (from its item/started), when it has arrived. None: not an approval.
pub fn approval_info(method: &str, params: &Value, changes: Option<&Value>) -> Option<ApprovalInfo> {
    let thread = text(params, "threadId").unwrap_or("").to_string();
    let mut info = ApprovalInfo { thread, ..ApprovalInfo::default() };
    let mut detail: Vec<String> = Vec::new();
    match method {
        "item/commandExecution/requestApproval" => {
            info.title = "コマンドの実行を許可しますか？".into();
            if let Some(c) = text(params, "command") {
                info.lines = c.lines().map(|l| (l.to_string(), PLAIN)).collect();
                info.long = info.lines.len() > 6 || c.chars().count() > 240;
            }
            if let Some(d) = text(params, "cwd") {
                detail.push(format!("作業フォルダ: {d}"));
            }
            if let Some(r) = text(params, "reason") {
                detail.push(format!("理由: {r}"));
            }
            if let Some(n) = params.get("networkApprovalContext").filter(|n| n.is_object()) {
                detail.push(format!("ネットワーク: {} {}", text(n, "protocol").unwrap_or(""), text(n, "host").unwrap_or("")).trim().to_string());
            }
            if let Some(p) = params.get("additionalPermissions").filter(|p| p.is_object()) {
                for l in permission_lines(p) {
                    detail.push(format!("追加の権限 {l}"));
                }
            }
        }
        "item/fileChange/requestApproval" => {
            info.title = "ファイルの変更を許可しますか？".into();
            if let Some(r) = text(params, "reason") {
                detail.push(r.to_string());
            }
            if let Some(g) = text(params, "grantRoot") {
                detail.push(format!("対象フォルダ: {g}"));
            }
            if let Some(list) = changes.and_then(Value::as_array).filter(|l| !l.is_empty()) {
                let (mut adds, mut removes) = (0, 0);
                for c in list {
                    let (a, r) = change_lines(c, &mut info.lines);
                    adds += a;
                    removes += r;
                }
                info.summary = format!("{} ファイル  +{adds} −{removes}", list.len());
                info.long = true;
                if info.lines.len() > MAX_LINES {
                    let rest = info.lines.len() - MAX_LINES;
                    info.lines.truncate(MAX_LINES);
                    info.lines.push((format!("… 以降 {rest} 行"), NOTE));
                }
            }
        }
        "item/permissions/requestApproval" => {
            info.title = "追加の権限を許可しますか？".into();
            if let Some(d) = text(params, "cwd") {
                detail.push(format!("作業フォルダ: {d}"));
            }
            if let Some(r) = text(params, "reason") {
                detail.push(format!("理由: {r}"));
            }
            if let Some(p) = params.get("permissions") {
                info.lines = permission_lines(p).into_iter().map(|l| (l, PLAIN)).collect();
            }
        }
        "mcpServer/elicitation/request" => {
            let server = text(params, "serverName").unwrap_or("MCP");
            // The approval of a call of an MCP tool (the server asks before it runs the tool).
            let meta = params.get("_meta");
            if meta.and_then(|m| m.get("codex_approval_kind")).and_then(Value::as_str) == Some("mcp_tool_call") {
                let tool = meta.and_then(|m| m.get("tool_title").or_else(|| m.get("tool_name"))).and_then(Value::as_str).unwrap_or("?");
                info.title = format!("{server} のツール「{tool}」を実行しますか？");
                info.persist_always = match meta.and_then(|m| m.get("persist")) {
                    Some(Value::Array(a)) => a.iter().any(|v| v.as_str() == Some("always")),
                    Some(Value::String(s)) => s == "always",
                    _ => false,
                };
                if let Some(args) = meta.and_then(|m| m.get("tool_params")).filter(|a| !a.is_null()) {
                    let pretty = serde_json::to_string_pretty(args).unwrap_or_else(|_| args.to_string());
                    info.summary = "引数".into();
                    info.lines = pretty.lines().take(MAX_LINES).map(|l| (l.to_string(), PLAIN)).collect();
                }
                return Some(info);
            }
            info.title = format!("{server} が入力を求めています");
            let mut d = text(params, "message").unwrap_or("").to_string();
            if let Some(u) = text(params, "url") {
                d.push_str(&format!("\n{u}"));
            }
            info.detail = d.trim().to_string();
            info.simple = true;
            return Some(info);
        }
        _ => return None,
    }
    info.detail = detail.join("\n");
    Some(info)
}

/// The fields an MCP server asks for (mcpServer/elicitation/request in form mode) as questions of the
/// question card: one per property of requestedSchema, with the choices of an enum or a boolean.
/// None when there is no form (a URL, or no properties): that stays the plain card.
pub fn elicitation_questions(params: &Value) -> Option<Vec<QuestionInfo>> {
    let mode = text(params, "mode").unwrap_or("form");
    if !matches!(mode, "form" | "openai/form" | "openaiForm") {
        return None;
    }
    let props = params.get("requestedSchema")?.get("properties")?.as_object()?;
    if props.is_empty() {
        return None;
    }
    let message = text(params, "message").unwrap_or("").trim().to_string();
    // the required fields first, then the others
    let required: Vec<&str> = params
        .get("requestedSchema")
        .and_then(|s| s.get("required"))
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    let mut ordered: Vec<(&String, &Value)> = props.iter().collect();
    ordered.sort_by_key(|(name, _)| !required.contains(&name.as_str()));
    let mut out = Vec::new();
    for (i, (name, p)) in ordered.into_iter().enumerate() {
        let title = text(p, "title").unwrap_or(name).to_string();
        let mut question = text(p, "description").unwrap_or("").to_string();
        if i == 0 && !message.is_empty() {
            question = if question.is_empty() { message.clone() } else { format!("{message}\n{question}") };
        }
        out.push(QuestionInfo { id: name.clone(), header: title, question, options: elicitation_options(p).into_iter().map(|(label, _)| label).collect(), secret: false });
    }
    Some(out)
}

/// (shown, sent) of the choices of one property.
fn elicitation_options(p: &Value) -> Vec<(String, Value)> {
    if let Some(list) = p.get("oneOf").or_else(|| p.get("anyOf")).and_then(Value::as_array) {
        return list
            .iter()
            .filter_map(|o| {
                let c = o.get("const")?.clone();
                let label = text(o, "title").map(str::to_string).unwrap_or_else(|| c.as_str().map(str::to_string).unwrap_or_else(|| c.to_string()));
                Some((label, c))
            })
            .collect();
    }
    if let Some(list) = p.get("enum").and_then(Value::as_array) {
        let names: Vec<String> = p.get("enumNames").and_then(Value::as_array).map(|a| a.iter().filter_map(|n| n.as_str().map(str::to_string)).collect()).unwrap_or_default();
        return list
            .iter()
            .enumerate()
            .map(|(i, v)| (names.get(i).cloned().unwrap_or_else(|| v.as_str().map(str::to_string).unwrap_or_else(|| v.to_string())), v.clone()))
            .collect();
    }
    if p.get("type").and_then(Value::as_str) == Some("boolean") {
        return vec![("はい".into(), json!(true)), ("いいえ".into(), json!(false))];
    }
    Vec::new()
}

/// The answer to an elicitation from what the user answered: accepted with the fields they filled
/// in; declined when they skipped every question.
pub fn elicitation_result(params: &Value, answers: &[(String, Vec<String>)]) -> Value {
    let props = params.get("requestedSchema").and_then(|s| s.get("properties")).and_then(Value::as_object);
    let mut content = serde_json::Map::new();
    for (id, list) in answers {
        let Some(shown) = list.first().filter(|t| !t.trim().is_empty()) else { continue };
        let Some(p) = props.and_then(|p| p.get(id)) else { continue };
        let options = elicitation_options(p);
        let value = if let Some((_, v)) = options.iter().find(|(label, _)| label == shown) {
            v.clone()
        } else {
            match p.get("type").and_then(Value::as_str) {
                Some("integer") => shown.trim().parse::<i64>().map(|n| json!(n)).unwrap_or_else(|_| json!(shown)),
                Some("number") => shown.trim().parse::<f64>().map(|n| json!(n)).unwrap_or_else(|_| json!(shown)),
                Some("boolean") => json!(matches!(shown.trim(), "true" | "はい" | "yes")),
                _ => json!(shown),
            }
        };
        content.insert(id.clone(), value);
    }
    if content.is_empty() {
        json!({ "action": "decline", "content": null, "_meta": null })
    } else {
        json!({ "action": "accept", "content": content, "_meta": null })
    }
}

/// A call of a tool that the client is to run (item/tool/call). This app registers none, so the
/// call is answered as a failure the model can read; the screen shows a row about it.
/// Returns (the response, the row's text, the row's body).
pub fn dynamic_tool_failure(params: &Value) -> (Value, String, String) {
    let tool = text(params, "tool").unwrap_or("?");
    let name = match text(params, "namespace") {
        Some(ns) => format!("{ns}.{tool}"),
        None => tool.to_string(),
    };
    let message = format!("InoCode Desktop は動的ツール「{name}」を実行できません");
    let result = serde_json::json!({ "contentItems": [{ "type": "inputText", "text": message }], "success": false });
    let args = params.get("arguments").filter(|a| !a.is_null()).map(|a| format!("引数: {a}")).unwrap_or_default();
    (result, format!("{message}(失敗として返しました)"), args)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_dynamic_tool_call_is_answered_as_a_failure_and_shown() {
        let p = json!({ "threadId": "t", "callId": "c1", "namespace": "ide", "tool": "openFile", "arguments": { "path": "a.rs" } });
        let (result, row, body) = dynamic_tool_failure(&p);
        assert_eq!(result["success"], false);
        assert_eq!(result["contentItems"][0]["type"], "inputText");
        assert!(result["contentItems"][0]["text"].as_str().unwrap().contains("ide.openFile"));
        assert!(row.contains("失敗として返しました"));
        assert!(body.contains("a.rs"));
        assert_eq!(dynamic_tool_failure(&json!({})).2, "");
    }

    #[test]
    fn command_card_has_the_whole_command_and_where_it_runs() {
        let long = "a".repeat(300);
        let p = json!({
            "threadId": "t1", "command": format!("cd x\n{long}"), "cwd": "D:/w", "reason": "ネットワークが要る",
            "networkApprovalContext": { "host": "example.com", "protocol": "https" },
            "additionalPermissions": { "network": { "enabled": true }, "fileSystem": { "write": ["D:/out"] } },
        });
        let i = approval_info("item/commandExecution/requestApproval", &p, None).unwrap();
        assert_eq!(i.thread, "t1");
        assert_eq!(i.lines.len(), 2);
        assert_eq!(i.lines[1].0, long);
        assert!(i.long);
        assert!(i.detail.contains("作業フォルダ: D:/w") && i.detail.contains("理由: ネットワークが要る"));
        assert!(i.detail.contains("ネットワーク: https example.com"));
        assert!(i.detail.contains("追加の権限 ネットワーク: 許可") && i.detail.contains("追加の権限 書き込み: D:/out"));
        let short = approval_info("item/commandExecution/requestApproval", &json!({ "command": "dir" }), None).unwrap();
        assert!(!short.long);
    }

    #[test]
    fn file_change_card_shows_the_diff_of_every_file() {
        let changes = json!([
            { "path": "a.rs", "kind": { "type": "update" }, "diff": "@@ -1,2 +1,2 @@\n keep\n-old\n+new" },
            { "path": "b.txt", "kind": { "type": "add" }, "diff": "one\ntwo" },
            { "path": "c.txt", "kind": { "type": "delete" }, "diff": "gone" },
        ]);
        let p = json!({ "threadId": "t", "reason": "r", "grantRoot": "D:/w" });
        let i = approval_info("item/fileChange/requestApproval", &p, Some(&changes)).unwrap();
        assert_eq!(i.summary, "3 ファイル  +3 −2");
        let kinds: Vec<i32> = i.lines.iter().map(|l| l.1).collect();
        assert_eq!(kinds, vec![HEADER, NOTE, PLAIN, REMOVED, ADDED, HEADER, ADDED, ADDED, HEADER, REMOVED]);
        assert_eq!(i.lines[0].0, "変更 a.rs");
        assert!(i.detail.contains("r") && i.detail.contains("対象フォルダ: D:/w"));
        // 差分がまだ届いていないとき: 理由と対象フォルダだけ
        let bare = approval_info("item/fileChange/requestApproval", &p, None).unwrap();
        assert!(bare.lines.is_empty() && bare.summary.is_empty());
    }

    #[test]
    fn huge_diffs_are_cut() {
        let diff = (0..1000).map(|i| format!("+line {i}")).collect::<Vec<_>>().join("\n");
        let changes = json!([{ "path": "big", "kind": { "type": "update" }, "diff": diff }]);
        let i = approval_info("item/fileChange/requestApproval", &json!({}), Some(&changes)).unwrap();
        assert_eq!(i.lines.len(), MAX_LINES + 1);
        assert_eq!(i.lines.last().unwrap().1, NOTE);
        assert!(i.summary.contains("+1000"));
    }

    #[test]
    fn permission_request_lists_what_it_grants() {
        let p = json!({ "threadId": "t", "cwd": "D:/w", "reason": "ビルド",
            "permissions": { "network": { "enabled": true }, "fileSystem": { "read": ["C:/sdk"], "write": ["D:/w/out"] } } });
        let i = approval_info("item/permissions/requestApproval", &p, None).unwrap();
        let lines: Vec<&str> = i.lines.iter().map(|l| l.0.as_str()).collect();
        assert_eq!(lines, vec!["ネットワーク: 許可", "読み取り: C:/sdk", "書き込み: D:/w/out"]);
        assert!(i.detail.contains("理由: ビルド"));
    }

    #[test]
    fn a_form_elicitation_is_asked_as_questions_and_answered_with_the_fields() {
        let p = json!({
            "serverName": "s", "mode": "form", "message": "設定を入力してください",
            "requestedSchema": { "type": "object", "properties": {
                "name": { "type": "string", "title": "名前" },
                "count": { "type": "integer", "title": "件数" },
                "ok": { "type": "boolean", "title": "実行する" },
                "kind": { "type": "string", "enum": ["a", "b"] }
            } }
        });
        let q = elicitation_questions(&p).unwrap();
        assert_eq!(q.len(), 4);
        assert!(q.iter().any(|x| x.id == "ok" && x.options == vec!["はい".to_string(), "いいえ".to_string()]));
        let answers = vec![
            ("name".to_string(), vec!["x".to_string()]),
            ("count".to_string(), vec!["3".to_string()]),
            ("ok".to_string(), vec!["はい".to_string()]),
            ("kind".to_string(), vec![]),
        ];
        let r = elicitation_result(&p, &answers);
        assert_eq!(r["action"], "accept");
        assert_eq!(r["content"]["count"], 3);
        assert_eq!(r["content"]["ok"], true);
        assert!(r["content"].get("kind").is_none());
        let skipped = elicitation_result(&p, &[("name".to_string(), vec![])]);
        assert_eq!(skipped["action"], "decline");
        assert!(elicitation_questions(&json!({ "mode": "url", "url": "https://x" })).is_none());
    }

    #[test]
    fn an_mcp_tool_approval_names_the_tool_and_shows_its_arguments() {
        let p = json!({
            "serverName": "demo", "mode": "form", "message": "Allow the demo MCP server to run tool \"ask\"?",
            "_meta": { "codex_approval_kind": "mcp_tool_call", "persist": ["session", "always"], "tool_name": "ask", "tool_params": { "q": "x" } },
            "requestedSchema": { "type": "object", "properties": {} }
        });
        assert!(elicitation_questions(&p).is_none());
        let i = approval_info("mcpServer/elicitation/request", &p, None).unwrap();
        assert!(i.title.contains("demo") && i.title.contains("ask"), "{}", i.title);
        assert!(!i.simple);
        assert!(i.lines.iter().any(|(l, _)| l.contains("\"q\"")));
    }

    #[test]
    fn elicitation_is_simple_and_unknown_methods_are_not_approvals() {
        let p = json!({ "serverName": "docs", "message": "ログインしてください", "url": "https://x" });
        let i = approval_info("mcpServer/elicitation/request", &p, None).unwrap();
        assert!(i.simple);
        assert_eq!(i.title, "docs が入力を求めています");
        assert_eq!(i.detail, "ログインしてください\nhttps://x");
        assert!(approval_info("item/tool/call", &json!({}), None).is_none());
    }
}
