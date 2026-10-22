// 跨账号会话保留：切号前记录旧账号会话与最后一个已完成消息，切号后用 `fork_session`
// 在新账号自己的 project 里复制整段会话。
// 服务端 `fork_session` 是完整复制（含 task/plan_item/工具卡片），副本属于新账号，可直接继续对话；
// 旧的 chat_migrate 只收纯文本，agent 会话会丢正文，已弃用。
use super::paths::IdeKind;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::path::PathBuf;

const OUTER_PAD_MS: u64 = 15_000;
const MAX_SESSIONS: usize = 30;
const PAGE_SIZE: i64 = 100;
const SESSION_LIST_LIMIT: i64 = 50;

fn ide_tag(ide: IdeKind) -> &'static str {
    match ide {
        IdeKind::Trae => "trae",
        IdeKind::TraeCn => "traecn",
    }
}

fn archive_dir() -> PathBuf {
    super::vault::app_data_dir().join("chat-archive")
}

fn archive_path(ide: IdeKind, folder: &str) -> PathBuf {
    let mut h = Sha256::new();
    h.update(folder.to_lowercase().as_bytes());
    let digest = h.finalize();
    let hash: String = digest[..8].iter().map(|b| format!("{b:02x}")).collect();
    archive_dir().join(format!("{}-{hash}.json", ide_tag(ide)))
}

fn rpc(kind: IdeKind, service: &str, method: &str, data: Value, timeout_ms: u64) -> Result<Value, String> {
    let ctx = data
        .get("project_id")
        .and_then(|v| v.as_str())
        .map(|p| json!({ "project_id": p }))
        .unwrap_or_else(|| json!({}));
    let payload = json!({
        "action": "invoke",
        "command": "_tsw.chatRpc",
        "args": [{ "service": service, "method": method, "data": data, "context": ctx }],
        "timeout_ms": timeout_ms,
    });
    let r = super::shim::request_hot_swap(kind, &payload, timeout_ms + OUTER_PAD_MS)?;
    let v = r.get("value").cloned().unwrap_or(Value::Null);
    if v.get("ok").and_then(|b| b.as_bool()) != Some(true) {
        let msg = v.get("error").and_then(|e| e.as_str()).unwrap_or("agent RPC 失败");
        return Err(msg.to_string());
    }
    let raw = v.get("json").and_then(|s| s.as_str()).unwrap_or("null");
    serde_json::from_str(raw).map_err(|e| format!("agent 响应解析失败：{e}"))
}

fn shim_cmd(kind: IdeKind, command: &str, timeout_ms: u64) -> Result<Value, String> {
    let payload = json!({
        "action": "invoke",
        "command": command,
        "args": [],
        "timeout_ms": timeout_ms,
    });
    let r = super::shim::request_hot_swap(kind, &payload, timeout_ms + OUTER_PAD_MS)?;
    Ok(r.get("value").cloned().unwrap_or(Value::Null))
}

/// 当前窗口 project_id + 该 project 的工作区目录
fn current_project(kind: IdeKind) -> Result<(String, String), String> {
    let v = shim_cmd(kind, "icube.common.commands.tooling.getAllOpenedProjectsV2", 6_000)?;
    let pid = v
        .pointer("/data/ide_windows")
        .and_then(|w| w.as_array())
        .and_then(|w| w.first())
        .and_then(|w| w.get("project_id"))
        .and_then(|p| p.as_str())
        .filter(|s| !s.is_empty())
        .ok_or("未取到 project_id（Trae 未打开工作区窗口？）")?
        .to_string();
    let list = rpc(kind, "lite", "list_projects_v2", json!({ "page_size": 100 }), 15_000)?;
    let folder = find_project_folder(&list, &pid).unwrap_or_default();
    Ok((pid, folder))
}

fn project_items(list: &Value) -> Vec<Value> {
    list.pointer("/data/data/items")
        .and_then(|a| a.as_array())
        .cloned()
        .unwrap_or_default()
}

fn find_project_folder(list: &Value, pid: &str) -> Option<String> {
    project_items(list)
        .iter()
        .find(|it| it.get("project_id").and_then(|v| v.as_str()) == Some(pid))
        .and_then(|it| {
            it.get("single_root_workspace_folder")
                .and_then(|v| v.as_str())
                .or_else(|| {
                    it.get("workspace_folders")
                        .and_then(|v| v.as_array())
                        .and_then(|a| a.first())
                        .and_then(|v| v.as_str())
                })
        })
        .map(String::from)
}

fn same_folder(a: &str, b: &str) -> bool {
    let norm = |s: &str| s.replace('/', "\\").trim_end_matches('\\').to_lowercase();
    !a.is_empty() && norm(a) == norm(b)
}

fn find_project_by_folder(list: &Value, folder: &str) -> Option<String> {
    project_items(list)
        .iter()
        .find(|it| {
            it.get("single_root_workspace_folder")
                .and_then(|f| f.as_str())
                .map(|f| same_folder(f, folder))
                .unwrap_or(false)
        })
        .and_then(|it| it.get("project_id").and_then(|p| p.as_str()))
        .map(String::from)
}

/// 会话最后一个「已完成」消息 id（fork 不能落在 in_progress 回合上）
fn last_completed_message(kind: IdeKind, session_id: &str, project_id: &str) -> Result<String, String> {
    let v = rpc(
        kind,
        "chat",
        "get_messages",
        json!({ "session_id": session_id, "project_id": project_id, "page_size": PAGE_SIZE }),
        45_000,
    )?;
    let arr = v
        .pointer("/data/messages")
        .and_then(|a| a.as_array())
        .cloned()
        .unwrap_or_default();
    arr.iter()
        .rev()
        .find(|m| {
            m.get("status").and_then(|s| s.as_str()) != Some("in_progress")
                && m.get("status").and_then(|s| s.as_str()) != Some("ai-generate-ing")
        })
        .and_then(|m| m.get("message_id"))
        .and_then(|id| id.as_str())
        .filter(|s| !s.is_empty())
        .map(String::from)
        .ok_or_else(|| "没有已完成的回合".to_string())
}

fn load_root(path: &std::path::Path) -> Value {
    std::fs::read(path)
        .ok()
        .and_then(|cipher| super::vault::unprotect(&cipher).ok())
        .and_then(|plain| serde_json::from_slice::<Value>(&plain).ok())
        .filter(|v| v.is_object())
        .unwrap_or_else(|| json!({ "accounts": {}, "copies": {} }))
}

fn save_root(path: &std::path::Path, root: &Value) {
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let Ok(plain) = serde_json::to_vec(root) else { return };
    let Ok(cipher) = super::vault::protect(&plain) else { return };
    let _ = std::fs::write(path, cipher);
}

/// 归档当前账号的会话清单（切号前调用；失败只降级，不阻断换号）
pub fn export(kind: IdeKind) -> Result<usize, String> {
    let (project_id, folder) = current_project(kind)?;
    if folder.is_empty() {
        return Err("当前项目未关联工作区目录".into());
    }
    let uid = super::switch::scan_current(&kind)
        .ok()
        .flatten()
        .map(|b| b.user_id)
        .ok_or("当前未登录，跳过归档")?;

    let mut sessions = Vec::new();
    let mut offset = 0i64;
    loop {
        let v = rpc(
            kind,
            "chat",
            "get_sessions",
            json!({ "project_id": project_id, "limit": SESSION_LIST_LIMIT, "offset": offset }),
            30_000,
        )?;
        let arr = v
            .pointer("/data/sessions")
            .and_then(|a| a.as_array())
            .cloned()
            .unwrap_or_default();
        if arr.is_empty() {
            break;
        }
        for s in &arr {
            if sessions.len() >= MAX_SESSIONS {
                break;
            }
            let sid = s.get("session_id").and_then(|x| x.as_str()).unwrap_or("");
            if sid.is_empty() {
                continue;
            }
            let Ok(message_id) = last_completed_message(kind, sid, &project_id) else {
                continue;
            };
            sessions.push(json!({
                "session_id": sid,
                "title": s.get("name").and_then(|x| x.as_str()).unwrap_or(""),
                "created_at": s.get("created_at").and_then(|x| x.as_i64()).unwrap_or(0),
                "updated_at": s.get("update_at").and_then(|x| x.as_i64()).unwrap_or(0),
                "last_message_id": message_id,
            }));
        }
        offset += arr.len() as i64;
        if (arr.len() as i64) < SESSION_LIST_LIMIT || sessions.len() >= MAX_SESSIONS {
            break;
        }
    }

    let path = archive_path(kind, &folder);
    let mut root = load_root(&path);
    // 导入产生的副本不再归档（否则切回去会把副本再复制一遍）
    let copies: std::collections::HashSet<String> = root
        .pointer(&format!("/copies/{uid}"))
        .and_then(|c| c.as_object())
        .map(|o| o.keys().cloned().collect())
        .unwrap_or_default();
    let sessions: Vec<Value> = sessions
        .into_iter()
        .filter(|s| {
            !s.get("session_id")
                .and_then(|v| v.as_str())
                .map(|sid| copies.contains(sid))
                .unwrap_or(true)
        })
        .collect();
    root["workspace"] = json!(folder);
    let accounts = root
        .as_object_mut()
        .map(|o| o.entry("accounts").or_insert_with(|| json!({})))
        .and_then(|v| v.as_object_mut())
        .ok_or("归档结构异常")?;
    let entry = accounts.entry(uid).or_insert_with(|| json!({ "imports": {} }));
    entry["source_project_id"] = json!(project_id);
    entry["exported_at"] = json!(super::refresh::now_millis());
    entry["sessions"] = json!(sessions);
    if entry.get("imports").is_none() {
        entry["imports"] = json!({});
    }
    let n = sessions_len(entry);
    save_root(&path, &root);
    Ok(n)
}

fn sessions_len(entry: &Value) -> usize {
    entry.get("sessions").and_then(|s| s.as_array()).map(|a| a.len()).unwrap_or(0)
}

fn current_uid(kind: IdeKind) -> Option<String> {
    shim_cmd(kind, "icube.common.getUserInfo", 6_000)
        .ok()
        .and_then(|v| v.get("userId").and_then(|u| u.as_str()).map(String::from))
}

fn already_imported(root: &Value, src_uid: &str, target_uid: &str, sid: &str) -> bool {
    root.pointer(&format!("/accounts/{src_uid}/imports/{target_uid}"))
        .and_then(|d| d.as_array())
        .map(|a| a.iter().any(|x| x.as_str() == Some(sid)))
        .unwrap_or(false)
}

/// 用 fork_session 把归档中其它账号的会话完整复制到目标账号自己的 project
pub fn import_pending(kind: IdeKind, target_uid: &str) -> Result<usize, String> {
    let start = std::time::Instant::now();
    while start.elapsed().as_millis() < 20_000 {
        if current_uid(kind).as_deref() == Some(target_uid) {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(300));
    }
    if current_uid(kind).as_deref() != Some(target_uid) {
        return Err("等待新账号生效超时".into());
    }

    let dir = archive_dir();
    let prefix = format!("{}-", ide_tag(kind));
    let Ok(entries) = std::fs::read_dir(&dir) else { return Ok(0) };
    let mut total = 0;
    for e in entries.flatten() {
        let path = e.path();
        let name = path.file_name().unwrap_or_default().to_string_lossy().to_string();
        if !name.starts_with(&prefix) || !name.ends_with(".json") {
            continue;
        }
        let mut root = load_root(&path);
        let workspace = root.get("workspace").and_then(|w| w.as_str()).unwrap_or("").to_string();
        if workspace.is_empty() {
            continue;
        }
        // 目标账号在该目录下的 project：新账号首次切换时由 initChatView 异步创建
        let mut pid = None;
        for attempt in 0..20 {
            let Ok(list) = rpc(kind, "lite", "list_projects_v2", json!({ "page_size": 100 }), 20_000) else {
                return Ok(total);
            };
            pid = find_project_by_folder(&list, &workspace);
            if pid.is_some() || attempt == 19 {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(1_000));
        }
        let Some(pid) = pid else {
            super::logger::log(
                super::logger::Level::Warn,
                super::logger::Source::Switch,
                format!("会话复制跳过：新账号尚未为 {workspace} 创建 project"),
            );
            continue;
        };

        let snapshot: Vec<(String, Vec<Value>)> = root
            .get("accounts")
            .and_then(|a| a.as_object())
            .map(|accounts| {
                accounts
                    .iter()
                    .filter(|(u, _)| u.as_str() != target_uid)
                    .map(|(u, v)| {
                        (
                            u.clone(),
                            v.get("sessions").and_then(|s| s.as_array()).cloned().unwrap_or_default(),
                        )
                    })
                    .collect()
            })
            .unwrap_or_default();
        for (src_uid, sessions) in snapshot {
            for s in sessions {
                let sid = s.get("session_id").and_then(|x| x.as_str()).unwrap_or("");
                let mid = s.get("last_message_id").and_then(|x| x.as_str()).unwrap_or("");
                if sid.is_empty() || mid.is_empty() || already_imported(&root, &src_uid, target_uid, sid) {
                    continue;
                }
                let resp = rpc(
                    kind,
                    "lite",
                    "fork_session",
                    json!({ "session_id": sid, "message_id": mid, "project_id": pid }),
                    60_000,
                );
                let new_sid = match resp {
                    Ok(v) if v.get("code").and_then(|c| c.as_i64()) == Some(0) => v
                        .pointer("/data/new_session_id")
                        .and_then(|x| x.as_str())
                        .unwrap_or("")
                        .to_string(),
                    Ok(v) => {
                        let msg = v.get("message").and_then(|m| m.as_str()).unwrap_or("未知错误");
                        super::logger::log(
                            super::logger::Level::Warn,
                            super::logger::Source::Switch,
                            format!("会话复制失败（{sid}）：{msg}"),
                        );
                        continue;
                    }
                    Err(e) => {
                        super::logger::log(
                            super::logger::Level::Warn,
                            super::logger::Source::Switch,
                            format!("会话复制失败（{sid}）：{e}"),
                        );
                        continue;
                    }
                };
                if new_sid.is_empty() {
                    continue;
                }
                let done = &mut root["accounts"][src_uid.as_str()]["imports"][target_uid];
                if !done.is_array() {
                    *done = json!([]);
                }
                if let Some(arr) = done.as_array_mut() {
                    arr.push(json!(sid));
                }
                let copied = &mut root["copies"][target_uid][new_sid.as_str()];
                *copied = json!(format!("{src_uid}:{sid}"));
                total += 1;
                super::logger::log(
                    super::logger::Level::Info,
                    super::logger::Source::Switch,
                    format!("会话已复制：{sid} → {new_sid}"),
                );
                save_root(&path, &root);
            }
        }
    }
    Ok(total)
}
