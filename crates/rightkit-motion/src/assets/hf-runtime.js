// RightKit Motion runtime: injected into every composition page by the render
// protocol handler. Gives the Rust capture loop a deterministic clock:
// window.__hf_seek(t) positions every timeline, clip and animation at t seconds
// and resolves once the frame is ready to be captured. The engine reads
// window.__hf_meta (set once the composition has loaded) over CDP.
(function () {
  "use strict";
  var NS = "/__hf/";
  function post(path, body) {
    return fetch(NS + path, { method: "POST", body: body == null ? "" : JSON.stringify(body) }).catch(function () {});
  }
  function num(v, d) { var n = parseFloat(v); return isFinite(n) ? n : d; }
  function sleep(ms) { return new Promise(function (r) { setTimeout(r, ms); }); }

  function loadScript(node) {
    return new Promise(function (resolve) {
      var s = document.createElement("script");
      for (var i = 0; i < node.attributes.length; i++) s.setAttribute(node.attributes[i].name, node.attributes[i].value);
      if (node.src) {
        s.onload = s.onerror = function () { resolve(); };
        s.async = false;
        s.src = node.src;
        document.head.appendChild(s);
      } else {
        s.textContent = node.textContent;
        document.head.appendChild(s);
        resolve();
      }
    });
  }

  // Sub-compositions: <div data-composition-src="x.html"> holds a <template>.
  function inlineSubCompositions() {
    var hosts = Array.prototype.slice.call(document.querySelectorAll("[data-composition-src]"));
    return hosts.reduce(function (chain, host) {
      return chain.then(function () {
        return fetch(host.getAttribute("data-composition-src")).then(function (r) { return r.text(); }).then(function (text) {
          var doc = new DOMParser().parseFromString(text, "text/html");
          var tpl = doc.querySelector("template");
          var frag = tpl ? tpl.content : doc.body;
          var inner = frag.querySelector("[data-composition-id]") || frag;
          var scripts = [];
          Array.prototype.slice.call(inner.querySelectorAll("script")).forEach(function (s) { scripts.push(s); s.parentNode.removeChild(s); });
          Array.prototype.slice.call(inner.childNodes).forEach(function (n) { host.appendChild(document.importNode(n, true)); });
          var id = inner.getAttribute && inner.getAttribute("data-composition-id");
          if (id && !host.getAttribute("data-composition-id")) host.setAttribute("data-composition-id", id);
          if (id && host.getAttribute("data-composition-id") !== id) host.setAttribute("data-composition-inner", id);
          return scripts.reduce(function (c, s) { return c.then(function () { return loadScript(s); }); }, Promise.resolve());
        });
      });
    }, Promise.resolve());
  }

  function startOf(el, byId) {
    var raw = (el.getAttribute("data-start") || "0").trim();
    var n = parseFloat(raw);
    if (/^[-+]?\d*\.?\d+$/.test(raw)) return n;
    var m = raw.match(/^([\w-]+)\s*([+-]\s*[\d.]+)?$/);
    if (m && byId[m[1]]) {
      var ref = byId[m[1]];
      var end = startOf(ref, byId) + num(ref.getAttribute("data-duration"), 0);
      return end + (m[2] ? parseFloat(m[2].replace(/\s+/g, "")) : 0);
    }
    return 0;
  }

  function clips() {
    var byId = {};
    Array.prototype.forEach.call(document.querySelectorAll("[id]"), function (e) { byId[e.id] = e; });
    return { byId: byId, list: Array.prototype.slice.call(document.querySelectorAll("[data-start]")) };
  }

  function seekVideo(v, t) {
    return new Promise(function (resolve) {
      var done = false;
      function finish() { if (!done) { done = true; v.removeEventListener("seeked", finish); resolve(); } }
      v.addEventListener("seeked", finish);
      try { v.pause(); v.currentTime = Math.max(0, t); } catch (e) { finish(); }
      setTimeout(finish, 1500);
    });
  }

  window.__hf_seek = function (t) {
    var c = clips();
    var waits = [];
    c.list.forEach(function (el) {
      if (el.tagName === "AUDIO" || el.tagName === "SCRIPT") return;
      var start = startOf(el, c.byId);
      var dur = el.hasAttribute("data-duration") ? num(el.getAttribute("data-duration"), Infinity) : Infinity;
      var visible = t >= start - 1e-6 && t < start + dur - 1e-6;
      if (el.hasAttribute("data-composition-id") && el === document.querySelector("[data-composition-id]")) visible = true;
      el.style.visibility = visible ? "" : "hidden";
      if (el.tagName === "VIDEO" && visible) waits.push(seekVideo(el, num(el.getAttribute("data-media-start"), 0) + (t - start)));
    });
    var tls = window.__timelines || {};
    Object.keys(tls).forEach(function (id) {
      var tl = tls[id];
      var host = document.querySelector('[data-composition-id="' + id + '"]') || document.querySelector('[data-composition-inner="' + id + '"]');
      var start = host ? startOf(host, c.byId) : 0;
      try { tl.pause(); tl.seek(Math.max(0, t - start), false); } catch (e) {}
    });
    try {
      document.getAnimations().forEach(function (a) { a.pause(); a.currentTime = t * 1000; });
    } catch (e) {}
    return Promise.all(waits).then(function () { return (document.fonts && document.fonts.ready) || null; }).then(function () { return sleep(20); });
  };

  function meta() {
    var root = document.querySelector("[data-composition-id]");
    var c = clips();
    var audio = [];
    c.list.forEach(function (el) {
      if (el.tagName !== "AUDIO") return;
      audio.push({
        src: el.getAttribute("src"),
        start: startOf(el, c.byId),
        duration: el.hasAttribute("data-duration") ? num(el.getAttribute("data-duration"), null) : null,
        media_start: num(el.getAttribute("data-media-start"), 0),
        volume: num(el.getAttribute("data-volume"), 1)
      });
    });
    return {
      duration: root ? num(root.getAttribute("data-duration"), 0) : 0,
      width: root ? num(root.getAttribute("data-width"), 1920) : 1920,
      height: root ? num(root.getAttribute("data-height"), 1080) : 1080,
      audio: audio,
      timelines: Object.keys(window.__timelines || {})
    };
  }

  window.addEventListener("error", function (e) { post("log", { level: "error", text: String(e.message) }); });
  window.addEventListener("load", function () {
    inlineSubCompositions().then(function () { return sleep(50); }).then(function () {
      return window.__hf_seek(0);
    }).then(function () { var m = meta(); window.__hf_meta = m; return post("loaded", m); });
  });
})();
