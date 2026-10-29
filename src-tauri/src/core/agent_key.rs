// 对话库 SQLCipher raw key：运行中从 agent 进程采集一次，DPAPI 落盘，换号不必每次扫内存
use super::paths::{IdeKind, IdePaths};
use super::sqlcipher;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::fs::OpenOptions;
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::windows::fs::OpenOptionsExt;
use std::os::windows::ffi::OsStrExt;
use std::path::Path;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

static LAST_AGENT_PID: AtomicU32 = AtomicU32::new(0);

const PROCESS_VM_READ: u32 = 0x0010;
const PROCESS_QUERY_INFORMATION: u32 = 0x0400;
const PROCESS_TERMINATE: u32 = 0x0001;
const MEM_COMMIT: u32 = 0x1000;
const PAGE_GUARD: u32 = 0x100;
const PAGE_NOACCESS: u32 = 0x01;
const READABLE: u32 = 0x02 | 0x04 | 0x08 | 0x20 | 0x40 | 0x80;

#[repr(C)]
struct MemoryBasicInformation {
    base_address: usize,
    allocation_base: usize,
    allocation_protect: u32,
    _partition: u32,
    region_size: usize,
    state: u32,
    protect: u32,
    type_: u32,
}

const CCH_RM_SESSION_KEY: usize = 32;
const ERROR_MORE_DATA: u32 = 234;

#[repr(C)]
#[derive(Clone, Copy)]
struct RmUniqueProcess {
    dw_process_id: u32,
    process_start_time: i64,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct RmProcessInfo {
    process: RmUniqueProcess,
    str_app_name: [u16; 256],
    str_service_short_name: [u16; 64],
    application_type: i32,
    app_status: u32,
    ts_session_id: u32,
    b_restartable: i32,
}

#[link(name = "rstrtmgr")]
extern "system" {
    fn RmStartSession(session: *mut u32, flags: u32, key: *mut u16) -> u32;
    fn RmRegisterResources(
        session: u32,
        n_files: u32,
        files: *const *const u16,
        n_applications: u32,
        applications: *const RmUniqueProcess,
        n_services: u32,
        services: *const *const u16,
    ) -> u32;
    fn RmGetList(
        session: u32,
        needed: *mut u32,
        n_info: *mut u32,
        info: *mut RmProcessInfo,
        reboot_reasons: *mut u32,
    ) -> u32;
    fn RmEndSession(session: u32) -> u32;
}

fn pids_locking(path: &Path) -> Vec<u32> {
    let wide: Vec<u16> = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    unsafe {
        let mut key = [0u16; CCH_RM_SESSION_KEY + 1];
        let mut session = 0u32;
        if RmStartSession(&mut session, 0, key.as_mut_ptr()) != 0 {
            return Vec::new();
        }
        let ptr = wide.as_ptr();
        let files = [ptr];
        if RmRegisterResources(session, 1, files.as_ptr(), 0, std::ptr::null(), 0, std::ptr::null()) != 0 {
            RmEndSession(session);
            return Vec::new();
        }
        let mut needed = 0u32;
        let mut n = 0u32;
        let mut reboot = 0u32;
        let st = RmGetList(session, &mut needed, &mut n, std::ptr::null_mut(), &mut reboot);
        let mut out = Vec::new();
        if (st == 0 || st == ERROR_MORE_DATA) && needed > 0 {
            let mut info = vec![std::mem::zeroed::<RmProcessInfo>(); needed as usize];
            n = needed;
            if RmGetList(session, &mut needed, &mut n, info.as_mut_ptr(), &mut reboot) == 0 {
                for it in info.iter().take(n as usize) {
                    if it.process.dw_process_id != 0 {
                        out.push(it.process.dw_process_id);
                    }
                }
            }
        }
        RmEndSession(session);
        out
    }
}

#[link(name = "kernel32")]
extern "system" {
    fn OpenProcess(access: u32, inherit: i32, pid: u32) -> *mut std::ffi::c_void;
    fn CloseHandle(h: *mut std::ffi::c_void) -> i32;
    fn ReadProcessMemory(
        h: *mut std::ffi::c_void,
        addr: *const std::ffi::c_void,
        buf: *mut std::ffi::c_void,
        size: usize,
        read: *mut usize,
    ) -> i32;
    fn VirtualQueryEx(
        h: *mut std::ffi::c_void,
        addr: *const std::ffi::c_void,
        info: *mut MemoryBasicInformation,
        len: usize,
    ) -> usize;
    fn TerminateProcess(h: *mut std::ffi::c_void, code: u32) -> i32;
}

fn store_path() -> std::path::PathBuf {
    super::vault::app_data_dir().join("agent-keys.dpapi")
}

fn slot(ide: IdeKind) -> &'static str {
    match ide {
        IdeKind::Trae => "trae",
        IdeKind::TraeCn => "trae-cn",
    }
}

fn to_hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn from_hex(s: &str) -> Option<Vec<u8>> {
    let s = s.trim();
    if s.len() % 2 != 0 {
        return None;
    }
    (0..s.len() / 2)
        .map(|i| u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).ok())
        .collect()
}

fn load_map() -> HashMap<String, [u8; 32]> {
    migrate_legacy();
    let path = store_path();
    let Ok(cipher) = std::fs::read(&path) else {
        return HashMap::new();
    };
    let Ok(plain) = super::vault::decrypt_bytes(&cipher) else {
        return HashMap::new();
    };
    let Ok(v) = serde_json::from_slice::<Value>(&plain) else {
        return HashMap::new();
    };
    let mut out = HashMap::new();
    if let Some(obj) = v.as_object() {
        for (k, val) in obj {
            let hex = val
                .get("enc_key")
                .and_then(|x| x.as_str())
                .or_else(|| val.as_str());
            if let Some(h) = hex {
                if let Some(raw) = from_hex(h) {
                    if let Ok(arr) = <[u8; 32]>::try_from(raw.as_slice()) {
                        out.insert(k.clone(), arr);
                    }
                }
            }
        }
    }
    out
}

fn save_map(map: &HashMap<String, [u8; 32]>) -> Result<(), String> {
    let mut obj = serde_json::Map::new();
    for (k, key) in map {
        obj.insert(k.clone(), json!({ "enc_key": to_hex(key) }));
    }
    let plain = serde_json::to_vec(&Value::Object(obj)).map_err(|e| e.to_string())?;
    let dir = super::vault::app_data_dir();
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let cipher = super::vault::encrypt_bytes(&plain).map_err(|e| e.to_string())?;
    std::fs::write(store_path(), cipher).map_err(|e| e.to_string())
}

/// 把早期研究脚本留下的明文 json 迁进 DPAPI，并删掉明文
fn migrate_legacy() {
    let dir = super::vault::app_data_dir().join("trae").join("keys");
    let mut map = HashMap::new();
    if store_path().exists() {
        map = load_map_raw();
    }
    let mut changed = false;
    for (name, slot_name) in [("trae-cn.json", "trae-cn"), ("trae.json", "trae")] {
        let p = dir.join(name);
        if !p.exists() {
            continue;
        }
        if let Ok(s) = std::fs::read_to_string(&p) {
            if let Ok(v) = serde_json::from_str::<Value>(&s) {
                if let Some(h) = v.get("enc_key").and_then(|x| x.as_str()) {
                    if let Some(raw) = from_hex(h) {
                        if let Ok(arr) = <[u8; 32]>::try_from(raw.as_slice()) {
                            map.entry(slot_name.to_string()).or_insert(arr);
                            changed = true;
                        }
                    }
                }
            }
        }
        let _ = std::fs::remove_file(&p);
    }
    if changed {
        let _ = save_map(&map);
    }
}

fn load_map_raw() -> HashMap<String, [u8; 32]> {
    let path = store_path();
    let Ok(cipher) = std::fs::read(&path) else {
        return HashMap::new();
    };
    let Ok(plain) = super::vault::decrypt_bytes(&cipher) else {
        return HashMap::new();
    };
    let Ok(v) = serde_json::from_slice::<Value>(&plain) else {
        return HashMap::new();
    };
    let mut out = HashMap::new();
    if let Some(obj) = v.as_object() {
        for (k, val) in obj {
            let hex = val
                .get("enc_key")
                .and_then(|x| x.as_str())
                .or_else(|| val.as_str());
            if let Some(h) = hex {
                if let Some(raw) = from_hex(h) {
                    if let Ok(arr) = <[u8; 32]>::try_from(raw.as_slice()) {
                        out.insert(k.clone(), arr);
                    }
                }
            }
        }
    }
    out
}

pub fn load_key(ide: IdeKind) -> Option<[u8; 32]> {
    load_map().get(slot(ide)).copied()
}

pub fn save_key(ide: IdeKind, key: &[u8; 32]) -> Result<(), String> {
    let mut map = load_map();
    map.insert(slot(ide).to_string(), *key);
    save_map(&map)
}

fn is_hex(b: u8) -> bool {
    matches!(b, b'0'..=b'9' | b'a'..=b'f' | b'A'..=b'F')
}

fn parse_x_key(buf: &[u8], salt_hex: &[u8]) -> Option<[u8; 32]> {
    // 形态：x'<64hex><32hex_salt>'
    let needle = salt_hex;
    let mut from = 0;
    while from + needle.len() <= buf.len() {
        let Some(rel) = buf[from..]
            .windows(needle.len())
            .position(|w| w.eq_ignore_ascii_case(needle))
        else {
            break;
        };
        let salt_at = from + rel;
        if salt_at >= 66 {
            let start = salt_at - 66;
            if buf[start] == b'x' && buf[start + 1] == b'\'' {
                let hex64 = &buf[start + 2..start + 66];
                if hex64.iter().all(|&c| is_hex(c)) {
                    let q = salt_at + needle.len();
                    if q < buf.len() && buf[q] == b'\'' {
                        if let Some(raw) = from_hex(&String::from_utf8_lossy(hex64)) {
                            if let Ok(arr) = <[u8; 32]>::try_from(raw.as_slice()) {
                                return Some(arr);
                            }
                        }
                    }
                }
            }
        }
        from = salt_at + 1;
    }
    None
}

fn scan_pid(pid: u32, salt_hex: &[u8]) -> Option<[u8; 32]> {
    unsafe {
        let h = OpenProcess(PROCESS_VM_READ | PROCESS_QUERY_INFORMATION, 0, pid);
        if h.is_null() {
            return None;
        }
        let mut addr: usize = 0;
        let mut info: MemoryBasicInformation = std::mem::zeroed();
        let overlap = 96usize;
        let mut leftover = Vec::new();
        while VirtualQueryEx(
            h,
            addr as *const _,
            &mut info,
            std::mem::size_of::<MemoryBasicInformation>(),
        ) == std::mem::size_of::<MemoryBasicInformation>()
        {
            let readable = info.state == MEM_COMMIT
                && (info.protect & PAGE_GUARD) == 0
                && (info.protect & PAGE_NOACCESS) == 0
                && (info.protect & READABLE) != 0
                && info.region_size > 0
                && info.region_size < 512 * 1024 * 1024;
            if readable {
                let mut off = 0usize;
                while off < info.region_size {
                    let chunk = (info.region_size - off).min(2 * 1024 * 1024);
                    let mut buf = vec![0u8; leftover.len() + chunk];
                    if !leftover.is_empty() {
                        buf[..leftover.len()].copy_from_slice(&leftover);
                    }
                    let mut n = 0usize;
                    let ok = ReadProcessMemory(
                        h,
                        (info.base_address + off) as *const _,
                        buf[leftover.len()..].as_mut_ptr() as *mut _,
                        chunk,
                        &mut n,
                    );
                    if ok != 0 && n > 0 {
                        buf.truncate(leftover.len() + n);
                        if let Some(k) = parse_x_key(&buf, salt_hex) {
                            CloseHandle(h);
                            return Some(k);
                        }
                        leftover = if buf.len() > overlap {
                            buf[buf.len() - overlap..].to_vec()
                        } else {
                            buf
                        };
                    } else {
                        leftover.clear();
                    }
                    off += chunk;
                }
            } else {
                leftover.clear();
            }
            let next = info.base_address.saturating_add(info.region_size);
            if next <= addr {
                break;
            }
            addr = next;
        }
        CloseHandle(h);
        None
    }
}

/// 从正在跑的 Trae 进程里抓 SQLCipher raw key（salt 必须对得上当前 database.db）
pub fn capture_from_memory(paths: &IdePaths) -> Option<[u8; 32]> {
    let db = paths.agent_database();
    let salt = sqlcipher::read_salt(&db).ok()?;
    let salt_hex = to_hex(&salt).into_bytes();
    for pid in super::launcher::pids(paths) {
        if let Some(k) = scan_pid(pid, &salt_hex) {
            if sqlcipher::verify_key(&db, &k).is_ok() {
                LAST_AGENT_PID.store(pid, Ordering::Relaxed);
                return Some(k);
            }
        }
    }
    None
}

/// 已验证的落盘密钥优先；没有或解不开再扫运行中的 agent
pub fn ensure_key(paths: &IdePaths) -> Result<[u8; 32], String> {
    let db = paths.agent_database();
    if !db.exists() {
        return Err("未找到对话库".into());
    }
    if let Some(k) = load_key(paths.ide) {
        if sqlcipher::verify_key(&db, &k).is_ok() {
            return Ok(k);
        }
    }
    if let Some(k) = capture_from_memory(paths) {
        let _ = save_key(paths.ide, &k);
        return Ok(k);
    }
    Err("尚未采集对话库密钥：请先打开 Trae 再换号一次".into())
}

pub fn pids_with_key(paths: &IdePaths, key: &[u8; 32]) -> Vec<u32> {
    let Ok(salt) = sqlcipher::read_salt(&paths.agent_database()) else {
        return Vec::new();
    };
    let salt_hex = to_hex(&salt).into_bytes();
    let want = to_hex(key);
    let mut hits = Vec::new();
    for pid in super::launcher::pids(paths) {
        if let Some(k) = scan_pid(pid, &salt_hex) {
            if to_hex(&k) == want {
                hits.push(pid);
            }
        }
    }
    hits
}

pub fn terminate_pid(pid: u32) -> bool {
    unsafe {
        let h = OpenProcess(PROCESS_TERMINATE, 0, pid);
        if h.is_null() {
            return false;
        }
        let ok = TerminateProcess(h, 1);
        CloseHandle(h);
        ok != 0
    }
}

pub fn db_unlocked(path: &Path) -> bool {
    OpenOptions::new()
        .read(true)
        .write(true)
        .share_mode(0)
        .open(path)
        .is_ok()
}

pub fn wait_db_unlocked(path: &Path, timeout: Duration) -> bool {
    let start = Instant::now();
    while start.elapsed() < timeout {
        if db_unlocked(path) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(80));
    }
    false
}

/// 结束握着对话库的 agent 进程，直到文件可独占。不杀 IDE 主进程。
pub fn pause_agent(paths: &IdePaths, key: &[u8; 32]) -> Result<(), String> {
    let db = paths.agent_database();
    if db_unlocked(&db) {
        return Ok(());
    }
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(8) {
        let mut targets = pids_locking(&db);
        if targets.is_empty() {
            let last = LAST_AGENT_PID.load(Ordering::Relaxed);
            if last != 0 {
                targets.push(last);
            }
        }
        if targets.is_empty() {
            targets = pids_with_key(paths, key);
        }
        let self_pid = std::process::id();
        let main_pid = super::launcher::main_pid(paths);
        for pid in targets {
            if pid == 0 || pid == self_pid {
                continue;
            }
            if main_pid == Some(pid) {
                continue;
            }
            LAST_AGENT_PID.store(pid, Ordering::Relaxed);
            let _ = terminate_pid(pid);
        }
        if wait_db_unlocked(&db, Duration::from_millis(400)) {
            return Ok(());
        }
    }
    if db_unlocked(&db) {
        Ok(())
    } else {
        Err("对话库仍被占用，无法改挂会话".into())
    }
}

/// 独占打开后把源文件拷到 dest（Windows 下 share_mode=0 时不能另开句柄读）
pub fn copy_exclusive(src: &Path, dst: &Path) -> Result<(), String> {
    let mut f = OpenOptions::new()
        .read(true)
        .write(true)
        .share_mode(0)
        .open(src)
        .map_err(|e| format!("无法独占对话库: {e}"))?;
    let mut out = std::fs::File::create(dst).map_err(|e| e.to_string())?;
    f.seek(SeekFrom::Start(0)).map_err(|e| e.to_string())?;
    let mut buf = [0u8; 1024 * 1024];
    loop {
        let n = f.read(&mut buf).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        out.write_all(&buf[..n]).map_err(|e| e.to_string())?;
    }
    out.flush().map_err(|e| e.to_string())?;
    Ok(())
}

/// 独占覆写 live 文件
pub fn replace_exclusive(live: &Path, new_file: &Path) -> Result<(), String> {
    let mut src = std::fs::File::open(new_file).map_err(|e| e.to_string())?;
    let mut dst = OpenOptions::new()
        .read(true)
        .write(true)
        .share_mode(0)
        .open(live)
        .map_err(|e| format!("无法独占写入对话库: {e}"))?;
    dst.set_len(0).map_err(|e| e.to_string())?;
    let mut buf = [0u8; 1024 * 1024];
    loop {
        let n = src.read(&mut buf).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        dst.write_all(&buf[..n]).map_err(|e| e.to_string())?;
    }
    dst.flush().map_err(|e| e.to_string())?;
    Ok(())
}
