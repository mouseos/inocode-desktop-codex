//! Files tab: the folder tree (`fs/readDirectory`) and a viewer for a file (`fs/readFile`).

use super::{b64, Host};
use crate::client::Event;
use crate::{AppWindow, FileEntry, FilesState};
use serde_json::{json, Value};
use slint::{ComponentHandle, ModelRc, VecModel};
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

const MAX_VIEW_LINES: usize = 3000;

#[derive(Default)]
struct Data {
    root: String,
    expanded: HashSet<String>,
    children: HashMap<String, Vec<(String, bool)>>,
    /// The rows on screen: (path, is_dir).
    rows: Vec<(String, bool)>,
}

thread_local! {
    static DATA: RefCell<Data> = RefCell::new(Data::default());
}

fn join(dir: &str, name: &str) -> String {
    let sep = if dir.contains('\\') { '\\' } else { '/' };
    if dir.ends_with(['\\', '/']) {
        format!("{dir}{name}")
    } else {
        format!("{dir}{sep}{name}")
    }
}

fn read_dir(host: &Host, path: &str) {
    host.rpc(&format!("files.dir:{path}"), "fs/readDirectory", json!({ "path": path }));
}

pub fn install(app: &AppWindow, host: &Host) {
    let state = app.global::<FilesState>();
    let (weak, h) = (app.as_weak(), host.clone());
    state.on_refresh(move || {
        if let Some(app) = weak.upgrade() {
            opened_force(&app, &h);
        }
    });
    let (weak, h) = (app.as_weak(), host.clone());
    state.on_toggle(move |i| {
        let Some(app) = weak.upgrade() else { return };
        let Some((path, is_dir)) = DATA.with(|d| d.borrow().rows.get(i as usize).cloned()) else { return };
        if is_dir {
            let need = DATA.with(|d| {
                let mut d = d.borrow_mut();
                if !d.expanded.remove(&path) {
                    d.expanded.insert(path.clone());
                    return !d.children.contains_key(&path);
                }
                false
            });
            if need {
                read_dir(&h, &path);
            }
            render(&app);
        } else {
            let name = path.rsplit(['\\', '/']).next().unwrap_or(&path).to_string();
            let st = app.global::<FilesState>();
            st.set_viewing(true);
            st.set_viewing_name(name.into());
            st.set_viewing_text("読み込み中…".into());
            h.rpc(&format!("files.file:{path}"), "fs/readFile", json!({ "path": path }));
        }
    });
    let weak = app.as_weak();
    state.on_close_file(move || {
        if let Some(app) = weak.upgrade() {
            app.global::<FilesState>().set_viewing(false);
        }
    });
}

pub fn opened(app: &AppWindow, host: &Host) {
    if DATA.with(|d| d.borrow().root.is_empty()) {
        opened_force(app, host);
    }
}

fn opened_force(app: &AppWindow, host: &Host) {
    let cwd = crate::state::STATE.with(|s| s.borrow().active_cwd.clone());
    if cwd.is_empty() {
        app.global::<FilesState>().set_error("作業フォルダがありません".into());
        return;
    }
    DATA.with(|d| {
        let mut d = d.borrow_mut();
        d.root = cwd.clone();
        d.children.clear();
        d.expanded.clear();
        d.expanded.insert(cwd.clone());
    });
    app.global::<FilesState>().set_error("".into());
    read_dir(host, &cwd);
    render(app);
}

pub fn event(app: &AppWindow, host: &Host, event: &Event) {
    match event {
        Event::ThreadActive { .. } => {
            // A different folder may be active now.
            let cwd = crate::state::STATE.with(|s| s.borrow().active_cwd.clone());
            let stale = DATA.with(|d| {
                let d = d.borrow();
                !d.root.is_empty() && d.root != cwd && !cwd.is_empty()
            });
            if stale {
                opened_force(app, host);
            }
        }
        Event::Rpc { tag, result } if tag.starts_with("files.dir:") => {
            let path = &tag["files.dir:".len()..];
            match result {
                Ok(v) => {
                    let mut list: Vec<(String, bool)> = v
                        .get("entries")
                        .and_then(Value::as_array)
                        .map(|a| {
                            a.iter()
                                .filter_map(|e| {
                                    Some((
                                        e.get("fileName")?.as_str()?.to_string(),
                                        e.get("isDirectory").and_then(Value::as_bool).unwrap_or(false),
                                    ))
                                })
                                .collect()
                        })
                        .unwrap_or_default();
                    list.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.to_lowercase().cmp(&b.0.to_lowercase())));
                    DATA.with(|d| d.borrow_mut().children.insert(path.to_string(), list));
                    app.global::<FilesState>().set_error("".into());
                    render(app);
                }
                Err(e) => app.global::<FilesState>().set_error(e.as_str().into()),
            }
        }
        Event::Rpc { tag, result } if tag.starts_with("files.file:") => {
            let st = app.global::<FilesState>();
            match result {
                Ok(v) => {
                    let bytes = b64::decode(v.get("dataBase64").and_then(Value::as_str).unwrap_or(""));
                    if bytes.contains(&0) {
                        st.set_viewing_text("バイナリファイルです".into());
                    } else {
                        let text = String::from_utf8_lossy(&bytes);
                        let mut out = String::new();
                        for (i, line) in text.lines().enumerate() {
                            if i >= MAX_VIEW_LINES {
                                out.push_str("…");
                                break;
                            }
                            out.push_str(line);
                            out.push('\n');
                        }
                        st.set_viewing_text(out.into());
                    }
                }
                Err(e) => st.set_viewing_text(e.as_str().into()),
            }
        }
        _ => {}
    }
}

fn flatten(d: &Data, dir: &str, depth: i32, out: &mut Vec<(String, bool, i32, String)>) {
    let Some(list) = d.children.get(dir) else { return };
    for (name, is_dir) in list {
        let path = join(dir, name);
        out.push((path.clone(), *is_dir, depth, name.clone()));
        if *is_dir && d.expanded.contains(&path) {
            flatten(d, &path, depth + 1, out);
        }
    }
}

fn render(app: &AppWindow) {
    let st = app.global::<FilesState>();
    DATA.with(|d| {
        let mut d = d.borrow_mut();
        let mut flat = Vec::new();
        let root = d.root.clone();
        flatten(&d, &root, 0, &mut flat);
        d.rows = flat.iter().map(|(p, dir, _, _)| (p.clone(), *dir)).collect();
        let rows: Vec<FileEntry> = flat
            .iter()
            .map(|(p, dir, depth, name)| FileEntry {
                name: name.as_str().into(),
                is_dir: *dir,
                depth: *depth,
                open: *dir && d.expanded.contains(p),
            })
            .collect();
        st.set_entries(ModelRc::from(Rc::new(VecModel::from(rows))));
        st.set_root_name(root.rsplit(['\\', '/']).find(|s| !s.is_empty()).unwrap_or("").into());
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn joins() {
        assert_eq!(join("D:\\a", "b"), "D:\\a\\b");
        assert_eq!(join("/a/", "b"), "/a/b");
    }
}
