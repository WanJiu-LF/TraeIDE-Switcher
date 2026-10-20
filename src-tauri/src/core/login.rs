// 内嵌登录采集：PKCE 登录 URL + 本地回调收 code + AuthCode 换 token
// URL 模板来源：main.js loginUrlBuilder.js（Die.buildLoginUrl）实测提取
use super::auth::AuthInfo;
use super::bytecrypto;
use super::paths::{machine_model, os_version, IdeKind, IdePaths};
use super::quota::api_host;
use super::refresh::{build_device_info, client_version, device_ids, CLIENT_ID};
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::atomic::{AtomicU16, AtomicU64, Ordering};
use std::sync::Mutex;

#[derive(Debug)]
pub enum LoginError {
    Runtime(String),
}

impl std::fmt::Display for LoginError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Runtime(e) => write!(f, "login: {e}"),
        }
    }
}

impl std::error::Error for LoginError {}

static RESULT: Mutex<Option<Result<String, String>>> = Mutex::new(None);
/// 采集代次：start/cancel 各自递增。accept 循环只在代次与自己一致时生效，
/// 代次被顶掉即退出并释放端口（取消后不会留下僵尸服务器）。
static GEN: AtomicU64 = AtomicU64::new(0);
/// 当前回调端口：cancel 用一次本地连接唤醒阻塞在 accept 的旧线程
static PORT: AtomicU16 = AtomicU16::new(0);

/// 拆掉上一次采集：清状态 + 顶掉代次 + 唤醒旧 accept 线程
fn teardown() {
    GEN.fetch_add(1, Ordering::SeqCst);
    *RESULT.lock().unwrap() = None;
    let p = PORT.swap(0, Ordering::SeqCst);
    if p != 0 {
        let _ = std::net::TcpStream::connect(("127.0.0.1", p));
    }
}

/// 取消采集：关闭对话框/点"取消采集"时调用（幂等，可重复调用）
pub fn cancel() {
    teardown();
}

fn console_host(ide: &IdeKind) -> &'static str {
    // bootConfig.consoleHost 实测值
    match ide {
        IdeKind::Trae => "https://www.trae.ai",
        IdeKind::TraeCn => "https://www.trae.cn",
    }
}

fn pkce() -> (String, String) {
    use base64::Engine as _;
    use sha2::{Digest, Sha256};
    let mut bytes = [0u8; 32];
    rand::RngCore::fill_bytes(&mut rand::thread_rng(), &mut bytes);
    let verifier: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    let mut h = Sha256::new();
    h.update(verifier.as_bytes());
    let challenge = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(h.finalize());
    (verifier, challenge)
}

/// 诊断日志：记录回调原始请求（%APPDATA%\TraeIDE-Switcher\login-callback.log）
fn log_callback(target: &str) {
    if let Some(base) = std::env::var_os("APPDATA") {
        let dir = std::path::PathBuf::from(base).join("TraeIDE-Switcher");
        let _ = std::fs::create_dir_all(&dir);
        let line = format!("[{}] {}\r\n", now_ms(), &target[..target.len().min(3000)]);
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(dir.join("login-callback.log")) {
            use std::io::Write as _;
            let _ = f.write_all(line.as_bytes());
        }
    }
}

fn now_ms() -> i128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i128
}

fn parse_pairs(s: &str) -> Vec<(String, String)> {
    s.split('&')
        .filter_map(|kv| {
            let mut it = kv.splitn(2, '=');
            Some((percent_decode(it.next()?), percent_decode(it.next().unwrap_or(""))))
        })
        .collect()
}

/// 多形态回调解析（对齐官方 updateLocalCredential：先 decodeURIComponent 整个 query 再 parse；兼容 credential 参数）
fn parse_query_multi(query: &str) -> Vec<(String, String)> {
    let mut out = parse_pairs(query);
    // 整体编码情形：decode 后再 parse（键不重复）
    let decoded = percent_decode(query);
    for (k, v) in parse_pairs(&decoded) {
        if !out.iter().any(|(x, _)| *x == k) {
            out.push((k, v));
        }
    }
    // credential 参数：值为 JSON 或 k=v&k=v 形式
    let cred = out.iter().find(|(k, _)| k.eq_ignore_ascii_case("credential")).map(|(_, v)| v.clone());
    if let Some(cred) = cred {
        if let Ok(j) = serde_json::from_str::<Value>(&cred) {
            if j.get("Token").is_some() {
                out.push(("userJwt".into(), cred.clone()));
            }
            for key in ["userJwt", "userInfo", "refreshToken", "consoleHost"] {
                if let Some(v) = j.get(key).and_then(|x| x.as_str()) {
                    out.push((key.into(), v.to_string()));
                }
            }
        } else {
            for (k, v) in parse_pairs(&cred) {
                out.push((k, v));
            }
        }
    }
    out
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or("");
                match u8::from_str_radix(hex, 16) {
                    Ok(b) => {
                        out.push(b);
                        i += 3;
                    }
                    Err(_) => {
                        out.push(bytes[i]);
                        i += 1;
                    }
                }
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// 提取 userInfo 字段（兼容两种包装与字段名：Data.UserInfo 包装 / 顶层；Avatar/AvatarUrl、Name/ScreenName）
fn extract_user_fields(info: &Value) -> (String, String, String, String, String, String) {
    let ui = info.pointer("/Data/UserInfo").cloned().unwrap_or_else(|| info.clone());
    let str_of = |keys: &[&str]| -> String {
        keys.iter()
            .find_map(|k| ui.get(*k).and_then(|x| x.as_str()).filter(|s| !s.is_empty()))
            .unwrap_or("")
            .to_string()
    };
    (
        str_of(&["UserID", "UserId"]),
        str_of(&["Name", "ScreenName"]),
        str_of(&["Email", "NonPlainTextEmail"]),
        str_of(&["Avatar", "AvatarUrl"]),
        str_of(&["AIRegion"]),
        str_of(&["Region"]),
    )
}

/// 回调直携凭据（userJwt/userInfo JSON）→ 合成 auth blob
/// 对齐官方 updateLocalCredential/convertTokenToUserInfo 的字段结构
fn build_auth_from_credential(
    user_jwt: &str,
    user_info: &Option<String>,
    console_host: &Option<String>,
) -> Result<String, String> {
    let jwt: Value = serde_json::from_str(user_jwt).unwrap_or_else(|_| json!({}));
    let info: Value = user_info
        .as_deref()
        .and_then(|s| serde_json::from_str(s).ok())
        .unwrap_or(json!({}));
    let (user_id, name, email, avatar, ai_region, region) = extract_user_fields(&info);

    let expired_at = {
        let at = jwt.get("TokenExpireAt").and_then(|v| v.as_i64());
        let dur = jwt.get("TokenExpireDuration").and_then(|v| v.as_i64());
        super::refresh::millis_to_iso(match at {
            Some(t) if t > 1_000_000_000_000 => t,
            Some(t) => t * 1000,
            None => super::refresh::now_millis() + dur.unwrap_or(0) * 1000,
        })
    };
    let refresh_expired_at = jwt
        .get("RefreshExpireAt")
        .and_then(|v| v.as_i64())
        .map(|t| super::refresh::millis_to_iso(if t > 1_000_000_000_000 { t } else { t * 1000 }))
        .unwrap_or_default();

    let host = console_host.clone().unwrap_or_else(|| super::quota::api_host(IdeKind::TraeCn, ""));
    let str_of_jwt = |k: &str| jwt.get(k).and_then(|x| x.as_str()).unwrap_or("").to_string();
    let auth = json!({
        "token": str_of_jwt("Token"),
        "refreshToken": str_of_jwt("RefreshToken"),
        "expiredAt": expired_at,
        "refreshExpiredAt": refresh_expired_at,
        "tokenReleaseAt": super::refresh::millis_to_iso(super::refresh::now_millis()),
        "userId": user_id,
        "userRegion": { "_aiRegion": ai_region, "region": region },
        "host": host,
        "account": { "username": name, "email": email, "avatar_url": avatar },
    });
    let _check: AuthInfo = serde_json::from_value(auth.clone()).map_err(|e| e.to_string())?;
    if str_of_jwt("Token").is_empty() && str_of_jwt("RefreshToken").is_empty() {
        return Err("回调凭据缺少 token/refreshToken".to_string());
    }
    Ok(bytecrypto::encrypt_string(&auth.to_string()))
}

/// 启动登录采集：起本地回调服务器并返回登录 URL（供内嵌窗打开）
/// 同机共享身份：设备用本机 aha 注册设备（换设备会 20403 Token device not match）
pub fn start(ide: IdeKind) -> Result<(String, u16), LoginError> {
    let (verifier, challenge) = pkce();
    // 自愈：上一次采集（含被取消的）在这里直接顶掉，保证"取消后还能重新采集"
    teardown();
    let gen = GEN.load(Ordering::SeqCst);

    let (device_id, machine_id, _kp) =
        device_ids(&ide).map_err(|e| LoginError::Runtime(e.to_string()))?;

    let listener = TcpListener::bind("127.0.0.1:0").map_err(|e| LoginError::Runtime(e.to_string()))?;
    let port = listener.local_addr().unwrap().port();
    PORT.store(port, Ordering::SeqCst);

    std::thread::spawn(move || {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15 * 60);
        // 循环 accept：跳过 favicon/无凭据请求，等待真正的登录回调
        // （CN/国际版回调直接携带凭据 userJwt/userInfo/refreshToken，无 code 交换；兼容 code）
        while std::time::Instant::now() < deadline {
            let Ok((mut stream, _)) = listener.accept() else { break };
            // 代次被 start/cancel 顶掉：本次采集已作废，退出并释放端口
            if GEN.load(Ordering::SeqCst) != gen {
                break;
            }
            let mut buf = [0u8; 16384];
            let n = stream.read(&mut buf).unwrap_or(0);
            let req = String::from_utf8_lossy(&buf[..n]).to_string();
            let target = req.split_whitespace().nth(1).unwrap_or("").to_string();
            log_callback(&target);
            let query = target.split_once('?').map(|(_, q)| q).unwrap_or("").to_string();
            let params = parse_query_multi(&query);
            let get = |k: &str| {
                params
                    .iter()
                    .find(|(x, _)| x.eq_ignore_ascii_case(k))
                    .map(|(_, v)| v.clone())
            };

            let outcome = if let Some(err) = get("error_code") {
                Some(Err(format!("登录失败: {err}")))
            } else if let Some(ci) = get("authCodeInfo") {
                // CN 真实回调（HAR 实证）：authCodeInfo={"AuthCode",...} + userInfo(顶层) + host + userRegion
                let code = serde_json::from_str::<Value>(&ci)
                    .ok()
                    .and_then(|j| j.get("AuthCode").and_then(|x| x.as_str()).map(String::from));
                match code {
                    Some(code) => Some(exchange(
                        ide,
                        &code,
                        &verifier,
                        &get("userInfo"),
                        &get("host"),
                        &get("userRegion"),
                    )),
                    None => Some(Err("authCodeInfo 缺少 AuthCode".to_string())),
                }
            } else if let Some(jwt) = get("userJwt") {
                Some(build_auth_from_credential(&jwt, &get("userInfo"), &get("consoleHost")))
            } else if let Some(rt) = get("refreshToken").or_else(|| get("refresh_token")) {
                let jwt = serde_json::json!({ "RefreshToken": rt }).to_string();
                Some(build_auth_from_credential(&jwt, &get("userInfo"), &get("consoleHost")))
            } else if let Some(code) = get("code") {
                Some(exchange(ide, &code, &verifier, &get("userInfo"), &get("host"), &get("userRegion")))
            } else {
                None
            };

            let done = outcome.is_some();
            let body = match &outcome {
                Some(Ok(_)) => "<html><body style='font-family:sans-serif'><h3>登录成功</h3><p>请回到 TraeIDE-Switcher 完成入池。</p></body></html>".to_string(),
                Some(Err(e)) => format!("<html><body style='font-family:sans-serif'><h3>登录失败</h3><p>{e}</p></body></html>"),
                None => "<html><body>waiting...</body></html>".to_string(),
            };
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            let _ = stream.write_all(resp.as_bytes());
            if done {
                *RESULT.lock().unwrap() = outcome;
                break;
            }
        }
        // 超时兜底：避免前端轮询永远 pending
        // （代次须仍属于本次采集：被取消/被下一次 start 顶掉的旧线程不得写入结果）
        if GEN.load(Ordering::SeqCst) == gen {
            let mut r = RESULT.lock().unwrap();
            if r.is_none() {
                *r = Some(Err("等待登录回调超时，请重试".to_string()));
            }
        }
    });

    let trace = format!("{:x}", rand::random::<u64>()) + &format!("{:x}", rand::random::<u64>());
    // 原生 buildLoginUrl（out main.js loginUrlBuilder.js）逐参数复刻：
    //   plugin_version = productService.tronBuildVersion（缺省 "local"）
    //   x_app_version  = productService.appVersion
    //   x_app_type     = quality（stable/insider）
    //   x_device_brand = iCubeSystemInformationService.deviceModel（Windows 下 = BIOS SystemProductName）
    //   x_device_type  = "windows"（osName），x_os_version = os.version()
    //   x_env 仅在 BOE 环境出现（querystring.stringify 跳过 undefined），此处省略
    let paths = IdePaths::detect(ide).ok_or_else(|| LoginError::Runtime("未检测到安装".into()))?;
    let product = paths.product_config();
    let plugin_ver = paths.tron_build_version();
    let app_ver = paths.app_version();
    let app_type = paths.quality();
    let brand = machine_model();
    // channel_name / click_id 仅在 product.json 显式提供时追加（当前 CN/国际均未提供）
    let mut extra = String::new();
    if let Some(ch) = product.get("channelName").and_then(|v| v.as_str()).filter(|s| !s.is_empty()) {
        extra.push_str(&format!("&channel_name={}", percent_encode(ch)));
    }
    let url = format!(
        "{}/authorization?login_version=1&auth_from=trae&login_channel=native_ide\
         &plugin_version={}&auth_type=local&client_id={}&redirect=0&login_trace_id={}\
         &auth_callback_url=http://127.0.0.1:{}/authorize&machine_id={}&device_id={}\
         &x_device_id={}&x_machine_id={}&x_device_brand={}&x_device_type=windows\
         &x_os_version={}&x_app_version={}&x_app_type={}\
         &code_challenge={}&code_challenge_method=S256{}",
        console_host(&ide),
        plugin_ver,
        CLIENT_ID,
        trace,
        port,
        machine_id,
        device_id,
        device_id,
        machine_id,
        brand,
        os_version(),
        app_ver,
        app_type,
        challenge,
        extra
    );
    Ok((url, port))
}

/// 最小 percent-encode（仅保留 RFC3986 非保留字符）
fn percent_encode(s: &str) -> String {
    s.bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
                (b as char).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect()
}

/// 轮询捕获结果：Ok(auth_blob_b64) / Err(原因) / None(进行中)
pub fn poll() -> Option<Result<String, String>> {
    RESULT.lock().unwrap().take()
}

/// AuthCode 换 token（body 无 DeviceProof，见 exchangeTokenByAuthCode），并合成完整 auth blob
fn exchange(
    ide: IdeKind,
    code: &str,
    code_verifier: &str,
    user_info: &Option<String>,
    host_opt: &Option<String>,
    user_region: &Option<String>,
) -> Result<String, String> {
    let device_info = build_device_info(&ide).map_err(|e| e.to_string())?;
    let body = json!({
        "ClientID": CLIENT_ID,
        "AuthCode": code,
        "CodeVerifier": code_verifier,
        "DeviceInfo": serde_json::to_value(device_info).unwrap(),
        "IDEVersion": client_version(&ide),
    });
    // 回调 host 参数优先（CN 实测 https://api.trae.com.cn）
    let region = user_region.clone().unwrap_or_default();
    let host = host_opt
        .clone()
        .filter(|h| h.starts_with("http"))
        .unwrap_or_else(|| api_host(ide, &region));
    let resp = super::http::agent()
        .post(&format!("{host}/trae/api/v3/oauth/ExchangeToken"))
        .timeout(std::time::Duration::from_secs(15))
        .set("Content-Type", "application/json")
        .set("x-cloudide-token", "")
        .send_json(body)
        .map_err(|e| match e {
            ureq::Error::Status(code, r) => {
                format!("HTTP {code}: {}", r.into_string().unwrap_or_default())
            }
            other => other.to_string(),
        })?;
    let v: Value = resp.into_json().map_err(|e| e.to_string())?;
    if let Some(code) = v.pointer("/ResponseMetadata/Error/Code").and_then(|c| c.as_str()) {
        return Err(format!("登录失败: {code}"));
    }
    let result = v.get("Result").ok_or("响应缺少 Result")?;
    let str_of = |k: &str| result.get(k).and_then(|x| x.as_str()).unwrap_or("").to_string();
    let uid = result.get("UserID").or_else(|| result.get("UserId"));
    let (ui_id, name, email, avatar, ai_region, mut reg) = user_info
        .as_deref()
        .and_then(|s| serde_json::from_str::<Value>(s).ok())
        .map(|info| extract_user_fields(&info))
        .unwrap_or_default();
    if reg.is_empty() {
        reg = region.clone();
    }
    let user_id = if ui_id.is_empty() { uid.and_then(|x| x.as_str()).unwrap_or("").to_string() } else { ui_id };
    let info = json!({
        "token": result.get("Token").and_then(|x| x.as_str()).unwrap_or(""),
        "refreshToken": result.get("RefreshToken").and_then(|x| x.as_str()).unwrap_or(""),
        "expiredAt": result.get("TokenExpireAt").and_then(|x| x.as_i64()).unwrap_or(0).to_string(),
        "refreshExpiredAt": str_of("RefreshExpireAt"),
        "userId": user_id,
        "userRegion": { "_aiRegion": ai_region, "region": reg },
        "host": host,
        "account": { "username": name, "email": email, "avatar_url": avatar },
    });
    // 结构校验（复用 AuthInfo 形状）
    let _check: AuthInfo = serde_json::from_value(info.clone()).map_err(|e| e.to_string())?;
    Ok(bytecrypto::encrypt_string(&info.to_string()))
}
