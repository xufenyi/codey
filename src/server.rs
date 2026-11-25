use crate::{
    engine::{self, App},
    model::{Settings, id},
    rpc::Client,
    runtime,
    store::atomic_write,
};
use serde_json::{Value, json};
use std::{
    io::Read,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    thread,
    time::Duration,
};
use tiny_http::{Header, Request, Response, Server, StatusCode};

pub fn start(app: Arc<App>) -> Result<String, String> {
    let server = Server::http("127.0.0.1:0").map_err(|e| e.to_string())?;
    let address = server.server_addr().to_ip().ok_or("无法绑定本机端口")?;
    let host = address.to_string();
    let origin = format!("http://{host}");
    let token = format!("{}{}", id(), id());
    let url = format!("{origin}/#token={token}");
    let dir = app.store.lock().unwrap().dir.clone();
    atomic_write(
        &dir.join("session.json"),
        serde_json::to_string(&json!({"url":url}))
            .unwrap()
            .as_bytes(),
    )?;
    thread::spawn(move || {
        let count = Arc::new(AtomicUsize::new(0));
        while !app.stopping.load(Ordering::SeqCst) {
            let req = match server.recv_timeout(Duration::from_millis(200)) {
                Ok(Some(r)) => r,
                _ => continue,
            };
            let header = |name: &'static str| {
                req.headers()
                    .iter()
                    .find(|h| h.field.equiv(name))
                    .map(|h| h.value.as_str().to_string())
            };
            let api = req.url().starts_with("/api/");
            if header("Host").as_deref() != Some(&host)
                || header("Origin").is_some_and(|v| v != origin)
                || header("Sec-Fetch-Site").as_deref() == Some("cross-site")
            {
                respond(
                    req,
                    403,
                    "application/json",
                    b"{\"error\":\"Request origin rejected\"}".to_vec(),
                );
                continue;
            }
            if api && header("Authorization").as_deref() != Some(&format!("Bearer {token}")) {
                respond(
                    req,
                    401,
                    "application/json",
                    b"{\"error\":\"Please open Codey from its tray icon\"}".to_vec(),
                );
                continue;
            }
            if count.load(Ordering::SeqCst) >= 24 {
                respond(
                    req,
                    503,
                    "application/json",
                    b"{\"error\":\"Too many requests\"}".to_vec(),
                );
                continue;
            }
            count.fetch_add(1, Ordering::SeqCst);
            let a = app.clone();
            let n = count.clone();
            thread::spawn(move || {
                handle(req, &a);
                n.fetch_sub(1, Ordering::SeqCst);
            });
        }
    });
    Ok(url)
}
fn respond(req: Request, status: u16, kind: &str, bytes: Vec<u8>) {
    let mut response = Response::from_data(bytes).with_status_code(StatusCode(status));
    for (name, value) in [
        ("Content-Type", kind),
        ("Cache-Control", "no-store"),
        ("X-Content-Type-Options", "nosniff"),
        ("Referrer-Policy", "no-referrer"),
        (
            "Content-Security-Policy",
            "default-src 'self'; script-src 'self'; style-src 'self'; img-src 'self' data:; connect-src 'self'; object-src 'none'; base-uri 'none'; frame-ancestors 'none'; form-action 'none'",
        ),
    ] {
        response.add_header(Header::from_bytes(name, value).unwrap());
    }
    let _ = req.respond(response);
}
fn handle(mut req: Request, app: &Arc<App>) {
    let path = req.url().split('?').next().unwrap_or("/").to_string();
    let method = req.method().as_str().to_string();
    if !path.starts_with("/api/") {
        if method != "GET" {
            respond(req, 405, "text/plain", b"Method not allowed".to_vec());
            return;
        }
        let asset = match path.as_str() {
            "/" | "/index.html" => Some((
                "text/html; charset=utf-8",
                include_bytes!("../web/index.html").as_slice(),
            )),
            "/app.js" => Some((
                "text/javascript; charset=utf-8",
                include_bytes!("../web/app.js").as_slice(),
            )),
            "/style.css" => Some((
                "text/css; charset=utf-8",
                include_bytes!("../web/style.css").as_slice(),
            )),
            "/favicon.svg" => Some((
                "image/svg+xml",
                include_bytes!("../web/favicon.svg").as_slice(),
            )),
            _ => None,
        };
        match asset {
            Some((kind, bytes)) => respond(req, 200, kind, bytes.to_vec()),
            None => respond(req, 404, "text/plain", b"Not found".to_vec()),
        }
        return;
    }
    let result = (|| {
        let body = if method == "POST" {
            if req.body_length().is_some_and(|v| v > 1024 * 1024) {
                return Err("请求体超过 1 MiB".into());
            }
            if !req.headers().iter().any(|h| {
                h.field.equiv("Content-Type") && h.value.as_str().starts_with("application/json")
            }) {
                return Err("请求必须使用 application/json".into());
            }
            let mut bytes = Vec::new();
            req.as_reader()
                .take(1024 * 1024 + 1)
                .read_to_end(&mut bytes)
                .map_err(|e| e.to_string())?;
            if bytes.len() > 1024 * 1024 {
                return Err("请求体超过 1 MiB".into());
            }
            serde_json::from_slice(&bytes).map_err(|e| format!("JSON 无效：{e}"))?
        } else {
            Value::Null
        };
        route(app, &method, &path, body)
    })();
    match result {
        Ok(value) => respond(
            req,
            200,
            "application/json; charset=utf-8",
            serde_json::to_vec(&value).unwrap(),
        ),
        Err(error) => respond(
            req,
            400,
            "application/json; charset=utf-8",
            serde_json::to_vec(&json!({"error":error})).unwrap(),
        ),
    }
}
fn route(app: &Arc<App>, method: &str, path: &str, body: Value) -> Result<Value, String> {
    match (method, path) {
        ("GET", "/api/state") => Ok(app.snapshot()),
        ("POST", "/api/runtime") => {
            let path = body["path"].as_str().unwrap_or("");
            let info = runtime::discover(Some(path))?;
            let client = Client::connect(&info.path).map_err(|e| e.message)?;
            let account = client
                .request("account/read", json!({}))
                .map_err(|e| e.message)?;
            let models = client
                .request("model/list", json!({"limit":100,"includeHidden":false}))
                .map_err(|e| e.message)?;
            *app.runtime.lock().unwrap() = Some(info.clone());
            Ok(
                json!({"runtime":info,"account":account,"models":models,"bindingError":engine::account_key(&account).err()}),
            )
        }
        ("POST", "/api/settings") => {
            let settings: Settings = serde_json::from_value(body).map_err(|e| e.to_string())?;
            if !(30..=86400).contains(&settings.approval_timeout_secs)
                || settings.history_retention_days > 3650
            {
                return Err("审批等待范围为 30–86400 秒，历史保留范围为 0–3650 天".into());
            }
            let info = runtime::discover(Some(&settings.codex_path))?;
            let c = Client::connect(&info.path).map_err(|e| e.message)?;
            c.request("account/read", json!({}))
                .map_err(|e| e.message)?;
            app.store.lock().unwrap().change(|db| {
                db.settings = settings;
                Ok(())
            })?;
            *app.runtime.lock().unwrap() = Some(info);
            Ok(json!({"ok":true}))
        }
        ("POST", "/api/threads") => engine::list_threads(app, body["cursor"].clone()),
        ("POST", "/api/jobs") => Ok(json!({"job":app.save_job(body)?})),
        ("POST", "/api/history/refresh") => Ok(json!({"updated":engine::refresh_history(app)?})),
        ("POST", "/api/history/clear") => {
            let count = app.store.lock().unwrap().change(|db| {
                let before = db.jobs.len();
                db.jobs.retain(|j| !j.terminal());
                Ok(before - db.jobs.len())
            })?;
            Ok(json!({"removed":count}))
        }
        ("POST", "/api/shutdown") => {
            let active = app.active.lock().unwrap().len();
            let mode = body["mode"].as_str().unwrap_or("");
            if active > 0 && !["wait", "interrupt"].contains(&mode) {
                return Err("仍有任务运行，请选择等待完成或中断后退出".into());
            }
            app.request_exit(mode == "interrupt");
            Ok(json!({"ok":true}))
        }
        ("POST", "/api/browse") => browse(&body),
        _ => {
            if method == "POST" {
                if let Some(job_id) = path.strip_prefix("/api/jobs/") {
                    app.action(job_id, &body)?;
                    return Ok(json!({"ok":true}));
                }
                if let Some(approval_id) = path.strip_prefix("/api/approvals/") {
                    let allow = body["allow"].as_bool().ok_or("缺少审批决定")?;
                    app.decide(approval_id, allow)?;
                    return Ok(json!({"ok":true}));
                }
            }
            Err("接口不存在或请求方法错误".into())
        }
    }
}
fn browse(body: &Value) -> Result<Value, String> {
    let raw = body["path"].as_str().unwrap_or("");
    if raw.is_empty() {
        let roots: Vec<String> = if cfg!(windows) {
            ('A'..='Z')
                .map(|c| format!("{c}:/"))
                .filter(|p| std::path::Path::new(p).is_dir())
                .collect()
        } else {
            vec!["/".into()]
        };
        return Ok(
            json!({"path":"","parent":null,"directories":roots.iter().map(|p|json!({"name":p,"path":p})).collect::<Vec<_>>(),"files":[],"home":dirs::home_dir()}),
        );
    }
    let path = PathBuf::from(raw);
    if !path.is_absolute() {
        return Err("请输入绝对路径".into());
    }
    let path = std::fs::canonicalize(path).map_err(|e| e.to_string())?;
    let mut dirs = Vec::new();
    let mut files = Vec::new();
    for entry in std::fs::read_dir(&path)
        .map_err(|e| e.to_string())?
        .flatten()
        .take(4000)
    {
        let p = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        let v = json!({"name":name,"path":display_path(&p)});
        if p.is_dir() {
            dirs.push(v)
        } else if name.starts_with("codex")
            || name.ends_with(".exe")
            || name.ends_with(".cmd")
            || name.ends_with(".ps1")
        {
            files.push(v)
        }
    }
    dirs.sort_by_key(|v| v["name"].as_str().unwrap_or("").to_lowercase());
    files.sort_by_key(|v| v["name"].as_str().unwrap_or("").to_lowercase());
    Ok(
        json!({"path":display_path(&path),"parent":path.parent().map(display_path),"directories":dirs,"files":files,"home":dirs::home_dir()}),
    )
}
fn display_path(p: &std::path::Path) -> String {
    p.to_string_lossy()
        .trim_start_matches("\\\\?\\")
        .to_string()
}

#[cfg(test)]
mod tests {
    #[test]
    fn browser_assets_have_no_remote_dependencies_or_templates() {
        let html = include_str!("../web/index.html");
        assert!(!html.contains("src=\"https://"));
        assert!(!html.contains("<script>"));
        assert!(!html.contains("模板"));
    }
}
