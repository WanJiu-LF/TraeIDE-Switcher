// 本地存储适配：storage.json / TinyStorage / machineid / 注册表 deviceid / state.vscdb
use super::bytecrypto;
use super::paths::IdePaths;
use serde_json::{Map, Value};
use std::io;
use std::path::PathBuf;

fn read_json(path: &PathBuf) -> io::Result<Value> {
    let raw = std::fs::read_to_string(path)?;
    serde_json::from_str(&raw).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

/// globalStorage/storage.json —— Trae 内存 map 的落盘镜像（冷换号时直接改文件）
pub struct StorageJson {
    path: PathBuf,
    root: Map<String, Value>,
}

impl StorageJson {
    pub fn load(ide: &IdePaths) -> io::Result<Self> {
        Self::load_at(ide.storage_json())
    }

    /// 从任意路径读取（多开实例使用独立 user-data 目录，路径不由 IdePaths 决定）
    pub fn load_at(path: PathBuf) -> io::Result<Self> {
        let v = read_json(&path)?;
        let root = v.as_object().cloned().ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidData, "storage.json root is not an object")
        })?;
        Ok(Self { path, root })
    }

    pub fn get(&self, key: &str) -> Option<&Value> {
        self.root.get(key)
    }

    pub fn set(&mut self, key: &str, value: Value) {
        self.root.insert(key.to_string(), value);
    }

    pub fn set_str(&mut self, key: &str, value: &str) {
        self.set(key, Value::String(value.to_string()));
    }

    pub fn remove(&mut self, key: &str) {
        self.root.remove(key);
    }

    /// 前缀批量删除（iCubeAuthInfo://icube-dc:* ECDSA 密钥清理）
    pub fn remove_with_prefix(&mut self, prefix: &str) {
        let keys: Vec<String> = self
            .root
            .keys()
            .filter(|k| k.starts_with(prefix))
            .cloned()
            .collect();
        for k in keys {
            self.root.remove(&k);
        }
    }

    /// 前缀匹配键集合
    pub fn keys_with_prefix(&self, prefix: &str) -> Vec<String> {
        self.root
            .keys()
            .filter(|k| k.starts_with(prefix))
            .cloned()
            .collect()
    }

    /// 与 Trae writeAll 相同的格式：JSON.stringify(e, null, 2)
    pub fn save(&self) -> io::Result<()> {
        std::fs::write(&self.path, serde_json::to_string_pretty(&self.root).unwrap())
    }
}

/// aha/TinyStorage —— { tiny_storage_data: { key: 加密串 }, ... }，值经 bytecrypto 加密
pub struct TinyStorage {
    path: PathBuf,
    root: Map<String, Value>,
}

impl TinyStorage {
    pub fn load(ide: &IdePaths) -> io::Result<Self> {
        Self::load_at(ide.tiny_storage())
    }

    /// 从任意路径读取（多开实例使用独立 user-data 目录）
    pub fn load_at(path: PathBuf) -> io::Result<Self> {
        let v = read_json(&path)?;
        let root = v.as_object().cloned().unwrap_or_default();
        Ok(Self { path, root })
    }

    /// 文件不存在时返回空表（首次为账号创建 TinyStorage）
    pub fn load_or_default(ide: &IdePaths) -> io::Result<Self> {
        Self::load_or_default_at(ide.tiny_storage())
    }

    pub fn load_or_default_at(path: PathBuf) -> io::Result<Self> {
        match Self::load_at(path.clone()) {
            Ok(s) => Ok(s),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Self {
                path,
                root: Map::new(),
            }),
            Err(e) => Err(e),
        }
    }

    fn data_map(&self) -> &Map<String, Value> {
        self.root
            .get("tiny_storage_data")
            .and_then(|v| v.as_object())
            .expect("tiny_storage_data missing")
    }

    /// tiny_storage_data 原始快照（加密串原文，随号保存）
    pub fn raw_data(&self) -> Option<Value> {
        self.root.get("tiny_storage_data").cloned()
    }

    /// 整体替换 tiny_storage_data（快照恢复）
    pub fn restore_data(&mut self, snapshot: Value) {
        if snapshot.is_object() {
            self.root.insert("tiny_storage_data".into(), snapshot);
        }
    }

    pub fn get_decrypted(&self, key: &str) -> Option<String> {
        let enc = self.data_map().get(key)?.as_str()?;
        bytecrypto::decrypt_string(enc).ok()
    }

    pub fn set_encrypted(&mut self, key: &str, plain: &str) {
        let data = self
            .root
            .entry("tiny_storage_data")
            .or_insert_with(|| Value::Object(Map::new()));
        if let Some(map) = data.as_object_mut() {
            map.insert(key.to_string(), Value::String(bytecrypto::encrypt_string(plain)));
        }
    }

    pub fn save(&self) -> io::Result<()> {
        std::fs::write(&self.path, serde_json::to_string(&self.root).unwrap())
    }
}

/// %APPDATA%\<IDE>\machineid —— 36 字节 UUID 文本
pub fn read_machineid(ide: &IdePaths) -> io::Result<String> {
    Ok(std::fs::read_to_string(ide.machineid_file())?.trim().to_string())
}

pub fn write_machineid(ide: &IdePaths, value: &str) -> io::Result<()> {
    std::fs::write(ide.machineid_file(), value)
}

/// HKCU\Software\Microsoft\DeveloperTools\deviceid —— @vscode/deviceid 原生模块读写处
pub fn read_registry_deviceid() -> Option<String> {
    use winreg::enums::HKEY_CURRENT_USER;
    use winreg::RegKey;
    RegKey::predef(HKEY_CURRENT_USER)
        .open_subkey(r"Software\Microsoft\DeveloperTools")
        .ok()?
        .get_value("deviceid")
        .ok()
}

pub fn write_registry_deviceid(value: &str) -> io::Result<()> {
    use winreg::enums::HKEY_CURRENT_USER;
    use winreg::RegKey;
    RegKey::predef(HKEY_CURRENT_USER)
        .open_subkey_with_flags(r"Software\Microsoft\DeveloperTools", winreg::enums::KEY_SET_VALUE)?
        .set_value("deviceid", &value)
}

/// User/globalStorage/state.vscdb —— ItemTable(key TEXT, value TEXT)
pub struct Vscdb {
    conn: rusqlite::Connection,
}

impl Vscdb {
    pub fn open(ide: &IdePaths) -> rusqlite::Result<Self> {
        Self::open_path(ide.state_vscdb())
    }

    pub fn open_path(path: PathBuf) -> rusqlite::Result<Self> {
        Ok(Self {
            conn: rusqlite::Connection::open(path)?,
        })
    }

    pub fn get_text(&self, key: &str) -> rusqlite::Result<Option<String>> {
        let mut stmt = self
            .conn
            .prepare_cached("SELECT value FROM ItemTable WHERE key = ?1")?;
        let mut rows = stmt.query([key])?;
        Ok(rows.next()?.map(|r| r.get(0)).transpose()?)
    }

    pub fn set_text(&self, key: &str, value: &str) -> rusqlite::Result<()> {
        self.conn.execute(
            "INSERT INTO ItemTable (key, value) VALUES (?1, ?2) ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            rusqlite::params![key, value],
        )?;
        Ok(())
    }
}
