// Tauri command 层：把 core 能力暴露给前端
use crate::core::auth::parse_auth_blob;
use crate::core::assets::AssetsClient;
use crate::core::model::{AccountBundle, AccountStatus};
use crate::core::paths::{IdeKind, IdePaths};
use crate::core::quota::{summarize_ent_usage, PackQuota, QuotaClient};
use crate::core::switch;
use crate::core::{cdp, launcher, store, vault};
use serde::Serialize;
use serde_json::{json, Value};
use tauri_plugin_updater::UpdaterExt;

type CmdResult<T> = Result<T, String>;

#[derive(Serialize)]
pub struct AccountSummary {
    pub id: String,
    pub name: String,
    pub email: String,
    pub note: String,
    pub ide: IdeKind,
    pub status: AccountStatus,
    pub user_id: String,
    /// 置顶显示（账号池排序用）
    pub pinned: bool,
    /// 该账号独立耗尽阈值（剩余百分比），None = 用全局阈值
    pub threshold: Option<f64>,
    /// 本次入库是否覆盖了已存在的同 userId 账号（仅 add/import 返回时有意义）
    pub replaced: bool,
    /// 凭据 access token 到期时刻（epoch 毫秒）；无法解析时为 None
    pub expires_at: Option<i64>,
    /// 凭据已过期（access token 到期且未续期）
    pub cred_expired: bool,
    /// 上次续期失败原因（如 20403 Token device not match）；成功/未尝试时为 None
    pub refresh_error: Option<String>,
    /// 上次续期失败是否为设备不匹配（refreshToken 绑定签发设备，换机后必现）
    pub device_mismatch: bool,
    /// 该账号是否已创建过独立实例数据目录（多开过一次）
    pub instance_exists: bool,
    /// 该账号的独立实例当前是否在运行（其切换助手通道心跳新鲜）
    pub instance_alive: bool,
}

fn to_summary(b: &AccountBundle) -> AccountSummary {
    let expires_at = switch::token_expiry_ms(&b.auth_blob);
    let cred_expired = switch::token_near_expiry(&b.auth_blob, 0);
    let device_mismatch = b
        .refresh_error
        .as_deref()
        .map(|e| e.contains("20403") || e.to_lowercase().contains("device not match"))
        .unwrap_or(false);
    // 多开实例状态：目录存在 + 该账号专属通道心跳新鲜（同 exe 名无法从进程列表区分实例）
    let inst_dir = switch::isolated_dir(&b.id);
    let instance_exists = inst_dir.is_dir();
    let instance_alive = instance_exists
        && crate::core::shim::isolated_alive(b.ide, &inst_dir, &switch::isolated_channel(&b.id));
    AccountSummary {
        id: b.id.clone(),
        name: b.name.clone(),
        email: b.email.clone(),
        note: b.note.clone(),
        ide: b.ide,
        status: b.status,
        user_id: b.user_id.clone(),
        pinned: b.pinned,
        threshold: b.threshold,
        replaced: false,
        expires_at,
        cred_expired,
        refresh_error: b.refresh_error.clone(),
        device_mismatch,
        instance_exists,
        instance_alive,
    }
}

/// 入库去重（P0-5）：同 userId 已存在 → 原地覆盖凭据与设备资产，
/// 展示字段（名称/备注/置顶）在外部未指定时沿用旧值；返回是否发生了覆盖。
fn upsert_bundle(bundles: &mut Vec<AccountBundle>, mut bundle: AccountBundle) -> bool {
    match bundles.iter().position(|b| b.id == bundle.id) {
        Some(pos) => {
            if bundle.name.trim().is_empty() {
                bundle.name = bundles[pos].name.clone();
            }
            if bundle.note.trim().is_empty() {
                bundle.note = bundles[pos].note.clone();
            }
            bundle.pinned = bundles[pos].pinned;
            if bundle.threshold.is_none() {
                bundle.threshold = bundles[pos].threshold;
            }
            bundles[pos] = bundle;
            true
        }
        None => {
            bundles.push(bundle);
            false
        }
    }
}

fn load_bundles() -> CmdResult<Vec<AccountBundle>> {
    let v = vault::load_json().map_err(err)?;
    Ok(match v {
        Some(v) => serde_json::from_value(v["accounts"].clone()).unwrap_or_default(),
        None => Vec::new(),
    })
}

fn save_bundles(bundles: &[AccountBundle]) -> CmdResult<()> {
    let v = serde_json::json!({ "accounts": bundles });
    vault::save_json(&v).map_err(err)
}

fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}

fn parse_ide(ide: &str) -> CmdResult<IdeKind> {
    match ide {
        "trae" => Ok(IdeKind::Trae),
        "trae-cn" => Ok(IdeKind::TraeCn),
        other => Err(format!("unknown ide: {other}")),
    }
}

#[tauri::command]
pub fn list_accounts() -> CmdResult<Vec<AccountSummary>> {
    Ok(load_bundles()?.iter().map(to_summary).collect())
}

/// 扫描本机当前登录态（不入库，返回待确认的账号）
#[tauri::command]
pub async fn scan_local_account(ide: String) -> CmdResult<Option<AccountSummary>> {
    tauri::async_runtime::spawn_blocking(move || scan_local_account_impl(&ide))
        .await
        .map_err(err)?
}

fn scan_local_account_impl(ide: &str) -> CmdResult<Option<AccountSummary>> {
    let kind = parse_ide(ide)?;
    Ok(switch::scan_current(&kind).map_err(err)?.as_ref().map(to_summary))
}

/// 把扫描到的账号入池（携原始加密串）
#[tauri::command]
pub async fn import_local_account(
    app: tauri::AppHandle,
    ide: String,
    name: String,
    note: String,
) -> CmdResult<AccountSummary> {
    let r = tauri::async_runtime::spawn_blocking(move || import_local_account_impl(&ide, &name, &note))
        .await
        .map_err(err)?;
    crate::tray::refresh(&app);
    r
}

fn import_local_account_impl(ide: &str, name: &str, note: &str) -> CmdResult<AccountSummary> {
    let kind = parse_ide(ide)?;
    let mut bundle = switch::scan_current(&kind)
        .map_err(err)?
        .ok_or("本机未发现登录态")?;
    bundle.name = name.to_string();
    bundle.note = note.to_string();
    let mut bundles = load_bundles()?;
    let replaced = upsert_bundle(&mut bundles, bundle.clone());
    save_bundles(&bundles)?;
    let mut summary = to_summary(&bundle);
    summary.replaced = replaced;
    Ok(summary)
}

/// 粘贴凭据入池（凭据为 auth blob 加密串原文；名称留空时自动取账号用户名/邮箱）
#[tauri::command]
pub async fn add_account(
    app: tauri::AppHandle,
    ide: String,
    name: String,
    note: String,
    auth_blob: String,
) -> CmdResult<AccountSummary> {
    let r = tauri::async_runtime::spawn_blocking(move || add_account_impl(&ide, &name, &note, &auth_blob))
        .await
        .map_err(err)?;
    crate::tray::refresh(&app);
    r
}

fn add_account_impl(ide: &str, name: &str, note: &str, auth_blob: &str) -> CmdResult<AccountSummary> {
    let kind = parse_ide(ide)?;
    // 入库即自愈：外部提取的凭据常缺登录必需字段
    let auth_blob = crate::core::auth::repair_auth_blob(kind, auth_blob).map_err(err)?;
    let info = parse_auth_blob(auth_blob.as_str()).map_err(err)?;
    let auto_name = info
        .account
        .as_ref()
        .map(|a| if a.username.is_empty() { a.email.clone() } else { a.username.clone() })
        .unwrap_or_default();
    let name = if name.trim().is_empty() { auto_name } else { name.to_string() };
    let bundle = AccountBundle {
        id: format!("{}-{}", kind as u8, info.user_id),
        name,
        email: info.account.map(|a| a.email).unwrap_or_default(),
        note: note.to_string(),
        ide: kind,
        status: AccountStatus::Normal,
        auth_blob,
        usertag_blob: None,
        ecdsa_blobs: Default::default(),
        tiny_storage: None,
        dev_device_id: None,
        sqm_id: None,
        pinned: false,
        threshold: None,
        refresh_error: None,
        user_id: info.user_id,
    };
    let mut bundles = load_bundles()?;
    let replaced = upsert_bundle(&mut bundles, bundle.clone());
    save_bundles(&bundles)?;
    let mut summary = to_summary(&bundle);
    summary.replaced = replaced;
    Ok(summary)
}

#[tauri::command]
pub fn delete_account(app: tauri::AppHandle, id: &str) -> CmdResult<()> {
    let mut bundles = load_bundles()?;
    bundles.retain(|b| b.id != id);
    save_bundles(&bundles)?;
    crate::tray::refresh(&app);
    Ok(())
}

#[tauri::command]
pub fn update_account(
    app: tauri::AppHandle,
    id: &str,
    name: &str,
    note: &str,
    threshold: Option<f64>,
) -> CmdResult<()> {
    let mut bundles = load_bundles()?;
    let b = bundles.iter_mut().find(|b| b.id == id).ok_or("账号不存在")?;
    b.name = name.to_string();
    b.note = note.to_string();
    b.threshold = threshold;
    save_bundles(&bundles)?;
    crate::tray::refresh(&app);
    Ok(())
}

/// P0-6 置顶/取消置顶（账号池排序用）
#[tauri::command]
pub fn set_account_pinned(app: tauri::AppHandle, id: &str, pinned: bool) -> CmdResult<()> {
    let mut bundles = load_bundles()?;
    let b = bundles.iter_mut().find(|b| b.id == id).ok_or("账号不存在")?;
    b.pinned = pinned;
    save_bundles(&bundles)?;
    crate::tray::refresh(&app);
    Ok(())
}

/// 续期：refreshToken 换新 token 并写回账号库（同用户时同步 storage.json）
fn do_refresh(b: &mut AccountBundle) -> CmdResult<()> {
    match crate::core::refresh::refresh_auth_blob(&b.ide, &b.auth_blob) {
        Ok(nb) => {
            b.auth_blob = nb;
            switch::sync_auth_if_same_user(&b.ide, &b.user_id, &b.auth_blob).map_err(err)?;
            b.refresh_error = None;
            Ok(())
        }
        Err(e) => {
            // 续期失败原因落到账号上（如 20403 Token device not match），供界面显式告警
            let msg = e.to_string();
            b.refresh_error = Some(msg.clone());
            Err(msg)
        }
    }
}

/// A1 切换前预检：**已移除**。
/// 原先用 `/cloudide/api/v3/trae/CheckLogin` 判"失效即阻断切换"，但该接口在 CN 上
/// 对**有效账号**同样返回空 `Result` + `IsLogin=false`（换 x-device-id 结果一致，
/// 已用正在使用中的账号实测），形同空信封 → 只会产生假阴性：
/// 额度能查、签到能做，切换却被它拦下（真机日志「CheckLogin 判定失效 … 先强制续期」）。
/// 因此切换不再做任何前置判定；失效凭据由 IDE 自身在切换后处理（续期或弹登录页）。

/// A2 账号有效性校验：改用**经验证可用**的额度接口判定，而不是 CheckLogin。
/// 返回 {valid, reason, status}；valid 为 null 表示"无法判定"（网络/服务异常，不误标状态）。
#[tauri::command]
pub async fn verify_account(id: String) -> CmdResult<String> {
    tauri::async_runtime::spawn_blocking(move || verify_account_impl(&id))
        .await
        .map_err(err)?
}

fn verify_account_impl(id: &str) -> CmdResult<String> {
    use crate::core::logger::{Level, Source};
    use crate::core::quota::QuotaError;
    let mut bundles = load_bundles()?;
    let idx = bundles.iter().position(|b| b.id == id).ok_or("账号不存在")?;
    let name = bundles[idx].name.clone();
    let ide = bundles[idx].ide;
    let info = parse_auth_blob(&bundles[idx].auth_blob).map_err(err)?;
    let client = QuotaClient::for_ide(ide, &region_of(&info));

    // 已过期 → 无需请求即可判定不可用（且续期已过可救窗口，见 query_quota 注释）
    if switch::token_near_expiry(&bundles[idx].auth_blob, 0) {
        bundles[idx].status = AccountStatus::Error;
        save_bundles(&bundles)?;
        crate::core::logger::log(Level::Warn, Source::Account, format!("{name} 校验：凭据已过期"));
        return Ok(json!({
            "valid": false, "reason": "凭据已过期，需重新登录采集",
            "status": AccountStatus::Error,
        })
        .to_string());
    }

    let (valid, reason) = match client.ent_usage(&info.token) {
        Ok(_) => (Some(true), "额度接口校验通过（凭据可用）".to_string()),
        // 服务端明确拒绝 → 凭据失效
        Err(QuotaError::Http(401 | 403, msg)) => (Some(false), format!("服务端拒绝凭据（{msg}）")),
        Err(e) => (None, format!("无法判定（{e}）")),
    };
    let status = match valid {
        Some(true) => Some(AccountStatus::Normal),
        Some(false) => Some(AccountStatus::Error),
        None => None,
    };
    if let Some(s) = status {
        bundles[idx].status = s;
        save_bundles(&bundles)?;
    }
    crate::core::logger::log(
        if valid == Some(false) { Level::Warn } else { Level::Info },
        Source::Account,
        format!("{name} 校验：{reason}"),
    );
    Ok(json!({ "valid": valid, "reason": reason, "status": status }).to_string())
}

/// 额度查询（Cloud-IDE-JWT，双版本 host）。
/// 关键：**续期必须在 access token 仍有效时发起**。CN 实测：token 过期后调
/// ExchangeToken 会被服务端以 `TokenInvalid`(040012) 拒绝（原生 IDE 也只在到期前
/// 刷新，从不"过期后再刷"）。故这里先判到期：
///   · 已过期 → 不发起无效续期，直接给出明确文案（需重新登录采集），避免把
///     续期失败的裸 401 当作额度查询错误抛给用户；
///   · 未过期但查询失败 → 续期一次重试。
#[tauri::command]
pub async fn query_quota(id: String) -> CmdResult<Vec<PackQuota>> {
    tauri::async_runtime::spawn_blocking(move || query_quota_impl(&id))
        .await
        .map_err(err)?
}

fn query_quota_impl(id: &str) -> CmdResult<Vec<PackQuota>> {
    let mut bundles = load_bundles()?;
    let idx = bundles.iter().position(|b| b.id == id).ok_or("账号不存在")?;
    let mut info = parse_auth_blob(&bundles[idx].auth_blob).map_err(err)?;
    let region = region_of(&info);
    let client = QuotaClient::for_ide(bundles[idx].ide, &region);

    if switch::token_near_expiry(&bundles[idx].auth_blob, 0) {
        crate::core::logger::log(
            crate::core::logger::Level::Warn,
            crate::core::logger::Source::Quota,
            format!("{} 凭据已过期（续期需 token 仍有效），需重新登录采集", bundles[idx].name),
        );
        return Err(format!(
            "{} 的凭据已过期，无法自愈续期 —— 请用「登录采集」重新入库该账号",
            bundles[idx].name
        ));
    }

    let usage = match client.ent_usage(&info.token) {
        Ok(v) => v,
        Err(qerr) => {
            // 未过期却失败：续期一次再试
            if let Err(rerr) = do_refresh(&mut bundles[idx]) {
                return Err(format!("额度查询失败（{qerr}）→ 续期失败：{rerr}"));
            }
            save_bundles(&bundles)?;
            info = parse_auth_blob(&bundles[idx].auth_blob).map_err(err)?;
            client
                .ent_usage(&info.token)
                .map_err(|e2| format!("额度查询失败（{qerr}）→ 续期后仍失败：{e2}"))?
        }
    };
    let packs = summarize_ent_usage(&usage);
    // P1-1 采样：剩余量（CN 积分 / 国际版每日额度），供趋势折线与耗尽预测
    crate::core::history::record(
        id,
        crate::core::quota::remain_of(&packs),
        crate::core::refresh::now_millis(),
    );
    Ok(packs)
}

/// P1-1 读取账号额度采样序列（升序）[{ts, remain}]，供趋势图与预测
#[tauri::command]
pub fn usage_history(id: String) -> CmdResult<String> {
    Ok(crate::core::history::series(&id).to_string())
}

/// 切换历史时间线：[{ts, from, to, ide, hot, ok, ms, note}]（按发生顺序，前端倒序展示）
#[tauri::command]
pub fn switch_history() -> CmdResult<String> {
    Ok(crate::core::history::switches().to_string())
}

/// 设备身份管理：列出指定版本全部账号的随号设备资产，
/// 用于核对"每号是否携带独立设备身份"（aha device_id / ECDSA 指纹 / 缺项汇总）
#[tauri::command]
pub async fn account_devices(ide: String) -> CmdResult<String> {
    tauri::async_runtime::spawn_blocking(move || {
        let kind = parse_ide(&ide)?;
        let bundles = load_bundles()?;
        let arr: Vec<Value> = bundles
            .iter()
            .filter(|b| b.ide == kind)
            .map(|b| {
                let device_id = crate::core::switch::device_id_str_of(b);
                let fingerprint = crate::core::switch::ecdsa_fingerprint(b);
                let has_tiny = b.tiny_storage.is_some();
                let has_ecdsa = !b.ecdsa_blobs.is_empty();
                let has_dev = b.dev_device_id.is_some();
                let has_sqm = b.sqm_id.is_some();
                let mut missing = Vec::new();
                if device_id.is_none() {
                    missing.push("aha 设备 id");
                }
                if !has_ecdsa {
                    missing.push("ECDSA 密钥对");
                }
                if !has_dev {
                    missing.push("devDeviceId");
                }
                if !has_sqm {
                    missing.push("sqmId");
                }
                if !has_tiny {
                    missing.push("TinyStorage 快照");
                }
                json!({
                    "id": b.id,
                    "name": b.name,
                    "deviceIdStr": device_id,
                    "ecdsaFingerprint": fingerprint,
                    "ecdsaKeyCount": b.ecdsa_blobs.len(),
                    "hasTiny": has_tiny,
                    "hasEcdsa": has_ecdsa,
                    "hasDev": has_dev,
                    "hasSqm": has_sqm,
                    "missing": missing,
                })
            })
            .collect();
        Ok(json!(arr).to_string())
    })
    .await
    .map_err(err)?
}

// ============ 会话级用量明细（agent 本地 RPC，经 workbench 补丁代理）============

/// 会话用量采样上限：每会话分页拉消息，限制会话数与页数（每页一次 RPC）
const SESSION_USAGE_MAX: usize = 6;
const SESSION_USAGE_PAGE_SIZE: i64 = 100;
const SESSION_USAGE_MAX_PAGES: usize = 4;

/// 经 workbench 补丁命令 `_tsw.chatRpc` 调用 ai-agent 本地 RPC。
/// service/method 仅由本文件的固定调用点给出（不做任意转发）；需 Trae 运行中且补丁已生效。
fn chat_rpc(kind: IdeKind, service: &str, method: &str, data: Value) -> CmdResult<Value> {
    let r = shim_request(
        kind,
        "invoke",
        json!({
            "command": "_tsw.chatRpc",
            "args": [{ "service": service, "method": method, "data": data }],
            "timeout_ms": 8000,
        }),
        12_000,
    )?;
    let v = r.get("value").cloned().unwrap_or(Value::Null);
    if v.get("ok").and_then(|b| b.as_bool()) != Some(true) {
        let msg = v
            .get("error")
            .and_then(|e| e.as_str())
            .unwrap_or("agent RPC 失败");
        // 补丁未生效时的典型回执：命令不存在 / shim 超时未响应
        if msg.contains("not found") || msg.contains("未响应") {
            return Err(format!("{msg}（需先重新安装无感换号支持并重启 Trae 才能启用会话用量）"));
        }
        return Err(msg.to_string());
    }
    let raw = v.get("json").and_then(|s| s.as_str()).unwrap_or("null");
    serde_json::from_str(raw).map_err(|e| format!("agent 响应解析失败：{e}"))
}

/// 取当前窗口的 project_id（agent 的 chat 系列接口要求 data 携带该字段）。
/// 来源 `icube.common.commands.tooling.getAllOpenedProjectsV2` → data.ide_windows[0].project_id
/// （真机实测：该值即账号级共享 project，补丁⑦固定后所有账号共用同一 project）。
fn current_project_id(kind: IdeKind) -> CmdResult<String> {
    let r = shim_request(
        kind,
        "invoke",
        json!({ "command": "icube.common.commands.tooling.getAllOpenedProjectsV2", "args": [], "timeout_ms": 6000 }),
        8_000,
    )?;
    let v = r.get("value").cloned().unwrap_or(Value::Null);
    v.pointer("/data/ide_windows")
        .and_then(|w| w.as_array())
        .and_then(|w| w.first())
        .and_then(|w| w.get("project_id"))
        .and_then(|p| p.as_str())
        .filter(|s| !s.is_empty())
        .map(String::from)
        .ok_or_else(|| "未取到 project_id（Trae 未打开工作区窗口？）".to_string())
}

/// 会话级用量明细：会话列表 → 逐会话消息 → 聚合 token 用量。
/// 数据源即 ai-modules-chat 自身的调用路径（get_sessions / get_messages），
/// 消息体 token_usage 字段形态来自真机实测（prompt_tokens/total_tokens/cache_* 等均为整数）。
#[tauri::command]
pub async fn session_usage(ide: String) -> CmdResult<String> {
    tauri::async_runtime::spawn_blocking(move || session_usage_impl(&ide))
        .await
        .map_err(err)?
}

fn session_usage_impl(ide: &str) -> CmdResult<String> {
    let kind = parse_ide(ide)?;
    let project_id = current_project_id(kind)?;
    let list = chat_rpc(
        kind,
        "chat",
        "get_sessions",
        json!({ "project_id": project_id, "limit": 30, "offset": 0 }),
    )?;
    let mut sessions = list
        .pointer("/data/sessions")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let total = list.pointer("/data/total_count").and_then(|v| v.as_i64());
    // 按更新时间倒序取最近 N 个（服务端顺序未在协议中约定）
    sessions.sort_by_key(|s| {
        -s.get("update_at")
            .or_else(|| s.get("created_at"))
            .and_then(|v| v.as_i64())
            .unwrap_or(0)
    });

    let mut rows = Vec::new();
    let mut errors = Vec::new();
    // 按模型累计（消息体 model 字段名在不同版本间有差异，按优先级兜底取值）
    let mut models: std::collections::BTreeMap<String, ModelAgg> = std::collections::BTreeMap::new();
    for s in sessions.iter().take(SESSION_USAGE_MAX) {
        let sid = s.get("session_id").and_then(|v| v.as_str()).unwrap_or("");
        if sid.is_empty() {
            continue;
        }
        let title = s
            .get("title")
            .or_else(|| s.get("name"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let updated = s
            .get("update_at")
            .or_else(|| s.get("created_at"))
            .and_then(|v| v.as_i64())
            .map(|t| if t < 100_000_000_000 { t * 1000 } else { t })
            .unwrap_or(0);
        let mut agg = SessionUsage::default();
        agg.session_id = sid.to_string();
        agg.title = title;
        agg.updated_at = updated;
        // 分页拉全量消息：get_messages 首页从最早开始，单页会把长会话的后期轮次漏掉
        let mut page_token: Option<String> = None;
        let mut pages = 0usize;
        let mut failed = false;
        loop {
            let mut req = json!({
                "project_id": project_id,
                "session_id": sid,
                "page_size": SESSION_USAGE_PAGE_SIZE,
            });
            if let Some(t) = &page_token {
                req["next_page_token"] = json!(t);
            }
            let msgs = match chat_rpc(kind, "chat", "get_messages", req) {
                Ok(v) => v,
                Err(e) => {
                    errors.push(json!({ "sessionId": sid, "error": e }));
                    failed = true;
                    break;
                }
            };
            let list = msgs
                .pointer("/data/messages")
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default();
            agg.message_count += list.len();
            let mut used = 0usize;
            for m in &list {
                let usage = m.get("token_usage").or_else(|| m.get("tokenUsage"));
                let Some(u) = usage else { continue };
                let pick = |keys: &[&str]| -> f64 {
                    for k in keys {
                        if let Some(n) = u.get(k).and_then(|v| v.as_f64()) {
                            return n;
                        }
                    }
                    0.0
                };
                let prompt = pick(&["prompt_tokens", "input"]);
                let completion = pick(&["completion_tokens", "output"]);
                let total = pick(&["total_tokens"]).max(prompt + completion);
                agg.prompt += prompt;
                agg.completion += completion;
                agg.total += total;
                agg.cache_read += pick(&["cache_read_input_tokens"]);
                agg.cache_create += pick(&["cache_creation_input_tokens"]);
                agg.reasoning += pick(&["reasoning_tokens"]);
                agg.turns += 1;
                used += 1;
                // 模型名：真实字段在 user_message_context.model_info（真机日志核对），
                // 助手轮次另有 model_smart_selection_meta.config_name；均无则归入“未知模型”
                let model = m
                    .pointer("/user_message_context/model_info/display_model_name")
                    .or_else(|| m.pointer("/user_message_context/model_info/model_name"))
                    .or_else(|| m.pointer("/model_smart_selection_meta/config_name"))
                    .or_else(|| m.pointer("/model_smart_selection_meta/display_model_name"))
                    .or_else(|| m.get("model_name"))
                    .or_else(|| m.get("modelName"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let e = models.entry(model.to_string()).or_default();
                e.turns += 1;
                e.prompt += prompt;
                e.completion += completion;
                e.total += total;
                let last_total = pick(&["last_turn_total_tokens"]);
                if last_total > 0.0 {
                    agg.last_turn_total = last_total;
                }
                let max_tokens = pick(&["max_tokens"]);
                if max_tokens > 0.0 {
                    agg.max_tokens = max_tokens;
                }
            }
            // 该页无用量字段且已是最后一页 → 提前收敛，避免空转
            page_token = msgs
                .pointer("/data/next_page_token")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .map(String::from);
            pages += 1;
            if page_token.is_none() || pages >= SESSION_USAGE_MAX_PAGES || (used == 0 && list.is_empty()) {
                break;
            }
        }
        agg.truncated = page_token.is_some();
        if failed && agg.turns == 0 {
            continue;
        }
        rows.push(agg.to_json());
    }

    crate::core::logger::log(
        crate::core::logger::Level::Info,
        crate::core::logger::Source::Quota,
        format!("会话用量：已聚合 {} 个会话（共 {} 个）", rows.len(), total.unwrap_or(0)),
    );
    let mut model_rows: Vec<Value> = models
        .into_iter()
        .map(|(name, a)| {
            json!({
                "name": if name.is_empty() { "未知模型" } else { name.as_str() },
                "turns": a.turns,
                "promptTokens": a.prompt,
                "completionTokens": a.completion,
                "totalTokens": a.total,
            })
        })
        .collect();
    model_rows.sort_by(|a, b| {
        b["totalTokens"]
            .as_f64()
            .unwrap_or(0.0)
            .partial_cmp(&a["totalTokens"].as_f64().unwrap_or(0.0))
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    Ok(json!({
        "sessions": rows,
        "totalCount": total.unwrap_or(0),
        "projectId": project_id,
        "models": model_rows,
        "errors": errors,
    })
    .to_string())
}

/// 单模型用量累计（按模型汇总本轮拉取到的全部会话）
#[derive(Default)]
struct ModelAgg {
    turns: usize,
    prompt: f64,
    completion: f64,
    total: f64,
}

/// 单会话用量聚合中间态
#[derive(Default)]
struct SessionUsage {
    session_id: String,
    title: String,
    updated_at: i64,
    turns: usize,
    message_count: usize,
    prompt: f64,
    completion: f64,
    total: f64,
    cache_read: f64,
    cache_create: f64,
    reasoning: f64,
    last_turn_total: f64,
    max_tokens: f64,
    /// 分页在达到页数上限时仍有后续页 → 本轮统计不完整
    truncated: bool,
}

impl SessionUsage {
    fn to_json(&self) -> Value {
        json!({
            "sessionId": self.session_id,
            "title": self.title,
            "updatedAt": self.updated_at,
            "turns": self.turns,
            "messageCount": self.message_count,
            "promptTokens": self.prompt,
            "completionTokens": self.completion,
            "totalTokens": self.total,
            "cacheReadTokens": self.cache_read,
            "cacheCreationTokens": self.cache_create,
            "reasoningTokens": self.reasoning,
            "lastTurnTotalTokens": self.last_turn_total,
            "maxTokens": self.max_tokens,
            "truncated": self.truncated,
        })
    }
}

/// 多开：为该账号准备独立 user-data 目录并以第二个实例启动
/// （绕过单实例锁，两个账号可同时在线；不带 --inspect，不参与热换号通道）
#[tauri::command]
pub async fn launch_isolated_instance(id: String) -> CmdResult<String> {
    tauri::async_runtime::spawn_blocking(move || {
        use crate::core::logger::{Level, Source};
        let bundles = load_bundles()?;
        let b = bundles.into_iter().find(|b| b.id == id).ok_or("账号不存在")?;
        let paths = IdePaths::detect(b.ide).ok_or("未检测到安装")?;
        if !paths.exe().exists() {
            return Err("未找到 Trae 可执行文件".to_string());
        }
        let dir = switch::prepare_isolated_data(&b.ide, &b).map_err(err)?;
        // 多开实例装独立通道的切换助手：否则两实例抢同一触发文件，实例内无法无感换号
        let channel = switch::isolated_channel(&b.id);
        if let Err(e) = crate::core::shim::install_isolated(b.ide, &dir, &channel) {
            crate::core::logger::log(
                Level::Warn,
                Source::Switch,
                format!("多开实例切换助手安装失败（该实例暂不支持无感换号）：{e}"),
            );
        }
        let pid = launcher::launch_isolated(&paths, &dir).map_err(err)?;
        crate::core::logger::log(
            Level::Info,
            Source::Switch,
            format!("多开实例已启动：{}（pid {pid}，数据目录 {}）", b.name, dir.display()),
        );
        Ok(json!({ "pid": pid, "dir": dir.display().to_string(), "name": b.name }).to_string())
    })
    .await
    .map_err(err)?
}

/// 在多开实例内无感换号（走该实例的独立切换助手通道；实例不带 --inspect，无 CDP 兜底）
#[tauri::command]
pub async fn switch_isolated(id: String) -> CmdResult<String> {
    tauri::async_runtime::spawn_blocking(move || {
        let bundles = load_bundles()?;
        let b = bundles.into_iter().find(|b| b.id == id).ok_or("账号不存在")?;
        switch::hot_switch_isolated(&b.ide, &b, &b.id)
    })
    .await
    .map_err(err)?
}

fn region_of(info: &crate::core::auth::AuthInfo) -> String {
    info.user_region
        .as_ref()
        .map(|r| r.region.clone())
        .unwrap_or_default()
}

#[tauri::command]
pub async fn refresh_account(id: String) -> CmdResult<AccountSummary> {
    tauri::async_runtime::spawn_blocking(move || refresh_account_impl(&id))
        .await
        .map_err(err)?
}

fn refresh_account_impl(id: &str) -> CmdResult<AccountSummary> {
    let mut bundles = load_bundles()?;
    let idx = bundles.iter().position(|b| b.id == id).ok_or("账号不存在")?;
    let r = do_refresh(&mut bundles[idx]);
    // 无论成功失败都落盘：失败时把续期失败原因写回账号（否则界面无从得知）
    save_bundles(&bundles)?;
    r?;
    Ok(to_summary(&bundles[idx]))
}

/// 签到设备 id：服务端对 `checkin_credits/*` 只按 **x-device-id 字符串** 记"设备当日已签到"
/// （实测：同一账号换 id 后 did_checked_in 立刻翻回 false，且任意格式字符串均被 code:0 接受）。
/// 本机 aha 设备被任一账号占用后，其余账号一律被 9095「当前设备今日已经签到」拒绝，
/// 派生式见 core::assets::device_slot，账号之间互不挤占。
fn checkin_device_id(b: &crate::core::model::AccountBundle) -> String {
    crate::core::assets::device_slot(&b.user_id)
}

/// CN 签到状态：{enable, checked_in, did_checked_in, credits}
#[tauri::command]
pub async fn query_checkin(id: String) -> CmdResult<String> {
    tauri::async_runtime::spawn_blocking(move || query_checkin_impl(&id))
        .await
        .map_err(err)?
}

fn query_checkin_impl(id: &str) -> CmdResult<String> {
    let bundles = load_bundles()?;
    let b = bundles.iter().find(|b| b.id == id).ok_or("账号不存在")?;
    let info = parse_auth_blob(&b.auth_blob).map_err(err)?;
    let region = region_of(&info);
    let client = QuotaClient::for_ide(b.ide, &region);
    let did = checkin_device_id(b);
    let v = client.checkin_status(&info.token, Some(&did)).map_err(err)?;
    Ok(serde_json::to_string(&v).map_err(err)?)
}

/// CN 一键签到（x-device-id = 该账号专属设备槽，逐账号独立）
#[tauri::command]
pub async fn claim_checkin(id: String) -> CmdResult<String> {
    tauri::async_runtime::spawn_blocking(move || claim_checkin_impl(&id))
        .await
        .map_err(err)?
}

fn claim_checkin_impl(id: &str) -> CmdResult<String> {
    let bundles = load_bundles()?;
    let b = bundles.iter().find(|b| b.id == id).ok_or("账号不存在")?;
    let info = parse_auth_blob(&b.auth_blob).map_err(err)?;
    let region = region_of(&info);
    let client = QuotaClient::for_ide(b.ide, &region);
    let did = checkin_device_id(b);
    let v = client.checkin_claim(&info.token, Some(&did)).map_err(err)?;
    crate::core::logger::log(
        crate::core::logger::Level::Info,
        crate::core::logger::Source::Quota,
        format!("{} 签到成功（设备槽 {}）", b.name, did),
    );
    Ok(serde_json::to_string(&v).map_err(err)?)
}

/// 采集本机当前登录账号的最新资产回写账号库（凭据轮换/ECDSA/设备快照保持同步）
fn capture_current_assets(bundles: &mut [AccountBundle]) {
    for kind in [IdeKind::Trae, IdeKind::TraeCn] {
        let Ok(Some(cur)) = switch::scan_current(&kind) else { continue };
        let id = format!("{}-{}", kind as u8, cur.user_id);
        if let Some(b) = bundles.iter_mut().find(|b| b.id == id) {
            b.auth_blob = cur.auth_blob;
            b.usertag_blob = cur.usertag_blob;
            b.ecdsa_blobs = cur.ecdsa_blobs;
            b.tiny_storage = cur.tiny_storage;
            b.dev_device_id = cur.dev_device_id;
            b.sqm_id = cur.sqm_id;
        }
    }
}

/// P0-3 切换前可用性预检：已过期直接拒绝；未过期时探一次额度接口，
/// 401/403（服务端明确拒绝凭据）拒绝切换；其余错误（网络/服务异常）放行，避免误拦可用账号。
fn precheck_target(b: &AccountBundle) -> CmdResult<()> {
    use crate::core::logger::{Level, Source};
    if switch::token_near_expiry(&b.auth_blob, 0) {
        return Err(format!(
            "{} 的凭据已过期，无法切换 —— 请用「登录采集」重新入库该账号",
            b.name
        ));
    }
    let info = parse_auth_blob(&b.auth_blob).map_err(err)?;
    let client = QuotaClient::for_ide(b.ide, &region_of(&info));
    match client.ent_usage(&info.token) {
        Ok(_) => Ok(()),
        Err(crate::core::quota::QuotaError::Http(401 | 403, msg)) => Err(format!(
            "{} 的凭据被服务端拒绝（{msg}），需重新登录采集后再切换",
            b.name
        )),
        Err(e) => {
            crate::core::logger::log(
                Level::Warn,
                Source::Switch,
                format!("{} 切换前检查未通过但非鉴权问题（{e}），继续切换", b.name),
            );
            Ok(())
        }
    }
}

/// 换号：hot=true 走 shim 主通道（CDP 兜底）热换号，否则冷换号（写文件）
#[tauri::command]
pub async fn switch_account(app: tauri::AppHandle, id: String, hot: bool) -> CmdResult<String> {
    let r = tauri::async_runtime::spawn_blocking(move || switch_account_impl(&id, hot))
        .await
        .map_err(err)?;
    crate::tray::refresh(&app);
    r
}

fn switch_account_impl(id: &str, hot: bool) -> CmdResult<String> {
    let mut bundles = load_bundles()?;
    let idx = bundles.iter().position(|b| b.id == id).ok_or("账号不存在")?;
    // ① 目标 token 临期（<5min）预刷新；失败不阻断（切换后 IDE 内 30s 仍会主动续期）
    if switch::token_near_expiry(&bundles[idx].auth_blob, 300_000) {
        if let Err(e) = do_refresh(&mut bundles[idx]) {
            crate::core::logger::log(
                crate::core::logger::Level::Warn,
                crate::core::logger::Source::Switch,
                format!("临期预刷新失败，携带现有凭据继续: {e}"),
            );
        }
    }
    // ①' 凭据自愈：外部提取的 blob 缺 storeRegion/userTag 等必需字段会导致切换后停在登录页
    match crate::core::auth::repair_auth_blob(bundles[idx].ide, &bundles[idx].auth_blob) {
        Ok(repaired) if repaired != bundles[idx].auth_blob => {
            bundles[idx].auth_blob = repaired;
            crate::core::logger::log(
                crate::core::logger::Level::Info,
                crate::core::logger::Source::Switch,
                format!("已修复 {} 的残缺凭据（补齐登录必需字段）", bundles[idx].name),
            );
        }
        Ok(_) => {}
        Err(e) => {
            crate::core::logger::log(
                crate::core::logger::Level::Warn,
                crate::core::logger::Source::Switch,
                format!("凭据修复失败: {e}"),
            );
        }
    }
    // ② 当前登录账号最新资产回写，防覆盖丢失
    capture_current_assets(&mut bundles);
    let b = bundles[idx].clone();
    let kind = b.ide;
    let paths = IdePaths::detect(kind).ok_or("未检测到安装")?;
    // ③ 切换前预检（P0-3）：过期/被服务端拒绝的账号不切，避免切过去立刻掉登录
    if let Err(e) = precheck_target(&b) {
        crate::core::history::record_switch("", &b.name, kind.label(), hot, false, 0, &e);
        return Err(e);
    }
    // ④ 记录切换前的生效账号：供热切换失败回滚，并写入切换历史时间线
    let prev = switch::scan_current(&kind).ok().flatten().filter(|p| p.id != id);
    let from_name = prev.as_ref().map(|p| p.name.clone()).unwrap_or_default();
    let t0 = std::time::Instant::now();
    let msg = if hot {
        match switch::hot_switch_full(&kind, &b, id) {
            Ok(m) => {
                crate::core::history::record_switch(
                    &from_name,
                    &b.name,
                    kind.label(),
                    true,
                    true,
                    t0.elapsed().as_millis() as u64,
                    &m,
                );
                m
            }
            Err(e) => {
                // P0-3 热切换中途失败可能已改坏内存态 → 尝试切回原账号
                if let Some(p) = &prev {
                    match switch::hot_switch_full(&kind, p, &p.id) {
                        Ok(_) => {
                            crate::core::logger::log(
                                crate::core::logger::Level::Warn,
                                crate::core::logger::Source::Switch,
                                format!("无感换号到 {} 失败，已自动还原到 {}", b.name, p.name),
                            );
                            crate::core::history::record_switch(
                                &from_name,
                                &b.name,
                                kind.label(),
                                true,
                                false,
                                t0.elapsed().as_millis() as u64,
                                &format!("{e}（已自动还原到 {}）", p.name),
                            );
                            return Err(format!("切换到 {} 失败：{e}（已自动还原到 {}）", b.name, p.name));
                        }
                        Err(e2) => {
                            crate::core::logger::log(
                                crate::core::logger::Level::Err,
                                crate::core::logger::Source::Switch,
                                format!("无感换号失败且自动还原失败：{e} / {e2}"),
                            );
                            crate::core::history::record_switch(
                                &from_name,
                                &b.name,
                                kind.label(),
                                true,
                                false,
                                t0.elapsed().as_millis() as u64,
                                &format!("{e}；自动还原 {} 也失败：{e2}", p.name),
                            );
                            return Err(format!("切换到 {} 失败：{e}；自动还原到 {} 也失败：{e2}（建议重启切换）", b.name, p.name));
                        }
                    }
                }
                crate::core::history::record_switch(
                    &from_name,
                    &b.name,
                    kind.label(),
                    true,
                    false,
                    t0.elapsed().as_millis() as u64,
                    &e,
                );
                return Err(e);
            }
        }
    } else {
        if launcher::is_running(&paths) {
            let e = "Trae 正在运行：重启切换会被内存回写覆盖，请改用无感换号或先退出 Trae".to_string();
            crate::core::history::record_switch(&from_name, &b.name, kind.label(), false, false, 0, &e);
            return Err(e);
        }
        switch::cold_switch(&kind, &b).map_err(err)?;
        crate::core::vault::set_current_id(id);
        crate::core::logger::log(
            crate::core::logger::Level::Info,
            crate::core::logger::Source::Switch,
            format!("重启切换到 {}（下次启动生效）", b.name),
        );
        crate::core::history::record_switch(
            &from_name,
            &b.name,
            kind.label(),
            false,
            true,
            t0.elapsed().as_millis() as u64,
            "重启切换（下次启动生效）",
        );
        "重启切换完成（下次启动生效）".into()
    };
    save_bundles(&bundles)?;
    Ok(msg)
}

/// 冷切换兜底：关闭运行中的 Trae → 写登录态 → 以 --inspect 重启
/// （Trae 运行中直接写 storage.json 会被内存回写覆盖，必须先关闭）
#[tauri::command]
pub async fn cold_switch_restart(app: tauri::AppHandle, id: String) -> CmdResult<String> {
    let r = tauri::async_runtime::spawn_blocking(move || cold_switch_restart_impl(&id))
        .await
        .map_err(err)?;
    crate::tray::refresh(&app);
    r
}

fn cold_switch_restart_impl(id: &str) -> CmdResult<String> {
    let mut bundles = load_bundles()?;
    let idx = bundles.iter().position(|b| b.id == id).ok_or("账号不存在")?;
    if switch::token_near_expiry(&bundles[idx].auth_blob, 300_000) {
        let _ = do_refresh(&mut bundles[idx]);
    }
    capture_current_assets(&mut bundles);
    let b = bundles[idx].clone();
    let paths = IdePaths::detect(b.ide).ok_or("未检测到安装")?;
    // P0-3 预检：失效账号不必先关掉 Trae 再切
    if let Err(e) = precheck_target(&b) {
        crate::core::history::record_switch("", &b.name, b.ide.label(), false, false, 0, &e);
        return Err(e);
    }
    let from_name = switch::scan_current(&b.ide).ok().flatten().filter(|p| p.id != id).map(|p| p.name);
    let t0 = std::time::Instant::now();
    let exe_name = paths.exe().file_name().and_then(|n| n.to_str()).unwrap_or("").to_string();
    // ① 关闭 Trae（含子进程树）
    let _ = std::process::Command::new("taskkill")
        .args(["/IM", &exe_name, "/F", "/T"])
        .output();
    for _ in 0..20 {
        if !launcher::is_running(&paths) {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(250));
    }
    // ② 写登录态（安全写入顺序，备份在 cold_switch 内完成）
    switch::cold_switch(&b.ide, &b).map_err(err)?;
    // ③ 以 --inspect 重启：本次登录生效 + 打通后续热换号 CDP 通道
    launcher::launch_with_inspect(&paths, launcher::inspect_port(b.ide)).map_err(err)?;
    crate::core::vault::set_current_id(id);
    crate::core::logger::log(
        crate::core::logger::Level::Info,
        crate::core::logger::Source::Switch,
        format!("重启切换到 {}（已重启 Trae）", b.name),
    );
    crate::core::history::record_switch(
        from_name.as_deref().unwrap_or(""),
        &b.name,
        b.ide.label(),
        false,
        true,
        t0.elapsed().as_millis() as u64,
        "重启切换并重启 Trae",
    );
    save_bundles(&bundles)?;
    Ok("已重启 Trae 并切换登录态".into())
}

/// 注入热换号补丁（__sw 桥 + TSW 三补丁链；Trae 须未运行，需管理员权限写安装目录）
#[tauri::command]
pub async fn inject_bridge(ide: String) -> CmdResult<String> {
    tauri::async_runtime::spawn_blocking(move || inject_bridge_impl(&ide))
        .await
        .map_err(err)?
}

fn inject_bridge_impl(ide: &str) -> CmdResult<String> {
    let kind = parse_ide(ide)?;
    let paths = IdePaths::detect(kind).ok_or("未检测到安装")?;
    match crate::core::inject::patch_all(&paths) {
        Ok(msg) => Ok(msg),
        Err(e) if e.raw_os_error() == Some(5) => {
            Err("无写入权限：安装无感换号支持需管理员身份，请以管理员身份运行 TraeIDE-Switcher 后重试".into())
        }
        Err(e) => Err(e.to_string()),
    }
}

#[tauri::command]
pub async fn remove_bridge(ide: String) -> CmdResult<String> {
    tauri::async_runtime::spawn_blocking(move || remove_bridge_impl(&ide))
        .await
        .map_err(err)?
}

fn remove_bridge_impl(ide: &str) -> CmdResult<String> {
    let kind = parse_ide(ide)?;
    let paths = IdePaths::detect(kind).ok_or("未检测到安装")?;
    match crate::core::inject::unpatch_all(&paths) {
        Ok(msg) => Ok(msg),
        Err(e) if e.raw_os_error() == Some(5) => {
            Err("无写入权限：请以管理员身份运行 TraeIDE-Switcher 后重试".into())
        }
        Err(e) => Err(e.to_string()),
    }
}

#[tauri::command]
pub fn bridge_status(ide: &str) -> CmdResult<bool> {
    let kind = parse_ide(ide)?;
    let paths = IdePaths::detect(kind).ok_or("未检测到安装")?;
    // 严格判定 TSW 六补丁：__sw 桥只服务 CDP 兜底通道，不代表 shim 主通道可用
    Ok(crate::core::inject::is_tsw_patched(&paths))
}

// ============ shim 扩展（热切换主通道）============

#[tauri::command]
pub fn shim_install(ide: &str) -> CmdResult<String> {
    let kind = parse_ide(ide)?;
    crate::core::shim::install(kind).map_err(err)
}

#[tauri::command]
pub fn shim_uninstall(ide: &str) -> CmdResult<String> {
    let kind = parse_ide(ide)?;
    crate::core::shim::uninstall(kind).map_err(err)
}

#[tauri::command]
pub fn shim_status(ide: &str) -> CmdResult<crate::core::shim::ShimStatus> {
    let kind = parse_ide(ide)?;
    Ok(crate::core::shim::status(kind))
}

/// 开始登录采集：返回 {url, port}，前端用内嵌窗打开 url
#[tauri::command]
pub async fn start_login(ide: String) -> CmdResult<String> {
    tauri::async_runtime::spawn_blocking(move || start_login_impl(&ide))
        .await
        .map_err(err)?
}

fn start_login_impl(ide: &str) -> CmdResult<String> {
    let kind = parse_ide(ide)?;
    let (url, port) = crate::core::login::start(kind).map_err(err)?;
    Ok(json!({ "url": url, "port": port }).to_string())
}

/// 取消登录采集（关闭对话框/取消按钮时调用；幂等）
/// 必须显式调用：只关前端窗不通知后端会留下僵尸回调服务器，导致二次采集被占用
#[tauri::command]
pub fn cancel_login() -> CmdResult<()> {
    crate::core::login::cancel();
    Ok(())
}

/// 轮询登录捕获结果：{status: pending|done|error, authBlob?, name?, email?, message?}
#[tauri::command]
pub fn poll_login() -> CmdResult<String> {
    Ok(match crate::core::login::poll() {
        None => json!({ "status": "pending" }).to_string(),
        Some(Ok(blob)) => {
            let info = parse_auth_blob(&blob).ok();
            let (name, email) = info
                .and_then(|i| i.account)
                .map(|a| {
                    let name = if a.username.is_empty() { a.email.clone() } else { a.username.clone() };
                    (name, a.email)
                })
                .unwrap_or_default();
            json!({ "status": "done", "authBlob": blob, "name": name, "email": email }).to_string()
        }
        Some(Err(message)) => json!({ "status": "error", "message": message }).to_string(),
    })
}

#[tauri::command]
pub fn launch_ide(ide: &str) -> CmdResult<u32> {
    let kind = parse_ide(ide)?;
    let paths = IdePaths::detect(kind).ok_or("未检测到安装")?;
    launcher::launch_with_inspect(&paths, launcher::inspect_port(kind)).map_err(err)
}

#[tauri::command]
pub async fn ide_status(ide: String) -> CmdResult<String> {
    tauri::async_runtime::spawn_blocking(move || ide_status_impl(&ide))
        .await
        .map_err(err)?
}

fn ide_status_impl(ide: &str) -> CmdResult<String> {
    let kind = parse_ide(ide)?;
    match IdePaths::detect(kind) {
        Some(p) => {
            // 一次 Toolhelp 遍历拿到运行状态 + 主进程启动时刻（避免 fork tasklist）
            let (running, started_ms) = launcher::state(&p);
            let cdp_ready = running && cdp::list_targets(launcher::inspect_port(kind)).is_ok();
            let tsw_patched = crate::core::inject::is_tsw_patched(&p);
            let shim_alive = crate::core::shim::status(kind).alive;
            // 登录态与"当前生效账号"按该 IDE 自身 storage.json 判定（两版本互不干扰）
            let cur = switch::scan_current(&kind).ok().flatten();
            let login = cur.is_some();
            let current_id = cur.map(|b| b.id).unwrap_or_default();
            // 磁盘补丁晚于运行实例启动 = 内存仍是旧 bundle（补丁只在启动时加载）
            let stale_patch = running
                && tsw_patched
                && match (crate::core::inject::patch_mtime_ms(&p), started_ms) {
                    (Some(pm), Some(sm)) => pm > sm,
                    _ => false,
                };
            // 会话/任务记录归属补丁（workbench + ai-modules-chat）
            let chat_pinned = crate::core::inject::is_chat_pinned(&p);
            // 必须走 serde_json：install 路径含反斜杠，手拼 JSON 会产生非法转义（\d），
            // 前端 JSON.parse 抛错 → 整个状态（含登录态/当前账号）丢失
            Ok(json!({
                "installed": true,
                "install": p.install_dir.to_string_lossy(),
                "running": running,
                "cdpReady": cdp_ready,
                "tswPatched": tsw_patched,
                "chatPinned": chat_pinned,
                "shimAlive": shim_alive,
                "loggedIn": login,
                "currentId": current_id,
                "stalePatch": stale_patch,
            })
            .to_string())
        }
        None => Ok(r#"{"installed":false,"currentId":""}"#.into()),
    }
}

#[tauri::command]
pub fn registry_deviceid() -> CmdResult<String> {
    Ok(store::read_registry_deviceid().unwrap_or_default())
}

/// 两个版本各自实际生效的安装路径（手填优先 → 注册表探测），设置页展示用
#[tauri::command]
pub fn install_paths() -> CmdResult<String> {
    let one = |k: IdeKind| {
        IdePaths::detect(k)
            .map(|p| p.install_dir.to_string_lossy().to_string())
            .unwrap_or_default()
    };
    Ok(json!({ "trae": one(IdeKind::Trae), "trae-cn": one(IdeKind::TraeCn) }).to_string())
}

#[tauri::command]
pub fn autostart(enable: bool) -> CmdResult<()> {
    use winreg::enums::HKEY_CURRENT_USER;
    use winreg::RegKey;
    let run = RegKey::predef(HKEY_CURRENT_USER)
        .open_subkey_with_flags(
            r"Software\Microsoft\Windows\CurrentVersion\Run",
            winreg::enums::KEY_SET_VALUE,
        )
        .map_err(err)?;
    if enable {
        let exe = std::env::current_exe().map_err(err)?;
        run.set_value("TraeIDE-Switcher", &exe.display().to_string()).map_err(err)?;
    } else {
        let _ = run.delete_value("TraeIDE-Switcher");
    }
    crate::core::logger::log(
        crate::core::logger::Level::Info,
        crate::core::logger::Source::System,
        if enable { "已开启开机自启" } else { "已关闭开机自启" },
    );
    Ok(())
}

// ============ 日志 ============

#[tauri::command]
pub fn get_logs() -> CmdResult<Vec<crate::core::logger::LogEntry>> {
    Ok(crate::core::logger::recent(1000))
}

#[tauri::command]
pub fn export_logs() -> CmdResult<String> {
    let path = crate::core::logger::export().map_err(err)?;
    Ok(path.display().to_string())
}

// ============ 设置 ============

#[tauri::command]
pub fn get_settings() -> CmdResult<crate::core::settings::AppSettings> {
    Ok(crate::core::settings::load())
}

#[tauri::command]
pub fn update_settings(patch: serde_json::Value) -> CmdResult<crate::core::settings::AppSettings> {
    let mut s = crate::core::settings::load();
    if let Some(obj) = patch.as_object() {
        let mut cur = serde_json::to_value(&s).unwrap();
        if let Some(cur_obj) = cur.as_object_mut() {
            for (k, v) in obj {
                cur_obj.insert(k.clone(), v.clone());
            }
        }
        if let Ok(next) = serde_json::from_value::<crate::core::settings::AppSettings>(cur) {
            s = next;
        }
    }
    crate::core::settings::save(&s).map_err(err)?;
    crate::core::logger::log(crate::core::logger::Level::Info, crate::core::logger::Source::System, "设置已更新");
    Ok(s)
}

// ============ 应用内更新（走设置中的代理与自定义更新源）============

/// 检查 / 下载并安装应用更新。`install=false` 只检查并返回元信息；
/// `install=true` 直接下载并安装（Windows 下安装器启动后本进程退出）。
/// 更新源与代理均取自设置：update_endpoint 为空用内置默认端点，http_proxy 为空则直连。
#[tauri::command]
pub async fn app_update(app: tauri::AppHandle, install: bool) -> CmdResult<String> {
    use tauri::Emitter;
    let s = crate::core::settings::load();
    let mut builder = app.updater_builder();
    let ep = s.update_endpoint.trim();
    if !ep.is_empty() {
        let url = url::Url::parse(ep).map_err(|e| format!("更新源地址无效：{e}"))?;
        builder = builder.endpoints(vec![url]).map_err(err)?;
    }
    let proxy = s.http_proxy.trim();
    if !proxy.is_empty() {
        let p = url::Url::parse(proxy).map_err(|e| format!("代理地址无效：{e}"))?;
        builder = builder.proxy(p);
    }
    let updater = builder.build().map_err(err)?;
    let update = updater.check().await.map_err(err)?;
    let Some(update) = update else {
        return Ok(json!({ "available": false }).to_string());
    };
    if !install {
        return Ok(json!({
            "available": true,
            "version": update.version.to_string(),
            "date": update.date.map(|d| d.to_string()),
            "body": update.body,
        })
        .to_string());
    }
    let handle = app.clone();
    update
        .download_and_install(
            move |chunk, total| {
                let _ = handle.emit(
                    "update-progress",
                    json!({ "chunk": chunk, "total": total }),
                );
            },
            || {},
        )
        .await
        .map_err(err)?;
    Ok(json!({ "available": true, "installed": true }).to_string())
}

// ============ 增值信息（权益/付费状态/活动/账单/产品目录/会话用量）与设备诊断 ============

struct AccCtx {
    bundle: AccountBundle,
    token: String,
    device: String,
    client: AssetsClient,
}

/// 装载单账号查询上下文：token + 账号级设备槽 + 按版本/区域选定的客户端
fn load_ctx(id: &str) -> CmdResult<AccCtx> {
    let bundles = load_bundles()?;
    let bundle = bundles.into_iter().find(|b| b.id == id).ok_or("账号不存在")?;
    let info = parse_auth_blob(&bundle.auth_blob).map_err(err)?;
    let region = region_of(&info);
    let device = crate::core::assets::device_slot(&bundle.user_id);
    let client = AssetsClient::for_ide(bundle.ide, &region);
    Ok(AccCtx {
        token: info.token,
        device,
        client,
        bundle,
    })
}

/// 聚合拉取单账号增值信息：权益包 / 付费状态 / 活动中心 / 账单 / 产品目录。
/// 单接口失败不阻断整体，错误按 key 归入 errors，前端逐项展示。
#[tauri::command]
pub async fn query_assets(id: String) -> CmdResult<String> {
    tauri::async_runtime::spawn_blocking(move || query_assets_impl(&id))
        .await
        .map_err(err)?
}

fn query_assets_impl(id: &str) -> CmdResult<String> {
    use crate::core::logger::{Level, Source};
    let c = load_ctx(id)?;
    let mut data = serde_json::Map::new();
    let mut errors = serde_json::Map::new();
    macro_rules! q {
        ($key:literal, $call:expr) => {
            match $call {
                Ok(v) => {
                    crate::core::logger::log(
                        Level::Info,
                        Source::Quota,
                        format!("[资产] {} 查询成功 → {}", $key, truncate(&v.to_string(), 400)),
                    );
                    data.insert($key.into(), v);
                }
                Err(e) => {
                    crate::core::logger::log(
                        Level::Warn,
                        Source::Quota,
                        format!("[资产] {} 查询失败: {e}", $key),
                    );
                    errors.insert($key.into(), json!(e.to_string()));
                }
            }
        };
    }
    q!("entitlement", c.client.entitlement_list(&c.token, &c.device));
    q!("pay_status", c.client.pay_status(&c.token, &c.device));
    // 活动中心是 CN 专有接口（`ug/activity/info` 在国际版 ug 主机上恒 404，实测），
    // 国际版直接跳过，不计入"未返回"错误，避免每次预取都误报
    if c.bundle.ide == IdeKind::TraeCn {
        q!("activity", c.client.activity_info(&c.token, &c.device));
    }
    q!("billing", c.client.billing_history(&c.token, &c.device, 1, 20));
    q!("products", c.client.sellable_products(&c.token, &c.device));
    Ok(json!({
        "id": c.bundle.id,
        "name": c.bundle.name,
        "ide": c.bundle.ide,
        "device": c.device,
        "data": data,
        "errors": errors,
        "activitySupported": c.bundle.ide == IdeKind::TraeCn,
    })
    .to_string())
}

/// 幂等活动领奖：只对 activity/info 中 `work_extra.grantable && !granted` 的活动发起，
/// 其余一律不调（避免盲调触发风控）。
///
/// 通道选择（CN 真机核对结论）：走原生命令 `workbench.action.icubeHandleCommercialActivity`
/// —— 它由 IDE 主进程 `handleCommercialActivity` 按 activity_id 分派
/// （`code_fission_202609` 特例走 `/trae/api/v2/ug/code_fission/grant`，其余走
/// `/trae/api/v2/ug/activity/action`），并回传业务码；我们直呼 activity/action
/// 实测返回空 data 且未发放，故不再自行拼 HTTP。
/// 该命令要求 Trae 运行中 + shim 已激活（无 shim 时返回明确错误，不静默跳过）。
#[tauri::command]
pub async fn claim_activity_rewards(id: String) -> CmdResult<String> {
    tauri::async_runtime::spawn_blocking(move || claim_activity_rewards_impl(&id))
        .await
        .map_err(err)?
}

fn claim_activity_rewards_impl(id: &str) -> CmdResult<String> {
    use crate::core::logger::{Level, Source};
    const CMD: &str = "workbench.action.icubeHandleCommercialActivity";
    let c = load_ctx(id)?;
    let info = c.client.activity_info(&c.token, &c.device).map_err(err)?;
    let mut ids = Vec::new();
    crate::core::assets::collect_grantable(&info, &mut ids);
    crate::core::logger::log(
        Level::Info,
        Source::Quota,
        format!("{} 活动领奖：可领取 {} 项 {:?}", c.bundle.name, ids.len(), ids),
    );
    let mut results = Vec::new();
    for aid in &ids {
        match shim_request(
            c.bundle.ide,
            "invoke",
            json!({ "command": CMD, "args": [{ "activity_id": aid }], "timeout_ms": 8000 }),
            12_000,
        ) {
            Ok(r) => {
                let v = r.get("value").cloned().unwrap_or(Value::Null);
                let code = v.pointer("/data/code").and_then(|x| x.as_i64());
                let ok = code.unwrap_or(0) == 0;
                crate::core::logger::log(
                    if ok { Level::Info } else { Level::Warn },
                    Source::Quota,
                    format!(
                        "{} 活动领奖（{aid}）{} → {}",
                        c.bundle.name,
                        if ok { "已提交" } else { "业务失败" },
                        truncate(&v.to_string(), 300)
                    ),
                );
                results.push(json!({ "activity_id": aid, "ok": ok, "result": v }));
            }
            Err(e) => {
                crate::core::logger::log(Level::Warn, Source::Quota, format!("{} 活动领奖（{aid}）失败: {e}", c.bundle.name));
                results.push(json!({ "activity_id": aid, "ok": false, "result": e.to_string() }));
            }
        }
    }
    // 落地校验：claim 返回 200 仅是"受理"，是否真发放要看 granted 是否翻转。
    // 实测 student_auth_reward 受理后仍 false（该活动需先完成学生认证）
    let landed = if ids.is_empty() {
        Vec::new()
    } else {
        match c.client.activity_info(&c.token, &c.device) {
            Ok(after) => {
                let granted = crate::core::assets::collect_granted(&after);
                ids.iter().filter(|id| granted.contains(id)).cloned().collect::<Vec<_>>()
            }
            Err(_) => Vec::new(),
        }
    };
    crate::core::logger::log(
        if landed.is_empty() { Level::Warn } else { Level::Info },
        Source::Quota,
        format!("{} 活动领奖落地：{}/{} 实际发放", c.bundle.name, landed.len(), ids.len()),
    );
    Ok(json!({ "grantable": ids, "landed": landed, "results": results }).to_string())
}

/// 1024 签到抽奖（活动 id `checkin_1024`）—— 逆向自 CN `main.js` 的 checkinLotteryController：
/// 抽奖经主进程 `handleCommercialActivity` → `POST /trae/api/v2/ug/activity/action`
/// （body: {activity_id, req_source}）。`via` 选择通道：
/// - `shim`（默认）：IDE 原生命令 `workbench.action.icubeHandleCommercialActivity`
///   —— 主进程做环境与资格校验（CN + MARSCODE + `checkin_1024` 已启用），与"活动领奖"同路；
/// - `http`：本工具直呼 activity/action（对照用；历史实测直呼对领奖返回空 data）。
/// 注意：原生命令以 IDE **当前登录账号** 身份执行、不携带目标账号 token；
/// 非当前账号需先无感换号（前端批量抽奖据此编排）。
#[tauri::command]
pub async fn draw_checkin_lottery(id: String, via: Option<String>) -> CmdResult<String> {
    tauri::async_runtime::spawn_blocking(move || {
        draw_checkin_lottery_impl(&id, via.as_deref().unwrap_or("shim"))
    })
    .await
    .map_err(err)?
}

const CHECKIN_LOTTERY_AID: &str = "checkin_1024";

fn draw_checkin_lottery_impl(id: &str, via: &str) -> CmdResult<String> {
    use crate::core::logger::{Level, Source};
    let c = load_ctx(id)?;
    if c.bundle.ide != IdeKind::TraeCn {
        return Err("1024 签到抽奖仅支持 Trae 国内版".into());
    }
    let is_current = switch::scan_current(&c.bundle.ide)
        .ok()
        .flatten()
        .map(|b| b.id)
        .as_deref()
        == Some(c.bundle.id.as_str());
    let (code, raw) = if via == "http" {
        let v = c
            .client
            .activity_action(&c.token, &c.device, CHECKIN_LOTTERY_AID)
            .map_err(err)?;
        let code = v
            .pointer("/data/code")
            .and_then(|x| x.as_i64())
            .or_else(|| v.get("code").and_then(|x| x.as_i64()));
        (code, v)
    } else {
        let r = shim_request(
            c.bundle.ide,
            "invoke",
            json!({
                "command": "workbench.action.icubeHandleCommercialActivity",
                "args": [{ "activity_id": CHECKIN_LOTTERY_AID }],
                "timeout_ms": 8000,
            }),
            12_000,
        )?;
        let v = r.get("value").cloned().unwrap_or(Value::Null);
        let code = v.pointer("/data/code").and_then(|x| x.as_i64());
        (code, v)
    };
    let ok = code.unwrap_or(0) == 0;
    crate::core::logger::log(
        if ok { Level::Info } else { Level::Warn },
        Source::Quota,
        format!(
            "{} 1024 抽奖（{via}{}）{} → {}",
            c.bundle.name,
            if is_current { "" } else { "，非当前账号" },
            if ok { "已参与" } else { "业务失败" },
            truncate(&raw.to_string(), 300)
        ),
    );
    Ok(json!({
        "ok": ok,
        "code": code,
        "isCurrent": is_current,
        "via": via,
        "raw": raw,
    })
    .to_string())
}

/// 日志截断：保留前 n 个字符（按字符边界切，避免 UTF-8 半字符 panic）
fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        return s.to_string();
    }
    let cut: String = s.chars().take(n).collect();
    format!("{cut}…")
}

/// ICDRS 设备日志（F1）：读该 IDE 最新一轮 main.log 的 did/ldid/rdid。
/// 用于核对"磁盘设备身份 vs 运行实例解析出的设备身份"是否一致。
#[tauri::command]
pub async fn icdrs_log(ide: String) -> CmdResult<String> {
    tauri::async_runtime::spawn_blocking(move || {
        let kind = parse_ide(&ide)?;
        Ok(crate::core::assets::read_icdrs_log(kind).to_string())
    })
    .await
    .map_err(err)?
}

/// A4 运行时配置视图：该版本 product.json 的 bootConfig 主机表 / authConfig /
/// ug appId / commercialConfig，供诊断页展示（api_host、ug_host 也由此解析）
#[tauri::command]
pub async fn runtime_config(ide: String) -> CmdResult<String> {
    tauri::async_runtime::spawn_blocking(move || {
        let kind = parse_ide(&ide)?;
        Ok(crate::core::assets::runtime_config(kind, "").to_string())
    })
    .await
    .map_err(err)?
}

// ============ shim 命令通道（A3 能力字典 / 只读命令调用）============

/// 向 shim 发一次请求并校验 success（list-commands / invoke 共用）
fn shim_request(kind: IdeKind, action: &str, extra: Value, timeout_ms: u64) -> CmdResult<Value> {
    if !crate::core::shim::installed(kind) {
        return Err("未安装切换助手：请先在设置页安装切换助手并重启 IDE".into());
    }
    let mut payload = extra;
    match payload.as_object_mut() {
        Some(o) => {
            o.insert("action".into(), json!(action));
        }
        None => payload = json!({ "action": action }),
    }
    let r = crate::core::shim::request_hot_swap(kind, &payload, timeout_ms)?;
    if r.get("success").and_then(|b| b.as_bool()) != Some(true) {
        return Err(format!(
            "切换助手返回失败: {}",
            r.get("error").and_then(|e| e.as_str()).unwrap_or("未响应或未知错误")
        ));
    }
    Ok(r)
}

/// A3 能力字典：shim 实时返回 IDE 已注册的 trae/icube 命令
/// （`vscode.commands.getCommands`，这些命令可由扩展宿主 executeCommand 直接调用）
#[tauri::command]
pub async fn shim_commands(ide: String) -> CmdResult<String> {
    tauri::async_runtime::spawn_blocking(move || {
        let kind = parse_ide(&ide)?;
        let r = shim_request(kind, "list-commands", json!({}), 8000)?;
        Ok(r.get("commands").cloned().unwrap_or(json!([])).to_string())
    })
    .await
    .map_err(err)?
}

/// 只读/幂等命令白名单前缀。破坏类命令（deleteAccount / logout / redeem / openCashier …）
/// 一律不得从本工具触发，与"写接口仅限幂等项"策略一致。
/// 会话类取舍：导出当前会话 / 会话迁移(teleport) / 收回本地 可重复执行（幂等）→ 放行；
/// `icube.chat.forkSession` 每次都产生新会话，非幂等 → 不放行，仅在能力字典中列出。
/// 白名单前缀已按真机 v1.1.0 shim 实测（562 条命令）逐条核对。
/// 放行的窗口类命令依赖 shim v1.2.0 的 invoke 超时护栏，避免 busy 锁被对话框挂死。
const READONLY_CMD_PREFIXES: [&str; 19] = [
    "icube.common.get",
    "icube.common.commands.get",
    "icube.common.runtimeConfig",
    "icube.common.is",
    "icube.userConfiguration.get",
    "icube.getProjectInfo",
    "icube.getCurrentSession",
    "icube.cloudide.get",
    "icube.cloudide.aiSessionID",
    // 会话只读：getCurrentSessionId / getSessionRunningStatus 实测同步返回
    "icube.chat.get",
    "icube.ai.get",
    "iCube.workspace.get",
    "workbench.action.icubeGet",
    "trae.ai.get",
    "vscode.icube.sidebar.activeViewId",
    // B1：会话导出 / 迁移（幂等，可重复执行）
    "workbench.action.icube.aiChatSidebar.exportCurrentSession",
    "icube.session.teleport",
    "icube.session.returnToLocal",
    // 学生认证入口：student_auth_reward(2000 通用积分) 的真实发放路径
    // （auto_grant：完成认证后服务端自动发放，claim 调用对它是空转）
    "workbench.action.icubeOpenStudentVerification",
];

/// 调用一条白名单内的命令（能力字典里看到即可实测验证）。
/// shim 侧 8s 超时（Promise.race）先于本侧 12s 轮询超时收敛，
/// 保证窗口类命令挂住时拿到明确的 timeout 错误而不是"shim 未响应"。
#[tauri::command]
pub async fn shim_invoke(ide: String, command: String, args: Option<Value>) -> CmdResult<String> {
    tauri::async_runtime::spawn_blocking(move || {
        let kind = parse_ide(&ide)?;
        if !READONLY_CMD_PREFIXES.iter().any(|p| command.starts_with(p)) {
            return Err(format!("仅允许调用白名单内的只读/幂等命令，已拒绝：{command}"));
        }
        let list = args.and_then(|v| v.as_array().cloned()).unwrap_or_default();
        let r = shim_request(
            kind,
            "invoke",
            json!({ "command": command, "args": list, "timeout_ms": 8000 }),
            12_000,
        )?;
        Ok(json!({ "command": command, "value": r.get("value").cloned().unwrap_or(Value::Null) }).to_string())
    })
    .await
    .map_err(err)?
}

// ============ P0-1 账号库备份 / 恢复 ============

/// 导出加密备份包：把账号库（DPAPI 密文原样）复制到 backup/accounts-<ts>.dpapi
#[tauri::command]
pub fn export_vault_backup() -> CmdResult<String> {
    use crate::core::logger::{Level, Source};
    let dir = crate::core::vault::backup_dir();
    std::fs::create_dir_all(&dir).map_err(err)?;
    let src = crate::core::vault::vault_file();
    if !src.is_file() {
        return Err("账号库不存在，暂无可备份内容".into());
    }
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let dst = dir.join(format!("accounts-{ts}.dpapi"));
    std::fs::copy(&src, &dst).map_err(err)?;
    crate::core::logger::log(Level::Info, Source::Account, format!("已导出账号库备份：{}", dst.display()));
    Ok(dst.display().to_string())
}

/// 列出可用备份包（新→旧）：[{path, mtime_ms, size}]
#[tauri::command]
pub fn list_vault_backups() -> CmdResult<String> {
    let dir = crate::core::vault::backup_dir();
    let mut items: Vec<(u64, String, u64)> = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&dir) {
        for e in entries.flatten() {
            let p = e.path();
            if p.extension().and_then(|x| x.to_str()) != Some("dpapi") {
                continue;
            }
            let meta = e.metadata().ok();
            let mtime = meta
                .as_ref()
                .and_then(|m| m.modified().ok())
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_millis() as u64)
                .unwrap_or(0);
            let size = meta.as_ref().map(|m| m.len()).unwrap_or(0);
            items.push((mtime, p.display().to_string(), size));
        }
    }
    items.sort_by(|a, b| b.0.cmp(&a.0));
    let arr: Vec<Value> = items
        .into_iter()
        .map(|(mtime, path, size)| json!({ "path": path, "mtime_ms": mtime, "size": size }))
        .collect();
    Ok(json!(arr).to_string())
}

/// 从备份包恢复：先校验可解密且结构合法，再整体替换（原库自动轮转为 accounts.bak）
#[tauri::command]
pub async fn import_vault_backup(app: tauri::AppHandle, path: String) -> CmdResult<String> {
    let r = tauri::async_runtime::spawn_blocking(move || import_vault_backup_impl(&path))
        .await
        .map_err(err)?;
    crate::tray::refresh(&app);
    r
}

fn import_vault_backup_impl(path: &str) -> CmdResult<String> {
    use crate::core::logger::{Level, Source};
    let src = std::path::PathBuf::from(path);
    if !src.is_file() {
        return Err("备份文件不存在".into());
    }
    let raw = std::fs::read(&src).map_err(err)?;
    // 必须先校验（DPAPI 只能同机同用户解密 + 内容须是合法账号库），避免用坏文件覆盖库
    let plain = crate::core::vault::decrypt_bytes(&raw)
        .map_err(|e| format!("备份无法解密（需同一 Windows 用户导出的备份）：{e}"))?;
    let v: Value = serde_json::from_slice(&plain).map_err(|e| format!("备份内容不是合法账号库：{e}"))?;
    let Some(arr) = v.get("accounts").and_then(|a| a.as_array()) else {
        return Err("备份内容缺少 accounts 列表".into());
    };
    let count = arr.len();
    crate::core::vault::replace_with_bytes(&raw).map_err(err)?;
    crate::core::logger::log(
        Level::Warn,
        Source::Account,
        format!("已从备份恢复账号库（{count} 个账号），原库轮转为 accounts.bak：{}", src.display()),
    );
    Ok(format!("已恢复 {count} 个账号；原账号库已轮转为 accounts.bak"))
}

// ============ 可移植账号包（口令加密，跨机迁移）============

/// 导出账号包：以口令重加密账号库，写入备份目录并返回 {path, content}
#[tauri::command]
pub async fn export_portable(passphrase: String) -> CmdResult<String> {
    tauri::async_runtime::spawn_blocking(move || export_portable_impl(&passphrase))
        .await
        .map_err(err)?
}

fn export_portable_impl(passphrase: &str) -> CmdResult<String> {
    use crate::core::logger::{Level, Source};
    // PBKDF2 20 万轮为 CPU 密集操作，已在 spawn_blocking 线程内执行
    let envelope = crate::core::portable::export(passphrase)?;
    let dir = crate::core::vault::backup_dir();
    std::fs::create_dir_all(&dir).map_err(err)?;
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let path = dir.join(format!("portable-{ts}.tsw"));
    std::fs::write(&path, &envelope).map_err(err)?;
    crate::core::logger::log(
        Level::Info,
        Source::Account,
        format!("已导出可移植账号包：{}", path.display()),
    );
    Ok(json!({ "path": path.display().to_string(), "content": envelope }).to_string())
}

/// 导入账号包：source 为文件路径或账号包内容（以 `{` 开头判定为内容）
#[tauri::command]
pub async fn import_portable(
    app: tauri::AppHandle,
    passphrase: String,
    source: String,
) -> CmdResult<String> {
    let r = tauri::async_runtime::spawn_blocking(move || import_portable_impl(&passphrase, &source))
        .await
        .map_err(err)?;
    crate::tray::refresh(&app);
    r
}

fn import_portable_impl(passphrase: &str, source: &str) -> CmdResult<String> {
    use crate::core::logger::{Level, Source};
    let raw = if source.trim_start().starts_with('{') {
        source.to_string()
    } else {
        std::fs::read_to_string(source.trim()).map_err(|e| format!("读取账号包失败：{e}"))?
    };
    let count = crate::core::portable::import(passphrase, &raw)?;
    crate::core::logger::log(
        Level::Warn,
        Source::Account,
        format!("已用可移植账号包恢复账号库（{count} 个账号）"),
    );
    Ok(format!("已恢复 {count} 个账号；原账号库已轮转为 accounts.bak"))
}
