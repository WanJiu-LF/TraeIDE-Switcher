// SQLCipher 运行中连接：内嵌 e_sqlcipher.dll（SQLCipher 4.5.2，BSD 许可），
// 首次使用时释放到 %APPDATA%\TraeIDE-Switcher\native 并 LoadLibrary，通过 sqlite3 C API
// 直接读写 Trae 对话库。运行中的 agent 打开的是同一个库文件，并发由 SQLite 自己的
// 锁 + WAL 语义保证（实测：可取得写锁、提交后 agent 立即读到，无需停服务）。
// 为什么不走 rusqlite：bundled-sqlcipher 需要编译 OpenSSL（Perl/NASM），本机与用户机器都没有。
use std::ffi::{c_char, c_int, c_void, CString};
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use windows_sys::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};

const DLL_BYTES: &[u8] = include_bytes!("../../native/e_sqlcipher.dll");

type ExecCb = Option<unsafe extern "C" fn(*mut c_void, c_int, *mut *mut c_char, *mut *mut c_char) -> c_int>;
type FnOpen = unsafe extern "C" fn(*const c_char, *mut *mut c_void, c_int, *const c_char) -> c_int;
type FnExec = unsafe extern "C" fn(*mut c_void, *const c_char, ExecCb, *mut c_void, *mut *mut c_char) -> c_int;
type FnClose = unsafe extern "C" fn(*mut c_void) -> c_int;
type FnErrmsg = unsafe extern "C" fn(*mut c_void) -> *const c_char;

struct Api {
    open: FnOpen,
    exec: FnExec,
    close: FnClose,
    errmsg: FnErrmsg,
}

static API: OnceLock<Result<Api, String>> = OnceLock::new();

const OPEN_RW: c_int = 0x0000_0002;

fn dll_path() -> Result<PathBuf, String> {
    let dir = super::vault::app_data_dir().join("native");
    let path = dir.join("e_sqlcipher.dll");
    let uptodate = std::fs::metadata(&path)
        .map(|m| m.len() as usize == DLL_BYTES.len())
        .unwrap_or(false);
    if !uptodate {
        std::fs::create_dir_all(&dir).map_err(|e| format!("创建 native 目录失败：{e}"))?;
        std::fs::write(&path, DLL_BYTES).map_err(|e| format!("释放 e_sqlcipher.dll 失败：{e}"))?;
    }
    Ok(path)
}

fn load_api() -> Result<Api, String> {
    let path = dll_path()?;
    unsafe {
        let wide: Vec<u16> = path.as_os_str().encode_wide().chain(std::iter::once(0)).collect();
        let h = LoadLibraryW(wide.as_ptr());
        if h.is_null() {
            return Err("加载 e_sqlcipher.dll 失败".into());
        }
        let sym = |name: &str| -> Result<*const c_void, String> {
            let c = CString::new(name).unwrap();
            GetProcAddress(h, c.as_ptr() as *const u8)
                .map(|f| f as *const c_void)
                .ok_or_else(|| format!("e_sqlcipher.dll 缺少导出：{name}"))
        };
        Ok(Api {
            open: std::mem::transmute(sym("sqlite3_open_v2")?),
            exec: std::mem::transmute(sym("sqlite3_exec")?),
            close: std::mem::transmute(sym("sqlite3_close_v2")?),
            errmsg: std::mem::transmute(sym("sqlite3_errmsg")?),
        })
    }
}

fn api() -> Result<&'static Api, String> {
    API.get_or_init(load_api).as_ref().map_err(|e| e.clone())
}

struct Rows {
    out: Vec<Vec<String>>,
}

unsafe extern "C" fn collect(
    ctx: *mut c_void,
    argc: c_int,
    argv: *mut *mut c_char,
    _cols: *mut *mut c_char,
) -> c_int {
    if ctx.is_null() || argv.is_null() {
        return 0;
    }
    let acc = &mut *(ctx as *mut Rows);
    let mut row = Vec::with_capacity(argc as usize);
    for i in 0..argc as isize {
        let p = *argv.offset(i);
        row.push(if p.is_null() {
            String::new()
        } else {
            std::ffi::CStr::from_ptr(p).to_string_lossy().into_owned()
        });
    }
    acc.out.push(row);
    0
}

pub struct Db {
    ptr: *mut c_void,
    api: &'static Api,
}

impl Db {
    /// 以读写方式打开对话库并设置 raw key（x'<64hex>'）；调用方保证密钥已采集。
    pub fn open_live(path: &Path, key: &[u8; 32]) -> Result<Db, String> {
        let api = api()?;
        let hex: String = key.iter().map(|b| format!("{b:02x}")).collect();
        unsafe {
            let cpath = CString::new(path.display().to_string()).map_err(|e| e.to_string())?;
            let mut ptr: *mut c_void = std::ptr::null_mut();
            let rc = (api.open)(cpath.as_ptr(), &mut ptr, OPEN_RW, std::ptr::null());
            if rc != 0 || ptr.is_null() {
                return Err(format!("打开对话库失败（rc={rc}）"));
            }
            let db = Db { ptr, api };
            db.exec(&format!("PRAGMA key = \"x'{hex}'\";"))?;
            db.exec("PRAGMA busy_timeout = 8000;")?;
            Ok(db)
        }
    }

    fn err(&self) -> String {
        unsafe {
            let p = (self.api.errmsg)(self.ptr);
            if p.is_null() {
                "unknown".into()
            } else {
                std::ffi::CStr::from_ptr(p).to_string_lossy().into_owned()
            }
        }
    }

    pub fn exec(&self, sql: &str) -> Result<(), String> {
        unsafe {
            let c = CString::new(sql).map_err(|e| e.to_string())?;
            let mut err: *mut c_char = std::ptr::null_mut();
            let rc = (self.api.exec)(self.ptr, c.as_ptr(), None, std::ptr::null_mut(), &mut err);
            if rc != 0 {
                return Err(format!("SQL 失败（{rc}）：{}", self.err_message(err)));
            }
            Ok(())
        }
    }

    fn err_message(&self, err: *mut c_char) -> String {
        if err.is_null() {
            self.err()
        } else {
            unsafe { std::ffi::CStr::from_ptr(err).to_string_lossy().into_owned() }
        }
    }

    /// 执行查询并返回全部行（每行按列拼接为字符串）
    pub fn query(&self, sql: &str) -> Result<Vec<Vec<String>>, String> {
        unsafe {
            let c = CString::new(sql).map_err(|e| e.to_string())?;
            let mut rows = Rows { out: Vec::new() };
            let mut err: *mut c_char = std::ptr::null_mut();
            let rc = (self.api.exec)(
                self.ptr,
                c.as_ptr(),
                Some(collect),
                &mut rows as *mut Rows as *mut c_void,
                &mut err,
            );
            if rc != 0 {
                return Err(format!("SQL 失败（{rc}）：{}", self.err_message(err)));
            }
            Ok(rows.out)
        }
    }

    pub fn one(&self, sql: &str) -> Result<Option<String>, String> {
        Ok(self.query(sql)?.into_iter().next().and_then(|r| r.into_iter().next()))
    }
}

impl Drop for Db {
    fn drop(&mut self) {
        unsafe {
            (self.api.close)(self.ptr);
        }
    }
}
