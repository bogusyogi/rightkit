//! Self-contained human HTML map of one Blueprint generation (#556).
//!
//! The page is rendered only from the already loaded generation. It loads no
//! external resource, runs no script from the repository, and builds its DOM
//! with `textContent`, so a symbol name cannot inject markup. The data sits in
//! one JSON island whose `<`, `>` and `&` are escaped, so it cannot close its
//! own script element.
//!
//! Zoom levels: areas (directories two levels deep) with guided flows between
//! them, then the files inside one area, then the symbols in one file or area.

use crate::model::{GraphEdge, GraphNode};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet, HashMap};

/// Most symbol rows the page carries; the rest are counted, never silently dropped.
pub const MAX_MAP_NODES: usize = 4000;
/// Most area-to-area flows the page carries.
pub const MAX_MAP_FLOWS: usize = 400;

const PLACEHOLDER: &str = "__BLUEPRINT_DATA__";

/// Area of a node: its directory, limited to two levels, so a repository lays
/// out as a short list. Nodes without a path group under `(no path)`, and
/// root-level files under `(root)`.
fn area_of(path: Option<&str>) -> String {
    let Some(path) = path.filter(|value| !value.is_empty()) else {
        return "(no path)".to_owned();
    };
    let segments: Vec<&str> = path.split('/').filter(|part| !part.is_empty()).collect();
    match segments.len() {
        0 | 1 => "(root)".to_owned(),
        count => segments[..(count - 1).min(2)].join("/"),
    }
}

#[derive(Default)]
struct Area {
    symbols: usize,
    files: BTreeSet<String>,
    inbound: usize,
    outbound: usize,
}

/// Render the self-contained HTML page for one generation.
pub fn render(
    generation_id: &str,
    provider: &str,
    complete: bool,
    nodes: &[GraphNode],
    edges: &[GraphEdge],
) -> String {
    let mut all: Vec<(String, &GraphNode)> = nodes
        .iter()
        .map(|node| (area_of(node.path.as_deref()), node))
        .collect();
    all.sort_by(|(area_a, a), (area_b, b)| {
        area_a
            .cmp(area_b)
            .then_with(|| a.path.cmp(&b.path))
            .then_with(|| a.name.cmp(&b.name))
            .then_with(|| a.id.cmp(&b.id))
    });

    let mut area_by_id: HashMap<&str, &str> = HashMap::new();
    let mut areas: BTreeMap<String, Area> = BTreeMap::new();
    for (area, node) in &all {
        area_by_id.insert(node.id.as_str(), area.as_str());
        let entry = areas.entry(area.clone()).or_default();
        entry.symbols += 1;
        if let Some(path) = &node.path {
            entry.files.insert(path.clone());
        }
    }

    let mut flows: BTreeMap<(String, String), (usize, BTreeSet<String>)> = BTreeMap::new();
    let mut edge_count = 0usize;
    for edge in edges {
        let Some(target) = edge.target.as_deref() else {
            continue;
        };
        let (Some(from), Some(to)) = (
            area_by_id.get(edge.source.as_str()).copied(),
            area_by_id.get(target).copied(),
        ) else {
            continue;
        };
        edge_count += 1;
        if from == to {
            continue;
        }
        if let Some(area) = areas.get_mut(from) {
            area.outbound += 1;
        }
        if let Some(area) = areas.get_mut(to) {
            area.inbound += 1;
        }
        let entry = flows
            .entry((from.to_owned(), to.to_owned()))
            .or_insert_with(|| (0, BTreeSet::new()));
        entry.0 += 1;
        entry.1.insert(edge.kind.clone());
    }

    let mut flow_rows: Vec<(String, String, usize, BTreeSet<String>)> = flows
        .into_iter()
        .map(|((from, to), (count, kinds))| (from, to, count, kinds))
        .collect();
    flow_rows.sort_by(|a, b| {
        b.2.cmp(&a.2)
            .then_with(|| a.0.cmp(&b.0))
            .then_with(|| a.1.cmp(&b.1))
    });
    let flow_total = flow_rows.len();
    flow_rows.truncate(MAX_MAP_FLOWS);

    let shown: Vec<Value> = all
        .iter()
        .take(MAX_MAP_NODES)
        .map(|(area, node)| {
            json!({
                "id": node.id,
                "kind": node.kind,
                "name": node.name,
                "path": node.path,
                "area": area,
            })
        })
        .collect();

    let area_rows: Vec<Value> = areas
        .iter()
        .map(|(id, area)| {
            json!({
                "id": id,
                "nodes": area.symbols,
                "files": area.files.len(),
                "in": area.inbound,
                "out": area.outbound,
            })
        })
        .collect();
    let flow_values: Vec<Value> = flow_rows
        .iter()
        .map(|(from, to, count, kinds)| {
            json!({
                "from": from,
                "to": to,
                "count": count,
                "kinds": kinds.iter().collect::<Vec<_>>(),
            })
        })
        .collect();

    let payload = json!({
        "generationId": generation_id,
        "provider": provider,
        "complete": complete,
        "nodeCount": all.len(),
        "shownNodes": shown.len(),
        "edgeCount": edge_count,
        "flowCount": flow_total,
        "shownFlows": flow_values.len(),
        "areas": area_rows,
        "flows": flow_values,
        "nodes": shown,
    });
    // JSON structure never contains these characters outside strings, so
    // escaping them is lossless and keeps the island from closing its script.
    let data = serde_json::to_string(&payload)
        .unwrap_or_else(|_| "{}".to_owned())
        .replace('<', "\\u003c")
        .replace('>', "\\u003e")
        .replace('&', "\\u0026");
    TEMPLATE.replacen(PLACEHOLDER, &data, 1)
}

const TEMPLATE: &str = r##"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>Blueprint map</title>
<style>
:root {
  --bg: #f6f7f9; --fg: #1d2229; --muted: #5b6573; --line: #d9dee6;
  --accent: #2f5bd3; --panel: #ffffff;
}
@media (prefers-color-scheme: dark) {
  :root:not([data-theme="light"]) {
    --bg: #15181d; --fg: #e6e9ee; --muted: #9aa4b2; --line: #2e3640;
    --accent: #7f9cff; --panel: #1d222a;
  }
}
:root[data-theme="dark"] {
  --bg: #15181d; --fg: #e6e9ee; --muted: #9aa4b2; --line: #2e3640;
  --accent: #7f9cff; --panel: #1d222a;
}
body { margin: 0; background: var(--bg); color: var(--fg);
  font: 14px/1.45 system-ui, -apple-system, "Segoe UI", sans-serif; }
main { max-width: 1100px; margin: 0 auto; padding: 16px; }
h1 { font-size: 20px; margin: 0 0 4px; }
h2 { font-size: 15px; margin: 18px 0 8px; }
.meta { color: var(--muted); font-size: 12px; overflow-wrap: anywhere; }
.bar { display: flex; flex-wrap: wrap; gap: 8px; align-items: center; margin: 12px 0; }
button { font: inherit; color: var(--fg); background: var(--panel);
  border: 1px solid var(--line); border-radius: 6px; padding: 6px 10px; cursor: pointer; }
button.on { border-color: var(--accent); color: var(--accent); }
input[type="search"] { font: inherit; flex: 1 1 180px; padding: 6px 10px; min-width: 0;
  border: 1px solid var(--line); border-radius: 6px; background: var(--panel); color: var(--fg); }
table { width: 100%; border-collapse: collapse; background: var(--panel); }
th, td { text-align: left; padding: 6px 8px; border-bottom: 1px solid var(--line);
  vertical-align: top; overflow-wrap: anywhere; }
tr.open { cursor: pointer; }
tr.open:hover td { background: var(--line); }
ol.flows li { margin: 6px 0; overflow-wrap: anywhere; }
.muted, .note { color: var(--muted); }
</style>
</head>
<body>
<main>
<header>
<h1>Blueprint map</h1>
<p class="meta" id="meta"></p>
</header>
<div class="bar" role="toolbar">
<button type="button" id="z-overview">Areas</button>
<button type="button" id="z-files">Files</button>
<button type="button" id="z-symbols">Symbols</button>
<button type="button" id="back" hidden>Back to areas</button>
<input type="search" id="query" placeholder="Filter" aria-label="Filter">
</div>
<div id="view"></div>
</main>
<script type="application/json" id="blueprint-data">__BLUEPRINT_DATA__</script>
<script>
(function () {
  'use strict';
  var D = JSON.parse(document.getElementById('blueprint-data').textContent);
  var view = document.getElementById('view');
  var meta = document.getElementById('meta');
  var query = document.getElementById('query');
  var back = document.getElementById('back');
  var zoomButtons = {
    overview: document.getElementById('z-overview'),
    files: document.getElementById('z-files'),
    symbols: document.getElementById('z-symbols')
  };
  var state = { zoom: 'overview', area: null, path: null, q: '' };

  function el(tag, cls, text) {
    var node = document.createElement(tag);
    if (cls) node.className = cls;
    if (text !== undefined && text !== null) node.textContent = String(text);
    return node;
  }
  function hit(parts) {
    if (!state.q) return true;
    for (var i = 0; i < parts.length; i++) {
      if (String(parts[i] === null || parts[i] === undefined ? '' : parts[i]).toLowerCase().indexOf(state.q) !== -1) return true;
    }
    return false;
  }
  function row(cells, cls) {
    var tr = el('tr', cls);
    cells.forEach(function (value) { tr.appendChild(el('td', null, value)); });
    return tr;
  }
  function table(headers) {
    var t = el('table');
    var head = el('tr');
    headers.forEach(function (h) { head.appendChild(el('th', null, h)); });
    t.appendChild(head);
    return t;
  }
  function overviewSection() {
    var wrap = el('section');
    var areas = D.areas.filter(function (a) { return hit([a.id]); });
    areas.sort(function (a, b) {
      if (b.nodes !== a.nodes) return b.nodes - a.nodes;
      return a.id < b.id ? -1 : (a.id > b.id ? 1 : 0);
    });
    wrap.appendChild(el('h2', null, 'Areas (' + areas.length + ')'));
    var t = table(['Area', 'Symbols', 'Files', 'Calls in', 'Calls out']);
    areas.forEach(function (a) {
      var tr = row([a.id, a.nodes, a.files, a.in, a.out], 'open');
      tr.addEventListener('click', function () {
        state.area = a.id; state.path = null; state.zoom = 'files'; render();
      });
      t.appendChild(tr);
    });
    wrap.appendChild(t);
    wrap.appendChild(el('h2', null, 'Guided flows between areas'));
    var flows = D.flows.filter(function (f) { return hit([f.from, f.to]); });
    if (!flows.length) wrap.appendChild(el('p', 'note', 'No flows match.'));
    var ol = el('ol', 'flows');
    flows.forEach(function (f) {
      var li = el('li');
      li.appendChild(el('strong', null, f.from));
      li.appendChild(document.createTextNode(' to '));
      li.appendChild(el('strong', null, f.to));
      li.appendChild(el('span', 'muted', ' : ' + f.count + ' edges (' + f.kinds.join(', ') + ')'));
      ol.appendChild(li);
    });
    wrap.appendChild(ol);
    if (D.flowCount > D.shownFlows) {
      wrap.appendChild(el('p', 'note', 'Showing ' + D.shownFlows + ' of ' + D.flowCount + ' flows.'));
    }
    return wrap;
  }
  function filesSection() {
    var wrap = el('section');
    var byPath = {};
    D.nodes.forEach(function (n) {
      if (state.area && n.area !== state.area) return;
      var p = n.path || '(no path)';
      if (!hit([p, n.name, n.kind])) return;
      if (!byPath[p]) byPath[p] = { count: 0, kinds: {} };
      byPath[p].count += 1;
      byPath[p].kinds[n.kind] = true;
    });
    var paths = Object.keys(byPath).sort();
    wrap.appendChild(el('h2', null, 'Files in ' + (state.area || 'all areas') + ' (' + paths.length + ')'));
    var t = table(['File', 'Symbols', 'Kinds']);
    paths.forEach(function (p) {
      var tr = row([p, byPath[p].count, Object.keys(byPath[p].kinds).join(', ')], 'open');
      tr.addEventListener('click', function () {
        state.path = p; state.zoom = 'symbols'; render();
      });
      t.appendChild(tr);
    });
    wrap.appendChild(t);
    if (D.nodeCount > D.shownNodes) {
      wrap.appendChild(el('p', 'note', 'Showing ' + D.shownNodes + ' of ' + D.nodeCount + ' symbols.'));
    }
    return wrap;
  }
  function symbolsSection() {
    var wrap = el('section');
    var rows = D.nodes.filter(function (n) {
      if (state.area && n.area !== state.area) return false;
      if (state.path && (n.path || '(no path)') !== state.path) return false;
      return hit([n.name, n.kind, n.path, n.id]);
    });
    var shownRows = rows.slice(0, 500);
    wrap.appendChild(el('h2', null, 'Symbols (' + rows.length + ')'));
    var t = table(['Kind', 'Name', 'File']);
    shownRows.forEach(function (n) {
      t.appendChild(row([n.kind, n.name || n.id, n.path || '(no path)']));
    });
    wrap.appendChild(t);
    if (rows.length > shownRows.length) {
      wrap.appendChild(el('p', 'note', 'Showing the first ' + shownRows.length + ' of ' + rows.length + '. Narrow with the filter.'));
    }
    return wrap;
  }
  function render() {
    Object.keys(zoomButtons).forEach(function (key) {
      zoomButtons[key].className = state.zoom === key ? 'on' : '';
    });
    back.hidden = !(state.area || state.path);
    var section = state.zoom === 'overview' ? overviewSection()
      : (state.zoom === 'files' ? filesSection() : symbolsSection());
    view.textContent = '';
    if (!D.nodeCount) view.appendChild(el('p', 'note', 'This generation has no symbols.'));
    view.appendChild(section);
  }
  function setMeta() {
    meta.textContent = 'Generation ' + D.generationId + ' | provider ' + D.provider +
      ' | ' + (D.complete ? 'complete' : 'partial') + ' | ' + D.nodeCount + ' symbols | ' +
      D.edgeCount + ' edges';
  }
  zoomButtons.overview.addEventListener('click', function () {
    state.zoom = 'overview'; state.area = null; state.path = null; render();
  });
  zoomButtons.files.addEventListener('click', function () {
    state.zoom = 'files'; state.path = null; render();
  });
  zoomButtons.symbols.addEventListener('click', function () {
    state.zoom = 'symbols'; render();
  });
  back.addEventListener('click', function () {
    state.zoom = 'overview'; state.area = null; state.path = null; render();
  });
  query.addEventListener('input', function () {
    state.q = query.value.trim().toLowerCase(); render();
  });
  setMeta();
  render();
})();
</script>
</body>
</html>
"##;
