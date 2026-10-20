// shim 扩展：跑在 Trae 扩展宿主里的热换号监听器（免 --inspect 的主热切换通道）
// 链路：本程序写 trigger 文件 → shim 轮询到 → executeCommand('_tsw.replaceUserInfo')
//   → workbench patch 命令 → IPC → main.js patch handler → setUserInfo + 设备态同步
// 协议文件按 IDE 分离（.trae / .trae-cn 共存互不干扰），由 shim 按 __dirname 自判。
use super::paths::IdeKind;
use serde::Serialize;
use serde_json::{json, Value};
use std::io;
use std::sync::Mutex;

pub const SHIM_VERSION: &str = "1.2.0";
pub const SHIM_EXT_ID: &str = "trae-ide-switcher.hotswap";
/// 固定 uuid：extensions.json 登记项须跨安装稳定
pub const SHIM_EXT_UUID: &str = "b7c4d2e8-9f2a-4b3c-8e5d-tswswitch01";

const HEARTBEAT_ALIVE_MS: u128 = 90_000;
const POLL_RESULT_INTERVAL_MS: u64 = 80;

fn ide_tag(ide: IdeKind) -> &'static str {
    match ide {
        IdeKind::Trae => "trae",
        IdeKind::TraeCn => "traecn",
    }
}

/// 协议文件的通道标签：主实例 = "trae"/"traecn"；多开实例 = "trae-<channel>"/"traecn-<channel>"。
/// 两实例共用一个 %TEMP% 目录，不加通道后缀会互相抢同一触发文件。
fn channel_tag(ide: IdeKind, channel: &str) -> String {
    if channel.is_empty() {
        ide_tag(ide).to_string()
    } else {
        format!("{}-{}", ide_tag(ide), channel)
    }
}

fn trigger_file_ch(ide: IdeKind, channel: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!("trae-ide-switcher-hotswap-{}.json", channel_tag(ide, channel)))
}
fn result_file_ch(ide: IdeKind, channel: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!(
        "trae-ide-switcher-hotswap-result-{}.json",
        channel_tag(ide, channel)
    ))
}
fn status_file_ch(ide: IdeKind, channel: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!(
        "trae-ide-switcher-shim-status-{}.json",
        channel_tag(ide, channel)
    ))
}

fn trigger_file(ide: IdeKind) -> std::path::PathBuf {
    trigger_file_ch(ide, "")
}
fn result_file(ide: IdeKind) -> std::path::PathBuf {
    result_file_ch(ide, "")
}
fn status_file(ide: IdeKind) -> std::path::PathBuf {
    status_file_ch(ide, "")
}

/// 用户级扩展根目录：~/.trae/extensions（intl）/ ~/.trae-cn/extensions（CN）
fn extensions_root(ide: IdeKind) -> std::path::PathBuf {
    let home = std::env::var_os("USERPROFILE")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from(r"C:\Users\Default"));
    match ide {
        IdeKind::Trae => home.join(".trae").join("extensions"),
        IdeKind::TraeCn => home.join(".trae-cn").join("extensions"),
    }
}

fn ext_dir_at(root: &std::path::Path) -> std::path::PathBuf {
    root.join(format!("{SHIM_EXT_ID}-{SHIM_VERSION}"))
}

fn ext_dir(ide: IdeKind) -> std::path::PathBuf {
    ext_dir_at(&extensions_root(ide))
}

const EXT_JS: &str = r#"// trae-ide-switcher shim：扩展宿主内的无感换号监听器
const fs = require('fs');
const os = require('os');
const path = require('path');
const vscode = require('vscode');

const SHIM_VERSION = '__SHIM_VERSION__';
// 通道标识（安装时注入）：主实例 'trae' / 'traecn'；多开实例 'trae-<accountId>' 等。
// 由安装方按扩展目录决定，不再嗅探 __dirname（多开实例目录里没有 .trae-cn 段）
const IDE_CHANNEL = __SHIM_CHANNEL__;
const TRIGGER_FILE = path.join(os.tmpdir(), 'trae-ide-switcher-hotswap-' + IDE_CHANNEL + '.json');
const RESULT_FILE = path.join(os.tmpdir(), 'trae-ide-switcher-hotswap-result-' + IDE_CHANNEL + '.json');
const STATUS_FILE = path.join(os.tmpdir(), 'trae-ide-switcher-shim-status-' + IDE_CHANNEL + '.json');
const POLL_INTERVAL_MS = 400;
const HEARTBEAT_INTERVAL_MS = 30000;

let pollTimer = null;
let heartbeatTimer = null;
let busy = false;

function activate(context) {
  beat();
  heartbeatTimer = setInterval(beat, HEARTBEAT_INTERVAL_MS);
  pollTimer = setInterval(poll, POLL_INTERVAL_MS);
  context.subscriptions.push({
    dispose: () => {
      if (pollTimer) { clearInterval(pollTimer); pollTimer = null; }
      if (heartbeatTimer) { clearInterval(heartbeatTimer); heartbeatTimer = null; }
    },
  });
}

function deactivate() {
  if (pollTimer) clearInterval(pollTimer);
  if (heartbeatTimer) clearInterval(heartbeatTimer);
}

function beat() {
  try {
    fs.writeFileSync(STATUS_FILE, JSON.stringify({ version: SHIM_VERSION, at: Date.now(), pid: process.pid }));
  } catch (_) { /* ignore */ }
}

async function poll() {
  if (busy || !fs.existsSync(TRIGGER_FILE)) return;
  // 单一互斥锁：同步置 true，所有早退路径由外层 finally 统一释放
  busy = true;
  try {
    let payload = null;
    try {
      const raw = fs.readFileSync(TRIGGER_FILE, 'utf-8');
      try { fs.unlinkSync(TRIGGER_FILE); } catch (_) { /* ignore */ }
      payload = JSON.parse(raw);
    } catch (e) {
      writeResult({ success: false, error: 'bad trigger: ' + (e && e.message) });
      return;
    }
    try {
      let result = null;
      if (payload.action === 'login') {
        if (!payload.userInfo) throw new Error('login payload missing userInfo');
        result = await vscode.commands.executeCommand('_tsw.replaceUserInfo', {
          userInfo: payload.userInfo,
          device: payload.device || null,
        });
        result = (result && typeof result === 'object') ? result : { success: true };
      } else if (payload.action === 'logout') {
        await vscode.commands.executeCommand('icube.common.logout');
        result = { success: true };
      } else if (payload.action === 'get-token') {
        const token = await vscode.commands.executeCommand('icube.common.getToken');
        const userInfo = await vscode.commands.executeCommand('icube.common.getUserInfo');
        result = { success: true, token: token || null, userInfo: userInfo || null };
      } else if (payload.action === 'ping') {
        result = { success: true, pong: true, version: SHIM_VERSION };
      } else if (payload.action === 'list-commands') {
        const all = await vscode.commands.getCommands(true);
        const re = /^(trae\.|icube\.|_icube\.|iCube\.|workbench\.action\.icube|vscode\.icube|_tsw\.)/;
        result = { success: true, commands: all.filter((c) => re.test(c)).sort() };
      } else if (payload.action === 'invoke') {
        if (!payload.command) throw new Error('invoke missing command');
        // 超时护栏：导出/迁移类命令会走侧边栏流程（可能弹保存对话框），
        // 若同步等待会把 shim 的全局 busy 单锁挂死（后续换号 trigger 全部超时）。
        // 因此用 Promise.race 强制收敛：超时按失败返回并释放锁。
        const ms = payload.timeout_ms || 8000;
        const out = await Promise.race([
          Promise.resolve(vscode.commands.executeCommand(payload.command, ...(payload.args || []))),
          new Promise((_, reject) => setTimeout(() => reject(new Error(
            'command timeout ' + ms + 'ms（命令可能弹出了对话框或未返回；busy 锁已释放，可重试）'
          )), ms)),
        ]);
        result = { success: true, value: safeJson(out) };
      } else {
        result = { success: false, error: 'invalid action: ' + payload.action };
      }
      writeResult(result);
    } catch (e) {
      writeResult({ success: false, error: (e && e.message) || String(e) });
    }
  } finally {
    busy = false;
  }
}

function safeJson(v) {
  if (v === undefined) return null;
  try { return JSON.parse(JSON.stringify(v)); } catch (_) { return String(v); }
}

function writeResult(r) {
  try {
    fs.writeFileSync(RESULT_FILE, JSON.stringify(Object.assign({}, r, { at: Date.now() })));
  } catch (_) { /* ignore */ }
}

module.exports = { activate, deactivate };
"#;

fn ext_pkg_json() -> String {
    json!({
        "name": "trae-ide-switcher-shim",
        "displayName": "TraeIDE-Switcher Hotswap",
        "description": "Listens for hotswap trigger files; calls _tsw.replaceUserInfo to swap accounts in-memory without restart.",
        "publisher": "trae-ide-switcher",
        "version": SHIM_VERSION,
        "engines": { "vscode": "^1.0.0" },
        "main": "./extension.js",
        "activationEvents": ["onStartupFinished"],
        "contributes": {}
    })
    .to_string()
}

/// 把 shim 登记进指定扩展根下的 extensions.json —— Trae 只加载登记过的扩展
fn register_at(root: &std::path::Path) -> io::Result<()> {
    let path = root.join("extensions.json");
    let mut arr: Vec<Value> = match std::fs::read_to_string(&path) {
        Ok(s) => serde_json::from_str::<Value>(&s)
            .ok()
            .and_then(|v| v.as_array().cloned())
            .unwrap_or_default(),
        Err(_) => Vec::new(),
    };
    // 去掉旧 shim 条目（同 id 任意版本），再追加当前版本
    arr.retain(|e| {
        e.get("identifier")
            .and_then(|i| i.get("id"))
            .and_then(|v| v.as_str())
            != Some(SHIM_EXT_ID)
    });
    let dir = ext_dir_at(root);
    let uri_path = format!("/{}", dir.to_string_lossy().replace('\\', "/"));
    arr.push(json!({
        "identifier": { "id": SHIM_EXT_ID, "uuid": SHIM_EXT_UUID },
        "version": SHIM_VERSION,
        "location": { "$mid": 1, "path": uri_path, "scheme": "file" },
        "relativeLocation": format!("{SHIM_EXT_ID}-{SHIM_VERSION}"),
        "metadata": {
            "isApplicationScoped": false,
            "isMachineScoped": false,
            "isBuiltin": false,
            "installedTimestamp": std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as u64)
                .unwrap_or(0),
            "pinned": true,
            "source": "vsix",
            "id": SHIM_EXT_UUID,
            "publisherId": SHIM_EXT_UUID,
            "publisherDisplayName": "trae-ide-switcher",
            "targetPlatform": "undefined",
            "updated": false,
            "private": false,
            "isPreReleaseVersion": false,
            "hasPreReleaseVersion": false,
            "preRelease": false,
        },
    }));
    std::fs::write(&path, serde_json::to_string(&arr).unwrap())
}

fn unregister_extensions_json(ide: IdeKind) {
    let path = extensions_root(ide).join("extensions.json");
    let Ok(s) = std::fs::read_to_string(&path) else { return };
    let Ok(mut v) = serde_json::from_str::<Value>(&s) else { return };
    let Some(arr) = v.as_array_mut() else { return };
    let before = arr.len();
    arr.retain(|e| {
        e.get("identifier")
            .and_then(|i| i.get("id"))
            .and_then(|v| v.as_str())
            != Some(SHIM_EXT_ID)
    });
    if arr.len() != before {
        let _ = std::fs::write(&path, serde_json::to_string(&v).unwrap());
    }
}

/// 写入 shim 扩展文件（channel 决定协议文件后缀：空 = 主实例，非空 = 多开实例）
fn write_extension(root: &std::path::Path, ide: IdeKind, channel: &str) -> io::Result<()> {
    let dir = ext_dir_at(root);
    std::fs::create_dir_all(&dir)?;
    // 注入与 Rust 侧一致的通道串（'trae' / 'traecn' / 'trae-<ch>'…）：不嗅探目录，
    // 多开实例的目录里既没有 .trae-cn 段也无法区分版本，必须由安装方显式告知
    let tag = channel_tag(ide, channel);
    let js = EXT_JS
        .replace("__SHIM_VERSION__", SHIM_VERSION)
        .replace("__SHIM_CHANNEL__", &format!("'{tag}'"));
    std::fs::write(dir.join("package.json"), ext_pkg_json())?;
    std::fs::write(dir.join("extension.js"), js)?;
    Ok(())
}

/// 安装 shim（写扩展文件 + 登记；重启 Trae 后激活）
pub fn install(ide: IdeKind) -> io::Result<String> {
    let root = extensions_root(ide);
    write_extension(&root, ide, "")?;
    register_at(&root)?;
    Ok(format!("已安装切换助手 v{SHIM_VERSION}，重启 {}/extensions 后生效", match ide {
        IdeKind::Trae => ".trae",
        IdeKind::TraeCn => ".trae-cn",
    }))
}

/// 把 shim 装进多开实例的独立扩展目录（channel 非空 → 协议文件与主实例分离，
/// 两个实例可各自无感换号而不互相抢触发文件）
pub fn install_isolated(ide: IdeKind, instance_dir: &std::path::Path, channel: &str) -> io::Result<()> {
    let root = instance_dir.join("extensions");
    write_extension(&root, ide, channel)?;
    register_at(&root)
}

/// 卸载 shim（删目录 + 反登记 + 清协议文件）
pub fn uninstall(ide: IdeKind) -> io::Result<String> {
    let dir = ext_dir(ide);
    if dir.exists() {
        std::fs::remove_dir_all(&dir)?;
    }
    unregister_extensions_json(ide);
    for f in [trigger_file(ide), result_file(ide), status_file(ide)] {
        let _ = std::fs::remove_file(f);
    }
    Ok("已卸载切换助手，重启 Trae 后彻底生效".into())
}

#[derive(Debug, Serialize)]
pub struct ShimStatus {
    pub installed: bool,
    pub installed_version: Option<String>,
    pub target_version: String,
    pub alive: bool,
    pub last_heartbeat: Option<u128>,
}

/// 状态：是否安装（package.json name 校验）+ 心跳是否 <90s
pub fn status(ide: IdeKind) -> ShimStatus {
    let dir = ext_dir(ide);
    let mut installed = false;
    let mut installed_version = None;
    let pkg = dir.join("package.json");
    let js = dir.join("extension.js");
    if pkg.exists() && js.exists() {
        if let Ok(s) = std::fs::read_to_string(&pkg) {
            if let Ok(v) = serde_json::from_str::<Value>(&s) {
                if v["name"].as_str() == Some("trae-ide-switcher-shim") {
                    installed = true;
                    installed_version = v["version"].as_str().map(String::from);
                }
            }
        }
    }
    let mut alive = false;
    let mut last_heartbeat = None;
    if let Ok(s) = std::fs::read_to_string(status_file(ide)) {
        if let Ok(v) = serde_json::from_str::<Value>(&s) {
            if let Some(at) = v["at"].as_u64() {
                last_heartbeat = Some(at as u128);
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_millis())
                    .unwrap_or(0);
                alive = now.saturating_sub(at as u128) < HEARTBEAT_ALIVE_MS;
            }
        }
    }
    ShimStatus {
        installed,
        installed_version,
        target_version: SHIM_VERSION.to_string(),
        alive,
        last_heartbeat,
    }
}

static HOTSWAP_LOCK: Mutex<()> = Mutex::new(());

/// 热切换请求：清旧 result → 写 trigger → 轮询 result（默认 5s 超时）
/// 串行锁防并发写踩；返回 shim result JSON。
pub fn request_hot_swap(ide: IdeKind, payload: &Value, timeout_ms: u64) -> Result<Value, String> {
    request_hot_swap_ch(ide, "", payload, timeout_ms)
}

/// 带通道的热切换请求（channel 为空 = 主实例；非空 = 对应的多开实例）
pub fn request_hot_swap_ch(
    ide: IdeKind,
    channel: &str,
    payload: &Value,
    timeout_ms: u64,
) -> Result<Value, String> {
    let _g = HOTSWAP_LOCK.lock().map_err(|_| "hotswap 锁中毒")?;
    let trig = trigger_file_ch(ide, channel);
    let res = result_file_ch(ide, channel);
    let _ = std::fs::remove_file(&res);

    let mut p = payload.clone();
    if let Some(obj) = p.as_object_mut() {
        obj.insert(
            "at".into(),
            json!(std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as u64)
                .unwrap_or(0)),
        );
    }
    std::fs::write(&trig, serde_json::to_string(&p).unwrap())
        .map_err(|e| format!("写 trigger 失败: {e}"))?;

    let start = std::time::Instant::now();
    while start.elapsed().as_millis() < timeout_ms as u128 {
        if let Ok(raw) = std::fs::read_to_string(&res) {
            let _ = std::fs::remove_file(&res);
            return serde_json::from_str::<Value>(&raw)
                .map_err(|e| format!("bad result JSON: {e}"));
        }
        std::thread::sleep(std::time::Duration::from_millis(POLL_RESULT_INTERVAL_MS));
    }
    // 超时清 trigger 防 stale
    let _ = std::fs::remove_file(&trig);
    Err("切换助手未响应（可能未安装、未激活或已崩溃）".into())
}

/// 等待 shim 心跳就绪（重启 Trae 后激活约需 5-10s）
pub fn wait_alive(ide: IdeKind, timeout_ms: u64) -> bool {
    let start = std::time::Instant::now();
    while start.elapsed().as_millis() < timeout_ms as u128 {
        if status(ide).alive {
            return true;
        }
        std::thread::sleep(std::time::Duration::from_millis(300));
    }
    false
}

/// 编排用热切换就绪判定：Trae 运行中 + shim 已装 + 心跳活着
pub fn ready_for_hotswap(ide: IdeKind) -> bool {
    let s = status(ide);
    s.installed && s.alive
}

/// shim 扩展目录（诊断/展示用）
pub fn install_dir(ide: IdeKind) -> std::path::PathBuf {
    ext_dir(ide)
}

/// shim 是否已安装（不考虑心跳）
pub fn installed(ide: IdeKind) -> bool {
    status(ide).installed
}
