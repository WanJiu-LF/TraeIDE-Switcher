// 换号时把上一账号（及当前号）的本地会话按原工作区接到新号。
// Trae 侧栏按 get_or_create_project_v2(biz_project_id, user_id) 取会话列表：biz_project_id 是
// VS Code 的 workspace.id（本地文件夹 = md5(fsPath + 创建毫秒)），补丁 ⑬ 再拼 __tsw_u_<uid>。
// 文件夹重建过的话旧 project 的 biz 已不是当前 id，所以目标 biz 按文件夹现算，不沿用旧值。
use super::agent_key;
use super::logger::{self, Level, Source};
use super::paths::{IdeKind, IdePaths};
use super::sqlcipher;
use rusqlite::{params, Connection};
use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};

const BIZ_SALT: &str = "__tsw_u_";
const KIND_WORKSPACE: &str = "workspace";

#[derive(Debug, Default, Clone)]
pub struct RebindReport {
    pub sessions: u32,
    pub projects_mapped: u32,
    /// 定位不到工作区、留在原账号的会话
    pub skipped: u32,
    /// 新号在当前窗口工作区下的会话数（算不出当前工作区时为 None）
    pub current_window: Option<u32>,
}

impl RebindReport {
    pub fn summary(&self) -> String {
        if self.sessions == 0 && self.skipped == 0 {
            return "没有需要接管的会话".into();
        }
        let mut s = format!(
            "已把 {} 条会话按原工作区接到新账号（{} 个工作区",
            self.sessions, self.projects_mapped
        );
        if let Some(n) = self.current_window {
            s.push_str(&format!("，当前窗口 {n} 条"));
        }
        s.push('）');
        if self.skipped > 0 {
            s.push_str(&format!("；{} 条定位不到工作区，留在原账号", self.skipped));
        }
        s
    }
}

fn wal_path(db: &Path) -> PathBuf {
    PathBuf::from(format!("{}-wal", db.display()))
}

fn shm_path(db: &Path) -> PathBuf {
    PathBuf::from(format!("{}-shm", db.display()))
}

fn norm_folder(s: &str) -> String {
    s.trim().replace('/', "\\").trim_end_matches('\\').to_lowercase()
}

fn folder_key(single: Option<&str>, folders_json: Option<&str>) -> String {
    if let Some(f) = single {
        let n = norm_folder(f);
        if !n.is_empty() {
            return n;
        }
    }
    if let Some(j) = folders_json {
        if let Ok(arr) = serde_json::from_str::<Vec<String>>(j) {
            let mut v: Vec<String> = arr
                .into_iter()
                .map(|s| norm_folder(&s))
                .filter(|s| !s.is_empty())
                .collect();
            v.sort();
            if !v.is_empty() {
                return v.join("|");
            }
        }
        let n = norm_folder(j);
        if n != "[]" {
            return n;
        }
    }
    String::new()
}

fn unsalt_biz(biz: &str) -> &str {
    biz.split(BIZ_SALT).next().unwrap_or(biz)
}

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn percent_decode(s: &str) -> String {
    let mut out = Vec::new();
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let Ok(v) = u8::from_str_radix(std::str::from_utf8(&b[i + 1..i + 3]).unwrap_or(""), 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// VS Code 的 fsPath 形态：反斜杠、盘符小写，其余大小写保持原样（参与 workspace.id 计算）
fn vscode_fs_path(p: &str) -> String {
    let p = p.trim().replace('/', "\\");
    let b = p.as_bytes();
    if b.len() >= 2 && b[1] == b':' && b[0].is_ascii_alphabetic() {
        let mut s = String::with_capacity(p.len());
        s.push(b[0].to_ascii_lowercase() as char);
        s.push_str(&p[1..]);
        s
    } else {
        p
    }
}

fn fs_path_from_file_uri(uri: &str) -> Option<String> {
    let rest = uri.strip_prefix("file://")?.trim_start_matches('/');
    let path = vscode_fs_path(&percent_decode(rest));
    (!path.is_empty()).then_some(path)
}

fn md5_hex(data: &[u8]) -> Option<String> {
    use windows_sys::Win32::Security::Cryptography::{BCryptHash, BCRYPT_MD5_ALG_HANDLE};
    let mut out = [0u8; 16];
    let status = unsafe {
        BCryptHash(
            BCRYPT_MD5_ALG_HANDLE,
            std::ptr::null(),
            0,
            data.as_ptr(),
            data.len() as u32,
            out.as_mut_ptr(),
            out.len() as u32,
        )
    };
    (status == 0).then(|| out.iter().map(|b| format!("{b:02x}")).collect())
}

/// 本地文件夹的 workspace.id：md5(fsPath + 创建时间毫秒)。文件夹删了重建后 id 会变。
fn workspace_id(fs_path: &str) -> Option<String> {
    let created = std::fs::metadata(fs_path).ok()?.created().ok()?;
    let ms = created.duration_since(std::time::UNIX_EPOCH).ok()?.as_millis();
    md5_hex(format!("{}{ms}", vscode_fs_path(fs_path)).as_bytes())
}

fn current_workspace_fs_path(paths: &IdePaths) -> Option<String> {
    let raw = std::fs::read_to_string(paths.storage_json()).ok()?;
    let v: serde_json::Value = serde_json::from_str(raw.trim_start_matches('\u{feff}')).ok()?;
    v.pointer("/windowsState/lastActiveWindow/folder")
        .and_then(|x| x.as_str())
        .and_then(fs_path_from_file_uri)
}

#[derive(Clone)]
struct ProjRow {
    project_id: String,
    biz: String,
    kind: String,
    /// project 表自增 id：agent 解析 project 时按 created_at DESC, id DESC 取最新
    id: i64,
    created_at: i64,
    workspace_type: String,
    /// single_root_workspace_folder 原值（VS Code fsPath），算 workspace.id 用
    root: Option<String>,
    multi_id: Option<String>,
    /// 规范化文件夹键：特殊 project 按它对位
    folder: String,
    last_active: i64,
}

fn load_user_projects(conn: &Connection, user_id: &str) -> Result<Vec<ProjRow>, rusqlite::Error> {
    let mut stmt = conn.prepare(
        "SELECT project_id, biz_project_id, IFNULL(project_kind,''), single_root_workspace_folder,
                workspace_folders, multi_root_workspace_id, IFNULL(last_active_at,0),
                id, IFNULL(created_at,0), IFNULL(workspace_type,'single_root')
         FROM project WHERE user_id=?1 AND IFNULL(deleted_at,0)=0",
    )?;
    let rows = stmt.query_map(params![user_id], |row| {
        let root: Option<String> = row.get(3)?;
        let folders: Option<String> = row.get(4)?;
        Ok(ProjRow {
            project_id: row.get(0)?,
            biz: row.get::<_, Option<String>>(1)?.unwrap_or_default(),
            kind: row.get(2)?,
            folder: folder_key(root.as_deref(), folders.as_deref()),
            root: root.filter(|r| !r.trim().is_empty()),
            multi_id: row.get::<_, Option<String>>(5)?.filter(|m| !m.trim().is_empty()),
            last_active: row.get::<_, i64>(6).unwrap_or(0),
            id: row.get::<_, i64>(7).unwrap_or(0),
            created_at: row.get::<_, i64>(8).unwrap_or(0),
            workspace_type: row.get::<_, String>(9).unwrap_or_else(|_| "single_root".into()),
        })
    })?;
    rows.collect()
}

fn live_session_counts(conn: &Connection) -> Result<HashMap<String, u32>, rusqlite::Error> {
    let mut stmt =
        conn.prepare("SELECT project_id, COUNT(*) FROM chat_session WHERE deleted_at=0 GROUP BY project_id")?;
    let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, u32>(1)?)))?;
    rows.collect()
}

/// 新号在源 project 那个工作区会去 get_or_create 的 biz
fn target_biz(src: &ProjRow, to_user: &str, salted: bool) -> Option<String> {
    let ws = src
        .root
        .as_deref()
        .and_then(workspace_id)
        .or_else(|| src.multi_id.clone())
        .or_else(|| {
            let base = unsalt_biz(&src.biz);
            (!base.is_empty() && base != src.project_id).then(|| base.to_string())
        })?;
    Some(if salted { format!("{ws}{BIZ_SALT}{to_user}") } else { ws })
}

/// 源 project 的会话该落到新号的哪个 project；None = 新号在这个工作区还没有真 project，
/// 会话留在原账号。
///
/// 只按 biz 精确匹配：客户端的 project_id 来自 get_or_create_project_v2(biz)，本地 project 行
/// 是它的镜像。按文件夹猜一行、或自己造一行随机 project_id，都会挂到客户端永远不查询的
/// project 上，打开就 session not found。新号还没打开过这个文件夹时宁可跳过。
/// 全局助手、空窗口这类 project 不按工作区 id 取，按同类型同文件夹对位。
fn resolve_target(src: &ProjRow, to_user: &str, salted: bool, to_rows: &[ProjRow]) -> Option<String> {
    if src.kind == KIND_WORKSPACE {
        let biz = target_biz(src, to_user, salted)?;
        return to_rows
            .iter()
            .filter(|p| p.kind == src.kind && p.workspace_type == src.workspace_type && p.biz == biz)
            .max_by_key(|p| (p.created_at, p.id))
            .map(|p| p.project_id.clone());
    }
    to_rows
        .iter()
        .filter(|p| p.kind == src.kind && p.folder == src.folder)
        .max_by_key(|p| (p.created_at, p.id))
        .map(|p| p.project_id.clone())
}

fn rebind_plain(
    plain: &Path,
    from_user: &str,
    to_user: &str,
    current_fs_path: Option<&str>,
    salted: bool,
) -> Result<RebindReport, String> {
    let conn = Connection::open(plain).map_err(|e| e.to_string())?;
    conn.execute_batch("PRAGMA busy_timeout=5000; PRAGMA foreign_keys=OFF;")
        .map_err(|e| e.to_string())?;
    // 历史迁移把 deleted_at 写成 0，而 agent 的查询条件是 `deleted_at IS NULL`：
    // 这些行对 agent 完全不可见。先把 0 归一为 NULL，已被会话挂靠的旧行才能重新被解析到。
    let normalized = conn
        .execute("UPDATE project SET deleted_at=NULL WHERE deleted_at=0", [])
        .unwrap_or(0);
    if normalized > 0 {
        logger::log(
            Level::Info,
            Source::Switch,
            format!("已修复 {normalized} 条 deleted_at=0 的 project 行（agent 只认 NULL）"),
        );
    }
    let counts = live_session_counts(&conn).map_err(|e| e.to_string())?;
    let from_rows = load_user_projects(&conn, from_user).map_err(|e| e.to_string())?;
    let to_rows = load_user_projects(&conn, to_user).map_err(|e| e.to_string())?;
    let sources: Vec<(ProjRow, bool)> = from_rows
        .iter()
        .map(|p| (p.clone(), true))
        .chain(to_rows.iter().map(|p| (p.clone(), false)))
        .filter(|(p, _)| counts.get(&p.project_id).copied().unwrap_or(0) > 0)
        .collect();

    let mut report = RebindReport::default();
    let mut touched: BTreeSet<String> = BTreeSet::new();
    let tx = conn.unchecked_transaction().map_err(|e| e.to_string())?;
    for (src, from_owned) in &sources {
        let Some(target) = resolve_target(src, to_user, salted, &to_rows) else {
            // 新号在这个工作区还没有真 project：会话留在原账号，等它打开过该文件夹再搬
            if *from_owned {
                report.skipped += counts.get(&src.project_id).copied().unwrap_or(0);
            }
            continue;
        };
        if target == src.project_id {
            continue;
        }
        let n = tx
            .execute(
                "UPDATE chat_session SET project_id=?1 WHERE project_id=?2 AND deleted_at=0",
                params![target, src.project_id],
            )
            .map_err(|e| e.to_string())? as u32;
        if n == 0 {
            continue;
        }
        report.sessions += n;
        tx.execute(
            "UPDATE project SET last_active_at=MAX(IFNULL(last_active_at,0), ?1) WHERE project_id=?2",
            params![src.last_active, target],
        )
        .map_err(|e| e.to_string())?;
        touched.insert(target);
    }
    let now = now_secs();
    for target in &touched {
        // 旧版本库可能没有 session_project 表，失败不影响主表改挂
        let _ = tx.execute(
            "DELETE FROM session_project WHERE session_id IN (
                SELECT session_id FROM chat_session WHERE project_id=?1
             ) AND project_id!=?1",
            params![target],
        );
        let _ = tx.execute(
            "INSERT OR IGNORE INTO session_project (project_id, session_id, created_at)
             SELECT project_id, session_id, ?2 FROM chat_session WHERE project_id=?1 AND deleted_at=0",
            params![target, now],
        );
    }
    tx.commit().map_err(|e| e.to_string())?;
    report.projects_mapped = touched.len() as u32;
    report.current_window = current_fs_path.map(|fp| {
        let folder = norm_folder(fp);
        let ws = workspace_id(fp);
        to_rows
            .iter()
            .filter(|p| {
                if p.kind != KIND_WORKSPACE || p.folder != folder {
                    return false;
                }
                match (&ws, salted) {
                    (Some(w), true) => p.biz == format!("{w}{BIZ_SALT}{to_user}"),
                    (Some(w), false) => p.biz == *w,
                    _ => true,
                }
            })
            .max_by_key(|p| (p.created_at, p.id))
            .and_then(|p| {
                conn.query_row(
                    "SELECT COUNT(*) FROM chat_session WHERE project_id=?1 AND deleted_at=0",
                    params![p.project_id],
                    |r| r.get::<_, u32>(0),
                )
                .ok()
            })
            .unwrap_or(0)
    });
    conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")
        .map_err(|e| e.to_string())?;
    Ok(report)
}

/// 加密库副本 → 明文（work/plain.db）→ 合并 WAL → 改挂 → 重新加密到 enc_new
fn rewrite_copy(
    enc: &Path,
    wal: Option<&Path>,
    work: &Path,
    enc_new: &Path,
    key: &[u8; 32],
    from_user: &str,
    to_user: &str,
    current_fs_path: Option<&str>,
    salted: bool,
) -> Result<RebindReport, String> {
    let plain = work.join("plain.db");
    let cipher_salt = sqlcipher::decrypt_file(enc, &plain, key).map_err(|e| e.to_string())?;
    if let Some(wal) = wal.filter(|w| w.exists()) {
        match sqlcipher::apply_wal(&plain, wal, key, &cipher_salt) {
            Ok(n) if n > 0 => logger::log(
                Level::Info,
                Source::Switch,
                format!("已合并对话库 WAL（{n} 次提交）"),
            ),
            Ok(_) => {}
            Err(e) => logger::log(
                Level::Warn,
                Source::Switch,
                format!("WAL 未合并（{e}），使用主库快照"),
            ),
        }
    }
    let report = rebind_plain(&plain, from_user, to_user, current_fs_path, salted)?;
    sqlcipher::encrypt_file(&plain, enc_new, key, &cipher_salt).map_err(|e| e.to_string())?;
    let _ = std::fs::remove_file(&plain);
    Ok(report)
}

/// 改写对话库（调用方保证此刻没有 agent 打开它）。新库先在同目录写好再整体替换：
/// 中途失败或 Trae 等超时自行启动 agent，都读不到写了一半的库。
fn rewrite_live(
    live: &Path,
    key: &[u8; 32],
    from_user: &str,
    to_user: &str,
    current_fs_path: Option<&str>,
    salted: bool,
) -> Result<RebindReport, String> {
    let tmp = std::env::temp_dir().join(format!("tsw-rebind-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).map_err(|e| e.to_string())?;
    let staged = live.with_file_name("database.db.tsw-new");
    let run = || -> Result<RebindReport, String> {
        let enc_copy = tmp.join("live.enc");
        let wal_copy = tmp.join("live.wal");
        agent_key::copy_exclusive(live, &enc_copy)?;
        let wal = wal_path(live);
        let wal_copied = wal.exists() && std::fs::copy(&wal, &wal_copy).is_ok();
        let report = rewrite_copy(
            &enc_copy,
            wal_copied.then_some(wal_copy.as_path()),
            &tmp,
            &staged,
            key,
            from_user,
            to_user,
            current_fs_path,
            salted,
        )?;
        std::fs::rename(&staged, live).map_err(|e| format!("替换对话库失败: {e}"))?;
        let _ = std::fs::remove_file(wal_path(live));
        let _ = std::fs::remove_file(shm_path(live));
        Ok(report)
    };
    let result = run();
    let _ = std::fs::remove_file(&staged);
    let _ = std::fs::remove_dir_all(&tmp);
    result
}

/// 按原工作区把 from/to 两号的会话接到新号并写回加密库。
/// 只在 Trae 未运行时调用：改挂要独占对话库，运行中的 agent 一直持有句柄；
/// 为改挂而停一次对话服务会让每次换号都重启一次 agent，触发服务端 4011「操作过于频繁」，
/// 面板也会在重启空窗里落到「服务启动异常」。
pub fn on_switch(ide: IdeKind, from_user: &str, to_user: &str) -> Result<RebindReport, String> {
    if from_user == to_user {
        return Ok(RebindReport::default());
    }
    let paths = IdePaths::detect(ide).ok_or("未检测到安装")?;
    if super::launcher::is_running(&paths) {
        return Err("Trae 运行中不改挂会话（不改挂需停一次对话服务）".into());
    }
    let live = paths.agent_database();
    if !live.exists() {
        return Err("未找到对话库".into());
    }
    let current = current_workspace_fs_path(&paths);
    let salted = super::inject::biz_salt_active(&paths);
    let key = agent_key::ensure_key(&paths)?;
    rewrite_live(&live, &key, from_user, to_user, current.as_deref(), salted)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_dir(tag: &str) -> PathBuf {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join(format!("rebind-test-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn schema(conn: &Connection) {
        conn.execute_batch(
            "CREATE TABLE project (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                project_id TEXT NOT NULL UNIQUE,
                user_id TEXT,
                biz_project_id TEXT,
                project_kind TEXT,
                single_root_workspace_folder TEXT,
                workspace_folders TEXT,
                multi_root_workspace_id TEXT,
                workspace_type TEXT NOT NULL DEFAULT 'single_root',
                created_at INTEGER DEFAULT (strftime('%s','now')),
                updated_at INTEGER,
                last_active_at INTEGER,
                deleted_at INTEGER DEFAULT 0,
                UNIQUE (biz_project_id, user_id)
             );
             CREATE TABLE chat_session (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                session_id TEXT NOT NULL UNIQUE,
                project_id TEXT,
                deleted_at INTEGER DEFAULT 0
             );
             CREATE TABLE session_project (
                project_id TEXT, session_id TEXT, created_at INTEGER,
                PRIMARY KEY (project_id, session_id)
             );",
        )
        .unwrap();
    }

    fn project(conn: &Connection, pid: &str, user: &str, biz: &str, kind: &str, root: &str) {
        conn.execute(
            "INSERT INTO project (project_id,user_id,biz_project_id,project_kind,single_root_workspace_folder,last_active_at)
             VALUES (?1,?2,?3,?4,?5,100)",
            params![pid, user, biz, kind, root],
        )
        .unwrap();
    }

    fn session(conn: &Connection, sid: &str, pid: &str) {
        conn.execute(
            "INSERT INTO chat_session (session_id, project_id) VALUES (?1, ?2)",
            params![sid, pid],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO session_project (project_id, session_id, created_at) VALUES (?1, ?2, 1)",
            params![pid, sid],
        )
        .unwrap();
    }

    fn owner_of(conn: &Connection, sid: &str) -> (String, String, String) {
        conn.query_row(
            "SELECT p.project_id, p.user_id, p.biz_project_id FROM chat_session s
             JOIN project p ON p.project_id = s.project_id WHERE s.session_id=?1",
            params![sid],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap()
    }

    #[test]
    fn md5_known_vectors() {
        assert_eq!(md5_hex(b"").unwrap(), "d41d8cd98f00b204e9800998ecf8427e");
        assert_eq!(md5_hex(b"abc").unwrap(), "900150983cd24fb0d6963f7d28e17f72");
    }

    #[test]
    fn fs_path_matches_vscode_form() {
        assert_eq!(
            fs_path_from_file_uri("file:///e%3A/Trea/trae_switcher").as_deref(),
            Some("e:\\Trea\\trae_switcher")
        );
        assert_eq!(vscode_fs_path("E:/Trea/A b"), "e:\\Trea\\A b");
    }

    fn fixture(salted: bool) -> (PathBuf, PathBuf, String, String, String) {
        let dir = tmp_dir(if salted { "salted" } else { "plain" });
        let x = dir.join("ws-x");
        let y = dir.join("ws-y");
        std::fs::create_dir_all(&x).unwrap();
        std::fs::create_dir_all(&y).unwrap();
        let x = vscode_fs_path(&x.to_string_lossy());
        let y = vscode_fs_path(&y.to_string_lossy());
        let wy = workspace_id(&y).unwrap();
        let db = dir.join("plain.db");
        let conn = Connection::open(&db).unwrap();
        schema(&conn);
        // A：X 上的旧 biz（文件夹重建前的 id）、Y 上的未加盐 biz、全局助手
        project(&conn, "pA1", "A", "stale-x", KIND_WORKSPACE, &x);
        project(&conn, "pA2", "A", &wy, KIND_WORKSPACE, &y);
        project(&conn, "pA3", "A", "pA3", "global_assistant", "c:\\assistant");
        session(&conn, "s1", "pA1");
        session(&conn, "s2", "pA1");
        session(&conn, "s3", "pA2");
        session(&conn, "s4", "pA3");
        // B：Y 上已是新号会查的 project；X 上一条 biz=project_id 的孤儿行；全局助手
        let by = if salted { format!("{wy}{BIZ_SALT}B") } else { wy.clone() };
        project(&conn, "pB1", "B", &by, KIND_WORKSPACE, &y);
        project(&conn, "pB4", "B", "pB4", KIND_WORKSPACE, &x);
        project(&conn, "pB3", "B", "pB3", "global_assistant", "c:\\assistant");
        session(&conn, "s5", "pB1");
        session(&conn, "s6", "pB4");
        // 无关账号 C 不动
        project(&conn, "pC1", "C", "c-biz", KIND_WORKSPACE, &x);
        session(&conn, "s7", "pC1");
        drop(conn);
        (dir, db, x, y, by)
    }

    #[test]
    fn rebinds_per_workspace_with_salt() {
        let (dir, db, x, _y, by) = fixture(true);
        let report = rebind_plain(&db, "A", "B", Some(&x), true).unwrap();
        let conn = Connection::open(&db).unwrap();

        // X 上 B 只有 biz=project_id 的孤儿行：biz 对不上就跳过，不按文件夹猜、不造 project_id
        assert_eq!(owner_of(&conn, "s1").0, "pA1");
        assert_eq!(owner_of(&conn, "s2").0, "pA1");
        assert_eq!(owner_of(&conn, "s6").0, "pB4");
        let (pid3, user3, biz3) = owner_of(&conn, "s3");
        assert_eq!((pid3.as_str(), user3.as_str(), biz3.as_str()), ("pB1", "B", by.as_str()));
        assert_eq!(owner_of(&conn, "s4").0, "pB3");
        assert_eq!(owner_of(&conn, "s5").0, "pB1");
        assert_eq!(owner_of(&conn, "s7").0, "pC1");

        let stale_links: u32 = conn
            .query_row(
                "SELECT COUNT(*) FROM session_project sp JOIN chat_session s ON s.session_id=sp.session_id
                 WHERE sp.project_id != s.project_id AND s.session_id != 's7'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(stale_links, 0);

        assert_eq!(report.sessions, 2);
        assert_eq!(report.projects_mapped, 2);
        assert_eq!(report.skipped, 2);
        assert_eq!(report.current_window, Some(0));
        drop(conn);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn rebinds_without_salt_when_patch_missing() {
        let (dir, db, x, _y, _by) = fixture(false);
        rebind_plain(&db, "A", "B", Some(&x), false).unwrap();
        let conn = Connection::open(&db).unwrap();
        // B 在 X 的孤儿行 biz=pB4 也对不上，s1 留在 A
        assert_eq!(owner_of(&conn, "s1").0, "pA1");
        assert_eq!(owner_of(&conn, "s3").0, "pB1");
        drop(conn);
        let _ = std::fs::remove_dir_all(dir);
    }
}

#[cfg(test)]
mod live_tests {
    use super::*;

    const FROM: &str = "1910309375982379";
    const TO: &str = "2024614372057156";

    fn work_dir(tag: &str) -> PathBuf {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join(format!("func-tmp-{tag}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// 只读检查本机对话库：解密副本 + 合并 WAL + integrity_check，打印会话分布
    #[test]
    #[ignore = "reads the local Trae CN database"]
    fn live_db_integrity() {
        let Some(paths) = IdePaths::detect(IdeKind::TraeCn) else { return };
        let live = paths.agent_database();
        let Some(key) = agent_key::load_key(IdeKind::TraeCn) else { return };
        let dir = work_dir("integrity");
        let enc = dir.join("live.enc");
        std::fs::copy(&live, &enc).unwrap();
        let plain = dir.join("plain.db");
        let cs = sqlcipher::decrypt_file(&enc, &plain, &key).expect("decrypt live db");
        let wal = wal_path(&live);
        if wal.exists() {
            let w = dir.join("live.wal");
            std::fs::copy(&wal, &w).unwrap();
            eprintln!("wal commits merged: {:?}", sqlcipher::apply_wal(&plain, &w, &key, &cs));
        }
        let conn = Connection::open(&plain).unwrap();
        let check: String = conn.query_row("PRAGMA integrity_check", [], |r| r.get(0)).unwrap();
        eprintln!("integrity_check: {check}");
        let mut st = conn
            .prepare(
                "SELECT p.user_id, IFNULL(p.single_root_workspace_folder,''), p.biz_project_id, COUNT(s.session_id)
                 FROM chat_session s JOIN project p ON p.project_id=s.project_id
                 WHERE s.deleted_at=0 GROUP BY p.project_id ORDER BY 1, 2",
            )
            .unwrap();
        let rows = st
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, Option<String>>(2)?.unwrap_or_default(),
                    r.get::<_, u32>(3)?,
                ))
            })
            .unwrap();
        for (u, f, biz, n) in rows.flatten() {
            let salted = biz.contains(BIZ_SALT);
            eprintln!("{u:<18} salted={salted:<5} sessions={n:<4} {f}");
        }
        drop(st);
        drop(conn);
        assert_eq!(check, "ok");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 本机当前窗口的工作区 id 必须能在 workspaceStorage 下找到（验证 md5 公式与 Trae 一致）
    #[test]
    #[ignore = "reads the local Trae CN install"]
    fn workspace_id_matches_trae_storage() {
        let Some(paths) = IdePaths::detect(IdeKind::TraeCn) else { return };
        let Some(cur) = current_workspace_fs_path(&paths) else { return };
        let id = workspace_id(&cur).expect("workspace id");
        let storage = paths.user_data.join("User").join("workspaceStorage").join(&id);
        assert!(storage.exists(), "{cur} -> {id} not in workspaceStorage");
        eprintln!("{cur} -> {id}");
    }

    /// 在本机对话库副本上跑完整换号改挂（解密 → 合并 WAL → 改挂 → 加密），live 库不动
    #[test]
    #[ignore = "reads the local Trae CN database"]
    fn rebind_live_copy_end_to_end() {
        let Some(paths) = IdePaths::detect(IdeKind::TraeCn) else { return };
        let live = paths.agent_database();
        let Some(key) = agent_key::load_key(IdeKind::TraeCn) else { return };
        if !live.exists() {
            return;
        }
        let dir = work_dir("rebind");
        let enc = dir.join("live.enc");
        std::fs::copy(&live, &enc).unwrap();
        let wal = dir.join("live.wal");
        let has_wal = std::fs::copy(wal_path(&live), &wal).is_ok();

        // 改挂前快照：from/to 两号名下每条活会话的 (project 文件夹, 类型)
        let before_dir = dir.join("before");
        std::fs::create_dir_all(&before_dir).unwrap();
        let before = before_dir.join("plain.db");
        let cs = sqlcipher::decrypt_file(&enc, &before, &key).unwrap();
        if has_wal {
            sqlcipher::apply_wal(&before, &wal, &key, &cs).unwrap();
        }
        let snapshot = |db: &Path| -> HashMap<String, (String, String, String, String)> {
            let conn = Connection::open(db).unwrap();
            let mut st = conn
                .prepare(
                    "SELECT s.session_id, p.user_id, IFNULL(p.single_root_workspace_folder,''),
                            IFNULL(p.project_kind,''), p.biz_project_id
                     FROM chat_session s JOIN project p ON p.project_id=s.project_id
                     WHERE s.deleted_at=0",
                )
                .unwrap();
            let rows = st
                .query_map([], |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        (
                            r.get::<_, String>(1)?,
                            r.get::<_, String>(2)?,
                            r.get::<_, String>(3)?,
                            r.get::<_, Option<String>>(4)?.unwrap_or_default(),
                        ),
                    ))
                })
                .unwrap();
            rows.flatten().collect()
        };
        let pre = snapshot(&before);

        let cur = current_workspace_fs_path(&paths);
        let t0 = std::time::Instant::now();
        let enc_new = dir.join("new.enc");
        let report = rewrite_copy(
            &enc,
            has_wal.then_some(wal.as_path()),
            &dir,
            &enc_new,
            &key,
            FROM,
            TO,
            cur.as_deref(),
            true,
        )
        .unwrap();
        let took = t0.elapsed();

        // 新加密库必须能用同一把钥匙解开
        let after_dir = dir.join("after");
        std::fs::create_dir_all(&after_dir).unwrap();
        let after = after_dir.join("plain.db");
        sqlcipher::decrypt_file(&enc_new, &after, &key).expect("re-encrypted db decrypts");
        let post = snapshot(&after);

        assert_eq!(pre.len(), post.len(), "live session count changed");
        let mut moved = 0u32;
        for (sid, (u0, folder0, kind0, _)) in &pre {
            let (u1, folder1, kind1, biz1) = &post[sid];
            if u0 != FROM && u0 != TO {
                assert_eq!(u1, u0, "{sid} of other user moved");
                continue;
            }
            if u1 != TO {
                continue; // 定位不到工作区，留在原号
            }
            assert_eq!(folder1, folder0, "{sid} changed folder");
            assert_eq!(kind1, kind0, "{sid} changed kind");
            if kind1 == KIND_WORKSPACE {
                if let Some(ws) = workspace_id(folder1) {
                    assert_eq!(biz1, &format!("{ws}{BIZ_SALT}{TO}"), "{sid} not on visible project");
                }
            }
            if pre[sid] != post[sid] {
                moved += 1;
            }
        }
        eprintln!("{}", report.summary());
        eprintln!(
            "moved {moved} sessions, report {:?}, rewrite took {:.1}s",
            report,
            took.as_secs_f64()
        );
        assert_eq!(report.sessions, moved);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
