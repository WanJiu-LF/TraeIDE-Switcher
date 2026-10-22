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
try{const _found=[];const _seen=new Set();
function _deep(o,d2){if(!o||typeof o!=="object"||d2>4||_seen.has(o))return;_seen.add(o);
if(typeof o.setI18nApiHostByUserInfo==="function")_found.push(o);
if(o instanceof Map){o.forEach(v=>_deep(v,d2+1));}else{for(const _k of Object.getOwnPropertyNames(o)){try{_deep(o[_k],d2+1);}catch(_){}}}}
_deep(this,0);
for(const _oas of _found){_oas._apiHost=t.userInfo.host;try{_oas.setI18nApiHostByUserInfo(t.userInfo);}catch(_e){}}
}catch(_e){}
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

// ════════════════ 会话/任务记录归属（已移除 project_id 固定补丁）════════════════
//
// Trae 的 project 由服务端按账号创建并归属，客户端 createProject 拿到的 project_id
// 只对当前账号有效。旧版为了跨号保留对话记录，把 project_id 固定成 localStorage 的
// 共享值（workbench 的 client_info.project_id + ai-modules-chat 的 setProjectId）。
// 换号后新账号并不拥有该 project，服务端返回 project_not_found（UI「项目不存在」）。
//
// 现改为移除固定表达式，恢复原生按账号隔离：每个账号使用自己 createProject 得到的
// project_id。下面的 unpin_pid_expr 负责把旧安装里已经写入的固定表达式还原。
pub const TSW_PID_FLAG: &str = "__TSW_PID__";

/// 把旧版 `(function(_v){/*__TSW_PID__*/...})(ARG)` 还原为 `ARG`。
/// 返回是否发生了替换；幂等，可对 workbench / ai-modules-chat 反复调用。
fn unpin_pid_expr(src: &mut String) -> bool {
    let prefix = "(function(_v){/*__TSW_PID__*/";
    let mut changed = false;
    while let Some(start) = src.find(prefix) {
        let Some(call_rel) = src[start..].find("})(") else { break };
        let call_at = start + call_rel + 3;
        let Some(end_rel) = src[call_at..].find(')') else { break };
        let arg = src[call_at..call_at + end_rel].to_string();
        let end = call_at + end_rel + 1;
        src.replace_range(start..end, &arg);
        changed = true;
    }
    changed
}

fn ensure_bak_named(file: &std::path::Path) -> io::Result<()> {
    let name = file.file_name().unwrap_or_default().to_string_lossy().to_string();
    let bak = file.with_file_name(format!("{name}.switcher.bak"));
    if !bak.exists() {
        std::fs::copy(file, &bak)?;
    }
    Ok(())
}

/// 会话归属迁移（ai-modules-chat：移除旧版 project_id 固定，恢复按账号隔离）
pub fn patch_chat(paths: &IdePaths) -> io::Result<InjectStatus> {
    patch_chat_file(&paths.chat_module_js())
}

pub fn patch_chat_file(chat: &std::path::Path) -> io::Result<InjectStatus> {
    if !chat.exists() {
        return Err(io::Error::new(io::ErrorKind::NotFound, "未找到 ai-modules-chat 模块"));
    }
    let mut src = std::fs::read_to_string(chat)?;
    if !unpin_pid_expr(&mut src) {
        return Ok(InjectStatus::AlreadyPatched);
    }
    ensure_bak_named(chat)?;
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

pub fn patch_main_file(main: &std::path::Path) -> io::Result<InjectStatus> {
    let mut src = std::fs::read_to_string(main)?;
    let repaired = repair_v2_require(&mut src);
    if src.contains(TSW_V2_FLAG) && src.contains(TSW_ICDRS_FLAG) {
        if repaired {
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
    // 旧安装可能带 project_id 固定表达式：先还原，再判断其余补丁
    let unpinned = unpin_pid_expr(&mut src);
    let already_cmd = src.contains(TSW_CMD_FLAG);
    let already_ret = src.contains(TSW_WRET_FLAG);
    let already_aic = src.contains(TSW_AIC_FLAG);
    let already_rpc = src.contains(TSW_RPC_FLAG);
    if already_cmd && already_ret && already_aic && already_rpc && !unpinned {
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
    // 会话用量链路：客户端暴露 + RPC 代理命令（结构面变动只降级，不影响热换号）
    if !already_aic {
        if let Err(e) = patch_ai_client(&mut src) {
            errs.push(format!("agent 客户端暴露失败：{e}"));
        }
    }
    if !already_rpc {
        if let Err(e) = patch_register_rpc(&mut src) {
            errs.push(format!("chatRpc 命令注册失败：{e}"));
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
        &format!(",globalThis.__tsw_ai={field}{TSW_AIC_FLAG}"),
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
if(!ctx.project_id&&p.data&&typeof p.data.project_id==="string")ctx.project_id=p.data.project_id;
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

/// (TSW 六补丁是否齐全, 旧 project_id 固定补丁是否仍在, 补丁落盘时刻)
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

/// 旧版 project_id 固定补丁是否仍在（workbench + ai-modules-chat 双处）
fn scan_chat_pinned(paths: &IdePaths) -> bool {
    let wb = std::fs::read_to_string(paths.workbench_js())
        .map(|s| s.contains(TSW_PID_FLAG))
        .unwrap_or(false);
    let chat = std::fs::read_to_string(paths.chat_module_js())
        .map(|s| s.contains(TSW_PID_FLAG))
        .unwrap_or(false);
    // 任一处仍有固定表达式都可能导致换号后 project_not_found
    wb || chat
}

/// 旧版 project_id 固定补丁是否仍在（GUI 展示用）
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

/// 一次打齐 CDP 桥 + main 三补丁 + workbench 四补丁，并迁移旧 project_id 固定补丁
pub fn patch_all(paths: &IdePaths) -> io::Result<String> {
    patch(paths)?;
    patch_main(paths)?;
    patch_workbench(paths)?;
    // 迁移独立于热换号链路：失败只降级（旧固定仍在时换号后会报「项目不存在」）
    let chat_note = match patch_chat(paths) {
        Ok(_) => "会话按账号隔离",
        Err(e) => {
            crate::core::logger::log(
                crate::core::logger::Level::Warn,
                crate::core::logger::Source::System,
                format!("会话归属迁移未生效（旧 project_id 固定可能仍在，换号后会报项目不存在）: {e}"),
            );
            "会话归属迁移失败（见日志）"
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
