// __sw 注入桥：对 Trae main.js 做特征锚补丁，暴露热换号入口
// 补丁点1：holder 类 setUserInfo(e,i){...fire({userInfo,forceLogout})} 定义处劫持捕获 this
// 补丁点2：main.js 尾部追加 globalThis.__sw 桥（提供 setUserInfo(json)）
// 两处均为精确文本手术，可逆、幂等、跨版本（特征锚=方法名+forceLogout 属性名）
use super::paths::IdePaths;
use std::io;

pub const BRIDGE_MARKER: &str = "/*__SW_BRIDGE_V1__*/";
pub const HIJACK_MARKER: &str = "/*__SW_HIJACK_V1__*/";

const TAIL_BRIDGE: &str = r#"
;(function(){globalThis.__sw={holder:null,setUserInfo:function(json){var u=JSON.parse(json);var h=globalThis.__sw.holder;if(!h)throw new Error("__sw holder not captured yet");h.setUserInfo(u);return true}}}());/*__SW_BRIDGE_V1__*/"#;

const HIJACK_SNIPPET: &str = "globalThis.__sw&&(globalThis.__sw.holder=this);/*__SW_HIJACK_V1__*/";

#[derive(Debug, PartialEq, Eq)]
pub enum InjectStatus {
    Patched,
    AlreadyPatched,
    Removed,
    NotPatched,
}

/// 定位 holder 类的 setUserInfo 定义：形态 `setUserInfo(x,y){` 且方法体 400 字符内含 `forceLogout`
fn find_holder_setuserinfo(src: &str) -> Option<usize> {
    let mut from = 0;
    while let Some(rel) = src[from..].find("setUserInfo(") {
        let start = from + rel;
        let after = start + "setUserInfo(".len();
        if let Some(close) = src[after..].find(')') {
            let params = &src[after..after + close];
            let is_two_ident = {
                let parts: Vec<&str> = params.split(',').collect();
                parts.len() == 2
                    && parts.iter().all(|p| {
                        let t = p.trim();
                        !t.is_empty()
                            && t.chars().next().unwrap().is_ascii_alphabetic()
                            && t.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '$')
                    })
            };
            let brace_at = after + close + 1;
            if is_two_ident
                && src[brace_at..].starts_with('{')
                && src[start..(start + 400).min(src.len())].contains("forceLogout")
            {
                return Some(brace_at + 1);
            }
        }
        from = start + 12;
    }
    None
}

/// 打补丁：尾部桥 + setUserInfo 劫持（幂等；Trae 须未运行；安装目录需管理员写权限）
pub fn patch(paths: &IdePaths) -> io::Result<InjectStatus> {
    patch_file(&paths.main_js())
}

pub fn patch_file(main: &std::path::Path) -> io::Result<InjectStatus> {
    let mut src = std::fs::read_to_string(main)?;
    if src.contains(BRIDGE_MARKER) && src.contains(HIJACK_MARKER) {
        return Ok(InjectStatus::AlreadyPatched);
    }
    // 保险备份（仅首次）
    let bak = main.with_extension("js.switcher.bak");
    if !bak.exists() {
        std::fs::copy(main, &bak)?;
    }
    let mut changed = false;
    if !src.contains(HIJACK_MARKER) {
        let pos = find_holder_setuserinfo(&src).ok_or_else(|| {
            io::Error::new(io::ErrorKind::NotFound, "特征锚未命中：setUserInfo holder（Trae 版本不支持）")
        })?;
        src.insert_str(pos, HIJACK_SNIPPET);
        changed = true;
    }
    if !src.contains(BRIDGE_MARKER) {
        src.push_str(TAIL_BRIDGE);
        changed = true;
    }
    if changed {
        std::fs::write(main, &src)?;
    }
    Ok(InjectStatus::Patched)
}

/// 卸补丁：精确删除注入文本（可逆、幂等）
pub fn unpatch(paths: &IdePaths) -> io::Result<InjectStatus> {
    unpatch_file(&paths.main_js())
}

pub fn unpatch_file(main: &std::path::Path) -> io::Result<InjectStatus> {
    let src = std::fs::read_to_string(main)?;
    if !src.contains(BRIDGE_MARKER) && !src.contains(HIJACK_MARKER) {
        return Ok(InjectStatus::NotPatched);
    }
    let restored = src.replace(TAIL_BRIDGE, "").replace(HIJACK_SNIPPET, "");
    std::fs::write(main, restored)?;
    Ok(InjectStatus::Removed)
}

pub fn is_patched(paths: &IdePaths) -> bool {
    std::fs::read_to_string(paths.main_js())
        .map(|s| s.contains(BRIDGE_MARKER) && s.contains(HIJACK_MARKER))
        .unwrap_or(false)
}

// ════════════════ TSW 热切换补丁链（shim 通道）════════════════
//
// main.js：
//   ① v2 handler 头部植入 marker 检测块（__TSW_REPLACE__）：
//      setUserInfo + ICDRS 设备态同步 + apiHost 切换 + 四层防登出 + telemetry 落盘守卫
//   ② IPC dispatcher 补 return（返回值链路修复）
//   ③ ICDRS.resolveDeviceId 暴露 globalThis.__sw_icdrs（设备寄存服务引用）
// workbench.desktop.main.js：
//   ④ 注册 _tsw.replaceUserInfo 命令（包 marker 走既有 refreshUserInfo IPC 链路）
//   ⑤ refreshUserInfo 补 return（同②）
//   ⑥ 完整性校验短路（isPure）
//
// 全部为精确文本手术，可逆、幂等、独立于 CDP __sw 桥。

pub const TSW_MARKER: &str = "__TSW_REPLACE__";
const TSW_V2_FLAG: &str = "/*__TSW_V2__*/";
const TSW_ICDRS_FLAG: &str = "/*__TSW_ICDRS__*/";
const TSW_RET_FLAG: &str = "/*__TSW_RET__*/";
const TSW_CMD_FLAG: &str = "/*__TSW_CMD__*/";
const TSW_WRET_FLAG: &str = "/*__TSW_WRET__*/";
const TSW_PURE_FLAG: &str = "/*__TSW_PURE__*/";
/// ⑨ 暴露进程内 ai-agent RPC 客户端到 globalThis
const TSW_AIC_FLAG: &str = "/*__TSW_AIC__*/";
/// ⑩ 注册 _tsw.chatRpc（只读 agent RPC 代理）
const TSW_RPC_FLAG: &str = "/*__TSW_RPC__*/";
/// ⑫ 注册 _tsw.prepareSwitch（换号前清空聊天侧 project_id 缓存）
const TSW_PREP_FLAG: &str = "/*__TSW_PREP__*/";
/// ⑪ 注册 _tsw.reloadAgent（换号后重建 agent 会话栈）
///
/// 当前代标记（第三代）：第一代只重建客户端；第二代补了进程重启但没有去抖/等健康，
/// 连续两次换号时第二次会撞上 SIGTERM 超时 → SIGKILL，把 agent 服务打死（16:40 实测）。
/// 第三代加了 30s 去抖 + 重启后等 3s 并轮询 startup error 再重建客户端。
/// 装过旧代的机器靠「命令 id 定位 + 整块剥掉 + 重插」迁移（见 find_reload_block）。
const TSW_RLD_FLAG: &str = "/*__TSW_RLD3__*/";

/// 旧版注入体遗留的裸 require 引用：main.js 是 ESM 入口，`require` 在 handler 作用域
/// 未定义，该语句一执行就抛 ReferenceError，handler 未进入 marker 分支即中断——
/// 热切换静默失败并回落冷切换（CDP 通道因局部定义了 require 变量才不受影响）。
const TSW_V2_REQ_BROKEN: &str = "var _tsw_req=globalThis.__sw_req||require;";
const TSW_V2_REQ_FIXED: &str =
    r#"var _tsw_req=globalThis.__sw_req||(typeof require==="function"?require:null);"#;

/// 修复磁盘上已注入体里的裸 require 引用（幂等）
fn repair_v2_require(src: &mut String) -> bool {
    if !src.contains(TSW_V2_REQ_BROKEN) {
        return false;
    }
    *src = src.replace(TSW_V2_REQ_BROKEN, TSW_V2_REQ_FIXED);
    true
}

/// v2 handler 头部 marker 检测块（{PROP} = userInfo 所在属性，intl=j / CN=userContext）
/// 逻辑移植自 windsurf_switcher 热切换 handler（实战验证版），去掉冗余日志。
pub const TSW_V2_BODY: &str = r#"var _tsw_req=globalThis.__sw_req||(typeof require==="function"?require:null);
if(t&&typeof t==="object"&&t.__TSW_REPLACE__){
try{
const svc=typeof this.setUserInfo==="function"?this:(this.{PROP}&&typeof this.{PROP}.setUserInfo==="function"?this.{PROP}:Object.getOwnPropertyNames(this).map(_pk=>this[_pk]).find(_pv=>_pv&&typeof _pv==="object"&&typeof _pv.setUserInfo==="function")||null);
if(!svc)throw new Error("setUserInfo target not found");
let deviceUpdated=false;
const d=t.device&&typeof t.device==="object"?t.device:null;
const dr=globalThis.__sw_icdrs||null;
let remoteDid="";let localDid="";
if(d){
const tiny=d.tinyDevice&&typeof d.tinyDevice==="object"?d.tinyDevice:null;
remoteDid=tiny&&typeof tiny.device_id_str==="string"&&tiny.device_id_str?tiny.device_id_str:(typeof d.devDeviceId==="string"&&d.devDeviceId?d.devDeviceId:"");
localDid=tiny&&typeof tiny.install_id_str==="string"&&tiny.install_id_str?tiny.install_id_str:(typeof d.machineId==="string"&&d.machineId?d.machineId:"");
if(dr&&remoteDid){try{dr.j=remoteDid;}catch(_){}try{dr.m=remoteDid;}catch(_){}try{dr.deviceId=remoteDid;}catch(_){}try{dr.remoteDeviceId=remoteDid;}catch(_){}deviceUpdated=true;}
if(dr&&localDid){try{dr.n=localDid;}catch(_){}try{dr.localDeviceId=localDid;}catch(_){}}
if(remoteDid){
globalThis.__sw_did=remoteDid;
try{for(const _pk of Object.getOwnPropertyNames(this)){const _pv=this[_pk];if(_pv&&typeof _pv==="object"&&"_cachedDeviceInfo" in _pv){_pv._cachedDeviceInfo=null;}}}catch(_){}
try{const _ahaMod=(globalThis.__sw_req||require)("electron/main");const _aha=_ahaMod&&_ahaMod.ahaDeviceService;if(_aha&&_aha.deviceId&&typeof _aha.deviceId==="object"){try{_aha.deviceId.deviceId=remoteDid;}catch(_){}}}catch(_){}
}
for(const _k of Object.getOwnPropertyNames(this)){try{const _v=this[_k];if(_v&&typeof _v==="object"&&typeof _v.getDeviceId==="function"&&typeof _v.getCommonApiParams==="function"&&remoteDid){try{_v.b=remoteDid;}catch(_){}if(!_v.__sw_prod){_v.getDeviceId=function(){return globalThis.__sw_did||remoteDid||"0";};_v.__sw_prod=1;}}}catch(_){}}
}
if(t.userInfo&&t.userInfo.host){
try{const _hInfo=JSON.stringify({consoleHost:t.userInfo.consoleHost||"",apiHost:t.userInfo.host});
for(const _k of Object.getOwnPropertyNames(this)){const _v=this[_k];if(_v&&typeof _v==="object"){
if(typeof _v.setItem==="function"){try{_v.setItem("iCubeHostInfo",_hInfo);}catch(_){}}
if(typeof _v.store==="function"){try{_v.store("iCubeHostInfo",_hInfo,-1,1);}catch(_){}}
}}}catch(_){}
}
const _ls=this.logService;const _sl=(m)=>{try{_ls&&typeof _ls.info==="function"?_ls.info("[tsw] "+m):void 0;}catch(_){}};
globalThis.__sw_force_logout_block=true;
globalThis.__sw_hotswap_protect=true;
_sl("hotswap: shields up (force_logout_block=perm, hotswap_protect=30s)");
if(typeof this.verifyTokenInBackground==="function"&&!this.__sw_vt){const _ovt=this.verifyTokenInBackground.bind(this);this.verifyTokenInBackground=function(_t){if(globalThis.__sw_hotswap_protect){_sl("BLOCKED verifyTokenInBackground");return _t;}return _ovt(_t);};this.__sw_vt=1;}
if(typeof this.checkLoginToken==="function"&&!this.__sw_clt){const _oclt=this.checkLoginToken.bind(this);this.checkLoginToken=async function(_t){if(globalThis.__sw_hotswap_protect){_sl("BLOCKED checkLoginToken");return;}return _oclt(_t);};this.__sw_clt=1;}
for(const _pk of Object.getOwnPropertyNames(this)){try{const _pv=this[_pk];if(_pv&&typeof _pv==="object"&&typeof _pv.setUserInfo==="function"&&!_pv.__sw_g){const _o=_pv.setUserInfo.bind(_pv);_pv.setUserInfo=function(_i,_f){if(globalThis.__sw_force_logout_block&&(_i==null||_f===true)){return;}return _o(_i,_f);};_pv.__sw_g=1;}}catch(_){}}
if(!svc.__sw_g){const _o=svc.setUserInfo.bind(svc);svc.setUserInfo=function(_i,_f){if(globalThis.__sw_force_logout_block&&(_i==null||_f===true)){return;}return _o(_i,_f);};svc.__sw_g=1;}
if(this.broadcastToCodeWindows&&!this.__sw_bc){const _obc=this.broadcastToCodeWindows.bind(this);this.broadcastToCodeWindows=function(_ch,_d){if(globalThis.__sw_force_logout_block&&typeof _ch==="string"){if(_ch.indexOf("show-auth-invalid-dialog")!==-1){return;}if(_ch.indexOf("send-userInfo")!==-1&&_d==null){return;}}return _obc(_ch,_d);};this.__sw_bc=1;}
svc.setUserInfo(t.userInfo);
setTimeout(()=>{
try{
const _u=svc.userInfo;
if(_u&&_u.refreshToken&&_u.account&&_u.account.scope){
const _s=Date.parse(_u.expiredAt)-Date.now();
if(_s<=0||_s<=300000){
_sl("token expired/near-expiry ("+_s+"ms), proactive refresh");
if(typeof this.refreshUserInfoByToken==="function"){
this.refreshUserInfoByToken(_u.refreshToken,"REFRESH_TOKEN",_u.account.scope).then(()=>{_sl("proactive refresh ok, lifting protect");globalThis.__sw_hotswap_protect=false;}).catch((_e)=>{_sl("proactive refresh failed: "+_e+", lifting anyway");globalThis.__sw_hotswap_protect=false;});
return;
}
}
}
}catch(_e){}
_sl("hotswap_protect lifted");
globalThis.__sw_hotswap_protect=false;
},30000);
if(d){
if(d.hasDeviceIdUpdated===true){for(const _pk of Object.getOwnPropertyNames(this)){const _pv=this[_pk];try{if(_pv&&typeof _pv==="object"&&typeof _pv.setItem==="function")_pv.setItem("has_device_id_updated_to_aha","true");if(_pv&&typeof _pv==="object"&&typeof _pv.store==="function")_pv.store("has_device_id_updated_to_aha",!0,-1,1);}catch(_){}}}
if(d.machineId||d.devDeviceId||d.sqmId){
if(typeof globalThis.__SWITCHER_TELEMETRY==="undefined")globalThis.__SWITCHER_TELEMETRY={};
if(d.machineId)globalThis.__SWITCHER_TELEMETRY["telemetry.machineId"]=d.machineId;
if(d.devDeviceId)globalThis.__SWITCHER_TELEMETRY["telemetry.devDeviceId"]=d.devDeviceId;
if(d.sqmId)globalThis.__SWITCHER_TELEMETRY["telemetry.sqmId"]=d.sqmId;
if(typeof _tsw_req==="function"&&_tsw_req&&!globalThis.__sw_fs_patched){
try{const _fs=_tsw_req("fs");const _orig=_fs.writeFileSync;
_fs.writeFileSync=function(p,d2,...a){
if(typeof p==="string"&&p.indexOf("storage.json")!==-1&&typeof d2==="string"&&globalThis.__SWITCHER_TELEMETRY){
try{const _j=JSON.parse(d2);let _m=false;
for(const _tk of Object.keys(globalThis.__SWITCHER_TELEMETRY)){const _tv=globalThis.__SWITCHER_TELEMETRY[_tk];if(_tk==="switcher.deviceIdStr"&&!/^\d+$/.test(String(_tv)))continue;if(_j[_tk]===undefined){_j[_tk]=_tv;_m=true;}else if(_j[_tk]!==_tv){_j[_tk]=_tv;_m=true;}}
if(_m)d2=JSON.stringify(_j);
}catch(_){}
}return _orig.call(this,p,d2,...a);};
globalThis.__sw_fs_patched=1;
try{const _pwp=_fs.promises&&_fs.promises.writeFile;if(typeof _pwp==="function"){const _op=_pwp.bind(_fs.promises);_fs.promises.writeFile=async function(p,d2,...a){
if(typeof p==="string"&&p.indexOf("storage.json")!==-1&&typeof d2==="string"&&globalThis.__SWITCHER_TELEMETRY){
try{const _j=JSON.parse(d2);let _m=false;for(const _tk of Object.keys(globalThis.__SWITCHER_TELEMETRY)){const _tv=globalThis.__SWITCHER_TELEMETRY[_tk];if(_tk==="switcher.deviceIdStr"&&!/^\d+$/.test(String(_tv)))continue;if(_j[_tk]===undefined){_j[_tk]=_tv;_m=true;}else if(_j[_tk]!==_tv){_j[_tk]=_tv;_m=true;}}if(_m)d2=JSON.stringify(_j);}catch(_){}
}return _op(p,d2,...a);};}
}catch(_){}
}catch(_){}
}
const _svcs=[];
for(const _k of Object.getOwnPropertyNames(this)){const _v=this[_k];if(!_v||typeof _v!=="object")continue;
if(typeof _v.getItem==="function"&&!_v.__sw_p){
const _p=Object.getPrototypeOf(_v);
if(_p&&typeof _p.getItem==="function"&&!_p.__sw_p){const _o=_p.getItem;_p.getItem=function(k,...a){if(globalThis.__SWITCHER_TELEMETRY&&typeof k==="string"&&globalThis.__SWITCHER_TELEMETRY[k]!==undefined)return globalThis.__SWITCHER_TELEMETRY[k];return _o.call(this,k,...a)};_p.__sw_p=1}
if(_p&&typeof _p.setItem==="function"&&!_p.__sw_si){const _o2=_p.setItem;_p.setItem=function(k,v,...a){if(globalThis.__SWITCHER_TELEMETRY&&typeof k==="string"&&globalThis.__SWITCHER_TELEMETRY[k]!==undefined)return _o2.call(this,k,globalThis.__SWITCHER_TELEMETRY[k],...a);return _o2.call(this,k,v,...a)};_p.__sw_si=1}
_v.__sw_p=1;_svcs.push(_v);}
if(typeof _v.store==="function"&&!_v.__sw_st){
const _p=Object.getPrototypeOf(_v);
if(_p&&typeof _p.store==="function"&&!_p.__sw_st){const _o3=_p.store;_p.store=function(k,v,...a){if(globalThis.__SWITCHER_TELEMETRY&&typeof k==="string"&&globalThis.__SWITCHER_TELEMETRY[k]!==undefined)return _o3.call(this,k,globalThis.__SWITCHER_TELEMETRY[k],...a);return _o3.call(this,k,v,...a)};_p.__sw_st=1}
_v.__sw_st=1;if(!_v.__sw_p)_svcs.push(_v);}
}
for(const _s of _svcs){
if(d.machineId){try{_s.setItem("telemetry.machineId",d.machineId)}catch(_){}try{if(_s.store)_s.store("telemetry.machineId",d.machineId,-1,1)}catch(_){}}
if(d.devDeviceId){try{_s.setItem("telemetry.devDeviceId",d.devDeviceId)}catch(_){}try{if(_s.store)_s.store("telemetry.devDeviceId",d.devDeviceId,-1,1)}catch(_){}}
if(d.sqmId){try{_s.setItem("telemetry.sqmId",d.sqmId)}catch(_){}try{if(_s.store)_s.store("telemetry.sqmId",d.sqmId,-1,1)}catch(_){}}
}
}
}
let _diag={hasICDRS:!!globalThis.__sw_icdrs,remoteDid:remoteDid,drJ:dr?dr.j:null,drM:dr?dr.m:null,swDid:globalThis.__sw_did};
return{success:true,deviceUpdated:deviceUpdated,diag:_diag};
}catch(err){return{success:false,error:String(err&&err.message||err)};}
}
"#;

// ════════════════ 会话/任务记录归属补丁（对话记录随号保留）════════════════
//
// 根因（真机取证 CN 3.3.100）：
//   AI 会话与任务列表按 (user_id, project_id) 归属。project_id 由 ai-modules-chat 的
//   createProject 取得后写进 project store（renderer.log 实测：每个账号各自持有一个
//   project_id，跨重启复用，如 498492885962313→6acf2a58…、1910309375982379→6ad169ff…）。
//   切号后客户端用自己的 project 查询会话列表 → 上一个账号的对话记录不可见。
// 修法：把 project_id 固定成一个共享值（localStorage 持久化，首次运行即固化），
//   所有账号落进同一 project，切号前后对话记录并存；首次运行取到的仍是本账号原值，
//   因此现有对话不会丢。
//   ⑦ workbench：client_info.project_id（所有请求的 project 出口）
//   ⑧ ai-modules-chat：project store 的 setProjectId（会话查询真正的取值处）
pub const TSW_PID_FLAG: &str = "__TSW_PID__";

const PID_PIN_TMPL: &str = r#"(function(_v){/*__TSW_PID__*/var _p=globalThis.__tsw_pid;if(typeof _p==="undefined"){_p=null;try{var _ls=typeof localStorage!=="undefined"?localStorage:null;if(_ls){_p=_ls.getItem("__tsw_project_id")||null;}}catch(_){}}if(_p&&/^[0-9a-f]{24}$/.test(_p))return _p;if(typeof _v==="string"&&/^[0-9a-f]{24}$/.test(_v)){globalThis.__tsw_pid=_v;try{var _l2=typeof localStorage!=="undefined"?localStorage:null;if(_l2)_l2.setItem("__tsw_project_id",_v);}catch(_){}}return _v;})(__ARG__)"#;

/// ⑧ 已废弃（2026-10-25）：agent 按 (user_id, project_id) 校验项目归属，
/// 把 project_id 钉成跨账号共享值只会让新账号查会话时 `project not found`。
/// 现在改为「会话按账号隔离」：pin 表达式降级成原样返回（`return _v;`），不再改写 project_id。
const TSW_PID_OFF_FLAG: &str = "/*__TSW_PID_OFF__*/";

/// 把已注入的 pin 表达式就地降级成 passthrough（已打过 ⑦/⑧ 的机器靠它迁移）
fn disable_pid_pin(src: &mut String) -> bool {
    if src.contains(TSW_PID_OFF_FLAG) {
        return false;
    }
    let mut changed = false;
    let mut from = 0usize;
    while let Some(rel) = src[from..].find(TSW_PID_FLAG) {
        // TSW_PID_FLAG 只是裸名（不带注释定界符），插入点要越过紧跟的 `*/`，
        // 否则会把 `return _v;` 写进注释里（2026-10-25 踩过，直接让模块语法崩掉）
        let mut at = from + rel + TSW_PID_FLAG.len();
        if src[at..].starts_with("*/") {
            at += 2;
        }
        src.insert_str(at, &format!("return _v;{TSW_PID_OFF_FLAG}"));
        from = at + 1;
        changed = true;
    }
    changed
}

fn pid_pin(arg: &str) -> String {
    PID_PIN_TMPL.replace("__ARG__", arg)
}

/// ⑦ workbench：`project_id:e.context?.project_id` → 固定值表达式
fn patch_pid_workbench(src: &mut String) -> Result<bool, String> {
    if src.contains(TSW_PID_FLAG) {
        return Ok(false);
    }
    let needle = "project_id:e.context?.project_id";
    let pos = src.find(needle).ok_or("特征锚未命中：client_info.project_id")?;
    let arg = "e.context?.project_id";
    let at = pos + "project_id:".len();
    let pinned = pid_pin(arg);
    src.replace_range(at..at + arg.len(), &pinned);
    Ok(true)
}

/// ⑧ ai-modules-chat：`_projectStore.actions.setProjectId(<o>.project_id,<o>.real_project_id)`
fn patch_pid_chat(src: &mut String) -> Result<bool, String> {
    if src.contains(TSW_PID_FLAG) {
        return Ok(false);
    }
    let needle = "_projectStore.actions.setProjectId(";
    let pos = src.find(needle).ok_or("特征锚未命中：project store setProjectId")?;
    let args_at = pos + needle.len();
    let close = src[args_at..].find(')').ok_or("setProjectId 参数未闭合")?;
    let args = src[args_at..args_at + close].to_string();
    let parts: Vec<&str> = args.split(',').collect();
    if parts.len() != 2 {
        return Err("setProjectId 参数数量异常".into());
    }
    let obj = parts[0]
        .strip_suffix(".project_id")
        .filter(|o| !o.is_empty() && o.bytes().all(is_ident_byte))
        .ok_or("setProjectId 首参形态异常")?;
    if parts[1].trim() != format!("{obj}.real_project_id") {
        return Err("setProjectId 次参形态异常".into());
    }
    let replacement = format!(
        "{},{}",
        pid_pin(&format!("{obj}.project_id")),
        pid_pin(&format!("{obj}.real_project_id"))
    );
    src.replace_range(args_at..args_at + close, &replacement);
    Ok(true)
}

/// ⑫ 聊天模块：把 project store 抓到 globalThis.__tsw_projectStore。
///
/// 为什么需要：换号后 store 里还留着上一个账号的 project_id，`createProject(false)` 直接返回它，
/// 接着 `load session` 用这个 id 去查会话 —— agent 按 (user_id, project_id) 校验归属，直接
/// `project not found`，面板就回落「服务启动异常」。换号前必须把这个缓存清掉。
const TSW_PSTORE_FLAG: &str = "/*__TSW_PSTORE__*/";

fn patch_capture_pstore(src: &mut String) -> Result<(), String> {
    if src.contains(TSW_PSTORE_FLAG) {
        return Ok(());
    }
    let anchor = "this._projectStore.actions.setIsCreatingProject(!0)";
    let at = src
        .find(anchor)
        .ok_or("特征锚未命中：projectStore.setIsCreatingProject")?;
    src.insert_str(
        at,
        &format!("(globalThis.__tsw_projectStore=this._projectStore){TSW_PSTORE_FLAG},"),
    );
    Ok(())
}

/// ⑫b 聊天模块：在 `createProject` 函数体开头抓 project store。
///
/// ⑫ 插在 `setIsCreatingProject` 前，而那一行在 `if(t&&!e)return` 之后。
/// Trae 重启后 store 被 `syncProjectStores` 填上，`createProject(false)` 永远 early return，
/// ⑫ 的捕获跑不到，换号清缓存拿不到 store。进函数就抓一次。
const TSW_PSTORE_HEAD_FLAG: &str = "/*__TSW_PSTORE_HEAD__*/";

fn patch_capture_pstore_head(src: &mut String) -> Result<(), String> {
    if src.contains(TSW_PSTORE_HEAD_FLAG) {
        return Ok(());
    }
    let anchor = "this._projectStore.actions.setIsCreatingProject(!0)";
    let anchor_at = src
        .find(anchor)
        .ok_or("特征锚未命中：projectStore.setIsCreatingProject")?;
    let needle = "createProject(";
    let mut window = anchor_at;
    let fn_at = loop {
        let at = src[..window]
            .rfind(needle)
            .ok_or("特征锚未命中：createProject 函数头")?;
        if anchor_at - at >= 400 {
            return Err("createProject 距 setIsCreatingProject 过远".into());
        }
        // 后面必须紧跟 createProject(，且前一字节不是标识符：排除 createProjectWithTimeout / xxxcreateProject
        let followed = src[at..].starts_with(needle);
        let boundary = at == 0 || !is_ident_byte(src.as_bytes()[at - 1]);
        if followed && boundary {
            break at;
        }
        if at == 0 {
            return Err("特征锚未命中：createProject 函数头".into());
        }
        window = at;
    };
    let brace_rel = src[fn_at..]
        .find('{')
        .filter(|&d| d > 0 && d < 40)
        .ok_or("createProject 函数体起点未找到")?;
    if src.as_bytes()[fn_at + brace_rel - 1] != b')' {
        return Err("createProject 函数体起点形态异常".into());
    }
    src.insert_str(
        fn_at + brace_rel + 1,
        &format!("(globalThis.__tsw_projectStore=this._projectStore);{TSW_PSTORE_HEAD_FLAG}"),
    );
    Ok(())
}

/// ⑬ 聊天模块：`biz_project_id` 加用户后缀 —— 会话按账号隔离的真正实现。
///
/// 背景（2026-10-25 18:04 实测）：服务端 `get_or_create_project_v2` 按 `biz_project_id`
/// （= 工作区 id）取/建项目，**不分用户**。所以去掉 ⑧ 的 pin 后，换号拿到的还是同一个
/// project，会话列表照样混账号。真正要隔离，必须让每个账号有独立的 project：
/// `biz_project_id: t` → `biz_project_id: t + "__tsw_u_" + userId`。
/// 代价：各账号旧会话（挂在原共享 project 下）不再可见。
const TSW_PIDSALT_FLAG: &str = "/*__TSW_PIDSALT__*/";

fn patch_salt_biz_pid(src: &mut String) -> Result<(), String> {
    if src.contains(TSW_PIDSALT_FLAG) {
        return Ok(());
    }
    let mut n = 0usize;
    // prepareProjectData：user 变量名 e；createProjectForWorkspaceMigration：user 变量名 i
    for (pat, user) in [
        ("biz_project_id:t,user_id:e.userId", "e"),
        ("biz_project_id:t,user_id:i.userId", "i"),
    ] {
        if let Some(at) = src.find(pat) {
            let salted = format!("biz_project_id:t+\"__tsw_u_\"+{user}.userId{TSW_PIDSALT_FLAG},user_id:{user}.userId");
            let end = at + "biz_project_id:t".len();
            src.replace_range(at..end, &salted[..salted.find(",user_id").unwrap()]);
            n += 1;
        }
    }
    if n == 0 {
        return Err("特征锚未命中：biz_project_id 构造点".into());
    }
    Ok(())
}

/// ⑭ 聊天模块：`initChatView` 撞上可恢复错误时自动重试（v2）。
///
/// 换号后面板立刻按新 userId 重跑 initChatView，而 agent（CKG）要按新账号重拉 boot config，
/// 这段空窗（2026-10-26 16:23 实测约 4s）里 getSessions 返回 503 boot_config_not_ready。
/// 另外 `createProject(false)` 会返回缓存的旧账号 project_id，createSession 报 `project not found`。
/// 重试改为 `initChatView(true)`，强制 `createProject(true)` 绕过缓存；`project not found` 同样可恢复。
/// 原版 catch 只把错误挂到面板上且不重试。重试期间直接 return，跳过之后的 `setLoading(false)`，
/// 面板保持加载态而不是错误态。
const TSW_INITRETRY_FLAG: &str = "/*__TSW_INITRETRY__*/";
const TSW_INITRETRY2_FLAG: &str = "/*__TSW_INITRETRY2__*/";
const INIT_RETRY_MAX: u32 = 8;
const INIT_RETRY_DELAY_MS: u32 = 1500;

fn patch_init_retry(src: &mut String) -> Result<(), String> {
    if src.contains(TSW_INITRETRY2_FLAG) {
        return Ok(());
    }
    if src.contains(TSW_INITRETRY_FLAG) {
        let flag_at = src.find(TSW_INITRETRY_FLAG).ok_or("v1 重试标记定位失败")?;
        let v1_head = "if(/boot_config_not_ready/.test(String(";
        let v1_at = src[..flag_at]
            .rfind(v1_head)
            .ok_or("v1 重试片段起始未找到")?;
        src.replace_range(v1_at..flag_at + TSW_INITRETRY_FLAG.len(), "");
    }
    // initChatView 本体：`<fn>=(0,<x>.A)(async(e=!1)=>{…<log>.info("[ai-chat] initChatView loggedIn:"`
    let login_log = src
        .find("\"[ai-chat] initChatView loggedIn:\"")
        .ok_or("特征锚未命中：initChatView loggedIn")?;
    let head_at = src[..login_log]
        .rfind("(async(")
        .filter(|&p| login_log - p < 400)
        .ok_or("特征锚未命中：initChatView 函数头")?;
    let wrap_at = src[..head_at]
        .rfind("=(0,")
        .filter(|&p| head_at - p < 40)
        .ok_or("特征锚未命中：initChatView 包装")?;
    let (_, init_fn) = ident_before(src.as_bytes(), wrap_at).ok_or("initChatView 函数名解析失败")?;

    // 成功出口：`,<log>.info("[ai-chat] initChatView load session done")` 前清零计数
    let done_anchor = ".info(\"[ai-chat] initChatView load session done\")";
    let done_at = src.find(done_anchor).ok_or("特征锚未命中：initChatView load session done")?;
    let (done_start, _) = ident_before(src.as_bytes(), done_at).ok_or("load session done 日志对象解析失败")?;
    if !matches!(src.as_bytes()[done_start - 1], b',' | b';' | b'{') {
        return Err("load session done 前缀形态异常".into());
    }

    // 失败出口：catch 里的 `<log>.error("[ai-chat] initChatView error:",<err>)`
    let err_anchor = ".error(\"[ai-chat] initChatView error:\",";
    let err_at = src.find(err_anchor).ok_or("特征锚未命中：initChatView error")?;
    let (err_start, logger) = ident_before(src.as_bytes(), err_at).ok_or("initChatView error 日志对象解析失败")?;
    // 要插入的是 if 语句，前面必须是语句边界
    if !matches!(src.as_bytes()[err_start - 1], b';' | b'{' | b'}') {
        return Err("initChatView error 前缀形态异常".into());
    }
    let arg_at = err_at + err_anchor.len();
    let err_var = src[arg_at..]
        .find(')')
        .map(|n| &src[arg_at..arg_at + n])
        .filter(|v| !v.is_empty() && v.bytes().all(is_ident_byte))
        .ok_or("initChatView error 参数形态异常")?
        .to_string();
    if done_start > err_start {
        return Err("initChatView 成功/失败出口顺序异常".into());
    }

    let retry = format!(
        "if(/boot_config_not_ready|project not found/.test(String({err_var}))){{let __n=(globalThis.__tsw_initRetry||0)+1;\
if(__n<={INIT_RETRY_MAX}){{globalThis.__tsw_initRetry=__n;\
{logger}.warn(\"[ai-chat] initChatView recoverable error, tsw retry\",__n,String({err_var}).slice(0,120));\
setTimeout(()=>{{{init_fn}(!0)}},{INIT_RETRY_DELAY_MS});return}}globalThis.__tsw_initRetry=0}}{TSW_INITRETRY2_FLAG}"
    );
    let reset_done = src.contains("globalThis.__tsw_initRetry=0,");
    src.insert_str(err_start, &retry);
    if !reset_done {
        src.insert_str(done_start, "globalThis.__tsw_initRetry=0,");
    }
    Ok(())
}

/// ⑭ 失败只降级：重试补丁缺失时面板仍是原版行为，不应连累会话归属补丁落盘
fn try_patch_init_retry(src: &mut String) -> bool {
    match patch_init_retry(src) {
        Ok(()) => true,
        Err(e) => {
            crate::core::logger::log(
                crate::core::logger::Level::Warn,
                crate::core::logger::Source::System,
                format!("换号后聊天面板自动重试未生效（面板可能停在「服务启动异常」，手点重试即可）: {e}"),
            );
            false
        }
    }
}

/// ⑮ 聊天模块：v2 会话服务把 503 / boot_config_not_ready 视为可重试。
///
/// v2 通用重试把带正整数 code 的错误判为不可重试，换号后 boot config 空窗的 503 因此直接失败、
/// 面板「服务启动异常」。改为 503 / boot_config_not_ready 可重试，沿用原有 1s×10 次。
const TSW_V2RETRY_FLAG: &str = "/*__TSW_V2RETRY__*/";

fn patch_v2_retry_503(src: &mut String) -> Result<(), String> {
    if src.contains(TSW_V2RETRY_FLAG) {
        return Ok(());
    }
    let anchor = "failed without retry`";
    let at = src
        .find(anchor)
        .ok_or("特征锚未命中：v2 SessionListService failed without retry")?;
    let mut lookback = at.saturating_sub(80);
    while lookback < at && !src.is_char_boundary(lookback) {
        lookback += 1;
    }
    if !src[lookback..at].contains("[ai-chat/v2][SessionListService]") {
        return Err("特征锚未命中：v2 SessionListService failed without retry".into());
    }
    let head = "if(!function(";
    let fn_at = src[..at]
        .rfind(head)
        .ok_or("特征锚未命中：v2 可重试判定 if(!function(")?;
    if at - fn_at >= 300 {
        return Err("v2 可重试判定距 failed without retry 过远".into());
    }
    let param_from = fn_at + head.len();
    let paren = src[param_from..]
        .find(')')
        .ok_or("v2 可重试判定函数形参形态异常")?;
    let param = src[param_from..param_from + paren].to_string();
    if param.is_empty() || !param.bytes().all(is_ident_byte) {
        return Err("v2 可重试判定函数形参形态异常".into());
    }
    let brace_at = param_from + paren + 1;
    if src.as_bytes().get(brace_at) != Some(&b'{') {
        return Err("v2 可重试判定函数体起点形态异常".into());
    }
    let insert_at = brace_at + 1;
    let mut window_end = (insert_at + 120).min(src.len());
    while window_end > insert_at && !src.is_char_boundary(window_end) {
        window_end -= 1;
    }
    if !src[insert_at..window_end].contains("\"code\"in ") {
        return Err("v2 可重试判定函数形态异常：缺少 \"code\"in".into());
    }
    let snippet = format!(
        "if({param}&&({param}.code===503||/boot_config_not_ready/.test(String({param}.message||\"\"))))return!0;{TSW_V2RETRY_FLAG}"
    );
    src.insert_str(insert_at, &snippet);
    Ok(())
}

/// ⑮ 失败只降级：v2 503 重试缺失时仍保留其余会话补丁
fn try_patch_v2_retry_503(src: &mut String) -> bool {
    match patch_v2_retry_503(src) {
        Ok(()) => true,
        Err(e) => {
            crate::core::logger::log(
                crate::core::logger::Level::Warn,
                crate::core::logger::Source::System,
                format!("v2 会话服务 503 重试未生效（换号后 boot config 空窗的 503 可能直接失败，面板停在「服务启动异常」）: {e}"),
            );
            false
        }
    }
}

/// ⑯ 聊天模块：v2 用新账号初始化换号前打开的旧会话时，不把「session not found」钉成错误页。
///
/// 旧版 `loadPreviousSessionOrCreateNewIfNeeded` 在 v2 开启时只写旧 store，不调用 `switchToSession`；
/// solo 视图的当前会话来自路由。catch 里 `setInitError` 会让该会话一直渲染错误页（刷新/重启服务）。
/// 这条错误说明会话不属于当前账号：清掉 initError、本地卸掉该会话（不打服务端删除），
/// 再走 NavigatePort.navigateHomePage 离开它。其它错误仍原样 `setInitError` 并 throw。
const TSW_V2NOTFOUND_FLAG: &str = "/*__TSW_V2NOTFOUND__*/";

fn patch_v2_session_not_found(src: &mut String) -> Result<(), String> {
    if src.contains(TSW_V2NOTFOUND_FLAG) {
        return Ok(());
    }
    let anchor = "[ChatSessionService] Session init error (Lite):";
    let at = src
        .find(anchor)
        .ok_or("特征锚未命中：Session init error (Lite)")?;
    if src[at + anchor.len()..].contains(anchor) {
        return Err("Session init error (Lite) 锚出现多次".into());
    }
    if at == 0 || src.as_bytes()[at - 1] != b'"' {
        return Err("Lite 错误日志锚形态异常".into());
    }
    let fn_at = src[..at]
        .rfind("async initSessionForSoloLite(")
        .ok_or("特征锚未命中：async initSessionForSoloLite")?;
    if at - fn_at > 12_000 {
        return Err("Lite init catch 距 initSessionForSoloLite 过远".into());
    }
    let win_start = at.saturating_sub(180);
    let catch_rel = src[win_start..at]
        .rfind("catch(")
        .ok_or("特征锚未命中：Lite init catch(")?;
    let err_from = win_start + catch_rel + "catch(".len();
    let (mut pos, err_var) = take_ident(src, err_from, "Lite init catch 形参")?;
    pos = expect_lit(src, pos, "){if(!")?;
    let (pred_end, pred) = take_ident(src, pos, "Lite init shouldCommit")?;
    pos = expect_lit(src, pred_end, "())return;")?;
    let (log_end, _logger) = take_ident(src, pos, "Lite init logger")?;
    pos = expect_lit(src, log_end, ".logger.error(\"")?;
    if pos != at {
        return Err("Lite init logger.error 与错误锚不连续".into());
    }
    pos = expect_lit(src, at + anchor.len(), "\",")?;
    pos = expect_lit(src, pos, &err_var)?;
    pos = expect_lit(src, pos, ");let ")?;
    let (msg_end, msg) = take_ident(src, pos, "Lite init 错误消息变量")?;
    pos = expect_lit(
        src,
        msg_end,
        &format!("={err_var} instanceof Error?{err_var}.message:\"Unknown error\","),
    )?;
    let (code_end, code) = take_ident(src, pos, "Lite init 错误码变量")?;
    let throw_at = expect_lit(src, code_end, &format!("={err_var}?.code;"))?;
    pos = expect_lit(src, throw_at, "throw ")?;
    let set_key = ".setInitError(";
    let set_rel = src[pos..]
        .find(set_key)
        .filter(|&d| d > 0 && d < 80)
        .ok_or("Lite init setInitError 形态异常")?;
    let recv = src[pos..pos + set_rel].to_string();
    if !is_member_expr(&recv) {
        return Err("Lite init setInitError 接收者形态异常".into());
    }
    pos += set_rel + set_key.len();
    let (sid_end, sid) = take_ident(src, pos, "Lite init 会话 id")?;
    pos = expect_lit(src, sid_end, &format!(",{{message:{msg},env:"))?;
    let (env_end, _env) = take_ident(src, pos, "Lite init env")?;
    expect_lit(src, env_end, &format!(",code:{code}}}),{err_var}"))?;
    let _ = pred;

    let life = "getSessionLifecyclePort(){return ";
    let life_at = src
        .find(life)
        .ok_or("特征锚未命中：getSessionLifecyclePort")?;
    if src[life_at + life.len()..].contains(life) {
        return Err("getSessionLifecyclePort 出现多次".into());
    }
    if !src.contains("async deleteSession(") {
        return Err("特征锚未命中：async deleteSession".into());
    }
    let (xt_end, xt) = take_ident(src, life_at + life.len(), "port registry")?;
    let after_xt = expect_lit(src, xt_end, ".XT.tryResolve(")?;
    let (tok_end, tok) = take_ident(src, after_xt, "port token")?;
    let life_end = expect_lit(src, tok_end, ".F.SessionLifecycle)}")?;
    if src.contains("tswV2Navigate(") {
        return Err("已有 tswV2Navigate 但缺少 v2 not-found 标记".into());
    }

    let guard = format!(
        "if(/session not found/i.test({msg})){{{recv}.clearInitError({sid});try{{await this.deleteSession({sid},!0)}}catch(__tswNf){{}}try{{this.tswV2Navigate()?.navigateHomePage?.()}}catch(__tswNf){{}}return}}{TSW_V2NOTFOUND_FLAG}"
    );
    let method = format!("tswV2Navigate(){{return {xt}.XT.tryResolve({tok}.F.Navigate)}}");
    // throw 在方法定义之后：先插入 catch，方法下标才不会位移
    if throw_at <= life_end {
        return Err("Lite init throw 不在 getSessionLifecyclePort 之后".into());
    }
    src.insert_str(throw_at, &guard);
    src.insert_str(life_end, &method);
    Ok(())
}

/// ⑯ 失败只降级：补丁缺失时换号后旧会话可能停在「服务启动异常」，可手动新建会话
fn try_patch_v2_session_not_found(src: &mut String) -> bool {
    match patch_v2_session_not_found(src) {
        Ok(()) => true,
        Err(e) => {
            crate::core::logger::log(
                crate::core::logger::Level::Warn,
                crate::core::logger::Source::System,
                format!("换号后旧会话可能停在「服务启动异常」，可手动新建会话: {e}"),
            );
            false
        }
    }
}

fn take_ident(src: &str, at: usize, what: &str) -> Result<(usize, String), String> {
    let b = src.as_bytes();
    if at >= b.len() || !is_ident_byte(b[at]) {
        return Err(format!("{what}形态异常"));
    }
    let mut end = at + 1;
    while end < b.len() && is_ident_byte(b[end]) {
        end += 1;
    }
    Ok((end, src[at..end].to_string()))
}

fn expect_lit(src: &str, at: usize, lit: &str) -> Result<usize, String> {
    if src[at..].starts_with(lit) {
        Ok(at + lit.len())
    } else {
        Err(format!("形态异常：未遇到 {lit}"))
    }
}

fn is_member_expr(s: &str) -> bool {
    !s.is_empty()
        && s.split('.').all(|p| !p.is_empty() && p.bytes().all(is_ident_byte))
}

fn ensure_bak_named(file: &std::path::Path) -> io::Result<()> {
    let name = file.file_name().unwrap_or_default().to_string_lossy().to_string();
    let bak = file.with_file_name(format!("{name}.switcher.bak"));
    if !bak.exists() {
        std::fs::copy(file, &bak)?;
    }
    Ok(())
}

/// 会话归属补丁（ai-modules-chat：project store 固定）
pub fn patch_chat(paths: &IdePaths) -> io::Result<InjectStatus> {
    patch_chat_file(&paths.chat_module_js())
}

pub fn patch_chat_file(chat: &std::path::Path) -> io::Result<InjectStatus> {
    if !chat.exists() {
        return Err(io::Error::new(io::ErrorKind::NotFound, "未找到 ai-modules-chat 模块"));
    }
    let mut src = std::fs::read_to_string(chat)?;
    let pid_done = src.contains(TSW_PID_FLAG);
    let store_done = src.contains(TSW_PSTORE_FLAG);
    if pid_done && store_done {
        let mut dirty = disable_pid_pin(&mut src);
        // ⑬（biz_project_id 加盐）是后来加的，老注入体要补
        if !src.contains(TSW_PIDSALT_FLAG) {
            patch_salt_biz_pid(&mut src).map_err(|e| io::Error::new(io::ErrorKind::NotFound, e))?;
            dirty = true;
        }
        if !src.contains(TSW_PSTORE_HEAD_FLAG) {
            patch_capture_pstore_head(&mut src).map_err(|e| io::Error::new(io::ErrorKind::NotFound, e))?;
            dirty = true;
        }
        if !src.contains(TSW_INITRETRY2_FLAG) && try_patch_init_retry(&mut src) {
            dirty = true;
        }
        if !src.contains(TSW_V2RETRY_FLAG) && try_patch_v2_retry_503(&mut src) {
            dirty = true;
        }
        if !src.contains(TSW_V2NOTFOUND_FLAG) && try_patch_v2_session_not_found(&mut src) {
            dirty = true;
        }
        if dirty {
            std::fs::write(chat, src)?;
            return Ok(InjectStatus::Patched);
        }
        return Ok(InjectStatus::AlreadyPatched);
    }
    ensure_bak_named(chat)?;
    if pid_done {
        // ⑧ 已废弃：把旧的钉值表达式降级成 passthrough
        disable_pid_pin(&mut src);
    } else {
        patch_pid_chat(&mut src).map_err(|e| io::Error::new(io::ErrorKind::NotFound, e))?;
    }
    // 换号要能清掉聊天侧缓存的 project_id，先把 project store 抓到 globalThis
    patch_capture_pstore(&mut src).map_err(|e| io::Error::new(io::ErrorKind::NotFound, e))?;
    patch_capture_pstore_head(&mut src).map_err(|e| io::Error::new(io::ErrorKind::NotFound, e))?;
    // ⑬ biz_project_id 按用户加盐（会话按账号隔离）
    patch_salt_biz_pid(&mut src).map_err(|e| io::Error::new(io::ErrorKind::NotFound, e))?;
    try_patch_init_retry(&mut src);
    try_patch_v2_retry_503(&mut src);
    try_patch_v2_session_not_found(&mut src);
    std::fs::write(chat, src)?;
    Ok(InjectStatus::Patched)
}

fn is_ident_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'$'
}

/// 从位置 pos 向前取标识符，返回 (start, ident)
fn ident_before(src: &[u8], pos: usize) -> Option<(usize, String)> {
    let mut start = pos;
    while start > 0 && is_ident_byte(src[start - 1]) {
        start -= 1;
    }
    if start == pos {
        return None;
    }
    Some((start, src[start..pos].iter().map(|&b| b as char).collect()))
}

/// 从 main.js 源码解析 v2 handler 的 {PROP}（userInfo 所在属性，intl=j / CN=userContext）
fn v2_prop_of(src: &str) -> Option<String> {
    let head = "async handleForceRefreshUserInfo(t){";
    let after = src.find(head)? + head.len();
    src[after..]
        .strip_prefix("if(!this.")
        .and_then(|s| {
            s.find(".userInfo")
                .filter(|&i| i > 0 && s[..i].bytes().all(is_ident_byte))
                .map(|i| s[..i].to_string())
        })
}

/// 磁盘 main.js 的 {PROP} 解析（CDP 兜底通道构造 body 用）
pub fn tsw_prop(paths: &IdePaths) -> Option<String> {
    v2_prop_of(&std::fs::read_to_string(paths.main_js()).ok()?)
}

/// ① v2 handler 头部植入 marker 检测块
fn patch_v2_handler(src: &mut String) -> Result<bool, String> {
    if src.contains(TSW_V2_FLAG) {
        return Ok(false);
    }
    let prop = v2_prop_of(src).ok_or("特征锚未命中：handleForceRefreshUserInfo v2（锚或方法体形态不匹配）")?;
    let head = "async handleForceRefreshUserInfo(t){";
    let after = src.find(head).unwrap() + head.len();
    let body = TSW_V2_BODY.replace("{PROP}", &prop);
    let insert_at = after + TSW_V2_FLAG.len();
    src.insert_str(after, TSW_V2_FLAG);
    src.insert_str(insert_at, &body);
    Ok(true)
}

/// ② IPC dispatcher 补 return：`<async> NAME(e,i){this.H.handleForceRefreshUserInfo(i)}`
fn patch_dispatcher_return(src: &mut String) -> bool {
    let suffix = ".handleForceRefreshUserInfo(i)}";
    if src.contains(TSW_RET_FLAG) {
        return false;
    }
    let Some(rel) = src.find(suffix) else { return false };
    let b = src.as_bytes();
    let mut p = rel; // 指向 '.' 处
    // helper 标识符
    let (hs, helper) = match ident_before(b, p) {
        Some(v) => v,
        None => return false,
    };
    p = hs;
    // this.
    if !src[..p].ends_with("this.") {
        return false;
    }
    p -= 5;
    // {
    if p == 0 || b[p - 1] != b'{' {
        return false;
    }
    p -= 1;
    // (e,i)
    if !src[..p].ends_with("(e,i)") {
        return false;
    }
    p -= 5;
    // 方法名
    let (ns, name) = match ident_before(b, p) {
        Some(v) => v,
        None => return false,
    };
    p = ns;
    // 可选 async
    let async_kw = if src[..p].ends_with("async ") {
        p -= 6;
        "async "
    } else {
        ""
    };
    let replacement = format!(
        "{async_kw}{name}(e,i){{return this.{helper}.handleForceRefreshUserInfo(i){TSW_RET_FLAG}}}"
    );
    src.replace_range(p..rel + suffix.len(), &replacement);
    true
}

/// ③ ICDRS.resolveDeviceId 暴露 globalThis.__sw_icdrs
fn patch_icdrs(src: &mut String) -> bool {
    if src.contains(TSW_ICDRS_FLAG) {
        return false;
    }
    let head = "async resolveDeviceId(){return this.";
    let Some(pos) = src.find(head) else { return false };
    let after = pos + head.len();
    let rest = &src[after..];
    let Some(bi) = rest.find(".isOpen()||await this.") else { return false };
    let barrier = &rest[..bi];
    if barrier.is_empty() || !barrier.bytes().all(is_ident_byte) {
        return false;
    }
    let rest2 = &rest[bi + ".isOpen()||await this.".len()..];
    let Some(rest2) = rest2.strip_prefix(barrier) else { return false };
    let Some(rest3) = rest2.strip_prefix(".wait(),this.") else { return false };
    let Some(di) = rest3.find('}') else { return false };
    let did = &rest3[..di];
    if did.is_empty() || !did.bytes().all(is_ident_byte) {
        return false;
    }
    let original = format!("{head}{barrier}.isOpen()||await this.{barrier}.wait(),this.{did}}}");
    let replacement = format!(
        "async resolveDeviceId(){{globalThis.__sw_icdrs=this;{TSW_ICDRS_FLAG}if(this.__sw_orig_did)return this.__sw_orig_did;return this.{barrier}.isOpen()||await this.{barrier}.wait(),this.{did}}}"
    );
    src.replace_range(pos..pos + original.len(), &replacement);
    true
}

/// main.js 三补丁合一（幂等；Trae 须未运行）
pub fn patch_main(paths: &IdePaths) -> io::Result<InjectStatus> {
    patch_main_file(&paths.main_js())
}

/// 移除注入体里对 setI18nApiHostByUserInfo 的深搜调用块：CN 3.4.4 无此方法，
/// 深搜会命中 main 里的 toService IPC 代理，跨进程抛 CodeExpectedError 成为 main
/// 未捕获异常并打断 ai-agent（换号后「服务启动异常」/ 一直加载的根因）。
/// 对磁盘上已写入的旧注入体同样生效（幂等）。
fn strip_bad_i18n_call(src: &mut String) -> bool {
    let start_marker = "try{const _found=[];const _seen=new Set();";
    let end_marker = "try{const _hInfo=";
    let Some(start) = src.find(start_marker) else {
        return false;
    };
    let Some(rel) = src[start..].find(end_marker) else {
        return false;
    };
    src.replace_range(start..start + rel, "");
    true
}

pub fn patch_main_file(main: &std::path::Path) -> io::Result<InjectStatus> {
    let mut src = std::fs::read_to_string(main)?;
    let repaired = repair_v2_require(&mut src);
    let stripped = strip_bad_i18n_call(&mut src);
    if src.contains(TSW_V2_FLAG) && src.contains(TSW_ICDRS_FLAG) {
        if repaired || stripped {
            std::fs::write(main, &src)?;
            return Ok(InjectStatus::Patched);
        }
        return Ok(InjectStatus::AlreadyPatched);
    }
    ensure_bak(main)?;
    let mut errs = Vec::new();
    if let Err(e) = patch_v2_handler(&mut src) {
        errs.push(e);
    }
    // dispatcher return：无锚不报错（可能已被其他工具修复）
    let _ = patch_dispatcher_return(&mut src);
    if !patch_icdrs(&mut src) && !src.contains(TSW_ICDRS_FLAG) {
        errs.push("特征锚未命中：resolveDeviceId ICDRS".into());
    }
    if !errs.is_empty() {
        return Err(io::Error::new(io::ErrorKind::NotFound, errs.join("; ")));
    }
    std::fs::write(main, src)?;
    Ok(InjectStatus::Patched)
}

/// ④⑤⑥ workbench 三补丁（命令注册 + return 修复 + 完整性短路）
pub fn patch_workbench(paths: &IdePaths) -> io::Result<InjectStatus> {
    patch_workbench_file(&paths.workbench_js())
}

pub fn patch_workbench_file(wb: &std::path::Path) -> io::Result<InjectStatus> {
    let mut src = std::fs::read_to_string(wb)?;
    let already_cmd = src.contains(TSW_CMD_FLAG);
    let already_ret = src.contains(TSW_WRET_FLAG);
    let already_pid = src.contains(TSW_PID_FLAG);
    let already_aic = src.contains(TSW_AIC_FLAG);
    let already_rpc = src.contains(TSW_RPC_FLAG);
    let already_rld = src.contains(TSW_RLD_FLAG);
    let already_prep = src.contains(TSW_PREP_FLAG);
    if already_cmd
        && already_ret
        && already_pid
        && already_aic
        && already_rpc
        && already_rld
        && already_prep
    {
        // ⑧ 已废弃：老机器上把钉值表达式降级成 passthrough
        if disable_pid_pin(&mut src) {
            std::fs::write(wb, src)?;
            return Ok(InjectStatus::Patched);
        }
        return Ok(InjectStatus::AlreadyPatched);
    }
    ensure_bak(wb)?;
    let mut errs = Vec::new();
    if !already_cmd {
        if let Err(e) = patch_register_command(&mut src) {
            errs.push(e);
        }
    }
    if !already_ret {
        if let Err(e) = patch_refresh_return(&mut src) {
            errs.push(e);
        }
    }
    if !already_pid {
        if let Err(e) = patch_pid_workbench(&mut src) {
            errs.push(e);
        }
    }
    // 会话用量链路：客户端暴露 + RPC 代理命令（结构面变动只降级，不影响热换号）
    // 无条件调用：已打过 ⑨ 的旧注入体需要补宿主 capture（换号后重建客户端）
    if let Err(e) = patch_ai_client(&mut src) {
        errs.push(format!("agent 客户端暴露失败：{e}"));
    }
    if !already_rpc {
        if let Err(e) = patch_register_rpc(&mut src) {
            errs.push(format!("chatRpc 命令注册失败：{e}"));
        }
    }
    if !already_rld {
        if let Err(e) = patch_register_reload(&mut src) {
            errs.push(format!("reloadAgent 命令注册失败：{e}"));
        }
    }
    if !already_prep {
        if let Err(e) = patch_register_prepare(&mut src) {
            errs.push(format!("prepareSwitch 命令注册失败：{e}"));
        }
    }
    if !src.contains(TSW_PURE_FLAG) {
        patch_integrity(&mut src);
    }
    if !errs.is_empty() {
        return Err(io::Error::new(io::ErrorKind::NotFound, errs.join("; ")));
    }
    std::fs::write(wb, src)?;
    Ok(InjectStatus::Patched)
}

/// ④ 命令注册锚：`<CALLER>.registerCommand("icube.common.refreshUserInfo",async function(i,e){await i.get(<TOKEN>).refreshUserInfo(!0)})`
/// 返回 (caller, service token, 锚尾偏移)——caller/token 均为本模块作用域内可用标识符
fn find_register_anchor(src: &str) -> Result<(String, String, usize), String> {
    let needle = "registerCommand(\"icube.common.refreshUserInfo\",async function(i,e){await i.get(";
    let npos = src
        .find(needle)
        .ok_or("特征锚未命中：icube.common.refreshUserInfo 注册")?;
    // 锚前一字必为 '.'（Te.registerCommand 形态），跳过点号回溯 caller 标识符
    let (_, caller) = ident_before(src.as_bytes(), npos - 1).ok_or("命令注册 caller 解析失败")?;
    let after = npos + needle.len();
    let rest = &src[after..];
    let Some(ti) = rest.find(')') else {
        return Err("service token 未闭合".into());
    };
    let token = &rest[..ti];
    if token.is_empty() || !token.bytes().all(is_ident_byte) {
        return Err("service token 形态异常".into());
    }
    let anchor_tail = ").refreshUserInfo(!0)})";
    if !rest[ti..].starts_with(anchor_tail) {
        return Err("refreshUserInfo 注册形态异常".into());
    }
    Ok((caller, token.to_string(), after + ti + anchor_tail.len()))
}

/// ④ 命令注册：锚 X.registerCommand("icube.common.refreshUserInfo",async function(i,e){await i.get(Y).refreshUserInfo(!0)})
fn patch_register_command(src: &mut String) -> Result<(), String> {
    let (caller, token, anchor_end) = find_register_anchor(src)?;
    let insertion = format!(
        ",{caller}.registerCommand(\"_tsw.replaceUserInfo\",async function(i,payload){{const p=payload&&typeof payload===\"object\"&&payload.userInfo?payload:{{userInfo:payload}};return await i.get({token}).refreshUserInfo({{__TSW_REPLACE__:true,device:p.device,userInfo:p.userInfo}}){TSW_CMD_FLAG}}})"
    );
    src.insert_str(anchor_end, &insertion);
    Ok(())
}

/// ⑨ workbench：把进程内 ai-agent RPC 客户端暴露到 globalThis.__tsw_ai
/// 锚 = initDefaultClient 内的 `this.<field>=await this.createClient(<arg>)`，
/// 两种发行版都命中：国际版压缩态 `this.a=await this.createClient(t)`、CN 可读态 `this._client=await this.createClient(t)`
fn patch_ai_client(src: &mut String) -> Result<(), String> {
    if src.contains(TSW_AIC_FLAG) {
        // 已打过 ⑨ 的旧注入体：在 initDefaultClient 方法头补一行宿主 capture
        // （换号后重建客户端单例要用它；在方法体内按方法名定位，不依赖插入点旁的内容）
        if !src.contains("globalThis.__tsw_aiHost") {
            let head = "initDefaultClient(){";
            let start = src.find(head).ok_or("特征锚未命中：initDefaultClient")?;
            let at = start + head.len();
            src.insert_str(at, "globalThis.__tsw_aiHost=this;");
        }
        return Ok(());
    }
    let head = "initDefaultClient(){";
    let start = src.find(head).ok_or("特征锚未命中：initDefaultClient")?;
    let win_end = (start + 700).min(src.len());
    let call = "=await this.createClient(";
    let rel = src[start..win_end]
        .find(call)
        .ok_or("特征锚未命中：initDefaultClient 内的 createClient 调用")?;
    let at = start + rel;
    // 客户端字段（this.<x>）：回溯标识符，须以 "this." 收尾
    let (field_end, _) = ident_before(src.as_bytes(), at).ok_or("客户端字段解析失败")?;
    if !src[..field_end].ends_with("this.") {
        return Err("客户端字段形态异常（非 this.<field>）".into());
    }
    let field_start = field_end - 5;
    let arg_start = at + call.len();
    let close = src[arg_start..].find(')').ok_or("createClient 调用未闭合")?;
    let after_call = arg_start + close + 1;
    let field = src[field_start..at].to_string();
    src.insert_str(
        after_call,
        &format!(",globalThis.__tsw_ai={field},globalThis.__tsw_aiHost=this{TSW_AIC_FLAG}"),
    );
    Ok(())
}

/// ⑩ workbench：注册 _tsw.chatRpc —— 经 __tsw_ai 直调 agent 本地 RPC 的只读代理。
/// service/method 由调用方（Rust 侧固定常量）给出，不做任意转发；
/// 未就绪时最多等 6s（渲染进程启动早期服务即实例化；再长会撞 shim 侧 8s invoke 护栏）。
const RPC_REG_TMPL: &str = r#".registerCommand("_tsw.chatRpc",async function(__acc,__p){/*__TSW_RPC__*/
try{
const p=__p||{};
let c=globalThis.__tsw_ai;
if(!c){for(let __i=0;__i<24&&!c;__i++){await new Promise(__r=>setTimeout(__r,250));c=globalThis.__tsw_ai;}}
if(!c)return{ok:false,error:"agent RPC 客户端未就绪（请先在 Trae 内打开一次 AI 侧栏或发起一次对话）"};
const ctx=p.context&&typeof p.context==="object"?p.context:{};
if(!ctx.project_id){try{const __v=localStorage.getItem("__tsw_project_id");if(__v)ctx.project_id=__v;}catch(__e){}}
const resp=await c.request({service:p.service,method:p.method,data:p.data||{},context:ctx});
let json;try{json=JSON.stringify(resp);}catch(__e){json=String(resp);}
return{ok:true,json:json};
}catch(__e){return{ok:false,error:String(__e&&__e.message||__e)}}})"#;

fn patch_register_rpc(src: &mut String) -> Result<(), String> {
    if src.contains(TSW_RPC_FLAG) {
        return Ok(());
    }
    let (caller, _token, anchor_end) = find_register_anchor(src)?;
    src.insert_str(anchor_end, &format!(",{caller}{RPC_REG_TMPL}"));
    Ok(())
}

/// 从 workbench 里取出 IProcessManagerAgentService 的 DI 令牌变量名。
///
/// 为什么不用 `workbench.action.reloadAiAgent`：那个 Action2 在 `registerProcessActions()`
/// 里被 `this.productService.quality==="stable"||(O(VAr),O(WAr))` 短路掉了 —— stable 版
/// （CN 3.4.4 实测）**根本没注册**，扩展宿主执行时报 command not found。
/// 但那份实现本身还在包里：`async run(e){const t=e.get(lt),s=e.get(eoe);try{await s.restartAiAgent()`，
/// 把 `eoe` 抠出来就能直接拿服务、走官方重启路径（channel.call(restartAiAgent) + reconnect）。
fn pm_token_of(src: &str) -> Option<String> {
    let at = src.find(";try{await s.restartAiAgent()")?;
    let k = src[..at].rfind("s=e.get(")?;
    let rest = &src[k + "s=e.get(".len()..at];
    let end = rest.find(')')?;
    let tok = &rest[..end];
    let ok = !tok.is_empty()
        && tok
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '$');
    ok.then(|| tok.to_string())
}

/// ⑪ workbench：注册 _tsw.reloadAgent —— 换号后重建 AI 会话栈。
///
/// 两步，顺序固定：
///   ① 重启 agent 进程（`IProcessManagerAgentService.restartAiAgent()`，= 面板那颗「重启服务」
///      按钮走的同一条实现），让 agent 按当前 storage.json 里的身份重新握手；
///   ② 拆掉渲染进程的客户端单例（一次性 deferred 已 settle，不换就永远返回旧句柄）再重新 init。
///
/// 只做 ② 不做 ①：进程还握着旧账号的会话上下文；只做 ① 不做 ②：`getDefaultClient()` 仍是旧
/// 客户端，会话列表/对话/任务拿到的还是旧句柄 —— 旧对话点开照样回落「服务启动异常」。
///
/// 字段名不写死：deferred 按形状定位（`p` 是 Promise 且带 `complete`），变更判定走宿主自己的
/// `getDefaultClient()`；国际版压缩态客户端字段是 `this.a`，与 CN 的 `_client` 不同名。
/// 旧客户端不 dispose（被多个服务共享，dispose 会连带打挂面板）。
const RELOAD_TMPL: &str = r#".registerCommand("_tsw.reloadAgent",async function(__acc,__p){/*__TSW_RLD3__*/
try{
let __proc={ok:false,error:"未定位 agent 进程服务令牌"};
try{
const __pm=__acc&&typeof __acc.get==="function"?__acc.get(__PM__):null;
if(!__pm||typeof __pm.restartAiAgent!=="function"){__proc={ok:false,error:"进程服务无 restartAiAgent（结构变动）"};}
else{
const __now=Date.now();
const __last=globalThis.__tsw_lastAgentRestart||0;
if(__now-__last<30000){
__proc={ok:true,skipped:"30s 内刚重启过，跳过进程重启",sinceMs:__now-__last};
}else{
globalThis.__tsw_lastAgentRestart=__now;
await __pm.restartAiAgent();
__proc={ok:true};
await new Promise(__r=>setTimeout(__r,3000));
let __hErr="";
for(let __i=0;__i<10;__i++){
__hErr="";
try{if(typeof __pm.getAiAgentProcessStartupError==="function")__hErr=(await __pm.getAiAgentProcessStartupError())||"";}catch(__e){__hErr="";}
if(!__hErr)break;
await new Promise(__r=>setTimeout(__r,500));
}
__proc.health=__hErr||"ok";
}
}
}catch(__e){__proc={ok:false,error:String(__e&&__e.message||__e)};}
const h=globalThis.__tsw_aiHost;
if(!h||typeof h.initDefaultClient!=="function")return{ok:false,process:__proc,client:{ok:false,error:"agent 宿主未就绪（未捕获宿主对象，需重装补丁并重启 Trae）"}};
let __k=null,__d=null;
for(const __key of Object.keys(h)){const __v=h[__key];if(__v&&typeof __v==="object"&&__v.p&&typeof __v.p.then==="function"&&typeof __v.complete==="function"){__k=__key;__d=__v;break;}}
if(!__k)return{ok:false,process:__proc,client:{ok:false,error:"agent 客户端 deferred 未定位（宿主结构变动，需重装补丁）"}};
const __gc=typeof h.getDefaultClient==="function"?()=>Promise.race([Promise.resolve(h.getDefaultClient()).catch(()=>null),new Promise(__r=>setTimeout(()=>__r(null),4000))]):null;
const __before=__gc?await __gc():null;
h[__k]=new __d.constructor();
try{
await h.initDefaultClient();
}catch(__e){
h[__k]=__d;
return{ok:false,process:__proc,client:{ok:false,error:"重新初始化失败：" + String(__e&&__e.message||__e)}};
}
const __after=__gc?await __gc():null;
return{ok:!!__proc.ok&&(__proc.health===undefined||__proc.health==="ok"),process:__proc,client:{ok:true,rebuilt:true,deferred:__k,changed:__gc?(__before&&__after?__before!==__after:null):null,hasClient:!!__after}};
}catch(__e){return{ok:false,error:String(__e&&__e.message||__e)}}})"#;

/// ⑫ workbench：注册 _tsw.prepareSwitch —— 换号前把聊天侧缓存的 project 清掉。
///
/// agent 按 (user_id, project_id) 校验项目归属。换号后 store 里仍是上一个账号的 project_id 时，
/// `createProject(false)` 会直接返回这个 id，随后 `loadPreviousSessionOrCreateNewIfNeeded()`
/// 拿它查会话 → `project not found` → 面板「服务启动异常」。清空后 `initChatView` 会重新
/// `get_or_create_project_v2`，拿到属于新账号的 project。
/// 顺手清 localStorage 里的钉值，避免下一轮又把旧 id 钉回去。
const PREP_TMPL: &str = r#".registerCommand("_tsw.prepareSwitch",async function(__acc,__p){/*__TSW_PREP__*/
try{
let __cleared=[];
try{
const __st=globalThis.__tsw_projectStore;
if(__st&&__st.actions&&typeof __st.actions.setProjectId==="function"){
const __cur=__st.getState?__st.getState().projectId:"";
__st.actions.setProjectId("","");
__cleared.push("store:"+(__cur||"(empty)"));
}
}catch(__e){}
try{
const __old=localStorage.getItem("__tsw_project_id");
localStorage.removeItem("__tsw_project_id");
localStorage.removeItem("__tsw_real_project_id");
if(__old)__cleared.push("pin:"+__old);
}catch(__e){}
return{ok:true,cleared:__cleared,hasStore:!!globalThis.__tsw_projectStore};
}catch(__e){return{ok:false,error:String(__e&&__e.message||__e)}}})"#;

fn patch_register_prepare(src: &mut String) -> Result<(), String> {
    if src.contains(TSW_PREP_FLAG) {
        return Ok(());
    }
    let (caller, _token, anchor_end) = find_register_anchor(src)?;
    src.insert_str(anchor_end, &format!(",{caller}{PREP_TMPL}"));
    Ok(())
}

fn patch_register_reload(src: &mut String) -> Result<(), String> {
    if src.contains(TSW_RLD_FLAG) {
        return Ok(());
    }
    // 迁移：旧代 ⑪ 的那一段要整块剥掉，否则会留下两个同 id 的 registerCommand
    if src.contains("registerCommand(\"_tsw.reloadAgent\"") {
        if let Some((start, end)) = find_reload_block(src) {
            src.replace_range(start..end, "");
        }
    }
    // 令牌缺失只降级（进程重启那步会返回明确错误），不影响换号本身
    let token = pm_token_of(src).unwrap_or_else(|| "null".to_string());
    let (caller, _token, anchor_end) = find_register_anchor(src)?;
    src.insert_str(
        anchor_end,
        &format!(",{caller}{}", RELOAD_TMPL.replace("__PM__", &token)),
    );
    Ok(())
}

/// 跳过 JS 字符串字面量（含转义），返回收尾引号之后的位置
fn skip_js_string(src: &str, quote_at: usize) -> usize {
    let b = src.as_bytes();
    let q = b[quote_at];
    let mut i = quote_at + 1;
    while i < b.len() {
        if b[i] == b'\\' {
            i += 2;
            continue;
        }
        if b[i] == q {
            return i + 1;
        }
        i += 1;
    }
    b.len()
}

/// 定位 `_tsw.reloadAgent` 命令块：返回 (前面的逗号, 收尾 `)` 之后)
fn find_reload_block(src: &str) -> Option<(usize, usize)> {
    let at = src.find("registerCommand(\"_tsw.reloadAgent\"")?;
    let fn_at = at + src[at..].find("async function(__acc,__p)")?;
    let brace = fn_at + src[fn_at..].find('{')?;
    let b = src.as_bytes();
    let mut depth = 0usize;
    let mut i = brace;
    while i < b.len() {
        match b[i] {
            b'"' | b'\'' | b'`' => {
                i = skip_js_string(src, i);
                continue;
            }
            b'/' if i + 1 < b.len() && b[i + 1] == b'/' => {
                while i < b.len() && b[i] != b'\n' {
                    i += 1;
                }
                continue;
            }
            b'/' if i + 1 < b.len() && b[i + 1] == b'*' => {
                i = i + src[i..].find("*/")? + 2;
                continue;
            }
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    i += 1;
                    break;
                }
            }
            _ => {}
        }
        i += 1;
    }
    // 函数体收尾后应紧跟 registerCommand 的 `)`
    while i < b.len() && b[i] != b')' && b[i] != b',' && b[i] != b';' {
        i += 1;
    }
    if i < b.len() && b[i] == b')' {
        i += 1;
    }
    let start = src[..at].rfind(',')?;
    Some((start, i))
}

/// ⑤ return 修复：async refreshUserInfo(e){await IPC.invoke(ENUM.SANDBOX_TO_MAIN_REFRESH_USERINFO,e)}
fn patch_refresh_return(src: &mut String) -> Result<(), String> {
    let needle = "async refreshUserInfo(e){await ";
    let Some(pos) = src.find(needle) else {
        // return 形态已存在（其他工具已修）→ 跳过
        return Ok(());
    };
    let after = pos + needle.len();
    let rest = &src[after..];
    let Some(ii) = rest.find(".invoke(") else {
        return Err("refreshUserInfo IPC 变量解析失败".into());
    };
    let ipc = &rest[..ii];
    if ipc.is_empty() || !ipc.bytes().all(is_ident_byte) {
        return Err("refreshUserInfo IPC 变量形态异常".into());
    }
    let seg = ii + ".invoke(".len();
    let rest2 = &rest[seg..];
    let enum_tail = ".SANDBOX_TO_MAIN_REFRESH_USERINFO,e)}";
    let Some(ei) = rest2.find(enum_tail) else {
        return Err("refreshUserInfo IPC 枚举形态异常".into());
    };
    let en = &rest2[..ei];
    if en.is_empty() || !en.bytes().all(is_ident_byte) {
        return Err("refreshUserInfo IPC 枚举形态异常".into());
    }
    let full_end = after + seg + ei + enum_tail.len();
    let replacement = format!(
        "async refreshUserInfo(e){{return await {ipc}.invoke({en}.SANDBOX_TO_MAIN_REFRESH_USERINFO,e){TSW_WRET_FLAG}}}"
    );
    src.replace_range(pos..full_end, &replacement);
    Ok(())
}

/// ⑥ 完整性校验短路：`async N(){const{isPure:X}=await this.Y.isPure();if(X)return;`
/// 两种形态：this.<prop>.isPure() / this.isPure()
fn patch_integrity(src: &mut String) {
    let needle = "const{isPure:";
    let Some(npos) = src.find(needle) else { return };
    let after = npos + needle.len();
    let Some(ci) = src[after..].find("}=await this.") else { return };
    let pure_var = src[after..after + ci].to_string();
    if pure_var.is_empty() || !pure_var.bytes().all(is_ident_byte) {
        return;
    }
    let seg_start = after + ci + "}=await this.".len();
    let seg = src[seg_start..].to_string();
    let prop_form = format!("{pure_var}.isPure();if({pure_var})return;");
    let bare_form = format!("isPure();if({pure_var})return;");
    let (matched_len, uses_prop) = if seg.starts_with(&prop_form) {
        (prop_form.len(), true)
    } else if seg.starts_with(&bare_form) {
        (bare_form.len(), false)
    } else {
        return;
    };
    let end = seg_start + matched_len;
    // 回溯方法头：async NAME(){
    let b = src.as_bytes();
    let mut p = npos;
    if p < 3 || b[p - 1] != b'{' || b[p - 2] != b')' || b[p - 3] != b'(' {
        return;
    }
    p -= 3;
    let Some((ns, name)) = ident_before(b, p) else { return };
    p = ns;
    if !src[..p].ends_with("async ") {
        return;
    }
    p -= 6;
    let svc_ref = if uses_prop {
        format!("this.{pure_var}.isPure();")
    } else {
        "this.isPure();".to_string()
    };
    let replacement = format!(
        "async {name}(){{return;{TSW_PURE_FLAG}const{{isPure:{pure_var}}}=await {svc_ref}if({pure_var})return;"
    );
    src.replace_range(p..end, &replacement);
}

fn ensure_bak(file: &std::path::Path) -> io::Result<()> {
    let bak = file.with_extension("js.switcher.bak");
    if !bak.exists() {
        std::fs::copy(file, &bak)?;
    }
    Ok(())
}

/// 补丁状态按 (install_dir, main mtime, workbench mtime) 缓存。
/// 依据：main.js 2.6MB + workbench 15MB 全量读+字符串搜索，5 秒轮询要 70MB IO，
/// 直接拖死 UI；补丁只在注入时变化，mtime 未变即结果未变。
/// (install_dir, main_mtime_ms, wb_mtime_ms, chat_mtime_ms, tsw_patched, chat_pinned, patch_at_ms)
static PATCH_CACHE: std::sync::Mutex<Vec<(String, u64, u64, u64, bool, bool, u64)>> =
    std::sync::Mutex::new(Vec::new());

fn file_mtime_ms(p: &std::path::Path) -> Option<u64> {
    std::fs::metadata(p)
        .ok()?
        .modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .map(|d| d.as_millis() as u64)
}

/// (TSW 六补丁是否齐全, 会话归属补丁是否落盘, 补丁落盘时刻)
fn patch_state(paths: &IdePaths) -> (bool, bool, u64) {
    let key = paths.install_dir.to_string_lossy().to_string();
    let main_m = file_mtime_ms(&paths.main_js()).unwrap_or(0);
    let wb_m = file_mtime_ms(&paths.workbench_js()).unwrap_or(0);
    let chat_m = file_mtime_ms(&paths.chat_module_js()).unwrap_or(0);
    {
        let cache = PATCH_CACHE.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((_, cm, cw, cc, patched, pinned, at)) = cache.iter().find(|e| e.0 == key) {
            if *cm == main_m && *cw == wb_m && *cc == chat_m {
                return (*patched, *pinned, *at);
            }
        }
    }
    let patched = scan_tsw_patched(paths);
    let pinned = scan_chat_pinned(paths);
    let at = main_m.max(wb_m).max(chat_m);
    {
        let mut cache = PATCH_CACHE.lock().unwrap_or_else(|e| e.into_inner());
        cache.retain(|e| e.0 != key);
        cache.push((key, main_m, wb_m, chat_m, patched, pinned, at));
    }
    (patched, pinned, at)
}

/// 热换号就绪判定：main.js 双补丁 + workbench 命令注册
/// （会话归属补丁独立于热换号链路，缺失不影响免重启换号）
fn scan_tsw_patched(paths: &IdePaths) -> bool {
    let main_ok = std::fs::read_to_string(paths.main_js())
        .map(|s| s.contains(TSW_V2_FLAG) && s.contains(TSW_ICDRS_FLAG))
        .unwrap_or(false);
    main_ok
        && std::fs::read_to_string(paths.workbench_js())
            .map(|s| s.contains(TSW_CMD_FLAG))
            .unwrap_or(false)
}

/// 会话/任务记录归属补丁是否落盘（workbench + ai-modules-chat 双处）
fn scan_chat_pinned(paths: &IdePaths) -> bool {
    let wb = std::fs::read_to_string(paths.workbench_js())
        .map(|s| s.contains(TSW_PID_FLAG))
        .unwrap_or(false);
    let chat = std::fs::read_to_string(paths.chat_module_js())
        .map(|s| s.contains(TSW_PID_FLAG))
        .unwrap_or(false);
    wb && chat
}

/// 会话归属补丁状态（GUI 展示用）
pub fn is_chat_pinned(paths: &IdePaths) -> bool {
    patch_state(paths).1
}

/// 补丁落盘时刻（main.js / workbench 中较晚者）：与运行实例启动时刻比对，
/// 判断运行中的 IDE 是否已加载补丁（先启动后打补丁 = 内存仍是旧 bundle）
pub fn patch_mtime_ms(paths: &IdePaths) -> Option<u64> {
    Some(patch_state(paths).2)
}

/// shim 通道就绪判定：main.js + workbench 双补丁齐全
pub fn is_tsw_patched(paths: &IdePaths) -> bool {
    patch_state(paths).0
}

/// 一次打齐 CDP 桥 + main 三补丁 + workbench 四补丁 + 聊天模块会话归属补丁
pub fn patch_all(paths: &IdePaths) -> io::Result<String> {
    patch(paths)?;
    patch_main(paths)?;
    patch_workbench(paths)?;
    // 会话归属补丁独立于热换号链路：失败只降级（新版本结构变动时不影响免重启换号）
    let chat_note = match patch_chat(paths) {
        Ok(_) => "会话记录归属",
        Err(e) => {
            crate::core::logger::log(
                crate::core::logger::Level::Warn,
                crate::core::logger::Source::System,
                format!("会话记录归属未生效（对话记录跨号保留不可用）: {e}"),
            );
            "会话记录归属失败（见日志）"
        }
    };
    Ok(format!("无感换号支持安装完成：__sw 桥 + 无感换号 handler + workbench 命令 + {chat_note}"))
}

/// 全量卸载：优先还原原始备份（干净彻底），无备份则逐段剥离
pub fn unpatch_all(paths: &IdePaths) -> io::Result<String> {
    let mut msgs = Vec::new();
    let chat = paths.chat_module_js();
    let chat_bak = chat.with_file_name(format!(
        "{}.switcher.bak",
        chat.file_name().unwrap_or_default().to_string_lossy()
    ));
    if chat_bak.exists() {
        std::fs::copy(&chat_bak, &chat)?;
        msgs.push("已还原 ai-modules-chat".into());
    }
    for f in [paths.main_js(), paths.workbench_js()] {
        let bak = f.with_extension("js.switcher.bak");
        if bak.exists() {
            std::fs::copy(&bak, &f)?;
            msgs.push(format!("已还原 {}", f.file_name().unwrap_or_default().to_string_lossy()));
        } else if f.exists() {
            if f == paths.main_js() {
                unpatch(paths)?;
                msgs.push("已移除 main.js 无感换号支持".into());
            }
        }
    }
    if msgs.is_empty() {
        msgs.push("无可移除的无感换号支持".into());
    }
    Ok(msgs.join("，"))
}

#[cfg(test)]
mod tests {
    use super::*;

    const INIT_CHAT_VIEW: &str = r#"e6=(0,rb.A)(async(e=!1)=>{eW(!0),eG("");let t=Date.now(),r="";try{let n=await ei.getUserInfo(),i=null!=n,o=n?.userId||"";if(Z.info("[ai-chat] initChatView loggedIn:",i,"userId:",o),!i||!o){eW(!1);return}await D.loadPreviousSessionOrCreateNewIfNeeded(),await j.initialize(),Z.info("[ai-chat] initChatView load session done"),$.event({})}catch(i){let e=i?.stack||i?.message||"";if(!W.getState().loggedIn){eW(!1);return}Z.error("[ai-chat] initChatView error:",e);let{message:n}=J.getErrorInfo(1);eG(n)}eW(!1)});"#;

    #[test]
    fn init_retry_patch_is_idempotent_and_targets_init_fn() {
        let mut src = INIT_CHAT_VIEW.to_string();
        patch_init_retry(&mut src).unwrap();
        assert!(src.contains("setTimeout(()=>{e6(!0)},1500);return}"));
        assert!(src.contains("/boot_config_not_ready|project not found/.test(String(e))"));
        assert!(src.contains("Z.warn(\"[ai-chat] initChatView recoverable error, tsw retry\""));
        assert!(src.contains("await j.initialize(),globalThis.__tsw_initRetry=0,Z.info("));
        assert!(src.contains(&format!("{TSW_INITRETRY2_FLAG}Z.error(\"[ai-chat] initChatView error:\"")));
        let once = src.clone();
        patch_init_retry(&mut src).unwrap();
        assert_eq!(src, once);
    }

    #[test]
    fn init_retry_patch_rejects_unknown_shape() {
        let mut src = INIT_CHAT_VIEW.replace("initChatView error:", "initChatView failed:");
        assert!(patch_init_retry(&mut src).is_err());
        assert!(!src.contains(TSW_INITRETRY_FLAG));
        assert!(!src.contains(TSW_INITRETRY2_FLAG));
    }

    #[test]
    fn init_retry_migrates_v1_to_v2() {
        let v1 = r#"if(/boot_config_not_ready/.test(String(e))){let __n=(globalThis.__tsw_initRetry||0)+1;if(__n<=8){globalThis.__tsw_initRetry=__n;Z.warn("[ai-chat] initChatView boot_config_not_ready, tsw retry",__n);setTimeout(()=>{e6()},1500);return}globalThis.__tsw_initRetry=0}/*__TSW_INITRETRY__*/"#;
        let mut src = INIT_CHAT_VIEW.to_string();
        let err_at = src.find("Z.error(\"[ai-chat] initChatView error:\"").unwrap();
        src.insert_str(err_at, v1);
        let done_at = src
            .find("Z.info(\"[ai-chat] initChatView load session done\")")
            .unwrap();
        src.insert_str(done_at, "globalThis.__tsw_initRetry=0,");

        patch_init_retry(&mut src).unwrap();
        assert!(!src.contains(TSW_INITRETRY_FLAG));
        assert!(!src.contains("if(/boot_config_not_ready/.test(String("));
        assert!(!src.contains("boot_config_not_ready, tsw retry"));
        assert!(!src.contains("e6()}"));
        assert!(src.contains(TSW_INITRETRY2_FLAG));
        assert!(src.contains("setTimeout(()=>{e6(!0)},1500);return}"));
        assert!(src.contains("/boot_config_not_ready|project not found/.test(String(e))"));
        assert_eq!(src.matches("globalThis.__tsw_initRetry=0,").count(), 1);
        assert!(src.contains("await j.initialize(),globalThis.__tsw_initRetry=0,Z.info("));
        let once = src.clone();
        patch_init_retry(&mut src).unwrap();
        assert_eq!(src, once);
    }

    #[test]
    fn capture_pstore_head_inserts_after_brace_and_is_idempotent() {
        let mut src = "async createProject(e=!1){let{projectId:t}=this._projectStore.getState();if(t&&!e)return{project_id:t};try{this._projectStore.actions.setIsCreatingProject(!0)}finally{}}async createProjectWithTimeout(){}".to_string();
        patch_capture_pstore_head(&mut src).unwrap();
        assert_eq!(
            src,
            "async createProject(e=!1){(globalThis.__tsw_projectStore=this._projectStore);/*__TSW_PSTORE_HEAD__*/let{projectId:t}=this._projectStore.getState();if(t&&!e)return{project_id:t};try{this._projectStore.actions.setIsCreatingProject(!0)}finally{}}async createProjectWithTimeout(){}"
        );
        let once = src.clone();
        patch_capture_pstore_head(&mut src).unwrap();
        assert_eq!(src, once);
    }

    const V2_SESSION_RETRY: &str = r#"async function S(e,t,r,n=10,i={}){let o;for(let a=0;a<=n;a++){if(a>0&&i.shouldContinue?.()===!1)throw o;try{return await t()}catch(t){if(o=t,i.shouldContinue?.()===!1)throw t;if(!function(e){if(!e||"object"!=typeof e||!("code"in e))return!0;let{code:t}=e;return"number"!=typeof t||t<=0}(t))throw r.error(`[ai-chat/v2][SessionListService] ${e} failed without retry`,{error:t}),t;if(a>=n)break;r.warn(`[ai-chat/v2][SessionListService] ${e} failed, retrying`,{attempt:a+1,retryCount:n,delayMs:1e3,error:t}),await (0,h.y)(1e3)}}throw r.error(...failed after retries...),o}"#;

    #[test]
    fn v2_retry_patch_marks_503_retryable() {
        let mut src = V2_SESSION_RETRY.to_string();
        patch_v2_retry_503(&mut src).unwrap();
        assert!(src.contains(
            r#"function(e){if(e&&(e.code===503||/boot_config_not_ready/.test(String(e.message||""))))return!0;/*__TSW_V2RETRY__*/if(!e||"#
        ));
        let once = src.clone();
        patch_v2_retry_503(&mut src).unwrap();
        assert_eq!(src, once);
    }

    #[test]
    fn v2_retry_patch_rejects_unknown_shape() {
        let mut no_code = V2_SESSION_RETRY.replace("\"code\"in ", "");
        assert!(patch_v2_retry_503(&mut no_code).is_err());
        assert!(!no_code.contains(TSW_V2RETRY_FLAG));

        let mut renamed = V2_SESSION_RETRY.replace("failed without retry", "failed without backoff");
        assert!(patch_v2_retry_503(&mut renamed).is_err());
        assert!(!renamed.contains(TSW_V2RETRY_FLAG));
    }

    const V2_LITE_NOTFOUND: &str = r#"getSessionLifecyclePort(){return o.XT.tryResolve(i.F.SessionLifecycle)}async deleteSession(e,t){return!0}async initSession(e){try{}catch(r){a.logger.error("[ChatSessionService] Failed to init session:",r);let t=r instanceof Error?r.message:"Unknown error";throw this.sessionStore.relation.setInitError(e,{message:t}),r}}async initSessionForSoloLite(e,t,r){let n=0,i=()=>!0;try{1}catch(o){if(!i())return;a.logger.error("[ChatSessionService] Session init error (Lite):",o);let r=o instanceof Error?o.message:"Unknown error",n=o?.code;throw this.sessionStore.relation.setInitError(e,{message:r,env:t,code:n}),o}finally{}}"#;

    #[test]
    fn v2_notfound_patch_switches_off_missing_session_and_is_idempotent() {
        let mut src = V2_LITE_NOTFOUND.to_string();
        patch_v2_session_not_found(&mut src).unwrap();
        assert!(src.contains("tswV2Navigate(){return o.XT.tryResolve(i.F.Navigate)}"));
        assert!(src.contains(
            r#"if(/session not found/i.test(r)){this.sessionStore.relation.clearInitError(e);try{await this.deleteSession(e,!0)}catch(__tswNf){}try{this.tswV2Navigate()?.navigateHomePage?.()}catch(__tswNf){}return}/*__TSW_V2NOTFOUND__*/throw this.sessionStore.relation.setInitError(e,{message:r,env:t,code:n}),o"#
        ));
        assert!(src.contains("throw this.sessionStore.relation.setInitError(e,{message:t}),r"));
        let once = src.clone();
        patch_v2_session_not_found(&mut src).unwrap();
        assert_eq!(src, once);
    }

    #[test]
    fn v2_notfound_patch_rejects_unknown_shape() {
        let mut renamed = V2_LITE_NOTFOUND.replace("Session init error (Lite):", "Session init failed (Lite):");
        assert!(patch_v2_session_not_found(&mut renamed).is_err());
        assert!(!renamed.contains(TSW_V2NOTFOUND_FLAG));
        assert_eq!(renamed, V2_LITE_NOTFOUND.replace("Session init error (Lite):", "Session init failed (Lite):"));

        let mut no_life = V2_LITE_NOTFOUND.replace("getSessionLifecyclePort(){return ", "getLifecyclePort(){return ");
        assert!(patch_v2_session_not_found(&mut no_life).is_err());
        assert!(!no_life.contains(TSW_V2NOTFOUND_FLAG));
        assert!(!no_life.contains("tswV2Navigate("));
    }
}
