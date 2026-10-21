// QA ONLY. Injected by rightkit-qa's embedded control plugin, and only when the runtime QA control
// service is active (RIGHTKIT_CONTROL_SERVICE set) in a build with the `control` feature.
//
// Tauri freezes its invoke function, so a test cannot replace it. Its IPC transport calls
// `window.fetch` once per command (URL = convertFileSrc('', 'ipc') + encodeURIComponent(cmd),
// POST, JSON body for JSON args, reply read from the `Tauri-Response` header: `ok` or `error`),
// and fetch is writable. This wrapper answers matching IPC requests from fixture rules and
// passes every other request to the real fetch. It never touches the Tauri internals object.
(function () {
  'use strict';
  if (window.__RK_QA_IPC__) return;

  var realFetch = window.fetch;
  var rules = [];
  var entries = [];
  var MAX_LOG = 1000;

  function prefix() {
    var t = window.__TAURI_INTERNALS__;
    if (!t || typeof t.convertFileSrc !== 'function') return null;
    return t.convertFileSrc('', 'ipc');
  }

  function clone(v) {
    return v === undefined ? null : JSON.parse(JSON.stringify(v));
  }

  function deepEqual(a, b) {
    return JSON.stringify(a) === JSON.stringify(b);
  }

  // Every key of `want` must be present in `have` with a matching value (objects recurse).
  function subset(want, have) {
    if (want === null || typeof want !== 'object') return want === have;
    if (Array.isArray(want)) return Array.isArray(have) && deepEqual(want, have);
    if (have === null || typeof have !== 'object' || Array.isArray(have)) return false;
    return Object.keys(want).every(function (k) {
      return Object.prototype.hasOwnProperty.call(have, k) && subset(want[k], have[k]);
    });
  }

  function parseBody(init) {
    var body = init && init.body;
    if (typeof body !== 'string') return null;
    try {
      return JSON.parse(body);
    } catch (e) {
      return null;
    }
  }

  function requestUrl(input) {
    if (typeof input === 'string') return input;
    if (input && typeof input.url === 'string') return input.url;
    return String(input);
  }

  function pick(cmd, args) {
    for (var i = 0; i < rules.length; i++) {
      var r = rules[i];
      if (r.cmd !== cmd) continue;
      if (r.times !== undefined && r.used >= r.times) continue;
      if (r.args !== undefined && !subset(r.args, args)) continue;
      return r;
    }
    return null;
  }

  function answer(rule) {
    var reply = rule.replies[Math.min(rule.used, rule.replies.length - 1)];
    rule.used += 1;
    var text = JSON.stringify(reply.value);
    return new Response(text === undefined ? 'null' : text, {
      headers: { 'Content-Type': 'application/json', 'Tauri-Response': reply.kind },
    });
  }

  function validate(rule) {
    if (!rule || typeof rule.cmd !== 'string' || rule.cmd === '') {
      throw new Error('ipc fixture rule needs a cmd string');
    }
    if (!Array.isArray(rule.replies) || rule.replies.length === 0) {
      throw new Error('ipc fixture rule needs a non-empty replies array');
    }
    rule.replies.forEach(function (r) {
      if (!r || (r.kind !== 'ok' && r.kind !== 'error')) {
        throw new Error("ipc fixture reply kind must be 'ok' or 'error'");
      }
    });
    if (rule.times !== undefined && !(Number.isInteger(rule.times) && rule.times > 0)) {
      throw new Error('ipc fixture times must be a positive integer');
    }
  }

  window.fetch = function (input, init) {
    var p = prefix();
    var url = requestUrl(input);
    if (p !== null && url.indexOf(p) === 0) {
      var cmd = url.slice(p.length).split('?')[0];
      try {
        cmd = decodeURIComponent(cmd);
      } catch (e) {
        // keep the raw text
      }
      var args = parseBody(init);
      entries.push({ cmd: cmd, args: clone(args) });
      if (entries.length > MAX_LOG) entries.shift();
      var rule = pick(cmd, args);
      if (rule) return Promise.resolve(answer(rule));
    }
    return realFetch.apply(this, arguments);
  };

  window.__RK_QA_IPC__ = {
    add: function (rule) {
      validate(rule);
      rules.push({
        cmd: rule.cmd,
        args: rule.args === undefined ? undefined : clone(rule.args),
        replies: clone(rule.replies),
        times: rule.times,
        used: 0,
      });
      return rules.length;
    },
    clear: function () {
      rules.length = 0;
      entries.length = 0;
      return 0;
    },
    log: function () {
      return clone(entries);
    },
  };
})();
