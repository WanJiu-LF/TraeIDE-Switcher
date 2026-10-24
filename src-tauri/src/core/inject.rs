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

/// 移除旧注入体里对 setI18nApiHostByUserInfo 的调用：CN 3.4.4 该方法不存在，
/// 调用会抛 CodeExpectedError 并中断热换号，导致 AI 服务启动异常。
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

// ════════════════ 会话/任务记录归属（固定 project_id，跨号保留对话）════════════════
//
// Trae 的 project 按账号归属。旧版把 project_id 固定成 localStorage 的共享值
// （workbench 的 client_info.project_id + ai-modules-chat 的 setProjectId），
// 换号后仍复用同一个 project，会话列表与任务记录得以跨账号保留。
// 代价：新账号并不拥有该 project，发消息时服务端会返回 project_not_found（「项目不存在」）。
pub const TSW_PID_FLAG: &str = "__TSW_PID__";

const TSW_WS_FLAG: &str = "/*__TSW_WS__*/";
const TSW_MERGE_FLAG: &str = "/*__TSW_MERGE__*/";

/// 把旧版固定 project 的 IIFE 改成直通：保留 marker（状态页可识别），不再改写 project。
fn neutralize_pid_pins(src: &mut String) -> bool {
    let prefix = "(function(_v){/*__TSW_PID__*/";
    let tail = "return _v;})(";
    let mut changed = false;
    loop {
        let Some(start) = src.find(prefix) else {
            break;
        };
        let Some(rel) = src[start..].find(tail) else {
            break;
        };
        let arg_at = start + rel + tail.len();
        let Some(close_rel) = src[arg_at..].find(')') else {
            break;
        };
        let end = arg_at + close_rel + 1;
        let arg = src[arg_at..arg_at + close_rel].to_string();
        let replacement = format!("(function(_v){{/*__TSW_PID__*/return _v}})({arg})");
        src.replace_range(start..end, &replacement);
        changed = true;
    }
    changed
}

/// ⑦ workbench：不再固定 project；只把真实工作区目录暴露给聊天模块，
/// 供本地会话快照按工作区过滤。已安装的旧固定表达式会被改成直通。
fn patch_pid_workbench(src: &mut String) -> Result<bool, String> {
    let mut changed = neutralize_pid_pins(src);
    if !src.contains(TSW_WS_FLAG) {
        let anchor = "workspace_folder:m,";
        let pos = src.find(anchor).ok_or("特征锚未命中：workspace_folder")?;
        let replacement = "workspace_folder:(globalThis.__tsw_ws_folder=m,(function(){try{if(m&&typeof localStorage!==\"undefined\")localStorage.setItem(\"__tsw_ws_folder\",m)}catch(_){}})(),m),/*__TSW_WS__*/";
        src.replace_range(pos..pos + anchor.len(), replacement);
        changed = true;
    }
    Ok(changed)
}

pub const TSW_SEND_PID_FLAG: &str = "__TSW_SENDPID__";
const TSW_SEND_PID_V2_FLAG: &str = "__TSW_SENDPID_V2__";
const SEND_PID_ANCHOR: &str = "let S=ei(e),k=await this.chatApi.sendMessage(eo(e))";
const SEND_PID_V2_PREFIX: &str = "let S=ei(e),__tsw_m=eo(e);";
const SEND_PID_OLD_PREFIX: &str = "let S=ei(e),__tsw_m=eo(e);try{/*";
const SEND_PID_OLD_CALL: &str = "let k=await this.chatApi.sendMessage(__tsw_m)";
pub const TSW_SEND_CTX_FLAG: &str = "__TSW_SENDCTX__";
const SEND_CTX_ANCHOR: &str = r#"async sendMessage(e,t){return this.adapter.invoke({service:"chat",method:"sendMessage",params:e,context:{...this.getContext(),...t&&{_invokeOptions:t}}})}"#;
const SEND_CTX_CODE: &str = r#"async sendMessage(e,t){/*__TSW_SENDCTX__*/const __tsw_c={...this.getContext(),...e&&e.project_id?{project_id:e.project_id}:{},...t&&{_invokeOptions:t}};return this.adapter.invoke({service:"chat",method:"sendMessage",params:e,context:__tsw_c})}"#;
const SUB_CTX_OLD: &str = r#"{service:"chat",method:"subscribe",params:{chat_session_id:e,...r},context:this.getContext(),lastEventId:t?.lastEventId}"#;
const SUB_CTX_NEW: &str = r#"{service:"chat",method:"subscribe",params:{chat_session_id:e,...r},context:{...this.getContext(),...globalThis.__tsw_ctx_project?{project_id:globalThis.__tsw_ctx_project}:{}},lastEventId:t?.lastEventId}/*__TSW_SUBCTX__*/"#;

/// 回滚 0.1.19~0.1.23 的发送项目替换与 context 覆盖，恢复原始发送链路。
fn strip_legacy_send(src: &mut String) -> bool {
    if src.contains(TSW_SEND_PID_V3_FLAG) {
        return false;
    }
    let mut changed = false;
    if let Some(marker) = src.find(TSW_SEND_PID_V2_FLAG) {
        if let Some(start) = src[..marker].rfind(SEND_PID_V2_PREFIX) {
            if let Some(rel) = src[marker..].find(SEND_PID_OLD_CALL) {
                let end = marker + rel + SEND_PID_OLD_CALL.len();
                src.replace_range(start..end, SEND_PID_ANCHOR);
                changed = true;
            }
        }
    } else if let Some(marker) = src.find(TSW_SEND_PID_FLAG) {
        if let Some(start) = src[..marker].rfind(SEND_PID_OLD_PREFIX) {
            if let Some(rel) = src[marker..].find(SEND_PID_OLD_CALL) {
                let end = marker + rel + SEND_PID_OLD_CALL.len();
                src.replace_range(start..end, SEND_PID_ANCHOR);
                changed = true;
            }
        }
    }
    if src.contains(SEND_CTX_CODE) {
        *src = src.replace(SEND_CTX_CODE, SEND_CTX_ANCHOR);
        changed = true;
    }
    if src.contains(SUB_CTX_NEW) {
        *src = src.replace(SUB_CTX_NEW, SUB_CTX_OLD);
        changed = true;
    }
    changed
}

const TSW_MERGE_V3_FLAG: &str = "__TSW_MERGE_V3__";
const SESSION_MERGE_ANCHOR: &str = "o.sessionDomainService.batchSetSessions(u);";
const SESSION_MERGE_CODE: &str = r#"u=(function(__u,__p){try{/*__TSW_MERGE__*//*__TSW_MERGE_V3__*/
var __K="__tsw_session_snapshot_v3",__W=String(globalThis.__tsw_ws_folder||"").split("\\").join("/").toLowerCase(),__C=[];
try{__C=JSON.parse(localStorage.getItem(__K)||"[]")||[]}catch(__e){__C=[]}
var __I=new Set(__u.map(function(s){return s.sessionId})),__O=__u.slice();
for(var __j=0;__j<__C.length;__j++){var __s=__C[__j];
if(!__s||!__s.sessionId||__I.has(__s.sessionId))continue;
var __f=String(__s.mainFolder||(__s.source&&(__s.source.local_folder||__s.source.remote_folder))||"").split("\\").join("/").toLowerCase();
if(__W&&__f&&__f!==__W)continue;
var __n=Object.assign({},__s);
__n.__tsw_owner_project=__s.__tsw_owner_project||__s.localProjectId;
__n.localProjectId=__n.__tsw_owner_project;
__n.remoteProjectId="";__n.status="finished";__n.isCurrent=false;
delete __n.messages;delete __n.pendingMessages;
__O.push(__n);__I.add(__n.sessionId);}
var __P=__O.map(function(s){var x=Object.assign({},s);delete x.messages;delete x.pendingMessages;
x.__tsw_owner_project=x.__tsw_owner_project||s.localProjectId;return x}).slice(0,400);
try{localStorage.setItem(__K,JSON.stringify(__P))}catch(__e){}
return __O;}catch(__e){return __u}})(u,e);o.sessionDomainService.batchSetSessions(u);"#;

fn patch_session_merge(src: &mut String) -> Result<bool, String> {
    if src.contains(TSW_MERGE_V3_FLAG) {
        return Ok(false);
    }
    let Some(anchor_pos) = src.find(SESSION_MERGE_ANCHOR) else {
        return Err("特征锚未命中：session batchSetSessions".into());
    };
    if let Some(flag) = src.find(TSW_MERGE_FLAG) {
        let start = src[..flag]
            .rfind("u=(function(__u,__p)")
            .ok_or("旧会话合并起始锚未命中")?;
        let end = flag + src[flag..].find(SESSION_MERGE_ANCHOR).ok_or("旧会话合并结束锚未命中")?
            + SESSION_MERGE_ANCHOR.len();
        src.replace_range(start..end, SESSION_MERGE_CODE);
        return Ok(true);
    }
    src.replace_range(anchor_pos..anchor_pos + SESSION_MERGE_ANCHOR.len(), SESSION_MERGE_CODE);
    Ok(true)
}

const TSW_SEND_PID_V3_FLAG: &str = "__TSW_SENDPID_V3__";
const SEND_PID_V3_CODE: &str = r#"let S=ei(e),__tsw_m=eo(e);/*__TSW_SENDPID__*//*__TSW_SENDPID_V3__*/
if(!__tsw_m||typeof __tsw_m!=="object")throw new Error("发送请求无效，已停止发送");
if(!globalThis.__tsw_ai||typeof globalThis.__tsw_ai.request!=="function")throw new Error("当前账号项目服务未就绪，已停止发送");
let __tsw_s=null;try{__tsw_s=this.sessionStore&&this.sessionStore.sessionById&&this.sessionStore.sessionById.get?this.sessionStore.sessionById.get(h):null;}catch(__tsw_e){}
const __tsw_src=(__tsw_s&&__tsw_s.source)||__tsw_m.source||{};
const __tsw_f=(__tsw_s&&(__tsw_s.mainFolder||__tsw_s.main_folder))||__tsw_src.local_folder||__tsw_src.remote_folder||(__tsw_s&&__tsw_s.worktreeData&&__tsw_s.worktreeData.worktreePath)||globalThis.__tsw_ws_folder||(typeof localStorage!=="undefined"?localStorage.getItem("__tsw_ws_folder")||"":"")||(__tsw_m.workspace_folders&&__tsw_m.workspace_folders[0])||__tsw_m.workspace_folder||"";
if(!__tsw_f)throw new Error("无法定位会话工作区，已停止发送");
const __tsw_data={project_kind:"workspace",workspace_type:"single_root",single_root_workspace_folder:__tsw_f,workspace_folders:[__tsw_f]};
const __tsw_r=await globalThis.__tsw_ai.request({service:"lite",method:"get_or_create_project_v2",data:__tsw_data,context:{}});
const __tsw_project=(r=>{let x=r;for(let i=0;i<8&&x&&typeof x==="object";i++){if(x.ok===false||x.success===false)throw new Error(x.message||x.error||"项目查询失败");if(x.code!==undefined&&String(x.code)!=="0")throw new Error(x.message||x.msg||"项目查询失败 (code "+x.code+")");if(typeof x.project_id==="string"&&x.project_id)return x;if(typeof x.json==="string"){try{x=JSON.parse(x.json);continue}catch(__e){}}if(x.data!==undefined){x=x.data;continue}break}throw new Error("项目查询响应缺少有效 ID")})(__tsw_r);
if(typeof __tsw_project.project_id!=="string"||!/^[0-9a-f]{24}$/i.test(__tsw_project.project_id))throw new Error("当前账号项目 ID 无效，已停止发送");
__tsw_m.project_id=__tsw_project.project_id;
__tsw_m.project_kind="workspace";
globalThis.__tsw_ctx_project=__tsw_project.project_id;
let k=await this.chatApi.sendMessage(__tsw_m)"#;

fn patch_send_project_v3(src: &mut String) -> Result<bool, String> {
    if src.contains(TSW_SEND_PID_V3_FLAG) {
        return Ok(false);
    }
    let Some(pos) = src.find(SEND_PID_ANCHOR) else {
        return Err("特征锚未命中：chatApi.sendMessage(eo(e))".into());
    };
    src.replace_range(pos..pos + SEND_PID_ANCHOR.len(), SEND_PID_V3_CODE);
    Ok(true)
}

fn patch_send_context(src: &mut String) -> Result<bool, String> {
    if src.contains(TSW_SEND_CTX_FLAG) {
        return Ok(false);
    }
    let Some(pos) = src.find(SEND_CTX_ANCHOR) else {
        return Err("特征锚未命中：chat sendMessage context".into());
    };
    src.replace_range(pos..pos + SEND_CTX_ANCHOR.len(), SEND_CTX_CODE);
    Ok(true)
}

fn patch_subscribe_context(src: &mut String) -> Result<bool, String> {
    if src.contains("__TSW_SUBCTX__") {
        return Ok(false);
    }
    let Some(pos) = src.find(SUB_CTX_OLD) else {
        return Err("特征锚未命中：chat subscribe context".into());
    };
    src.replace_range(pos..pos + SUB_CTX_OLD.len(), SUB_CTX_NEW);
    Ok(true)
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
    let mut changed = neutralize_pid_pins(&mut src);
    changed |= strip_legacy_send(&mut src);
    let mut errs = Vec::new();
    match patch_send_project_v3(&mut src) {
        Ok(true) => changed = true,
        Ok(false) => {}
        Err(e) => errs.push(format!("发送项目替换失败：{e}")),
    }
    match patch_send_context(&mut src) {
        Ok(true) => changed = true,
        Ok(false) => {}
        Err(e) => errs.push(format!("发送上下文同步失败：{e}")),
    }
    match patch_subscribe_context(&mut src) {
        Ok(true) => changed = true,
        Ok(false) => {}
        Err(e) => errs.push(format!("订阅上下文同步失败：{e}")),
    }
    match patch_session_merge(&mut src) {
        Ok(true) => changed = true,
        Ok(false) => {}
        Err(e) => errs.push(format!("本地会话合并失败：{e}")),
    }
    if !changed {
        if errs.is_empty() {
            return Ok(InjectStatus::AlreadyPatched);
        }
        return Err(io::Error::new(io::ErrorKind::NotFound, errs.join("; ")));
    }
    if !errs.is_empty() {
        crate::core::logger::log(
            crate::core::logger::Level::Warn,
            crate::core::logger::Source::System,
            format!("会话补丁部分未应用：{}", errs.join("; ")),
        );
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
    let mut errs = Vec::new();
    let pm = match patch_pm_token(&mut src) {
        Ok(v) => v,
        Err(e) => {
            errs.push(e);
            false
        }
    };
    let reload_cmd = patch_reload_command(&mut src);
    let neutralized = neutralize_pid_pins(&mut src);
    let already_cmd = src.contains(TSW_CMD_FLAG);
    let already_ret = src.contains(TSW_WRET_FLAG);
    let already_ws = src.contains(TSW_WS_FLAG);
    let already_aic = src.contains(TSW_AIC_FLAG);
    let already_rpc = src.contains(TSW_RPC_FLAG);
    if !pm
        && !reload_cmd
        && !neutralized
        && already_cmd
        && already_ret
        && already_ws
        && already_aic
        && already_rpc
    {
        return Ok(InjectStatus::AlreadyPatched);
    }
    ensure_bak(wb)?;
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
    if !already_ws {
        if let Err(e) = patch_pid_workbench(&mut src) {
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

const TSW_RESTART_AGENT_FLAG: &str = "__TSW_RESTART_AGENT__";
const CMD_BASE: &str = "refreshUserInfo({__TSW_REPLACE__:true,device:p.device,userInfo:p.userInfo})";
const CMD_SUFFIX: &str = r#"/*__TSW_RESTART_AGENT__*/.then(function(r){setTimeout(function(){try{var __pm=i.get(globalThis.__tsw_pm_token);if(__pm&&typeof __pm.restartAiAgent==="function")__pm.restartAiAgent();}catch(_){}setTimeout(function(){try{i.executeCommand("workbench.action.reloadWindow")}catch(_){}},5000);return r;})/*__TSW_CMD__*/"#;

const TSW_PM_FLAG: &str = "/*__TSW_PM__*/";

/// 暴露 IProcessManagerAgentService 的服务标识，供 _tsw.replaceUserInfo 重启 ai-agent。
fn patch_pm_token(src: &mut String) -> Result<bool, String> {
    if src.contains(TSW_PM_FLAG) {
        return Ok(false);
    }
    let needle = "ge(\"IProcessManagerAgentService\")";
    let Some(pos) = src.find(needle) else {
        return Err("特征锚未命中：IProcessManagerAgentService".into());
    };
    let Some(eq) = src[..pos].rfind('=') else {
        return Err("服务标识赋值形态异常".into());
    };
    let Some((_, name)) = ident_before(src.as_bytes(), eq) else {
        return Err("服务标识变量名解析失败".into());
    };
    let Some(rel) = src[pos..].find(';') else {
        return Err("服务标识语句未闭合".into());
    };
    let at = pos + rel + 1;
    src.insert_str(at, &format!("globalThis.__tsw_pm_token={name};{TSW_PM_FLAG}"));
    Ok(true)
}

/// 换号后重启 ai-agent 进程：agent 进程仍持有旧账号的连接凭据，只重载渲染窗口不会重连；
/// 先重启 agent，再重载窗口，聊天服务按新账号重新初始化。
fn patch_reload_command(src: &mut String) -> bool {
    if src.contains(TSW_RESTART_AGENT_FLAG) {
        return false;
    }
    let Some(base) = src.find(CMD_BASE) else {
        return false;
    };
    let after = base + CMD_BASE.len();
    let Some(rel) = src[after..].find("/*__TSW_CMD__*/") else {
        return false;
    };
    let end = after + rel + "/*__TSW_CMD__*/".len();
    src.replace_range(after..end, CMD_SUFFIX);
    true
}

/// ④ 命令注册：锚 X.registerCommand("icube.common.refreshUserInfo",async function(i,e){await i.get(Y).refreshUserInfo(!0)})
fn patch_register_command(src: &mut String) -> Result<(), String> {
    let (caller, token, anchor_end) = find_register_anchor(src)?;
    let template = r#",__CALLER__.registerCommand("_tsw.replaceUserInfo",async function(i,payload){const p=payload&&typeof payload==="object"&&payload.userInfo?payload:{userInfo:payload};return await i.get(__TOKEN__).refreshUserInfo({__TSW_REPLACE__:true,device:p.device,userInfo:p.userInfo})__SUFFIX__})"#;
    let insertion = template
        .replace("__CALLER__", &caller)
        .replace("__TOKEN__", &token)
        .replace("__SUFFIX__", CMD_SUFFIX);
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

/// 跨号会话补丁是否已安装（本地列表合并 + 发送项目替换）
fn scan_chat_pinned(paths: &IdePaths) -> bool {
    std::fs::read_to_string(paths.chat_module_js())
        .map(|s| s.contains(TSW_MERGE_FLAG) && s.contains(TSW_SEND_PID_V3_FLAG))
        .unwrap_or(false)
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
