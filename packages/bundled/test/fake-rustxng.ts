#!/usr/bin/env bun
// Stands in for rustxng: `--bind A --port P` serves /healthz and a SearXNG-style /search, and exits
// after RUSTXNG_IDLE_TIMEOUT seconds without a request, as the real one does.
const argv = process.argv.slice(2);
const port = Number(argv[argv.indexOf("--port") + 1]);
const idle = Number(process.env.RUSTXNG_IDLE_TIMEOUT ?? 0);
let last = Date.now();
const server = Bun.serve({
  hostname: argv[argv.indexOf("--bind") + 1],
  port,
  fetch(req) {
    last = Date.now();
    const url = new URL(req.url);
    if (url.pathname === "/healthz") return new Response("OK");
    const q = url.searchParams.get("q") ?? "";
    return Response.json({
      results: [{ title: `About ${q}`, url: `https://example.com/${encodeURIComponent(q)}`, content: `idle=${idle} max=${url.searchParams.get("max_results")}` }],
    });
  },
});
if (idle > 0)
  setInterval(() => {
    if (Date.now() - last >= idle * 1000) server.stop(true), process.exit(0);
  }, 100);
