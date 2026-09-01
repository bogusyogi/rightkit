const HOST_PRODUCTS = Object.freeze({
  "legion.orthiclabs.com": "legion",
  "membrane.orthiclabs.com": "membrane",
});

const HSTS = "max-age=63072000; includeSubDomains; preload";

export function objectKeyFor(request) {
  const url = new URL(request.url);
  const product = HOST_PRODUCTS[url.hostname.toLowerCase()];
  if (!product || url.pathname !== "/install.ps1") return null;
  return `${product}/install.ps1`;
}

export default {
  async fetch(request, env) {
    const url = new URL(request.url);
    if (url.protocol === "http:") {
      url.protocol = "https:";
      return Response.redirect(url.toString(), 301);
    }
    const respond = (body, init = {}) => {
      const headers = new Headers(init.headers ?? {});
      headers.set("Strict-Transport-Security", HSTS);
      return new Response(body, { ...init, headers });
    };
    if (!new Set(["GET", "HEAD"]).has(request.method)) {
      return respond("Method Not Allowed\n", { status: 405, headers: { Allow: "GET, HEAD" } });
    }
    if (url.pathname === "/") {
      return respond(null, { status: 302, headers: { Location: `${url.origin}/install.ps1` } });
    }
    const key = objectKeyFor(request);
    if (!key) return respond("Not Found\n", { status: 404 });
    const object = await env.INSTALL_BUCKET.get(key);
    if (!object) return respond("Not Found\n", { status: 404 });
    const headers = {
      "Cache-Control": "public, max-age=300",
      "Content-Type": "text/plain; charset=utf-8",
      "X-Content-Type-Options": "nosniff",
    };
    if (object.httpEtag) headers.ETag = object.httpEtag;
    return respond(request.method === "HEAD" ? null : object.body, { headers });
  },
};
