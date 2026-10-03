import http from "node:http";
import { pathToFileURL } from "node:url";

// Owned loopback fixture only. Its main-world proof API is independent of Nova.
export async function startFixture() {
  let origin;
  let otherOrigin;
  const html = (body) => `<!doctype html><meta charset="utf-8"><title>Nova child activation fixture</title>
    <style>body{font:16px/1.5 system-ui;margin:20px}iframe{width:430px;height:510px;margin:8px;border:1px solid #888}button{font:inherit;padding:6px;margin:4px}output{display:block}</style>${body}`;
  const proof = `<script>
    const proof=window.NovaChildActivationProof={documentId:crypto.randomUUID(),dispatches:{},trustedDispatches:{},publicCount:0,privateCount:0,hiddenCount:0,trustedCount:0};
    const byId=(id)=>document.getElementById(id);
    const on=(id,effect=()=>{})=>{const button=byId(id);if(!button)return;proof.dispatches[id]=0;proof.trustedDispatches[id]=0;button.onclick=(event)=>{proof.dispatches[id]++;if(event.isTrusted)proof.trustedDispatches[id]++;effect(event)}};
    on('top-action',()=>{byId('public-status').textContent='Top count '+(++proof.publicCount)});
    on('public-action',()=>{byId('public-status').textContent='Child count '+(++proof.publicCount)});
    on('inert-action');
    on('trusted-action',(event)=>{if(event.isTrusted)byId('trusted-status').textContent='Trusted count '+(++proof.trustedCount)});
    on('private-action',()=>{byId('private-status').textContent='EXCLUDE_PRIVATE_'+(++proof.privateCount)});
    on('hidden-action',()=>{byId('hidden-status').textContent='EXCLUDE_HIDDEN_'+(++proof.hiddenCount)});
    on('self-label-action',()=>{byId('self-label-action').textContent='Changed own public label'});
    on('slow-action',()=>{const end=performance.now()+11000;while(performance.now()<end){}byId('public-status').textContent='Slow public effect'});
    proof.changeTarget=(kind)=>{const target=byId('public-action');if(kind==='rename')target.textContent='Renamed child target';if(kind==='role')target.setAttribute('role','heading');if(kind==='disabled')target.disabled=true;if(kind==='hidden')target.hidden=true;if(kind==='private')target.setAttribute('data-private','')};
    proof.changeOwner=(id,kind)=>{const owner=byId(id);if(kind==='hidden')owner.hidden=true;if(kind==='private')owner.setAttribute('data-private','');if(kind==='sandbox')owner.setAttribute('sandbox','allow-same-origin allow-scripts');if(kind==='navigate')owner.contentWindow.location.reload();if(kind==='replace')owner.replaceWith(owner.cloneNode(true))};
    proof.inspect=()=>({documentId:proof.documentId,dispatches:{...proof.dispatches},trustedDispatches:{...proof.trustedDispatches},publicCount:proof.publicCount,privateCount:proof.privateCount,hiddenCount:proof.hiddenCount,trustedCount:proof.trustedCount,publicText:byId('public-status')?.textContent,targetLabel:byId('public-action')?.textContent,targetHidden:byId('public-action')?.hidden,targetDisabled:byId('public-action')?.disabled,frames:[...document.querySelectorAll('iframe')].map((owner)=>{let child;try{child=owner.contentWindow.NovaChildActivationProof?.inspect()}catch{}return{id:owner.id,hidden:owner.hidden,private:owner.hasAttribute('data-private'),sandbox:owner.hasAttribute('sandbox'),child}})});
  </script>`;
  const controls = `<button id="public-action">Shared child action</button><output id="public-status">Child count 0</output>
    <button id="inert-action">Inert child action</button><button id="trusted-action">Trusted child action</button><output id="trusted-status">Trusted count 0</output>
    <button id="private-action">Private child effect</button><div data-private><output id="private-status">EXCLUDE_PRIVATE_0</output></div>
    <button id="hidden-action">Hidden child effect</button><output id="hidden-status" hidden>EXCLUDE_HIDDEN_0</output>
    <button id="self-label-action">Change own child label</button><button id="slow-action">Slow child effect</button>
    <label>Child value stays read-only <input id="child-value" value="Unchanged child value 中文 🪷"></label>`;
  const handler = (request, response) => {
    const url = new URL(request.url, origin);
    let body;
    if (url.pathname === "/top") {
      const mode=url.searchParams.get("case");
      body=`<h1>Exact child DOM activation</h1><p>Same-URL siblings contain same-label buttons and nested children. Each action requires a fresh aggregate snapshot.</p>
        <button id="top-action">Top public action</button><output id="public-status">Top count 0</output>
        <iframe id="first-child" src="/child"></iframe><iframe id="second-child" src="/child"></iframe>`;
      if (mode === "exclusions") body += `<iframe id="cross-child" src="${otherOrigin}/cross"></iframe><iframe id="sandbox-child" sandbox="allow-same-origin allow-scripts" src="/child"></iframe><div data-private><iframe id="private-owner" src="/child"></iframe></div>`;
      if (mode === "documents") body += Array.from({length:10},(_,i)=>`<iframe id="extra-${i}" src="/nested"></iframe>`).join("");
    } else if (url.pathname === "/other-tab") {
      body=`<h1>Other active tab</h1><button id="top-action">Top public action</button><output id="public-status">Top count 0</output>`;
    } else if (url.pathname === "/cross") {
      body="<p>EXCLUDE_CROSS_ORIGIN_CHILD</p>";
    } else {
      body=`<h2>${url.pathname === "/nested" ? "Nested child" : "Same-URL sibling child"}</h2>${controls}`;
      if (url.pathname === "/child") body += '<iframe id="nested-child" src="/nested" style="height:450px"></iframe>';
      if (url.searchParams.get("case") === "budget") body += Array.from({length:800},(_,i)=>`<p>Filler ${i}: ${"bounded content 中文 🪷 ".repeat(12)}</p>`).join("");
    }
    response.writeHead(200,{"content-type":"text/html; charset=utf-8","cache-control":"no-store"});
    response.end(html(body+proof));
  };
  const servers=[http.createServer(handler),http.createServer(handler)];
  for (const server of servers) await new Promise((resolve)=>server.listen(0,"127.0.0.1",resolve));
  origin=`http://127.0.0.1:${servers[0].address().port}`;
  otherOrigin=`http://127.0.0.1:${servers[1].address().port}`;
  return {origin,otherOrigin,url:`${origin}/top`,close:()=>Promise.all(servers.map((server)=>new Promise((resolve)=>server.close(resolve))))};
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  const fixture=await startFixture();
  console.log(JSON.stringify({pid:process.pid,top:fixture.url,otherOrigin:fixture.otherOrigin}));
  for (const signal of ["SIGINT","SIGTERM"]) process.once(signal,async()=>{await fixture.close();process.exit(0)});
}
