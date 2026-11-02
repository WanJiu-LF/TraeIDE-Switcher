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
/// ⑪ 注册 _tsw.reloadAgent（换号后重建 agent 会话栈）
///
/// 当前代标记（第四代）：第三代 30s 去抖在改挂会话后会跳过进程重启，新库读不进去。
/// 第四代：`force` 绕过去抖。装过旧代的机器靠「命令 id 定位 + 整块剥掉 + 重插」迁移。
const TSW_RLD_FLAG: &str = "/*__TSW_RLD4__*/";

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

/// ⑮ main.js：Trae 自己重启 ai-agent 时，在「已停、未启」之间给换号器留出改写对话库的窗口。
/// 3.4.11 起 agent 被结束后进程管理器会立刻拉起，换号器自己杀进程改库会和新进程抢库、
/// 把进程管理器打成一直健康检查超时；performAiAgentRestart 期间 _processRestarting 为真，
/// 不会自动拉起，在它启动新进程前等换号器写完最安全。
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
            ensure_bak(main)?;
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
    let already_aic = src.contains(TSW_AIC_FLAG);
    let already_rpc = src.contains(TSW_RPC_FLAG);
    let already_rld = src.contains(TSW_RLD_FLAG);
    if already_cmd && already_ret && already_aic && already_rpc && already_rld {
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
/// 锚 = initDefaultClient 内的 `=await this.createClient(<arg>)`，两种形态：
/// - 直接赋字段：国际版压缩态 `this.a=await this.createClient(t)`、CN 3.4.9 `this._client=await this.createClient(t)`
/// - 先进局部变量（CN 3.4.11 起）：`const t=…,s=await this.createClient(t);…this._client=s,…`，
///   声明列表里不能插赋值，改接在 `this.<field>=<var>` 之后
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
    let (ident_start, var) = ident_before(src.as_bytes(), at).ok_or("客户端字段解析失败")?;
    let arg_start = at + call.len();
    let close = src[arg_start..].find(')').ok_or("createClient 调用未闭合")?;
    let after_call = arg_start + close + 1;
    let expose = |field: &str| format!(",globalThis.__tsw_ai={field},globalThis.__tsw_aiHost=this{TSW_AIC_FLAG}");
    if src[..ident_start].ends_with("this.") {
        let field = src[ident_start - 5..at].to_string();
        src.insert_str(after_call, &expose(&field));
        return Ok(());
    }
    let needle = format!("={var}");
    let win = (after_call + 600).min(src.len());
    let mut from = after_call;
    while let Some(rel) = src[from..win].find(&needle) {
        let eq = from + rel;
        let end = eq + needle.len();
        from = end;
        if src.as_bytes().get(end).is_some_and(|b| is_ident_byte(*b)) {
            continue;
        }
        let Some((field_start, _)) = ident_before(src.as_bytes(), eq) else { continue };
        if src[..field_start].ends_with("this.") {
            let field = src[field_start - 5..eq].to_string();
            src.insert_str(end, &expose(&field));
            return Ok(());
        }
    }
    Err("客户端字段形态异常（createClient 结果没有赋给 this.<field>）".into())
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
const RELOAD_TMPL: &str = r#".registerCommand("_tsw.reloadAgent",async function(__acc,__p){/*__TSW_RLD4__*/
try{
let __proc={ok:false,error:"未定位 agent 进程服务令牌"};
try{
const __pm=__acc&&typeof __acc.get==="function"?__acc.get(__PM__):null;
if(!__pm||typeof __pm.restartAiAgent!=="function"){__proc={ok:false,error:"进程服务无 restartAiAgent（结构变动）"};}
else{
const __now=Date.now();
const __last=globalThis.__tsw_lastAgentRestart||0;
if(__now-__last<30000&&!(__p&&__p.force)){
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

/// ⑪ workbench：注册 _tsw.reloadAgent —— 换号后重建 AI 会话栈。
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
/// (install_dir, main_mtime_ms, wb_mtime_ms, tsw_patched, patch_at_ms)
static PATCH_CACHE: std::sync::Mutex<Vec<(String, u64, u64, bool, u64)>> =
    std::sync::Mutex::new(Vec::new());

/// 会话按账号重建：聊天侧 project store 会缓存 project_id，换号后仍拿旧账号的 id 调
/// `get_or_create_project_v2`，agent 按 (user_id, project_id) 校验直接回 5000001
/// project not found（面板「服务启动异常」）。这里只让缓存按账号失效：
/// initChatView 记录本次登录 userId；createProject 发现缓存项目不属于当前账号时忽略缓存，
/// 重新按当前账号取项目；成功设置 project_id 后记录其归属账号。
const TSW_UIDCACHE_FLAG: &str = "/*__TSW_UIDCACHE__*/";

fn patch_chat_user_cache(src: &mut String) -> Result<(), String> {
    if src.contains(TSW_UIDCACHE_FLAG) {
        return Ok(());
    }
    let uid_anchor = "let n=await ei.getUserInfo(),i=null!=n,o=n?.userId||\"\";";
    let at = src.find(uid_anchor).ok_or("特征锚未命中：initChatView getUserInfo")?;
    src.insert_str(at + uid_anchor.len(), "globalThis.__tsw_uid=o;");

    let early = "if(t&&!e)return{project_id:t};";
    let at = src.find(early).ok_or("特征锚未命中：createProject 缓存短路")?;
    let cond = "if(t&&!e&&(function(){try{return localStorage.getItem(\"__tsw_pid_uid\")||\"\"}catch(_){return \"\"}})()===globalThis.__tsw_uid)return{project_id:t};";
    src.replace_range(at..at + early.len(), cond);

    let set_anchor = "this._projectStore.actions.setProjectId(e.project_id,e.real_project_id)";
    let at = src.find(set_anchor).ok_or("特征锚未命中：setProjectId")?;
    src.insert_str(
        at + set_anchor.len(),
        ",(function(){try{localStorage.setItem(\"__tsw_pid_uid\",globalThis.__tsw_uid||\"\")}catch(_){}})()",
    );
    src.push_str(TSW_UIDCACHE_FLAG);
    Ok(())
}

/// 4011 自动恢复：服务端风控拒绝（ABNORMAL_ACCOUNT_LIMIT / ErrAccessReject，界面文案
/// 「操作过于频繁啦，请稍候再试。」）会直接掐断当前轮次。客户端自带的「上游错误自动重试」
/// 默认只认 [-1,3003]、最多 1 次、退避 200ms×n —— 4011 一律判死，且服务端限流窗口
/// 实测十几秒到一分钟，默认退避根本等不到解除。把 4011 并入可重试码，退避拉到 8s×n、
/// 次数拉到 10，服务端解除限制后本轮自动续上，用户只需等，不用手点重试。
const TSW_RETRY4011_FLAG: &str = "/*__TSW_RETRY4011__*/";

fn patch_chat_retry_4011(src: &mut String) -> Result<(), String> {
    if src.contains(TSW_RETRY4011_FLAG) {
        return Ok(());
    }
    // ① 三处默认码表补 4011（v2 ChatSessionService / SideChat 流服务 / shouldAutoRetry）
    let codes = "retryableErrorCodes??[-1,3003]";
    if src.matches(codes).count() == 0 {
        return Err("特征锚未命中：retryableErrorCodes 默认码表".into());
    }
    *src = src.replace(codes, "retryableErrorCodes??[-1,3003,4011]");

    // ② v2 ChatSessionService：动态配置给了码表时也强制并入 4011，且缺 enable 不当作关闭。
    //    ③ 重试次数与退避：默认 1 次 / 200ms 线性退避等不到服务端限流解除。
    //    压缩后的局部变量名随版本变化（CN 3.4.x 为 s/c，intl 3.6.x 为 l/d），两套形态都试。
    let shapes = [
        (
            "l=s?.retryableErrorCodes??[-1,3003,4011];",
            "l=[...(((s&&s.retryableErrorCodes)||[-1,3003])),4011];s={...(s||{}),enable:!0};",
            "let c=Math.max(0,s?.maxRetryCount??1)",
            "let c=Math.max(10,s?.maxRetryCount??1)",
            "let h=m.attempt*Math.max(0,s?.baseDelayMs??200);",
            "let h=Math.max(8000,m.attempt*Math.max(8000,s?.baseDelayMs??8000));",
        ),
        (
            "c=l?.retryableErrorCodes??[-1,3003,4011];",
            "c=[...(((l&&l.retryableErrorCodes)||[-1,3003])),4011];l={...(l||{}),enable:!0};",
            "let d=Math.max(0,l?.maxRetryCount??1)",
            "let d=Math.max(10,l?.maxRetryCount??1)",
            "let h=m.attempt*Math.max(0,l?.baseDelayMs??200);",
            "let h=Math.max(8000,m.attempt*Math.max(8000,l?.baseDelayMs??8000));",
        ),
    ];
    let mut ok = false;
    for (guard, guard_new, max, max_new, delay, delay_new) in shapes {
        if src.contains(guard) && src.contains(max) && src.contains(delay) {
            *src = src.replace(guard, guard_new);
            *src = src.replace(max, max_new);
            *src = src.replace(delay, delay_new);
            ok = true;
            break;
        }
    }
    if !ok {
        return Err("特征锚未命中：scheduleAutoRetry 码表/次数/退避".into());
    }

    // ④ SideChat 流服务同口径
    let side = ",d=c?.retryableErrorCodes??[-1,3003,4011],u=c?.enable??!0,p=c?.maxRetryCount??1,";
    if !src.contains(side) {
        return Err("特征锚未命中：SideChat 重试码表".into());
    }
    *src = src.replace(
        side,
        ",d=[...(((c&&c.retryableErrorCodes)||[-1,3003])),4011],u=!0,p=Math.max(10,(c&&c.maxRetryCount)||1),",
    );

    let should = "(n?.retryableErrorCodes??[-1,3003,4011]).includes(e)";
    if !src.contains(should) {
        return Err("特征锚未命中：SideChat shouldAutoRetry".into());
    }
    *src = src.replace(
        should,
        "[...(((n&&n.retryableErrorCodes)||[-1,3003])),4011].includes(e)",
    );

    src.push_str(TSW_RETRY4011_FLAG);
    Ok(())
}

fn ensure_bak_named(file: &std::path::Path) -> io::Result<()> {
    let name = file.file_name().unwrap_or_default().to_string_lossy().to_string();
    let bak = file.with_file_name(format!("{name}.switcher.bak"));
    if !bak.exists() {
        std::fs::copy(file, &bak)?;
    }
    Ok(())
}

pub fn patch_chat(paths: &IdePaths) -> io::Result<InjectStatus> {
    let chat = paths.chat_module_js();
    if !chat.exists() {
        return Err(io::Error::new(io::ErrorKind::NotFound, "未找到 ai-modules-chat 模块"));
    }
    let mut src = std::fs::read_to_string(&chat)?;
    let mut changed = false;
    if !src.contains(TSW_UIDCACHE_FLAG) {
        patch_chat_user_cache(&mut src).map_err(|e| io::Error::new(io::ErrorKind::NotFound, e))?;
        changed = true;
    }
    // 4011 自动重试失败只降级：老版本锚点变动时不影响按账号重建会话（半成品不落盘）
    if !src.contains(TSW_RETRY4011_FLAG) {
        let mut tmp = src.clone();
        match patch_chat_retry_4011(&mut tmp) {
            Ok(()) => {
                src = tmp;
                changed = true;
            }
            Err(e) => crate::core::logger::log(
                crate::core::logger::Level::Warn,
                crate::core::logger::Source::System,
                format!("4011 自动重试补丁未生效（服务端限流时仍需手动重试）: {e}"),
            ),
        }
    }
    if !changed {
        return Ok(InjectStatus::AlreadyPatched);
    }
    ensure_bak_named(&chat)?;
    std::fs::write(&chat, src)?;
    Ok(InjectStatus::Patched)
}

fn file_mtime_ms(p: &std::path::Path) -> Option<u64> {
    std::fs::metadata(p)
        .ok()?
        .modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .map(|d| d.as_millis() as u64)
}

/// (TSW 六补丁是否齐全, 补丁落盘时刻)
fn patch_state(paths: &IdePaths) -> (bool, u64) {
    let key = paths.install_dir.to_string_lossy().to_string();
    let main_m = file_mtime_ms(&paths.main_js()).unwrap_or(0);
    let wb_m = file_mtime_ms(&paths.workbench_js()).unwrap_or(0);
    {
        let cache = PATCH_CACHE.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((_, cm, cw, patched, at)) = cache.iter().find(|e| e.0 == key) {
            if *cm == main_m && *cw == wb_m {
                return (*patched, *at);
            }
        }
    }
    let patched = scan_tsw_patched(paths);
    let at = main_m.max(wb_m);
    {
        let mut cache = PATCH_CACHE.lock().unwrap_or_else(|e| e.into_inner());
        cache.retain(|e| e.0 != key);
        cache.push((key, main_m, wb_m, patched, at));
    }
    (patched, at)
}

/// 热换号就绪判定：main.js 双补丁 + workbench 命令注册
fn scan_tsw_patched(paths: &IdePaths) -> bool {
    let main_ok = std::fs::read_to_string(paths.main_js())
        .map(|s| s.contains(TSW_V2_FLAG) && s.contains(TSW_ICDRS_FLAG))
        .unwrap_or(false);
    main_ok
        && std::fs::read_to_string(paths.workbench_js())
            .map(|s| s.contains(TSW_CMD_FLAG))
            .unwrap_or(false)
}

/// 补丁落盘时刻（main.js / workbench 中较晚者）：与运行实例启动时刻比对，
/// 判断运行中的 IDE 是否已加载补丁（先启动后打补丁 = 内存仍是旧 bundle）
pub fn patch_mtime_ms(paths: &IdePaths) -> Option<u64> {
    Some(patch_state(paths).1)
}

/// shim 通道就绪判定：main.js + workbench 双补丁齐全
pub fn is_tsw_patched(paths: &IdePaths) -> bool {
    patch_state(paths).0
}

/// 一次打齐 CDP 桥 + main 三补丁 + workbench 四补丁 + 聊天模块按账号重建会话
pub fn patch_all(paths: &IdePaths) -> io::Result<String> {
    patch(paths)?;
    patch_main(paths)?;
    patch_workbench(paths)?;
    patch_chat(paths)?;
    Ok("无感换号支持安装完成：__sw 桥 + 无感换号 handler + workbench 命令 + 会话按账号重建".into())
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

    const INIT_CLIENT_3411: &str = "this.initDefaultClient()}async initDefaultClient(){const e=this._remoteAgentService.getConnection();e&&await Promise.all([e.getInitialConnectionTimeMs()]);const t=await this.provideConfig(),s=await this.createClient(t);this._client||this._register(r);const n=this._client!==void 0&&this._client!==s;this._client=s,n&&this._onDidChangeRuntime.fire(),this._clientPromise.complete(this._client)}";

    #[test]
    fn ai_client_patch_handles_local_var_shape() {
        let mut src = INIT_CLIENT_3411.to_string();
        patch_ai_client(&mut src).unwrap();
        assert!(src.contains(&format!(
            "this._client=s,globalThis.__tsw_ai=this._client,globalThis.__tsw_aiHost=this{TSW_AIC_FLAG},n&&"
        )));
        assert!(src.contains("s=await this.createClient(t);"));
        assert_eq!(src.matches(TSW_AIC_FLAG).count(), 1);
    }

    #[test]
    fn ai_client_patch_keeps_field_shape() {
        let mut src = "async initDefaultClient(){const t=await this.provideConfig();this.a=await this.createClient(t),this.b.complete(this.a)}".to_string();
        patch_ai_client(&mut src).unwrap();
        assert!(src.contains(&format!(
            "this.a=await this.createClient(t),globalThis.__tsw_ai=this.a,globalThis.__tsw_aiHost=this{TSW_AIC_FLAG},this.b"
        )));
    }

    /// 在本机 Trae CN 安装文件的副本上跑全部补丁（安装目录不动），确认新版本的特征锚都还命中
    #[test]
    #[ignore = "reads the local Trae CN install"]
    fn dry_run_all_patches_on_local_install_copy() {
        let Some(paths) = IdePaths::detect(crate::core::paths::IdeKind::TraeCn) else { return };
        let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join("inject-dryrun");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let main = dir.join("main.js");
        let wb = dir.join("workbench.desktop.main.js");
        let chat = dir.join("index.mjs");
        std::fs::copy(paths.main_js(), &main).unwrap();
        std::fs::copy(paths.workbench_js(), &wb).unwrap();
        std::fs::copy(paths.chat_module_js(), &chat).unwrap();
        let err = |r: io::Result<InjectStatus>| r.err().map(|e| e.to_string());
        let results = [
            ("bridge", err(patch_file(&main))),
            ("main", err(patch_main_file(&main))),
            ("workbench", err(patch_workbench_file(&wb))),
        ];
        for (name, e) in &results {
            eprintln!("{name}: {}", e.as_deref().unwrap_or("ok"));
        }
        assert!(results.iter().all(|(_, e)| e.is_none()));
        let patched_chat = std::fs::read_to_string(&chat).unwrap();
        let mut chat_src = patched_chat.clone();
        patch_chat_user_cache(&mut chat_src).expect("chat uidcache patch");
        assert!(chat_src.contains(TSW_UIDCACHE_FLAG));
        assert!(chat_src.contains("globalThis.__tsw_uid=o;"));
        let wb_src = std::fs::read_to_string(&wb).unwrap();
        for (name, ok) in [
            ("workbench command", wb_src.contains(TSW_CMD_FLAG)),
            ("reloadAgent v4", wb_src.contains(TSW_RLD_FLAG)),
        ] {
            eprintln!("{name}: {ok}");
            assert!(ok, "{name} missing");
        }
    }
}
