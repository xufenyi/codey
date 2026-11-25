'use strict';
const $ = id => document.getElementById(id);
const fragment = new URLSearchParams(location.hash.slice(1));
const token = fragment.get('token') || sessionStorage.getItem('codey-token') || '';
const exitRequested = fragment.get('exit') === '1';
if (token) sessionStorage.setItem('codey-token', token);
history.replaceState(null, '', '/');
let state = null, models = [], threads = [], threadCursor = null, editing = null;
let currentView = 'workspace', jobsPage = 0, historyPage = 0, polling = false, leaving = false;
let browseTarget = null, browserState = null, knownApprovals = new Set();
let renderedJobsKey = '', renderedApprovalsKey = '';
const labels = {scheduled:'等待执行',checking:'检查中',sending:'正在提交',running:'执行中',approval:'等待审批',confirmation:'等待确认',paused:'已暂停',review:'结果待核对',completed:'已完成',failed:'执行失败',cancelled:'已取消',skipped:'已跳过',expired:'等待已到期',interrupted:'已中断'};
const terminal = new Set(['completed','failed','cancelled','skipped','expired','interrupted']);
const active = new Set(['checking','sending','running','approval']);
const attention = new Set(['approval','confirmation','paused','review']);
const date = ms => ms ? new Date(ms).toLocaleString('zh-CN',{month:'2-digit',day:'2-digit',hour:'2-digit',minute:'2-digit',hour12:false}) : '—';
function element(tag, className, text) { const node = document.createElement(tag); if (className) node.className = className; if (text !== undefined) node.textContent = text; return node; }
function show(node, visible) { node.classList.toggle('hidden', !visible); }
function toast(message) { $('toast').textContent=message;show($('toast'),true);clearTimeout(toast.timer);toast.timer=setTimeout(()=>show($('toast'),false),4200); }
function error(message) { $('error-banner').textContent=message;show($('error-banner'),true); }
async function api(path, body) {
  const response=await fetch('/api/'+path,{method:body===undefined?'GET':'POST',headers:{'Authorization':'Bearer '+token,'Content-Type':'application/json'},body:body===undefined?undefined:JSON.stringify(body),signal:AbortSignal.timeout(65000)});
  let value;try{value=await response.json();}catch{throw new Error('后台返回了无法读取的响应');}
  if(!response.ok)throw new Error(value.error||'请求失败');return value;
}
async function busy(button, action) { const original=button.textContent;button.disabled=true;button.textContent='处理中…';try{return await action();}catch(e){error(e.message);}finally{button.disabled=false;button.textContent=button.id==='save-job'?(editing?'保存修改':'安排任务 ↗'):original;} }
function changeView(view) {
  currentView=view;for(const name of ['workspace','history','settings'])show($('view-'+name),name===view);
  document.querySelectorAll('[data-view]').forEach(b=>b.classList.toggle('selected',b.dataset.view===view));
  const titles={workspace:['任务工作台','把工作安排好，让 Codex 在合适的时间接着做。'],history:['执行记录','每一项工作的来路与结果，都有迹可循。'],settings:['运行环境','连接本机 Codex，让每次执行都有明确的环境。']};
  $('page-title').replaceChildren(document.createTextNode(titles[view][0]),element('span','heading-dot','.'));$('page-subtitle').textContent=titles[view][1];
}
document.querySelectorAll('[data-view]').forEach(b=>b.addEventListener('click',()=>changeView(b.dataset.view)));
document.querySelectorAll('[data-close]').forEach(b=>b.addEventListener('click',()=>$(b.dataset.close).close()));

async function poll() {
  if(polling||leaving)return;polling=true;
  try {
    state=await api('state');render();show($('connection-banner'),state.draining);
    if(state.draining)$('connection-banner').textContent='后台正在等待执行结束后退出，已停止启动新任务。';
    if(state.lastError)error(state.lastError);
  }catch(e){$('connection-banner').textContent='无法连接后台：'+e.message+'。可通过系统托盘重新打开 Codey。';show($('connection-banner'),true);}
  finally{polling=false;}
}
function render() {
  const jobs=state.jobs;
  $('count-scheduled').textContent=jobs.filter(j=>j.status==='scheduled').length;
  $('count-running').textContent=jobs.filter(j=>active.has(j.status)).length;
  $('count-attention').textContent=jobs.filter(j=>attention.has(j.status)).length;
  $('queue-count').textContent=jobs.filter(j=>!terminal.has(j.status)).length;
  $('app-version').textContent=state.version;$('data-dir').textContent=state.dataDir;
  if(state.runtime)$('runtime-version').textContent=state.runtime.version;
  const jobsKey=JSON.stringify(jobs.map(j=>[j.id,j.updatedAt,j.status,j.output.length]));
  if(jobsKey!==renderedJobsKey){renderJobs();renderedJobsKey=jobsKey;}
  const approvalsKey=JSON.stringify(state.approvals);
  if(approvalsKey!==renderedApprovalsKey){renderApprovals();renderedApprovalsKey=approvalsKey;}
  if($('detail-dialog').open){const j=jobs.find(j=>j.id===$('detail-dialog').dataset.jobId);if(j)renderDetail(j);}
}
function empty(title, subtitle) {const box=element('div','empty-state');box.append(element('span','empty-symbol','◷'),element('strong','',title),element('p','',subtitle));return box;}
function card(job) {
  const card=element('article','job-card');const top=element('div','job-top');
  const color=job.status==='completed'?'green':attention.has(job.status)?'amber':job.status==='failed'?'red':active.has(job.status)||job.status==='scheduled'?'blue':'';
  top.append(element('strong','job-title',job.title||job.prompt.slice(0,45)),element('span','tag '+color,labels[job.status]||job.status));
  const meta=element('div','job-meta');meta.append(element('span','',job.sendMode==='available'?'◷ 可用时执行':'◷ '+date(job.sendAt)),element('span','',job.threadMode==='new'?'＋ 新建对话':'↳ 已有对话'));
  if(job.sendMode==='available')meta.append(element('span','',`截止 ${date(job.expiresAt)}`));
  card.append(top,element('p','job-preview',job.prompt),meta,element('div','job-path',job.cwd));
  if(job.reason)card.append(element('div','job-reason',job.reason));
  const actions=element('div','job-actions');
  function button(label, action) {const b=element('button','text-button',label);b.addEventListener('click',()=>action(b));actions.append(b);}
  button('查看详情',()=>openDetail(job));
  if(job.status==='scheduled')button(job.sendMode==='available'?'立即检查':'立即执行',b=>busy(b,()=>jobAction(job,'now')));
  if(['scheduled','paused','confirmation','expired'].includes(job.status)&&!job.submittedAt)button('编辑',()=>editJob(job));
  if(job.status==='confirmation'){
    button('确认执行',b=>busy(b,()=>jobAction(job,'confirm')));button('跳过',b=>busy(b,()=>jobAction(job,'skip')));
  }
  if(active.has(job.status))button('请求中断',b=>{if(confirm('请求中断这次执行？提交后的结果会保留供核对。'))return busy(b,()=>jobAction(job,'interrupt'));});
  if(!active.has(job.status)&&!terminal.has(job.status)&&job.status!=='review')button('取消任务',b=>busy(b,()=>jobAction(job,'cancel')));
  if(job.status==='review')button('核对执行状态',b=>busy(b,async()=>{await api('history/refresh',{});await poll();toast('已按轮次和消息 ID 核对；没有确切结果的记录继续保留。');}));
  card.append(actions);return card;
}
async function jobAction(job,action) {await api('jobs/'+job.id,{action,confirmationId:job.confirmationId});await poll();toast('任务状态已更新');}
function paginate(container,total,page,setPage) {
  container.replaceChildren();const pages=Math.ceil(total/25);if(pages<=1)return;
  const prev=element('button','text-button','← 上一页');prev.disabled=page===0;prev.onclick=()=>setPage(page-1);
  const next=element('button','text-button','下一页 →');next.disabled=page>=pages-1;next.onclick=()=>setPage(page+1);
  container.append(prev,element('span','',`${page+1} / ${pages}`),next);
}
function renderJobs() {
  const search=$('job-search').value.toLowerCase();const filter=$('job-filter').value;
  const all=state.jobs.filter(j=>!terminal.has(j.status)).filter(j=>(j.title+' '+j.prompt).toLowerCase().includes(search)).filter(j=>filter==='all'||(filter==='attention'?attention.has(j.status):filter==='running'?active.has(j.status):j.status===filter)).sort((a,b)=>a.sendAt-b.sendAt);
  jobsPage=Math.min(jobsPage,Math.max(0,Math.ceil(all.length/25)-1));$('job-list').replaceChildren(...all.slice(jobsPage*25,jobsPage*25+25).map(card));
  if(!all.length)$('job-list').append(empty('下一项工作，从这里开始','写下指令，选择时间。\nCodey 会在后台等待合适的时机。'));
  paginate($('jobs-pagination'),all.length,jobsPage,p=>{jobsPage=p;renderJobs();});
  const history=state.jobs.filter(j=>terminal.has(j.status)||j.status==='review').sort((a,b)=>(b.finishedAt||b.updatedAt)-(a.finishedAt||a.updatedAt));
  historyPage=Math.min(historyPage,Math.max(0,Math.ceil(history.length/25)-1));$('history-list').replaceChildren(...history.slice(historyPage*25,historyPage*25+25).map(card));
  if(!history.length)$('history-list').append(empty('还没有执行记录','任务结束后，结果和输出会出现在这里。'));
  paginate($('history-pagination'),history.length,historyPage,p=>{historyPage=p;renderJobs();});
}
function renderApprovals() {
  show($('approval-section'),state.approvals.length>0);$('approval-list').replaceChildren();
  for(const approval of state.approvals){
    const job=state.jobs.find(j=>j.id===approval.jobId);const box=element('div','approval-card');
    box.append(element('strong','',job?.title||'任务审批'),element('p','hint',`${approval.method} · 等待至 ${date(approval.expiresAt)}`),element('pre','',JSON.stringify(approval.detail,null,2)));
    const actions=element('div','input-row');for(const[allow,label]of [[true,'允许这一次'],[false,'拒绝并中断']]){const b=element('button','button '+(allow?'primary':'secondary'),label);b.onclick=()=>busy(b,async()=>{await api('approvals/'+approval.id,{allow});await poll();});actions.append(b);}box.append(actions);$('approval-list').append(box);
    if(!knownApprovals.has(approval.id)){knownApprovals.add(approval.id);if('Notification'in window&&Notification.permission==='granted')new Notification('Codey 有任务等待审批',{body:job?.title||'请打开 Codey 管理页面处理。',tag:approval.id});}
  }
  document.title=state.approvals.length?`(${state.approvals.length}) 待审批 · Codey`:'Codey · 本地任务调度';
}
function openDetail(job) {$('detail-dialog').dataset.jobId=job.id;renderDetail(job);$('detail-dialog').showModal();}
function renderDetail(job) {
  $('detail-title').textContent=job.title||'任务详情';$('detail-meta').replaceChildren();
  for(const text of [`状态：${labels[job.status]||job.status}　${job.reason}`,`项目：${job.cwd}`,`对话：${job.threadId||'执行时创建'}　轮次：${job.turnId||'尚未接受'}`,`权限：${job.sandbox} / ${job.approvalPolicy} / 网络 ${job.networkAccess?'允许':'关闭'}`,`创建 ${date(job.createdAt)}　提交 ${date(job.submittedAt)}　完成 ${date(job.finishedAt)}`])$('detail-meta').append(element('div','meta-line',text));
  $('detail-prompt').textContent=job.prompt;$('detail-output').textContent=job.output||'暂无输出。结果待核对时，可在执行记录中按 ID 核对状态。';
}

function defaultTime() {const d=new Date(Date.now()+5*60000);d.setSeconds(0,0);const local=new Date(d.getTime()-d.getTimezoneOffset()*60000);return local.toISOString().slice(0,16);}
function setTime(ms) {const d=new Date(ms);return new Date(d.getTime()-d.getTimezoneOffset()*60000).toISOString().slice(0,16);}
function resetForm() {
  $('job-form').reset();editing=null;$('send-at').value=defaultTime();$('form-title').textContent='安排一项工作';$('save-job').replaceChildren(document.createTextNode('安排任务'),element('span','','↗'));show($('cancel-edit'),false);syncFields();updateSummary();
}
function syncFields() {
  const existing=$('thread-mode').value==='existing',available=$('send-mode').value==='available';
  show($('thread-fields'),existing);show($('model-fields'),!existing);show($('stale-field'),existing);show($('time-field'),!available);show($('wait-field'),available);$('send-at').required=!available;
  updateSummary();
}
function updateSummary() {
  const bytes=new TextEncoder().encode($('prompt').value).length;$('prompt-size').textContent=(bytes/1024).toFixed(1)+' / 64 KiB';
  const mode=$('send-mode').value==='available'?`条件满足时执行，最长等待 ${$('wait-days').value} 天`:`计划于 ${date(new Date($('send-at').value).getTime())} 执行`;
  $('save-summary').textContent=mode+' · 权限 '+$('sandbox').selectedOptions[0].textContent+' · '+$('approval-policy').selectedOptions[0].textContent;
}
function renderModels() {
  const selected=$('model').value;$('model').replaceChildren(new Option('Codex 默认模型',''));
  for(const m of models)$('model').add(new Option(m.displayName||m.model,m.model));if(models.some(m=>m.model===selected))$('model').value=selected;renderEfforts();
}
function renderEfforts() {
  const selected=$('effort').value;$('effort').replaceChildren(new Option('模型默认值',''));const m=models.find(m=>m.model===$('model').value);
  for(const e of m?.supportedReasoningEfforts||[])$('effort').add(new Option(e.reasoningEffort,e.reasoningEffort));if([...$('effort').options].some(o=>o.value===selected))$('effort').value=selected;
}
function renderThreads() {
  const selected=$('thread-id').value,search=$('thread-search').value.toLowerCase();$('thread-id').replaceChildren(new Option('请选择一个对话',''));
  for(const t of threads.filter(t=>(`${t.name||t.preview||''} ${t.cwd} ${t.id}`).toLowerCase().includes(search)))$('thread-id').add(new Option((t.name||t.preview||t.id).slice(0,65)+' · '+t.cwd,t.id));
  if([...$('thread-id').options].some(o=>o.value===selected))$('thread-id').value=selected;show($('more-threads'),!!threadCursor);
}
async function loadThreads(more=false) {
  const result=await api('threads',{cursor:more?threadCursor:null});
  threads=more?threads.concat(result.data||[]):result.data||[];threads=[...new Map(threads.map(t=>[t.id,t])).values()];threadCursor=result.nextCursor;renderThreads();toast(`已读取 ${threads.length} 个对话`);
}
async function editJob(job) {
  try{
    if(job.threadMode==='existing'&&!threads.some(t=>t.id===job.threadId)){threads.push({id:job.threadId,name:job.title||job.threadId,cwd:job.cwd});renderThreads();}
    editing=job;$('job-title').value=job.title;$('cwd').value=job.cwd;$('prompt').value=job.prompt;$('thread-mode').value=job.threadMode;$('thread-id').value=job.threadId||'';
    $('send-mode').value=job.sendMode;$('send-at').value=setTime(Math.max(job.sendAt,Date.now()+60000));$('wait-days').value=Math.max(1,Math.min(30,Math.ceil(((job.expiresAt||Date.now()+8*86400000)-Date.now())/86400000)));
    $('model').value=job.model||'';renderEfforts();$('effort').value=job.effort||'';$('sandbox').value=job.sandbox;$('approval-policy').value=job.approvalPolicy;$('network').checked=job.networkAccess;$('stale-policy').value=job.stalePolicy;$('missed-policy').value=job.missedPolicy;
    $('form-title').textContent='编辑任务';$('save-job').textContent='保存修改';show($('cancel-edit'),true);syncFields();changeView('workspace');$('job-form').scrollIntoView({behavior:'smooth',block:'start'});
  }catch(e){error(e.message);}
}
$('job-form').addEventListener('submit',event=>{
  event.preventDefault();busy($('save-job'),async()=>{
    const existing=$('thread-mode').value==='existing',available=$('send-mode').value==='available';
    const body={title:$('job-title').value.trim(),cwd:$('cwd').value.trim(),prompt:$('prompt').value,threadMode:existing?'existing':'new',threadId:existing?$('thread-id').value:null,
      sendMode:available?'available':'time',sendAt:available?Date.now():new Date($('send-at').value).getTime(),expiresAt:available?Date.now()+Number($('wait-days').value)*86400000:null,
      model:existing?null:$('model').value||null,effort:existing?null:$('effort').value||null,sandbox:$('sandbox').value,approvalPolicy:$('approval-policy').value,networkAccess:$('network').checked,
      stalePolicy:existing?$('stale-policy').value:'send',missedPolicy:$('missed-policy').value};
    if(editing){body.id=editing.id;body.expectedUpdatedAt=editing.updatedAt;}
    await api('jobs',body);const wasEditing=!!editing;resetForm();await poll();toast(wasEditing?'任务修改已保存':'任务已安排，关闭浏览器后仍会继续执行');
  });
});
$('reset-form').onclick=resetForm;$('cancel-edit').onclick=resetForm;
for(const id of ['thread-mode','send-mode'])$(id).addEventListener('change',syncFields);
for(const id of ['prompt','send-at','wait-days','sandbox','approval-policy'])$(id).addEventListener('input',updateSummary);
$('model').onchange=renderEfforts;$('thread-search').oninput=renderThreads;
$('thread-id').onchange=()=>{const t=threads.find(t=>t.id===$('thread-id').value);if(t?.cwd)$('cwd').value=t.cwd;};
$('refresh-threads').onclick=event=>busy(event.currentTarget,()=>loadThreads());$('more-threads').onclick=event=>busy(event.currentTarget,()=>loadThreads(true));
$('job-search').oninput=()=>{jobsPage=0;if(state)renderJobs();};$('job-filter').onchange=()=>{jobsPage=0;if(state)renderJobs();};

async function probe() {
  const result=await api('runtime',{path:$('codex-path').value.trim()});models=result.models.data||[];renderModels();
  $('runtime-version').textContent=result.runtime.version;$('account-label').textContent=result.account.account?.email||result.account.account?.type||'尚未登录';
  const dl=element('dl');for(const[k,v]of [['检测结果',result.runtime.source==='PATH'?'已从系统 PATH 自动发现':'已验证手动指定的入口'],['命令入口',result.runtime.entry],['实际二进制',result.runtime.path],['版本',result.runtime.version],['协议','initialize、account/read、model/list 已通过'],['账户',result.bindingError||result.account.account?.email||'已连接']])dl.append(element('dt','',k),element('dd','',v));$('runtime-result').replaceChildren(dl);
  if(result.bindingError)error(result.bindingError);else show($('error-banner'),false);return result;
}
$('probe-runtime').onclick=event=>busy(event.currentTarget,probe);
$('save-settings').onclick=event=>busy(event.currentTarget,async()=>{await api('settings',{codexPath:$('codex-path').value.trim(),approvalTimeoutSecs:Number($('approval-timeout').value),historyRetentionDays:Number($('retention').value)});await probe();await poll();toast('运行设置已保存');});
$('refresh-history').onclick=event=>busy(event.currentTarget,async()=>{const r=await api('history/refresh',{});await poll();toast(`已核对，更新 ${r.updated} 条记录`);});
$('clear-history').onclick=event=>{if(confirm('清理所有已结束记录？结果待核对、等待中及运行中的任务会保留。'))busy(event.currentTarget,async()=>{const r=await api('history/clear',{});await poll();toast(`已清理 ${r.removed} 条记录`);});};

async function browse(path) {
  browserState=await api('browse',{path});$('browse-path').value=browserState.path;$('browse-list').replaceChildren();
  for(const d of browserState.directories){const b=element('button','file-entry','▸  '+d.name);b.onclick=()=>browse(d.path).catch(e=>error(e.message));$('browse-list').append(b);}
  if(browseTarget==='runtime')for(const f of browserState.files){const b=element('button','file-entry','   '+f.name);b.onclick=()=>{$('codex-path').value=f.path;$('browse-dialog').close();};$('browse-list').append(b);}
  show($('browse-select'),browseTarget==='cwd');$('browse-select').disabled=!browserState.path;
}
async function openBrowse(target) {browseTarget=target;$('browse-title').textContent=target==='cwd'?'选择项目目录':'定位 Codex 命令入口';$('browse-dialog').showModal();try{await browse(target==='cwd'?$('cwd').value.trim():'');}catch{await browse('');}}
$('browse-cwd').onclick=()=>openBrowse('cwd').catch(e=>error(e.message));$('browse-runtime').onclick=()=>openBrowse('runtime').catch(e=>error(e.message));
$('browse-select').onclick=()=>{$('cwd').value=browserState.path;$('browse-dialog').close();};$('browse-go').onclick=()=>browse($('browse-path').value).catch(e=>error(e.message));
$('browse-path').onkeydown=e=>{if(e.key==='Enter'){e.preventDefault();$('browse-go').click();}};
$('browse-roots').onclick=()=>browse('').catch(e=>error(e.message));$('browse-up').onclick=()=>browse(browserState.parent||'').catch(e=>error(e.message));$('browse-home').onclick=()=>browse(browserState.home||'').catch(e=>error(e.message));
$('notifications-button').onclick=async()=>{if(!('Notification'in window)){toast('此浏览器不支持桌面通知');return}const permission=await Notification.requestPermission();toast(permission==='granted'?'已启用浏览器通知；关闭页面后可通过托盘查看待审批状态':'未启用通知，仍可在页面与托盘查看状态');};
function openExit() {$('exit-active').textContent=`当前有 ${state?.jobs.filter(j=>active.has(j.status)).length||0} 项任务执行中。`;$('exit-dialog').showModal();}
$('exit-button').onclick=openExit;
async function exit(mode) {await api('shutdown',{mode});leaving=true;$('exit-dialog').close();$('connection-banner').textContent=mode==='wait'?'后台正在等待当前任务结束后退出。':'已请求中断执行并退出后台。';show($('connection-banner'),true);}
$('exit-wait').onclick=event=>busy(event.currentTarget,()=>exit('wait'));$('exit-interrupt').onclick=event=>busy(event.currentTarget,()=>exit('interrupt'));

async function init() {
  resetForm();await poll();
  if(state){$('codex-path').value=state.settings.codexPath;$('approval-timeout').value=state.settings.approvalTimeoutSecs;$('retention').value=state.settings.historyRetentionDays;
    try{await probe();await loadThreads();}catch(e){error(e.message);changeView('settings');}
    if(exitRequested)openExit();
  }setInterval(poll,2000);
}
init().catch(e=>error(e.message));
