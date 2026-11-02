// 会话迁移（独立功能）：把本机其他账号保存在对话库里的会话，按原工作区改挂到当前登录账号，
// 原账号不再持有。归属链是 chat_session.project_id → project.user_id。
//
// 目标 project 按**工作区文件夹**对位：当前版本的客户端调 get_or_create_project_v2 时对单根工作区
// 不传 biz_project_id，agent 按 (user_id, 文件夹) 去重，每个账号每个工作区只有一个活 project
// （本机实测 95 组 (user, folder) 无一重复），客户端打开该工作区拿到的就是这一行。
// 当前账号没打开过该文件夹（没有活行）就跳过，不造 project_id —— 挂到客户端永不查询的行上
// 等于会话彻底失联。
//
// 只在 Trae 未运行时执行：改写要独占对话库，运行中的 agent 一直持有句柄。
use super::agent_key;
use super::logger::{self, Level, Source};
use super::paths::{IdeKind, IdePaths};
use super::sqlcipher;
use rusqlite::{params, Connection};
use serde::Serialize;
use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};

const BIZ_SALT: &str = "__tsw_u_";
const KIND_WORKSPACE: &str = "workspace";

#[derive(Debug, Default, Clone, Serialize)]
pub struct TransferReport {
    /// 迁移后的归属账号（当前登录账号）
    pub to_name: String,
    /// 有会话被迁走的源账号数
    pub accounts: u32,
    pub sessions: u32,
    /// 会话落到的目标 project 数
    pub projects: u32,
    /// 定位不到当前账号对应工作区、留在原账号的会话数
    pub skipped: u32,
    /// 迁移前的整库备份目录
    pub backup: String,
}

impl TransferReport {
    pub fn summary(&self) -> String {
        if self.sessions == 0 {
            return if self.skipped == 0 {
                "没有其他账号的会话可迁移".into()
            } else {
                format!("没有可迁移的会话；{} 条会话在当前账号下找不到对应工作区", self.skipped)
            };
        }
        let mut s = format!(
            "已把 {} 个账号的 {} 条会话迁移到「{}」（{} 个工作区）",
            self.accounts, self.sessions, self.to_name, self.projects
        );
        if self.skipped > 0 {
            s.push_str(&format!("；{} 条找不到当前账号下的对应工作区，留在原账号", self.skipped));
        }
        s
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct BackupEntry {
    pub path: String,
    pub mtime_ms: u64,
    pub size: u64,
}

/// 可迁移会话（其他账号名下的本地会话）：供「只迁选中的会话」列表使用
#[derive(Debug, Clone, Serialize)]
pub struct Candidate {
    pub session_id: String,
    pub title: String,
    pub updated_at: i64,
    pub project_id: String,
    pub user_id: String,
    /// 归属账号名（账号库里查不到时回落 userId）
    pub account: String,
    /// 会话所在工作区文件夹
    pub workspace: String,
    /// 当前账号在该工作区是否已有活 project（false = 这次迁不过去）
    pub migratable: bool,
}

fn lit(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

fn in_list(ids: &BTreeSet<String>) -> String {
    ids.iter().map(|s| lit(s)).collect::<Vec<_>>().join(",")
}

/// 账号库里的 userId → 账号名
fn account_names() -> HashMap<String, String> {
    super::vault::load_json()
        .ok()
        .flatten()
        .and_then(|v| v.get("accounts").and_then(|a| a.as_array()).cloned())
        .map(|arr| {
            arr.iter()
                .filter_map(|a| {
                    Some((
                        a.get("user_id")?.as_str()?.to_string(),
                        a.get("name")?.as_str()?.to_string(),
                    ))
                })
                .collect()
        })
        .unwrap_or_default()
}

/// 列出其他账号的会话（运行中也可读，只读查询）；migratable 表示当前账号已在同一工作区建过项目
pub fn candidates(ide: IdeKind) -> Result<Vec<Candidate>, String> {
    let paths = IdePaths::detect(ide).ok_or("未检测到 Trae 安装")?;
    let live = paths.agent_database();
    if !live.is_file() {
        return Err("未找到 AI 对话库：请先启动 Trae 并打开一次对话面板".into());
    }
    let running = super::launcher::is_running(&paths);
    let key = match agent_key::load_key(ide).filter(|k| sqlcipher::verify_key(&live, k).is_ok()) {
        Some(k) => k,
        None if running => {
            let k = agent_key::capture_from_memory(&paths)
                .ok_or("未能从运行中的 Trae 采集到对话库密钥（请先打开对话面板再试）")?;
            agent_key::save_key(ide, &k)?;
            k
        }
        None => return Err("尚未获取对话库密钥：请先保持 Trae 运行并刷新本页".into()),
    };
    let cur = super::switch::scan_current(&ide)
        .map_err(|e| e.to_string())?
        .ok_or("当前未检测到登录账号，请先在 Trae 中登录")?;
    let db = super::sqlcipher_ffi::Db::open_live(&live, &key)?;

    let proj_rows = db.query(
        "SELECT project_id, IFNULL(biz_project_id,''), IFNULL(project_kind,''), IFNULL(single_root_workspace_folder,''), \
         IFNULL(workspace_folders,''), IFNULL(multi_root_workspace_id,''), IFNULL(last_active_at,0), id, \
         IFNULL(created_at,0), IFNULL(workspace_type,'single_root'), IFNULL(user_id,'') \
         FROM project WHERE IFNULL(deleted_at,0)=0;",
    )?;
    let mut projects: HashMap<String, ProjRow> = HashMap::new();
    for r in proj_rows {
        if r.len() < 11 {
            continue;
        }
        let root: Option<String> = (!r[3].is_empty()).then(|| r[3].clone());
        let folders: Option<String> = (!r[4].is_empty()).then(|| r[4].clone());
        projects.insert(
            r[0].clone(),
            ProjRow {
                user_id: r[10].clone(),
                project_id: r[0].clone(),
                biz: r[1].clone(),
                kind: r[2].clone(),
                id: r[7].parse().unwrap_or(0),
                created_at: r[8].parse().unwrap_or(0),
                workspace_type: r[9].clone(),
                multi_id: (!r[5].trim().is_empty()).then(|| r[5].clone()),
                folder: folder_key(root.as_deref(), folders.as_deref()),
                last_active: r[6].parse().unwrap_or(0),
            },
        );
    }
    let to_rows: Vec<ProjRow> = projects.values().filter(|p| p.user_id == cur.user_id).cloned().collect();
    let names = account_names();
    let sessions = db.query(
        "SELECT session_id, IFNULL(session_title,''), IFNULL(updated_at,0), project_id \
         FROM chat_session WHERE deleted_at=0 ORDER BY IFNULL(updated_at,0) DESC;",
    )?;
    let mut out = Vec::new();
    for r in sessions {
        if r.len() < 4 {
            continue;
        }
        let Some(p) = projects.get(&r[3]) else { continue };
        if p.user_id == cur.user_id || p.user_id.is_empty() {
            continue;
        }
        let target = resolve_target(p, &to_rows);
        out.push(Candidate {
            session_id: r[0].clone(),
            title: if r[1].trim().is_empty() {
                "未命名会话".into()
            } else {
                r[1].clone()
            },
            updated_at: r[2].parse().unwrap_or(0),
            project_id: p.project_id.clone(),
            user_id: p.user_id.clone(),
            account: names.get(&p.user_id).cloned().unwrap_or_else(|| p.user_id.clone()),
            workspace: p.folder.clone(),
            migratable: target.is_some(),
        });
    }
    Ok(out)
}

fn slot(ide: IdeKind) -> &'static str {
    match ide {
        IdeKind::Trae => "trae",
        IdeKind::TraeCn => "trae-cn",
    }
}

fn wal_path(db: &Path) -> PathBuf {
    PathBuf::from(format!("{}-wal", db.display()))
}

fn shm_path(db: &Path) -> PathBuf {
    PathBuf::from(format!("{}-shm", db.display()))
}

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn norm_folder(s: &str) -> String {
    s.trim()
        .replace('/', "\\")
        .trim_end_matches('\\')
        .to_lowercase()
}

/// 文件夹规范化键：单根取文件夹；多根取排序后的文件夹集合
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

/// 旧版本 inject ⑬ 会把用户后缀拼进 biz / multi_root_workspace_id
fn unsalt(biz: &str) -> &str {
    biz.split(BIZ_SALT).next().unwrap_or(biz)
}

#[derive(Clone)]
struct ProjRow {
    user_id: String,
    project_id: String,
    biz: String,
    kind: String,
    /// project 表自增 id：agent 解析 project 时按 created_at DESC, id DESC 取最新
    id: i64,
    created_at: i64,
    workspace_type: String,
    multi_id: Option<String>,
    /// 规范化文件夹键：按它对位目标 project
    folder: String,
    last_active: i64,
}

fn load_projects(conn: &Connection) -> Result<Vec<ProjRow>, rusqlite::Error> {
    let mut stmt = conn.prepare(
        "SELECT project_id, biz_project_id, IFNULL(project_kind,''), single_root_workspace_folder,
                workspace_folders, multi_root_workspace_id, IFNULL(last_active_at,0),
                id, IFNULL(created_at,0), IFNULL(workspace_type,'single_root'), IFNULL(user_id,'')
         FROM project WHERE IFNULL(deleted_at,0)=0",
    )?;
    let rows = stmt.query_map([], |row| {
        let root: Option<String> = row.get(3)?;
        let folders: Option<String> = row.get(4)?;
        Ok(ProjRow {
            user_id: row.get(10)?,
            project_id: row.get(0)?,
            biz: row.get::<_, Option<String>>(1)?.unwrap_or_default(),
            kind: row.get(2)?,
            folder: folder_key(root.as_deref(), folders.as_deref()),
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

fn newest(rows: &[&ProjRow]) -> Option<String> {
    rows.iter()
        .max_by_key(|p| (p.created_at, p.id))
        .map(|p| p.project_id.clone())
}

/// 源 project 的会话该落到当前账号的哪个 project；None = 当前账号在这个工作区还没有活 project。
/// 主键是文件夹（与 agent 的 (user_id, 文件夹) 去重一致）；biz 只作旧库兜底
/// （旧版本 biz = workspace id，可能带 ⑬ 的用户后缀）。
fn resolve_target(src: &ProjRow, to_rows: &[ProjRow]) -> Option<String> {
    let same: Vec<&ProjRow> = to_rows
        .iter()
        .filter(|p| p.kind == src.kind && p.workspace_type == src.workspace_type)
        .collect();
    if !src.folder.is_empty() {
        let by_folder: Vec<&ProjRow> = same.iter().copied().filter(|p| p.folder == src.folder).collect();
        if let Some(id) = newest(&by_folder) {
            return Some(id);
        }
    }
    if src.kind == KIND_WORKSPACE {
        if let Some(ws) = src.multi_id.as_deref() {
            let ws = unsalt(ws);
            let by_multi: Vec<&ProjRow> = same
                .iter()
                .copied()
                .filter(|p| p.multi_id.as_deref().map(unsalt) == Some(ws))
                .collect();
            if let Some(id) = newest(&by_multi) {
                return Some(id);
            }
        }
    }
    let src_biz = unsalt(&src.biz);
    if !src_biz.is_empty() {
        let by_biz: Vec<&ProjRow> = same.iter().copied().filter(|p| p.biz == src_biz).collect();
        if let Some(id) = newest(&by_biz) {
            return Some(id);
        }
    }
    None
}

fn migrate_plain(plain: &Path, to_user: &str, only: Option<&BTreeSet<String>>) -> Result<TransferReport, String> {
    let conn = Connection::open(plain).map_err(|e| e.to_string())?;
    conn.execute_batch("PRAGMA busy_timeout=5000; PRAGMA foreign_keys=OFF;")
        .map_err(|e| e.to_string())?;
    // 历史迁移把 deleted_at 写成 0，而 agent 的查询条件是 `deleted_at IS NULL`：
    // 这些行对 agent 完全不可见，先归一为 NULL 才能被解析到。
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
    let rows = load_projects(&conn).map_err(|e| e.to_string())?;
    let to_rows: Vec<ProjRow> = rows.iter().filter(|p| p.user_id == to_user).cloned().collect();
    let sources: Vec<&ProjRow> = rows
        .iter()
        .filter(|p| p.user_id != to_user)
        .filter(|p| counts.get(&p.project_id).copied().unwrap_or(0) > 0)
        .collect();

    // 指定了会话集合时：只在该集合内统计、只改这些 session_id（其余原样留在原账号）
    let filter = only.filter(|s| !s.is_empty()).map(|s| format!(" AND session_id IN ({})", in_list(s)));
    let mut picked: HashMap<String, u32> = HashMap::new();
    if let Some(sel) = only {
        let sql = format!(
            "SELECT project_id, COUNT(*) FROM chat_session WHERE deleted_at=0 AND session_id IN ({}) GROUP BY project_id",
            in_list(sel)
        );
        let mut stmt = conn.prepare(&sql).map_err(|e| e.to_string())?;
        let mut rows = stmt
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, u32>(1)?)))
            .map_err(|e| e.to_string())?;
        while let Some(Ok((pid, n))) = rows.next() {
            picked.insert(pid, n);
        }
    }

    let mut report = TransferReport::default();
    let mut touched: BTreeSet<String> = BTreeSet::new();
    let mut moved_users: BTreeSet<String> = BTreeSet::new();
    let tx = conn.unchecked_transaction().map_err(|e| e.to_string())?;
    for src in &sources {
        let want = match only {
            Some(_) => picked.get(&src.project_id).copied().unwrap_or(0),
            None => counts.get(&src.project_id).copied().unwrap_or(0),
        };
        if want == 0 {
            continue;
        }
        let Some(target) = resolve_target(src, &to_rows) else {
            // 当前账号在这个工作区还没有活 project：会话留在原账号，等它打开过该文件夹再迁
            report.skipped += want;
            continue;
        };
        let n = tx
            .execute(
                &format!(
                    "UPDATE chat_session SET project_id=?1 WHERE project_id=?2 AND deleted_at=0{}",
                    filter.as_deref().unwrap_or("")
                ),
                params![target, src.project_id],
            )
            .map_err(|e| e.to_string())? as u32;
        if n == 0 {
            continue;
        }
        report.sessions += n;
        moved_users.insert(src.user_id.clone());
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
    report.projects = touched.len() as u32;
    report.accounts = moved_users.len() as u32;
    conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")
        .map_err(|e| e.to_string())?;
    Ok(report)
}

/// 加密库副本 → 明文 → 合并 WAL → 迁移 → 重新加密到 staged → 整体替换 live。
/// 新库先在同目录写好再替换：中途失败不会留下写了一半的库。
fn rewrite_live(
    live: &Path,
    key: &[u8; 32],
    to_user: &str,
    only: Option<&BTreeSet<String>>,
) -> Result<TransferReport, String> {
    let tmp = std::env::temp_dir().join(format!("tsw-transfer-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).map_err(|e| e.to_string())?;
    let staged = live.with_file_name("database.db.tsw-new");
    let run = || -> Result<TransferReport, String> {
        let enc = tmp.join("live.enc");
        let wal = tmp.join("live.wal");
        agent_key::copy_exclusive(live, &enc)?;
        let src_wal = wal_path(live);
        let has_wal = src_wal.exists() && std::fs::copy(&src_wal, &wal).is_ok();
        let plain = tmp.join("plain.db");
        let cipher_salt =
            sqlcipher::decrypt_file(&enc, &plain, key).map_err(|e| format!("解密对话库失败：{e}"))?;
        if has_wal {
            match sqlcipher::apply_wal(&plain, &wal, key, &cipher_salt) {
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
        let report = migrate_plain(&plain, to_user, only)?;
        sqlcipher::encrypt_file(&plain, &staged, key, &cipher_salt)
            .map_err(|e| format!("重新加密对话库失败：{e}"))?;
        let _ = std::fs::remove_file(&plain);
        std::fs::rename(&staged, live).map_err(|e| format!("替换对话库失败：{e}"))?;
        // 旧 WAL/SHM 已并入新库，残留会让 agent 读到旧页
        let _ = std::fs::remove_file(wal_path(live));
        let _ = std::fs::remove_file(shm_path(live));
        Ok(report)
    };
    let result = run();
    let _ = std::fs::remove_file(&staged);
    let _ = std::fs::remove_dir_all(&tmp);
    result
}

/// 迁移前整库备份：database.db 及 -wal/-shm 原样复制到 backup/agent-db-<ide>-<ts>/
fn backup_database(live: &Path, ide: IdeKind) -> Result<String, String> {
    let dir = super::vault::backup_dir().join(format!("agent-db-{}-{}", slot(ide), now_secs()));
    std::fs::create_dir_all(&dir).map_err(|e| format!("创建备份目录失败：{e}"))?;
    agent_key::copy_exclusive(live, &dir.join("database.db"))?;
    for (src, name) in [(wal_path(live), "database.db-wal"), (shm_path(live), "database.db-shm")] {
        if src.is_file() {
            std::fs::copy(&src, dir.join(name)).map_err(|e| format!("备份 {name} 失败：{e}"))?;
        }
    }
    Ok(dir.display().to_string())
}

/// 把其他账号的会话按原工作区迁到当前登录账号（调用方保证此刻没有 agent 打开对话库）。
/// `only` 为 None 表示整库迁移；Some(ids) 时只迁这些会话，其余原样留在原账号。
pub fn transfer(ide: IdeKind, only: Option<Vec<String>>) -> Result<TransferReport, String> {
    let paths = IdePaths::detect(ide).ok_or("未检测到 Trae 安装")?;
    let live = paths.agent_database();
    if !live.is_file() {
        return Err("未找到 AI 对话库：请先启动 Trae 并打开一次对话面板".into());
    }
    let running = super::launcher::is_running(&paths);
    let key = match agent_key::load_key(ide).filter(|k| sqlcipher::verify_key(&live, k).is_ok()) {
        Some(k) => k,
        None if running => {
            // 密钥只能从运行中的 agent 内存里取：本次点击先采集，迁移要等 Trae 退出后再点
            let k = agent_key::capture_from_memory(&paths)
                .ok_or("未能从运行中的 Trae 采集到对话库密钥（请先打开对话面板再试）")?;
            agent_key::save_key(ide, &k)?;
            logger::log(Level::Info, Source::Switch, "已采集对话库密钥（会话迁移用）");
            return Err("已获取对话库密钥：请完全退出 Trae，然后再次点击「开始迁移」".into());
        }
        None => {
            return Err("尚未获取对话库密钥：请先保持 Trae 运行并点击一次「开始迁移」，退出 Trae 后再迁移".into())
        }
    };
    if running {
        return Err("请先完全退出 Trae：迁移需要独占对话库".into());
    }
    let cur = super::switch::scan_current(&ide)
        .map_err(|e| e.to_string())?
        .ok_or("当前未检测到登录账号，请先在 Trae 中登录")?;
    let sel: Option<BTreeSet<String>> = only
        .map(|v| v.into_iter().filter(|s| !s.trim().is_empty()).collect())
        .filter(|s: &BTreeSet<String>| !s.is_empty());
    let backup = backup_database(&live, ide)?;
    let mut report = rewrite_live(&live, &key, &cur.user_id, sel.as_ref())
        .map_err(|e| format!("{e}（对话库未改动，备份：{backup}）"))?;
    report.to_name = if cur.name.trim().is_empty() { cur.user_id.clone() } else { cur.name.clone() };
    report.backup = backup;
    logger::log(
        Level::Info,
        Source::Switch,
        format!("会话迁移：{}", report.summary()),
    );
    Ok(report)
}

/// 该版本由本工具创建的对话库备份（新→旧）
pub fn list_backups(ide: IdeKind) -> Vec<BackupEntry> {
    let prefix = format!("agent-db-{}-", slot(ide));
    let mut out = Vec::new();
    if let Ok(entries) = std::fs::read_dir(super::vault::backup_dir()) {
        for e in entries.flatten() {
            let p = e.path();
            let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if !name.starts_with(&prefix) || !p.join("database.db").is_file() {
                continue;
            }
            let mtime_ms = e
                .metadata()
                .ok()
                .and_then(|m| m.modified().ok())
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_millis() as u64)
                .unwrap_or(0);
            let size = std::fs::metadata(p.join("database.db")).map(|m| m.len()).unwrap_or(0);
            out.push(BackupEntry {
                path: p.display().to_string(),
                mtime_ms,
                size,
            });
        }
    }
    out.sort_by(|a, b| b.mtime_ms.cmp(&a.mtime_ms));
    out
}

/// 回滚：用备份整体覆盖对话库（当前库另存为 database.db.pre-restore）
pub fn restore(ide: IdeKind, backup: &str) -> Result<String, String> {
    let paths = IdePaths::detect(ide).ok_or("未检测到 Trae 安装")?;
    if super::launcher::is_running(&paths) {
        return Err("请先完全退出 Trae，再恢复对话库".into());
    }
    let src = PathBuf::from(backup.trim());
    let name = src.file_name().and_then(|n| n.to_str()).unwrap_or("").to_string();
    if !name.starts_with(&format!("agent-db-{}-", slot(ide))) || !src.join("database.db").is_file() {
        return Err("不是本工具为该版本创建的对话库备份".into());
    }
    let live = paths.agent_database();
    if live.is_file() {
        let _ = std::fs::copy(&live, live.with_file_name("database.db.pre-restore"));
    }
    std::fs::copy(src.join("database.db"), &live).map_err(|e| format!("恢复对话库失败：{e}"))?;
    for (from, to) in [
        (src.join("database.db-wal"), wal_path(&live)),
        (src.join("database.db-shm"), shm_path(&live)),
    ] {
        if from.is_file() {
            std::fs::copy(&from, &to).map_err(|e| format!("恢复 {} 失败：{e}", to.display()))?;
        } else {
            let _ = std::fs::remove_file(&to);
        }
    }
    logger::log(
        Level::Warn,
        Source::Switch,
        format!("已用备份恢复对话库：{}", src.display()),
    );
    Ok(format!("已恢复对话库：{name}"))
}
