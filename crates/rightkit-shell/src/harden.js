(function () {
  var cfg = __RK_SHELL_CONFIG__;
  var root = window;
  if (root.__rkShell && root.__rkShell.cfgHash === JSON.stringify(cfg)) return;
  var state = root.__rkShell = { version: 1, cfgHash: JSON.stringify(cfg), blocked: [] };
  function note(kind, detail) {
    state.blocked.push({ kind: kind, detail: detail || '' });
    if (state.blocked.length > 100) state.blocked.shift();
  }
  var EDITABLE = 'input, textarea, select, [contenteditable=""], [contenteditable="true"]';
  function closest(t, sel) {
    var n = t && t.nodeType === 1 ? t : t && t.parentElement;
    return n && n.closest ? n.closest(sel) : null;
  }
  function openExternal(url) {
    var internals = window.__TAURI_INTERNALS__;
    if (!internals || !internals.invoke) { note('external-link-no-bridge', url); return; }
    internals.invoke('rk_shell_open_external', { url: String(url) }).catch(function (e) { note('external-link-failed', String(e)); });
  }
  function isExternalUrl(u) {
    return u && (u.protocol === 'http:' || u.protocol === 'https:' || u.protocol === 'mailto:');
  }
  function sameDocumentOrigin(u) {
    try { return u.origin === location.origin && u.protocol !== 'mailto:'; } catch (e) { return false; }
  }

  if (cfg.blockContextMenu) {
    window.addEventListener('contextmenu', function (e) {
      if (cfg.allowEditableMenu && closest(e.target, EDITABLE)) { note('contextmenu-allowed-editable'); return; }
      if (closest(e.target, '[data-rk-native-menu]')) { note('contextmenu-allowed-surface'); return; }
      e.preventDefault();
      note('contextmenu-blocked');
    }, true);
  }

  window.addEventListener('keydown', function (e) {
    var mod = e.metaKey || e.ctrlKey;
    var k = e.key;
    var lk = typeof k === 'string' ? k.toLowerCase() : '';
    var hit = null;
    if (cfg.blockReload) {
      if (k === 'F5' || (mod && lk === 'r') || (mod && lk === 'u') || (mod && lk === 'p')) hit = 'reload';
    }
    if (!hit && cfg.blockZoom && mod && (k === '+' || k === '=' || k === '-' || k === '_' || k === '0')) hit = 'zoom';
    if (!hit && cfg.blockHistoryNav) {
      if ((e.altKey && (k === 'ArrowLeft' || k === 'ArrowRight')) || (e.metaKey && (k === '[' || k === ']')) || k === 'BrowserBack' || k === 'BrowserForward') hit = 'history';
    }
    if (!hit && cfg.blockDevtoolsKeys) {
      if (k === 'F12' || (e.ctrlKey && e.shiftKey && (lk === 'i' || lk === 'j' || lk === 'c')) || (e.metaKey && e.altKey && (lk === 'i' || lk === 'j' || lk === 'c' || k === 'ˆ' || k === '∆' || k === 'ç'))) hit = 'devtools';
    }
    if (hit) { e.preventDefault(); note('key-' + hit, k); }
  }, true);

  if (cfg.blockZoom) {
    window.addEventListener('wheel', function (e) {
      if (e.ctrlKey || e.metaKey) { e.preventDefault(); note('wheel-zoom'); }
    }, { capture: true, passive: false });
    ['gesturestart', 'gesturechange', 'gestureend'].forEach(function (g) {
      window.addEventListener(g, function (e) { e.preventDefault(); note('gesture-zoom'); }, { capture: true, passive: false });
    });
  }

  if (cfg.blockHistoryNav) {
    ['mouseup', 'auxclick'].forEach(function (t) {
      window.addEventListener(t, function (e) {
        if (e.button === 3 || e.button === 4) { e.preventDefault(); note('mouse-history'); }
      }, true);
    });
  }

  if (cfg.blockDropNavigation) {
    function hasFiles(e) {
      var types = e.dataTransfer && e.dataTransfer.types;
      if (!types) return false;
      for (var i = 0; i < types.length; i++) if (types[i] === 'Files' || types[i] === 'text/uri-list') return true;
      return false;
    }
    ['dragover', 'drop'].forEach(function (t) {
      window.addEventListener(t, function (e) {
        if (!hasFiles(e)) return;
        if (closest(e.target, '[data-rk-dropzone]')) return;
        e.preventDefault();
        if (t === 'drop') note('drop-blocked');
      }, true);
    });
  }

  if (cfg.interceptExternalLinks) {
    function onLink(e) {
      var a = closest(e.target, 'a[href], area[href]');
      if (!a) return;
      var u;
      try { u = new URL(a.getAttribute('href'), document.baseURI); } catch (err) { return; }
      if (!isExternalUrl(u)) return;
      if (sameDocumentOrigin(u) && a.target !== '_blank') return;
      e.preventDefault();
      note('external-link', u.href);
      openExternal(u.href);
    }
    window.addEventListener('click', onLink, true);
    window.addEventListener('auxclick', onLink, true);
    window.open = function (url) {
      try {
        var u = new URL(String(url), document.baseURI);
        if (isExternalUrl(u)) { note('window-open-external', u.href); openExternal(u.href); }
        else note('window-open-denied', String(url));
      } catch (err) { note('window-open-denied', String(url)); }
      return null;
    };
  }
})();
