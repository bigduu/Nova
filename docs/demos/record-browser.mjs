// Run from Nova's root. Requires NOVA_BIN, PLAYWRIGHT_MODULE and ffmpeg.
// The adapter adds a dedicated recording-browser URL, preserving Nova's arguments.
import { createRequire } from 'node:module';
import { spawn } from 'node:child_process';
import { createServer } from 'node:http';
import { readFile, writeFile, mkdir, mkdtemp, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import path from 'node:path';
const require = createRequire(import.meta.url);
const { chromium } = require(process.env.PLAYWRIGHT_MODULE || 'playwright');
const out = path.resolve('docs/demos');
const temp = await mkdtemp(path.join(tmpdir(),'nova-demo-'));
const log=[];
let browser, child;
const server = createServer(async (_req,res)=>{res.setHeader('Content-Type','text/html');res.end(await readFile(path.join(out,'fixture.html')));});
await new Promise(r=>server.listen(9751,'127.0.0.1',r));
try {
 browser=await chromium.launch({headless:true,args:['--remote-debugging-port=9752']});
 const context=await browser.newContext({viewport:{width:1000,height:800},recordVideo:{dir:temp,size:{width:1000,height:800}}});
 const page=await context.newPage();const videoStart=Date.now();
 const adapter=path.join(temp,'npx-recording');
 await writeFile(adapter,'#!/bin/sh\nexec npx "$@" --browserUrl=http://127.0.0.1:9752\n',{mode:0o755});
 child=spawn(process.env.NOVA_BIN || 'nova',['chrome-devtools','--headless','--npx',adapter],{stdio:['pipe','pipe','pipe']});
 let next=0,buffer='';const pending=new Map();
 child.stdout.on('data',b=>{buffer+=b;let i;while((i=buffer.indexOf('\n'))>=0){const line=buffer.slice(0,i);buffer=buffer.slice(i+1);if(!line)continue;const msg=JSON.parse(line);if(msg.id&&pending.has(msg.id)){pending.get(msg.id)(msg);pending.delete(msg.id);}}});
 child.stderr.on('data',b=>process.stderr.write(b));
 const rpc=(method,params)=>new Promise((resolve,reject)=>{const id=++next;const timer=setTimeout(()=>reject(new Error('MCP timeout: '+method)),60000);pending.set(id,m=>{clearTimeout(timer);if(m.error)reject(new Error(JSON.stringify(m.error)));else resolve(m.result);});child.stdin.write(JSON.stringify({jsonrpc:'2.0',id,method,params})+'\n');});
 const call=async(name,args)=>{const result=await rpc('tools/call',{name,arguments:args});log.push({name,arguments:args,result});if(result.isError)throw new Error(JSON.stringify(result));return result;};
 await rpc('initialize',{protocolVersion:'2024-11-05',capabilities:{},clientInfo:{name:'nova-readme-demo',version:'1.0.0'}});
 child.stdin.write(JSON.stringify({jsonrpc:'2.0',method:'notifications/initialized'})+'\n');
 const list=await rpc('tools/list',{});await writeFile(path.join(temp,'tools.json'),JSON.stringify(list,null,2));
 const pages=await call('list_pages',{});console.log(JSON.stringify(pages));
 // Browser has only the fresh recording page; page ID is supplied by real MCP output.
 const pageText=pages.content.filter(c=>c.type==='text').map(c=>c.text).join('\n');
 const pageId=Number(pageText.match(/(?:^|\n)(\d+):/)?.[1]);
 if(!Number.isFinite(pageId))throw new Error('Could not identify recording page: '+pageText);
 await call('navigate_page',{pageId,type:'url',url:'http://127.0.0.1:9751'});
 console.log('TRIM_SECONDS='+((Date.now()-videoStart)/1000));
 await page.waitForTimeout(3000);
 let snapshot=await call('take_snapshot',{pageId});
 const click=async(label)=>{const text=snapshot.content.filter(c=>c.type==='text').map(c=>c.text).join('\n');const row=text.split('\n').find(l=>l.includes('"'+label+'"'));const uid=row?.match(/uid=([^\s]+)/)?.[1];if(!uid)throw new Error('Missing '+label);await call('click',{pageId,uid});await page.waitForTimeout(2500);snapshot=await call('take_snapshot',{pageId});};
 await click('Verify README links');await click('Review recorded demo');await click('Prepare review');
 await page.getByRole('status').filter({hasText:'Ready for review — 2 of 2 checks complete.'}).waitFor();
 await page.screenshot({path:path.join(out,'browser-checklist.png')});
 await page.waitForTimeout(3000);
 const video=page.video();await context.close();await video.saveAs(path.join(temp,'browser.webm'));
 await writeFile(path.join(out,'browser-evidence.json'),JSON.stringify({source:'Nova current checkout; source-only, not release v0.2.1',adapter:'--npx runner appends browserUrl to dedicated Playwright browser; all interactions use MCP',calls:log},null,2)+'\n');
 console.log('VIDEO='+path.join(temp,'browser.webm'));
} finally {child?.kill();await browser?.close();server.close();}
