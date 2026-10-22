// 账号切换执行器：shim 热切换（主）/ CDP 热切换（兜底）/ 冷切换与本机登录态扫描
use super::auth::parse_auth_blob;
use super::model::{AccountBundle, AccountStatus};
use super::paths::{IdeKind, IdePaths};
use super::store::{StorageJson, TinyStorage, Vscdb};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::io;

pub const AUTH_KEY: &str = "iCubeAuthInfo://icube.cloudide";
pub const USERTAG_KEY: &str = "iCubeAuthInfo://usertag";
/// 额度快照缓存（ServerData）：随账号变动，切换时必须清掉旧值，否则 Trae 校验 4 键不一致
pub const SERVER_DATA_KEY: &str = "iCubeServerData://icube.cloudide";
/// AHA ECDSA 设备密钥键前缀（随号保存恢复，防重生成风控 4015）
pub const ECDSA_PREFIX: &str = "iCubeAuthInfo://icube-dc:";
/// 遥测身份：machineId 三处保留本机原值（SQLCipher 密钥来源），dev/sqm 随号
pub const MACHINE_ID_KEY: &str = "telemetry.machineId";
pub const DEV_DEVICE_ID_KEY: &str = "telemetry.devDeviceId";
pub const SQM_ID_KEY: &str = "telemetry.sqmId";
pub const DEVICE_ID_STR_KEY: &str = "switcher.deviceIdStr";
pub const HAS_DEVICE_UPDATED_KEY: &str = "has_device_id_updated_to_aha";
/// TinyStorage 内 aha 设备对象键
pub const TINY_DEVICE_KEY: &str = "aha.device.device_id";

/// 扫描本机当前登录态 -> 账号资产包（含 ECDSA/TinyStorage/遥测身份全量采集）
pub fn scan_current(ide: &IdeKind) -> io::Result<Option<AccountBundle>> {
    let paths = IdePaths::detect(*ide)
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "IDE not installed"))?;
    let sj = StorageJson::load(&paths)?;
    let Some(blob) = sj.get(AUTH_KEY).and_then(|v| v.as_str()) else {
        return Ok(None);
    };
    let info = parse_auth_blob(blob)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;
    let account = info.account.unwrap_or_default();
    let ecdsa_blobs: BTreeMap<String, String> = sj
        .keys_with_prefix(ECDSA_PREFIX)
        .into_iter()
        .filter_map(|k| sj.get(&k).and_then(|v| v.as_str()).map(|s| (k, s.to_string())))
        .collect();
    let tiny_storage = TinyStorage::load(&paths).ok().and_then(|ts| ts.raw_data());
    Ok(Some(AccountBundle {
        id: format!("{}-{}", *ide as u8, info.user_id),
        name: if account.username.is_empty() { account.email.clone() } else { account.username.clone() },
        email: account.email,
        note: String::new(),
        ide: *ide,
        status: AccountStatus::Normal,
        auth_blob: blob.to_string(),
        usertag_blob: sj.get(USERTAG_KEY).and_then(|v| v.as_str()).map(String::from),
        ecdsa_blobs,
        tiny_storage,
        dev_device_id: sj.get(DEV_DEVICE_ID_KEY).and_then(|v| v.as_str()).map(String::from),
        sqm_id: sj.get(SQM_ID_KEY).and_then(|v| v.as_str()).map(String::from),
        machine_id: sj.get(MACHINE_ID_KEY).and_then(|v| v.as_str()).map(String::from),
        device_info: None,
        pinned: false,
        threshold: None,
        refresh_error: None,
        user_id: info.user_id,
    }))
}

/// bundle 内 aha 设备对象的解密快照（payload.tinyDevice 用）
fn tiny_device_of(bundle: &AccountBundle) -> Option<Value> {
    let enc = bundle.tiny_storage.as_ref()?.get(TINY_DEVICE_KEY)?.as_str()?;
    let plain = super::bytecrypto::decrypt_string(enc).ok()?;
    serde_json::from_str::<Value>(&plain).ok()
}

/// 账号资产中的 aha 设备 id（TinyStorage `aha.device.device_id` → `device_id_str`）
pub fn device_id_str_of(bundle: &AccountBundle) -> Option<String> {
    tiny_device_of(bundle)?
        .get("device_id_str")?
        .as_str()
        .map(String::from)
}

/// ECDSA 设备密钥指纹：sha256(键名 + publicKeyPEM) 前 16 hex。
/// 用于在 UI 上区分"各账号携带的设备密钥是否为同一份"（相同指纹 = 设备身份未随号独立）。
pub fn ecdsa_fingerprint(bundle: &AccountBundle) -> Option<String> {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    let mut any = false;
    for (k, v) in &bundle.ecdsa_blobs {
        let Ok(plain) = super::bytecrypto::decrypt_string(v) else { continue };
        let Ok(json) = serde_json::from_str::<Value>(&plain) else { continue };
        let Some(pem) = json.get("publicKeyPEM").and_then(|x| x.as_str()) else { continue };
        hasher.update(k.as_bytes());
        hasher.update(pem.as_bytes());
        any = true;
    }
    if !any {
        return None;
    }
    let digest = hasher.finalize();
    Some(digest[..8].iter().map(|b| format!("{b:02x}")).collect())
}

/// 本机当前 machineId（热切换遥测锚：三处保留原值）
pub fn current_machine_id(ide: &IdeKind) -> Option<String> {
    let paths = IdePaths::detect(*ide)?;
    StorageJson::load(&paths)
        .ok()?
        .get(MACHINE_ID_KEY)
        .and_then(|v| v.as_str())
        .map(String::from)
}

/// 热换号（shim 主通道）：经扩展宿主调 _tsw.replaceUserInfo，免 --inspect
/// device 载荷：machineId=本机原值锚定遥测；tinyDevice=目标账号 aha 设备（每号独立防绑定上限）
pub fn hot_switch_via_shim(ide: &IdeKind, bundle: &AccountBundle, preserved_machine_id: &str) -> Result<Value, String> {
    hot_switch_via_shim_ch(ide, bundle, preserved_machine_id, "")
}

/// 带通道的 shim 热换号（channel 非空 = 多开实例的独立通道）
pub fn hot_switch_via_shim_ch(
    ide: &IdeKind,
    bundle: &AccountBundle,
    preserved_machine_id: &str,
    channel: &str,
) -> Result<Value, String> {
    let user_info: Value =
        serde_json::from_str(&super::bytecrypto::decrypt_string(&bundle.auth_blob).map_err(|e| e.to_string())?)
            .map_err(|e| format!("auth blob JSON 解析失败: {e}"))?;
    let tiny_device = tiny_device_of(bundle);
    let device = json!({
        "machineId": preserved_machine_id,
        "devDeviceId": bundle.dev_device_id,
        "sqmId": bundle.sqm_id,
        "hasDeviceIdUpdated": true,
        "tinyDevice": tiny_device,
    });
    let payload = json!({ "action": "login", "userInfo": user_info, "device": device });
    let result = super::shim::request_hot_swap_ch(*ide, channel, &payload, 5000)?;
    if result.get("success").and_then(|v| v.as_bool()) != Some(true) {
        return Err(format!("切换助手无感换号失败: {result}"));
    }
    Ok(result)
}

/// 多开实例的 shim 通道名：由账号 id 派生（同账号的实例通道固定、跨账号互不冲突）
pub fn isolated_channel(account_id: &str) -> String {
    account_id
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' { c } else { '_' })
        .collect()
}

/// 在多开实例内无感换号：只走该实例的独立 shim 通道（多开实例不带 --inspect，无 CDP 兜底），
/// 落盘目标写到实例自己的 user-data 目录；不更新主实例的 current 标记。
pub fn hot_switch_isolated(ide: &IdeKind, bundle: &AccountBundle, account_id: &str) -> Result<String, String> {
    let channel = isolated_channel(account_id);
    let mid = current_machine_id(ide).ok_or("无法读取本机 machineId")?;
    hot_switch_via_shim_ch(ide, bundle, &mid, &channel)?;
    persist_after_hot_isolated(ide, account_id, bundle, &mid).map_err(|e| e.to_string())?;
    super::logger::log(
        super::logger::Level::Info,
        super::logger::Source::Switch,
        format!("多开实例无感换号到 {}（通道 {channel}）", bundle.name),
    );
    Ok("已在独立实例中完成无感换号（无需重启）".into())
}

/// 写 usertag：有快照随号写回；无快照时保留现有映射并确保含目标 userId 的区域条目。
/// usertag 明文是 {userId: 区域标签("row"/"cn")} 映射，健康登录态必含该键——
/// 直接删除会让启动登录链 convertCredentialToUserInfo 抛 TokenInvalid（切换后停在登录页）。
fn write_usertag(sj: &mut StorageJson, bundle: &AccountBundle) {
    match &bundle.usertag_blob {
        Some(tag) => sj.set_str(USERTAG_KEY, tag),
        None => {
            let cur: serde_json::Map<String, serde_json::Value> = sj
                .get(USERTAG_KEY)
                .and_then(|v| v.as_str())
                .and_then(|s| super::bytecrypto::decrypt_string(s).ok())
                .and_then(|p| serde_json::from_str(&p).ok())
                .unwrap_or_default();
            let entry = serde_json::Value::from(match bundle.ide {
                IdeKind::TraeCn => "cn",
                IdeKind::Trae => "row",
            });
            if cur.get(&bundle.user_id) != Some(&entry) {
                let mut map = cur;
                map.insert(bundle.user_id.clone(), entry);
                let plain = serde_json::to_string(&map).unwrap_or_default();
                if !plain.is_empty() {
                    sj.set_str(USERTAG_KEY, &super::bytecrypto::encrypt_string(&plain));
                }
            }
            // 条目已存在且值一致：不动磁盘现值（保留其余用户的条目）
        }
    }
}

/// 把账号资产写入 storage.json map（热/冷切换/多开实例共用）：
/// 凭据 + usertag + 清 ServerData + 遥测 dev/sqm（缺失则保留现值）+ aha 设备 id + ECDSA 密钥对
/// machine_id 传入时覆盖（热切换锚定本机原值）；不传则保留文件现值（冷切换/多开）
fn write_bundle_identity(sj: &mut StorageJson, bundle: &AccountBundle, machine_id: Option<&str>) {
    sj.set_str(AUTH_KEY, &bundle.auth_blob);
    write_usertag(sj, bundle);
    sj.remove(SERVER_DATA_KEY);
    if let Some(mid) = machine_id {
        sj.set_str(MACHINE_ID_KEY, mid);
    }
    // 资产缺失（旧版入库）时保留本机现值：无法随号恢复时剥离设备身份只会制造漂移/触发重注册风控
    if let Some(v) = &bundle.dev_device_id {
        sj.set_str(DEV_DEVICE_ID_KEY, v);
    }
    if let Some(v) = &bundle.sqm_id {
        sj.set_str(SQM_ID_KEY, v);
    }
    sj.set_str(HAS_DEVICE_UPDATED_KEY, "true");
    if let Some(d) = device_id_str_of(bundle) {
        sj.set_str(DEVICE_ID_STR_KEY, &d);
    }
    if !bundle.ecdsa_blobs.is_empty() {
        sj.remove_with_prefix(ECDSA_PREFIX);
        for (k, v) in &bundle.ecdsa_blobs {
            sj.set_str(k, v);
        }
    }
}

/// 热换号后落盘：storage.json（凭据+遥测身份+ECDSA）/ TinyStorage 快照 / state.vscdb
/// （Trae 内存稍后会整体回写 storage.json，由 main.js 落盘守卫保证遥测键不被覆盖）
pub fn persist_after_hot(ide: &IdeKind, bundle: &AccountBundle, preserved_machine_id: &str) -> io::Result<()> {
    let paths = IdePaths::detect(*ide)
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "IDE not installed"))?;
    persist_after_hot_with(&paths, bundle, preserved_machine_id)
}

/// 同上，但落盘目标指向多开实例自己的 user-data 目录（不能写主实例的 storage.json）
pub fn persist_after_hot_isolated(
    ide: &IdeKind,
    account_id: &str,
    bundle: &AccountBundle,
    preserved_machine_id: &str,
) -> io::Result<()> {
    let mut paths = IdePaths::detect(*ide)
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "IDE not installed"))?;
    paths.user_data = isolated_dir(account_id);
    persist_after_hot_with(&paths, bundle, preserved_machine_id)
}

fn persist_after_hot_with(
    paths: &IdePaths,
    bundle: &AccountBundle,
    preserved_machine_id: &str,
) -> io::Result<()> {
    let mut sj = StorageJson::load(paths)?;
    write_bundle_identity(&mut sj, bundle, Some(preserved_machine_id));
    sj.save()?;

    if let Some(snapshot) = bundle.tiny_storage.clone() {
        let mut ts = TinyStorage::load_or_default(paths)?;
        ts.restore_data(snapshot);
        ts.save()?;
    }

    let vscdb = paths.state_vscdb();
    write_vscdb_identity(
        &vscdb,
        preserved_machine_id,
        bundle.dev_device_id.as_deref(),
        bundle.sqm_id.as_deref(),
    );
    // 5s 后补写一次：对抗 Trae 启动/切换期的 state.vscdb 回写
    let mid = preserved_machine_id.to_string();
    let dev = bundle.dev_device_id.clone();
    let sqm = bundle.sqm_id.clone();
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(5000));
        write_vscdb_identity(&vscdb, &mid, dev.as_deref(), sqm.as_deref());
    });
    Ok(())
}

fn write_vscdb_identity(path: &std::path::Path, machine_id: &str, dev: Option<&str>, sqm: Option<&str>) {
    let Ok(db) = Vscdb::open_path(path.to_path_buf()) else { return };
    let _ = db.set_text(MACHINE_ID_KEY, machine_id);
    if let Some(v) = dev {
        let _ = db.set_text(DEV_DEVICE_ID_KEY, v);
    }
    if let Some(v) = sqm {
        let _ = db.set_text(SQM_ID_KEY, v);
    }
}

/// CDP 兜底完整表达式模板（node 真机迭代定稿形态）：
/// awaitPromise 必开（async IIFE）；require 经 process.mainModule 注入（ESM 下为 null，require 分支静默跳过）；
/// JSON.parse 探针抓异常调用栈；ICDRS BFS 动态定位（TSW 静态补丁存在时直接命中 __sw_icdrs）；
/// v2 body 以 .call(H, t) 绑定 this=holder（防 fallback 扫描误中 globalThis.__sw 桥自身），
/// t 显式携带 __TSW_REPLACE__ 标记（该标记原由 workbench 补丁命令构造）。
const CDP_FULL_EXPR: &str = r#"(async function(){try{
var require=process.mainModule&&process.mainModule.require;
var _errs=[];var _op=JSON.parse;
JSON.parse=function(x,...a){try{return _op(x,...a)}catch(e){_errs.push({input:String(x).slice(0,80),stack:(e.stack||"").split("\n").slice(0,4).join(" | ")});throw e}};
const H=globalThis.__sw.holder;
if(!globalThis.__sw_icdrs){
var _q=[[H,0],[globalThis,0]];var _seen=new Set([H,globalThis]);var _n=0;
while(_q.length&&_n<3000){const [_o,_d]=_q.shift();_n++;
let ks;try{ks=Object.getOwnPropertyNames(_o)}catch(_){continue}
for(const _k of ks){let _v;try{_v=_o[_k]}catch(_){continue}
if(!_v||typeof _v!=="object")continue;
if(_seen.has(_v))continue;_seen.add(_v);
if(typeof _v.isOpen==="function"&&typeof _v.wait==="function"&&typeof _v.resolveDeviceId==="function"){globalThis.__sw_icdrs=_v;_q.length=0;break}
if(_d<3&&_q.length<400)_q.push([_v,_d+1]);
}}
}
const _r=await (async function(t){
__BODY__
}).call(H, JSON.parse(__PAYLOAD__));
JSON.parse=_op;
return {r:_r,errs:_errs};
}catch(e){JSON.parse=_op;return{success:false,error:String(e&&e.message||e),errs:_errs}}})()"#;

/// 热换号（CDP 兜底，完整 v2 逻辑）：经 __sw.holder 直调 v2 handler 体
/// （setUserInfo + 防登出护盾 + 遥测落盘守卫 + apiHost 切换；ICDRS 依赖 BFS 或 TSW 静态补丁，
///  轻量模式 deviceUpdated=false，设备态落盘由 persist_after_hot 补全）
pub fn hot_switch(ide: &IdeKind, bundle: &AccountBundle, preserved_machine_id: &str) -> Result<Value, String> {
    use super::{cdp, launcher};
    let port = launcher::inspect_port(*ide);
    let ready = cdp::evaluate(port, "!!(globalThis.__sw&&globalThis.__sw.holder)").map_err(|e| e.to_string())?;
    if ready.trim() != "true" {
        return Err("备用通道无感换号支持未就绪（需换号器以 --inspect 启动 Trae 且已安装无感换号支持）".into());
    }
    let paths = IdePaths::detect(*ide).ok_or("未检测到安装")?;
    let prop = super::inject::tsw_prop(&paths).ok_or("v2 handler 特征锚未命中（版本不支持）")?;
    let user_info: Value =
        serde_json::from_str(&super::bytecrypto::decrypt_string(&bundle.auth_blob).map_err(|e| e.to_string())?)
            .map_err(|e| format!("auth blob JSON 解析失败: {e}"))?;
    let device = json!({
        "machineId": preserved_machine_id,
        "devDeviceId": bundle.dev_device_id,
        "sqmId": bundle.sqm_id,
        "hasDeviceIdUpdated": true,
        "tinyDevice": tiny_device_of(bundle),
    });
    // t 参数显式带 __TSW_REPLACE__ 标记；payload 以 JS 字符串字面量嵌入（JSON.parse 双重包装）
    let t_arg = json!({ "__TSW_REPLACE__": true, "userInfo": user_info, "device": device });
    let payload_lit = serde_json::to_string(&t_arg.to_string()).map_err(|e| e.to_string())?;
    let body = super::inject::TSW_V2_BODY.replace("{PROP}", &prop);
    let expr = CDP_FULL_EXPR
        .replace("__BODY__", &body)
        .replace("__PAYLOAD__", &payload_lit);
    let raw = cdp::evaluate_await(port, &expr).map_err(|e| e.to_string())?;
    let result: Value = serde_json::from_str(raw.trim())
        .map_err(|e| format!("备用通道返回解析失败: {e} / raw={raw}"))?;
    if result.get("success").and_then(|v| v.as_bool()) != Some(true) {
        return Err(format!("备用通道无感换号失败: {result}"));
    }
    Ok(result)
}

/// 统一热换号编排（GUI/托盘/自动换号共用）：shim 主通道（TSW 补丁就绪）→ CDP 兜底；
/// 成功后落盘设备态（persist_after_hot）并记录 current 账号
pub fn hot_switch_full(ide: &IdeKind, bundle: &AccountBundle, account_id: &str) -> Result<String, String> {
    let paths = IdePaths::detect(*ide).ok_or("未检测到安装")?;
    // 热通道均依赖运行中的 IDE（shim 心跳=扩展宿主 / CDP=调试端口）；未运行时直接冷切换 + --inspect 重启，等效无感
    if !super::launcher::is_running(&paths) {
        cold_switch(ide, bundle).map_err(|e| e.to_string())?;
        super::launcher::launch_with_inspect(&paths, super::launcher::inspect_port(*ide))
            .map_err(|e| e.to_string())?;
        super::vault::set_current_id(account_id);
        super::logger::log(
            super::logger::Level::Info,
            super::logger::Source::Switch,
            format!("切换到 {}（重启，--inspect）", bundle.name),
        );
        return Ok("已切换登录态并重启 Trae".into());
    }
    let mid = current_machine_id(ide).ok_or("无法读取本机 machineId")?;
    // 切号前归档旧账号会话（服务端按账号隔离，只有导出后才能在新账号里重建）
    let prev_uid = scan_current(ide).ok().flatten().map(|p| p.user_id);
    let archived = if prev_uid.as_deref() != Some(bundle.user_id.as_str())
        && super::shim::ready_for_hotswap(*ide)
    {
        match super::chat_history::export(*ide) {
            Ok(n) => {
                super::logger::log(
                    super::logger::Level::Info,
                    super::logger::Source::Switch,
                    format!("已归档当前账号 {n} 条会话"),
                );
                Some(n)
            }
            Err(e) => {
                super::logger::log(
                    super::logger::Level::Warn,
                    super::logger::Source::Switch,
                    format!("历史会话归档跳过：{e}"),
                );
                None
            }
        }
    } else {
        None
    };
    let (result, channel) = if super::shim::ready_for_hotswap(*ide) {
        (hot_switch_via_shim(ide, bundle, &mid), "shim")
    } else {
        (hot_switch(ide, bundle, &mid), "CDP")
    };
    let r = result
        .and_then(|r| persist_after_hot(ide, bundle, &mid).map(|_| r).map_err(|e| e.to_string()))?;
    // 设备寄存（ICDRS）未随号同步时显式告警：ICDRS 封在 ESM 闭包内，动态定位是死路，
    // 唯一通路是静态补丁暴露的 __sw_icdrs；未命中说明补丁不完整，必须重装后重启 Trae
    if r.get("deviceUpdated").and_then(|v| v.as_bool()) != Some(true) {
        super::logger::log(
            super::logger::Level::Warn,
            super::logger::Source::Switch,
            format!(
                "无感换号到 {}（{channel} 通道）未同步设备寄存：请重新安装无感换号支持并重启 Trae",
                bundle.name
            ),
        );
    }
    super::vault::set_current_id(account_id);
    super::logger::log(
        super::logger::Level::Info,
        super::logger::Source::Switch,
        format!("无感换号到 {}（{channel}）", bundle.name),
    );
    // 切号后：先导入归档会话，再重载窗口。
    // 渲染层仍持有旧账号的 project/会话缓存，不重载时 send_message 会带旧 project_id → 992500「项目不存在」；
    // 重载后 initChatView 会用新账号重新建 project。
    let target = bundle.user_id.clone();
    let kind = *ide;
    std::thread::spawn(move || {
        if archived.is_some() {
            match super::chat_history::import_pending(kind, &target) {
                Ok(n) if n > 0 => super::logger::log(
                    super::logger::Level::Info,
                    super::logger::Source::Switch,
                    format!("已复制 {n} 个会话到新账号"),
                ),
                Ok(_) => {}
                Err(e) => super::logger::log(
                    super::logger::Level::Warn,
                    super::logger::Source::Switch,
                    format!("历史会话导入未完成：{e}"),
                ),
            }
        }
        let payload = json!({
            "action": "invoke",
            "command": "workbench.action.reloadWindow",
            "args": [],
            "timeout_ms": 3000,
        });
        let _ = super::shim::request_hot_swap(kind, &payload, 5_000);
    });
    let mut msg: String = match channel {
        "shim" => "无感换号完成（切换助手通道，无需重启）".into(),
        _ if r.get("deviceUpdated").and_then(|v| v.as_bool()) == Some(true) => {
            "无感换号完成（备用通道，无需重启）".into()
        }
        _ => "无感换号完成（备用通道轻量模式：设备寄存未随号同步，建议安装无感换号支持后使用切换助手）".into(),
    };
    if let Some(n) = archived {
        msg.push_str(&format!("，已归档 {n} 条会话"));
    }
    msg.push_str("，Trae 窗口将自动刷新");
    Ok(msg)
}

/// 续期后同步：若 storage.json 当前登录用户与目标一致，写入新凭据
/// （防止服务端 refreshToken 轮换导致 Trae 持有的旧凭据作废）
pub fn sync_auth_if_same_user(ide: &IdeKind, user_id: &str, new_auth_blob: &str) -> io::Result<bool> {
    let paths = IdePaths::detect(*ide)
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "IDE not installed"))?;
    let mut sj = StorageJson::load(&paths)?;
    let current = sj
        .get(AUTH_KEY)
        .and_then(|v| v.as_str())
        .and_then(|b| parse_auth_blob(b).ok())
        .map(|info| info.user_id);
    if current.as_deref() != Some(user_id) {
        return Ok(false);
    }
    sj.set_str(AUTH_KEY, new_auth_blob);
    sj.save()?;
    Ok(true)
}

/// 冷换号：备份后按“storage.json 最后写”原则替换登录态 + 随号身份
/// （machineId 三处与 machineid 文件保持本机原值，不制造设备漂移；Trae 须未运行）
pub fn cold_switch(ide: &IdeKind, bundle: &AccountBundle) -> io::Result<()> {
    let paths = IdePaths::detect(*ide)
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "IDE not installed"))?;
    let storage_path = paths.storage_json();

    // ① 备份
    let backup = storage_path.with_extension("json.switcher.bak");
    std::fs::copy(&storage_path, &backup)?;

    // ② 写登录态与随号身份（usertag/ServerData/ECDSA/遥测 dev/sqm/TinyStorage 随号）
    let mut sj = StorageJson::load(&paths)?;
    write_bundle_identity(&mut sj, bundle, None);
    sj.save()?;

    if let Some(snapshot) = bundle.tiny_storage.clone() {
        let mut ts = TinyStorage::load_or_default(&paths)?;
        ts.restore_data(snapshot);
        ts.save()?;
    }
    Ok(())
}

/// 多开实例的独立 user-data 目录（%APPDATA%\TraeIDE-Switcher\instances\<account_id>）
pub fn isolated_dir(account_id: &str) -> std::path::PathBuf {
    crate::core::vault::app_data_dir().join("instances").join(account_id)
}

/// 为多开实例准备独立数据目录：本机 storage.json 作模板 + 目标账号凭据/设备身份，
/// machineid 与 state.vscdb 一律**沿用本机现值**（SQLCipher 密钥来源，跨实例不能漂移）。
/// 返回该实例的 user-data 目录（供 --user-data-dir 使用）。
pub fn prepare_isolated_data(ide: &IdeKind, bundle: &AccountBundle) -> io::Result<std::path::PathBuf> {
    let paths = IdePaths::detect(*ide)
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "IDE not installed"))?;
    let dir = isolated_dir(&bundle.id);
    let global = dir.join("User").join("globalStorage");
    std::fs::create_dir_all(&global)?;
    std::fs::create_dir_all(dir.join("aha"))?;

    // machineid：锚定本机原值（新实例缺失该文件会自行生成新 UUID，导致设备身份漂移）
    if let Ok(mid) = std::fs::read_to_string(paths.machineid_file()) {
        std::fs::write(dir.join("machineid"), mid)?;
    }
    // storage.json：以本机文件为模板（继承其他设置），再覆盖为本账号登录态
    let src = paths.storage_json();
    let dst = global.join("storage.json");
    if src.exists() {
        std::fs::copy(&src, &dst)?;
    } else {
        std::fs::write(&dst, "{}")?;
    }
    let mut sj = StorageJson::load_at(dst)?;
    write_bundle_identity(&mut sj, bundle, None);
    sj.save()?;

    if let Some(snapshot) = bundle.tiny_storage.clone() {
        let mut ts = TinyStorage::load_or_default_at(dir.join("aha").join("TinyStorage"))?;
        ts.restore_data(snapshot);
        ts.save()?;
    }
    Ok(dir)
}

/// ISO-8601 UTC（"2026-01-02T03:04:05.000Z"）-> epoch ms；解析失败返回 None
fn iso_to_epoch_ms(s: &str) -> Option<i64> {
    let num = |a: usize, z: usize| s.get(a..z)?.parse::<i64>().ok();
    let y = num(0, 4)?;
    let mo = num(5, 7)?;
    let d = num(8, 10)?;
    let h = num(11, 13)?;
    let mi = num(14, 16)?;
    let sec = num(17, 19)?;
    let y = if mo <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (mo + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    Some((days * 86_400 + h * 3600 + mi * 60 + sec) * 1000)
}

/// 到期时间解析：Trae 磁盘凭据的 expiredAt 既有 ISO 字符串也有 epoch 数字串
/// （实测 CN 为 "1793264917486"）。只认 ISO 会让临期预刷新对这类账号整体失效。
fn expiry_to_epoch_ms(s: &str) -> Option<i64> {
    let t = s.trim();
    if t.is_empty() {
        return None;
    }
    if t.bytes().all(|b| b.is_ascii_digit()) {
        let n: i64 = t.parse().ok()?;
        return Some(if n > 100_000_000_000 { n } else { n * 1000 });
    }
    iso_to_epoch_ms(t)
}

/// 凭据 access token 到期时刻（epoch 毫秒）；缺字段/解析失败返回 None
pub fn token_expiry_ms(auth_blob: &str) -> Option<i64> {
    let info = parse_auth_blob(auth_blob).ok()?;
    expiry_to_epoch_ms(&info.expired_at)
}

/// token 是否将在 window_ms 内过期（含已过期）；解析失败按未临期处理（不阻断切换）
pub fn token_near_expiry(auth_blob: &str, window_ms: i64) -> bool {
    let Some(info) = parse_auth_blob(auth_blob).ok() else { return false };
    if info.expired_at.is_empty() {
        return false;
    }
    match expiry_to_epoch_ms(&info.expired_at) {
        Some(at) => at - super::refresh::now_millis() <= window_ms,
        None => false,
    }
}
