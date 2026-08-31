const HOST_PRODUCTS = Object.freeze({
  "legion.orthiclabs.com": "legion",
  "membrane.orthiclabs.com": "membrane",
});

export function objectKeyFor(request) {
  const url = new URL(request.url);
  const product = HOST_PRODUCTS[url.hostname.toLowerCase()];
  if (!product || url.pathname !== "/install.ps1") return null;
  return `${product}/install.ps1`;
}

export default {
  async fetch(request, env) {
    if (!new Set(["GET", "HEAD"]).has(request.method)) {
      return new Response("Method Not Allowed\n", { status: 405, headers: { Allow: "GET, HEAD" } });
    }
    const url = new URL(request.url);
    if (url.pathname === "/") return Response.redirect(`${url.origin}/install.ps1`, 302);
    const key = objectKeyFor(request);
    if (!key) return new Response("Not Found\n", { status: 404 });
    const object = await env.INSTALL_BUCKET.get(key);
    if (!object) return new Response("Not Found\n", { status: 404 });
    const headers = new Headers({
      "Cache-Control": "public, max-age=300",
      "Content-Type": "text/plain; charset=utf-8",
      "X-Content-Type-Options": "nosniff",
    });
    if (object.httpEtag) headers.set("ETag", object.httpEtag);
    return new Response(request.method === "HEAD" ? null : object.body, { headers });
  },
};

