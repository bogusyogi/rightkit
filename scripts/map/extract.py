#!/usr/bin/env python3
"""RightKit connection map: three passes over every crate (crates/*) and npm package (packages/*).

Pass 1 (declared): manifests: version, publish, features, every declared RightKit dependency (normal, dev, build, target),
                   modules, binaries, size.
Pass 2 (used):     what the sources actually import (`rightkit_x::`, `use rightkit_x`, `@rightkit/x`), compared with pass 1:
                   declared-but-unused and used-but-undeclared edges.
Pass 3 (runtime):  the contracts that cross process and package boundaries: environment variables (read / set / named),
                   well-known files, sockets and pipes, spawned programs; producers matched to consumers.

Writes docs/map/rightkit-map.json. Read-only over the sources; run with `python3 -I scripts/map/extract.py` from the repo root.
"""
import json
import os
import re
import sys
import tomllib
from collections import defaultdict

ROOT = os.path.abspath(os.path.join(os.path.dirname(__file__), "..", ".."))
SKIP_DIRS = {"target", "node_modules", ".git", "dist", ".right-release", ".cache", "gen", "vendor", "fixtures", "third-party"}


def rel(p):
    return os.path.relpath(p, ROOT).replace(os.sep, "/")


def walk(base, exts):
    for d, dirs, files in os.walk(base):
        # A nested project (its own Cargo.toml or package.json below the unit's root) is not part of the unit: an example app, a fixture.
        dirs[:] = [x for x in dirs if x not in SKIP_DIRS and not x.startswith(".")
                   and not (os.path.isfile(os.path.join(d, x, "Cargo.toml")) or os.path.isfile(os.path.join(d, x, "package.json")))]
        for f in files:
            if f.endswith(exts):
                yield os.path.join(d, f)


def read(p):
    try:
        with open(p, encoding="utf-8", errors="replace") as fh:
            return fh.read()
    except OSError:
        return ""


def line_of(text, idx):
    return text.count("\n", 0, idx) + 1


# ---------------------------------------------------------------- pass 1: declared
def dep_entries(table, kind, target=None):
    out = []
    for name, spec in (table or {}).items():
        real = spec.get("package", name) if isinstance(spec, dict) else name
        if not real.startswith("rightkit-") and real != "rightkit":
            continue
        e = {"name": real, "kind": kind, "local": name}
        if target:
            e["target"] = target
        if isinstance(spec, dict):
            for k in ("version", "path", "optional", "features", "default-features", "workspace"):
                if k in spec:
                    e[k.replace("-", "_")] = spec[k]
        else:
            e["version"] = spec
        out.append(e)
    return out


def crate_info(d):
    manifest = os.path.join(d, "Cargo.toml")
    try:
        with open(manifest, "rb") as fh:
            m = tomllib.load(fh)
    except Exception as e:  # noqa: BLE001
        return {"error": f"cannot parse {rel(manifest)}: {e}"}
    pkg = m.get("package", {})
    deps = dep_entries(m.get("dependencies"), "normal") + dep_entries(m.get("dev-dependencies"), "dev") + dep_entries(m.get("build-dependencies"), "build")
    for tgt, t in (m.get("target") or {}).items():
        deps += dep_entries(t.get("dependencies"), "normal", tgt) + dep_entries(t.get("dev-dependencies"), "dev", tgt)
    ext = sorted(set(list((m.get("dependencies") or {}).keys())) - {e["name"] for e in deps})
    src = os.path.join(d, "src")
    lib = read(os.path.join(src, "lib.rs"))
    mods = sorted(set(re.findall(r"^\s*pub(?:\([^)]*\))?\s+mod\s+([a-z_0-9]+)", lib, re.M)))
    priv = sorted(set(re.findall(r"^\s*mod\s+([a-z_0-9]+)", lib, re.M)))
    bins = [b.get("name") for b in m.get("bin", [])]
    if os.path.isfile(os.path.join(src, "main.rs")) and pkg.get("name") not in bins:
        bins.append(pkg.get("name"))
    files = list(walk(d, (".rs",)))
    loc = sum(read(f).count("\n") for f in files)
    tests = sum(1 for f in files for _ in re.finditer(r"#\[(?:tokio::)?test\]", read(f)))
    doc = re.search(r"^//!\s?(.*)$", lib, re.M)
    nested = sorted(rel(os.path.join(dp, x)) for dp, ds, _ in os.walk(d) for x in ds
                    if x not in SKIP_DIRS and not x.startswith(".") and os.path.isfile(os.path.join(dp, x, "Cargo.toml")))
    return {
        "lib_name": (m.get("lib") or {}).get("name") or (pkg.get("name") or "").replace("-", "_"), "nested_projects": nested,
        "kind": "crate", "name": pkg.get("name"), "dir": rel(d), "version": pkg.get("version"),
        "publish": pkg.get("publish", True), "description": pkg.get("description", ""),
        "features": {k: v for k, v in (m.get("features") or {}).items()}, "deps": deps, "external_deps": ext,
        "modules": mods, "private_modules": [x for x in priv if x not in mods], "bins": bins,
        "files": len(files), "loc": loc, "tests": tests, "doc": doc.group(1).strip() if doc else "",
        "workspace_root": "workspace" in m,
    }


def npm_info(d):
    try:
        p = json.loads(read(os.path.join(d, "package.json")))
    except Exception as e:  # noqa: BLE001
        return {"error": str(e)}
    deps = []
    for kind, key in (("normal", "dependencies"), ("dev", "devDependencies"), ("peer", "peerDependencies"), ("optional", "optionalDependencies")):
        for n, v in (p.get(key) or {}).items():
            if n.startswith("@rightkit/"):
                deps.append({"name": n, "kind": kind, "version": v})
    files = list(walk(d, (".mjs", ".js", ".ts", ".tsx", ".cjs")))
    loc = sum(read(f).count("\n") for f in files)
    tests = sum(1 for f in files if re.search(r"\.test\.|/tests?/", f))
    exp = p.get("exports")
    return {
        "kind": "npm", "name": p.get("name"), "dir": rel(d), "version": p.get("version"), "private": bool(p.get("private")),
        "description": p.get("description", ""), "deps": deps,
        "external_deps": sorted(n for n in list((p.get("dependencies") or {}).keys()) if not n.startswith("@rightkit/")),
        "exports": sorted(exp.keys()) if isinstance(exp, dict) else ([exp] if isinstance(exp, str) else []),
        "bins": sorted((p.get("bin") or {}).keys()) if isinstance(p.get("bin"), dict) else ([p.get("bin")] if p.get("bin") else []),
        "scripts": sorted((p.get("scripts") or {}).keys()), "files": len(files), "loc": loc, "test_files": tests,
    }


# ---------------------------------------------------------------- pass 2: used
RS_IDENT_USE = r"\b{id}\s*::|\buse\s+{id}\b|extern\s+crate\s+{id}\b"
JS_USE = re.compile(r"""(?:from\s+|import\s*\(\s*|require\s*\(\s*)["'](@rightkit/[a-z0-9-]+)(?:/[^"']*)?["']""")


def code_only(t, js=False):
    """`t` with comments and string literals blanked (same length, newlines kept), so an import in a comment or a string is not a use."""
    out = []
    i, n = 0, len(t)
    while i < n:
        c = t[i]
        if t.startswith("//", i):
            j = t.find("\n", i)
            j = n if j < 0 else j
            out.append(" " * (j - i)); i = j
        elif t.startswith("/*", i):
            j = t.find("*/", i + 2)
            j = n if j < 0 else j + 2
            out.append(re.sub(r"[^\n]", " ", t[i:j])); i = j
        elif not js and c == "'" and re.match(r"'(?:\\u\{[0-9a-fA-F]+\}|\\.|[^\\'\n])'", t[i:i + 12]):
            m = re.match(r"'(?:\\u\{[0-9a-fA-F]+\}|\\.|[^\\'\n])'", t[i:i + 12])
            out.append(" " * len(m.group(0))); i += len(m.group(0))  # a Rust char literal ('"', '\'', '{'), not a string
        elif not js and c == "r" and re.match(r'r#*"', t[i:i + 8]) and (i == 0 or not (t[i - 1].isalnum() or t[i - 1] == "_")):
            hashes = re.match(r'r(#*)"', t[i:i + 8]).group(1)
            end = t.find('"' + hashes, i + 2 + len(hashes))
            j = n if end < 0 else end + 1 + len(hashes)
            out.append(re.sub(r"[^\n]", " ", t[i:j])); i = j  # a raw string r#"..."#
        elif c == '"' or (js and c in "'`"):
            j = i + 1
            while j < n and t[j] != c:
                j += 2 if t[j] == "\\" else 1
            j = min(n, j + 1)
            seg = t[i:j]
            # keep module specifiers of JS imports: blank only strings not preceded by from/import/require
            keep = js and re.search(r"(?:from\s*|import\s*\(\s*|require\s*\(\s*|import\s+)$", t[max(0, i - 20):i])
            out.append(seg if keep else c + re.sub(r"[^\n]", " ", seg[1:-1]) + (seg[-1] if len(seg) > 1 else "")); i = j
        else:
            out.append(c); i += 1
    return "".join(out)


TEST_MOD = re.compile(r"#\[cfg\(test\)\]\s*(?:#\[[^\]]*\]\s*)*(?:pub(?:\([^)]*\))?\s+)?mod\s+\w+\s*\{")


def test_ranges(code):
    """[start, end) spans of `#[cfg(test)] mod x { ... }` blocks in comment- and string-free Rust source."""
    spans = []
    for m in TEST_MOD.finditer(code):
        depth, i = 1, m.end()
        while i < len(code) and depth:
            depth += {"{": 1, "}": -1}.get(code[i], 0)
            i += 1
        spans.append((m.start(), i))
    return spans


def in_spans(i, spans):
    return any(a <= i < b for a, b in spans)


def used_rust(d, own, idents):
    """`idents`: Rust identifier -> crate name (every RightKit crate's lib name, plus this unit's renamed dependencies)."""
    used = defaultdict(list)
    pattern = re.compile("|".join(f"(?P<g{i}>" + RS_IDENT_USE.format(id=re.escape(k)) + ")" for i, k in enumerate(idents)))
    keys = list(idents)
    for f in walk(d, (".rs",)):
        t = code_only(read(f))
        is_test = "/tests/" in f or "/benches/" in f or "/examples/" in f
        spans = test_ranges(t)
        for m in pattern.finditer(t):
            n = idents[keys[int(m.lastgroup[1:])]]
            if n == own:
                continue
            used[n].append({"file": rel(f), "line": line_of(t, m.start()), "test": is_test or in_spans(m.start(), spans)})
    return used


def used_js(d, own):
    used = defaultdict(list)
    for f in walk(d, (".mjs", ".js", ".ts", ".tsx", ".cjs")):
        t = code_only(read(f), js=True)
        is_test = bool(re.search(r"\.test\.|/tests?/|/test-support/", f))
        for m in JS_USE.finditer(t):
            n = m.group(1)
            if n == own:
                continue
            used[n].append({"file": rel(f), "line": line_of(t, m.start()), "test": is_test})
    return used


# ---------------------------------------------------------------- pass 3: runtime contracts
ENV_NAME = r"[A-Z][A-Z0-9]*(?:_[A-Z0-9]+)+"
RS_ENV_READ = re.compile(r"(?:env::var(?:_os)?|var_os|dev_var_os!|dev_var!|option_env!|std::env::var(?:_os)?)\s*\(\s*\"(" + ENV_NAME + r")\"|\.(?:env|get_env|var)\s*\(\s*\"(" + ENV_NAME + r")\"\s*\)")
RS_ENV_SET = re.compile(r"\.(?:env|set_var|remove_var|insert)\s*\(\s*\"(" + ENV_NAME + r")\"(?:\s*\.into\(\))?\s*,|set_var\s*\(\s*\"(" + ENV_NAME + r")\"")
# An environment entry built as a pair: `("NAME".into(), value)`, `(NAME.to_string(), value)`, `pair(NAME, value)` (world_env, launch envs).
RS_ENV_PAIR = re.compile(r"(?:\(|\bpair\s*\()\s*\"(" + ENV_NAME + r")\"(?:\.(?:into|to_string|to_owned)\(\))?\s*,")
RS_ENV_REMOVE = re.compile(r"\.(?:env_remove|remove_var)\s*\(\s*\"(" + ENV_NAME + r")\"")
RS_CONST = re.compile(r"const\s+([A-Z_0-9]+)\s*:\s*&(?:'static\s+)?str\s*=\s*\"(" + ENV_NAME + r")\"")
JS_ENV_READ = re.compile(r"(?:process\.env|\benv|\bo\.env|options\.env|deps\.env)(?:\?\.)?(?:\.(" + ENV_NAME + r")\b|\[\s*[\"'](" + ENV_NAME + r")[\"']\s*\])")
# On comment- and string-free code: an object key `NAME:` (an env object handed to a child), or `env.NAME =` / `process.env.NAME =`.
JS_ENV_SET = re.compile(r"(?:^|[{,(])\s*(" + ENV_NAME + r")\s*:(?!:)|(?:process\.env|\benv)\.(" + ENV_NAME + r")\s*=(?!=)")
JS_ENV_SET_QUOTED = re.compile(r"(?:process\.env|\benv)\[\s*[\"'](" + ENV_NAME + r")[\"']\s*\]\s*=(?!=)|[{,]\s*[\"'](" + ENV_NAME + r")[\"']\s*:")
JS_CONST = re.compile(r"(?:export\s+)?const\s+([A-Z_0-9]+)\s*=\s*[\"'](" + ENV_NAME + r")[\"']")
IDENT = r"(?:[A-Za-z_][A-Za-z0-9_]*::)*([A-Z][A-Z0-9_]+)"
RS_ENV_READ_ID = re.compile(r"(?:env::var(?:_os)?|var_os|std::env::var(?:_os)?|dev_var_os!|dev_var!|\.env|\.get_env)\s*\(\s*&?" + IDENT + r"\s*\)")
RS_ENV_SET_ID = re.compile(r"\.(?:env|insert|set_var)\s*\(\s*&?" + IDENT + r"(?:\.(?:into|to_string|to_owned)\(\))?\s*,|set_var\s*\(\s*&?" + IDENT + r"\s*,|(?:\(|\bpair\s*\()\s*&?" + IDENT + r"(?:\.(?:into|to_string|to_owned)\(\))?\s*,")
JS_ENV_READ_ID = re.compile(r"(?:process\.env|\benv|\bo\.env|options\.env|deps\.env)(?:\?\.)?\[\s*([A-Z][A-Z0-9_]+)\s*\]")
JS_ENV_SET_ID = re.compile(r"\[\s*([A-Z][A-Z0-9_]+)\s*\]\s*:|(?:process\.env|\benv)\[\s*([A-Z][A-Z0-9_]+)\s*\]\s*=(?!=)")
ENV_PREFIXES = ("RIGHTKIT_", "RIGHT_RELEASE_", "RK_", "RUSTRIGHT_", "RIGHTSUITE_", "RIGHTAPP", "CARGO_", "TAURI_", "WEBVIEW2_", "TMPDIR", "XDG_")
FILE_LIT = re.compile(r"[\"']([A-Za-z0-9_.\-]+\.(?:json|toml|token|sock|lock|jsonl|sqlite|db))[\"']")
SOCK_LIT = re.compile(r"[\"'](\\\\\\\\\.\\\\pipe\\\\[^\"']*|[^\"'\s]*\.sock)[\"']")
RS_SPAWN = re.compile(r"Command::new\s*\(\s*\"([^\"]+)\"|OwnedCommand::new\s*\(\s*\"([^\"]+)\"")
JS_SPAWN = re.compile(r"\b(?:spawn|spawnSync|execFile|execFileSync|spawnInherit|spawnCapture)\s*\(\s*[\"']([^\"']+)[\"']")


def interesting_env(n):
    return n.startswith(ENV_PREFIXES) or n in {"HOME", "USERPROFILE", "APPDATA", "LOCALAPPDATA", "TMP", "TEMP", "PATH"}


def runtime_scan(unit):
    exts = (".rs",) if unit["kind"] == "crate" else (".mjs", ".js", ".ts", ".tsx", ".cjs")
    env = defaultdict(lambda: {"read": [], "set": [], "removed": [], "const": []})
    files_named, socks, spawns = defaultdict(list), defaultdict(list), defaultdict(list)
    for f in walk(os.path.join(ROOT, unit["dir"]), exts):
        t = read(f)
        is_test = bool(re.search(r"\.test\.|/tests?/|/benches/|/examples/|/test-support/", f)) or "#[cfg(test)]" in t[: max(0, t.find("#[cfg(test)]") + 1)] and False
        # a Rust file's own test module is the tail after `#[cfg(test)]`
        spans = test_ranges(code_only(t)) if unit["kind"] == "crate" else []
        loc = lambda i: {"file": rel(f), "line": line_of(t, i), "test": is_test or in_spans(i, spans)}  # noqa: E731
        if unit["kind"] == "crate":
            for m in RS_ENV_READ.finditer(t):
                env[m.group(1) or m.group(2)]["read"].append(loc(m.start()))
            for m in RS_ENV_SET.finditer(t):
                env[m.group(1) or m.group(2)]["set"].append(loc(m.start()))
            for m in RS_ENV_PAIR.finditer(t):
                env[m.group(1)]["set"].append({**loc(m.start()), "pair": True})
            for m in RS_ENV_REMOVE.finditer(t):
                env[m.group(1)]["removed"].append(loc(m.start()))
            for m in RS_CONST.finditer(t):
                env[m.group(2)]["const"].append({**loc(m.start()), "ident": m.group(1)})
            for m in RS_SPAWN.finditer(t):
                spawns[m.group(1) or m.group(2)].append(loc(m.start()))
        else:
            for m in JS_ENV_READ.finditer(t):
                env[m.group(1) or m.group(2)]["read"].append(loc(m.start()))
            code = code_only(t, js=True)
            for m in JS_ENV_SET.finditer(code):
                n = m.group(1) or m.group(2)
                if interesting_env(n):
                    env[n]["set"].append({**loc(m.start()), "objkey": bool(m.group(1))})
            for m in JS_ENV_SET_QUOTED.finditer(t):
                n = m.group(1) or m.group(2)
                if interesting_env(n):
                    env[n]["set"].append(loc(m.start()))
            for m in JS_CONST.finditer(t):
                env[m.group(2)]["const"].append({**loc(m.start()), "ident": m.group(1)})
            for m in JS_SPAWN.finditer(t):
                spawns[m.group(1)].append(loc(m.start()))
        for m in FILE_LIT.finditer(t):
            files_named[m.group(1)].append(loc(m.start()))
        for m in SOCK_LIT.finditer(t):
            socks[m.group(1)].append(loc(m.start()))
    return env, dict(files_named), dict(socks), dict(spawns)


def ident_access(unit):
    """Environment access through a named constant: [(role, ident, site)]."""
    exts = (".rs",) if unit["kind"] == "crate" else (".mjs", ".js", ".ts", ".tsx", ".cjs")
    hits = []
    for f in walk(os.path.join(ROOT, unit["dir"]), exts):
        raw = read(f)
        code = code_only(raw, js=unit["kind"] != "crate")
        is_test = bool(re.search(r"\.test\.|/tests?/|/benches/|/examples/|/test-support/", f))
        spans = test_ranges(code) if unit["kind"] == "crate" else []
        pats = ((RS_ENV_READ_ID, "read"), (RS_ENV_SET_ID, "set")) if unit["kind"] == "crate" else ((JS_ENV_READ_ID, "read"), (JS_ENV_SET_ID, "set"))
        for pat, role in pats:
            for m in pat.finditer(code):
                ident = next(g for g in m.groups() if g)
                hits.append((role, ident, {"file": rel(f), "line": line_of(code, m.start()), "test": is_test or in_spans(m.start(), spans)}))
    return hits


APPS_ROOT = os.path.dirname(ROOT)
APPS = ["coderight", "cutright", "ember", "genright", "heardright", "legion", "mailright", "postright", "pulse", "scraperight", "sellright", "swiftright", "viewright"]
APP_ENV_READ = re.compile(r"(?:env::var(?:_os)?|var_os|dev_var_os!|dev_var!|process\.env\.|process\.env\[\s*[\"']|\.env\s*\(\s*)\"?(" + ENV_NAME + r")")


def app_info(name):
    """A product app that consumes RightKit: its pins (every Cargo.toml and package.json under it) and the RightKit-owned env it reads."""
    base = os.path.join(APPS_ROOT, name)
    deps, reads = {}, defaultdict(list)
    for d, dirs, files in os.walk(base):
        dirs[:] = [x for x in dirs if x not in SKIP_DIRS and not x.startswith(".")]
        for f in files:
            p = os.path.join(d, f)
            if f == "Cargo.toml":
                try:
                    with open(p, "rb") as fh:
                        m = tomllib.load(fh)
                except Exception:  # noqa: BLE001
                    continue
                tables = [m.get("dependencies"), m.get("dev-dependencies"), (m.get("workspace") or {}).get("dependencies")]
                tables += [t.get("dependencies") for t in (m.get("target") or {}).values()]
                for e in [x for tb in tables for x in dep_entries(tb, "normal")]:
                    deps.setdefault(e["name"], set()).add(str(e.get("version") or ("path" if e.get("path") else "workspace")))
            elif f == "package.json":
                try:
                    pj = json.loads(read(p))
                except Exception:  # noqa: BLE001
                    continue
                for key in ("dependencies", "devDependencies"):
                    for n, v in (pj.get(key) or {}).items():
                        if n.startswith("@rightkit/"):
                            deps.setdefault(n, set()).add(str(v))
            elif f.endswith((".rs", ".ts", ".tsx", ".mjs", ".js")) and "/tests/" not in p:
                txt = read(p)
                for m in APP_ENV_READ.finditer(txt):
                    if m.group(1).startswith(("RIGHTKIT_", "RIGHT_RELEASE_", "RK_")):
                        reads[m.group(1)].append({"file": os.path.relpath(p, APPS_ROOT).replace(os.sep, "/"), "line": line_of(txt, m.start()), "test": False})
    return {"kind": "app", "name": name, "dir": "../" + name, "deps": [{"name": n, "kind": "pin", "version": ",".join(sorted(v))} for n, v in sorted(deps.items())],
            "env_reads": {k: [f"{s['file']}:{s['line']}" for s in v[:4]] for k, v in sorted(reads.items())}}


def main():
    units = []
    for base, fn in (("crates", crate_info), ("packages", npm_info)):
        for name in sorted(os.listdir(os.path.join(ROOT, base))):
            d = os.path.join(ROOT, base, name)
            if not os.path.isfile(os.path.join(d, "Cargo.toml" if base == "crates" else "package.json")):
                continue
            info = fn(d)
            if "error" in info:
                print("skip", rel(d), info["error"], file=sys.stderr)
                continue
            units.append(info)
    by_name = {u["name"]: u for u in units}

    # pass 2
    for u in units:
        d = os.path.join(ROOT, u["dir"])
        if u["kind"] == "crate":
            idents = {x["lib_name"]: x["name"] for x in units if x["kind"] == "crate"}
            idents.update({e["local"].replace("-", "_"): e["name"] for e in u["deps"] if e.get("local") and e["local"] != e["name"]})
            used = used_rust(d, u["name"], idents)
        else:
            used = used_js(d, u["name"])
            # A package may be used through its bin or by name (spawned, resolved at run time): a string naming it counts.
            for f in walk(d, (".mjs", ".js", ".ts", ".cjs")):
                t = read(f)
                for e in u["deps"]:
                    if e["name"] not in used and e["name"] in t:
                        i = t.find(e["name"])
                        used[e["name"]].append({"file": rel(f), "line": line_of(t, i), "test": bool(re.search(r"\.test\.|/tests?/", f)), "by_name": True})
        u["used"] = {n: {"sites": len(s), "non_test_sites": sum(1 for x in s if not x["test"]), "first": s[0]} for n, s in used.items()}
        declared = {e["name"] for e in u["deps"]}
        declared_normal = {e["name"] for e in u["deps"] if e["kind"] in ("normal", "build", "peer", "optional")}
        u["declared_unused"] = sorted(n for n in declared if n not in used and n in by_name and n != u["name"])
        u["self_dev_dep"] = u["name"] in declared
        u["used_undeclared"] = sorted(n for n in used if n not in declared and n in by_name)
        u["used_only_in_tests_but_normal_dep"] = sorted(n for n in declared_normal if n in used and u["used"][n]["non_test_sites"] == 0)
        u["unknown_rightkit_refs"] = sorted(n for n in used if n not in by_name)

    # pass 3
    env_index = defaultdict(lambda: {"readers": defaultdict(list), "setters": defaultdict(list), "removers": defaultdict(list), "consts": defaultdict(list)})
    file_index, sock_index, spawn_index = defaultdict(lambda: defaultdict(list)), defaultdict(lambda: defaultdict(list)), defaultdict(lambda: defaultdict(list))
    scans = {u["name"]: runtime_scan(u) for u in units}
    const_values = defaultdict(set)
    for u in units:
        for k, v in scans[u["name"]][0].items():
            for c in v["const"]:
                const_values[c["ident"]].add(k)
    for u in units:
        env = scans[u["name"]][0]
        for role, ident, site in ident_access(u):
            vals = const_values.get(ident)
            if vals and len(vals) == 1:
                env[next(iter(vals))][role].append({**site, "via": ident})
    for u in units:
        env, files_named, socks, spawns = scans[u["name"]]
        # only names some unit actually reads or sets through the environment are environment variables (error codes are not)
        u["runtime"] = {"env_read": sorted(k for k, v in env.items() if v["read"]), "env_set": sorted(k for k, v in env.items() if v["set"]),
                        "files": sorted(files_named), "sockets": sorted(socks), "spawns": sorted(spawns)}
        for k, v in env.items():
            for role, key in (("read", "readers"), ("set", "setters"), ("removed", "removers"), ("const", "consts")):
                if v[role]:
                    env_index[k][key][u["name"]].extend(v[role])
        for k, s in files_named.items():
            file_index[k][u["name"]].extend(s)
        for k, s in socks.items():
            sock_index[k][u["name"]].extend(s)
        for k, s in spawns.items():
            spawn_index[k][u["name"]].extend(s)

    def nontest(sites):
        return [s for s in sites if not s["test"]]

    # An environment variable is a name some unit reads from the environment, or sets through an environment API; a bare object key
    # (`{ NAME: ... }`) counts only for a name that is read somewhere (error-code tables are objects keyed by RK_* codes too).
    def explicit_set(v):
        return any(not s.get("objkey") for ss in v["setters"].values() for s in ss)
    env_index = {k: v for k, v in env_index.items() if interesting_env(k) and (v["readers"] or explicit_set(v))}
    # Mentions: any non-test code naming the variable (its literal, or a constant holding it) — loops over names, (NAME, value) pairs,
    # forwarding lists. A role the patterns above cannot classify is still a connection.
    ident_of = defaultdict(set)
    for ident, vals in const_values.items():
        if len(vals) == 1:
            ident_of[next(iter(vals))].add(ident)
    tok = re.compile(r"[A-Za-z_][A-Za-z0-9_]*")
    mentions = defaultdict(set)
    wanted = set(env_index)
    ident_to_var = {i: v for v, ids in ident_of.items() if v in wanted for i in ids}
    for u in units:
        exts = (".rs",) if u["kind"] == "crate" else (".mjs", ".js", ".ts", ".tsx", ".cjs")
        for f in walk(os.path.join(ROOT, u["dir"]), exts):
            if re.search(r"\.test\.|/tests?/|/benches/|/test-support/", f):
                continue
            raw = read(f)
            for lit in set(re.findall(r"[\"'`](" + ENV_NAME + r")[\"'`]", raw)) & wanted:
                mentions[lit].add(u["name"])
            code = code_only(raw, js=u["kind"] != "crate")
            for name in set(tok.findall(code)) & ident_to_var.keys():
                mentions[ident_to_var[name]].add(u["name"])
    def dedupe(sites):
        seen, out = set(), []
        for s in sites:
            key = (s["file"], s["line"])
            if key not in seen:
                seen.add(key); out.append(s)
        return out
    for v in env_index.values():
        for role in ("readers", "setters", "removers", "consts"):
            for unit in list(v[role]):
                v[role][unit] = dedupe(v[role][unit])
    env_out = {}
    for k, v in sorted(env_index.items()):
        readers = {n: s for n, s in v["readers"].items()}
        setters = {n: s for n, s in v["setters"].items()}
        consts = {n: s for n, s in v["consts"].items()}
        prod_rt = {n for n, s in setters.items() if nontest(s)}
        cons_rt = {n for n, s in readers.items() if nontest(s)}
        mentioned = sorted(mentions.get(k, set()))
        others = set(mentioned) - prod_rt - cons_rt
        env_out[k] = {
            "readers": {n: [f"{s['file']}:{s['line']}{' (test)' if s['test'] else ''}" for s in ss[:6]] for n, ss in readers.items()},
            "setters": {n: [f"{s['file']}:{s['line']}{' (test)' if s['test'] else ''}" for s in ss[:6]] for n, ss in setters.items()},
            "named_in_consts": {n: [f"{s['file']}:{s['line']} {s.get('ident', '')}" for s in ss[:4]] for n, ss in consts.items()},
            "removers": {n: [f"{s['file']}:{s['line']}" for s in ss[:4]] for n, ss in v["removers"].items()},
            "mentioned_by": mentioned,
            "cross_unit": len(prod_rt | cons_rt | set(mentioned)) > 1,
            # Flags only when nothing else in RightKit names the variable: an outside input (an operator or app setting) or a dead end.
            "read_never_set_in_rk": bool(cons_rt) and not prod_rt and not (others - cons_rt) and k.startswith(("RIGHTKIT_", "RIGHT_RELEASE_", "RK_")),
            "set_never_read_in_rk": bool(prod_rt) and not cons_rt and not (others - prod_rt) and k.startswith(("RIGHTKIT_", "RIGHT_RELEASE_", "RK_")),
        }

    def flat(idx):
        return {k: {n: [f"{s['file']}:{s['line']}" for s in ss[:4]] for n, ss in v.items()} for k, v in sorted(idx.items())}

    edges = []
    for u in units:
        for e in u["deps"]:
            if e["name"] in by_name:
                used = u["used"].get(e["name"])
                edges.append({"from": u["name"], "to": e["name"], "kind": e["kind"], "optional": bool(e.get("optional")),
                              "target": e.get("target"), "path": e.get("path"), "version": e.get("version"),
                              "used_sites": used["sites"] if used else 0, "used_non_test": used["non_test_sites"] if used else 0})
        for n in u["used_undeclared"]:
            edges.append({"from": u["name"], "to": n, "kind": "undeclared", "used_sites": u["used"][n]["sites"], "used_non_test": u["used"][n]["non_test_sites"]})

    apps = [app_info(a) for a in APPS if os.path.isdir(os.path.join(APPS_ROOT, a))]
    for a in apps:
        for e in a["deps"]:
            if e["name"] in by_name:
                edges.append({"from": a["name"], "to": e["name"], "kind": "app-pin", "version": e["version"]})
        for k, sites in a["env_reads"].items():
            if k in env_out:
                env_out[k].setdefault("app_readers", {})[a["name"]] = sites
                if env_out[k]["set_never_read_in_rk"]:
                    env_out[k]["set_never_read_in_rk"] = False
                    env_out[k]["read_by_apps_only"] = True
    out = {"root": ROOT, "apps": apps, "units": units, "edges": edges, "env": env_out, "files": flat(file_index), "sockets": flat(sock_index), "spawns": flat(spawn_index)}
    dest = os.path.join(ROOT, "docs", "map", "rightkit-map.json")
    with open(dest, "w", encoding="utf-8") as fh:
        json.dump(out, fh, indent=1, sort_keys=False)
    print(f"units {len(units)} (crates {sum(u['kind']=='crate' for u in units)}, npm {sum(u['kind']=='npm' for u in units)}); edges {len(edges)}; env vars {len(env_out)}; files {len(file_index)}; sockets {len(sock_index)}; spawns {len(spawn_index)}")
    print("->", rel(dest))


if __name__ == "__main__":
    main()
