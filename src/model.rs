use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::time::{SystemTime, UNIX_EPOCH};
use uuid::Uuid;

pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}
pub fn id() -> String {
    Uuid::new_v4().to_string()
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Settings {
    pub codex_path: String,
    pub approval_timeout_secs: u64,
    pub history_retention_days: u32,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            codex_path: String::new(),
            approval_timeout_secs: 600,
            history_retention_days: 0,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Job {
    pub id: String,
    pub title: String,
    pub prompt: String,
    pub cwd: String,
    pub thread_id: Option<String>,
    pub thread_mode: String,
    pub send_mode: String,
    pub send_at: u64,
    pub expires_at: Option<u64>,
    pub next_check_at: u64,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub sandbox: String,
    pub approval_policy: String,
    pub network_access: bool,
    pub stale_policy: String,
    pub missed_policy: String,
    pub revision: Option<String>,
    pub pending_revision: Option<String>,
    pub confirmation_id: Option<String>,
    pub account_key: String,
    pub runtime_path: String,
    pub status: String,
    pub reason: String,
    pub created_at: u64,
    pub updated_at: u64,
    pub submitted_at: Option<u64>,
    pub accepted_at: Option<u64>,
    pub finished_at: Option<u64>,
    pub turn_id: Option<String>,
    pub client_message_id: String,
    pub attempts: u32,
    pub check_failures: u32,
    pub output: String,
}
impl Default for Job {
    fn default() -> Self {
        Self {
            id: id(),
            title: String::new(),
            prompt: String::new(),
            cwd: String::new(),
            thread_id: None,
            thread_mode: "new".into(),
            send_mode: "time".into(),
            send_at: now(),
            expires_at: None,
            next_check_at: now(),
            model: None,
            effort: None,
            sandbox: "workspace-write".into(),
            approval_policy: "on-request".into(),
            network_access: false,
            stale_policy: "confirm".into(),
            missed_policy: "confirm".into(),
            revision: None,
            pending_revision: None,
            confirmation_id: None,
            account_key: String::new(),
            runtime_path: String::new(),
            status: "scheduled".into(),
            reason: String::new(),
            created_at: now(),
            updated_at: now(),
            submitted_at: None,
            accepted_at: None,
            finished_at: None,
            turn_id: None,
            client_message_id: id(),
            attempts: 0,
            check_failures: 0,
            output: String::new(),
        }
    }
}
impl Job {
    pub fn active(&self) -> bool {
        matches!(
            self.status.as_str(),
            "checking" | "sending" | "running" | "approval"
        )
    }
    pub fn terminal(&self) -> bool {
        matches!(
            self.status.as_str(),
            "completed" | "failed" | "cancelled" | "skipped" | "expired" | "interrupted"
        )
    }
    pub fn due(&self) -> u64 {
        if self.send_mode == "available" {
            self.next_check_at.min(self.expires_at.unwrap_or(u64::MAX))
        } else {
            self.send_at.max(self.next_check_at)
        }
    }
    pub fn validate(&self) -> Result<(), String> {
        if self.prompt.trim().is_empty() || self.prompt.len() > 65536 {
            return Err("Prompt 必须填写，且不能超过 64 KiB UTF-8 字节".into());
        }
        if !std::path::Path::new(&self.cwd).is_absolute()
            || !std::path::Path::new(&self.cwd).is_dir()
        {
            return Err("请选择存在的本机项目绝对路径".into());
        }
        for (v, allowed) in [
            (self.thread_mode.as_str(), &["new", "existing"][..]),
            (self.send_mode.as_str(), &["time", "available"][..]),
            (
                self.sandbox.as_str(),
                &["read-only", "workspace-write", "danger-full-access"][..],
            ),
            (
                self.approval_policy.as_str(),
                &["on-request", "untrusted", "never"][..],
            ),
            (self.stale_policy.as_str(), &["send", "skip", "confirm"][..]),
            (
                self.missed_policy.as_str(),
                &["send", "skip", "confirm"][..],
            ),
        ] {
            if !allowed.contains(&v) {
                return Err(format!("不支持的设置：{v}"));
            }
        }
        if self.thread_mode == "existing" && self.thread_id.as_deref().unwrap_or("").is_empty() {
            return Err("请选择已有对话".into());
        }
        if self.send_mode == "available"
            && !self
                .expires_at
                .is_some_and(|v| v > now() && v <= now() + 30 * 86400000)
        {
            return Err("等待截止期限应在未来 30 天内".into());
        }
        if self.effort.is_some() && self.model.is_none() {
            return Err("设置推理强度前请选择模型".into());
        }
        Ok(())
    }
}

#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Database {
    pub schema: u32,
    pub settings: Settings,
    pub jobs: Vec<Job>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Approval {
    pub id: String,
    pub job_id: String,
    pub method: String,
    pub detail: Value,
    pub created_at: u64,
    pub expires_at: u64,
    #[serde(skip)]
    pub decision: Option<bool>,
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn availability_deadline_wakes_before_a_later_retry() {
        let j = Job {
            send_mode: "available".into(),
            next_check_at: 900000,
            expires_at: Some(1000),
            ..Job::default()
        };
        assert_eq!(j.due(), 1000);
    }
    #[test]
    fn rejects_invalid_prompt_and_permissions() {
        let mut j = Job {
            cwd: std::env::current_dir().unwrap().to_string_lossy().into(),
            ..Job::default()
        };
        assert!(j.validate().is_err());
        j.prompt = "你好".into();
        assert!(j.validate().is_ok());
        j.prompt = "中".repeat(22000);
        assert!(j.validate().is_err());
        j.prompt = "hello".into();
        j.approval_policy = "auto-approve".into();
        assert!(j.validate().is_err());
    }
}
