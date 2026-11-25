//! Development-only JSON-RPC peer. Never included in the release binary.
use serde_json::{Value, json};
use std::{
    io::{self, BufRead, Write},
    path::PathBuf,
};
fn send(v: Value) {
    let mut out = io::stdout().lock();
    writeln!(out, "{v}").unwrap();
    out.flush().unwrap();
}
fn turns(dir: &std::path::Path) -> Vec<Value> {
    std::fs::read_to_string(dir.join("accepted.jsonl"))
        .unwrap_or_default()
        .lines()
        .filter_map(|s| serde_json::from_str(s).ok())
        .collect()
}
fn main() {
    if std::env::args().any(|s| s == "--version") {
        println!("codex-cli 0.172.0-codey-test");
        return;
    }
    let dir = PathBuf::from(std::env::var("CODEY_MOCK_DIR").expect("mock needs CODEY_MOCK_DIR"));
    let cwd = dir.to_string_lossy().to_string();
    let mut pending: Option<(String, String)> = None;
    for line in io::stdin().lock().lines() {
        let Ok(line) = line else { break };
        let Ok(v) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        if v.get("method").is_none() {
            if let Some((tid, turn)) = pending.take()
                && v["result"]["decision"] == "accept"
            {
                send(
                    json!({"method":"turn/completed","params":{"threadId":tid,"turn":{"id":turn,"status":"completed","error":null}}}),
                );
            }
            continue;
        }
        let method = v["method"].as_str().unwrap();
        let p = &v["params"];
        let rid = &v["id"];
        let thread = |tid: &str| json!({"id":tid,"cwd":cwd,"model":"mock-model","reasoningEffort":"low","status":{"type":if tid=="busy-thread"{"active"}else{"idle"}},"name":"已有测试对话","preview":"Existing conversation","turns":[]});
        let result = match method {
            "initialize" => json!({}),
            "initialized" => continue,
            "account/read" => {
                json!({"account":{"type":"chatgpt","email":"test@codey.invalid","planType":"plus"},"userId":if dir.join("account-changed").exists(){"other-user"}else{"test-user"}})
            }
            "model/list" => {
                json!({"data":[{"model":"mock-model","displayName":"Mock Model","isDefault":true,"supportedReasoningEfforts":[{"reasoningEffort":"low"},{"reasoningEffort":"high"}]}],"nextCursor":null})
            }
            "config/read" => json!({"config":{"model":"mock-model"}}),
            "account/rateLimits/read" => {
                json!({"ordinaryUsageAllowed":!dir.join("quota-blocked").exists(),"rateLimits":{"primary":{"usedPercent":if dir.join("quota-blocked").exists(){100}else{10},"resetsAt":1}}})
            }
            "thread/list" => {
                json!({"data":[thread("existing-thread"),thread("busy-thread")],"nextCursor":null})
            }
            "thread/read" | "thread/resume" => {
                json!({"thread":thread(p["threadId"].as_str().unwrap()),"model":"mock-model","reasoningEffort":"low"})
            }
            "thread/start" => {
                json!({"thread":thread(&uuid::Uuid::new_v4().to_string()),"model":"mock-model","reasoningEffort":"low"})
            }
            "thread/turns/list" => {
                let t: Vec<Value> = turns(&dir)
                    .into_iter()
                    .filter(|t| t["threadId"] == p["threadId"])
                    .collect();
                let data = if t.is_empty() {
                    vec![
                        json!({"id":"baseline","status":"completed","itemsView":"full","items":[{"id":"baseline-message","type":"agentMessage","text":if dir.join("history-changed").exists(){"changed"}else{"baseline"}}]}),
                    ]
                } else {
                    t
                };
                json!({"data":data,"nextCursor":null})
            }
            "turn/start" => {
                let text = p["input"][0]["text"].as_str().unwrap();
                let tid = p["threadId"].as_str().unwrap().to_string();
                let turn = uuid::Uuid::new_v4().to_string();
                if text.contains("[reject]") {
                    send(json!({"id":rid,"error":{"code":-32602,"message":"Rejected input"}}));
                    continue;
                }
                let completed = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_secs();
                use std::fs::OpenOptions;
                let mut f = OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(dir.join("accepted.jsonl"))
                    .unwrap();
                writeln!(f,"{}",json!({"id":turn,"threadId":tid,"status":"completed","completedAt":completed,"itemsView":"full","items":[{"id":"message","type":"userMessage","clientId":p["clientUserMessageId"],"content":p["input"]}]})).unwrap();
                if text.contains("[disconnect]") {
                    return;
                }
                send(json!({"id":rid,"result":{"turn":{"id":turn}}}));
                if text.contains("[approval]") {
                    pending = Some((tid.clone(), turn.clone()));
                    send(
                        json!({"id":"approval-request","method":"item/commandExecution/requestApproval","params":{"threadId":tid,"turnId":turn,"command":"mock command"}}),
                    );
                } else if !text.contains("[hold]") {
                    send(
                        json!({"method":"item/agentMessage/delta","params":{"threadId":tid,"delta":"Mock 执行完成。\n"}}),
                    );
                    send(
                        json!({"method":"turn/completed","params":{"threadId":tid,"turn":{"id":turn,"status":"completed","error":null}}}),
                    );
                }
                continue;
            }
            "turn/interrupt" => json!({}),
            _ => {
                send(json!({"id":rid,"error":{"code":-32601,"message":"Method not found"}}));
                continue;
            }
        };
        send(json!({"id":rid,"result":result}));
    }
}
