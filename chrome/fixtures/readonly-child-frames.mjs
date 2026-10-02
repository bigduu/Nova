import http from "node:http";
import { pathToFileURL } from "node:url";

// Owned loopback-only acceptance fixture. No native host or daily profile.
export async function startFixture() {
  let origin;
  let otherOrigin;
  const frame = (path, attributes = "") => `<iframe src="${path}" ${attributes}></iframe>`;
  const html = (body) => `<!doctype html><html><head><meta charset="utf-8"><title>Nova read-only frame fixture</title>
    <style>body{font:16px system-ui;margin:20px;line-height:1.4}iframe{width:360px;height:180px;border:1px solid #999;margin:6px}button,input{font:inherit;margin:6px;padding:6px}</style>
    </head><body>${body}</body></html>`;
  const handler = (request, response) => {
    const url = new URL(request.url, origin);
    let body;
    if (url.pathname === "/top") {
      const mode = url.searchParams.get("case") ?? "base";
      let frames = frame("/child", 'id="first-child"') + frame("/child");
      if (mode === "base") frames += frame(`${otherOrigin}/cross`) + frame("/leaf?kind=opaque", "sandbox") + '<div id="closed-owner"></div>';
      if (mode === "hidden") frames = frame("/child") + `<div hidden>${frame("/leaf?kind=hidden")}</div>`;
      if (mode === "sensitive") frames = frame("/child") + `<div data-private>${frame("/leaf?kind=sensitive")}</div>`;
      if (mode === "sandbox") frames = frame("/child") + frame("/leaf?kind=sandbox", 'sandbox="allow-same-origin"');
      if (mode === "opaque") frames = frame("/child") + frame("/leaf?kind=opaque", "sandbox");
      if (mode === "open" || mode === "closed") frames = frame("/child") + `<div id="${mode}-owner"></div>`;
      if (mode === "closed-slot") frames = frame("/child") + `<div id="closed-slot-owner">${frame("/leaf?kind=closed-slot")}</div>`;
      if (mode === "zero") frames = frame("/child") + frame("/leaf?kind=zero", 'style="width:0;height:0;border:0;padding:0"');
      if (mode === "documents") frames = Array.from({ length: 12 }, () => frame("/leaf?kind=visible")).join("");
      if (mode === "depth") frames = frame("/depth?level=1");
      if (mode === "budget") frames = frame("/large") + frame("/large");
      body = `<h1>Read-only child frames</h1><p>Case: ${["base", "hidden", "sensitive", "sandbox", "opaque", "open", "closed", "closed-slot", "zero", "documents", "depth", "budget"].includes(mode) ? mode : "base"}</p>
        <button id="top-action">Increment top counter</button><p id="top-counter">Top counter: 0</p>
        <label>Top value <input id="top-value" value="Initial top value"></label>
        <button id="navigate-child">Navigate first child</button>${frames}
        <script>let clicks=0;document.getElementById('top-action').onclick=()=>{document.getElementById('top-counter').textContent='Top counter: '+(++clicks)};
        document.getElementById('navigate-child').onclick=()=>{const child=document.getElementById('first-child');if(child)child.src='/leaf?kind=navigated'};
        for(const mode of ['open','closed']){const host=document.getElementById(mode+'-owner');if(host){const root=host.attachShadow({mode});const iframe=document.createElement('iframe');iframe.src='/leaf?kind='+mode;root.appendChild(iframe)}};
        const slotted=document.getElementById('closed-slot-owner');if(slotted)slotted.attachShadow({mode:'closed'}).innerHTML='<div data-private><slot></slot></div>';</script>`;
    } else if (url.pathname === "/child") {
      body = `<h2>Same-URL child visible 中文 🪷</h2><label>Child value <input value="Read-only child value"></label>
        <button onclick="this.textContent='CHILD ACTION MUST NOT RUN'">Child control</button>${frame("/leaf?kind=nested")}`;
    } else if (url.pathname === "/cross") {
      body = `<p>DO_NOT_LEAK_CROSS_ORIGIN</p>${frame(`${origin}/leaf?kind=returned-origin`)}`;
    } else if (url.pathname === "/depth") {
      const level = Math.min(8, Math.max(1, Number(url.searchParams.get("level")) || 1));
      body = `<p>Depth ${level}</p>${level < 6 ? frame(`/depth?level=${level + 1}`) : ""}`;
    } else if (url.pathname === "/large") {
      body = Array.from({ length: 1200 }, (_, index) => `<p>${index} ${"界".repeat(490)}</p>`).join("");
    } else {
      const kind = url.searchParams.get("kind");
      const excluded = ["hidden", "sensitive", "sandbox", "opaque", "open", "closed", "closed-slot", "zero", "returned-origin"];
      body = `<p>${excluded.includes(kind) ? `DO_NOT_LEAK_${kind.toUpperCase()}` : kind === "nested" ? "Nested visible text 中文 🪷" : "Visible leaf text"}</p>`;
    }
    response.writeHead(200, { "content-type": "text/html; charset=utf-8", "cache-control": "no-store" });
    response.end(html(body));
  };
  const servers = [http.createServer(handler), http.createServer(handler)];
  for (const server of servers) await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
  origin = `http://127.0.0.1:${servers[0].address().port}`;
  otherOrigin = `http://127.0.0.1:${servers[1].address().port}`;
  return { origin, otherOrigin, url: `${origin}/top`,
    close: () => Promise.all(servers.map((server) => new Promise((resolve) => server.close(resolve)))) };
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  const fixture = await startFixture();
  console.log(JSON.stringify({ pid: process.pid, top: fixture.url, otherOrigin: fixture.otherOrigin }));
  for (const signal of ["SIGINT", "SIGTERM"]) process.once(signal, async () => { await fixture.close(); process.exit(0); });
}
