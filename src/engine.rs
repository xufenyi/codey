use crate::{
    model::{Approval, Job, id, now},
    rpc::{Client, RpcError},
    runtime::{self, RuntimeInfo},
    store::Store,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{HashMap, HashSet},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

pub struct App {
    pub store: Mutex<Store>,
    pub runtime: Mutex<Option<RuntimeInfo>>,
    pub approvals: Mutex<Vec<Approval>>,
    pub active: Mutex<HashMap<String, Arc<AtomicBool>>>,
    pub stopping: AtomicBool,
    pub draining: AtomicBool,
    pub last_error: Mutex<String>,
}
impl App {
    pub fn new(store: Store) -> Arc<Self> {
        Arc::new(Self {
            store: Mutex::new(store),
            runtime: Mutex::new(None),
            approvals: Mutex::new(Vec::new()),
            active: Mutex::new(HashMap::new()),
            stopping: AtomicBool::new(false),
            draining: AtomicBool::new(false),
            last_error: Mutex::new(String::new()),
        })
    }
    pub fn runtime_info(&self) -> Result<RuntimeInfo, String> {
        let path = self.store.lock().unwrap().db.settings.codex_path.clone();
        let info = runtime::discover(Some(&path))?;
        *self.runtime.lock().unwrap() = Some(info.clone());
        Ok(info)
    }
    pub fn client(&self) -> Result<Client, String> {
        let r = self.runtime_info()?;
        Client::connect(&r.path).map_err(|e| e.message)
    }
    pub fn snapshot(&self) -> Value {
        let store = self.store.lock().unwrap();
        json!({"version":env!("CARGO_PKG_VERSION"),"platform":std::env::consts::OS,"settings":store.db.settings,"jobs":store.db.jobs,
            "runtime":*self.runtime.lock().unwrap(),"approvals":*self.approvals.lock().unwrap(),"dataDir":store.dir,
            "draining":self.draining.load(Ordering::SeqCst),"lastError":*self.last_error.lock().unwrap()})
    }
    pub fn update(&self, job_id: &str, f: impl FnOnce(&mut Job)) -> Result<(), String> {
        let result = self.store.lock().unwrap().change(|db| {
            let job = db
                .jobs
                .iter_mut()
                .find(|j| j.id == job_id)
                .ok_or("任务不存在")?;
            f(job);
            job.updated_at = now();
            Ok(())
        });
        if let Err(e) = &result {
            *self.last_error.lock().unwrap() = format!("持久化失败，已停止新任务调度：{e}");
            self.draining.store(true, Ordering::SeqCst);
        }
        result
    }
    pub fn get(&self, job_id: &str) -> Result<Job, String> {
        self.store
            .lock()
            .unwrap()
            .db
            .jobs
            .iter()
            .find(|j| j.id == job_id)
            .cloned()
            .ok_or("任务不存在".into())
    }
    pub fn save_job(&self, body: Value) -> Result<Job, String> {
        if self.draining.load(Ordering::SeqCst) {
            return Err("后台正在退出，无法保存任务".into());
        }
        let old = body["id"]
            .as_str()
            .filter(|s| !s.is_empty())
            .map(|s| self.get(s))
            .transpose()?;
        if let Some(j) = &old
            && (j.active() || j.terminal() || j.status == "review" || j.submitted_at.is_some())
        {
            return Err("此记录已经执行或提交结果不确定，不能重新编辑为待发送任务".into());
        }
        let expected = body["expectedUpdatedAt"].as_u64();
        let mut job: Job = serde_json::from_value(body).map_err(|e| e.to_string())?;
        job.validate()?;
        let runtime = self.runtime_info()?;
        let client = Client::connect(&runtime.path).map_err(|e| e.message)?;
        let account = client
            .request("account/read", json!({}))
            .map_err(|e| e.message)?;
        job.account_key = account_key(&account)?;
        job.runtime_path = runtime.path;
        if job.thread_mode == "existing" {
            let result = client
                .request(
                    "thread/read",
                    json!({"threadId":job.thread_id,"includeTurns":false}),
                )
                .map_err(|e| e.message)?;
            if result["thread"]["cwd"]
                .as_str()
                .is_some_and(|p| !same_path(p, &job.cwd))
            {
                return Err("已有对话的项目目录与所选目录不同，请核对后使用对话原目录".into());
            }
            job.model = None;
            job.effort = None;
            job.revision = if job.stale_policy != "send" {
                Some(revision(&client, job.thread_id.as_ref().unwrap()).map_err(|e| e.message)?)
            } else {
                None
            };
        } else {
            job.thread_id = None;
            job.revision = None;
            validate_model(&client, &job).map_err(|e| e.message)?;
        }
        job.id = old.as_ref().map(|j| j.id.clone()).unwrap_or_else(id);
        job.created_at = old.as_ref().map(|j| j.created_at).unwrap_or_else(now);
        job.status = "scheduled".into();
        job.reason = String::new();
        job.updated_at = now();
        job.next_check_at = if job.send_mode == "available" {
            now()
        } else {
            job.send_at
        };
        job.submitted_at = None;
        job.accepted_at = None;
        job.turn_id = None;
        job.finished_at = None;
        job.output.clear();
        job.attempts = 0;
        job.check_failures = 0;
        job.client_message_id = id();
        job.confirmation_id = None;
        job.pending_revision = None;
        self.store.lock().unwrap().change(|db| {
            if let Some(index) = db.jobs.iter().position(|j| j.id == job.id) {
                if Some(db.jobs[index].updated_at) != expected || db.jobs[index].active() {
                    return Err("任务已变化，请刷新后再保存".into());
                }
                db.jobs[index] = job.clone();
            } else {
                db.jobs.push(job.clone());
            }
            Ok(())
        })?;
        Ok(job)
    }
    pub fn action(&self, job_id: &str, body: &Value) -> Result<(), String> {
        let action = body["action"].as_str().unwrap_or("");
        if action == "interrupt" {
            let active = self.active.lock().unwrap();
            let control = active.get(job_id).ok_or("任务未在本实例运行")?;
            control.store(true, Ordering::SeqCst);
            return Ok(());
        }
        self.store.lock().unwrap().change(|db| {
            let j = db
                .jobs
                .iter_mut()
                .find(|j| j.id == job_id)
                .ok_or("任务不存在")?;
            if j.active() {
                return Err("任务正在执行，操作已拒绝".into());
            }
            match action {
                "cancel" => {
                    if j.status == "review" {
                        return Err("结果待核对记录不能作为未发送任务取消".into());
                    }
                    if j.terminal() {
                        return Err("任务已经结束".into());
                    }
                    j.status = "cancelled".into();
                    j.finished_at = Some(now());
                }
                "now" => {
                    if j.status != "scheduled" {
                        return Err("仅待执行任务可以立即触发".into());
                    }
                    j.send_at = now();
                    j.next_check_at = now();
                }
                "confirm" | "skip" => {
                    if j.status != "confirmation"
                        || body["confirmationId"].as_str() != j.confirmation_id.as_deref()
                    {
                        return Err("确认已过期，请刷新".into());
                    }
                    if action == "skip" {
                        j.status = "skipped".into();
                        j.finished_at = Some(now());
                    } else {
                        j.status = "scheduled".into();
                        j.revision = j.pending_revision.take().or_else(|| j.revision.clone());
                        j.send_at = now();
                        j.next_check_at = now();
                    }
                    j.confirmation_id = None;
                }
                _ => return Err("不支持的任务操作".into()),
            }
            j.updated_at = now();
            Ok(())
        })
    }
    pub fn decide(&self, approval_id: &str, allow: bool) -> Result<(), String> {
        let mut approvals = self.approvals.lock().unwrap();
        let a = approvals
            .iter_mut()
            .find(|a| a.id == approval_id)
            .ok_or("审批已结束")?;
        if now() >= a.expires_at || a.decision.is_some() {
            return Err("审批已过期或已处理".into());
        }
        a.decision = Some(allow);
        Ok(())
    }
    pub fn request_exit(&self, interrupt: bool) {
        self.draining.store(true, Ordering::SeqCst);
        if interrupt {
            for control in self.active.lock().unwrap().values() {
                control.store(true, Ordering::SeqCst);
            }
        }
        if self.active.lock().unwrap().is_empty() {
            self.stopping.store(true, Ordering::SeqCst);
        }
    }
    pub fn shutdown(&self, interrupt: bool) -> Result<(), String> {
        self.request_exit(interrupt);
        let until = Instant::now() + Duration::from_secs(45);
        while !self.active.lock().unwrap().is_empty() {
            if Instant::now() > until {
                return Err("仍有任务退出中，请稍后重试".into());
            }
            thread::sleep(Duration::from_millis(100));
        }
        Ok(())
    }
}

fn same_path(a: &str, b: &str) -> bool {
    std::fs::canonicalize(a)
        .ok()
        .zip(std::fs::canonicalize(b).ok())
        .is_some_and(|(a, b)| a == b)
}
pub fn account_key(value: &Value) -> Result<String, String> {
    let account = &value["account"];
    if account.is_null() {
        return Err("Codex 尚未登录，请在终端运行 codex login 后重新检测".into());
    }
    let identity = value["workspaceRouting"]["chatgptAccountId"]
        .as_str()
        .or_else(|| value["userId"].as_str())
        .or_else(|| account["email"].as_str());
    let Some(identity) = identity else {
        return Err("当前账户未提供可绑定身份。Codey 首版需要提供身份标识的 ChatGPT 登录；请运行 codex login".into());
    };
    Ok(hash(&json!([
        account["type"],
        identity,
        account["workspaceName"],
        value["workspaceRouting"]["backendOrigin"]
    ])))
}
pub fn hash(value: &Value) -> String {
    format!("{:x}", Sha256::digest(serde_json::to_vec(value).unwrap()))
}

fn validate_model(client: &Client, job: &Job) -> Result<(), RpcError> {
    let Some(model) = &job.model else {
        return Ok(());
    };
    let mut cursor = Value::Null;
    for _ in 0..20 {
        let result = client.request(
            "model/list",
            json!({"limit":100,"includeHidden":false,"cursor":cursor}),
        )?;
        if let Some(entry) = result["data"]
            .as_array()
            .and_then(|a| a.iter().find(|v| v["model"].as_str() == Some(model)))
        {
            if let Some(effort) = &job.effort
                && !entry["supportedReasoningEfforts"]
                    .as_array()
                    .is_some_and(|a| {
                        a.iter()
                            .any(|v| v["reasoningEffort"].as_str() == Some(effort))
                    })
            {
                return Err("所选推理强度不可用".to_string().into());
            }
            return Ok(());
        }
        cursor = result["nextCursor"].clone();
        if cursor.is_null() {
            break;
        }
    }
    Err("所选模型不可用，请重新选择".to_string().into())
}
pub fn revision(client: &Client, thread_id: &str) -> Result<String, RpcError> {
    let result = client.request(
        "thread/turns/list",
        json!({"threadId":thread_id,"limit":1,"sortDirection":"desc","itemsView":"full"}),
    )?;
    let turns = result["data"]
        .as_array()
        .ok_or_else(|| RpcError::from("无法读取完整对话历史".to_string()))?;
    let Some(turn) = turns.first() else {
        return Ok(hash(&json!({"turn":null,"items":[]})));
    };
    if turn["itemsView"].as_str().is_some_and(|s| s != "full") {
        return Err("对话历史不完整".to_string().into());
    }
    let items = turn["items"]
        .as_array()
        .ok_or_else(|| RpcError::from("对话历史缺少消息".to_string()))?;
    let mut messages = Vec::new();
    for item in items {
        match item["type"].as_str() {
            Some("userMessage") => {
                if !item["content"].is_array() || !item["id"].is_string() {
                    return Err("用户消息身份不完整".to_string().into());
                }
                messages
                    .push(json!({"id":item["id"],"type":"userMessage","content":item["content"]}));
            }
            Some("agentMessage") => {
                if !item["text"].is_string() || !item["id"].is_string() {
                    return Err("助手消息身份不完整".to_string().into());
                }
                messages.push(json!({"id":item["id"],"type":"agentMessage","text":item["text"]}));
            }
            _ => {}
        }
    }
    Ok(hash(&json!({"turn":turn["id"],"items":messages})))
}

pub fn availability(value: &Value, model: Option<&str>, time: u64) -> (bool, u64, String) {
    let mut buckets = Vec::new();
    if value["rateLimits"].is_object() {
        buckets.push(&value["rateLimits"])
    }
    if let Some(map) = value["rateLimitsByLimitId"].as_object() {
        for (key, bucket) in map {
            if key == "codex"
                || model.is_some_and(|m| key == m || bucket["normalModelSlug"].as_str() == Some(m))
            {
                buckets.push(bucket)
            }
        }
    }
    let mut blocked = false;
    let mut reset = 0;
    for b in buckets {
        if !b["rateLimitReachedType"].is_null()
            || b["spendControlReached"] == true
            || b["individualLimit"]["remainingPercent"]
                .as_f64()
                .is_some_and(|p| p <= 0.0)
        {
            blocked = true;
        }
        for key in ["primary", "secondary"] {
            let w = &b[key];
            if w["usedPercent"].as_f64().is_some_and(|p| p >= 100.0) {
                blocked = true;
                reset = reset.max(w["resetsAt"].as_u64().unwrap_or(0) * 1000);
            }
        }
    }
    if !blocked && value["ordinaryUsageAllowed"] == true {
        return (true, time, String::new());
    }
    let interval = if reset > time && reset - time <= 1800000 {
        120000
    } else {
        900000
    };
    let next = if reset > time {
        (time + interval).min(reset + 60000)
    } else {
        time + interval
    };
    (
        false,
        next,
        if blocked {
            "等待额度恢复"
        } else {
            "Codex 尚未确认可用性"
        }
        .into(),
    )
}

pub fn start(app: Arc<App>) {
    thread::spawn(move || {
        let mut last = now();
        let mut last_prune = 0;
        while !app.stopping.load(Ordering::SeqCst) {
            let time = now();
            if time.saturating_sub(last) > 5000 {
                let result = app.store.lock().unwrap().change(|db| {
                    for j in &mut db.jobs {
                        if j.status == "scheduled"
                            && j.send_mode == "time"
                            && j.due() < time
                            && j.due() > last
                        {
                            match j.missed_policy.as_str() {
                                "send" => {}
                                "skip" => {
                                    j.status = "skipped".into();
                                    j.finished_at = Some(time)
                                }
                                _ => {
                                    j.status = "confirmation".into();
                                    j.reason = "休眠或时钟变化后错过执行时间".into();
                                    j.confirmation_id = Some(id());
                                }
                            }
                        }
                    }
                    Ok(())
                });
                if let Err(e) = result {
                    *app.last_error.lock().unwrap() = e;
                    app.draining.store(true, Ordering::SeqCst);
                }
            }
            last = time;
            if app.draining.load(Ordering::SeqCst) {
                if app.active.lock().unwrap().is_empty() {
                    app.stopping.store(true, Ordering::SeqCst);
                }
                thread::sleep(Duration::from_millis(250));
                continue;
            }
            if time - last_prune > 86400000 {
                let result = app.store.lock().unwrap().change(|db| {
                    let days = db.settings.history_retention_days;
                    if days > 0 {
                        let cutoff = time.saturating_sub(u64::from(days) * 86400000);
                        db.jobs.retain(|j| {
                            !(j.terminal() && j.finished_at.is_some_and(|v| v < cutoff))
                        });
                    }
                    Ok(())
                });
                if let Err(e) = result {
                    *app.last_error.lock().unwrap() = e;
                    app.draining.store(true, Ordering::SeqCst);
                }
                last_prune = time;
            }
            // First release runs one job at a time, preventing project and thread collisions.
            if app.active.lock().unwrap().is_empty() {
                let due = app
                    .store
                    .lock()
                    .unwrap()
                    .db
                    .jobs
                    .iter()
                    .filter(|j| j.status == "scheduled" && j.due() <= time)
                    .min_by_key(|j| j.due())
                    .cloned();
                if let Some(job) = due {
                    let control = Arc::new(AtomicBool::new(false));
                    app.active
                        .lock()
                        .unwrap()
                        .insert(job.id.clone(), control.clone());
                    let a = app.clone();
                    thread::spawn(move || {
                        if let Err(e) = execute(&a, &job.id, &control) {
                            let current = a.get(&job.id).ok();
                            if let Some(j) = current
                                && !j.terminal()
                                && !["confirmation", "review", "scheduled", "paused"]
                                    .contains(&j.status.as_str())
                            {
                                let submitted = j.submitted_at.is_some();
                                let _ = a.update(&job.id, |j| {
                                    j.status = if submitted { "review" } else { "paused" }.into();
                                    j.reason = e.message;
                                });
                            }
                        }
                        a.approvals.lock().unwrap().retain(|p| p.job_id != job.id);
                        a.active.lock().unwrap().remove(&job.id);
                    });
                }
            }
            thread::sleep(Duration::from_millis(500));
        }
    });
}

fn persist(app: &App, job_id: &str, f: impl FnOnce(&mut Job)) -> Result<(), RpcError> {
    app.update(job_id, f).map_err(Into::into)
}
fn cancelled(control: &AtomicBool) -> Result<(), RpcError> {
    if control.load(Ordering::SeqCst) {
        Err("执行已被用户中断".to_string().into())
    } else {
        Ok(())
    }
}
fn execute(app: &App, job_id: &str, control: &AtomicBool) -> Result<(), RpcError> {
    persist(app, job_id, |j| {
        j.status = "checking".into();
        j.reason.clear();
    })?;
    let mut job = app.get(job_id).map_err(RpcError::from)?;
    cancelled(control)?;
    if job.send_mode == "available" && job.expires_at.is_some_and(|e| e <= now()) {
        persist(app, job_id, |j| {
            j.status = "expired".into();
            j.finished_at = Some(now());
        })?;
        return Ok(());
    }
    job.validate().map_err(RpcError::from)?;
    let runtime = app.runtime_info().map_err(RpcError::from)?;
    if runtime.path != job.runtime_path {
        return Err("Codex 运行时路径已变化，请编辑保存任务重新绑定"
            .to_string()
            .into());
    }
    let client = Client::connect(&runtime.path)?;
    let key = account_key(&client.request("account/read", json!({}))?).map_err(RpcError::from)?;
    if key != job.account_key {
        return Err("账户已变化，请核对后编辑保存任务".to_string().into());
    }
    let mut stored = Value::Null;
    if let Some(thread_id) = &job.thread_id {
        stored = client.request(
            "thread/read",
            json!({"threadId":thread_id,"includeTurns":false}),
        )?;
        if stored["thread"]["status"]["type"] == "active" {
            return busy(app, job_id);
        }
    }
    let mut resolved = job
        .model
        .clone()
        .or_else(|| stored["thread"]["model"].as_str().map(String::from));
    if job.send_mode == "available" {
        if resolved.is_none() {
            let config =
                client.request("config/read", json!({"cwd":job.cwd,"includeLayers":false}))?;
            resolved = config["config"]["model"].as_str().map(String::from);
        }
        if resolved.is_none() {
            let models =
                client.request("model/list", json!({"limit":100,"includeHidden":false}))?;
            resolved = models["data"]
                .as_array()
                .and_then(|a| a.iter().find(|v| v["isDefault"] == true))
                .and_then(|v| v["model"].as_str())
                .map(String::from);
        }
        let result = match client.request("account/rateLimits/read", json!({})) {
            Ok(r) => r,
            Err(e) => {
                persist(app, job_id, |j| {
                    j.check_failures += 1;
                    j.status = "scheduled".into();
                    j.next_check_at = now()
                        + [60000, 120000, 300000, 900000]
                            [(j.check_failures.saturating_sub(1) as usize).min(3)];
                    j.reason = format!("可用性检查失败：{e}");
                })?;
                return Ok(());
            }
        };
        if account_key(&client.request("account/read", json!({}))?).map_err(RpcError::from)?
            != job.account_key
        {
            return Err("检查期间账户发生变化".to_string().into());
        }
        let (ready, next, reason) = availability(&result, resolved.as_deref(), now());
        if !ready {
            persist(app, job_id, |j| {
                j.status = "scheduled".into();
                j.next_check_at = next.min(j.expires_at.unwrap_or(u64::MAX));
                j.reason = reason;
                j.check_failures = 0;
            })?;
            return Ok(());
        }
    }
    cancelled(control)?;
    validate_model(&client, &job)?;
    if let Some(thread_id) = &job.thread_id
        && job.stale_policy != "send"
    {
        let current = revision(&client, thread_id)?;
        if job.revision.as_deref() != Some(&current) {
            persist(app, job_id, |j| {
                j.pending_revision = Some(current);
                j.reason = "保存任务后对话已经变化".into();
                if j.stale_policy == "skip" {
                    j.status = "skipped".into();
                    j.finished_at = Some(now());
                } else {
                    j.status = "confirmation".into();
                    j.confirmation_id = Some(id());
                }
            })?;
            return Ok(());
        }
    }
    let mut params = json!({"cwd":job.cwd,"approvalPolicy":job.approval_policy,"sandbox":job.sandbox,"config":{"sandbox_workspace_write.network_access":job.network_access}});
    let preparation = if let Some(tid) = &job.thread_id {
        params["threadId"] = json!(tid);
        params["excludeTurns"] = json!(true);
        if let Some(model) = stored["thread"]["model"].as_str() {
            params["model"] = json!(model)
        }
        if let Some(effort) = stored["thread"]["reasoningEffort"].as_str() {
            params["config"]["model_reasoning_effort"] = json!(effort)
        }
        client.request("thread/resume", params)
    } else {
        params["serviceName"] = json!("codey");
        params["threadSource"] = json!("codey");
        if let Some(model) = &job.model {
            params["model"] = json!(model)
        }
        client.request("thread/start", params)
    };
    let prepared = match preparation {
        Ok(p) => p,
        Err(e) => {
            if is_busy(&e) {
                return busy(app, job_id);
            }
            return Err(e);
        }
    };
    if prepared["thread"]["status"]["type"] == "active"
        || prepared["thread"]["canAcceptDirectInput"] == false
    {
        return busy(app, job_id);
    }
    let thread_id = prepared["thread"]["id"]
        .as_str()
        .ok_or_else(|| RpcError::from("Codex 未返回对话 ID".to_string()))?
        .to_string();
    if job.thread_id.is_none() {
        persist(app, job_id, |j| j.thread_id = Some(thread_id.clone()))?;
        job.thread_id = Some(thread_id.clone());
    }
    cancelled(control)?;
    if account_key(&client.request("account/read", json!({}))?).map_err(RpcError::from)?
        != job.account_key
    {
        return Err("提交前账户发生变化".to_string().into());
    }
    if job.send_mode == "available" && job.expires_at.is_some_and(|e| e <= now()) {
        persist(app, job_id, |j| {
            j.status = "expired".into();
            j.finished_at = Some(now());
        })?;
        return Ok(());
    }
    if job.attempts >= 5 {
        return Err("已达到提交尝试上限，请人工检查".to_string().into());
    }
    let sandbox = match job.sandbox.as_str() {
        "read-only" => json!({"type":"readOnly","networkAccess":job.network_access}),
        "workspace-write" => {
            json!({"type":"workspaceWrite","writableRoots":[job.cwd],"networkAccess":job.network_access,"excludeTmpdirEnvVar":false,"excludeSlashTmp":false})
        }
        _ => json!({"type":"dangerFullAccess"}),
    };
    let mut turn = json!({"threadId":thread_id,"clientUserMessageId":job.client_message_id,"input":[{"type":"text","text":job.prompt,"text_elements":[]}],"cwd":job.cwd,"approvalPolicy":job.approval_policy,"sandboxPolicy":sandbox});
    if job.thread_mode == "new" {
        if let Some(m) = &job.model {
            turn["model"] = json!(m)
        }
        if let Some(e) = &job.effort {
            turn["effort"] = json!(e)
        }
    }
    // Commit the submission boundary before writing any prompt bytes to Codex.
    persist(app, job_id, |j| {
        j.status = "sending".into();
        j.submitted_at = Some(now());
        j.attempts += 1;
    })?;
    let started = match client.request("turn/start", turn) {
        Ok(r) => r,
        Err(e) => {
            if matches!(e.code, Some(-32602..=-32600)) || is_busy(&e) {
                persist(app, job_id, |j| {
                    j.submitted_at = None;
                    j.status = "paused".into();
                    j.reason = format!("Codex 明确拒绝了提交：{e}");
                })?;
            }
            return Err(e);
        }
    };
    let turn_id = started["turn"]["id"]
        .as_str()
        .ok_or_else(|| RpcError::from("Codex 未返回执行轮次 ID，请核对对话".to_string()))?
        .to_string();
    persist(app, job_id, |j| {
        j.turn_id = Some(turn_id.clone());
        j.accepted_at = Some(now());
        j.status = "running".into();
    })?;
    let mut buffers = String::new();
    let mut last_flush = Instant::now();
    loop {
        if control.load(Ordering::SeqCst) {
            let result = client.request_timeout(
                "turn/interrupt",
                json!({"threadId":thread_id,"turnId":turn_id}),
                Duration::from_secs(5),
            );
            persist(app, job_id, |j| {
                j.status = "review".into();
                j.reason = if result.is_ok() {
                    "已请求中断，请刷新历史核对最终状态"
                } else {
                    "中断结果不确定，请核对对话"
                }
                .into();
            })?;
            return Ok(());
        }
        match client.events.recv_timeout(Duration::from_millis(200)) {
            Ok(event) => {
                let method = event["method"].as_str().unwrap_or("");
                if event.get("id").is_some() {
                    if [
                        "item/commandExecution/requestApproval",
                        "item/fileChange/requestApproval",
                    ]
                    .contains(&method)
                    {
                        if !handle_approval(app, &client, job_id, &event, control)? {
                            let _ = client.request_timeout(
                                "turn/interrupt",
                                json!({"threadId":thread_id,"turnId":turn_id}),
                                Duration::from_secs(5),
                            );
                            persist(app, job_id, |j| {
                                j.status = "review".into();
                                j.reason = "审批被拒绝、超时或执行被中断；请刷新历史核对".into();
                            })?;
                            return Ok(());
                        }
                    } else {
                        client.unsupported(event["id"].clone())?;
                        let _ = client.request_timeout(
                            "turn/interrupt",
                            json!({"threadId":thread_id,"turnId":turn_id}),
                            Duration::from_secs(5),
                        );
                        return Err(format!("此 Codex 交互尚未支持：{method}").into());
                    }
                    continue;
                }
                if method == "codey/disconnected" {
                    return Err("Codex 连接中断，请核对执行结果".to_string().into());
                }
                let p = &event["params"];
                if p["threadId"].as_str().is_some_and(|t| t != thread_id) {
                    continue;
                }
                if method == "turn/completed" && p["turn"]["id"].as_str() == Some(&turn_id) {
                    let status = match p["turn"]["status"].as_str() {
                        Some("completed") => "completed",
                        Some("interrupted") => "interrupted",
                        Some("failed") => "failed",
                        _ => "review",
                    };
                    // Release the app-server process and writer lease before reporting completion.
                    drop(client);
                    persist(app, job_id, |j| {
                        append_output(j, &buffers);
                        j.status = status.into();
                        j.finished_at = if status == "review" {
                            None
                        } else {
                            Some(now())
                        };
                        j.reason = if p["turn"]["error"].is_null() {
                            String::new()
                        } else {
                            p["turn"]["error"].to_string()
                        };
                    })?;
                    return Ok(());
                }
                if let Some(delta) = p["delta"].as_str()
                    && (method == "item/agentMessage/delta"
                        || method == "item/commandExecution/outputDelta")
                {
                    buffers.push_str(delta);
                }
                if buffers.len() > 65536 || last_flush.elapsed() > Duration::from_secs(1) {
                    if !buffers.is_empty() {
                        persist(app, job_id, |j| append_output(j, &buffers))?;
                        buffers.clear();
                    }
                    last_flush = Instant::now();
                }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(_) => return Err("Codex 事件通道已关闭".to_string().into()),
        }
    }
}
fn append_output(job: &mut Job, text: &str) {
    job.output.push_str(text);
    if job.output.len() > 262144 {
        let mut n = job.output.len() - 262144;
        while !job.output.is_char_boundary(n) {
            n += 1
        }
        job.output.drain(..n);
    }
}
fn is_busy(e: &RpcError) -> bool {
    let t = e.message.to_lowercase();
    [
        "already running",
        "thread busy",
        "active writer",
        "writer lease",
        "in progress",
    ]
    .iter()
    .any(|s| t.contains(s))
}
fn busy(app: &App, job_id: &str) -> Result<(), RpcError> {
    persist(app, job_id, |j| {
        j.status = "scheduled".into();
        j.reason = "对话正被使用，稍后重试".into();
        j.next_check_at = now() + 30000;
    })
}

fn handle_approval(
    app: &App,
    client: &Client,
    job_id: &str,
    event: &Value,
    control: &AtomicBool,
) -> Result<bool, RpcError> {
    let timeout = app.store.lock().unwrap().db.settings.approval_timeout_secs;
    let approval = Approval {
        id: id(),
        job_id: job_id.into(),
        method: event["method"].as_str().unwrap_or("").into(),
        detail: event["params"].clone(),
        created_at: now(),
        expires_at: now() + timeout * 1000,
        decision: None,
    };
    let approval_id = approval.id.clone();
    let expires = approval.expires_at;
    app.approvals.lock().unwrap().push(approval);
    persist(app, job_id, |j| {
        j.status = "approval".into();
        j.reason = "等待用户审批".into();
    })?;
    thread::spawn(|| {
        let _ = notify_rust::Notification::new()
            .summary("Codey · 有任务等待审批")
            .body("请通过系统托盘打开 Codey，查看请求并决定是否允许。等待超时后本次执行会中断。")
            .appname("Codey")
            .show();
    });
    let decision = loop {
        if control.load(Ordering::SeqCst) || now() >= expires {
            break false;
        }
        if let Some(value) = app
            .approvals
            .lock()
            .unwrap()
            .iter()
            .find(|a| a.id == approval_id)
            .and_then(|a| a.decision)
        {
            break value;
        }
        thread::sleep(Duration::from_millis(100));
    };
    app.approvals
        .lock()
        .unwrap()
        .retain(|a| a.id != approval_id);
    client.respond(
        event["id"].clone(),
        json!({"decision":if decision{"accept"}else{"decline"}}),
    )?;
    persist(app, job_id, |j| {
        j.status = "running".into();
        j.reason.clear();
    })?;
    Ok(decision)
}

pub fn list_threads(app: &App, cursor: Value) -> Result<Value, String> {
    let c = app.client()?;
    c.request("thread/list",json!({"limit":50,"cursor":cursor,"sortKey":"recency_at","sourceKinds":["cli","vscode","appServer","exec"],"archived":false,"currentAccountOnly":true})).map_err(|e|e.message)
}
pub fn refresh_history(app: &App) -> Result<usize, String> {
    let jobs: Vec<Job> = app
        .store
        .lock()
        .unwrap()
        .db
        .jobs
        .iter()
        .filter(|j| j.status == "review" && !j.active() && j.thread_id.is_some())
        .take(100)
        .cloned()
        .collect();
    if jobs.is_empty() {
        return Ok(0);
    }
    let c = app.client()?;
    let key = account_key(
        &c.request("account/read", json!({}))
            .map_err(|e| e.message)?,
    )?;
    let mut changed = 0;
    for j in jobs {
        if j.account_key != key {
            continue;
        }
        let mut cursor = Value::Null;
        let mut seen = HashSet::new();
        let mut found = None;
        for _ in 0..10 {
            let result=c.request("thread/turns/list",json!({"threadId":j.thread_id,"limit":50,"sortDirection":"desc","itemsView":"full","cursor":cursor})).map_err(|e|e.message)?;
            if let Some(turns) = result["data"].as_array() {
                found = turns
                    .iter()
                    .find(|t| {
                        j.turn_id.as_deref() == t["id"].as_str()
                            || t["items"].as_array().is_some_and(|a| {
                                a.iter().any(|i| {
                                    i["type"] == "userMessage"
                                        && (i["clientId"].as_str() == Some(&j.client_message_id)
                                            || i["clientUserMessageId"].as_str()
                                                == Some(&j.client_message_id))
                                })
                            })
                    })
                    .cloned();
            }
            if found.is_some() {
                break;
            }
            cursor = result["nextCursor"].clone();
            if cursor.is_null() || !seen.insert(cursor.to_string()) {
                break;
            }
        }
        if let Some(t) = found {
            let status = t["status"].as_str().unwrap_or("");
            if ["completed", "failed", "interrupted"].contains(&status)
                && t["completedAt"].as_f64().is_some()
            {
                app.update(&j.id, |job| {
                    job.status = status.into();
                    job.turn_id = t["id"].as_str().map(String::from);
                    job.finished_at = t["completedAt"].as_f64().map(|v| (v * 1000.0) as u64);
                    job.reason = if t["error"].is_null() {
                        String::new()
                    } else {
                        t["error"].to_string()
                    };
                })?;
                changed += 1;
            }
        }
    }
    Ok(changed)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn past_reset_does_not_prove_recovery() {
        let v = json!({"ordinaryUsageAllowed":false,"rateLimits":{"primary":{"usedPercent":100,"resetsAt":1}}});
        let (r, n, _) = availability(&v, None, 10000);
        assert!(!r);
        assert!(n > 10000);
    }
    #[test]
    fn model_bucket_can_block_base_readiness() {
        let v = json!({"ordinaryUsageAllowed":true,"rateLimits":{},"rateLimitsByLimitId":{"model-a":{"primary":{"usedPercent":100,"resetsAt":500}}}});
        assert!(!availability(&v, Some("model-a"), 1000).0);
        assert!(availability(&v, Some("model-b"), 1000).0);
    }
    #[test]
    fn unknown_readiness_is_not_ready() {
        assert!(!availability(&json!({"rateLimits":{}}), None, 0).0);
    }
    #[test]
    fn account_plan_does_not_change_binding() {
        let a = json!({"userId":"id-a","account":{"type":"chatgpt","email":"a","planType":"plus"}});
        let mut b = a.clone();
        b["account"]["planType"] = json!("pro");
        assert_eq!(account_key(&a).unwrap(), account_key(&b).unwrap());
        b["userId"] = json!("id-b");
        assert_ne!(account_key(&a).unwrap(), account_key(&b).unwrap());
    }
}
