// 额度采样历史：%APPDATA%\TraeIDE-Switcher\usage-history.json
// 结构：{ "<account_id>": [ { "ts": <epoch_ms>, "remain": <剩余积分> }, ... ] }
// 用途：前端画消耗趋势 + 按斜率预测耗尽时间；只存聚合数字，不存任何凭据。
use serde_json::{json, Value};
use std::path::PathBuf;

/// 每账号保留的采样点数上限
const MAX_SAMPLES: usize = 240;
/// 采样保留时长（30 天）
const MAX_AGE_MS: i64 = 30 * 86_400_000;

fn path() -> PathBuf {
    crate::core::vault::app_data_dir().join("usage-history.json")
}

fn load_root() -> Value {
    std::fs::read_to_string(path())
        .ok()
        .and_then(|s| serde_json::from_str::<Value>(&s).ok())
        .filter(|v| v.is_object())
        .unwrap_or_else(|| json!({}))
}

/// 记一次采样。同分钟内重复采样按最新值覆盖，避免高频刷新把序列撑爆。
pub fn record(account_id: &str, remain: f64, ts_ms: i64) {
    let mut root = load_root();
    let Some(obj) = root.as_object_mut() else { return };
    let arr = obj
        .entry(account_id.to_string())
        .or_insert_with(|| json!([]));
    let Some(list) = arr.as_array_mut() else { return };

    let minute_bucket = ts_ms / 60_000;
    if let Some(last) = list.last_mut() {
        let last_bucket = last.get("ts").and_then(|t| t.as_i64()).unwrap_or(0) / 60_000;
        if last_bucket == minute_bucket {
            *last = json!({ "ts": ts_ms, "remain": remain });
            write(&root);
            return;
        }
    }
    list.push(json!({ "ts": ts_ms, "remain": remain }));
    // 裁剪：按时间与本数上限
    list.retain(|p| {
        p.get("ts")
            .and_then(|t| t.as_i64())
            .map(|t| ts_ms - t <= MAX_AGE_MS)
            .unwrap_or(false)
    });
    if list.len() > MAX_SAMPLES {
        let cut = list.len() - MAX_SAMPLES;
        list.drain(0..cut);
    }
    write(&root);
}

fn write(root: &Value) {
    let p = path();
    if let Some(dir) = p.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(s) = serde_json::to_string(root) {
        let _ = std::fs::write(p, s);
    }
}

/// 读取某账号的采样序列（升序）
pub fn series(account_id: &str) -> Value {
    load_root()
        .get(account_id)
        .cloned()
        .unwrap_or_else(|| json!([]))
}

// ============ 切换历史时间线：%APPDATA%\TraeIDE-Switcher\switch-history.json ============
// 结构：[ { ts, from, to, ide, hot, ok, ms, note } ]（新→旧由前端排序；此处按发生顺序追加）
// 只记展示字段，不落任何凭据。

/// 保留的切换记录条数上限
const MAX_SWITCH_RECORDS: usize = 500;

fn switch_path() -> PathBuf {
    crate::core::vault::app_data_dir().join("switch-history.json")
}

/// 记一次账号切换（成功/失败都记，便于回溯"何时切过、是否切成功"）
pub fn record_switch(from: &str, to: &str, ide: &str, hot: bool, ok: bool, ms: u64, note: &str) {
    let mut list: Vec<Value> = std::fs::read_to_string(switch_path())
        .ok()
        .and_then(|s| serde_json::from_str::<Vec<Value>>(&s).ok())
        .unwrap_or_default();
    list.push(json!({
        "ts": super::refresh::now_millis(),
        "from": from,
        "to": to,
        "ide": ide,
        "hot": hot,
        "ok": ok,
        "ms": ms,
        "note": note,
    }));
    if list.len() > MAX_SWITCH_RECORDS {
        let cut = list.len() - MAX_SWITCH_RECORDS;
        list.drain(0..cut);
    }
    let p = switch_path();
    if let Some(dir) = p.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(s) = serde_json::to_string(&list) {
        let _ = std::fs::write(p, s);
    }
}

/// 读取全部切换记录（按发生顺序，前端自行倒序展示）
pub fn switches() -> Value {
    let list: Vec<Value> = std::fs::read_to_string(switch_path())
        .ok()
        .and_then(|s| serde_json::from_str::<Vec<Value>>(&s).ok())
        .unwrap_or_default();
    json!(list)
}
