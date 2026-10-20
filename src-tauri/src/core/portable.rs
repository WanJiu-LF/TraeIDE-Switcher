// 可移植账号包：以口令派生密钥（PBKDF2-HMAC-SHA256）重新加密整个账号库，
// 摆脱 DPAPI 的"同机同用户"限制，可在任意 Windows 机器/用户下用同一口令恢复。
// 账号包内含凭据与设备资产，属敏感数据；口令遗失无法找回。
use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Nonce};
use base64::Engine as _;
use rand::RngCore;
use serde_json::json;

const ITER: u32 = 200_000;
const FORMAT: &str = "traeide-switcher-portable";

fn b64e(b: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(b)
}

fn b64d(s: &str) -> Result<Vec<u8>, String> {
    base64::engine::general_purpose::STANDARD
        .decode(s)
        .map_err(|e| format!("base64 解析失败：{e}"))
}

fn derive(passphrase: &str, salt: &[u8], iter: u32) -> [u8; 32] {
    let mut key = [0u8; 32];
    pbkdf2::pbkdf2_hmac::<sha2::Sha256>(passphrase.as_bytes(), salt, iter, &mut key);
    key
}

/// 导出：读取账号库整体，用口令加密后返回账号包 JSON 字符串
pub fn export(passphrase: &str) -> Result<String, String> {
    if passphrase.chars().count() < 6 {
        return Err("口令至少 6 位".into());
    }
    let v = crate::core::vault::load_json()
        .map_err(|e| e.to_string())?
        .ok_or("账号库不存在，暂无可导出内容")?;
    let plain = serde_json::to_vec(&v).map_err(|e| e.to_string())?;
    let mut salt = [0u8; 16];
    let mut nonce = [0u8; 12];
    let mut rng = rand::thread_rng();
    rng.fill_bytes(&mut salt);
    rng.fill_bytes(&mut nonce);
    let key = derive(passphrase, &salt, ITER);
    let cipher = Aes256Gcm::new_from_slice(&key).map_err(|_| "密钥长度不合法".to_string())?;
    let sealed = cipher
        .encrypt(Nonce::from_slice(&nonce), plain.as_ref())
        .map_err(|_| "加密失败".to_string())?;
    serde_json::to_string_pretty(&json!({
        "format": FORMAT,
        "version": 1,
        "kdf": "pbkdf2-hmac-sha256",
        "iterations": ITER,
        "cipher": "aes-256-gcm",
        "salt": b64e(&salt),
        "nonce": b64e(&nonce),
        "data": b64e(&sealed),
    }))
    .map_err(|e| e.to_string())
}

/// 导入：解析账号包并用口令恢复账号库（原库由 save_json 自动轮转为 accounts.bak）
pub fn import(passphrase: &str, envelope: &str) -> Result<usize, String> {
    let env: serde_json::Value =
        serde_json::from_str(envelope.trim()).map_err(|_| "账号包内容不是合法 JSON".to_string())?;
    if env.get("format").and_then(|f| f.as_str()) != Some(FORMAT) {
        return Err("账号包格式不匹配".into());
    }
    // iterations 来自文件，不可全信：夹到合理区间，避免被构造出 0/1 轮的退化 KDF
    let iter = env
        .get("iterations")
        .and_then(|i| i.as_u64())
        .unwrap_or(ITER as u64)
        .clamp(1_000, 5_000_000) as u32;
    let salt = b64d(env.get("salt").and_then(|s| s.as_str()).ok_or("账号包缺少 salt 字段")?)?;
    let nonce = b64d(env.get("nonce").and_then(|s| s.as_str()).ok_or("账号包缺少 nonce 字段")?)?;
    let data = b64d(env.get("data").and_then(|s| s.as_str()).ok_or("账号包缺少 data 字段")?)?;
    let key = derive(passphrase, &salt, iter);
    let cipher = Aes256Gcm::new_from_slice(&key).map_err(|_| "密钥长度不合法".to_string())?;
    let plain = cipher
        .decrypt(Nonce::from_slice(&nonce), data.as_ref())
        .map_err(|_| "口令错误或账号包已损坏".to_string())?;
    let v: serde_json::Value =
        serde_json::from_slice(&plain).map_err(|_| "账号包内容不合法".to_string())?;
    let count = v
        .get("accounts")
        .and_then(|a| a.as_array())
        .ok_or("账号包缺少 accounts 列表")?
        .len();
    crate::core::vault::save_json(&v).map_err(|e| e.to_string())?;
    Ok(count)
}
