// 真实日志系统：内存环形缓冲 + 按天落盘（保留 30 天）
use serde::Serialize;
use std::collections::VecDeque;
use std::io::Write as _;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum Level {
    Info,
    Warn,
    Err,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum Source {
    Switch,
    Quota,
    Inject,
    Account,
    System,
}

#[derive(Debug, Clone, Serialize)]
pub struct LogEntry {
    pub id: u64,
    pub ts_ms: u64,
    pub level: Level,
    pub source: Source,
    pub message: String,
}

const RING_CAP: usize = 5000;
const KEEP_DAYS: u64 = 30;

static RING: Mutex<VecDeque<LogEntry>> = Mutex::new(VecDeque::new());
static SEQ: AtomicU64 = AtomicU64::new(1);
static APP: Mutex<Option<tauri::AppHandle>> = Mutex::new(None);

/// setup 时注入 AppHandle，log() 统一广播 log-changed 事件
pub fn set_app(h: tauri::AppHandle) {
    *APP.lock().unwrap() = Some(h);
}

fn logs_dir() -> PathBuf {
    crate::core::vault::app_data_dir().join("logs")
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}

/// 启动时调用：建目录并清理过期日志文件
pub fn init() {
    let dir = logs_dir();
    let _ = std::fs::create_dir_all(&dir);
    let cutoff = now_ms() - KEEP_DAYS * 86_400_000;
    if let Ok(entries) = std::fs::read_dir(&dir) {
        for e in entries.flatten() {
            if let Ok(meta) = e.metadata() {
                if let Ok(modified) = meta.modified() {
                    if let Ok(age) = modified.duration_since(std::time::UNIX_EPOCH) {
                        if (age.as_millis() as u64) < cutoff {
                            let _ = std::fs::remove_file(e.path());
                        }
                    }
                }
            }
        }
    }
}

/// 记一条日志：进环形缓冲 + 追加当天文件
pub fn log(level: Level, source: Source, message: impl Into<String>) {
    let entry = LogEntry {
        id: SEQ.fetch_add(1, Ordering::Relaxed),
        ts_ms: now_ms(),
        level,
        source,
        message: message.into(),
    };
    {
        let mut ring = RING.lock().unwrap();
        if ring.len() >= RING_CAP {
            ring.pop_front();
        }
        ring.push_back(entry.clone());
    }
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(logs_dir().join(format!("{}.log", day_stamp())))
    {
        let _ = writeln!(f, "[{}] {:?}/{:?} {}", entry.ts_ms, level, source, entry.message);
    }
    if let Some(app) = APP.lock().unwrap().as_ref() {
        use tauri::Emitter;
        let _ = app.emit("log-changed", ());
    }
}

fn day_stamp() -> String {
    let secs = (now_ms() / 1000) as i64;
    let days = secs / 86_400;
    let (y, m, d) = civil_from_days(days);
    format!("{y:04}{m:02}{d:02}")
}

/// days since 1970-01-01 -> (y, m, d)（Howard Hinnant 算法）
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (yoe + era * 400 + (m <= 2) as i64, m, d)
}

/// 最近 n 条（新→旧）
pub fn recent(n: usize) -> Vec<LogEntry> {
    let ring = RING.lock().unwrap();
    ring.iter().rev().take(n).cloned().collect()
}

/// 导出全量缓冲为 txt，返回文件路径
pub fn export() -> std::io::Result<PathBuf> {
    let dir = logs_dir();
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(format!("export-{}.txt", day_stamp()));
    let mut out = String::new();
    for e in RING.lock().unwrap().iter() {
        out.push_str(&format!("[{}] {:?}/{:?} {}\r\n", e.ts_ms, e.level, e.source, e.message));
    }
    std::fs::write(&path, out)?;
    Ok(path)
}
