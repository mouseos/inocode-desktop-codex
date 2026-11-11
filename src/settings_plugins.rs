//! 設定の「プラグイン」: マーケットプレースごとのプラグインの一覧、インストール、アンインストール、
//! 有効・無効、マーケットプレースの追加・削除・更新。構造と文言は Codex Desktop の設定(plugins.card.*)に合わせる。
//!
//! plugin/list、plugin/install、plugin/uninstall、marketplace/add|remove|upgrade、
//! config/value/write(plugins."<id>".enabled)。

use crate::settings_page::{refresh_plugins, row, rpc};
use crate::state::STATE;
use crate::{AppState, AppWindow, SRow};
use serde_json::{json, Value};
use slint::ComponentHandle;
use std::cell::RefCell;

#[derive(Clone)]
struct Item {
    id: String,
    name: String,
    marketplace: String,
    path: Option<String>,
}

#[derive(Default)]
struct Data {
    items: Vec<Item>,
    marketplaces: Vec<String>,
    source: String,
    busy: Vec<String>,
}

thread_local! {
    static D: RefCell<Data> = RefCell::new(Data::default());
}

fn s<'a>(v: &'a Value, k: &str) -> &'a str {
    v.get(k).and_then(Value::as_str).unwrap_or("")
}

fn status(app: &AppWindow, text: &str, level: i32) {
    app.global::<AppState>().set_status(text.into());
    app.global::<AppState>().set_status_level(level);
}

/// plugin/list の引数
pub fn list_params() -> Value {
    let cwd = STATE.with(|st| st.borrow().active_cwd.clone());
    if cwd.is_empty() {
        json!({})
    } else {
        json!({ "cwds": [cwd] })
    }
}

/// 画面の行。v は plugin/list の応答。
pub fn rows(v: &Value) -> Vec<SRow> {
    let mut out: Vec<SRow> = Vec::new();
    let mut items: Vec<Item> = Vec::new();
    let mut marketplaces: Vec<String> = Vec::new();
    let busy = D.with(|d| d.borrow().busy.clone());
    let list = v.get("marketplaces").and_then(Value::as_array).cloned().unwrap_or_default();
    for m in &list {
        let mname = s(m, "name");
        marketplaces.push(mname.to_string());
        let title = m
            .get("interface")
            .and_then(|i| i.get("displayName"))
            .and_then(Value::as_str)
            .filter(|t| !t.is_empty())
            .unwrap_or(mname);
        out.push(row("", title, "", "", 6, false));
        let path = m.get("path").and_then(Value::as_str).map(str::to_string);
        for p in m.get("plugins").and_then(Value::as_array).into_iter().flatten() {
            let idx = items.len();
            let name = s(p, "name");
            let id = s(p, "id");
            items.push(Item { id: id.to_string(), name: name.to_string(), marketplace: mname.to_string(), path: path.clone() });
            let iface = p.get("interface");
            let shown = iface.and_then(|i| i.get("displayName")).and_then(Value::as_str).filter(|t| !t.is_empty()).unwrap_or(name);
            let desc = iface.and_then(|i| i.get("shortDescription")).and_then(Value::as_str).unwrap_or("");
            let installed = p.get("installed").and_then(Value::as_bool).unwrap_or(false);
            let working = busy.iter().any(|b| b == id);
            if installed {
                let enabled = p.get("enabled").and_then(Value::as_bool).unwrap_or(false);
                out.push(row(&format!("plgen:{idx}"), shown, desc, "", 1, enabled));
                out.push(row(&format!("plgdel:{idx}"), "", "", if working { "アンインストール中" } else { "アンインストール" }, 2, false));
            } else {
                let disabled = s(p, "availability").to_ascii_lowercase().contains("unavailable") || p.get("disabledReason").map(|r| r.is_string()).unwrap_or(false);
                let label = if working { "インストール中" } else if disabled { "利用できません" } else { "インストール" };
                out.push(row(&format!("plginst:{idx}"), shown, desc, label, 2, false));
            }
        }
    }
    if items.is_empty() {
        out.insert(0, row("", "インストールできるプラグインはありません", "", "", 0, false));
    }
    for e in v.get("marketplaceLoadErrors").and_then(Value::as_array).into_iter().flatten() {
        out.push(row("", &format!("{} を読み込めませんでした", s(e, "marketplaceName")), s(e, "message"), "", 0, false));
    }
    out.push(row("", "マーケットプレース", "", "", 6, false));
    for (i, m) in marketplaces.iter().enumerate() {
        out.push(row(&format!("plgmdel:{i}"), m, "", "削除", 2, false));
    }
    let src = D.with(|d| d.borrow().source.clone());
    out.push(row("plgf:source", "ソース(Git のリポジトリかフォルダ)", "", &src, 5, false));
    out.push(row("plg-madd", "", "", "追加", 2, false));
    out.push(row("plg-mup", "", "", "すべて更新", 2, false));
    D.with(|d| {
        let mut d = d.borrow_mut();
        d.items = items;
        d.marketplaces = marketplaces;
    });
    out
}

fn op(tag: &str, method: &str, params: Value) {
    rpc(tag, method, params);
}

/// ボタンの行。使ったら true。
pub fn action(app: &AppWindow, id: &str) -> bool {
    let item = |s: &str| s.parse::<usize>().ok().and_then(|i| D.with(|d| d.borrow().items.get(i).cloned()));
    if let Some(i) = id.strip_prefix("plginst:") {
        let Some(it) = item(i) else { return true };
        D.with(|d| d.borrow_mut().busy.push(it.id.clone()));
        let mut params = json!({ "pluginName": it.name });
        match &it.path {
            Some(p) => params["marketplacePath"] = json!(p),
            None => params["remoteMarketplaceName"] = json!(it.marketplace),
        }
        op("spage.plg.install", "plugin/install", params);
        refresh_plugins(app);
        return true;
    }
    if let Some(i) = id.strip_prefix("plgdel:") {
        let Some(it) = item(i) else { return true };
        D.with(|d| d.borrow_mut().busy.push(it.id.clone()));
        op("spage.plg.uninstall", "plugin/uninstall", json!({ "pluginId": it.id }));
        refresh_plugins(app);
        return true;
    }
    if let Some(i) = id.strip_prefix("plgmdel:") {
        if let Some(name) = i.parse::<usize>().ok().and_then(|i| D.with(|d| d.borrow().marketplaces.get(i).cloned())) {
            op("spage.plg.op", "marketplace/remove", json!({ "marketplaceName": name }));
        }
        return true;
    }
    match id {
        "plg-madd" => {
            let source = D.with(|d| d.borrow().source.trim().to_string());
            if source.is_empty() {
                status(app, "ソースを入力してください", 2);
            } else {
                op("spage.plg.madd", "marketplace/add", json!({ "source": source }));
            }
            true
        }
        "plg-mup" => {
            op("spage.plg.op", "marketplace/upgrade", json!({}));
            true
        }
        _ => false,
    }
}

/// スイッチの行(有効・無効)。
pub fn toggle(id: &str, on: bool) -> bool {
    let Some(i) = id.strip_prefix("plgen:") else { return false };
    let Some(it) = i.parse::<usize>().ok().and_then(|i| D.with(|d| d.borrow().items.get(i).cloned())) else { return true };
    // the id may hold dots: it is one quoted part of the key path
    let quoted = it.id.replace('\\', "\\\\").replace('"', "\\\"");
    rpc(
        "spage.plg.cfg",
        "config/value/write",
        json!({ "keyPath": format!("plugins.\"{quoted}\".enabled"), "value": on, "mergeStrategy": "replace" }),
    );
    true
}

pub fn text(id: &str, value: &str) -> bool {
    if id != "plgf:source" {
        return false;
    }
    D.with(|d| d.borrow_mut().source = value.to_string());
    true
}

/// この画面の RPC の応答。使ったら true。
pub fn on_rpc(app: &AppWindow, tag: &str, result: &Result<Value, String>) -> bool {
    let fail = |what: &str, e: &str| status(app, &format!("{what}: {}", crate::errmsg::short(e)), 2);
    match tag {
        "spage.plg.install" | "spage.plg.uninstall" => {
            D.with(|d| d.borrow_mut().busy.clear());
            match result {
                Ok(_) => {}
                Err(e) => fail(if tag == "spage.plg.install" { "プラグインをインストールできませんでした" } else { "プラグインをアンインストールできませんでした" }, e),
            }
            refresh_plugins(app);
            true
        }
        "spage.plg.madd" | "spage.plg.op" | "spage.plg.cfg" => {
            match result {
                Ok(_) => {
                    if tag == "spage.plg.madd" {
                        D.with(|d| d.borrow_mut().source.clear());
                    }
                }
                Err(e) => fail(
                    match tag {
                        "spage.plg.madd" => "マーケットプレースを追加できませんでした",
                        "spage.plg.cfg" => "プラグインを更新できませんでした",
                        _ => "マーケットプレースを更新できませんでした",
                    },
                    e,
                ),
            }
            refresh_plugins(app);
            true
        }
        _ => false,
    }
}
