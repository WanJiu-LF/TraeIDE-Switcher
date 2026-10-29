// Trae / Trae CN 安装目录与用户数据目录解析
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum IdeKind {
    Trae,
    TraeCn,
}

impl IdeKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Trae => "Trae",
            Self::TraeCn => "Trae CN",
        }
    }

    /// %APPDATA% 下的用户数据目录名
    pub fn user_data_dir_name(self) -> &'static str {
        match self {
            Self::Trae => "Trae",
            Self::TraeCn => "Trae CN",
        }
    }
}

pub struct IdePaths {
    pub ide: IdeKind,
    pub install_dir: PathBuf,
    pub user_data: PathBuf,
}

impl IdePaths {
    pub fn detect(ide: IdeKind) -> Option<Self> {
        let install_dir = manual_install_dir(ide).or_else(|| detect_install_dir(ide))?;
        Some(Self {
            ide,
            install_dir,
            user_data: user_data_dir(ide),
        })
    }

    pub fn with_install_dir(ide: IdeKind, install_dir: PathBuf) -> Self {
        Self {
            ide,
            install_dir,
            user_data: user_data_dir(ide),
        }
    }

    /// 存储主战场：globalStorage/storage.json
    pub fn storage_json(&self) -> PathBuf {
        self.user_data.join("User").join("globalStorage").join("storage.json")
    }

    /// SQLite 状态库
    pub fn state_vscdb(&self) -> PathBuf {
        self.user_data.join("User").join("globalStorage").join("state.vscdb")
    }

    /// AHA SDK TinyStorage（JSON，值经 bytecrypto 加密）
    pub fn tiny_storage(&self) -> PathBuf {
        self.user_data.join("aha").join("TinyStorage")
    }

    /// 36 字节 UUID 的 machineid 文件
    pub fn machineid_file(&self) -> PathBuf {
        self.user_data.join("machineid")
    }

    /// AI-Agent SQLCipher 对话库（会话归属挂在 project.user_id 上）
    pub fn agent_database(&self) -> PathBuf {
        self.user_data
            .join("ModularData")
            .join("ai-agent")
            .join("database.db")
    }

    /// 主进程入口（--inspect 注入目标）
    pub fn main_js(&self) -> PathBuf {
        self.install_dir.join("resources").join("app").join("out").join("main.js")
    }

    /// 渲染层工作台入口（_tsw.replaceUserInfo 命令注册目标）
    pub fn workbench_js(&self) -> PathBuf {
        self.install_dir
            .join("resources")
            .join("app")
            .join("out")
            .join("vs")
            .join("workbench")
            .join("workbench.desktop.main.js")
    }

    /// AI 聊天模块（会话/任务列表真正的读写方：project_id 决定会话归属）
    pub fn chat_module_js(&self) -> PathBuf {
        self.install_dir
            .join("resources")
            .join("app")
            .join("node_modules")
            .join("@byted-icube")
            .join("ai-modules-chat")
            .join("dist")
            .join("index.mjs")
    }

    /// resources/app/product.json（appVersion/tronBuildVersion/quality/authConfig 等运行时配置）
    pub fn product_json(&self) -> PathBuf {
        self.install_dir
            .join("resources")
            .join("app")
            .join("product.json")
    }

    /// product.json 内容（读取或解析失败返回空对象）
    pub fn product_config(&self) -> serde_json::Value {
        std::fs::read_to_string(self.product_json())
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_else(|| serde_json::json!({}))
    }

    /// 版本号真实来源：product.json 的 appVersion（CN 3.4.3 / 国际 3.5.91），
    /// 硬编码常量会随 IDE 升级过期（3.3.100 早已不是 CN 现值）
    pub fn app_version(&self) -> String {
        self.product_config()
            .get("appVersion")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string()
    }

    /// 原生登录 URL 的 plugin_version = tronBuildVersion（缺省 "local"）
    pub fn tron_build_version(&self) -> String {
        self.product_config()
            .get("tronBuildVersion")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .unwrap_or("local")
            .to_string()
    }

    /// 原生登录 URL 的 x_app_type = quality（stable/insider）
    pub fn quality(&self) -> String {
        self.product_config()
            .get("quality")
            .and_then(|v| v.as_str())
            .unwrap_or("stable")
            .to_string()
    }

    /// IDE 日志主目录（%APPDATA%\<ide>\logs）
    pub fn logs_dir(&self) -> PathBuf {
        self.user_data.join("logs")
    }

    /// bootConfig.<section>.trae 主机（region 缺失回落 normal）
    pub fn boot_host(&self, section: &str, region: &str) -> Option<String> {
        let cfg = self.product_config();
        let table = cfg.pointer(&format!("/bootConfig/{section}/trae"))?;
        table
            .get(region)
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .or_else(|| table.get("normal").and_then(|v| v.as_str()).filter(|s| !s.is_empty()))
            .map(String::from)
    }

    /// 可执行文件
    pub fn exe(&self) -> PathBuf {
        self.install_dir.join(exe_name(self.ide))
    }
}

fn user_data_dir(ide: IdeKind) -> PathBuf {
    dirs_roaming().join(ide.user_data_dir_name())
}

fn dirs_roaming() -> PathBuf {
    std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\Users\Default\AppData\Roaming"))
}

/// 设置页手填的安装路径优先；仅当该目录下确实存在对应 exe 时才采用（否则回落注册表探测）
fn manual_install_dir(ide: IdeKind) -> Option<PathBuf> {
    let s = crate::core::settings::load();
    let raw = match ide {
        IdeKind::Trae => s.trae_path,
        IdeKind::TraeCn => s.trae_cn_path,
    };
    let raw = raw.trim().trim_matches('"');
    if raw.is_empty() {
        return None;
    }
    let dir = PathBuf::from(raw);
    dir.join(exe_name(ide)).exists().then_some(dir)
}

/// 可执行文件名（探测与校验共用）
fn exe_name(ide: IdeKind) -> &'static str {
    match ide {
        IdeKind::Trae => "Trae.exe",
        IdeKind::TraeCn => "Trae CN.exe",
    }
}

/// 从 HKCU Uninstall 注册表读 InstallLocation
fn detect_install_dir(ide: IdeKind) -> Option<PathBuf> {
    use winreg::enums::HKEY_CURRENT_USER;
    use winreg::RegKey;

    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let uninstall = hkcu
        .open_subkey(r"Software\Microsoft\Windows\CurrentVersion\Uninstall")
        .ok()?;
    for name in uninstall.enum_keys().flatten() {
        let Ok(sub) = uninstall.open_subkey(&name) else { continue };
        let display: String = sub.get_value("DisplayName").unwrap_or_default();
        let publisher: String = sub.get_value("Publisher").unwrap_or_default();
        if !uninstall_entry_matches(ide, &display, &publisher) {
            continue;
        }
        // InstallLocation 可能是带引号的串（实测 `"D:\data\TraeIDE-Switcher"`），
        // 且 Uninstall 条目本身不作安装位置可信来源——必须目录下真有对应 exe 才采用
        let loc = loc_clean(&sub.get_value::<String, _>("InstallLocation").unwrap_or_default());
        if loc.is_empty() {
            continue;
        }
        let dir = PathBuf::from(loc);
        if dir.join(exe_name(ide)).exists() {
            return Some(dir);
        }
    }
    None
}

/// 剥掉 InstallLocation 两层引号/空白（注册表写入方风格不统一）
fn loc_clean(raw: &str) -> String {
    raw.trim().trim_matches('"').trim().to_string()
}

/// Uninstall 条目归属判定。DisplayName 会随版本变（实测 10/16 CN 更新后
/// `TraeCode CN (User)` → `TRAE CN (User)`，精确匹配会漏检整个安装目录），
/// 因此按归一化名称判断，Publisher 作兜底（CN=Beijing Yinli，国际=SPRING）。
fn uninstall_entry_matches(ide: IdeKind, display: &str, publisher: &str) -> bool {
    let d: String = display
        .to_lowercase()
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect();
    if !d.contains("trae") {
        return false;
    }
    // 排除本工具自身（`TraeIDE-Switcher` / `AI IDE Switcher`）：名称含 trae 且不含 cn，
    // 会被下面的国际版规则误判，抓到换号器自己的安装目录
    if d.contains("switcher") {
        return false;
    }
    let p = publisher.to_lowercase();
    let cn_by_publisher = p.contains("yinli") || p.contains("beijing");
    let intl_by_publisher = p.contains("spring") || p.contains("pte");
    let cn_by_name = d.contains("cn");
    match ide {
        IdeKind::TraeCn => cn_by_name || cn_by_publisher,
        IdeKind::Trae => !cn_by_name || intl_by_publisher,
    }
}

/// 机器型号：原生 `x-device-brand` = os.deviceInfo.model，Windows 下等价 BIOS SystemProductName
pub fn machine_model() -> String {
    use winreg::enums::{HKEY_LOCAL_MACHINE, KEY_READ};
    use winreg::RegKey;
    RegKey::predef(HKEY_LOCAL_MACHINE)
        .open_subkey_with_flags(r"HARDWARE\DESCRIPTION\System\BIOS", KEY_READ)
        .and_then(|k| k.get_value::<String, _>("SystemProductName"))
        .unwrap_or_default()
}

/// 系统版本串：等价 node `os.version()`（原生 `x-os-version`）。
/// 注册表 ProductName 在 Win11 升级后不更新（仍写 "Windows 10"），按 CurrentBuildNumber 纠正。
pub fn os_version() -> String {
    use winreg::enums::{HKEY_LOCAL_MACHINE, KEY_READ};
    use winreg::RegKey;
    let key = RegKey::predef(HKEY_LOCAL_MACHINE)
        .open_subkey_with_flags(r"SOFTWARE\Microsoft\Windows NT\CurrentVersion", KEY_READ);
    let Ok(key) = key else {
        return "Windows 11".to_string();
    };
    let product: String = key.get_value("ProductName").unwrap_or_default();
    let build: u32 = key
        .get_value::<String, _>("CurrentBuildNumber")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    if product.is_empty() {
        "Windows 11".to_string()
    } else if build >= 22000 && product.contains("Windows 10") {
        product.replace("Windows 10", "Windows 11")
    } else {
        product
    }
}

/// 从 IDE product.json 的 bootConfig.<section>.trae 解析主机（无安装/无配置时 None）
pub fn config_host(ide: IdeKind, section: &str, region: &str) -> Option<String> {
    IdePaths::detect(ide)?.boot_host(section, region)
}
