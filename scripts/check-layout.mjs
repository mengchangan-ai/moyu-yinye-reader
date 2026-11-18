import fs from "node:fs";
const list = await (await fetch('http://127.0.0.1:9359/json')).json();
const t = list.find(x=>x.type==='page');
const ws = new WebSocket(t.webSocketDebuggerUrl);
let id=0; const p=new Map();
const send=(m,params={})=>new Promise(r=>{const i=++id;p.set(i,r);ws.send(JSON.stringify({id:i,method:m,params}));});
ws.onmessage=e=>{const m=JSON.parse(e.data); if(m.id&&p.has(m.id)){p.get(m.id)(m);p.delete(m.id);}};
await new Promise(r=>ws.onopen=r);
await send('Page.enable'); await send('Runtime.enable');
const MOCK = `(() => {
  const toc = Array.from({length: 30}, (_, i) => ({ title: '第' + (i+1) + '章 一个比较长的章节标题用来测试', url: 'http://x/ch/' + i }));
  const text = Array.from({length: 60}, (_, i) => '第 ' + (i+1) + ' 行正文，测试窗口缩放后的自适应排版效果。').join('\\n');
  const shelf = Array.from({length: 6}, (_, i) => ({ id:'id'+i, sourceId:1, url:'http://x/'+i, title:'书名'+(i+1)+'很长的书名测试', author:'作者'+(i+1), cover:null, lastChapter:i }));
  window.__TAURI__ = {
    core: { invoke: async (cmd) => {
      if (cmd === 'shelf_list') return shelf;
      if (cmd === 'open_book') return { title:'斗破苍穹', author:'天蚕土豆', cover:null, chapters:toc, cachedChapters:3, shelfId:'id0' };
      if (cmd === 'load_chapter') return text;
      if (cmd === 'prefetch_chapters') return 0;
      return null;
    } },
    window: { getCurrentWindow: () => ({ close(){}, minimize(){}, toggleMaximize(){}, setFocus(){}, startDragging(){return Promise.resolve();}, startResizeDragging(){return Promise.resolve();}, outerPosition(){return Promise.resolve({x:0,y:0});}, outerSize(){return Promise.resolve({width:1000,height:700});} }) },
    event: { listen: async () => () => {} },
  };
})();`;
await send("Page.addScriptToEvaluateOnNewDocument", { source: MOCK });
await send("Page.navigate", { url: `file://${process.cwd()}/src/index.html` });
await new Promise(r=>setTimeout(r,1300));
const ev=async(x)=>{const r=await send('Runtime.evaluate',{expression:x,returnByValue:true,awaitPromise:true}); return r.result?.result?.value ?? ('EXC '+(r.result?.exceptionDetails?.exception?.description||''));};
const shot=async(n)=>{const r=await send('Page.captureScreenshot',{format:'png'}); fs.writeFileSync(`/tmp/v9-${n}.png`, Buffer.from(r.result.data,'base64'));};
const size=async(w,h)=>{await send('Emulation.setDeviceMetricsOverride',{width:w,height:h,deviceScaleFactor:2,mobile:false}); await new Promise(r=>setTimeout(r,350));};

for (const [w,h] of [[1000,700],[760,520],[520,420],[420,320]]) {
  await size(w,h);
  console.log(`--- ${w}x${h}`);
  console.log("  toolbar overflow:", await ev("document.querySelector('.toolbar').scrollWidth - document.querySelector('.toolbar').clientWidth"));
  console.log("  home overflow:", await ev("document.querySelector('.home').scrollWidth - document.querySelector('.home').clientWidth"));
  console.log("  grid cols:", await ev("getComputedStyle(document.querySelector('.book-grid')).gridTemplateColumns.split(' ').length"));
  await shot(`home-${w}`);
}

await size(760,520);
await ev("document.querySelectorAll('.book-card')[0].click()"); await new Promise(r=>setTimeout(r,1200));
for (const [w,h] of [[760,520],[420,320]]) {
  await size(w,h);
  console.log(`--- reader ${w}x${h}`);
  console.log("  reader overflow:", await ev("document.querySelector('.reader').scrollWidth - document.querySelector('.reader').clientWidth"));
  console.log("  status overflow:", await ev("document.querySelector('.statusbar').scrollWidth - document.querySelector('.statusbar').clientWidth"));
  await shot(`reader-${w}`);
}
await ev("setDrawer(true)"); await new Promise(r=>setTimeout(r,450));
console.log("drawer:", await ev("JSON.stringify({w: Math.round(document.querySelector('#tocDrawer').getBoundingClientRect().width), anim: getComputedStyle(document.querySelector('#tocDrawer')).animationName, scrim: !document.querySelector('#drawerScrim').hidden})"));
await shot("drawer-small");
ws.close();
