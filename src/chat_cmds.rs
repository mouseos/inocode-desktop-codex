//! More slash commands of the composer, as Codex has them: /model, /reasoning, /speed, /plan,
//! /mcp and /usage. They use what the pickers use (the shared settings) and the server's own calls.

use crate::state::*;
use crate::*;
use serde_json::{json, Value};

pub(crate) const MORE: [(&str, &str); 6] = [
    ("/model", "モデルを選ぶ"),
    ("/reasoning", "思考量を選ぶ"),
    ("/speed", "速度を選ぶ(fast / standard)"),
    ("/plan", "プランモードの切り替え(続けて指示も書ける)"),
    ("/mcp", "MCP サーバーの状態"),
    ("/usage", "使用量の上限"),
];

fn say(app: &AppWindow, id: &str, text: &str, detail: &str) {
    crate::view::messages_with(app, |m| crate::view::upsert_tool(m, id, text, Some(detail)));
    app.invoke_scroll_bottom();
}

/// Run one of these commands. False when `cmd` is not one of them.
pub(crate) fn run(app: &AppWindow, h: &client::Handle, cmd: &str, arg: &str) -> bool {
    let arg_l = arg.to_lowercase();
    match cmd {
        "/model" => {
            let list: Vec<(String, String, String)> = STATE.with(|s| {
                let st = s.borrow();
                visible_models(&st).iter().map(|m| (m.id.clone(), m.name.clone(), m.provider.clone())).collect()
            });
            let found = (!arg.is_empty())
                .then(|| list.iter().find(|(id, _, _)| id.to_lowercase() == arg_l).or_else(|| list.iter().find(|(id, name, _)| id.to_lowercase().contains(&arg_l) || name.to_lowercase().contains(&arg_l))))
                .flatten()
                .cloned();
            match found {
                Some((id, _, provider)) => {
                    let mut switch = false;
                    change(app, h, |st| {
                        switch = crate::model_init::provider_changed(st, &provider);
                        let m = visible_models(st).iter().find(|m| m.id == id && m.provider == provider).map(|m| (m.efforts.clone(), m.default_effort.clone()));
                        st.settings.model = Some(id.clone());
                        st.settings.model_provider = Some(provider.clone());
                        crate::gui_prefs::save_last_model(&id, &provider);
                        if let Some((efforts, default)) = m {
                            if !st.settings.effort.as_ref().map(|e| efforts.contains(e)).unwrap_or(false) {
                                st.settings.effort = Some(default).filter(|e| !e.is_empty());
                            }
                        }
                    });
                    if switch {
                        h.send(Command::NewThread);
                    }
                    say(app, "slash:model", &format!("モデル: {id}"), "");
                }
                None => {
                    let now = STATE.with(|s| s.borrow().settings.model.clone().unwrap_or_default());
                    let lines: Vec<String> = list.iter().map(|(id, name, p)| format!("{id}  {name}  ({p})")).collect();
                    let head = if arg.is_empty() { format!("今のモデル: {now}") } else { format!("「{arg}」に合うモデルがありません(今: {now})") };
                    say(app, "slash:model", &head, &lines.join("\n"));
                }
            }
            true
        }
        "/reasoning" => {
            let (efforts, now) = STATE.with(|s| {
                let st = s.borrow();
                let v = visible_models(&st);
                (selected_model(&st, &v).map(|m| m.efforts.clone()).unwrap_or_default(), st.settings.effort.clone().unwrap_or_default())
            });
            if efforts.iter().any(|e| e.to_lowercase() == arg_l) {
                let e = efforts.iter().find(|e| e.to_lowercase() == arg_l).cloned().unwrap_or_default();
                change(app, h, |st| st.settings.effort = Some(e.clone()));
                say(app, "slash:reasoning", &format!("思考量: {e}"), "");
            } else {
                say(app, "slash:reasoning", &format!("今の思考量: {now}"), &format!("選べる値: {}", efforts.join(" / ")));
            }
            true
        }
        "/speed" => {
            let fast = STATE.with(|s| {
                let st = s.borrow();
                let v = visible_models(&st);
                selected_model(&st, &v).and_then(|m| m.fast_tier().map(str::to_string))
            });
            match (arg_l.as_str(), fast) {
                ("fast", Some(t)) => {
                    change(app, h, |st| st.settings.service_tier = Some(Some(t.clone())));
                    say(app, "slash:speed", "速度: fast", "");
                }
                ("standard", _) => {
                    change(app, h, |st| st.settings.service_tier = Some(None));
                    say(app, "slash:speed", "速度: standard", "");
                }
                (_, None) => say(app, "slash:speed", "このモデルに fast はありません", ""),
                _ => say(app, "slash:speed", "使い方: /speed fast または /speed standard", ""),
            }
            true
        }
        "/plan" => {
            // On: the model plans and asks, and does not change files. Off: it works.
            let on = match arg_l.as_str() {
                "on" => true,
                "off" => false,
                _ => !arg.is_empty() || !STATE.with(|s| s.borrow().settings.plan_mode),
            };
            change(app, h, |st| st.settings.plan_mode = on);
            say(app, "slash:plan", if on { "プランモード: オン" } else { "プランモード: オフ(実行します)" }, "");
            if on && !arg.is_empty() && arg_l != "on" {
                h.send(Command::Prompt(arg.to_string()));
            }
            true
        }
        "/mcp" => {
            h.send(Command::Rpc { tag: "slash.mcp".into(), method: "mcpServerStatus/list".into(), params: json!({}) });
            true
        }
        "/usage" => {
            h.send(Command::Rpc { tag: "slash.usage".into(), method: "account/rateLimits/read".into(), params: Value::Null });
            true
        }
        _ => false,
    }
}

pub(crate) fn on_rpc(app: &AppWindow, tag: &str, result: &Result<Value, String>) {
    match (tag, result) {
        ("slash.mcp", Ok(v)) => {
            let rows = v.get("data").and_then(Value::as_array).map(Vec::as_slice).unwrap_or(&[]);
            let lines: Vec<String> = rows
                .iter()
                .map(|r| {
                    let name = r.get("name").and_then(Value::as_str).unwrap_or("");
                    let tools = r.get("tools").and_then(Value::as_object).map(|t| t.len()).unwrap_or(0);
                    let auth = r.get("authStatus").and_then(Value::as_str).unwrap_or("");
                    format!("{name}: ツール {tools} 件 {auth}").trim_end().to_string()
                })
                .collect();
            say(app, "slash:mcp", &format!("MCP サーバー {} 件", rows.len()), &lines.join("\n"));
        }
        ("slash.usage", Ok(v)) => {
            let text = client::rate_limit_text(v.get("rateLimits").unwrap_or(&Value::Null)).unwrap_or_else(|| "使用量の情報がありません".to_string());
            say(app, "slash:usage", "使用量", &text);
        }
        ("slash.mcp" | "slash.usage", Err(e)) => say(app, &format!("slash:{}", &tag[6..]), &format!("取得できませんでした: {e}"), ""),
        _ => {}
    }
}
