use serde::Serialize;
use std::{
    env, fs,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeInfo {
    pub path: String,
    pub entry: String,
    pub source: String,
    pub version: String,
}

pub fn command(path: &str) -> Command {
    let mut cmd = Command::new(path);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x08000000);
    }
    cmd
}
fn native_name() -> &'static str {
    if cfg!(windows) { "codex.exe" } else { "codex" }
}
fn find_vendor(root: &Path, depth: usize) -> Option<PathBuf> {
    if depth == 0 {
        return None;
    }
    let entries = fs::read_dir(root).ok()?;
    let mut dirs = Vec::new();
    for item in entries.flatten() {
        let path = item.path();
        if path.is_file() && item.file_name() == native_name() {
            return Some(path);
        }
        if path.is_dir() {
            dirs.push(path)
        }
    }
    // npm optional packages are architecture-specific; don't select another platform's binary.
    for path in dirs {
        let name = path.file_name()?.to_string_lossy();
        if (name.contains("linux") && !cfg!(target_os = "linux"))
            || (name.contains("darwin") && !cfg!(target_os = "macos"))
            || (name.contains("windows") && !cfg!(windows))
            || (name.contains("win32") && !cfg!(windows))
            || (name.contains("aarch64") && !cfg!(target_arch = "aarch64"))
            || (name.contains("x86_64") && !cfg!(target_arch = "x86_64"))
        {
            continue;
        }
        if let Some(found) = find_vendor(&path, depth - 1) {
            return Some(found);
        }
    }
    None
}
fn resolve_entry(entry: &Path) -> Result<PathBuf, String> {
    let canonical = fs::canonicalize(entry).map_err(|e| format!("无法读取 Codex 入口：{e}"))?;
    let ext = canonical
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_lowercase();
    let is_script = ["cmd", "bat", "ps1", "js"].contains(&ext.as_str())
        || canonical.file_name().is_some_and(|n| n == "codex.js");
    if !is_script {
        return Ok(canonical);
    }
    let parent = canonical.parent().ok_or("入口路径没有父目录")?;
    let original = entry.parent().unwrap_or(parent);
    for root in [
        parent.join("node_modules/@openai/codex"),
        original.join("node_modules/@openai/codex"),
        parent.join("../lib/node_modules/@openai/codex"),
        parent.join(".."),
        parent.to_path_buf(),
    ] {
        if let Some(native) = find_vendor(&root.join("vendor"), 5)
            .or_else(|| find_vendor(&root.join("node_modules/@openai"), 7))
        {
            return fs::canonicalize(native).map_err(|e| e.to_string());
        }
    }
    Err("已找到 npm 启动脚本，但未找到对应 Codex 二进制。请重新安装 Codex CLI，或手动填写实际 codex/codex.exe 路径".into())
}
pub fn discover(manual: Option<&str>) -> Result<RuntimeInfo, String> {
    let mut candidates = Vec::new();
    if let Some(path) = manual.filter(|p| !p.trim().is_empty()) {
        candidates.push((PathBuf::from(path.trim()), "manual"));
    } else {
        let paths = env::var_os("PATH").unwrap_or_default();
        for dir in env::split_paths(&paths) {
            let names: &[&str] = if cfg!(windows) {
                &["codex.exe", "codex.cmd", "codex.ps1", "codex"]
            } else {
                &["codex"]
            };
            for name in names {
                let p = dir.join(name);
                if p.is_file() {
                    candidates.push((p, "PATH"));
                }
            }
        }
    }
    let mut errors = Vec::new();
    for (entry, source) in candidates {
        let result = (|| {
            let path = resolve_entry(&entry)?;
            let mut child = command(&path.to_string_lossy())
                .arg("--version")
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn()
                .map_err(|e| e.to_string())?;
            let until = Instant::now() + Duration::from_secs(8);
            loop {
                if let Some(status) = child.try_wait().map_err(|e| e.to_string())? {
                    if !status.success() {
                        return Err("Codex --version 返回失败".into());
                    }
                    break;
                }
                if Instant::now() > until {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err("Codex 版本检查超时".into());
                }
                thread::sleep(Duration::from_millis(30));
            }
            use std::io::Read;
            let mut version = String::new();
            child
                .stdout
                .take()
                .unwrap()
                .take(4096)
                .read_to_string(&mut version)
                .map_err(|e| e.to_string())?;
            if !version.to_lowercase().contains("codex") {
                return Err("所选程序未报告 Codex 版本".into());
            }
            Ok(RuntimeInfo {
                path: path.to_string_lossy().into(),
                entry: entry.to_string_lossy().into(),
                source: source.into(),
                version: version.trim().into(),
            })
        })();
        match result {
            Ok(info) => return Ok(info),
            Err(e) => errors.push(e),
        }
    }
    Err(errors.into_iter().next().unwrap_or_else(||"PATH 中没有找到 Codex CLI。请安装 Node.js LTS 后运行 npm install -g @openai/codex@latest，再重新检测；也可手动填写二进制绝对路径".into()))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn missing_manual_path_does_not_silently_fall_back() {
        assert!(discover(Some("Z:/codey-missing/codex.exe")).is_err());
    }
}
