// 设置持久化：%APPDATA%\TraeIDE-Switcher\settings.json
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppSettings {
    pub auto_switch: bool,
    /// 触发阈值（剩余百分比低于该值视为耗尽）
    pub threshold: f64,
    /// 额度刷新间隔（分钟）
    pub refresh_interval: u64,
    pub auto_start: bool,
    pub close_to_tray: bool,
    pub notifications: bool,
    pub trae_path: String,
    pub trae_cn_path: String,
    /// P1-2 每日定点批量签到（仅 CN 账号）
    #[serde(default)]
    pub daily_checkin: bool,
    /// P1-2 每日定点全量刷新额度
    #[serde(default)]
    pub daily_refresh: bool,
    /// 定时任务执行时刻（本地时间 "HH:MM"）
    #[serde(default = "default_daily_time")]
    pub daily_time: String,
    /// 自动换号策略："order"=按入库顺序轮换（默认） / "remain"=剩余额度最多优先
    #[serde(default = "default_switch_strategy")]
    pub switch_strategy: String,
    /// 自动换号范围："all"=全池（默认） / "pinned"=只用置顶账号参与轮换
    #[serde(default = "default_switch_scope")]
    pub switch_scope: String,
    /// HTTP 代理地址（如 http://127.0.0.1:7890）；空串 = 直连
    #[serde(default)]
    pub http_proxy: String,
    /// 自定义更新源（latest.json 地址）；空串 = 使用内置默认端点
    #[serde(default)]
    pub update_endpoint: String,
}

fn default_daily_time() -> String {
    "09:00".to_string()
}

fn default_switch_strategy() -> String {
    "order".to_string()
}

fn default_switch_scope() -> String {
    "all".to_string()
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            auto_switch: true,
            threshold: 2.0,
            refresh_interval: 5,
            auto_start: false,
            close_to_tray: true,
            notifications: true,
            trae_path: String::new(),
            trae_cn_path: String::new(),
            daily_checkin: false,
            daily_refresh: false,
            daily_time: default_daily_time(),
            switch_strategy: default_switch_strategy(),
            switch_scope: default_switch_scope(),
            http_proxy: String::new(),
            update_endpoint: String::new(),
        }
    }
}

fn settings_path() -> PathBuf {
    crate::core::vault::app_data_dir().join("settings.json")
}

pub fn load() -> AppSettings {
    std::fs::read_to_string(settings_path())
        .ok()
        // 手工编辑（记事本/PowerShell Set-Content）会写入 UTF-8 BOM，
        // 直接 serde 解析会失败并**静默回落到默认配置**（实测踩到：定时任务开了却不执行）
        .map(|s| s.trim_start_matches('\u{feff}').to_string())
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

pub fn save(s: &AppSettings) -> std::io::Result<()> {
    let path = settings_path();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(path, serde_json::to_string_pretty(s).unwrap())
}
