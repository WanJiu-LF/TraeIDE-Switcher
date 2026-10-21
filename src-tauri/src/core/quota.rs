// 额度/付费查询客户端（Cloud-IDE-JWT，双版本 host/路径）
use super::paths::IdeKind;
use serde_json::{json, Value};

#[derive(Debug)]
pub enum QuotaError {
    Http(u16, String),
    Transport(String),
    BadResponse(String),
}

impl std::fmt::Display for QuotaError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Http(code, msg) => write!(f, "HTTP {code}: {msg}"),
            Self::Transport(e) => write!(f, "transport: {e}"),
            Self::BadResponse(e) => write!(f, "bad response: {e}"),
        }
    }
}

impl std::error::Error for QuotaError {}

/// account 主机（pay/oauth/refresh 用）：优先读 IDE product.json 的 bootConfig.account.trae
/// 实测表：CN 仅 normal=api.trae.cn；intl normal=grow-normal / SG、US=growsg-normal / USTTP=grow-normal.traeapi.us
pub fn api_host(ide: IdeKind, region: &str) -> String {
    super::paths::config_host(ide, "account", region).unwrap_or_else(|| fallback_account_host(ide, region))
}

/// ug 主机（签到/活动等增长侧接口用）：读 bootConfig.ug.trae。
/// 与 account 表**不同**：intl 的 US 为 growva-normal.trae.ai（旧实现错用 growsg-normal）
pub fn ug_host(ide: IdeKind, region: &str) -> String {
    super::paths::config_host(ide, "ug", region).unwrap_or_else(|| api_host(ide, region))
}

/// product.json 缺失/未检测到安装时的回落值（与实测表一致）
fn fallback_account_host(ide: IdeKind, region: &str) -> String {
    if ide == IdeKind::TraeCn {
        return "https://api.trae.cn".into();
    }
    match region {
        "SG" | "US" => "https://growsg-normal.trae.ai".into(),
        "USTTP" => "https://grow-normal.traeapi.us".into(),
        _ => "https://grow-normal.trae.ai".into(),
    }
}

/// x-app-version 取值：IDE product.json 的 appVersion；探测失败回落已知常量
pub fn app_version(ide: IdeKind) -> String {
    let v = super::paths::IdePaths::detect(ide)
        .map(|p| p.app_version())
        .unwrap_or_default();
    if v.is_empty() {
        match ide {
            IdeKind::Trae => "3.5.91".into(),
            IdeKind::TraeCn => "3.4.3".into(),
        }
    } else {
        v
    }
}

/// 原生设备头（A9）：x-device-brand = 机器型号，x-os-version = os.version()。
/// 注册表读取按进程缓存，避免每次请求重复读。
fn device_headers() -> &'static (String, String) {
    static CACHE: std::sync::OnceLock<(String, String)> = std::sync::OnceLock::new();
    CACHE.get_or_init(|| (super::paths::machine_model(), super::paths::os_version()))
}

/// 通用鉴权 POST（原始响应，不校验 body 内 code；供 CheckLogin 这类
/// 错误信息在 ResponseMetadata 内、且顶层 code 可能非 0 的接口使用）
pub(crate) fn post_raw(
    base: &str,
    path: &str,
    token: &str,
    device_id: &str,
    app_version: &str,
    body: Value,
) -> Result<Value, QuotaError> {
    let (brand, osver) = device_headers();
    let resp = super::http::agent()
        .post(&format!("{base}{path}"))
        .timeout(std::time::Duration::from_secs(12))
        .set("Authorization", &format!("Cloud-IDE-JWT {token}"))
        .set("Content-Type", "application/json")
        .set("x-device-id", device_id)
        .set("x-device-brand", brand)
        .set("x-device-type", "windows")
        .set("x-os-version", osver)
        .set("x-app-version", app_version)
        .send_json(body)
        .map_err(|e| match e {
            ureq::Error::Status(code, resp) => QuotaError::Http(code, resp.status_text().to_string()),
            other => QuotaError::Transport(other.to_string()),
        })?;
    resp.into_json::<Value>()
        .map_err(|e| QuotaError::BadResponse(e.to_string()))
}

/// ug / commercial 系列通用 POST：Cloud-IDE-JWT + 账号级设备头 + 顶层 code 校验
/// x-device-id 必须是该账号的稳定设备槽（服务端按此字符串记"设备已签到/设备绑号上限"）
pub(crate) fn post_authed(
    base: &str,
    path: &str,
    token: &str,
    device_id: &str,
    app_version: &str,
    body: Value,
) -> Result<Value, QuotaError> {
    check_code(post_raw(base, path, token, device_id, app_version, body)?)
}

/// 额度/付费查询客户端（Cloud-IDE-JWT，双版本 host/路径）
pub struct QuotaClient {
    base: String,
    /// ug 系列专用主机（与 account 表不同，见 ug_host）
    ug_base: String,
    pay_path: String,
    usage_path: String,
    is_cn: bool,
    /// x-app-version：取 IDE product.json 的 appVersion，避免硬编码随版本过期
    version: String,
}

impl QuotaClient {
    pub fn for_ide(ide: IdeKind, region: &str) -> Self {
        let version = app_version(ide);
        let base = api_host(ide, region);
        let ug_base = ug_host(ide, region);
        match ide {
            IdeKind::Trae => Self {
                base,
                ug_base,
                pay_path: "/trae/api/v1/pay/ide_user_pay_status".into(),
                usage_path: "/trae/api/v1/pay/ide_user_ent_usage".into(),
                is_cn: false,
                version,
            },
            IdeKind::TraeCn => Self {
                base: api_host(ide, region),
                ug_base: ug_host(ide, region),
                pay_path: "/trae/api/v2/pay/ide_user_pay_status".into(),
                usage_path: "/trae/api/v2/pay/ide_user_ent_usage".into(),
                is_cn: true,
                version,
            },
        }
    }

    fn post(&self, path: &str, token: &str, body: Value) -> Result<Value, QuotaError> {
        let resp = super::http::agent()
            .post(&format!("{}{}", self.base, path))
            // 必须设超时：ureq 默认无超时，链路抖动（连接重置/半开）会挂到操作系统级超时才失败。
            // Request::timeout 覆盖 DNS/连接/读写全程
            .timeout(std::time::Duration::from_secs(12))
            .set("Authorization", &format!("Cloud-IDE-JWT {token}"))
            .set("Content-Type", "application/json")
            .send_json(body)
            .map_err(|e| match e {
                ureq::Error::Status(code, resp) => {
                    QuotaError::Http(code, resp.status_text().to_string())
                }
                other => QuotaError::Transport(other.to_string()),
            })?;
        resp.into_json::<Value>()
            .map_err(|e| QuotaError::BadResponse(e.to_string()))
    }

    pub fn pay_status(&self, token: &str, device_id: &str) -> Result<Value, QuotaError> {
        let body = json!({ "trae_client": "IDE", "device_id": device_id });
        let v = self.post(&self.pay_path, token, body)?;
        check_code(v)
    }

    pub fn ent_usage(&self, token: &str) -> Result<Value, QuotaError> {
        let body = if self.is_cn {
            json!({ "require_usage": true, "req_source": 1 })
        } else {
            json!({ "require_usage": true })
        };
        let v = self.post(&self.usage_path, token, body)?;
        check_code(v)
    }

    /// CN 签到状态（body {"req_source":1} + 设备头）
    /// `device_id`：账号级独立设备身份的 device_id（登录时绑定）；None 回退派生值
    pub fn checkin_status(&self, token: &str, device_id: Option<&str>) -> Result<Value, QuotaError> {
        self.post_ug("/trae/api/v2/ug/checkin_credits/status", token, device_id)
    }

    /// CN 一键签到（body {"req_source":1} + 设备头）
    /// 设备级限一次：x-device-id 必须是该 token 绑定的设备
    pub fn checkin_claim(&self, token: &str, device_id: Option<&str>) -> Result<Value, QuotaError> {
        self.post_ug("/trae/api/v2/ug/checkin_credits/claim", token, device_id)
    }

    /// ug 接口：body {"req_source":1} + 设备头（设备槽优先用账号绑定设备；走 ug 专用主机）
    fn post_ug(&self, path: &str, token: &str, device_id: Option<&str>) -> Result<Value, QuotaError> {
        post_authed(
            &self.ug_base,
            path,
            token,
            device_id.unwrap_or("0"),
            &self.version,
            json!({ "req_source": 1 }),
        )
    }
}

fn check_code(v: Value) -> Result<Value, QuotaError> {
    match v.get("code").and_then(|c| c.as_i64()) {
        Some(0) | None => Ok(v),
        Some(code) => {
            let msg = v.get("message").and_then(|m| m.as_str()).unwrap_or("unknown");
            Err(QuotaError::Http(code as u16, msg.to_string()))
        }
    }
}

/// 单个权益包的额度摘要
/// 国际版：basic/bonus 为基础/赠送额度（is_dollar_usage_billing=true 时以美元计），
///        no_bonus_quota=true 表示该账号无赠送额度（IDE 侧直接不展示该行）；
/// CN 积分制：credits_* 即积分（可小数，-1=无限）
#[derive(Debug, Clone, serde::Serialize)]
pub struct PackQuota {
    pub product_type: i64,
    /// package_source_type：奖励来源枚举（福利/签到/赠送等）
    pub source_type: i64,
    /// 来源名（display_desc，如"用户福利""每日签到""每月登录赠送"）
    pub desc: String,
    pub basic_limit: f64,
    pub basic_used: f64,
    pub bonus_limit: f64,
    pub bonus_used: f64,
    pub credits_limit: f64,
    pub credits_used: f64,
    /// credits_limit == -1（无限）
    pub credits_unlimited: bool,
    /// quota.no_bonus_quota：无赠送额度
    pub no_bonus_quota: bool,
    /// is_dollar_usage_billing：额度以美元计（国际版新计费版本）
    pub is_dollar_billing: bool,
    /// premium_model_fast_request_limit：快速请求额度（-1=无限）
    pub fast_limit: f64,
    /// premium_model_fast_amount / premium_model_fast_request_usage：快速请求已用
    pub fast_used: f64,
    pub fast_unlimited: bool,
    /// entitlement_base_info.end_time（结算周期结束时刻，秒）：额度重置时间基准
    pub end_time: f64,
    pub expire_time: String,
}

pub fn summarize_ent_usage(v: &Value) -> Vec<PackQuota> {
    let dollar_billing = v
        .get("is_dollar_usage_billing")
        .and_then(|b| b.as_bool())
        .unwrap_or(false);
    v.get("user_entitlement_pack_list")
        .and_then(|l| l.as_array())
        .map(|packs| {
            packs
                .iter()
                .filter_map(|p| {
                    let info = p.get("entitlement_base_info")?;
                    let quota = info.get("quota")?;
                    let usage = p.get("usage")?;
                    let credits_limit = num(quota, "credits_limit");
                    let fast_limit = num(quota, "premium_model_fast_request_limit");
                    let fast_used = usage
                        .get("premium_model_fast_amount")
                        .and_then(|x| x.as_f64())
                        .unwrap_or_else(|| num(usage, "premium_model_fast_request_usage"));
                    let source_type = info
                        .pointer("/product_extra/package_extra/package_source_type")
                        .and_then(|t| t.as_i64())
                        .unwrap_or(-1);
                    Some(PackQuota {
                        product_type: info.get("product_type").and_then(|t| t.as_i64()).unwrap_or(-1),
                        source_type,
                        desc: p
                            .get("display_desc")
                            .and_then(|d| d.as_str())
                            .unwrap_or_default()
                            .to_string(),
                        basic_limit: num(quota, "basic_usage_limit"),
                        basic_used: num(usage, "basic_usage_amount"),
                        bonus_limit: num(quota, "bonus_usage_limit"),
                        bonus_used: num(usage, "bonus_usage_amount"),
                        credits_limit,
                        credits_used: num(usage, "credits_amount"),
                        credits_unlimited: credits_limit == -1.0,
                        no_bonus_quota: quota
                            .get("no_bonus_quota")
                            .and_then(|b| b.as_bool())
                            .unwrap_or(false),
                        is_dollar_billing: dollar_billing,
                        fast_limit,
                        fast_used,
                        fast_unlimited: fast_limit == -1.0,
                        end_time: num(info, "end_time"),
                        expire_time: p
                            .get("expire_time")
                            .and_then(|t| t.as_str())
                            .or_else(|| info.get("end_time").and_then(|t| t.as_str()))
                            .unwrap_or_default()
                            .to_string(),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

fn num(v: &Value, key: &str) -> f64 {
    v.get(key)
        .and_then(|x| x.as_f64())
        .unwrap_or(0.0)
}

/// 单账号"剩余量"统一口径：CN 用积分（credits），国际版用额度总量（基础 + 赠送）。
/// 单位不同但都用于聚合与趋势采样，关键是**两版本都要有值** ——
/// 国际版 credits_limit 恒为 0，只取 credits 会把国际版剩余一直记成 0（实测踩到）。
pub fn remain_of(packs: &[PackQuota]) -> f64 {
    let credit_based = packs
        .iter()
        .any(|p| p.credits_limit != 0.0 || p.credits_used != 0.0);
    if credit_based {
        return packs
            .iter()
            .filter(|p| !p.credits_unlimited)
            .map(|p| (p.credits_limit - p.credits_used).max(0.0))
            .sum();
    }
    // 国际版：基础额度 + 赠送额度（no_bonus_quota 时赠送为 0，不影响和值）
    packs
        .iter()
        .map(|p| {
            (p.basic_limit - p.basic_used).max(0.0) + (p.bonus_limit - p.bonus_used).max(0.0)
        })
        .sum()
}
