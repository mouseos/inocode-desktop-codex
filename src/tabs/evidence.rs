//! 根拠タブのマインドマップと文書の検証。ページ・引用は src/tabs/kb.rs。
//! どちらも、いま開いているプロジェクトの資料ページ(inocode/kb/page/*)から作る。ページの `[リンク]` が辺、
//! 引用の出典が原資料のノード。プロジェクトに属さない旧い根拠 DB(inocode/evidence/*)は、使わない。
//! 文書の検証は、サーバーの `inocode/kb/verify`(提案中。まだ無ければ、その旨を出す)に頼む。

use super::Host;
use crate::client::Event;
use crate::{AppWindow, EvidenceState, Finding, GEdge, GNode, VSource};
use serde_json::{json, Value};
use slint::{ComponentHandle, ModelRc, VecModel};
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

/// マップに載せるページの数の上限(1 ページずつ読むので)
const MAX_PAGES: usize = 60;

fn payload(result: &Value) -> &Value {
    result.get("response").unwrap_or(result)
}

fn s(v: &Value, k: &str) -> String {
    v.get(k).and_then(Value::as_str).unwrap_or("").to_string()
}

fn kind_label(kind: &str) -> String {
    match kind {
        "page" => "ページ",
        "link" => "未作成のページ",
        "source" => "原資料",
        other => other,
    }
    .to_string()
}

fn finding_label(kind: &str) -> String {
    match kind {
        "quote_missing" => "引用が原文に無い",
        "uncited" => "引用が無い",
        "novel_term" => "原資料に無い語",
        other => other,
    }
    .to_string()
}

// ------------------------------------------------------------------ pure helpers (tested)

#[derive(Debug, Clone, PartialEq)]
struct NodeData {
    id: String,
    kind: String,
    label: String,
    status: String,
    supported: bool,
}

#[derive(Debug, Clone, PartialEq)]
struct EdgeData {
    from: String,
    to: String,
    kind: String,
}

/// 読んだページ(page/read の応答)から、ノードと辺を作る: ページ -> リンク先のページ、ページ -> 引用の出典。
/// 作っていないページと、原文と一致しない引用のある出典は「根拠なし」(赤)。
fn build_graph(pages: &[Value]) -> (Vec<NodeData>, Vec<EdgeData>) {
    let mut nodes: Vec<NodeData> = Vec::new();
    let mut edges: Vec<EdgeData> = Vec::new();
    let mut add = |nodes: &mut Vec<NodeData>, id: String, kind: &str, label: String, status: &str, supported: bool| {
        match nodes.iter_mut().find(|n| n.id == id) {
            // a page that was read after a link to it was seen: it exists
            Some(n) if kind == "page" => {
                n.kind = kind.to_string();
                n.supported = supported;
                n.status = status.to_string();
            }
            Some(n) => n.supported &= supported,
            None => nodes.push(NodeData { id, kind: kind.to_string(), label, status: status.to_string(), supported }),
        }
    };
    for p in pages {
        let title = s(p, "title");
        if title.is_empty() {
            continue;
        }
        let confirmed = p.get("confirmed").and_then(Value::as_bool).unwrap_or(false);
        add(&mut nodes, format!("page:{title}"), "page", title.clone(), if confirmed { "verified" } else { "unverified" }, true);
        for l in p.get("links").and_then(Value::as_array).into_iter().flatten() {
            let to = s(l, "title");
            if to.is_empty() {
                continue;
            }
            let exists = l.get("exists").and_then(Value::as_bool).unwrap_or(false);
            let id = format!("page:{to}");
            if !nodes.iter().any(|n| n.id == id) {
                add(&mut nodes, id.clone(), if exists { "page" } else { "link" }, to.clone(), "unverified", exists);
            }
            edges.push(EdgeData { from: format!("page:{title}"), to: id, kind: "link".into() });
        }
        for q in p.get("quotes").and_then(Value::as_array).into_iter().flatten() {
            let r = s(q, "sourceRef");
            let label = if s(q, "sourceTitle").is_empty() { r.clone() } else { s(q, "sourceTitle") };
            if r.is_empty() && label.is_empty() {
                continue;
            }
            let ok = q.get("matches").and_then(Value::as_bool).unwrap_or(true);
            let id = format!("source:{}", if r.is_empty() { &label } else { &r });
            add(&mut nodes, id.clone(), "source", label, if ok { "verified" } else { "unverified" }, ok);
            edges.push(EdgeData { from: format!("page:{title}"), to: id, kind: "cites".into() });
        }
    }
    (nodes, edges)
}

/// 検証に使える原資料: 読んだページの引用の出典(出典の参照ごとに 1 件)
fn sources_of(pages: &[Value]) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    for q in pages.iter().flat_map(|p| p.get("quotes").and_then(Value::as_array).into_iter().flatten()) {
        let r = s(q, "sourceRef");
        if !r.is_empty() && !out.iter().any(|(x, _)| *x == r) {
            out.push((r.clone(), if s(q, "sourceTitle").is_empty() { r } else { s(q, "sourceTitle") }));
        }
    }
    out
}

const NODE_W: f32 = 210.0;
const NODE_H: f32 = 58.0;
const H_GAP: f32 = 70.0;
const V_GAP: f32 = 18.0;
/// The map is at most this many columns and rows: a cycle (or a very long chain) must not make the
/// canvas wider than the renderer can draw (it panics beyond about 32000 physical pixels: keep the map under 12000 logical ones, the screen may scale by 2).
const MAX_DEPTH: usize = 35;
const MAX_ROWS: usize = 150;
const MARGIN: f32 = 24.0;

#[derive(Debug, Clone, PartialEq)]
struct Placed {
    id: String,
    x: f32,
    y: f32,
}

#[derive(Debug, Clone, PartialEq)]
struct Layout {
    placed: Vec<Placed>,
    /// (始点のノード番号, 終点のノード番号, 形 [始点 x, 始点 y, 制御点 1 の x, 制御点 2 の x, 終点 x, 終点 y])
    edges: Vec<(usize, usize, [f32; 6])>,
    width: f32,
    height: f32,
}

/// 左から右へ、有向辺の最長経路で層を決め、層の中は前の層の位置の平均で並べる (交差を減らす)。
fn layout(nodes: &[NodeData], edges: &[EdgeData]) -> Layout {
    let index: HashMap<&str, usize> = nodes.iter().enumerate().map(|(i, n)| (n.id.as_str(), i)).collect();
    let links: Vec<(usize, usize)> = edges
        .iter()
        .filter_map(|e| Some((*index.get(e.from.as_str())?, *index.get(e.to.as_str())?)))
        .collect();
    // 層: 最長経路 (循環があっても止まるよう反復回数を制限する)
    let mut layer = vec![0usize; nodes.len()];
    for _ in 0..nodes.len() {
        let mut changed = false;
        for &(a, b) in &links {
            if a != b && layer[b] < layer[a] + 1 && layer[a] + 1 < MAX_DEPTH {
                layer[b] = layer[a] + 1;
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    let depth = layer.iter().copied().max().map(|m| m + 1).unwrap_or(0);
    let mut row_of = vec![0f32; nodes.len()];
    let mut columns: Vec<Vec<usize>> = vec![Vec::new(); depth];
    for l in 0..depth {
        let mut members: Vec<usize> = (0..nodes.len()).filter(|&i| layer[i] == l).collect();
        if l > 0 {
            let bary = |i: usize| -> f32 {
                let preds: Vec<f32> =
                    links.iter().filter(|(a, b)| *b == i && layer[*a] < l).map(|(a, _)| row_of[*a]).collect();
                if preds.is_empty() { f32::MAX } else { preds.iter().sum::<f32>() / preds.len() as f32 }
            };
            members.sort_by(|&a, &b| bary(a).partial_cmp(&bary(b)).unwrap_or(std::cmp::Ordering::Equal));
        }
        for (r, &i) in members.iter().enumerate() {
            row_of[i] = r as f32;
        }
        columns[l] = members;
    }
    let rows = columns.iter().map(Vec::len).max().unwrap_or(0).min(MAX_ROWS);
    let mut placed = vec![Placed { id: String::new(), x: 0.0, y: 0.0 }; nodes.len()];
    for (l, members) in columns.iter().enumerate() {
        for (r, &i) in members.iter().enumerate() {
            placed[i] = Placed {
                id: nodes[i].id.clone(),
                x: MARGIN + l as f32 * (NODE_W + H_GAP),
                y: MARGIN + r.min(MAX_ROWS - 1) as f32 * (NODE_H + V_GAP),
            };
        }
    }
    let geom = |a: usize, b: usize| -> [f32; 6] {
        let (x1, y1) = (placed[a].x + NODE_W, placed[a].y + NODE_H / 2.0);
        let (x2, y2) = (placed[b].x, placed[b].y + NODE_H / 2.0);
        let dx = ((x2 - x1).abs() / 2.0).max(24.0);
        [x1, y1, x1 + dx, x2 - dx, x2, y2]
    };
    Layout {
        edges: links.iter().map(|&(a, b)| (a, b, geom(a, b))).collect(),
        width: MARGIN * 2.0 + depth as f32 * NODE_W + depth.saturating_sub(1) as f32 * H_GAP,
        height: MARGIN * 2.0 + rows as f32 * NODE_H + rows.saturating_sub(1) as f32 * V_GAP,
        placed,
    }
}

/// One edge as path commands, with (ox, oy) as the origin: the curve and the head of its arrow.
fn edge_commands(g: &[f32; 6], ox: f32, oy: f32) -> String {
    let [x1, y1, c1, c2, x2, y2] = *g;
    format!(
        "M {} {} C {} {} {} {} {} {} M {} {} L {} {} M {} {} L {} {} ",
        x1 - ox,
        y1 - oy,
        c1 - ox,
        y1 - oy,
        c2 - ox,
        y2 - oy,
        x2 - ox,
        y2 - oy,
        x2 - ox,
        y2 - oy,
        x2 - 7.0 - ox,
        y2 - 4.0 - oy,
        x2 - ox,
        y2 - oy,
        x2 - 7.0 - ox,
        y2 + 4.0 - oy
    )
}

/// The lines are drawn in cells of this size, one path per colour and cell: a screen only draws the cells it shows.
const CELL: f32 = 1600.0;

/// The lines grouped by colour and cell: (tone, cell x, cell y) -> commands, in the coordinates of the cell.
fn edge_cells(layout: &Layout, tones: &[i32]) -> Vec<((i32, i32, i32), String)> {
    let mut cells: HashMap<(i32, i32, i32), String> = HashMap::new();
    for ((_, _, g), tone) in layout.edges.iter().zip(tones) {
        let (x0, x1) = (g[0].min(g[3]).min(g[2]).min(g[4] - 7.0), g[0].max(g[2]).max(g[3]).max(g[4]));
        let (y0, y1) = (g[1].min(g[5]) - 4.0, g[1].max(g[5]) + 4.0);
        let (cx0, cx1) = ((x0 / CELL).floor() as i32, (x1 / CELL).floor() as i32);
        let (cy0, cy1) = ((y0 / CELL).floor() as i32, (y1 / CELL).floor() as i32);
        for cx in cx0..=cx1 {
            for cy in cy0..=cy1 {
                cells.entry((*tone, cx, cy)).or_default().push_str(&edge_commands(g, cx as f32 * CELL, cy as f32 * CELL));
            }
        }
    }
    let mut v: Vec<_> = cells.into_iter().collect();
    v.sort_by_key(|(k, _)| *k);
    v
}

#[derive(Debug, Clone, PartialEq)]
struct FindingData {
    kind_label: String,
    text: String,
    range: String,
}

fn parse_findings(result: &Value) -> Vec<FindingData> {
    payload(result)
        .get("findings")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .map(|f| {
                    let start = f.pointer("/position/start").and_then(Value::as_u64);
                    let end = f.pointer("/position/end").and_then(Value::as_u64);
                    FindingData {
                        kind_label: finding_label(&s(f, "kind")),
                        text: s(f, "text"),
                        range: match (start, end) {
                            (Some(a), Some(b)) => format!("{a}–{b} 文字目"),
                            _ => String::new(),
                        },
                    }
                })
                .collect()
        })
        .unwrap_or_default()
}


// ------------------------------------------------------------------ state

#[derive(Default)]
struct E {
    /// 読んだページ(page/read の応答)
    pages: Vec<Value>,
    nodes: Vec<NodeData>,
    edges: Vec<EdgeData>,
    layout: Option<Layout>,
    /// 線を色とセルでまとめたもの
    cells: Vec<((i32, i32, i32), String)>,
    selected_node: Option<String>,
    verify_checked: Vec<bool>,
    /// いま画面にある地図の元(題と版)。同じなら作り直さない
    key: Option<String>,
    /// 作り直しの世代: 古い結果は捨てる
    generation: u64,
}

thread_local! {
    static E: RefCell<E> = RefCell::new(E::default());
    static REBUILD: RefCell<Option<slint::Timer>> = const { RefCell::new(None) };
    static BENCH: RefCell<Option<slint::Timer>> = const { RefCell::new(None) };
}

/// いま開いているスレッドの作業フォルダ(プロジェクトはここから決まる)。無ければ、何も頼まない。
fn cwd(_app: &AppWindow) -> String {
    crate::state::STATE.with(|s| s.borrow().active_cwd.clone())
}

fn model<T: Clone + 'static>(v: Vec<T>) -> ModelRc<T> {
    ModelRc::from(Rc::new(VecModel::from(v)))
}

fn push_verify_sources(app: &AppWindow) {
    E.with(|e| {
        let e = e.borrow();
        let list = sources_of(&e.pages);
        let rows: Vec<VSource> = list
            .iter()
            .enumerate()
            .map(|(i, (_, title))| VSource { id: "".into(), title: title.as_str().into(), checked: e.verify_checked.get(i).copied().unwrap_or(true) })
            .collect();
        app.global::<EvidenceState>().set_verify_sources(model(rows));
    });
}

/// The map, built from the pages: nodes, edges, the places, the colour of every edge.
struct Built {
    nodes: Vec<NodeData>,
    edges: Vec<EdgeData>,
    layout: Layout,
    tones: Vec<i32>,
    /// 線を色とセルでまとめたもの(画面は、見えるセルだけ作る)
    cells: Vec<((i32, i32, i32), String)>,
}

fn build_all(pages: &[Value]) -> Built {
    let (nodes, edges) = build_graph(pages);
    let layout = layout(&nodes, &edges);
    let kind_of: HashMap<(&str, &str), &str> = edges.iter().map(|e| ((e.from.as_str(), e.to.as_str()), e.kind.as_str())).collect();
    let tones: Vec<i32> = layout
        .edges
        .iter()
        .map(|(a, b, _)| {
            if !nodes[*b].supported {
                3
            } else {
                match kind_of.get(&(nodes[*a].id.as_str(), nodes[*b].id.as_str())).copied().unwrap_or("") {
                    "premise" => 1,
                    "cites" => 2,
                    _ => 0,
                }
            }
        })
        .collect();
    let cells = edge_cells(&layout, &tones);
    Built { nodes, edges, layout, tones, cells }
}

fn pages_key(pages: &[Value]) -> String {
    pages.iter().map(|p| format!("{}\u{1}{}", s(p, "title"), s(p, "rev"))).collect::<Vec<_>>().join("\u{2}")
}

/// Pages come one by one: the map is made once they stop (a short wait), in the background, and only when the
/// pages are not the same as the ones the map on the screen was made from.
fn rebuild(app: &AppWindow) {
    let weak = app.as_weak();
    REBUILD.with(|t| {
        let mut t = t.borrow_mut();
        let timer = t.get_or_insert_with(slint::Timer::default);
        timer.start(slint::TimerMode::SingleShot, std::time::Duration::from_millis(120), move || {
            if let Some(app) = weak.upgrade() {
                rebuild_now(&app);
            }
        });
    });
}

fn rebuild_now(app: &AppWindow) {
    let (pages, key, generation) = E.with(|e| {
        let mut e = e.borrow_mut();
        e.generation += 1;
        (e.pages.clone(), pages_key(&e.pages), e.generation)
    });
    if E.with(|e| e.borrow().key.as_deref() == Some(key.as_str())) {
        return;
    }
    // the list of sources for the verification does not wait for the layout
    push_verify_sources(app);
    let weak = app.as_weak();
    std::thread::spawn(move || {
        let built = build_all(&pages);
        let _ = slint::invoke_from_event_loop(move || {
            let Some(app) = weak.upgrade() else { return };
            let fresh = E.with(|e| {
                let mut e = e.borrow_mut();
                if e.generation != generation {
                    return false;
                }
                e.nodes = built.nodes;
                e.edges = built.edges;
                e.layout = Some(built.layout);
                e.cells = built.cells;
                e.key = Some(key);
                true
            });
            if fresh {
                push_graph(&app);
            }
        });
    });
}

fn push_graph(app: &AppWindow) {
    E.with(|e| {
        let e = e.borrow();
        let st = app.global::<EvidenceState>();
        let Some(lay) = &e.layout else {
            st.set_nodes(model(Vec::new()));
            st.set_edges(model(Vec::new()));
            return;
        };
        st.set_nodes(model(
            e.nodes
                .iter()
                .zip(&lay.placed)
                .map(|(n, p)| GNode {
                    id: n.id.as_str().into(),
                    kind_label: kind_label(&n.kind).into(),
                    label: n.label.as_str().into(),
                    supported: n.supported,
                    selected: e.selected_node.as_deref() == Some(n.id.as_str()),
                    x: p.x,
                    y: p.y,
                    w: NODE_W,
                    h: NODE_H,
                })
                .collect(),
        ));
        st.set_edges(model(
            e.cells
                .iter()
                .map(|((tone, cx, cy), commands)| GEdge { commands: commands.as_str().into(), tone: *tone, x: *cx as f32 * CELL, y: *cy as f32 * CELL, w: CELL, h: CELL })
                .collect(),
        ));
        st.set_canvas_w(lay.width);
        st.set_canvas_h(lay.height);
    });
}

fn show_node_detail(app: &AppWindow) {
    E.with(|e| {
        let e = e.borrow();
        let st = app.global::<EvidenceState>();
        let Some(id) = &e.selected_node else { return st.set_has_node(false) };
        let Some(n) = e.nodes.iter().find(|n| &n.id == id) else { return st.set_has_node(false) };
        let label_of = |x: &str| e.nodes.iter().find(|n| n.id == x).map(|n| n.label.clone()).unwrap_or_else(|| x.to_string());
        let mut links: Vec<String> = Vec::new();
        for ed in &e.edges {
            if ed.from == n.id {
                links.push(format!("{}: {}", if ed.kind == "cites" { "出典" } else { "リンク" }, label_of(&ed.to)));
            } else if ed.to == n.id {
                links.push(format!("{}: {}", if ed.kind == "cites" { "引用しているページ" } else { "このページを参照しているページ" }, label_of(&ed.from)));
            }
        }
        st.set_node_title(n.label.as_str().into());
        st.set_node_meta(kind_label(&n.kind).into());
        st.set_node_links(links.join("\n").into());
        st.set_has_node(true);
    });
}

/// マップと検証のために、プロジェクトのページ(リンクと引用の出典つき)を取る
fn request(app: &AppWindow, host: &Host) {
    let cwd = cwd(app);
    E.with(|e| *e.borrow_mut() = E { selected_node: None, ..E::default() });
    if cwd.is_empty() {
        rebuild(app);
        return;
    }
    // the pages with their links and the sources of their quotes, in one call
    host.rpc("evidence.graph", "inocode/kb/page/graph", json!({ "cwd": cwd, "limit": MAX_PAGES }));
}

// ------------------------------------------------------------------ tab entry points

/// INOCODE_GUI_KB_BENCH=<pages>: 資料ページを作り(各ページ: リンク 8 本、引用 4 件)、マップを作る様子を測る
/// (読むたびに作り直す今のやり方で、ページの数だけ)。結果は標準エラーへ。
fn bench(app: &AppWindow, pages: usize) {
    use std::time::Instant;
    let mut seed: u64 = 12345;
    let mut next = |m: usize| {
        seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        ((seed >> 33) as usize) % m
    };
    let titles = (pages * 5 / 2).max(10);
    let list: Vec<Value> = (0..pages)
        .map(|i| {
            let links: Vec<Value> = (0..8).map(|_| json!({ "title": format!("ページ {}", next(titles)), "exists": true })).collect();
            let quotes: Vec<Value> = (0..4).map(|_| { let k = next(40); json!({ "sourceRef": format!("src_{k}"), "sourceTitle": format!("出典 {k}"), "text": "x", "matches": true }) }).collect();
            json!({ "title": format!("ページ {i}"), "confirmed": true, "links": links, "quotes": quotes })
        })
        .collect();
    E.with(|e| *e.borrow_mut() = E::default());
    // the pages arrive one by one: what the screen thread spends on each arrival
    let (mut total, mut worst) = (0u128, 0u128);
    for p in &list {
        E.with(|e| e.borrow_mut().pages.push(p.clone()));
        let t = Instant::now();
        rebuild(app);
        let us = t.elapsed().as_micros();
        total += us;
        worst = worst.max(us);
    }
    // the one build, as the background thread does it
    let pages_now = E.with(|e| e.borrow().pages.clone());
    let t = Instant::now();
    let built = build_all(&pages_now);
    let lay_us = t.elapsed().as_micros();
    let (nodes, edges, w, h) = (built.nodes.len(), built.edges.len(), built.layout.width, built.layout.height);
    let cells = built.cells.len();
    E.with(|e| {
        let mut e = e.borrow_mut();
        e.nodes = built.nodes;
        e.edges = built.edges;
        e.layout = Some(built.layout);
        e.cells = built.cells;
    });
    let build_us = 0u128;
    let t = Instant::now();
    push_graph(app);
    let push_us = t.elapsed().as_micros();
    let nodes_on_screen = nodes;
    let _ = nodes_on_screen;
    // the first picture also lays the whole window out: the 2nd and 3rd are the cost of drawing again
    let mut frames = Vec::new();
    let mut shot = true;
    for _ in 0..3 {
        let t = Instant::now();
        shot &= slint::ComponentHandle::window(app).take_snapshot().is_ok();
        frames.push(t.elapsed().as_micros());
    }
    let frame_us = frames[2];
    eprintln!(
        "bench pages={pages} rebuilds={pages} total_ms={:.1} worst_ms={:.1} | nodes={nodes} edges={edges} elements_made={} (nodes {nodes}, lines {edges} in {cells} cells) canvas={w:.0}x{h:.0} | build_ms={:.2} layout_ms={:.2} push_ms={:.2} frame_ms={:.1} (1st {:.1}) shot={shot}",
        total as f64 / 1000.0,
        worst as f64 / 1000.0,
        nodes + cells,
        build_us as f64 / 1000.0,
        lay_us as f64 / 1000.0,
        push_us as f64 / 1000.0,
        frame_us as f64 / 1000.0,
        frames[0] as f64 / 1000.0
    );
}

/// 起動時に 1 回。コールバックの登録をここで行う。
pub fn install(app: &AppWindow, host: &Host) {
    if let Some(n) = std::env::var("INOCODE_GUI_KB_BENCH").ok().and_then(|v| v.parse::<usize>().ok()) {
        let weak = app.as_weak();
        let t = slint::Timer::default();
        t.start(slint::TimerMode::SingleShot, std::time::Duration::from_secs(5), move || {
            if let Some(app) = weak.upgrade() {
                app.global::<crate::KbState>().set_evid_sub(12);
                bench(&app, n);
            }
        });
        BENCH.with(|b| *b.borrow_mut() = Some(t));
    }
    let st = app.global::<EvidenceState>();
    let (weak, h) = (app.as_weak(), host.clone());
    st.on_refresh(move || {
        if let Some(app) = weak.upgrade() {
            request(&app, &h);
        }
    });
    let (weak, h) = (app.as_weak(), host.clone());
    st.on_tab_changed(move |_| {
        if let Some(app) = weak.upgrade() {
            request(&app, &h);
        }
    });
    let weak = app.as_weak();
    st.on_select_node(move |id| {
        let Some(app) = weak.upgrade() else { return };
        E.with(|e| e.borrow_mut().selected_node = Some(id.to_string()));
        push_graph(&app);
        show_node_detail(&app);
    });
    let weak = app.as_weak();
    st.on_toggle_verify_source(move |i| {
        let Some(app) = weak.upgrade() else { return };
        E.with(|e| {
            let mut e = e.borrow_mut();
            let n = sources_of(&e.pages).len();
            if e.verify_checked.len() < n {
                e.verify_checked.resize(n, true);
            }
            if let Some(c) = e.verify_checked.get_mut(i as usize) {
                *c = !*c;
            }
        });
        push_verify_sources(&app);
    });
    let (weak, h) = (app.as_weak(), host.clone());
    st.on_verify(move || {
        let Some(app) = weak.upgrade() else { return };
        let st = app.global::<EvidenceState>();
        let text = st.get_verify_text().to_string();
        if text.trim().is_empty() {
            st.set_verify_note("検証する文書を入力してください".into());
            return;
        }
        let refs: Vec<String> = E.with(|e| {
            let e = e.borrow();
            sources_of(&e.pages)
                .into_iter()
                .enumerate()
                .filter(|(i, _)| e.verify_checked.get(*i).copied().unwrap_or(true))
                .map(|(_, (r, _))| r)
                .collect()
        });
        st.set_verifying(true);
        st.set_verify_note("".into());
        h.rpc("evidence.verify", "inocode/kb/verify", json!({ "cwd": cwd(&app), "text": text, "sourceRefs": refs }));
    });
}

/// 根拠タブが開かれるたびに呼ばれる。マップか検証を開いているときだけ、取り直す。
pub fn opened(app: &AppWindow, host: &Host) {
    if app.global::<crate::KbState>().get_evid_sub() >= 12 {
        request(app, host);
    }
}

/// クライアントのイベント (Event::Rpc / Event::Notify) が届くたびに呼ばれる。
pub fn event(app: &AppWindow, host: &Host, event: &Event) {
    if let Event::Notify { method, params } = event {
        // the project's pages changed: the map and the list of sources follow
        if method == "inocode/kb/changed" && matches!(s(params, "area").as_str(), "page" | "project") && app.global::<crate::KbState>().get_evid_sub() >= 12 {
            request(app, host);
        }
        return;
    }
    let Event::Rpc { tag, result } = event else { return };
    let st = app.global::<EvidenceState>();
    match (tag.as_str(), result) {
        ("evidence.graph", Ok(v)) => {
            let pages: Vec<Value> = payload(v).get("pages").and_then(Value::as_array).cloned().unwrap_or_default();
            st.set_note(if pages.is_empty() { "このプロジェクトに資料ページはありません".into() } else { "".into() });
            E.with(|e| e.borrow_mut().pages = pages);
            rebuild_now(app);
        }
        ("evidence.graph", Err(e)) => st.set_note(crate::errmsg::short(e).into()),
        ("evidence.verify", Ok(v)) => {
            st.set_verifying(false);
            let findings = parse_findings(v);
            st.set_verify_note(if findings.is_empty() { "指摘はありません".into() } else { "".into() });
            st.set_findings(model(findings.into_iter().map(|f| Finding { kind_label: f.kind_label.into(), text: f.text.into(), range: f.range.into() }).collect()));
        }
        ("evidence.verify", Err(e)) => {
            st.set_verifying(false);
            st.set_findings(model(Vec::new()));
            // a server without `inocode/kb/verify` says so
            st.set_verify_note(crate::errmsg::short(e).into());
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> Value {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/contract").join(name);
        let v: Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        v["response"].clone()
    }

    #[test]
    fn the_map_is_made_from_the_pages_of_the_project() {
        let page = fixture("kb-page-read.json");
        let (nodes, edges) = build_graph(&[page]);
        // the page, its unresolved link, and the source of its quote
        assert_eq!(nodes.len(), 3);
        let link = nodes.iter().find(|n| n.kind == "link").unwrap();
        assert!(!link.supported, "作っていないページは根拠なし");
        let src = nodes.iter().find(|n| n.kind == "source").unwrap();
        assert_eq!(src.label, "SQLite docs");
        assert!(src.supported);
        assert_eq!(edges.len(), 2);
        assert!(edges.iter().any(|e| e.kind == "cites") && edges.iter().any(|e| e.kind == "link"));
        let lay = layout(&nodes, &edges);
        assert_eq!(lay.placed.len(), 3);
        assert!(build_graph(&[]).0.is_empty());
    }

    #[test]
    fn the_graph_call_gives_the_map_without_reading_each_page() {
        let pages = fixture("kb-page-graph.json").get("pages").and_then(Value::as_array).cloned().unwrap();
        let (nodes, edges) = build_graph(&pages);
        // the page, its unresolved link, and the source of its quote: the same map as from page/read
        assert_eq!(nodes.len(), 3);
        assert_eq!(edges.len(), 2);
        assert!(nodes.iter().any(|n| n.kind == "link" && !n.supported));
        assert!(!pages_key(&pages).is_empty());
        let srcs = sources_of(&pages);
        assert_eq!(srcs, vec![("src_0001".to_string(), "SQLite docs".to_string())]);
    }

    #[test]
    fn verify_findings_come_with_their_place() {
        let f = parse_findings(&fixture("kb-verify.json"));
        assert_eq!(f.len(), 3);
        assert_eq!(f[0].kind_label, "原資料に無い語");
        assert_eq!(f[0].range, "22–27 文字目");
        assert_eq!(f[2].kind_label, "引用が無い");
    }

    #[test]
    fn a_quote_that_does_not_match_marks_its_source() {
        let mut page = fixture("kb-page-read.json");
        page["quotes"][0]["matches"] = json!(false);
        let (nodes, _) = build_graph(&[page]);
        assert!(!nodes.iter().find(|n| n.kind == "source").unwrap().supported);
        let two = fixture("kb-page-read.json");
        let srcs = sources_of(&[two.clone(), two]);
        assert_eq!(srcs, vec![("src_0001".to_string(), "SQLite docs".to_string())]);
    }

    #[test]
    fn layout_survives_cycles_and_empty_graphs() {
        let n = |id: &str| NodeData { id: id.into(), kind: "claim".into(), label: id.into(), status: "".into(), supported: true };
        let e = |a: &str, b: &str| EdgeData { from: a.into(), to: b.into(), kind: "premise".into() };
        let lay = layout(&[n("a"), n("b")], &[e("a", "b"), e("b", "a"), e("a", "a"), e("a", "zzz")]);
        assert_eq!(lay.placed.len(), 2);
        let empty = layout(&[], &[]);
        assert_eq!((empty.width, empty.height), (MARGIN * 2.0, MARGIN * 2.0));
    }

    #[test]
    fn layout_stays_inside_what_the_renderer_draws() {
        // a ring of 200 nodes: the longest path never ends, and the canvas must not grow with it
        let nodes: Vec<NodeData> = (0..200).map(|i| NodeData { id: format!("n{i}"), kind: "claim".into(), label: "".into(), status: "".into(), supported: true }).collect();
        let edges: Vec<EdgeData> = (0..200).map(|i| EdgeData { from: format!("n{i}"), to: format!("n{}", (i + 1) % 200), kind: "premise".into() }).collect();
        let lay = layout(&nodes, &edges);
        assert!(lay.width < 12000.0 && lay.height < 12000.0, "{} x {}", lay.width, lay.height);
        let many: Vec<NodeData> = (0..2000).map(|i| NodeData { id: format!("m{i}"), kind: "claim".into(), label: "".into(), status: "".into(), supported: true }).collect();
        let lay = layout(&many, &[]);
        assert!(lay.height < 12000.0, "{}", lay.height);
    }

    #[test]
    fn findings() {
        let f = parse_findings(&fixture("evidence-verifyDocument.json"));
        assert_eq!(f.len(), 3);
        assert_eq!(f[0].kind_label, "原資料に無い語");
        assert_eq!(f[0].text, "強盗致死罪");
        assert_eq!(f[0].range, "22–27 文字目");
        assert_eq!(f[2].kind_label, "引用が無い");
    }
}
