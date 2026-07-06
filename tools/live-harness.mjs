// Live-checkpoint harness (built for bead conceptify-e7m.5; reusable): serves
// the REAL built shell (dist/) in a plain browser with
// window.__TAURI_INTERNALS__ shimmed. EVERY invoke is forwarded to the live
// bridge (src-tauri/src/live_bridge.rs — `CONCEPTIFY_LIVE_BRIDGE=1 cargo test
// -p conceptify live_bridge -- --ignored --nocapture`, 127.0.0.1:4560), which
// dispatches through Tauri's REAL IPC layer into the REAL command/flow/run
// stack on the REAL app DB; events emitted on the bridge's app handle are
// polled from /events and re-dispatched to the shell's listeners. Artifact
// iframes are served from the real on-disk artifact files with bridge.js
// injected + the reference CSP, same as artifact_protocol.rs.
//
// Usage: `npm run build && node tools/live-harness.mjs`, then open
// http://localhost:4599 in any browser. Run `npm run tauri dev` alongside so
// agent-spawned `conceptify` CLI children reach the real HTTP API on 4477.
//
// Known quirk when driving via CDP automation: clicks on some buttons inside
// the settings sheet can miss (overlay hit-testing); dispatching
// element.click() via JS works and exercises the same handler.
import http from "node:http";
import { readFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const REPO = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const DIST = path.join(REPO, "dist");
const BRIDGE_JS = path.join(REPO, "src-tauri/assets/bridge.js");
const BRIDGE = "http://127.0.0.1:4560";
const PORT = 4599;

// Same reference CSP as artifact_protocol.rs (docs/artifact-spec.md §3).
const ARTIFACT_CSP =
  "default-src 'none'; script-src 'unsafe-inline' https://cdn.jsdelivr.net; " +
  "style-src 'unsafe-inline' https://cdn.jsdelivr.net; " +
  "font-src data: https://cdn.jsdelivr.net; img-src data:; connect-src 'none'";

const MOCK_JS = `
(() => {
  let nextCbId = 1;
  const callbacks = new Map();
  function dispatch(event, payload) {
    for (const { ev, cb } of callbacks.values()) {
      if (ev === event) cb({ event, payload });
    }
  }
  window.__invokeLog = [];
  window.__TAURI_INTERNALS__ = {
    transformCallback(cb) {
      const id = nextCbId++;
      callbacks.set(id, { ev: null, cb });
      return id;
    },
    async invoke(cmd, args = {}) {
      if (cmd === "plugin:event|listen") {
        const entry = callbacks.get(args.handler);
        if (entry) entry.ev = args.event;
        return args.handler;
      }
      if (cmd === "plugin:event|unlisten") { callbacks.delete(args.eventId); return; }
      window.__invokeLog.push({ cmd, args });
      const r = await fetch("/invoke", {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({ cmd, args }),
      });
      const text = await r.text();
      const value = text ? JSON.parse(text) : null;
      if (!r.ok) throw value;
      return value;
    },
  };
  window.__TAURI_EVENT_PLUGIN_INTERNALS__ = {
    unregisterListener(event, id) { callbacks.delete(id); },
  };
  // Event pump: poll the bridge's buffered events and re-dispatch.
  let cursor = null;
  window.__eventLog = [];
  async function pump() {
    try {
      const r = await fetch("/events?since=" + (cursor ?? 0));
      const { next, events } = await r.json();
      // First poll: skip history from before this page load.
      if (cursor === null) { cursor = next; return; }
      cursor = next;
      for (const { event, payload } of events) {
        window.__eventLog.push({ event, payload });
        dispatch(event, payload);
      }
    } catch {}
  }
  setInterval(pump, 400);
  pump();
})();
`;

async function proxy(req, res, target) {
  let body = "";
  for await (const chunk of req) body += chunk;
  try {
    const r = await fetch(target, {
      method: req.method,
      headers: { "content-type": "application/json" },
      body: req.method === "POST" ? body : undefined,
    });
    const text = await r.text();
    res.writeHead(r.status, { "content-type": "application/json" });
    res.end(text);
  } catch (e) {
    res.writeHead(502, { "content-type": "text/plain" });
    res.end(String(e));
  }
}

const MIME = {
  ".html": "text/html", ".js": "text/javascript", ".css": "text/css",
  ".svg": "image/svg+xml", ".png": "image/png", ".woff2": "font/woff2",
};

const server = http.createServer(async (req, res) => {
  try {
    const url = new URL(req.url, `http://localhost:${PORT}`);
    const p = url.pathname;

    if (p === "/tauri-mock.js") {
      res.writeHead(200, { "content-type": "text/javascript" });
      return res.end(MOCK_JS);
    }
    if (p === "/invoke") return proxy(req, res, `${BRIDGE}/invoke`);
    if (p === "/events") return proxy(req, res, `${BRIDGE}/events${url.search}`);

    // artifact://localhost/<thread>/<version> equivalent: real file via the
    // bridge (DB lookup), bridge.js injected + reference CSP, opaque origin
    // comes from the shell's sandbox="allow-scripts" iframe attribute.
    const art = /^\/artifact\/([A-Za-z0-9_-]+)\/(\d+|latest)$/.exec(p);
    if (art) {
      const r = await fetch(`${BRIDGE}/artifact/${art[1]}/${art[2]}`);
      if (!r.ok) { res.writeHead(r.status); return res.end("not found"); }
      let html = await r.text();
      const bridge = readFileSync(BRIDGE_JS, "utf8");
      const tag = `\n<script data-cfy-bridge="v1">\n${bridge}\n</script>\n`;
      const idx = html.toLowerCase().lastIndexOf("</body>");
      html = idx >= 0 ? html.slice(0, idx) + tag + html.slice(idx) : html + tag;
      res.writeHead(200, {
        "content-type": "text/html",
        "content-security-policy": ARTIFACT_CSP,
        "cache-control": "no-store",
      });
      return res.end(html);
    }

    // Static shell from dist/.
    let file = p === "/" ? "/index.html" : p;
    const full = path.join(DIST, path.normalize(file).replace(/^(\.\.[/\\])+/, ""));
    let data;
    try {
      data = readFileSync(full);
    } catch {
      res.writeHead(404);
      return res.end("not found");
    }
    const ext = path.extname(full);
    if (file === "/index.html") {
      let html = data.toString("utf8");
      html = html.replace("<script", '<script src="/tauri-mock.js"></script><script');
      res.writeHead(200, { "content-type": "text/html", "cache-control": "no-store" });
      return res.end(html);
    }
    if (ext === ".js") {
      let js = data.toString("utf8");
      js = js.replaceAll("artifact://localhost/", `http://localhost:${PORT}/artifact/`);
      res.writeHead(200, { "content-type": "text/javascript", "cache-control": "no-store" });
      return res.end(js);
    }
    res.writeHead(200, { "content-type": MIME[ext] || "application/octet-stream" });
    return res.end(data);
  } catch (e) {
    res.writeHead(500);
    res.end(String(e));
  }
});

server.listen(PORT, "127.0.0.1", () => console.log(`harness on http://localhost:${PORT}`));
