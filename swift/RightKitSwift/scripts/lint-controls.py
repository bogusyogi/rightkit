#!/usr/bin/env python3
"""Lint RightKitHub views against SwiftRight's proven-controls list.

Scans Sources/RightKitHub and Sources/RightKitHubQA (**/*.swift; python3 standard library only).
Code inside an `os(macOS)`-only conditional branch is not linted: that is how the macOS platform
services (AppKit and friends) stay out of the view rule. Everything else is linted, whatever the
file is called; a `*Mac.swift` file is skipped only to the extent its body is inside
`#if os(macOS)`, and the `#else` branch of such a conditional IS linted.

What it enforces:
  error    a FORBIDDEN control, modifier, gradient or import, in any spelling: bare, qualified
           (`SwiftUI.ProgressView`), `.init` form, or a modifier called with or without a leading dot;
  error    the data form `List(items, id:)`; use `List(selection:) { ForEach }`;
  error    a name that is clearly a SwiftUI view or scene and is neither in `apis.fromManifest` nor in
           the explicit ALLOWLIST below (for example Circle, GeometryReader, Menu);
  warning  a known SwiftUI modifier that is neither in `apis.fromManifest` nor in the ALLOWLIST.

It does NOT prove a control works on Windows; it only keeps views inside the manifest plus the
allowlist. `apis.fromManifest` is read from proven-controls.json (one `apis` object per block, merged).
`fromCaseSources` is deliberately NOT accepted: it lists what the proven cases happen to use, not
what SwiftRight proved.

Exit status: 0 when there are no errors (warnings are printed), 1 otherwise.
"""
import json
import re
import sys
from pathlib import Path

PACKAGE = Path(__file__).resolve().parent.parent
SOURCE_DIRS = [PACKAGE / "Sources" / "RightKitHub", PACKAGE / "Sources" / "RightKitHubQA"]
PROVEN = PACKAGE / "proven-controls.json"

# --- The allowlist -----------------------------------------------------------------------------
# Authority: docs/swiftui-hub.md, "Controls the sections may use" and the hub-base brief of
# 2026-10-15 ("ALLOWED IN VIEWS"). These are accepted although `apis.fromManifest` does not list
# them. Anything else that is not in the manifest is a warning (modifier) or an error (view).
ALLOWLIST_VIEWS = {
    # Listed as allowed in the design doc and the brief; not (yet) in the manifest.
    "HStack", "VStack", "Spacer", "Divider", "Color",
    # The brief requires these: HubSection.makeView returns AnyView, HubScene returns a WindowGroup.
    "AnyView", "WindowGroup",
}
ALLOWLIST_MODIFIERS = {
    # The brief's ALLOWED list, spelled as modifiers.
    "padding", "frame", "foregroundStyle", "opacity", "animation", "transition", "task", "overlay",
    "disabled", "confirmationDialog", "alert", "preferredColorScheme",
    # `List(selection:) { ForEach }` (design doc, block 16) selects rows by `.tag`.
    "tag",
    # `.toggleStyle(.switch)` is proven by SwiftRight (block 8), used for every settings switch.
    "toggleStyle",
    # SwiftRight round 5 delivered both: `layoutPriority` (a priority set under padding or frame does not
    # forward, as on macOS, so put it on the outermost view of the item) and `lineLimit(n)` with a tail
    # ellipsis. `fixedSize` is NOT delivered; it stays a warning.
    "layoutPriority", "lineLimit",
    # SwiftRight block 38 (on-hover) is delivered with caveats (independently reviewed 2026-10-16, CI run
    # 38916701641 at N=20): real-cursor enter, move and leave, adjacent views and list rows. Its
    # `proven-controls.json` entry still reads not-delivered, so the allowlist carries it, not the manifest.
    "onHover", "onTapGesture",
}

# --- Forbidden ---------------------------------------------------------------------------------
# docs/swiftui-hub.md "Do not use" and the brief's FORBIDDEN list. Matched as whole identifiers in
# code (comments and strings removed), after stripping a leading `SwiftUI.`, and also in `.init` form.
FORBIDDEN_TYPES = {
    "Path": "Path is not covered; draw with Rectangle or RoundedRectangle",
    "ProgressView": "ProgressView is not covered; use HubProgressBar",
    "NavigationSplitView": "NavigationSplitView is not covered; use HubShell",
    "NavigationStack": "NavigationStack is not covered",
    "NavigationView": "NavigationView is not covered",
    "NavigationLink": "NavigationLink is not covered",
    "Form": "Form is not covered; use HubCard and HubRow",
    "Section": "Section is not covered; use HubSettingsGroup and plain Text headings",
    "Picker": "Picker is not covered; use HubChoiceList",
    "Slider": "Slider is not covered",
    "Grid": "Grid is not covered",
    "GridRow": "Grid is not covered",
    "LinearGradient": "gradients are not covered",
    "RadialGradient": "gradients are not covered",
    "AngularGradient": "gradients are not covered",
    "EllipticalGradient": "gradients are not covered",
    "MeshGradient": "gradients are not covered",
    "Gradient": "gradients are not covered",
    "StrokeStyle": "StrokeStyle is not covered",
    "Canvas": "Canvas is not covered; draw with shapes",
    "Chart": "Swift Charts is not covered; use HubBarChart",
}

# Modifiers: caught with or without a leading dot (View-extension helpers call `background(...)`
# on self), skipping declarations (`func background(`). The closure form `name {` is checked only
# for the names that take a trailing closure.
FORBIDDEN_MODIFIERS = {
    "trim": "Circle().trim is not covered; use HubProgressBar",
    "sheet": "sheet is not covered; use .confirmationDialog or an alert",
    "searchable": "searchable is not covered; use HubSearchField",
    "toolbar": "toolbar is not covered; use HubHeader",
    "clipShape": "clipShape is not covered",
    "offset": "offset is not covered",
    "scaleEffect": "scaleEffect is not covered",
    "rotationEffect": "rotationEffect is not covered",
    "background": "background (both forms) is not covered; use HubLayered, .overlay or a ZStack",
    "linearGradient": "gradients are not covered",
    "radialGradient": "gradients are not covered",
    "angularGradient": "gradients are not covered",
    "ellipticalGradient": "gradients are not covered",
}
CLOSURE_FORM_MODIFIERS = {"background", "sheet", "toolbar"}

FORBIDDEN_IMPORTS = {"AppKit", "UIKit", "Cocoa", "Charts", "WebKit"}

# --- Names that are clearly SwiftUI ------------------------------------------------------------
# A view or scene in this set that is in neither the manifest nor the allowlist is an error.
KNOWN_SWIFTUI_VIEWS = {
    "Group", "GeometryReader", "GeometryReader3D", "TabView", "DisclosureGroup", "OutlineGroup",
    "Table", "TextEditor", "SecureField", "Stepper", "DatePicker", "ColorPicker", "Gauge",
    "ScrollViewReader", "EmptyView", "AsyncImage", "ContentUnavailableView", "ViewThatFits",
    "TimelineView", "Label", "Link", "Menu", "LazyVStack", "LazyHStack", "LazyVGrid", "LazyHGrid",
    "ControlGroup", "LabeledContent", "ShareLink", "PhotosPicker", "TextFieldLink", "Capsule",
    "Circle", "Ellipse", "UnevenRoundedRectangle", "ContainerRelativeShape", "HSplitView",
    "VSplitView", "Map", "Window", "MenuBarExtra", "Settings", "DocumentGroup", "SpriteView",
    "SceneView", "EquatableView", "ToolbarItem", "ToolbarItemGroup", "TextField", "Text", "Image",
    "Rectangle", "RoundedRectangle", "ZStack", "HStack", "VStack", "ScrollView", "List", "ForEach",
    "Button", "Toggle", "Spacer", "Divider", "Color", "AnyView", "WindowGroup", "Layout",
}

KNOWN_SWIFTUI_MODIFIERS = {
    "accessibilityAddTraits", "accessibilityElement", "accessibilityHidden", "accessibilityHint",
    "accessibilityLabel", "accessibilityValue", "allowsHitTesting", "aspectRatio", "badge",
    "blur", "bold", "border", "brightness", "buttonBorderShape", "clipped", "colorMultiply",
    "compositingGroup", "containerRelativeFrame", "contentShape", "contextMenu", "contrast",
    "controlSize", "cornerRadius", "disableAutocorrection", "drawingGroup", "dropDestination",
    "fixedSize", "focusable", "focused", "fontDesign", "fontWeight", "foregroundColor",
    "foregroundStyle", "gesture", "help", "hidden", "highPriorityGesture", "id", "ignoresSafeArea",
    "interactiveDismissDisabled", "italic", "kerning", "layoutPriority", "lineSpacing",
    "listRowBackground", "listRowInsets", "listRowSeparator", "mask", "menuStyle", "minimumScaleFactor",
    "monospaced", "monospacedDigit", "multilineTextAlignment", "navigationTitle", "onChange",
    "onDisappear", "onDrag", "onDrop", "onKeyPress", "onLongPressGesture", "onTapGesture",
    "padding", "pickerStyle", "popover", "position", "preferredColorScheme", "redacted",
    "refreshable", "rotation3DEffect", "safeAreaInset", "saturation", "scaledToFill", "scaledToFit",
    "scenePadding", "scrollContentBackground", "scrollDisabled", "scrollIndicators", "selectionDisabled",
    "shadow", "strikethrough", "submitLabel", "symbolRenderingMode", "symbolVariant", "textCase",
    "textContentType", "textSelection", "tint", "toggleStyle", "tracking", "truncationMode",
    "underline", "unredacted", "zIndex", "frame", "fill", "stroke", "strokeBorder", "overlay",
    "opacity", "animation", "transition", "task", "disabled", "alert", "confirmationDialog",
    "environment", "font", "lineLimit", "tag", "labelsHidden", "buttonStyle", "textFieldStyle",
    "onSubmit", "keyboardShortcut", "listStyle", "onAppear", "resizable", "imageScale",
    "system",  # Font.system(...): only Font.custom is in the manifest
    "defaultSize", "windowResizability", "defaultScrollAnchor", "onPreferenceChange",
    "preference", "transaction", "focusedValue", "searchSuggestions", "onMove", "onDelete",
}


def load_manifest():
    data = json.loads(PROVEN.read_text())
    apis = []
    if isinstance(data.get("apis"), dict):
        apis.append(data["apis"])
    for block in data.get("blocks", []):
        if isinstance(block.get("apis"), dict):
            apis.append(block["apis"])
    manifest = set()
    for entry in apis:
        for name in entry.get("fromManifest", []):
            manifest.update(name.split("."))
    if not manifest:
        sys.exit(f"lint-controls: no apis.fromManifest in {PROVEN}")
    return manifest


def strip_comments_and_strings(text):
    """Blank out comments and string literals, keeping line numbers."""
    out = []
    i, n = 0, len(text)
    while i < n:
        two = text[i:i + 2]
        if text.startswith('"""', i):  # multi-line string
            j = text.find('"""', i + 3)
            j = n if j < 0 else j + 3
            out.append(re.sub(r"[^\n]", " ", text[i:j]))
            i = j
        elif two == "//":
            j = text.find("\n", i)
            j = n if j < 0 else j
            out.append(" " * (j - i))
            i = j
        elif two == "/*":  # Swift block comments nest
            depth, j = 1, i + 2
            while j < n and depth:
                if text.startswith("/*", j):
                    depth, j = depth + 1, j + 2
                elif text.startswith("*/", j):
                    depth, j = depth - 1, j + 2
                else:
                    j += 1
            out.append(re.sub(r"[^\n]", " ", text[i:j]))
            i = j
        elif text[i] == '"':
            j = i + 1
            while j < n and text[j] != '"' and text[j] != "\n":
                j += 2 if text[j] == "\\" else 1
            j = min(j + 1, n)
            out.append('""' + " " * (j - i - 2))
            i = j
        else:
            out.append(text[i])
            i += 1
    return "".join(out)


def blank_macos_branches(code):
    """Blank (keeping line numbers) every line inside an os(macOS)-only conditional branch.

    `#if os(macOS)` ... is mac-only; its `#else` is not. `#if !os(macOS)` ... `#else` is the reverse.
    `#elseif` and every other condition (canImport, `||`, ...) count as lintable.
    Returns (code, blanked_line_count).
    """
    stack = []  # one frame per open #if: {"mac": bool, "else_mac": bool}
    out, blanked = [], 0
    for line in code.split("\n"):
        t = line.strip()
        directive = re.match(r"#(if|elseif|else|endif)\b(.*)$", t)
        if directive:
            kind, rest = directive.group(1), directive.group(2).replace(" ", "")
            if kind == "if":
                stack.append({"mac": rest == "os(macOS)", "else_mac": rest == "!os(macOS)"})
            elif kind == "elseif" and stack:
                stack[-1] = {"mac": rest == "os(macOS)", "else_mac": False}
            elif kind == "else" and stack:
                stack[-1] = {"mac": stack[-1]["else_mac"], "else_mac": False}
            elif kind == "endif" and stack:
                stack.pop()
            out.append("")  # the directive line itself is never code
            continue
        if any(frame["mac"] for frame in stack):
            blanked += 1 if t else 0
            out.append("")
        else:
            out.append(line)
    return "\n".join(out), blanked


def line_of(text, pos):
    return text.count("\n", 0, pos) + 1


def lint_file(path, manifest):
    rel = rel_path(path)
    code, blanked = blank_macos_branches(strip_comments_and_strings(path.read_text()))
    code = re.sub(r"\bSwiftUI\s*\.\s*(?=[A-Za-z_])", "", code)  # SwiftUI.ProgressView -> ProgressView
    errors, warnings, accepted = [], [], 0
    allowed_views = manifest | ALLOWLIST_VIEWS
    allowed_modifiers = manifest | ALLOWLIST_MODIFIERS

    for m in re.finditer(r"^[ \t]*(?:@\w+[ \t]+)?import[ \t]+(?:(?:struct|class|enum|protocol|func|var|let|typealias)[ \t]+)?(\w+)", code, re.M):
        if m.group(1) in FORBIDDEN_IMPORTS:
            errors.append((rel, line_of(code, m.start()), f"import {m.group(1)} is not allowed in views; Mac-only code goes inside #if os(macOS)"))

    # Forbidden types: any use as an identifier (call, `.init`, annotation, generic argument), not a member.
    for m in re.finditer(r"(?<![\w.])([A-Z][A-Za-z0-9]*)\b", code):
        name = m.group(1)
        if name in FORBIDDEN_TYPES:
            errors.append((rel, line_of(code, m.start()), f"{name}: {FORBIDDEN_TYPES[name]}"))

    # Clearly-SwiftUI view or scene names: calls, generics, trailing closures and `.init`.
    for m in re.finditer(r"(?<![\w.])([A-Z][A-Za-z0-9]*)\s*(?:[({<]|\.\s*init\b)", code):
        name = m.group(1)
        if name in FORBIDDEN_TYPES:
            continue
        if name in KNOWN_SWIFTUI_VIEWS:
            if name in allowed_views:
                accepted += 1
            else:
                errors.append((rel, line_of(code, m.start()), f"{name} is a SwiftUI view that is not in apis.fromManifest or the allowlist"))
        elif name in manifest:
            accepted += 1

    for m in re.finditer(r"\bCircle\s*\(\s*\)\s*\.\s*trim\b", code):
        errors.append((rel, line_of(code, m.start()), "Circle().trim is not covered; use HubProgressBar"))

    for m in re.finditer(r"(?<![\w.])List\s*(?:\.\s*init\s*)?\(([^)]*)\)", code):
        args = m.group(1).strip()
        if args and not args.startswith("selection:"):
            errors.append((rel, line_of(code, m.start()), "List(data, id:) is not covered; use List(selection:) { ForEach }"))

    # Forbidden modifiers, with or without a leading dot, never a declaration.
    for name, why in FORBIDDEN_MODIFIERS.items():
        opener = r"[({]" if name in CLOSURE_FORM_MODIFIERS else r"\("
        for m in re.finditer(rf"(?<![A-Za-z0-9_])({name})\s*{opener}", code):
            before = code[max(0, m.start() - 12):m.start()]
            if re.search(r"\bfunc\s+$", before):
                continue  # a declaration, e.g. a View extension helper named after it
            errors.append((rel, line_of(code, m.start()), f"{name}: {why}"))

    # `.gradient` member uses (Color.red.gradient, ...).
    for m in re.finditer(r"\.\s*gradient\b", code):
        errors.append((rel, line_of(code, m.start()), ".gradient: gradients are not covered"))

    for m in re.finditer(r"\.\s*([a-z][A-Za-z0-9]*)\s*[({]", code):
        name = m.group(1)
        if name in FORBIDDEN_MODIFIERS:
            continue
        if name in KNOWN_SWIFTUI_MODIFIERS:
            if name in allowed_modifiers:
                accepted += 1
            else:
                warnings.append((rel, line_of(code, m.start()), f".{name} is a SwiftUI modifier that is not in apis.fromManifest or the allowlist"))

    whole_file_mac = blanked > 0 and not code.strip()
    return sorted(set(errors)), sorted(set(warnings)), accepted, blanked, whole_file_mac


def rel_path(path):
    """Path shown in messages: relative to the package, else to the working directory."""
    for base in (PACKAGE, Path.cwd()):
        try:
            return path.relative_to(base)
        except ValueError:
            pass
    return path


def main():
    manifest = load_manifest()
    # Source folders to lint: the arguments if given (an app linting its own views against the same
    # rules), else this package's view targets.
    dirs = [Path(a).resolve() for a in sys.argv[1:]] or SOURCE_DIRS
    files = sorted(f for d in dirs for f in d.rglob("*.swift"))
    if not files:
        sys.exit(f"lint-controls: no Swift files under {dirs}")
    errors, warnings, mac_only, accepted, mac_lines = [], [], [], 0, 0
    for path in files:
        e, w, a, blanked, whole = lint_file(path, manifest)
        errors += e
        warnings += w
        accepted += a
        mac_lines += blanked
        if whole:
            mac_only.append(rel_path(path))
    for rel, line, msg in warnings:
        print(f"{rel}:{line}: warning: {msg}")
    for rel, line, msg in errors:
        print(f"{rel}:{line}: error: {msg}")
    print(
        f"lint-controls: {len(files)} files scanned, {mac_lines} lines skipped inside os(macOS) branches, "
        f"{len(manifest)} names in apis.fromManifest, {accepted} accepted uses, "
        f"{len(warnings)} warnings, {len(errors)} errors"
    )
    for rel in mac_only:
        print(f"  wholly os(macOS): {rel}")
    return 1 if errors else 0


if __name__ == "__main__":
    sys.exit(main())
