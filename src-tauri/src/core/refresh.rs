// ExchangeToken 续期：以 refreshToken 换新 token（DeviceProof = ECDSA P-256(SHA256) 签名）
// 规格来源：main.js E_e()/j()/exchangeTokenByRefreshToken() 实测提取
use super::bytecrypto;
use super::paths::{IdeKind, IdePaths};
use super::quota::api_host;
use super::store::{StorageJson, TinyStorage};
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine as _;
use p256::ecdsa::{signature::Signer, Signature, SigningKey};
use p256::pkcs8::DecodePrivateKey;
use serde::Serialize;
use serde_json::{json, Value};

const EXCHANGE_PATH: &str = "/trae/api/v3/oauth/ExchangeToken";
pub(crate) const CLIENT_ID: &str = "ono9krqynydwx5";
const PLATFORM_CODE: &str = "IDE_PC";

#[derive(Debug)]
pub enum RefreshError {
    Missing(&'static str),
    Sign(String),
    Http(u16, String),
    Transport(String),
    Response(String),
}

impl std::fmt::Display for RefreshError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Missing(what) => write!(f, "missing {what}"),
            Self::Sign(e) => write!(f, "sign: {e}"),
            Self::Http(code, msg) => write!(f, "HTTP {code}: {msg}"),
            Self::Transport(e) => write!(f, "transport: {e}"),
            Self::Response(e) => write!(f, "response: {e}"),
        }
    }
}

impl std::error::Error for RefreshError {}

#[derive(Serialize)]
pub struct DeviceInfo {
    #[serde(rename = "DeviceID")]
    pub device_id: String,
    #[serde(rename = "MachineID")]
    pub machine_id: String,
    #[serde(rename = "PlatformCode")]
    pub platform_code: &'static str,
    #[serde(rename = "DeviceType")]
    pub device_type: &'static str,
    #[serde(rename = "DeviceName")]
    pub device_name: String,
    #[serde(rename = "DeviceModel")]
    pub device_model: String,
    #[serde(rename = "DeviceBrand")]
    pub device_brand: String,
    #[serde(rename = "DeviceCPU")]
    pub device_cpu: String,
    #[serde(rename = "OSInfo")]
    pub os_info: &'static str,
    #[serde(rename = "OSVersion")]
    pub os_version: String,
    #[serde(rename = "ClientVersion")]
    pub client_version: String,
    #[serde(rename = "DevicePublicKey")]
    pub device_public_key: String,
}

#[derive(Serialize)]
struct DeviceProof {
    #[serde(rename = "Signature")]
    signature: String,
    #[serde(rename = "Timestamp")]
    timestamp: u64,
    #[serde(rename = "Nonce")]
    nonce: String,
}

#[derive(Serialize)]
struct ExchangeRequest {
    #[serde(rename = "ClientID")]
    client_id: &'static str,
    #[serde(rename = "ClientSecret")]
    client_secret: &'static str,
    #[serde(rename = "RefreshToken")]
    refresh_token: String,
    #[serde(rename = "DeviceInfo")]
    device_info: DeviceInfo,
    #[serde(rename = "DeviceProof")]
    device_proof: DeviceProof,
    #[serde(rename = "IDEVersion")]
    ide_version: String,
}

/// DeviceProof 载荷：POST\n<path>\n<ClientID>\n<RefreshToken>\n<秒时间戳>\n<32hex nonce>
fn proof_payload(refresh_token: &str, ts: u64, nonce: &str) -> String {
    format!("POST\n{EXCHANGE_PATH}\n{CLIENT_ID}\n{refresh_token}\n{ts}\n{nonce}")
}

fn sign_proof(private_pem: &str, refresh_token: &str) -> Result<DeviceProof, RefreshError> {
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let nonce: String = {
        let mut bytes = [0u8; 16];
        rand::RngCore::fill_bytes(&mut rand::thread_rng(), &mut bytes);
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    };
    let msg = proof_payload(refresh_token, ts, &nonce);
    let key = SigningKey::from_pkcs8_pem(private_pem).map_err(|e| RefreshError::Sign(e.to_string()))?;
    let sig: Signature = key.sign(msg.as_bytes());
    Ok(DeviceProof {
        signature: B64.encode(sig.to_der().as_bytes()),
        timestamp: ts,
        nonce,
    })
}

/// 本机 aha 注册设备 ID（同机共享身份策略：所有账号共用本机设备，签到/续期用）
pub fn local_device_id(ide: &IdeKind) -> Option<String> {
    device_identity(ide).ok().map(|(did, ..)| did)
}

/// 组装 DeviceInfo（登录/续期共用，同机共享身份：真实本机设备指纹）
pub fn build_device_info(ide: &IdeKind) -> Result<DeviceInfo, RefreshError> {
    let (device_id, machine_id, _private, public_pem) = device_identity(ide)?;
    Ok(DeviceInfo {
        device_id,
        machine_id,
        platform_code: PLATFORM_CODE,
        device_type: "PC",
        device_name: format!("{}的电脑", env_or("USERNAME", "PC")),
        device_model: String::new(),
        device_brand: String::new(),
        device_cpu: env_or("PROCESSOR_IDENTIFIER", ""),
        os_info: "windows",
        os_version: "Windows 11".to_string(),
        client_version: client_version(ide).to_string(),
        device_public_key: public_pem,
    })
}

/// 登录 URL 所需的设备标识（device_id/machine_id）
pub(crate) fn device_ids(ide: &IdeKind) -> Result<(String, String, String), RefreshError> {
    let (device_id, machine_id, private_pem, _) = device_identity(ide)?;
    Ok((device_id, machine_id, private_pem))
}

fn device_identity(ide: &IdeKind) -> Result<(String, String, String, String), RefreshError> {
    let paths = IdePaths::detect(*ide).ok_or(RefreshError::Missing("IDE install"))?;
    // DeviceID：TinyStorage 的 aha device_id_str（= icube-dc:<did> 键名后缀）
    let device_id = TinyStorage::load(&paths)
        .ok()
        .and_then(|ts| ts.get_decrypted("aha.device.device_id"))
        .and_then(|p| serde_json::from_str::<Value>(&p).ok())
        .and_then(|v| v.get("device_id_str").and_then(|s| s.as_str()).map(String::from))
        .ok_or(RefreshError::Missing("device_id_str"))?;
    let sj = StorageJson::load(&paths).map_err(|_| RefreshError::Missing("storage.json"))?;
    let machine_id = sj
        .get("telemetry.machineId")
        .and_then(|v| v.as_str())
        .map(String::from)
        .ok_or(RefreshError::Missing("telemetry.machineId"))?;
    let kp_key = format!("iCubeAuthInfo://icube-dc:{device_id}");
    let kp_blob = sj
        .get(&kp_key)
        .and_then(|v| v.as_str())
        .ok_or(RefreshError::Missing("device keypair"))?;
    let kp: Value = serde_json::from_str(&bytecrypto::decrypt_string(kp_blob).map_err(|e| RefreshError::Sign(e.to_string()))?)
        .map_err(|e| RefreshError::Response(e.to_string()))?;
    let private_pem = kp
        .get("privateKeyPEM")
        .and_then(|v| v.as_str())
        .ok_or(RefreshError::Missing("privateKeyPEM"))?
        .to_string();
    let public_pem = kp
        .get("publicKeyPEM")
        .and_then(|v| v.as_str())
        .map(String::from)
        .unwrap_or_default();
    Ok((device_id, machine_id, private_pem, public_pem))
}

fn env_or(key: &str, fallback: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| fallback.to_string())
}

/// 客户端版本：唯一来源为 IDE product.json 的 appVersion（硬编码常量会随升级过期，
/// 实测 CN 已从 3.3.100 升至 3.4.3 / 国际 3.5.91）
pub(crate) fn client_version(ide: &IdeKind) -> String {
    super::quota::app_version(*ide)
}

/// 用 refreshToken 换新 token，返回更新后的 auth blob（加密串）
/// 同机共享身份：用本机 aha 真实设备密钥对签名（换设备会 20403 Token device not match）
pub fn refresh_auth_blob(ide: &IdeKind, auth_blob_b64: &str) -> Result<String, RefreshError> {
    // 原始 JSON 层读写：经 AuthInfo 结构体重建会丢弃未建模字段（host / tokenReleaseAt /
    // account 的 storeRegion、avatar_url 等），续期一次就把完整凭据降级成残缺形态。
    // 这里只改 token 相关键，其余原样保留。
    let plain = bytecrypto::decrypt_string(auth_blob_b64)
        .map_err(|e| RefreshError::Response(e.to_string()))?;
    let mut raw: Value =
        serde_json::from_str(&plain).map_err(|e| RefreshError::Response(e.to_string()))?;
    let old_token = raw.get("token").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let old_refresh = raw.get("refreshToken").and_then(|v| v.as_str()).unwrap_or("").to_string();
    if old_refresh.is_empty() {
        return Err(RefreshError::Missing("refreshToken"));
    }
    let region = raw
        .pointer("/userRegion/region")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let (device_id, machine_id, private_pem, public_pem) = device_identity(ide)?;

    let device_info = DeviceInfo {
        device_id,
        machine_id,
        platform_code: PLATFORM_CODE,
        device_type: "PC",
        device_name: format!("{}的电脑", env_or("USERNAME", "PC")),
        device_model: String::new(),
        device_brand: String::new(),
        device_cpu: env_or("PROCESSOR_IDENTIFIER", ""),
        os_info: "windows",
        os_version: "Windows 11".to_string(),
        client_version: client_version(ide).to_string(),
        device_public_key: public_pem,
    };
    let proof = sign_proof(&private_pem, &old_refresh)?;
    let body = ExchangeRequest {
        client_id: CLIENT_ID,
        client_secret: "",
        refresh_token: old_refresh.clone(),
        device_info,
        device_proof: proof,
        ide_version: client_version(ide).to_string(),
    };

    let url = format!("{}/{}", api_host(*ide, &region), EXCHANGE_PATH);
    let resp = ureq::post(&url)
        // 续期是额度查询失败后的隐式阻塞点：无超时会整链挂死在前一步
        .timeout(std::time::Duration::from_secs(12))
        .set("Content-Type", "application/json")
        .set("x-cloudide-token", &old_token)
        .send_json(serde_json::to_value(&body).unwrap())
        .map_err(|e| match e {
            ureq::Error::Status(code, r) => {
                let body = r.into_string().unwrap_or_default();
                RefreshError::Http(code, body)
            }
            other => RefreshError::Transport(other.to_string()),
        })?;
    let v: Value = resp.into_json().map_err(|e| RefreshError::Response(e.to_string()))?;

    if let Some(code) = v.pointer("/ResponseMetadata/Error/Code").and_then(|c| c.as_str()) {
        return Err(RefreshError::Http(0, format!("token invalid: {code}")));
    }
    let result = v.get("Result").ok_or(RefreshError::Response("no Result".into()))?;

    // 合并新凭据字段（只覆盖 token 相关键，account/host/userRegion 等原样保留）
    if let Some(obj) = raw.as_object_mut() {
        obj.insert("token".into(), result.get("Token").cloned().unwrap_or(Value::Null));
        obj.insert("refreshToken".into(), result.get("RefreshToken").cloned().unwrap_or(Value::Null));
        obj.insert(
            "expiredAt".into(),
            json!(iso_from_expire(result.get("TokenExpireAt"), result.get("TokenExpireDuration"))),
        );
        obj.insert(
            "refreshExpiredAt".into(),
            json!(iso_from_expire(
                result
                    .get("RefreshRefreshExpireAt")
                    .or_else(|| result.get("RefreshTokenExpireAt"))
                    .or_else(|| result.get("RefreshExpireAt")),
                None
            )),
        );
        if let Some(uid) = result.get("UserID").or_else(|| result.get("UserId")) {
            obj.insert("userId".into(), uid.clone());
        }
    }
    Ok(bytecrypto::encrypt_string(&serde_json::to_string(&raw).unwrap()))
}

/// TokenExpireAt（epoch）或 now+TokenExpireDuration -> ISO 字符串
fn iso_from_expire(at: Option<&Value>, duration: Option<&Value>) -> String {
    let millis = at
        .and_then(|v| v.as_i64())
        .map(|t| if t > 1_000_000_000_000 { t } else { t * 1000 })
        .or_else(|| {
            duration
                .and_then(|v| v.as_i64())
                .map(|d| now_millis() + d * 1000)
        })
        .unwrap_or_else(now_millis);
    millis_to_iso(millis)
}

pub(crate) fn now_millis() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
}

pub(crate) fn millis_to_iso(ms: i64) -> String {
    // 无第三方时间库，手工换算为 UTC ISO-8601
    let secs = ms / 1000;
    let days = secs / 86_400;
    let rem = secs % 86_400;
    let (h, m, s) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    let (y, mo, d) = civil_from_days(days);
    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{m:02}:{s:02}.000Z")
}

/// days since 1970-01-01 -> (y, m, d)（Howard Hinnant 算法）
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (yoe + era * 400 + (m <= 2) as i64, m, d)
}
