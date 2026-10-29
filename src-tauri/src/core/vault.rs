// 账号库：DPAPI（CurrentUser）加密落盘的 JSON 文件
use std::io;
use std::path::PathBuf;
use windows_sys::Win32::Foundation::LocalFree;
use windows_sys::Win32::Security::Cryptography::{
    CryptProtectData, CryptUnprotectData, CRYPTPROTECT_UI_FORBIDDEN, CRYPT_INTEGER_BLOB as DATA_BLOB,
};

pub fn app_data_dir() -> PathBuf {
    let base = std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\Users\Default\AppData\Roaming"));
    base.join("TraeIDE-Switcher")
}

fn vault_path() -> PathBuf {
    app_data_dir().join("accounts.dpapi")
}

fn blob(data: &[u8]) -> DATA_BLOB {
    DATA_BLOB {
        cbData: data.len() as u32,
        pbData: data.as_ptr() as *mut u8,
    }
}

fn protect(plain: &[u8]) -> io::Result<Vec<u8>> {
    unsafe {
        let mut in_blob = blob(plain);
        let mut out_blob = DATA_BLOB { cbData: 0, pbData: std::ptr::null_mut() };
        let ok = CryptProtectData(
            &mut in_blob,
            std::ptr::null(),
            std::ptr::null(),
            std::ptr::null(),
            std::ptr::null(),
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut out_blob,
        );
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        let out = std::slice::from_raw_parts(out_blob.pbData, out_blob.cbData as usize).to_vec();
        LocalFree(out_blob.pbData as _);
        Ok(out)
    }
}

fn unprotect(cipher: &[u8]) -> io::Result<Vec<u8>> {
    unsafe {
        let mut in_blob = blob(cipher);
        let mut out_blob = DATA_BLOB { cbData: 0, pbData: std::ptr::null_mut() };
        let ok = CryptUnprotectData(
            &mut in_blob,
            std::ptr::null_mut(),
            std::ptr::null(),
            std::ptr::null(),
            std::ptr::null(),
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut out_blob,
        );
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        let out = std::slice::from_raw_parts(out_blob.pbData, out_blob.cbData as usize).to_vec();
        LocalFree(out_blob.pbData as _);
        Ok(out)
    }
}

/// 读账号库 JSON（DPAPI 解密）。不存在返回 None。
pub fn load_json() -> io::Result<Option<serde_json::Value>> {
    let path = vault_path();
    if !path.exists() {
        return Ok(None);
    }
    let cipher = std::fs::read(path)?;
    let plain = unprotect(&cipher)?;
    let v = serde_json::from_slice(&plain).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    Ok(Some(v))
}

/// 账号库写锁。save_json 是无锁整文件覆写，而额度查询/保活预刷新等路径会并发触发，
/// 并发写会互相覆盖（丢失刚刷新到的凭据）甚至写出半截文件 → 必须串行化。
static WRITE_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// 备份/恢复用目录（导出包与轮转备份都放这里）
pub fn backup_dir() -> PathBuf {
    app_data_dir().join("backup")
}

/// 账号库文件路径（备份/恢复需要直接搬运密文）
pub fn vault_file() -> PathBuf {
    vault_path()
}

/// 覆写前轮转上一版为 accounts.bak：提供一级"误删/写坏"回滚
fn rotate_backup() {
    let path = vault_path();
    if !path.exists() {
        return;
    }
    let _ = std::fs::copy(&path, app_data_dir().join("accounts.bak"));
}

/// 写账号库 JSON（DPAPI 加密）
pub fn save_json(value: &serde_json::Value) -> io::Result<()> {
    let _g = WRITE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    save_unlocked(value)
}

fn save_unlocked(value: &serde_json::Value) -> io::Result<()> {
    let path = vault_path();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    rotate_backup();
    let plain = serde_json::to_vec(value).unwrap();
    std::fs::write(path, protect(&plain)?)
}

/// 原子读改写：全程持写锁，避免"读→改→写"之间被其他写入覆盖。
/// 供后台保活等"必须基于最新库内容做局部修改"的路径使用。
pub fn update<F: FnOnce(&mut serde_json::Value)>(f: F) -> io::Result<()> {
    let _g = WRITE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut v = load_json()?.unwrap_or_else(|| serde_json::json!({ "accounts": [] }));
    f(&mut v);
    save_unlocked(&v)
}

/// 备份校验用：解密任意 DPAPI 密文（同机同用户才可解）
pub fn decrypt_bytes(cipher: &[u8]) -> io::Result<Vec<u8>> {
    unprotect(cipher)
}

/// 给对话库密钥等非账号库敏感文件用的 DPAPI 封装
pub fn encrypt_bytes(plain: &[u8]) -> io::Result<Vec<u8>> {
    protect(plain)
}

/// 备份恢复用：用备份密文整体替换账号库（先轮转原库，再写入）
pub fn replace_with_bytes(cipher: &[u8]) -> io::Result<()> {
    let _g = WRITE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    rotate_backup();
    let path = vault_path();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(path, cipher)
}

/// 当前激活账号 id（明文 id，不含凭据）
fn current_path() -> PathBuf {
    app_data_dir().join("current.txt")
}

pub fn set_current_id(id: &str) {
    if let Some(dir) = app_data_dir().to_str() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(current_path(), id);
}

pub fn current_id() -> Option<String> {
    std::fs::read_to_string(current_path())
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}
