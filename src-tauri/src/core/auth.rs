// iCubeAuthInfo 加密串的解析（取 token/身份信息供额度查询与展示）
use super::bytecrypto;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AuthAccount {
    #[serde(default)]
    pub email: String,
    #[serde(default)]
    pub username: String,
    #[serde(default)]
    pub scope: String,
    #[serde(default, rename = "userTag")]
    pub user_tag: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserRegion {
    #[serde(default, rename = "_aiRegion")]
    pub ai_region: String,
    #[serde(default)]
    pub region: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthInfo {
    #[serde(default)]
    pub token: String,
    #[serde(default)]
    pub host: String,
    #[serde(default, rename = "refreshToken")]
    pub refresh_token: String,
    #[serde(default, rename = "userId")]
    pub user_id: String,
    #[serde(default, rename = "expiredAt")]
    pub expired_at: String,
    #[serde(default, rename = "refreshExpiredAt")]
    pub refresh_expired_at: String,
    #[serde(default, rename = "userRegion")]
    pub user_region: Option<UserRegion>,
    #[serde(default)]
    pub account: Option<AuthAccount>,
}

#[derive(Debug)]
pub enum AuthError {
    Crypto(String),
    Json(String),
    /// 粘贴的是 usertag，不是登录凭据
    WrongKeyUsertag,
    /// 粘贴的是 icube-dc 设备密钥
    WrongKeyDevice,
    /// 密文能解，但没有 token/userId
    NotLoginToken,
    /// 文本里找不到登录密文
    NoBlob,
}

impl std::fmt::Display for AuthError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Crypto(e) => write!(f, "auth blob decrypt: {e}"),
            Self::Json(e) => write!(f, "auth json: {e}"),
            Self::WrongKeyUsertag => write!(
                f,
                "这是区域标签（iCubeAuthInfo://usertag），不是登录 Token。请复制键 iCubeAuthInfo://icube.cloudide 的值"
            ),
            Self::WrongKeyDevice => write!(
                f,
                "这是设备密钥（iCubeAuthInfo://icube-dc），不是登录 Token。请复制键 iCubeAuthInfo://icube.cloudide 的值"
            ),
            Self::NotLoginToken => write!(
                f,
                "密文已解密，但不是登录凭据。请从 storage.json 复制 iCubeAuthInfo://icube.cloudide 的值"
            ),
            Self::NoBlob => write!(
                f,
                "没有识别到登录 Token。请复制 iCubeAuthInfo://icube.cloudide 的值（不要键名，也不要 usertag / icube-dc）"
            ),
        }
    }
}

impl std::error::Error for AuthError {}

const AUTH_STORAGE_KEY: &str = "iCubeAuthInfo://icube.cloudide";
const MIN_BLOB_LEN: usize = 80;

#[derive(Debug, PartialEq, Eq)]
enum BlobClass {
    Login,
    Usertag,
    Device,
    Other,
}

fn is_b64_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '+' | '/' | '=' | '-' | '_')
}

fn normalize_b64(s: &str) -> String {
    s.replace("\\/", "/")
        .chars()
        .filter(|c| is_b64_char(*c))
        .collect::<String>()
        .replace('-', "+")
        .replace('_', "/")
}

fn decrypt_pasted(b64: &str) -> Result<String, AuthError> {
    let s = normalize_b64(b64);
    if s.len() < MIN_BLOB_LEN {
        return Err(AuthError::NoBlob);
    }
    super::bytecrypto::decrypt_string(&s).map_err(|e| AuthError::Crypto(e.to_string()))
}

fn is_login_json(v: &serde_json::Value) -> bool {
    let token = v.get("token").and_then(|x| x.as_str()).unwrap_or("");
    let uid = v.get("userId").and_then(|x| x.as_str()).unwrap_or("");
    !token.is_empty() && !uid.is_empty()
}

fn is_usertag_json(v: &serde_json::Value) -> bool {
    let Some(obj) = v.as_object() else { return false };
    if obj.is_empty() || v.get("token").is_some() || v.get("userId").is_some() {
        return false;
    }
    obj.values().all(|x| {
        matches!(
            x.as_str().map(|s| s.to_ascii_lowercase()).as_deref(),
            Some("cn" | "row" | "sg" | "us" | "hk")
        )
    })
}

fn is_device_json(v: &serde_json::Value) -> bool {
    v.get("publicKeyPEM").is_some() || v.get("privateKeyPEM").is_some()
}

fn classify_blob(blob_b64: &str) -> Result<BlobClass, AuthError> {
    let plain = decrypt_pasted(blob_b64)?;
    let v: serde_json::Value =
        serde_json::from_str(&plain).map_err(|e| AuthError::Json(e.to_string()))?;
    Ok(if is_login_json(&v) {
        BlobClass::Login
    } else if is_usertag_json(&v) {
        BlobClass::Usertag
    } else if is_device_json(&v) {
        BlobClass::Device
    } else {
        BlobClass::Other
    })
}

fn extract_cloudide_json(s: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(s).ok()?;
    v.get(AUTH_STORAGE_KEY)?.as_str().map(String::from)
}

fn extract_cloudide_text(s: &str) -> Option<String> {
    let pos = s.find(AUTH_STORAGE_KEY)?;
    let rest = s[pos + AUTH_STORAGE_KEY.len()..].trim_start_matches(|c: char| {
        c == '"' || c == '\'' || c.is_whitespace() || c == ':' || c == '='
    });
    let rest = rest.trim_start_matches(['"', '\'']);
    let end = rest
        .find(|c: char| c == '"' || c == '\'' || c == ',' || c == '}' || c.is_whitespace())
        .unwrap_or(rest.len());
    let blob = rest[..end].trim();
    (blob.len() >= MIN_BLOB_LEN).then(|| blob.to_string())
}

fn b64_runs(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    for c in s.chars() {
        if is_b64_char(c) {
            cur.push(c);
        } else if !cur.is_empty() {
            if cur.len() >= MIN_BLOB_LEN {
                out.push(std::mem::take(&mut cur));
            } else {
                cur.clear();
            }
        }
    }
    if cur.len() >= MIN_BLOB_LEN {
        out.push(cur);
    }
    out
}

fn maybe_collapsed_blob(s: &str) -> Option<String> {
    let trimmed = s.trim();
    let other = trimmed
        .chars()
        .filter(|c| !is_b64_char(*c) && !c.is_whitespace())
        .count();
    if other > 2 {
        return None;
    }
    let collapsed = normalize_b64(trimmed);
    (collapsed.len() >= MIN_BLOB_LEN).then_some(collapsed)
}

fn candidate_blobs(input: &str) -> Vec<String> {
    let unescaped = input.replace("\\/", "/");
    let mut out = Vec::new();
    let mut push = |s: String| {
        let n = normalize_b64(&s);
        if n.len() >= MIN_BLOB_LEN && !out.contains(&n) {
            out.push(n);
        }
    };
    if let Some(b) = extract_cloudide_json(&unescaped) {
        push(b);
    }
    if let Some(b) = extract_cloudide_text(&unescaped) {
        push(b);
    }
    if let Some(b) = maybe_collapsed_blob(&unescaped) {
        push(b);
    }
    for run in b64_runs(&unescaped) {
        push(run);
    }
    out
}

/// 从粘贴文本抽出登录密文：支持 storage.json 整文件、键值行、裸密文、换行折断的 base64。
/// usertag / icube-dc 会给出明确错误，避免当成 Token 入库后切到登录页。
pub fn normalize_pasted_auth(input: &str) -> Result<String, AuthError> {
    let input = input.trim();
    if input.is_empty() {
        return Err(AuthError::NoBlob);
    }
    if input.len() >= 11 && input[..11].eq_ignore_ascii_case("TRAEBUNDLE:") {
        return Ok(input.to_string());
    }
    let mut seen_usertag = false;
    let mut seen_device = false;
    let mut seen_other = false;
    let mut last_crypto = None;
    for cand in candidate_blobs(input) {
        match classify_blob(&cand) {
            Ok(BlobClass::Login) => return Ok(cand),
            Ok(BlobClass::Usertag) => seen_usertag = true,
            Ok(BlobClass::Device) => seen_device = true,
            Ok(BlobClass::Other) => seen_other = true,
            Err(AuthError::Crypto(e)) => last_crypto = Some(e),
            Err(AuthError::Json(_)) => seen_other = true,
            Err(e) => return Err(e),
        }
    }
    if seen_usertag && !input.contains("cloudide") {
        return Err(AuthError::WrongKeyUsertag);
    }
    if seen_device && !input.contains("cloudide") {
        return Err(AuthError::WrongKeyDevice);
    }
    if seen_usertag || seen_device || seen_other {
        return Err(AuthError::NotLoginToken);
    }
    if input.contains("iCubeAuthInfo://usertag") && !input.contains(AUTH_STORAGE_KEY) {
        return Err(AuthError::WrongKeyUsertag);
    }
    if input.contains("iCubeAuthInfo://icube-dc") && !input.contains(AUTH_STORAGE_KEY) {
        return Err(AuthError::WrongKeyDevice);
    }
    if let Some(e) = last_crypto {
        return Err(AuthError::Crypto(e));
    }
    Err(AuthError::NoBlob)
}

/// 解析 `iCubeAuthInfo://<authProviderId>` 加密串
pub fn parse_auth_blob(blob_b64: &str) -> Result<AuthInfo, AuthError> {
    let plain =
        bytecrypto::decrypt_string(blob_b64).map_err(|e| AuthError::Crypto(e.to_string()))?;
    let info: AuthInfo = serde_json::from_str(&plain).map_err(|e| AuthError::Json(e.to_string()))?;
    Ok(info)
}

/// 凭据归属判定：只认密文解密后的语义字段，不对 base64 密文做文本匹配
/// （密文是随机字节，`/cn|china|volc|bytedance/` 这类猜测会随机误判）。
/// 依据：host 域名、userTag、userRegion 区域三者任一指向国内即 CN，否则按国际版。
pub fn detect_ide(blob_b64: &str) -> Result<super::paths::IdeKind, AuthError> {
    let info = parse_auth_blob(blob_b64)?;
    let host = info.host.to_lowercase();
    let cn_host = host.contains("trae.cn") || host.contains("trae.com.cn");
    let tag = info
        .account
        .as_ref()
        .map(|a| a.user_tag.trim().to_lowercase())
        .unwrap_or_default();
    let region = info
        .user_region
        .as_ref()
        .map(|r| {
            format!(
                "{} {}",
                r.region.trim().to_lowercase(),
                r.ai_region.trim().to_lowercase()
            )
        })
        .unwrap_or_default();
    let cn_region = region.split_whitespace().any(|x| x == "cn");
    Ok(if cn_host || tag == "cn" || cn_region {
        super::paths::IdeKind::TraeCn
    } else {
        super::paths::IdeKind::Trae
    })
}

/// 凭据修复：外部提取/旧版的 blob account 常缺 storeRegion/userTag 等登录必需字段，
/// Trae 启动校验（storeCountryCode/storeRegion/userTag 任一为空即 TokenInvalid）会判无效，
/// 表现为切换后停在登录页。在原始 JSON 层按 IDE 模板补齐缺失字段（完整凭据原样返回，
/// 不经 AuthInfo 结构体重建——那会丢弃未建模字段）。
pub fn repair_auth_blob(ide: super::paths::IdeKind, blob_b64: &str) -> Result<String, AuthError> {
    let plain =
        bytecrypto::decrypt_string(blob_b64).map_err(|e| AuthError::Crypto(e.to_string()))?;
    let mut v: serde_json::Value =
        serde_json::from_str(&plain).map_err(|e| AuthError::Json(e.to_string()))?;
    let Some(acct) = v.get_mut("account").and_then(|a| a.as_object_mut()) else {
        return Ok(blob_b64.to_string());
    };
    let (tag, region): (&str, &str) = match ide {
        super::paths::IdeKind::Trae => ("row", "SG"),
        super::paths::IdeKind::TraeCn => ("cn", "CN"),
    };
    let non_empty = |k: &str| {
        acct.get(k)
            .and_then(|x| x.as_str())
            .map(|s| !s.is_empty())
            .unwrap_or(false)
    };
    let mut fills: Vec<(&str, serde_json::Value)> = Vec::new();
    if !non_empty("userTag") {
        fills.push(("userTag", tag.into()));
    }
    if !non_empty("storeRegion") {
        fills.push(("storeRegion", region.into()));
    }
    if !non_empty("scope") {
        fills.push(("scope", "marscode".into()));
    }
    if !non_empty("loginScope") {
        fills.push(("loginScope", "trae".into()));
    }
    // intl 账号校验链要求 storeCountryCode 非空（CN 版 marscode 分支无此要求）
    if ide == super::paths::IdeKind::Trae && !non_empty("storeCountryCode") {
        fills.push(("storeCountryCode", "hk".into()));
        if !non_empty("storeCountrySrc") {
            fills.push(("storeCountrySrc", "uid".into()));
        }
    }
    if fills.is_empty() {
        return Ok(blob_b64.to_string());
    }
    for (k, val) in fills {
        acct.insert(k.into(), val);
    }
    let repaired = serde_json::to_string(&v).map_err(|e| AuthError::Json(e.to_string()))?;
    Ok(bytecrypto::encrypt_string(&repaired))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn login_json() -> &'static str {
        r#"{"token":"t-abc","refreshToken":"r","userId":"u1","host":"https://www.trae.ai","account":{"email":"a@b.com","username":"alice","userTag":"row"}}"#
    }

    fn enc(plain: &str) -> String {
        bytecrypto::encrypt_string(plain)
    }

    #[test]
    fn extracts_login_from_storage_json() {
        let login = enc(login_json());
        let sj = serde_json::json!({
            "iCubeAuthInfo://usertag": enc(r#"{"u1":"row"}"#),
            "iCubeAuthInfo://icube-dc:2644743167423884": enc(r#"{"publicKeyPEM":"k","privateKeyPEM":"p"}"#),
            "iCubeAuthInfo://icube.cloudide": login,
        });
        let blob = normalize_pasted_auth(&sj.to_string()).unwrap();
        let info = parse_auth_blob(&blob).unwrap();
        assert_eq!(info.user_id, "u1");
        assert_eq!(info.token, "t-abc");
    }

    #[test]
    fn extracts_login_from_json_line() {
        let login = enc(login_json());
        let line = format!("    \"iCubeAuthInfo://icube.cloudide\": \"{login}\",");
        let info = parse_auth_blob(&normalize_pasted_auth(&line).unwrap()).unwrap();
        assert_eq!(info.user_id, "u1");
    }

    #[test]
    fn accepts_raw_login_blob() {
        let login = enc(login_json());
        let info = parse_auth_blob(&normalize_pasted_auth(&login).unwrap()).unwrap();
        assert_eq!(info.token, "t-abc");
    }

    #[test]
    fn accepts_wrapped_base64() {
        let login = enc(login_json());
        let mid = login.len() / 2;
        let wrapped = format!("{}\n{}", &login[..mid], &login[mid..]);
        let info = parse_auth_blob(&normalize_pasted_auth(&wrapped).unwrap()).unwrap();
        assert_eq!(info.user_id, "u1");
    }

    #[test]
    fn rejects_usertag_value() {
        let e = normalize_pasted_auth(&enc(r#"{"u1":"cn"}"#)).unwrap_err();
        assert!(matches!(e, AuthError::WrongKeyUsertag), "{e}");
    }

    #[test]
    fn rejects_device_value() {
        let e = normalize_pasted_auth(&enc(r#"{"publicKeyPEM":"k","privateKeyPEM":"p"}"#)).unwrap_err();
        assert!(matches!(e, AuthError::WrongKeyDevice), "{e}");
    }

    #[test]
    fn rejects_key_name_only() {
        let e = normalize_pasted_auth("iCubeAuthInfo://icube.cloudide").unwrap_err();
        assert!(matches!(e, AuthError::NoBlob), "{e}");
    }

    #[test]
    fn detects_cn_from_extracted_blob() {
        let login = enc(
            r#"{"token":"t","refreshToken":"r","userId":"u2","host":"https://www.trae.cn","account":{"email":"c@d.com","userTag":"cn"}}"#,
        );
        let line = format!("\"iCubeAuthInfo://icube.cloudide\": \"{login}\"");
        let blob = normalize_pasted_auth(&line).unwrap();
        assert_eq!(detect_ide(&blob).unwrap(), crate::core::paths::IdeKind::TraeCn);
    }

    #[test]
    fn extracts_from_local_ide_storage_if_present() {
        let Some(appdata) = std::env::var_os("APPDATA") else { return };
        for name in ["Trae CN", "Trae"] {
            let path = std::path::PathBuf::from(&appdata)
                .join(name)
                .join("User")
                .join("globalStorage")
                .join("storage.json");
            if !path.exists() {
                continue;
            }
            let raw = std::fs::read_to_string(&path).expect("read storage.json");
            if !raw.contains("iCubeAuthInfo://icube.cloudide") {
                continue;
            }
            let blob = normalize_pasted_auth(&raw).expect("extract login from local storage.json");
            let info = parse_auth_blob(&blob).expect("parse extracted login");
            assert!(!info.user_id.is_empty(), "{name}: empty userId");
            assert!(!info.token.is_empty(), "{name}: empty token");

            let v: serde_json::Value = serde_json::from_str(&raw).expect("storage.json");
            if let Some(tag) = v.get("iCubeAuthInfo://usertag").and_then(|x| x.as_str()) {
                assert!(
                    matches!(normalize_pasted_auth(tag), Err(AuthError::WrongKeyUsertag)),
                    "{name}: usertag value should be rejected"
                );
            }
            if let Some(obj) = v.as_object() {
                if let Some((_, dc)) = obj.iter().find(|(k, _)| k.starts_with("iCubeAuthInfo://icube-dc:")) {
                    if let Some(dc) = dc.as_str() {
                        assert!(
                            matches!(normalize_pasted_auth(dc), Err(AuthError::WrongKeyDevice)),
                            "{name}: icube-dc value should be rejected"
                        );
                    }
                }
            }
        }
    }
}
