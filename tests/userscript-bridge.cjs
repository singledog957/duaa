const vm = require('node:vm');
const fs = require('node:fs');
const assert = require('node:assert/strict');
async function test(tampermonkey, early = false) {
  const listeners = [], timers = [], data = new Map();
  const requests = [];
  const window = {
    addEventListener: (_, fn) => listeners.push(fn),
    postMessage: message => queueMicrotask(() => listeners.forEach(fn => fn({source:window,origin:'https://duaa.singledog233.top',data:message}))),
  };
  window.top = window;
  window.self = window;
  const base = {window,location:{origin:'https://duaa.singledog233.top'},console,URLSearchParams,
    setTimeout:(fn,ms)=>{const t=setTimeout(fn,ms);timers.push(t);return t},clearTimeout,
    setInterval:(fn,ms)=>{const t=setInterval(fn,ms);timers.push(t);return t},clearInterval,alert:()=>{}};
  const page = vm.createContext(base);
  const document = {
    createElement: () => ({textContent:'',remove(){}}),
    documentElement: {appendChild: script => vm.runInContext(script.textContent, page)},
  };
  const root = document.documentElement;
  let observer;
  if (early) document.documentElement = null;
  const content = vm.createContext({...base,document,crypto:require('node:crypto').webcrypto,
    MutationObserver: class { constructor(fn){observer=fn} observe(){} disconnect(){} },
    ...(tampermonkey ? {unsafeWindow:window} : {}),GM:{
    getValue:async(k,d)=>data.has(k)?data.get(k):d,
    setValue:async(k,v)=>{await new Promise(r=>setTimeout(r,5));data.set(k,v)}},
    GM_xmlhttpRequest:options=>{
      requests.push(options);
      let result;
      if(options.url.includes('login_buaa')) result={STATUS:'0',result:{id:'class-1',realName:'测试'}};
      else result={STATUS:'0',result:[{id:'schedule-1',courseId:'course-1',courseName:'课程',classBeginTime:'2026-09-14 08:00',classEndTime:'2026-09-14 09:00',signStatus:'0'}]};
      queueMicrotask(()=>options.onload({status:200,responseText:JSON.stringify(result)}));
    }});
  const run=()=>vm.runInContext(fs.readFileSync(require('node:path').join(__dirname, '..', 'duaa.js'),'utf8'),content);
  try {
    run();
    if (early) {document.documentElement=root;observer()}
    await new Promise(r=>setTimeout(r,1));
    const bridge=window.__checkinBridge;
    assert(bridge);
    const save=bridge.saveLoginName('123','sso-name');
    const schedule=await bridge.querySchedule('123','2026-09-14');
    await save;
    assert.equal(new URL(requests[0].url).searchParams.get('phone'),'sso-name');
    assert.equal(schedule.student_name,'测试');
    assert.equal(schedule.schedules[0].time,'2026-09-14T08:00:00+08:00');
    assert.equal(bridge.matchSchedule({name:'课程'},['课程']),true);
    await bridge.querySchedule('123','2026-09-14');
    assert.equal(requests.filter(r=>r.url.includes('login_buaa')).length,1);
    content.GM_xmlhttpRequest=o=>queueMicrotask(()=>o.onerror({error:'模拟网络失败'}));
    await assert.rejects(bridge.querySchedule('456','2026-09-14'),e=>e.response.data.error.code==='login_sso_required');
    console.log('PASS',tampermonkey?'Tampermonkey API mock':early?'Userscripts early DOM':'Userscripts isolated page','storage ordering, schedule, cache, structured errors');
  } finally {timers.forEach(t=>{clearTimeout(t);clearInterval(t)})}
}
(async()=>{await test(false);await test(false,true);await test(true)})().catch(e=>{console.error(e);process.exitCode=1});
