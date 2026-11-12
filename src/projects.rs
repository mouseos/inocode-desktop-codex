//! The server's projects: project/list, and project/create for a folder added from the picker.

use crate::client::Command;
use crate::state::*;
use crate::*;
use serde_json::{json, Value};

thread_local! {
    static HANDLE: RefCell<Option<Rc<client::Handle>>> = const { RefCell::new(None) };
}

pub(crate) fn register(handle: &Rc<client::Handle>) {
    HANDLE.with(|c| *c.borrow_mut() = Some(handle.clone()));
}

pub(crate) fn request_list() {
    if let Some(h) = HANDLE.with(|c| c.borrow().clone()) {
        h.send(Command::Rpc { tag: "projects.list".into(), method: "project/list".into(), params: json!({}) });
    }
}

/// Make the folder a project (the same key for the same folder gives the same project).
pub(crate) fn create(dir: &str) {
    let name = std::path::Path::new(dir)
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| dir.to_string());
    if let Some(h) = HANDLE.with(|c| c.borrow().clone()) {
        h.send(Command::Rpc {
            tag: "projects.create".into(),
            method: "project/create".into(),
            params: json!({ "name": name, "roots": [{ "path": dir }], "idempotencyKey": format!("inocode-desktop:{dir}") }),
        });
    }
}

fn roots_of(project: &Value) -> Vec<String> {
    project
        .get("roots")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|r| r.get("path").and_then(Value::as_str))
        .map(str::to_string)
        .collect()
}

pub(crate) fn on_rpc(app: &AppWindow, tag: &str, result: &Result<Value, String>) {
    let Ok(v) = result else { return };
    let found: Vec<String> = match tag {
        "projects.list" => v.get("data").and_then(Value::as_array).into_iter().flatten().flat_map(roots_of).collect(),
        "projects.create" => v.get("project").map(roots_of).unwrap_or_default(),
        _ => return,
    };
    STATE.with(|s| {
        let mut st = s.borrow_mut();
        if tag == "projects.list" {
            st.server_projects.clear();
        }
        for p in found {
            if !st.server_projects.contains(&p) {
                st.server_projects.push(p);
            }
        }
        crate::view::sync_project_cwds(&mut st);
        crate::view::rebuild_projects(app, &st);
    });
}
