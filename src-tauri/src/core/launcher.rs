// 启动器：以 --inspect 拉起 Trae，为热换号保留主进程 CDP 通道
use super::paths::{IdeKind, IdePaths};
use std::io;

/// 按 IDE 分配调试端口：intl=9223 / CN=9224（双 IDE 并行时避免同端口抢占导致错连主进程）
pub fn inspect_port(ide: IdeKind) -> u16 {
    match ide {
        IdeKind::Trae => 9223,
        IdeKind::TraeCn => 9224,
    }
}

/// 以 --inspect=127.0.0.1:<port> 启动 Trae，返回子进程 pid
pub fn launch_with_inspect(ide: &IdePaths, port: u16) -> io::Result<u32> {
    let exe = ide.exe();
    if !exe.exists() {
        return Err(io::Error::new(io::ErrorKind::NotFound, format!("{:?}", exe)));
    }
    let child = std::process::Command::new(&exe)
        .arg(format!("--inspect=127.0.0.1:{port}"))
        // 官方 collector 逃生舱参数：跳过 main.js 对 inspector 的强制关闭
        // （条件 quality==="stable" && !argv.includes("--AutoCollectorIDE=Trae")）
        .arg("--AutoCollectorIDE=Trae")
        .current_dir(&ide.install_dir)
        .spawn()?;
    Ok(child.id())
}

/// Trae 是否正在运行（Toolhelp 快照进程名匹配）
pub fn is_running(ide: &IdePaths) -> bool {
    state(ide).0
}

/// 多开：以独立 user-data 目录启动第二个实例（绕过单实例锁，两个账号可同时在线）。
/// · 不带 --inspect：多开实例不参与热换号通道（避免与主实例调试端口抢占）；
/// · 独立 --extensions-dir：否则两个实例会同时加载 shim 扩展，两个监听器抢同一个
///   trigger 文件，热切换命令可能被错误的窗口消费。代价是多开实例不加载用户自装扩展。
pub fn launch_isolated(ide: &IdePaths, user_data_dir: &std::path::Path) -> io::Result<u32> {
    let exe = ide.exe();
    if !exe.exists() {
        return Err(io::Error::new(io::ErrorKind::NotFound, format!("{:?}", exe)));
    }
    let ext_dir = user_data_dir.join("extensions");
    std::fs::create_dir_all(&ext_dir)?;
    let child = std::process::Command::new(&exe)
        .arg(format!("--user-data-dir={}", user_data_dir.display()))
        .arg(format!("--extensions-dir={}", ext_dir.display()))
        .current_dir(&ide.install_dir)
        .spawn()?;
    Ok(child.id())
}

#[link(name = "kernel32")]
extern "system" {
    fn OpenProcess(access: u32, inherit: i32, pid: u32) -> *mut std::ffi::c_void;
    fn GetProcessTimes(
        h: *mut std::ffi::c_void,
        creation: *mut u64,
        exit: *mut u64,
        kernel: *mut u64,
        user: *mut u64,
    ) -> i32;
    fn CloseHandle(h: *mut std::ffi::c_void) -> i32;
    fn CreateToolhelp32Snapshot(flags: u32, pid: u32) -> *mut std::ffi::c_void;
    fn Process32FirstW(snap: *mut std::ffi::c_void, entry: *mut ProcessEntry32W) -> i32;
    fn Process32NextW(snap: *mut std::ffi::c_void, entry: *mut ProcessEntry32W) -> i32;
}

/// PROCESSENTRY32W（字段顺序/对齐与 Win32 一致）
#[repr(C)]
struct ProcessEntry32W {
    dw_size: u32,
    cnt_usage: u32,
    th32_process_id: u32,
    th32_default_heap_id: usize,
    th32_module_id: u32,
    cnt_threads: u32,
    th32_parent_process_id: u32,
    pc_pri_class_base: i32,
    dw_flags: u32,
    sz_exe_file: [u16; 260],
}

const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x1000;
const TH32CS_SNAPPROCESS: u32 = 0x2;
/// FILETIME(1601 起 100ns) → Unix 毫秒
const FILETIME_EPOCH_DIFF_MS: u64 = 11_644_473_600_000;

fn process_start_ms(pid: u32) -> Option<u64> {
    unsafe {
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if h.is_null() {
            return None;
        }
        let (mut c, mut e, mut k, mut u) = (0u64, 0u64, 0u64, 0u64);
        let ok = GetProcessTimes(h, &mut c, &mut e, &mut k, &mut u);
        CloseHandle(h);
        if ok == 0 || c / 10_000 < FILETIME_EPOCH_DIFF_MS {
            return None;
        }
        Some(c / 10_000 - FILETIME_EPOCH_DIFF_MS)
    }
}

/// 一次 Toolhelp 遍历同时拿到：是否运行 + 主进程（最早创建者）启动时刻。
/// 不用 tasklist：那是 fork 进程（实测单次 ~500ms），5 秒轮询会把 UI 冻住。
pub fn state(ide: &IdePaths) -> (bool, Option<u64>) {
    let name = match ide.exe().file_name().and_then(|n| n.to_str()) {
        Some(n) => n.to_string(),
        None => return (false, None),
    };
    unsafe {
        let snap = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snap as isize == -1 {
            return (false, None);
        }
        let mut entry: ProcessEntry32W = std::mem::zeroed();
        entry.dw_size = std::mem::size_of::<ProcessEntry32W>() as u32;
        let mut running = false;
        let mut earliest: Option<u64> = None;
        let mut ok = Process32FirstW(snap, &mut entry);
        while ok != 0 {
            let len = entry
                .sz_exe_file
                .iter()
                .position(|&c| c == 0)
                .unwrap_or(entry.sz_exe_file.len());
            let exe = String::from_utf16_lossy(&entry.sz_exe_file[..len]);
            if exe.eq_ignore_ascii_case(&name) {
                running = true;
                if let Some(ms) = process_start_ms(entry.th32_process_id) {
                    earliest = Some(earliest.map_or(ms, |b| b.min(ms)));
                }
            }
            ok = Process32NextW(snap, &mut entry);
        }
        CloseHandle(snap);
        (running, earliest)
    }
}

/// 当前运行实例的启动时刻（最早创建的进程 = 主进程）。
/// 用途：与磁盘补丁 mtime 比对，判断运行中的实例是否真的加载了补丁
/// ——磁盘写补丁时 IDE 若在运行，内存里仍是旧 bundle，通道不可用。
pub fn instance_start_ms(ide: &IdePaths) -> Option<u64> {
    state(ide).1
}

/// 该 IDE 全部进程 pid（含 NativeExtensionService 那种同名子进程）
pub fn pids(ide: &IdePaths) -> Vec<u32> {
    let Some(name) = ide.exe().file_name().and_then(|n| n.to_str()).map(|s| s.to_string()) else {
        return Vec::new();
    };
    unsafe {
        let snap = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snap as isize == -1 {
            return Vec::new();
        }
        let mut entry: ProcessEntry32W = std::mem::zeroed();
        entry.dw_size = std::mem::size_of::<ProcessEntry32W>() as u32;
        let mut out = Vec::new();
        let mut ok = Process32FirstW(snap, &mut entry);
        while ok != 0 {
            let len = entry
                .sz_exe_file
                .iter()
                .position(|&c| c == 0)
                .unwrap_or(entry.sz_exe_file.len());
            let exe = String::from_utf16_lossy(&entry.sz_exe_file[..len]);
            if exe.eq_ignore_ascii_case(&name) {
                out.push(entry.th32_process_id);
            }
            ok = Process32NextW(snap, &mut entry);
        }
        CloseHandle(snap);
        out
    }
}

/// 主进程 pid：同名进程里创建最早的那个
pub fn main_pid(ide: &IdePaths) -> Option<u32> {
    let mut best: Option<(u64, u32)> = None;
    for pid in pids(ide) {
        if let Some(ms) = process_start_ms(pid) {
            best = Some(match best {
                Some((t, p)) if t <= ms => (t, p),
                _ => (ms, pid),
            });
        }
    }
    best.map(|(_, pid)| pid)
}
