// 账号资产包：一个账号随身携带的全部凭据/身份数据
use super::paths::IdeKind;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AccountStatus {
    Normal,
    Exhausted,
    Error,
}

/// 账号资产包（凭据以 Trae 原始加密串搬运，不解构 token 明文）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccountBundle {
    pub id: String,
    pub name: String,
    pub email: String,
    pub note: String,
    pub ide: IdeKind,
    pub status: AccountStatus,
    /// iCubeAuthInfo://<authProviderId> 的加密串原文（bytecrypto base64）
    pub auth_blob: String,
    /// iCubeAuthInfo://usertag 的加密串原文（可选）
    pub usertag_blob: Option<String>,
    /// 采集该账号时的 userId（用于 usertag/icube-dc 键名）
    pub user_id: String,
    /// AHA ECDSA 设备密钥对：storage.json 键（iCubeAuthInfo://icube-dc:*）→ 加密串原文
    /// 随号保存恢复，防止重生成触发服务端风控 4015
    #[serde(default)]
    pub ecdsa_blobs: BTreeMap<String, String>,
    /// aha/TinyStorage 的 tiny_storage_data 完整快照（加密串原文，随号独立）
    /// 防止"该设备绑定的账户数量已达上限"——每号独立 aha.device.device_id
    #[serde(default)]
    pub tiny_storage: Option<serde_json::Value>,
    /// 服务端侧设备指纹（telemetry.devDeviceId / sqmId，随号）
    /// （本机 machineId 三处仍保留原值，见 machine_id 注释）
    #[serde(default)]
    pub dev_device_id: Option<String>,
    #[serde(default)]
    pub sqm_id: Option<String>,
    /// 该账号注册/采集时的 MachineID（sha256(machineid 文件)），续期 DeviceInfo 用。
    /// 不下发到本机三处遥测，避免破坏本机 SQLCipher 密钥来源。
    #[serde(default)]
    pub machine_id: Option<String>,
    /// 采集/签发时服务端收到的完整 DeviceInfo（原样重放，防止字段漂移触发风控）
    #[serde(default)]
    pub device_info: Option<serde_json::Value>,
    /// 置顶显示（账号池排序用，与凭据无关）
    #[serde(default)]
    pub pinned: bool,
    /// 该账号独立耗尽阈值（剩余百分比），None = 用全局 settings.threshold
    #[serde(default)]
    pub threshold: Option<f64>,
    /// 上次凭据续期（ExchangeToken）失败的原始原因；续期成功时清空。
    /// 用于把 20403「Token device not match」这类静默降级暴露到界面上。
    #[serde(default)]
    pub refresh_error: Option<String>,
}

impl AccountBundle {
    /// 从 auth_blob 解出的展示信息（email/username 等）供 UI 使用
    pub fn auth_provider_key() -> String {
        "iCubeAuthInfo://icube.cloudide".to_string()
    }
}
