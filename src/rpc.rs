use crate::runtime;
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    io::{BufRead, BufReader, Write},
    process::{Child, ChildStdin, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
        mpsc::{self, Receiver, Sender},
    },
    thread,
    time::Duration,
};

#[derive(Clone, Debug)]
pub struct RpcError {
    pub message: String,
    pub code: Option<i64>,
}
impl From<String> for RpcError {
    fn from(message: String) -> Self {
        Self {
            message,
            code: None,
        }
    }
}
impl std::fmt::Display for RpcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}
type Pending = Arc<Mutex<HashMap<u64, Sender<Result<Value, RpcError>>>>>;

pub struct Client {
    child: Mutex<Child>,
    input: Arc<Mutex<ChildStdin>>,
    pending: Pending,
    pub events: Receiver<Value>,
    next: AtomicU64,
}
impl Client {
    pub fn connect(path: &str) -> Result<Self, RpcError> {
        Self::spawn(path, &["app-server", "--listen", "stdio://"])
    }
    pub fn spawn(path: &str, args: &[&str]) -> Result<Self, RpcError> {
        let mut child = runtime::command(path)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| RpcError::from(e.to_string()))?;
        let input = Arc::new(Mutex::new(child.stdin.take().unwrap()));
        let stdout = child.stdout.take().unwrap();
        let stderr = child.stderr.take().unwrap();
        let tail = Arc::new(Mutex::new(String::new()));
        let t = tail.clone();
        thread::spawn(move || {
            use std::io::Read;
            let mut reader = stderr;
            let mut buf = [0; 1024];
            while let Ok(n) = reader.read(&mut buf) {
                if n == 0 {
                    break;
                }
                let mut s = t.lock().unwrap();
                s.push_str(&String::from_utf8_lossy(&buf[..n]));
                if s.len() > 8192 {
                    let mut boundary = s.len() - 8192;
                    while !s.is_char_boundary(boundary) {
                        boundary += 1
                    }
                    s.drain(..boundary);
                }
            }
        });
        let pending: Pending = Arc::new(Mutex::new(HashMap::new()));
        let p = pending.clone();
        let (tx, events) = mpsc::channel();
        thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            loop {
                let mut line = Vec::new();
                // Bound a line before allocating the full frame.
                use std::io::Read;
                let read = reader
                    .by_ref()
                    .take(16 * 1024 * 1024 + 1)
                    .read_until(b'\n', &mut line);
                if !matches!(read,Ok(n) if n>0) || line.len() > 16 * 1024 * 1024 {
                    break;
                }
                let Ok(value) = serde_json::from_slice::<Value>(&line) else {
                    continue;
                };
                if value.get("method").is_some() {
                    if tx.send(value).is_err() {
                        break;
                    }
                } else if let Some(id) = value["id"].as_u64()
                    && let Some(sender) = p.lock().unwrap().remove(&id)
                {
                    let result = if value.get("error").is_some() {
                        Err(RpcError {
                            message: value["error"]["message"]
                                .as_str()
                                .unwrap_or("Codex RPC error")
                                .into(),
                            code: value["error"]["code"].as_i64(),
                        })
                    } else {
                        Ok(value["result"].clone())
                    };
                    let _ = sender.send(result);
                }
            }
            let detail = tail.lock().unwrap().clone();
            let error = RpcError::from(if detail.is_empty() {
                "Codex 连接已关闭".into()
            } else {
                detail
            });
            for (_, sender) in p.lock().unwrap().drain() {
                let _ = sender.send(Err(error.clone()));
            }
            let _ = tx.send(json!({"method":"codey/disconnected"}));
        });
        let client = Self {
            child: Mutex::new(child),
            input,
            pending,
            events,
            next: AtomicU64::new(1),
        };
        client.request("initialize",json!({"clientInfo":{"name":"codey","title":"Codey","version":env!("CARGO_PKG_VERSION")},"capabilities":{"experimentalApi":true}}))?;
        client.send(json!({"method":"initialized"}))?;
        Ok(client)
    }
    fn send(&self, value: Value) -> Result<(), RpcError> {
        let mut input = self.input.lock().unwrap();
        serde_json::to_writer(&mut *input, &value).map_err(|e| RpcError::from(e.to_string()))?;
        input
            .write_all(b"\n")
            .and_then(|_| input.flush())
            .map_err(|e| RpcError::from(e.to_string()))
    }
    pub fn request(&self, method: &str, params: Value) -> Result<Value, RpcError> {
        self.request_timeout(method, params, Duration::from_secs(20))
    }
    pub fn request_timeout(
        &self,
        method: &str,
        params: Value,
        timeout: Duration,
    ) -> Result<Value, RpcError> {
        let id = self.next.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = mpsc::channel();
        self.pending.lock().unwrap().insert(id, tx);
        let result = self.send(json!({"id":id,"method":method,"params":params}));
        if let Err(e) = result {
            self.pending.lock().unwrap().remove(&id);
            return Err(e);
        }
        match rx.recv_timeout(timeout) {
            Ok(result) => result,
            Err(_) => {
                self.pending.lock().unwrap().remove(&id);
                Err(RpcError::from(format!("Codex 请求超时：{method}")))
            }
        }
    }
    pub fn respond(&self, id: Value, result: Value) -> Result<(), RpcError> {
        self.send(json!({"id":id,"result":result}))
    }
    pub fn unsupported(&self, id: Value) -> Result<(), RpcError> {
        self.send(json!({"id":id,"error":{"code":-32601,"message":"Codey does not support this interaction"}}))
    }
}
impl Drop for Client {
    fn drop(&mut self) {
        if let Ok(child) = self.child.get_mut() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}
