// 换号时把上一账号（及当前号）的本地会话接到新号「当前窗口」对应的 project。
// Trae 侧栏按 get_or_create_project_v2 的 biz_project_id（补丁 ⑬ 会加 __tsw_u_<uid>）取列表，
// 只按文件夹改挂会写到另一套 project，看起来像没效果。
use super::agent_key;
use super::logger::{self, Level, Source};
use super::paths::{IdeKind, IdePaths};
use super::sqlcipher;
use rand::RngCore;
use rusqlite::{params, params_from_iter, types::Value, Connection};
use std::path::{Path, PathBuf};

const BIZ_SALT: &str = "__tsw_u_";

#[derive(Debug, Default, Clone)]
pub struct RebindReport {
    pub sessions: u32,
    pub projects_mapped: u32,
    pub projects_created: u32,
}

impl RebindReport {
    pub fn summary(&self) -> String {
        if self.sessions == 0 {
            "没有可接到当前工作区的会话".into()
        } else {
            format!("已把 {} 条会话全部接到当前工作区", self.sessions)
        }
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

fn folder_key(single: Option<String>, folders_json: Option<String>) -> String {
    if let Some(f) = single {
        let n = norm_folder(&f);
        if !n.is_empty() {
            return n;
        }
    }
    if let Some(j) = folders_json {
        if let Ok(arr) = serde_json::from_str::<Vec<String>>(&j) {
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
        let n = norm_folder(&j);
        if n != "[]" {
            return n;
        }
    }
    String::new()
}

fn unsalt_biz(biz: &str) -> &str {
    biz.split(BIZ_SALT).next().unwrap_or(biz)
}

fn salted_biz(biz: &str, user_id: &str) -> String {
    format!("{}{BIZ_SALT}{user_id}", unsalt_biz(biz))
}

fn new_id() -> String {
    let mut b = [0u8; 12];
    rand::thread_rng().fill_bytes(&mut b);
    b.iter().map(|x| format!("{x:02x}")).collect()
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

fn folder_from_file_uri(uri: &str) -> Option<String> {
    let rest = uri.strip_prefix("file://")?;
    let rest = rest.trim_start_matches('/');
    let decoded = percent_decode(rest);
    let path = decoded.replace('/', "\\");
    let n = norm_folder(&path);
    if n.is_empty() {
        None
    } else {
        Some(n)
    }
}

fn current_workspace_folder(paths: &IdePaths) -> Option<String> {
    let raw = std::fs::read_to_string(paths.storage_json()).ok()?;
    let v: serde_json::Value = serde_json::from_str(raw.trim_start_matches('\u{feff}')).ok()?;
    if let Some(uri) = v
        .pointer("/windowsState/lastActiveWindow/folder")
        .and_then(|x| x.as_str())
    {
        if let Some(f) = folder_from_file_uri(uri) {
            return Some(f);
        }
    }
    v.pointer("/backupWorkspaces/folders/0/folderUri")
        .and_then(|x| x.as_str())
        .and_then(folder_from_file_uri)
}

struct ProjRow {
    project_id: String,
    biz: String,
    folder: String,
    last_active: i64,
}

fn load_user_projects(conn: &Connection, user_id: &str) -> Result<Vec<ProjRow>, rusqlite::Error> {
    let mut stmt = conn.prepare(
        "SELECT project_id, biz_project_id, single_root_workspace_folder, workspace_folders, IFNULL(last_active_at,0)
         FROM project WHERE user_id=?1 AND IFNULL(deleted_at,0)=0",
    )?;
    let rows = stmt.query_map(params![user_id], |row| {
        Ok(ProjRow {
            project_id: row.get(0)?,
            biz: row.get::<_, String>(1).unwrap_or_default(),
            folder: folder_key(row.get(2)?, row.get(3)?),
            last_active: row.get::<_, i64>(4).unwrap_or(0),
        })
    })?;
    rows.collect()
}

fn clone_project(conn: &Connection, from_pid: &str, to_user: &str, biz: &str) -> Result<String, rusqlite::Error> {
    let mut stmt = conn.prepare("SELECT * FROM project WHERE project_id=?1")?;
    let col_names: Vec<String> = stmt.column_names().iter().map(|s| (*s).to_string()).collect();
    let values: Vec<Value> = stmt.query_row(params![from_pid], |row| {
        let mut v = Vec::with_capacity(col_names.len());
        for i in 0..col_names.len() {
            v.push(row.get(i)?);
        }
        Ok(v)
    })?;
    drop(stmt);
    let new_pid = new_id();
    let mut names = Vec::new();
    let mut placeholders = Vec::new();
    let mut bind: Vec<Value> = Vec::new();
    for (name, val) in col_names.iter().zip(values.into_iter()) {
        if name == "id" {
            continue;
        }
        let v = if name == "project_id" {
            Value::Text(new_pid.clone())
        } else if name == "user_id" {
            Value::Text(to_user.to_string())
        } else if name == "biz_project_id" {
            Value::Text(biz.to_string())
        } else {
            val
        };
        names.push(name.clone());
        placeholders.push(format!("?{}", names.len()));
        bind.push(v);
    }
    let sql = format!(
        "INSERT INTO project ({}) VALUES ({})",
        names.join(","),
        placeholders.join(",")
    );
    match conn.execute(&sql, params_from_iter(bind.iter())) {
        Ok(_) => Ok(new_pid),
        Err(e) if e.to_string().contains("UNIQUE") => {
            if let Ok(existing) = conn.query_row(
                "SELECT project_id FROM project WHERE user_id=?1 AND biz_project_id=?2",
                params![to_user, biz],
                |row| row.get::<_, String>(0),
            ) {
                Ok(existing)
            } else {
                Err(e)
            }
        }
        Err(e) => Err(e),
    }
}

fn find_by_folder<'a>(rows: &'a [ProjRow], folder: &str) -> Option<&'a ProjRow> {
    if folder.is_empty() {
        return None;
    }
    rows.iter().find(|p| p.folder == folder)
}

fn find_by_salted_biz<'a>(rows: &'a [ProjRow], biz: &str) -> Option<&'a ProjRow> {
    rows.iter().find(|p| p.biz == *biz)
}

/// 新号在当前窗口会 get_or_create 的那个 project（biz 必须带 __tsw_u_<uid>）
fn ensure_visible_project(
    conn: &Connection,
    from_user: &str,
    to_user: &str,
    current_folder: Option<&str>,
) -> Result<(String, u32), String> {
    let to_rows = load_user_projects(conn, to_user).map_err(|e| e.to_string())?;
    let from_rows = load_user_projects(conn, from_user).map_err(|e| e.to_string())?;
    let folder = current_folder
        .map(str::to_string)
        .or_else(|| {
            from_rows
                .iter()
                .chain(to_rows.iter())
                .max_by_key(|p| p.last_active)
                .map(|p| p.folder.clone())
                .filter(|s| !s.is_empty())
        })
        .unwrap_or_default();

    let src = find_by_folder(&from_rows, &folder).or_else(|| find_by_folder(&to_rows, &folder));
    let want_biz = src
        .map(|p| salted_biz(&p.biz, to_user))
        .or_else(|| {
            find_by_folder(&to_rows, &folder).map(|p| salted_biz(&p.biz, to_user))
        });

    if let Some(biz) = want_biz.as_deref() {
        if let Some(hit) = find_by_salted_biz(&to_rows, biz) {
            return Ok((hit.project_id.clone(), 0));
        }
    }

    if let Some(hit) = find_by_folder(&to_rows, &folder) {
        if let Some(biz) = want_biz.as_deref() {
            match conn.execute(
                "UPDATE project SET biz_project_id=?1 WHERE project_id=?2",
                params![biz, hit.project_id],
            ) {
                Ok(_) => return Ok((hit.project_id.clone(), 0)),
                Err(e) if e.to_string().contains("UNIQUE") => {
                    if let Ok(existing) = conn.query_row(
                        "SELECT project_id FROM project WHERE user_id=?1 AND biz_project_id=?2",
                        params![to_user, biz],
                        |row| row.get::<_, String>(0),
                    ) {
                        return Ok((existing, 0));
                    }
                }
                Err(e) => return Err(e.to_string()),
            }
        }
        return Ok((hit.project_id.clone(), 0));
    }

    if let Some(src) = find_by_folder(&from_rows, &folder) {
        let biz = want_biz.unwrap_or_else(|| salted_biz(&src.biz, to_user));
        let id = clone_project(conn, &src.project_id, to_user, &biz).map_err(|e| e.to_string())?;
        return Ok((id, 1));
    }

    if let Some(best) = to_rows.iter().max_by_key(|p| p.last_active) {
        return Ok((best.project_id.clone(), 0));
    }
    if let Some(best) = from_rows.iter().max_by_key(|p| p.last_active) {
        let biz = salted_biz(&best.biz, to_user);
        let id = clone_project(conn, &best.project_id, to_user, &biz).map_err(|e| e.to_string())?;
        return Ok((id, 1));
    }
    Err("当前工作区没有可挂接的项目".into())
}

fn rebind_plain(
    plain: &Path,
    from_user: &str,
    to_user: &str,
    current_folder: Option<&str>,
) -> Result<RebindReport, String> {
    let conn = Connection::open(plain).map_err(|e| e.to_string())?;
    conn.execute_batch("PRAGMA busy_timeout=5000; PRAGMA foreign_keys=OFF;")
        .map_err(|e| e.to_string())?;
    let (target, created) = ensure_visible_project(&conn, from_user, to_user, current_folder)?;
    let n = conn
        .execute(
            "UPDATE chat_session SET project_id=?1
             WHERE deleted_at=0 AND project_id IN (
               SELECT project_id FROM project WHERE user_id IN (?2, ?3)
             )",
            params![target, from_user, to_user],
        )
        .map_err(|e| e.to_string())? as u32;
    let _ = conn.execute(
        "DELETE FROM session_project WHERE session_id IN (
            SELECT session_id FROM chat_session WHERE project_id=?1
         ) AND project_id!=?1",
        params![target],
    );
    let _ = conn.execute(
        "INSERT OR IGNORE INTO session_project (project_id, session_id, created_at)
         SELECT project_id, session_id, CAST(strftime('%s','now') AS INTEGER)
         FROM chat_session WHERE project_id=?1 AND deleted_at=0",
        params![target],
    );
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let _ = conn.execute(
        "UPDATE project SET last_active_at=?1 WHERE project_id=?2",
        params![now, target],
    );
    conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")
        .map_err(|e| e.to_string())?;
    drop(conn);
    Ok(RebindReport {
        sessions: n,
        projects_mapped: 1,
        projects_created: created,
    })
}

/// 停 agent → 合并 WAL → 把会话接到当前窗口 project → 写回加密库
pub fn on_switch(ide: IdeKind, from_user: &str, to_user: &str) -> Result<RebindReport, String> {
    if from_user == to_user {
        return Ok(RebindReport::default());
    }
    let paths = IdePaths::detect(ide).ok_or("未检测到安装")?;
    let live = paths.agent_database();
    if !live.exists() {
        return Err("未找到对话库".into());
    }
    let folder = current_workspace_folder(&paths);
    let key = agent_key::ensure_key(&paths)?;
    if super::launcher::is_running(&paths) {
        agent_key::pause_agent(&paths, &key)?;
    }
    let tmp = std::env::temp_dir().join(format!("tsw-rebind-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).map_err(|e| e.to_string())?;
    let enc_copy = tmp.join("live.enc");
    let wal_copy = tmp.join("live.wal");
    let plain = tmp.join("plain.db");
    let enc_new = tmp.join("new.enc");
    let cleanup = |dir: &Path| {
        let _ = std::fs::remove_dir_all(dir);
    };

    let run = || -> Result<RebindReport, String> {
        if !agent_key::wait_db_unlocked(&live, std::time::Duration::from_secs(2)) {
            agent_key::pause_agent(&paths, &key)?;
        }
        agent_key::copy_exclusive(&live, &enc_copy)?;
        let wal = wal_path(&live);
        if wal.exists() {
            let _ = std::fs::copy(&wal, &wal_copy);
        }
        let salt = sqlcipher::decrypt_file(&enc_copy, &plain, &key).map_err(|e| e.to_string())?;
        if wal_copy.exists() {
            match sqlcipher::apply_wal(&plain, &wal_copy, &key, &salt) {
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
        let report = rebind_plain(&plain, from_user, to_user, folder.as_deref())?;
        sqlcipher::encrypt_file(&plain, &enc_new, &key, &salt).map_err(|e| e.to_string())?;
        if super::launcher::is_running(&paths) {
            let _ = agent_key::pause_agent(&paths, &key);
        }
        agent_key::replace_exclusive(&live, &enc_new)?;
        let _ = std::fs::remove_file(wal_path(&live));
        let _ = std::fs::remove_file(shm_path(&live));
        Ok(report)
    };

    let result = run();
    cleanup(&tmp);
    result
}
