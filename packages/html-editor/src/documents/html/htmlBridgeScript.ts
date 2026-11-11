import {
  INTERACTIVE_HTML_LINK_MESSAGE,
  INTERACTIVE_HTML_LINK_HOVER,
  INTERACTIVE_HTML_APPLY_CSS,
  INTERACTIVE_HTML_APPLY_THEME,
  INTERACTIVE_HTML_ACTIVITY,
  INTERACTIVE_HTML_SCROLL,
  INTERACTIVE_HTML_SCROLL_TOP,
  INTERACTIVE_HTML_ZOOM,
  INTERACTIVE_HTML_ZOOM_RESET,
  INTERACTIVE_HTML_FIND_OPEN,
  INTERACTIVE_HTML_HISTORY,
  INTERACTIVE_HTML_OPEN_DEVTOOLS,
  INTERACTIVE_HTML_FIND,
  INTERACTIVE_HTML_FIND_STEP,
  INTERACTIVE_HTML_FIND_CLEAR,
  INTERACTIVE_HTML_FIND_RESULT,
  INTERACTIVE_HTML_SELECT_ALL,
  INTERACTIVE_HTML_INSPECT_MODE,
  INTERACTIVE_HTML_INSPECT_TARGET,
  INTERACTIVE_HTML_INSPECT_MENU,
  INTERACTIVE_HTML_CONTEXT_TARGET,
  INTERACTIVE_HTML_SCROLL_TO_INSPECT,
  INTERACTIVE_HTML_PRINT,
  INTERACTIVE_HTML_PRINT_CONTENT,
  INTERACTIVE_HTML_EDIT_VISUAL,
  INTERACTIVE_HTML_EDIT_REFUSED,
} from "./htmlInteractiveProtocol.js";

export const HTML_SURFACE_MESSAGE = "rk-document:interactive-html-surface";

function surfaceTokens(value: string): [string, string] | null {
  if (!value.startsWith("rgb")) return null;
  const parts = value.match(/[\d.]+/g)?.map(Number) ?? [];
  if (parts.length < 3 || (parts[3] !== undefined && parts[3] < 0.99)) return null;
  const linear = parts.slice(0, 3).map((n) => {
    const c = n / 255;
    return c <= 0.04045 ? c / 12.92 : ((c + 0.055) / 1.055) ** 2.4;
  });
  const luminance = 0.2126 * linear[0] + 0.7152 * linear[1] + 0.0722 * linear[2];
  return [value, luminance > 0.179 ? "#111111" : "#ffffff"];
}

export function publishHtmlSurface(iframe: HTMLIFrameElement, value: string): boolean {
  const tokens = surfaceTokens(value);
  if (!tokens) return false;
  iframe.dataset.vrHtmlSurface = tokens[0];
  if (!iframe.closest('[data-vr-pane-state="active"]')) return true;
  document.documentElement.style.setProperty("--active-html-surface", tokens[0]);
  document.documentElement.style.setProperty("--active-html-text", tokens[1]);
  return true;
}

// Injected into the opaque-origin interactive preview iframe; protocol constants are interpolated.
export function bridgeScript(sessionId: string, pagePath = ""): string {
  return `
(() => {
  const sessionId = ${JSON.stringify(sessionId)};
  const pagePath = ${JSON.stringify(pagePath)};
  // In packaged WebView2, the app's visible origin and the postMessage target origin can diverge
  // enough that a pinned target silently drops child→parent messages. Keep delivery reliable and
  // trust the parent-side event.source + sessionId guards for containment.
  const parentTarget = "*";
  const linkType = ${JSON.stringify(INTERACTIVE_HTML_LINK_MESSAGE)};
  const linkHoverType = ${JSON.stringify(INTERACTIVE_HTML_LINK_HOVER)};
  const cssType = ${JSON.stringify(INTERACTIVE_HTML_APPLY_CSS)};
  const themeType = ${JSON.stringify(INTERACTIVE_HTML_APPLY_THEME)};
  const activityType = ${JSON.stringify(INTERACTIVE_HTML_ACTIVITY)};
  const scrollType = ${JSON.stringify(INTERACTIVE_HTML_SCROLL)};
  const scrollTopType = ${JSON.stringify(INTERACTIVE_HTML_SCROLL_TOP)};
  const zoomType = ${JSON.stringify(INTERACTIVE_HTML_ZOOM)};
  const zoomResetType = ${JSON.stringify(INTERACTIVE_HTML_ZOOM_RESET)};
  const findOpenType = ${JSON.stringify(INTERACTIVE_HTML_FIND_OPEN)};
  const historyType = ${JSON.stringify(INTERACTIVE_HTML_HISTORY)};
  const openDevtoolsType = ${JSON.stringify(INTERACTIVE_HTML_OPEN_DEVTOOLS)};
  const findType = ${JSON.stringify(INTERACTIVE_HTML_FIND)};
  const stepType = ${JSON.stringify(INTERACTIVE_HTML_FIND_STEP)};
  const clearType = ${JSON.stringify(INTERACTIVE_HTML_FIND_CLEAR)};
  const resultType = ${JSON.stringify(INTERACTIVE_HTML_FIND_RESULT)};
  const selectAllType = ${JSON.stringify(INTERACTIVE_HTML_SELECT_ALL)};
  const inspectModeType = ${JSON.stringify(INTERACTIVE_HTML_INSPECT_MODE)};
  const inspectTargetType = ${JSON.stringify(INTERACTIVE_HTML_INSPECT_TARGET)};
  const inspectMenuType = ${JSON.stringify(INTERACTIVE_HTML_INSPECT_MENU)};
  const contextTargetType = ${JSON.stringify(INTERACTIVE_HTML_CONTEXT_TARGET)};
  const scrollToInspectType = ${JSON.stringify(INTERACTIVE_HTML_SCROLL_TO_INSPECT)};
  const printType = ${JSON.stringify(INTERACTIVE_HTML_PRINT)};
  const printContentType = ${JSON.stringify(INTERACTIVE_HTML_PRINT_CONTENT)};
  const editVisualType = ${JSON.stringify(INTERACTIVE_HTML_EDIT_VISUAL)};
  const editRefusedType = ${JSON.stringify(INTERACTIVE_HTML_EDIT_REFUSED)};
  const surfaceType = ${JSON.stringify(HTML_SURFACE_MESSAGE)};
  let lastSurface = "";
  const reportSurface = () => {
    const body = document.body ? getComputedStyle(document.body).backgroundColor : "";
    const root = getComputedStyle(document.documentElement).backgroundColor;
    const next = body + "|" + root;
    if (next === lastSurface) return;
    lastSurface = next;
    parent.postMessage({ type: surfaceType, sessionId, body, root }, parentTarget);
  };
  const surfaceObserver = new MutationObserver(() => requestAnimationFrame(reportSurface));
  surfaceObserver.observe(document.documentElement, { attributes: true, attributeFilter: ["style", "class"] });
  document.addEventListener("DOMContentLoaded", () => {
    if (document.body) surfaceObserver.observe(document.body, { attributes: true, attributeFilter: ["style", "class"] });
    reportSurface();
  });
  window.addEventListener("load", reportSurface);
  // Inactive-tab policy (reversible): while the tab is inactive or the window hidden, media is paused
  // (resumed on activation if it was playing), timeouts and animation frames are deferred until
  // activation, and intervals run at most once per second. Nothing is destroyed; page state is kept.
  let inactive = false;
  const heldMedia = new Set();
  const deferred = [];
  const deferredFrames = [];
  const realTimeout = window.setTimeout.bind(window);
  const realInterval = window.setInterval.bind(window);
  const realFrame = window.requestAnimationFrame.bind(window);
  window.setTimeout = function(fn, delay) {
    if (typeof fn !== "function") return realTimeout.apply(window, arguments);
    const args = Array.prototype.slice.call(arguments, 2);
    return realTimeout(() => {
      if (inactive) deferred.push(() => fn.apply(window, args));
      else fn.apply(window, args);
    }, delay);
  };
  window.setInterval = function(fn, delay) {
    if (typeof fn !== "function") return realInterval.apply(window, arguments);
    const args = Array.prototype.slice.call(arguments, 2);
    let last = 0;
    return realInterval(() => {
      if (inactive) {
        const now = Date.now();
        if (now - last < 1000) return;
        last = now;
      }
      fn.apply(window, args);
    }, delay);
  };
  window.requestAnimationFrame = function(cb) {
    if (typeof cb !== "function") return realFrame(cb);
    return realFrame((t) => {
      if (inactive) deferredFrames.push(cb);
      else cb(t);
    });
  };
  const pauseMedia = (el) => {
    if (el.paused || el.ended) return;
    heldMedia.add(el);
    el.pause();
  };
  document.addEventListener("play", (event) => {
    if (inactive && event.target && event.target.pause) pauseMedia(event.target);
  }, true);
  const setActivity = (active) => {
    const next = !active;
    if (next === inactive) return;
    inactive = next;
    if (inactive) {
      document.querySelectorAll("audio,video").forEach(pauseMedia);
      return;
    }
    heldMedia.forEach((el) => { try { const p = el.play(); if (p && p.catch) p.catch(() => {}); } catch {} });
    heldMedia.clear();
    deferred.splice(0).forEach((run) => realTimeout(run, 0));
    deferredFrames.splice(0).forEach((cb) => realFrame(cb));
  };
  // Element under the last right-click — scrolled back into view after "View source" reflows the page.
  let lastInspectEl = null;

  const open = window.open;
  window.open = function(url) {
    if (typeof url === "string" && url) {
      parent.postMessage({ type: linkType, sessionId, href: url }, parentTarget);
      return null;
    }
    return open.apply(window, arguments);
  };

  // Script-driven navigation (document.location = x, location.assign/replace, meta/anchor clicks the
  // click handler did not see) would replace this document, dropping the bridge and the preview.
  // Cancel any cancelable cross-document navigation and report the destination to the parent, which
  // routes it like a link click (relative page -> sub-page, external -> OS browser). Same-document
  // (#fragment) navigations and non-cancelable traversals are left alone.
  try {
    const nav = window.navigation;
    if (nav && typeof nav.addEventListener === "function") {
      nav.addEventListener("navigate", (event) => {
        if (!event.cancelable || event.hashChange || event.navigationType === "reload") return;
        let dest = event.destination && event.destination.url;
        if (typeof dest !== "string" || !dest) return;
        event.preventDefault();
        // The preview is served from its own origin (viewright-preview), so a relative script
        // navigation resolves against that origin. Report it as the page-relative path the author
        // wrote, never as an absolute viewright-preview URL the parent would treat as external.
        try {
          const own = new URL(location.href);
          const target = new URL(dest);
          if (target.protocol === own.protocol && target.host === own.host) {
            if (target.pathname === own.pathname) return;
            dest = target.pathname.replace(/^\\/+/, "") + target.search + target.hash;
          }
        } catch (_) {}
        parent.postMessage({ type: linkType, sessionId, href: dest }, parentTarget);
      });
    }
  } catch (_) {}

  let inspectActive = false;
  function normalizeInspectText(value) {
    return String(value || "").replace(/\\s+/g, " ").trim();
  }
  function inspectTextFromPoint(event) {
    const selectedText = normalizeInspectText(window.getSelection && window.getSelection().toString()).slice(0, 180);
    let targetElement = null;
    try {
      targetElement = document.elementFromPoint ? document.elementFromPoint(event.clientX, event.clientY) : null;
    } catch (_) {}
    targetElement = targetElement || (event.target && event.target.nodeType === Node.ELEMENT_NODE ? event.target : null);
    let node = null;
    try {
      const range = document.caretRangeFromPoint ? document.caretRangeFromPoint(event.clientX, event.clientY) : null;
      node = range && range.startContainer;
    } catch (_) {}
    if (node && targetElement && !targetElement.contains(node)) node = null;
    if (!node || node.nodeType !== Node.TEXT_NODE) {
      const walkerRoot = targetElement || document.body;
      try {
        const walker = document.createTreeWalker(walkerRoot, NodeFilter.SHOW_TEXT, {
          acceptNode(n) {
            return normalizeInspectText(n.nodeValue).length ? NodeFilter.FILTER_ACCEPT : NodeFilter.FILTER_REJECT;
          },
        });
        node = walker.nextNode();
      } catch (_) {}
    }
    const text = normalizeInspectText(node && node.nodeValue);
    // Anchor/image identity comes from the geometric hit element (targetElement), never from the
    // text node's parent: the text node is only a best-effort preview string and can land in a
    // different anchor than the one actually under the pointer (e.g. a block with multiple
    // links), or be absent entirely (an image-only block has no text node at all).
    const anchor = targetElement && targetElement.closest ? targetElement.closest("a[href]") : null;
    const image = targetElement && targetElement.closest ? targetElement.closest("img[src]") : null;
    if (!text && !anchor && !image) return null;
    const parent = (node && node.parentElement) || (targetElement && targetElement.closest ? targetElement.closest("*") : null);
    return {
      text: text.slice(0, 180),
      selectedText: selectedText || undefined,
      tagName: parent && parent.tagName ? parent.tagName.toLowerCase() : undefined,
      id: parent && parent.id ? parent.id : undefined,
      classNames: parent && parent.classList ? Array.from(parent.classList).slice(0, 8) : [],
      href: anchor ? anchor.getAttribute("href") || undefined : undefined,
      src: image ? image.getAttribute("src") || undefined : undefined,
    };
  }

  document.addEventListener("contextmenu", (event) => {
    // Plain right-click stays native on every platform so Inspect Element opens DevTools on this
    // exact opaque-frame node. Shift+right-click keeps the host app source/link menu available.
    try { lastInspectEl = (document.elementFromPoint && document.elementFromPoint(event.clientX, event.clientY)) || event.target; } catch (_) { lastInspectEl = event.target; }
    const target = inspectTextFromPoint(event);
    if (target) parent.postMessage(Object.assign({ type: contextTargetType, sessionId }, target), parentTarget);
    if (event.shiftKey && target) {
      event.preventDefault();
      parent.postMessage(Object.assign({ type: inspectMenuType, sessionId, x: event.clientX, y: event.clientY }, target), parentTarget);
    }
  }, true);

  function inlineStyle(element) {
    try {
      const style = getComputedStyle(element);
      let backgroundElement = element;
      let backgroundColor = "";
      while (backgroundElement) {
        const candidate = getComputedStyle(backgroundElement).backgroundColor || "";
        const normalized = candidate.trim().toLowerCase();
        const alpha = normalized.match(/^(?:rgba|hsla)\\([^)]*,\\s*([\\d.]+)\\s*\\)$/);
        if (normalized && normalized !== "transparent" && !(alpha && Number.parseFloat(alpha[1]) === 0)) {
          backgroundColor = candidate;
          break;
        }
        backgroundElement = backgroundElement.parentElement;
      }
      return {
        fontFamily: style.fontFamily,
        fontSize: style.fontSize,
        fontWeight: style.fontWeight,
        fontStyle: style.fontStyle,
        lineHeight: style.lineHeight,
        letterSpacing: style.letterSpacing,
        color: style.color,
        backgroundColor,
        textAlign: style.textAlign,
        padding: style.padding,
      };
    } catch (_) { return undefined; }
  }

  function currentBlockFingerprint(element) {
    const text = String(element.textContent || "").replace(/\\s+/g, " ").trim().slice(0, 180);
    return [
      String(element.tagName || "").toLowerCase(),
      String(element.id || ""),
      String(element.className || ""),
      text,
    ].join("|");
  }

  // Only a real user double-click can request editing. Some WKWebView pointer paths emit the
  // second click with detail=2 but omit dblclick, so both routes share one publisher. Source
  // identity was stamped before scripts ran; parent still treats every field as untrusted.
  let lastEditHit = null;
  let lastEditAt = 0;
  let lastRefusedAt = 0;
  function textOffsetIn(element, node, offset) {
    if (!node || node.nodeType !== Node.TEXT_NODE || !element.contains(node)) return null;
    try {
      const prefix = document.createRange();
      prefix.selectNodeContents(element);
      prefix.setEnd(node, offset);
      return prefix.toString().length;
    } catch (_) { return null; }
  }
  function textOffsetFromPoint(element, x, y) {
    try {
      const range = document.caretRangeFromPoint ? document.caretRangeFromPoint(x, y) : null;
      const direct = range && textOffsetIn(element, range.startContainer, range.startOffset);
      if (direct !== null && direct !== undefined) return direct;
    } catch (_) {}
    try {
      const position = document.caretPositionFromPoint ? document.caretPositionFromPoint(x, y) : null;
      const direct = position && textOffsetIn(element, position.offsetNode, position.offset);
      if (direct !== null && direct !== undefined) return direct;
    } catch (_) {}
    try {
      const walker = document.createTreeWalker(element, NodeFilter.SHOW_TEXT);
      let best = null;
      let visited = 0;
      for (let node = walker.nextNode(); node && visited < 4096; node = walker.nextNode()) {
        const value = String(node.nodeValue || "");
        for (let index = 0; index < value.length && visited < 4096; index += 1, visited += 1) {
          const character = document.createRange();
          character.setStart(node, index);
          character.setEnd(node, index + 1);
          if (typeof character.getBoundingClientRect !== "function") continue;
          const rect = character.getBoundingClientRect();
          if (!rect.width && !rect.height) continue;
          const dx = x < rect.left ? rect.left - x : x > rect.right ? x - rect.right : 0;
          const dy = y < rect.top ? rect.top - y : y > rect.bottom ? y - rect.bottom : 0;
          const distance = dx * dx + dy * dy;
          if (!best || distance < best.distance) {
            best = {
              node,
              offset: x > rect.left + rect.width / 2 ? index + 1 : index,
              distance,
            };
          }
          if (distance === 0) break;
        }
        if (best && best.distance === 0) break;
      }
      if (best) return textOffsetIn(element, best.node, best.offset);
    } catch (_) {}
    return null;
  }
  function editTextSelection(element, event) {
    const raw = String(element.textContent || "");
    if (!raw) return undefined;
    try {
      const selection = window.getSelection && window.getSelection();
      if (selection && !selection.isCollapsed && selection.rangeCount) {
        const range = selection.getRangeAt(0);
        const from = textOffsetIn(element, range.startContainer, range.startOffset);
        const to = textOffsetIn(element, range.endContainer, range.endOffset);
        if (from !== null && to !== null && from !== to)
          return { from: Math.min(from, to), to: Math.max(from, to) };
      }
    } catch (_) {}
    const point = textOffsetFromPoint(element, event.clientX, event.clientY);
    if (point !== null) {
      let from = Math.max(0, Math.min(point, raw.length));
      let to = from;
      while (from > 0 && !/\\s/.test(raw.charAt(from - 1))) from -= 1;
      while (to < raw.length && !/\\s/.test(raw.charAt(to))) to += 1;
      if (from < to) return { from, to };
    }
    // Native WebKit can withhold both Selection and caret APIs. Keep edit usable by selecting the
    // exact source-backed element rather than opening with an unusable collapsed range.
    const trimmed = raw.trim();
    const from = raw.indexOf(trimmed);
    return trimmed ? { from: Math.max(0, from), to: Math.max(0, from) + trimmed.length } : undefined;
  }
  // Form controls, media and page-owned editable regions are never offered the Source notice.
  const NO_REFUSAL_SELECTOR = "form,input,select,textarea,button,video,audio,object,embed,iframe,canvas,[contenteditable]:not([contenteditable=false])";
  function refusalAllowedAt(event) {
    let el = null;
    try { el = document.elementFromPoint ? document.elementFromPoint(event.clientX, event.clientY) : null; } catch (_) {}
    const nodes = [event.target, el];
    for (const node of nodes) {
      if (!node || node.nodeType !== Node.ELEMENT_NODE) continue;
      if (node.isContentEditable) return false;
      if (node.closest && node.closest(NO_REFUSAL_SELECTOR)) return false;
    }
    if (!el) return false;
    if (el.closest && (el.closest("a[href]") || el.closest("img[src]"))) return true;
    // Real text under the pointer only: a body/html hit on empty background must stay silent.
    try {
      const range = document.caretRangeFromPoint ? document.caretRangeFromPoint(event.clientX, event.clientY) : null;
      const node = range && range.startContainer;
      if (!node || node.nodeType !== Node.TEXT_NODE || !normalizeInspectText(node.nodeValue) || !el.contains(node)) return false;
      const probe = document.createRange();
      probe.selectNodeContents(node);
      return Array.from(probe.getClientRects()).some((r) =>
        event.clientX >= r.left && event.clientX <= r.right && event.clientY >= r.top && event.clientY <= r.bottom);
    } catch (_) {}
    return false;
  }
  function publishVisualEdit(event) {
    if (!event.isTrusted) return;
    const hit = event.target && event.target.closest ? event.target.closest("[data-vr-edit-path]") : null;
    const inspect = inspectTextFromPoint(event);
    if (!hit && inspect) {
      const at = Date.now();
      if (at - lastRefusedAt < 500 || !refusalAllowedAt(event)) return;
      lastRefusedAt = at;
      parent.postMessage({ type: editRefusedType, sessionId, pagePath }, parentTarget);
      return;
    }
    if (!hit || !inspect) return;
    const now = Date.now();
    if (lastEditHit === hit && now - lastEditAt < 500) return;
    const childPath = String(hit.getAttribute("data-vr-edit-path") || "")
      .split(".")
      .filter(Boolean)
      .map(Number);
    if (!childPath.length || childPath.some((value) => !Number.isInteger(value) || value < 0)) return;
    lastEditHit = hit;
    lastEditAt = now;
    const rect = hit.getBoundingClientRect();
    parent.postMessage(Object.assign({
      type: editVisualType,
      sessionId,
      pagePath,
      childPath,
      // Recompute from the rendered node. If page scripts changed it after identity stamping,
      // the parent cannot resolve this fingerprint against current source and refuses the edit.
      fingerprint: currentBlockFingerprint(hit),
      geometry: { left: rect.left, top: rect.top, width: rect.width, height: rect.height },
      style: inlineStyle(hit),
      textSelection: editTextSelection(hit, event),
    }, inspect), parentTarget);
  }
  document.addEventListener("dblclick", (event) => {
    if (!event.isTrusted) return;
    event.preventDefault();
    event.stopPropagation();
    publishVisualEdit(event);
  }, true);
  document.addEventListener("click", (event) => {
    if (event.detail < 2 || !event.isTrusted) return;
    event.preventDefault();
    event.stopPropagation();
    publishVisualEdit(event);
  }, true);

  // Ctrl/Cmd+click or middle-click opens through the parent instead of navigating this preview.
  function anchorFrom(event) {
    const direct = event.target && event.target.closest ? event.target.closest("a[href]") : null;
    if (direct) return direct;
    const path = event.composedPath ? event.composedPath() : [];
    for (let i = 0; i < path.length; i += 1) {
      const item = path[i];
      const anchor = item && item.closest ? item.closest("a[href]") : null;
      if (anchor) return anchor;
    }
    return null;
  }
  function openInNewTab(event, anchor) {
    const href = (anchor.getAttribute("href") || "").trim();
    if (!href || href.charAt(0) === "#") return false; // bare #fragment can't be a useful new tab
    event.preventDefault();
    event.stopPropagation();
    parent.postMessage({ type: linkType, sessionId, href, newTab: true }, parentTarget);
    return true;
  }
  document.addEventListener("auxclick", (event) => {
    if (event.button !== 1) return; // middle button only
    const anchor = anchorFrom(event);
    if (anchor) openInNewTab(event, anchor);
  }, true);

  document.addEventListener("click", (event) => {
    if (event.defaultPrevented || event.button !== 0) return;
    // Ctrl/Cmd+click on a link → new tab (Shift/Alt fall through to default).
    if ((event.metaKey || event.ctrlKey) && !event.shiftKey && !event.altKey) {
      const modAnchor = anchorFrom(event);
      if (modAnchor && openInNewTab(event, modAnchor)) return;
    }
    if (event.metaKey || event.ctrlKey || event.shiftKey || event.altKey) return;
    if (inspectActive) {
      const target = inspectTextFromPoint(event);
      if (target) {
        event.preventDefault();
        event.stopPropagation();
        parent.postMessage(Object.assign({ type: inspectTargetType, sessionId }, target), parentTarget);
        return;
      }
    }
    const anchor = anchorFrom(event);
    if (!anchor) return;
    const href = anchor.getAttribute("href") || "";
    if (!href) return;
    // In-page fragment links: handle the scroll ourselves. Native #fragment navigation in an
    // opaque-origin preview iframe can reload/blank the document instead of scrolling, so intercept it
    // and scroll to the target (id, then [name=]) directly. Bare "#" scrolls to top.
    if (href.charAt(0) === "#") {
      event.preventDefault();
      const id = decodeURIComponent(href.slice(1));
      if (!id) { window.scrollTo({ top: 0 }); return; }
      let target = document.getElementById(id);
      if (!target) {
        try { target = document.querySelector('[name="' + (window.CSS && CSS.escape ? CSS.escape(id) : id) + '"]'); } catch (_) { target = null; }
      }
      if (target && target.scrollIntoView) target.scrollIntoView();
      return;
    }
    // A plain click on a link opens it — external links go to the OS browser (the parent decides).
    // This is the expected document-viewer behavior and takes priority over the block editor's
    // double-click affordance; link text is edited via the toolbar's link control, not by clicking
    // the link itself.
    event.preventDefault();
    parent.postMessage({ type: linkType, sessionId, href }, parentTarget);
  }, true);

  // Hover over a link → tell the parent to show its URL in a status strip (browser-style), and
  // clear it when the pointer leaves the link. Deduplicated so we only message on actual changes.
  let hoveredHref = null;
  function reportHover(href) {
    if (href === hoveredHref) return;
    hoveredHref = href;
    parent.postMessage({ type: linkHoverType, sessionId, href }, parentTarget);
  }
  document.addEventListener("mouseover", (event) => {
    const anchor = anchorFrom(event);
    reportHover(anchor ? (anchor.getAttribute("href") || "").trim() || null : null);
  }, true);
  document.addEventListener("mouseout", (event) => {
    const to = event.relatedTarget;
    const stillInLink = to && to.closest ? to.closest("a[href]") : null;
    if (!stillInLink) reportHover(null);
  }, true);

  // Find shortcuts fire inside this frame, so ask the parent to open the app FindBar.
  window.addEventListener("keydown", (e) => {
    const openDevtools =
      (e.key === "F12" && !e.ctrlKey && !e.metaKey && !e.altKey && !e.shiftKey) ||
      ((e.ctrlKey || e.metaKey) && (e.altKey || e.shiftKey) && e.code === "KeyI");
    if (openDevtools) {
      e.preventDefault();
      e.stopPropagation();
      parent.postMessage({ type: openDevtoolsType, sessionId }, parentTarget);
      return;
    }
    if ((e.ctrlKey || e.metaKey) && !e.altKey) {
      const k = (e.key || "").toLowerCase();
      if (k === "f" || k === "g") {
        e.preventDefault();
        parent.postMessage({ type: findOpenType, sessionId }, parentTarget);
      } else if (k === "a") {
        e.preventDefault();
        selectWholeDocument();
      } else if (k === "z" || k === "y") {
        // Focus sits in this frame after a rendered-pane edit, so the app's undo never arrives.
        const t = e.target;
        const editable = t && t.closest && t.closest("input, textarea, [contenteditable]:not([contenteditable='false'])");
        if (!editable) {
          e.preventDefault();
          parent.postMessage({ type: historyType, sessionId, direction: k === "y" || e.shiftKey ? "redo" : "undo" }, parentTarget);
        }
      } else if (k === "0") {
        // Focus can sit inside this frame, where the app's own Cmd/Ctrl+0 never arrives.
        e.preventDefault();
        setPageZoom(1);
      }
    }
  }, true);

  let pageZoom = 1;
  function setPageZoom(next) {
    pageZoom = next;
    try {
      document.documentElement.style.zoom = String(pageZoom);
      document.documentElement.style.transformOrigin = "0 0";
    } catch (_) {}
    // Always report back, including on reset: the parent's HUD is the only thing telling the user
    // what this frame's zoom actually is, and a stale HUD would offer a reset that already happened.
    parent.postMessage({ type: zoomType, sessionId, percent: Math.round(pageZoom * 100) }, parentTarget);
  }
  function applyPageZoom(delta) {
    setPageZoom(Math.max(0.5, Math.min(3, Math.round((pageZoom + (delta > 0 ? 0.1 : -0.1)) * 100) / 100)));
  }

  window.addEventListener("wheel", (e) => {
    if (!(e.ctrlKey || e.metaKey)) return;
    e.preventDefault();
    e.stopPropagation();
    applyPageZoom(e.deltaY < 0 ? 1 : -1);
  }, { capture: true, passive: false });

  function selectWholeDocument() {
    try {
      const root = document.body || document.documentElement;
      if (!root) return;
      const selection = window.getSelection && window.getSelection();
      if (!selection) return;
      const range = document.createRange();
      range.selectNodeContents(root);
      selection.removeAllRanges();
      selection.addRange(range);
    } catch (_) {}
  }

  // Search + highlight run in-frame because the opaque-origin parent can't read this DOM.
  var MARK_CLASS = "__vr_find";
  var ACTIVE_CLASS = "__vr_find_active";
  let matches = [];
  let markGroups = []; // markGroups[i] = the <span>(s) wrapping match i (a match can cross text nodes)
  let nodeIndex = [];
  let active = -1;
  (function injectMarkStyle() {
    try {
      if (document.getElementById("__vr_find_style__")) return;
      const s = document.createElement("style");
      s.id = "__vr_find_style__";
      s.textContent =
        "." + MARK_CLASS + "{background-color:rgba(12,106,165,.40);border-radius:2px}" +
        "." + ACTIVE_CLASS + "{background-color:#0c6aa5;color:#fff;border-radius:2px}";
      (document.head || document.documentElement).appendChild(s);
    } catch (_) {}
  })();
  const isWordChar = (ch) => ch != null && /[\\p{L}\\p{N}_]/u.test(ch);
  const buildIndex = () => {
    const root = document.body || document.documentElement;
    const walker = document.createTreeWalker(root, NodeFilter.SHOW_TEXT, {
      acceptNode(n) {
        const tag = n.parentNode && n.parentNode.nodeName;
        if (tag === "SCRIPT" || tag === "STYLE" || tag === "NOSCRIPT" || tag === "TITLE") return NodeFilter.FILTER_REJECT;
        return n.nodeValue ? NodeFilter.FILTER_ACCEPT : NodeFilter.FILTER_REJECT;
      },
    });
    nodeIndex = [];
    let text = "";
    let n;
    while ((n = walker.nextNode())) {
      const v = n.nodeValue || "";
      nodeIndex.push({ node: n, start: text.length, len: v.length });
      text += v;
    }
    return text;
  };
  const compute = (text, q, opts) => {
    const out = [];
    if (!q) return out;
    const hay = opts.matchCase ? text : text.toLowerCase();
    const needle = opts.matchCase ? q : q.toLowerCase();
    let i = hay.indexOf(needle);
    while (i !== -1) {
      const s = i, e = i + needle.length;
      if (!opts.wholeWord || (!isWordChar(text[s - 1]) && !isWordChar(text[e]))) out.push({ start: s, end: e });
      i = hay.indexOf(needle, i + (needle.length || 1));
    }
    return out;
  };
  const clearMarks = () => {
    for (let g = 0; g < markGroups.length; g++) {
      const group = markGroups[g];
      for (let s = 0; s < group.length; s++) {
        const span = group[s];
        const parent = span.parentNode;
        if (!parent) continue;
        while (span.firstChild) parent.insertBefore(span.firstChild, span);
        parent.removeChild(span);
        try { parent.normalize(); } catch (_) {}
      }
    }
    markGroups = [];
  };
  // Wrap [start,end) in span(s). splitText keeps the original node as the LEADING part, so wrapping
  // matches back-to-front leaves earlier offsets (even within a shared text node) valid.
  const wrapRange = (start, end) => {
    const segs = [];
    for (let i = 0; i < nodeIndex.length; i++) {
      const en = nodeIndex[i];
      const es = en.start, ee = en.start + en.len;
      if (ee <= start || es >= end) continue;
      const ls = Math.max(start, es) - es;
      const le = Math.min(end, ee) - es;
      if (le > ls) segs.push({ node: en.node, s: ls, e: le });
    }
    const spans = [];
    for (let si = segs.length - 1; si >= 0; si--) {
      const seg = segs[si];
      try {
        let node = seg.node;
        if (seg.e < node.length) node.splitText(seg.e);
        if (seg.s > 0) node = node.splitText(seg.s);
        const span = document.createElement("span");
        span.className = MARK_CLASS;
        node.parentNode.insertBefore(span, node);
        span.appendChild(node);
        spans.unshift(span);
      } catch (_) {}
    }
    return spans;
  };
  const setActive = (idx) => {
    for (let g = 0; g < markGroups.length; g++) {
      const on = g === idx;
      const group = markGroups[g];
      for (let s = 0; s < group.length; s++) group[s].className = on ? MARK_CLASS + " " + ACTIVE_CLASS : MARK_CLASS;
    }
    const grp = markGroups[idx];
    if (grp && grp[0]) { try { grp[0].scrollIntoView({ block: "center" }); } catch (_) {} }
  };
  const applyMatches = () => {
    markGroups = new Array(matches.length);
    for (let mi = matches.length - 1; mi >= 0; mi--) markGroups[mi] = wrapRange(matches[mi].start, matches[mi].end);
    if (active >= 0) setActive(active);
  };
  const respond = (requestId) => parent.postMessage({ type: resultType, sessionId, requestId, count: matches.length, activeIndex: active }, parentTarget);

  window.addEventListener("message", (event) => {
    if (event.source !== parent) return;
    const data = event.data;
    if (!data || data.sessionId !== sessionId) return;
    if (data.type === activityType) {
      setActivity(data.active === true);
    } else if (data.type === cssType && typeof data.css === "string") {
      // Live CSS hot-apply: set the text of one injected <style>. The parent already authors the whole
      // document, so this grants no capability it didn't already have.
      let el = document.getElementById("__vr_live_css__");
      if (!el) {
        el = document.createElement("style");
        el.id = "__vr_live_css__";
        el.setAttribute("data-vr-live-css", "");
        (document.head || document.documentElement).appendChild(el);
      }
      el.textContent = data.css;
      requestAnimationFrame(reportSurface);
    } else if (data.type === themeType && typeof data.css === "string") {
      let el = document.querySelector("style[data-vr-theme-style]");
      if (!el) {
        el = document.createElement("style");
        el.setAttribute("data-vr-theme-style", "");
        (document.head || document.documentElement).appendChild(el);
      }
      el.textContent = data.css;
      requestAnimationFrame(reportSurface);
    } else if (data.type === findType) {
      clearMarks(); // unwrap prior spans + normalize so the text index is rebuilt over merged nodes
      const text = buildIndex();
      matches = compute(text, String(data.query || ""), { matchCase: !!data.matchCase, wholeWord: !!data.wholeWord });
      active = matches.length ? 0 : -1;
      applyMatches();
      respond(data.requestId);
    } else if (data.type === stepType) {
      if (matches.length) {
        active = (((active + (data.delta || 0)) % matches.length) + matches.length) % matches.length;
        setActive(active); // spans already wrapped — just move the active class + scroll
      }
      respond(data.requestId);
    } else if (data.type === clearType) {
      clearMarks();
      matches = [];
      active = -1;
    } else if (data.type === selectAllType) {
      selectWholeDocument();
    } else if (data.type === zoomResetType) {
      setPageZoom(1);
    } else if (data.type === scrollTopType) {
      try {
        const reduceMotion = window.matchMedia && window.matchMedia("(prefers-reduced-motion: reduce)").matches;
        window.scrollTo({ top: 0, behavior: reduceMotion ? "auto" : "smooth" });
      } catch (_) { window.scrollTo(0, 0); }
    } else if (data.type === inspectModeType) {
      inspectActive = !!data.active;
      try { document.documentElement.style.cursor = inspectActive ? "crosshair" : ""; } catch (_) {}
    } else if (data.type === scrollToInspectType) {
      try { if (lastInspectEl && lastInspectEl.scrollIntoView) lastInspectEl.scrollIntoView({ block: "center", inline: "nearest" }); } catch (_) {}
    } else if (data.type === printType) {
      // Don't print this frame directly: it's pinned to the on-screen content-pane box, so window.print()
      // here would paginate against that fixed viewport, not the document's full height. Send the
      // rendered markup back to the parent, which prints it in-flow (documentPrinting.ts).
      try {
        const printable = document.documentElement.cloneNode(true);
        printable.querySelectorAll("[data-vr-edit-path]").forEach((node) => {
          node.removeAttribute("data-vr-edit-path");
        });
        parent.postMessage({ type: printContentType, sessionId, html: printable.outerHTML }, parentTarget);
      } catch (_) {}
    }
  });

  // Report scroll position to the parent (rAF-throttled) so a live rebuild can restore it — the
  // opaque-origin parent can't read this frame's scrollY directly.
  let scrolling = false;
  window.addEventListener("scroll", () => {
    if (scrolling) return;
    scrolling = true;
    requestAnimationFrame(() => {
      scrolling = false;
      parent.postMessage({ type: scrollType, sessionId, y: window.scrollY || document.documentElement.scrollTop || 0 }, parentTarget);
    });
  }, { passive: true });
})();`;
}
