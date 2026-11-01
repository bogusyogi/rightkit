// the host app PWA service worker. Minimal app-shell cache so the web client is
// installable and loads instantly offline. It deliberately does NOT touch the
// daemon API or SSE: only same-origin GET static assets and navigations are
// cached; everything else (the daemon on another origin, POSTs, /sessions,
// /events, /runtime) always goes straight to the network.

const CACHE = "coderight-shell-v1";
const SHELL = ["/", "/index.html", "/manifest.webmanifest", "/icon.svg"];

self.addEventListener("install", (event) => {
  event.waitUntil(
    caches
      .open(CACHE)
      .then((c) => c.addAll(SHELL))
      .then(() => self.skipWaiting()),
  );
});

self.addEventListener("activate", (event) => {
  event.waitUntil(
    caches
      .keys()
      .then((keys) => Promise.all(keys.filter((k) => k !== CACHE).map((k) => caches.delete(k))))
      .then(() => self.clients.claim()),
  );
});

self.addEventListener("fetch", (event) => {
  const req = event.request;
  if (req.method !== "GET") return;
  const url = new URL(req.url);
  // Never intercept the daemon API (it lives on another origin) or SSE.
  if (url.origin !== self.location.origin) return;

  if (req.mode === "navigate") {
    // Network-first for the page; fall back to the cached shell offline.
    event.respondWith(fetch(req).catch(() => caches.match("/index.html")));
    return;
  }

  // Cache-first for static assets (hashed bundles never change under one name).
  event.respondWith(
    caches.match(req).then(
      (hit) =>
        hit ||
        fetch(req).then((res) => {
          const copy = res.clone();
          caches.open(CACHE).then((c) => c.put(req, copy));
          return res;
        }),
    ),
  );
});
