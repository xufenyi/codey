use crate::model::{Database, id, now};
use fs2::FileExt;
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
};

pub struct Store {
    pub db: Database,
    pub dir: PathBuf,
    _lock: File,
}
impl Store {
    pub fn load(dir: PathBuf) -> Result<Self, String> {
        fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(dir.join("instance.lock"))
            .map_err(|e| e.to_string())?;
        lock.try_lock_exclusive()
            .map_err(|_| "Codey 已在运行，请通过托盘或再次启动打开管理页面".to_string())?;
        let file = dir.join("codey.json");
        let mut db: Database = if file.exists() {
            let f = File::open(&file).map_err(|e| e.to_string())?;
            if f.metadata().map_err(|e| e.to_string())?.len() > 16 * 1024 * 1024 {
                return Err("任务库超过 16 MiB；请先备份并清理历史".into());
            }
            serde_json::from_reader(f).map_err(|e| format!("任务库损坏，已保留原文件：{e}"))?
        } else {
            Database {
                schema: 1,
                ..Database::default()
            }
        };
        if db.schema != 1 {
            return Err("不支持此任务库版本".into());
        }
        for job in &mut db.jobs {
            if job.active() {
                job.status = if job.submitted_at.is_some() {
                    "review"
                } else {
                    "paused"
                }
                .into();
                job.reason = "上次运行中断，请核对执行结果".into();
            } else if job.status == "scheduled" && job.send_mode == "time" && job.due() < now() {
                match job.missed_policy.as_str() {
                    "skip" => {
                        job.status = "skipped".into();
                        job.finished_at = Some(now());
                    }
                    "send" => {}
                    _ => {
                        job.status = "confirmation".into();
                        job.reason = "启动时发现错过执行时间".into();
                        job.confirmation_id = Some(id());
                    }
                }
            }
        }
        let s = Self {
            db,
            dir,
            _lock: lock,
        };
        s.save()?;
        Ok(s)
    }
    pub fn save(&self) -> Result<(), String> {
        let bytes = serde_json::to_vec_pretty(&self.db).map_err(|e| e.to_string())?;
        if bytes.len() > 16 * 1024 * 1024 {
            return Err("任务库超过 16 MiB，请清理已结束历史".into());
        }
        atomic_write(&self.dir.join("codey.json"), &bytes)
    }
    pub fn change<T>(
        &mut self,
        f: impl FnOnce(&mut Database) -> Result<T, String>,
    ) -> Result<T, String> {
        let previous = self.db.clone();
        let value = match f(&mut self.db) {
            Ok(v) => v,
            Err(e) => {
                self.db = previous;
                return Err(e);
            }
        };
        if let Err(e) = self.save() {
            self.db = previous;
            return Err(e);
        }
        Ok(value)
    }
}

pub fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let tmp = path.with_extension(format!("{}.tmp", id()));
    let mut opts = OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let result = (|| {
        let mut f = opts.open(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
        drop(f);
        // std::fs::rename replaces an existing destination on all supported platforms.
        fs::rename(&tmp, path)?;
        #[cfg(unix)]
        File::open(path.parent().unwrap())?.sync_all()?;
        Ok::<_, std::io::Error>(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result.map_err(|e| e.to_string())
}
pub fn read_small(path: &Path) -> Result<String, String> {
    let mut value = String::new();
    File::open(path)
        .map_err(|e| e.to_string())?
        .take(16384)
        .read_to_string(&mut value)
        .map_err(|e| e.to_string())?;
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn crash_recovery_never_resubmits_uncertain_input() {
        let dir = std::env::temp_dir().join(format!("codey-test-{}", id()));
        {
            let mut s = Store::load(dir.clone()).unwrap();
            let j = crate::model::Job {
                status: "sending".into(),
                submitted_at: Some(now()),
                ..Default::default()
            };
            s.db.jobs.push(j);
            s.save().unwrap();
        }
        {
            let s = Store::load(dir.clone()).unwrap();
            assert_eq!(s.db.jobs[0].status, "review");
            assert!(Store::load(dir.clone()).is_err());
        }
        fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn failed_change_rolls_back() {
        let dir = std::env::temp_dir().join(format!("codey-test-{}", id()));
        {
            let mut s = Store::load(dir.clone()).unwrap();
            let result: Result<(), String> = s.change(|d| {
                d.schema = 99;
                Err("test".into())
            });
            assert!(result.is_err());
            assert_eq!(s.db.schema, 1);
        }
        fs::remove_dir_all(dir).unwrap();
    }
}
