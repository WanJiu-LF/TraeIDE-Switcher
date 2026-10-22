// 跨账号会话归档：切号前导出旧账号会话文本，切号后用 chat_migrate 导入新账号自己的 project。
// 服务端会话按账号隔离，固定 project_id 的旧做法只会得到空列表 + project_not_found；
// 这里改用官方迁移接口，把文本记录复制到新账号名下（session_id 重新生成，避免全局唯一冲突）。
use super::paths::IdeKind;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::path::PathBuf;
use std::time::Instant;

const OUTER_PAD_MS: u64 = 15_000;
const EXPORT_BUDGET_MS: u128 = 60_000;
const MAX_SESSIONS: usize = 20;
const MAX_PAGES: usize = 2;
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

/// `_tsw.chatRpc` 代理调用（service/method 固定由本模块给出，未就绪时返回错误由调用方降级）
fn rpc(kind: IdeKind, service: &str, method: &str, data: Value, timeout_ms: u64) -> Result<Value, String> {
    // 显式带 context.project_id：运行中的旧版补丁仍会回落到 localStorage 里的历史 project_id，
    // 不显式覆盖会把请求路由到切号前的项目（表现为 code 0 但会话不落库）
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
        let msg = v
            .get("error")
            .and_then(|e| e.as_str())
            .unwrap_or("agent RPC 失败");
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
    let v = shim_cmd(
        kind,
        "icube.common.commands.tooling.getAllOpenedProjectsV2",
        6_000,
    )?;
    let pid = v
        .pointer("/data/ide_windows")
        .and_then(|w| w.as_array())
        .and_then(|w| w.first())
        .and_then(|w| w.get("project_id"))
        .and_then(|p| p.as_str())
        .filter(|s| !s.is_empty())
        .ok_or("未取到 project_id（Trae 未打开工作区窗口？）")?
        .to_string();
    let list = rpc(
        kind,
        "lite",
        "list_projects_v2",
        json!({ "page_size": 100 }),
        15_000,
    )?;
    let folder = list
        .pointer("/data/data/items")
        .and_then(|a| a.as_array())
        .and_then(|items| {
            items
                .iter()
                .find(|it| it.get("project_id").and_then(|v| v.as_str()) == Some(pid.as_str()))
        })
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
        .unwrap_or("")
        .to_string();
    Ok((pid, folder))
}

fn extract_text(content: Option<&Value>) -> String {
    match content {
        Some(Value::String(s)) => s.trim().to_string(),
        Some(Value::Array(blocks)) => blocks
            .iter()
            .filter_map(|b| {
                if b.get("type").and_then(|t| t.as_str()) == Some("text") {
                    b.get("text_content").and_then(|t| t.as_str())
                } else {
                    None
                }
            })
            .collect::<Vec<_>>()
            .join("\n\n")
            .trim()
            .to_string(),
        Some(Value::Object(o)) => {
            if o.get("type").and_then(|t| t.as_str()) == Some("text") {
                o.get("text_content")
                    .and_then(|t| t.as_str())
                    .unwrap_or("")
                    .trim()
                    .to_string()
            } else {
                String::new()
            }
        }
        _ => String::new(),
    }
}

/// 会话文本消息（分页拉取；task/plan_item 无纯文本正文，归入本地归档但不参与导入）
fn fetch_text_messages(kind: IdeKind, session_id: &str, project_id: &str) -> Result<Vec<Value>, String> {
    let mut out = Vec::new();
    let mut token = String::new();
    for _ in 0..MAX_PAGES {
        let mut data = json!({
            "session_id": session_id,
            "project_id": project_id,
            "page_size": PAGE_SIZE,
        });
        if !token.is_empty() {
            data["page_token"] = json!(token);
        }
        let v = rpc(kind, "chat", "get_messages", data, 45_000)?;
        let arr = v
            .pointer("/data/messages")
            .and_then(|a| a.as_array())
            .cloned()
            .unwrap_or_default();
        if arr.is_empty() {
            break;
        }
        for m in arr {
            let role = m.get("role").and_then(|r| r.as_str()).unwrap_or("");
            if !matches!(role, "user" | "assistant" | "system") {
                continue;
            }
            let text = extract_text(m.get("content"));
            if text.is_empty() {
                continue;
            }
            out.push(json!({ "role": role, "content": text }));
        }
        token = v
            .pointer("/data/next_page_token")
            .and_then(|t| t.as_str())
            .unwrap_or("")
            .to_string();
        if token.is_empty() {
            break;
        }
    }
    Ok(out)
}

fn load_root(path: &std::path::Path) -> Value {
    std::fs::read(path)
        .ok()
        .and_then(|cipher| super::vault::unprotect(&cipher).ok())
        .and_then(|plain| serde_json::from_slice::<Value>(&plain).ok())
        .filter(|v| v.is_object())
        .unwrap_or_else(|| json!({ "accounts": {} }))
}

fn save_root(path: &std::path::Path, root: &Value) {
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let Ok(plain) = serde_json::to_vec(root) else { return };
    let Ok(cipher) = super::vault::protect(&plain) else { return };
    let _ = std::fs::write(path, cipher);
}

/// 归档当前账号（切号前调用；失败只降级，不阻断换号）
pub fn export(kind: IdeKind) -> Result<usize, String> {
    let t0 = Instant::now();
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
            if sessions.len() >= MAX_SESSIONS || t0.elapsed().as_millis() > EXPORT_BUDGET_MS {
                break;
            }
            let sid = s.get("session_id").and_then(|x| x.as_str()).unwrap_or("");
            if sid.is_empty() {
                continue;
            }
            let messages = fetch_text_messages(kind, sid, &project_id)?;
            if messages.is_empty() {
                continue;
            }
            sessions.push(json!({
                "session_id": sid,
                "title": s.get("name").and_then(|x| x.as_str()).unwrap_or(""),
                "created_at": s.get("created_at").and_then(|x| x.as_i64()).unwrap_or(0),
                "updated_at": s.get("update_at").and_then(|x| x.as_i64()).unwrap_or(0),
                "messages": messages,
            }));
        }
        offset += arr.len() as i64;
        if (arr.len() as i64) < SESSION_LIST_LIMIT
            || sessions.len() >= MAX_SESSIONS
            || t0.elapsed().as_millis() > EXPORT_BUDGET_MS
        {
            break;
        }
    }

    let path = archive_path(kind, &folder);
    let mut root = load_root(&path);
    // 导入产生的新账号副本不再归档（否则切回去会把副本再导入一遍，形成重复）
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
    let entry = accounts
        .entry(uid)
        .or_insert_with(|| json!({ "imports": {} }));
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
    entry
        .get("sessions")
        .and_then(|s| s.as_array())
        .map(|a| a.len())
        .unwrap_or(0)
}

fn same_folder(a: &str, b: &str) -> bool {
    let norm = |s: &str| s.replace('/', "\\").trim_end_matches('\\').to_lowercase();
    !a.is_empty() && norm(a) == norm(b)
}

/// 生成 24 位 hex id（服务端会话/消息 id 形态：秒级时间戳前缀 + 随机）
fn new_id() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let mut rnd = [0u8; 8];
    rand::RngCore::fill_bytes(&mut rand::thread_rng(), &mut rnd);
    let tail: String = rnd.iter().map(|b| format!("{b:02x}")).collect();
    format!("{now:08x}{tail}")
}

const MAX_TOTAL_BYTES: usize = 180_000;
const MAX_MSG_BYTES: usize = 8_000;

fn truncate_utf8(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// 导入消息体：保留最近的部分并限制总量（服务端对超大 payload 会返回成功但不落库）
fn import_messages(messages: &[Value]) -> Vec<Value> {
    let mut picked: Vec<(String, String)> = Vec::new();
    let mut budget = MAX_TOTAL_BYTES;
    for m in messages.iter().rev() {
        let role = m.get("role").and_then(|r| r.as_str()).unwrap_or("user");
        let text = m.get("content").and_then(|c| c.as_str()).unwrap_or("").trim();
        if text.is_empty() {
            continue;
        }
        let text = truncate_utf8(text, MAX_MSG_BYTES);
        if text.len() > budget {
            break;
        }
        budget -= text.len();
        picked.push((role.to_string(), text.to_string()));
    }
    picked.reverse();
    // 会话必须首条为用户、末条为助手：仅用户消息的会话会被服务端返回 success 但不落库
    // （agent 会话的助手正文在 task/plan_item 里，抽取后只剩用户提问）
    if picked.first().map(|(r, _)| r.as_str()) != Some("user") {
        picked.insert(0, ("user".into(), "（历史记录导入）".into()));
    }
    if picked.last().map(|(r, _)| r.as_str()) != Some("assistant") {
        picked.push((
            "assistant".into(),
            "（历史记录导入：该轮回复为工具/计划卡片，无文本内容）".into(),
        ));
    }
    picked
        .into_iter()
        .enumerate()
        .map(|(i, (role, content))| {
            json!({
                "model_name": "",
                "role": role,
                "display_model_name": "",
                "content": content,
                "turn_id": new_id(),
                "multi_media": [],
                "status": "completed",
                "turn_index": i,
                "parsed_query": [],
                "meta": {
                    "ckg_status": "to_build_ckg",
                    "has_workspace_mentioned": false,
                    "has_folder_mentioned": false,
                    "has_lint_error_mentioned": false,
                    "references": [],
                },
                "snapshot_id": "",
                "ppe_env_name": "",
                "revert_enable": false,
                "exception": { "message": "", "code": 0 },
            })
        })
        .collect()
}

fn session_landed(kind: IdeKind, project_id: &str, session_id: &str) -> Result<bool, String> {
    let check = rpc(
        kind,
        "lite",
        "list_chat_sessions",
        json!({ "in_project_id": [project_id], "page_size": 100 }),
        20_000,
    )?;
    Ok(check
        .pointer("/data/data/items")
        .and_then(|a| a.as_array())
        .map(|items| {
            items
                .iter()
                .any(|it| it.get("chat_session_id").and_then(|x| x.as_str()) == Some(session_id))
        })
        .unwrap_or(false))
}

fn migrate_session(
    kind: IdeKind,
    project_id: &str,
    s: &Value,
) -> Result<Option<String>, String> {
    let Some(raw) = s.get("messages").and_then(|m| m.as_array()) else {
        return Ok(None);
    };
    let out = import_messages(raw);
    if out.is_empty() {
        return Ok(None);
    }
    let mut last_err = String::new();
    // 并发/服务端抖动都可能让 migrate 返回 success 但未落库：换新 id 重试一次
    for _ in 0..2 {
        let session_id = new_id();
        let data = json!({
            "session_id": session_id,
            "title": s.get("title").and_then(|t| t.as_str()).unwrap_or(""),
            "created_at": s.get("created_at").and_then(|t| t.as_i64()).unwrap_or(0) * 1000,
            "updated_at": s.get("updated_at").and_then(|t| t.as_i64()).unwrap_or(0) * 1000,
            "project_id": project_id,
            "messages": out.clone(),
        });
        let v = match rpc(kind, "chat", "chat_migrate", data, 60_000) {
            Ok(v) => v,
            Err(e) => {
                last_err = e;
                continue;
            }
        };
        let code = v.get("code").and_then(|c| c.as_i64()).unwrap_or(-1);
        if code != 0 {
            let msg = v.get("message").and_then(|m| m.as_str()).unwrap_or("未知错误");
            last_err = format!("chat_migrate {code}: {msg}");
            continue;
        }
        // 落库有短暂延迟：最多等 3s 再判定
        for i in 0..7 {
            if i > 0 {
                std::thread::sleep(std::time::Duration::from_millis(500));
            }
            match session_landed(kind, project_id, &session_id) {
                Ok(true) => return Ok(Some(session_id)),
                Ok(false) => last_err = "chat_migrate 未落库（目标项目中无该会话）".into(),
                Err(e) => last_err = e,
            }
        }
    }
    Err(last_err)
}

fn current_uid(kind: IdeKind) -> Option<String> {
    shim_cmd(kind, "icube.common.getUserInfo", 6_000)
        .ok()
        .and_then(|v| v.get("userId").and_then(|u| u.as_str()).map(String::from))
}

/// 把归档中其它账号的会话导入目标账号自己的 project（切号成功后调用）
pub fn import_pending(kind: IdeKind, target_uid: &str) -> Result<usize, String> {
    // 等主进程凭据切到目标账号（agent RPC 按当前 token 鉴权）
    let start = Instant::now();
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
        let workspace = root
            .get("workspace")
            .and_then(|w| w.as_str())
            .unwrap_or("")
            .to_string();
        if workspace.is_empty() {
            continue;
        }
        // 目标账号在该目录下的 project：新账号首次切换时由 IDE 的 initChatView 异步创建，
        // 轮询等待最多 20s，避免刚切过去就因项目未建好而漏导
        let mut pid = None;
        for attempt in 0..20 {
            let list = match rpc(
                kind,
                "lite",
                "list_projects_v2",
                json!({ "page_size": 100 }),
                20_000,
            ) {
                Ok(v) => v,
                Err(e) => {
                    super::logger::log(
                        super::logger::Level::Warn,
                        super::logger::Source::Switch,
                        format!("会话导入跳过（项目列表不可用）：{e}"),
                    );
                    return Ok(total);
                }
            };
            pid = list
                .pointer("/data/data/items")
                .and_then(|a| a.as_array())
                .and_then(|items| {
                    items.iter().find(|it| {
                        it.get("single_root_workspace_folder")
                            .and_then(|f| f.as_str())
                            .map(|f| same_folder(f, &workspace))
                            .unwrap_or(false)
                    })
                })
                .and_then(|it| it.get("project_id").and_then(|p| p.as_str()))
                .map(String::from);
            if pid.is_some() || attempt == 19 {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(1_000));
        }
        let Some(pid) = pid else {
            super::logger::log(
                super::logger::Level::Warn,
                super::logger::Source::Switch,
                format!("会话导入跳过：新账号尚未为 {workspace} 创建 project"),
            );
            continue;
        };

        let Some(accounts) = root.get("accounts").and_then(|a| a.as_object()) else {
            continue;
        };
        let snapshot: Vec<(String, Vec<Value>)> = accounts
            .iter()
            .filter(|(u, _)| u.as_str() != target_uid)
            .map(|(u, v)| {
                (
                    u.clone(),
                    v.get("sessions")
                        .and_then(|s| s.as_array())
                        .cloned()
                        .unwrap_or_default(),
                )
            })
            .collect();
        let mut changed = false;
        for (src_uid, sessions) in snapshot {
            for s in sessions {
                let sid = s.get("session_id").and_then(|x| x.as_str()).unwrap_or("");
                if sid.is_empty()
                    || root
                        .pointer(&format!("/accounts/{src_uid}/imports/{target_uid}"))
                        .and_then(|d| d.as_array())
                        .map(|a| a.iter().any(|x| x.as_str() == Some(sid)))
                        .unwrap_or(false)
                {
                    continue;
                }
                match migrate_session(kind, &pid, &s) {
                    Ok(Some(dst_sid)) => {
                        let done = &mut root["accounts"][src_uid.as_str()]["imports"]
                            [target_uid];
                        if !done.is_array() {
                            *done = json!([]);
                        }
                        if let Some(arr) = done.as_array_mut() {
                            arr.push(json!(sid));
                        }
                        // 登记副本：该 id 属于导入产物，下次导出源账号/切换时排除
                        let copied = &mut root["copies"][target_uid][dst_sid.as_str()];
                        *copied = json!(format!("{src_uid}:{sid}"));
                        total += 1;
                        changed = true;
                        super::logger::log(
                            super::logger::Level::Info,
                            super::logger::Source::Switch,
                            format!("会话已导入：{sid} → {dst_sid}"),
                        );
                        save_root(&path, &root);
                    }
                    Ok(None) => {}
                    Err(e) => {
                        super::logger::log(
                            super::logger::Level::Warn,
                            super::logger::Source::Switch,
                            format!("历史会话导入失败（{sid}）：{e}"),
                        );
                    }
                }
            }
        }
        if changed {
            save_root(&path, &root);
        }
    }
    Ok(total)
}
