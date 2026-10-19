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
}

impl std::fmt::Display for AuthError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Crypto(e) => write!(f, "auth blob decrypt: {e}"),
            Self::Json(e) => write!(f, "auth json: {e}"),
        }
    }
}

impl std::error::Error for AuthError {}

/// 解析 `iCubeAuthInfo://<authProviderId>` 加密串
pub fn parse_auth_blob(blob_b64: &str) -> Result<AuthInfo, AuthError> {
    let plain =
        bytecrypto::decrypt_string(blob_b64).map_err(|e| AuthError::Crypto(e.to_string()))?;
    let info: AuthInfo = serde_json::from_str(&plain).map_err(|e| AuthError::Json(e.to_string()))?;
    Ok(info)
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
