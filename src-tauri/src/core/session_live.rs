// 无感会话保留：热换号时用 SQLCipher 连接（agent 不停止）把其他账号在同一工作区下的
// 活会话改挂到当前账号的 project。归属链 chat_session.project_id → project.user_id，
// 客户端按 (user_id, 文件夹) 取 project，所以目标必须是当前账号在该文件夹下已存在的活行；
// 目标还没建立就等它出现（换号后客户端会强制重建 project），等不到就留在原账号。
use super::agent_key;
use super::logger::{self, Level, Source};
use super::paths::{IdeKind, IdePaths};
use super::sqlcipher_ffi::Db;
use std::time::{Duration, Instant};

#[derive(Debug, Default, Clone)]
pub struct LiveReport {
    pub sessions: u32,
    pub projects: u32,
    pub accounts: u32,
    pub target: String,
}

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
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

/// 当前窗口的工作区文件夹（与 DB 里 single_root_workspace_folder 同形：反斜杠、盘符小写）
fn current_workspace_folder(paths: &IdePaths) -> Option<String> {
    let raw = std::fs::read_to_string(paths.storage_json()).ok()?;
    let v: serde_json::Value = serde_json::from_str(raw.trim_start_matches('\u{feff}')).ok()?;
    let uri = v.pointer("/windowsState/lastActiveWindow/folder")?.as_str()?;
    let rest = uri.strip_prefix("file://")?.trim_start_matches('/');
    let p = percent_decode(rest).replace('/', "\\");
    let b = p.as_bytes();
    if b.len() >= 2 && b[1] == b':' && b[0].is_ascii_alphabetic() {
        let mut s = String::with_capacity(p.len());
        s.push(b[0].to_ascii_lowercase() as char);
        s.push_str(&p[1..]);
        return (!s.is_empty()).then_some(s);
    }
    (!p.is_empty()).then_some(p)
}

/// SQL 字面量（路径里可能有单引号）
fn q(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

/// 数据库里的工作区列做同形比较（大小写与斜杠不敏感）
fn folder_expr() -> &'static str {
    "LOWER(REPLACE(IFNULL(single_root_workspace_folder,''),'/','\\'))"
}

fn find_target(db: &Db, to_user: &str, folder: &str) -> Result<Option<String>, String> {
    let sql = format!(
        "SELECT project_id FROM project \
         WHERE user_id={u} AND deleted_at IS NULL AND IFNULL(project_kind,'')='workspace' \
           AND {f}={folder} \
         ORDER BY created_at DESC, id DESC LIMIT 1;",
        u = q(to_user),
        f = folder_expr(),
        folder = q(&folder.to_lowercase())
    );
    db.one(&sql)
}

fn sources(db: &Db, to_user: &str, folder: &str) -> Result<Vec<(String, u32, i64)>, String> {
    let sql = format!(
        "SELECT p.project_id, COUNT(*), MAX(IFNULL(p.last_active_at,0)) \
         FROM project p JOIN chat_session s ON s.project_id=p.project_id \
         WHERE s.deleted_at=0 AND p.deleted_at IS NULL AND p.user_id<>{u} \
           AND {f}={folder} \
         GROUP BY p.project_id;",
        u = q(to_user),
        f = folder_expr(),
        folder = q(&folder.to_lowercase())
    );
    Ok(db
        .query(&sql)?
        .into_iter()
        .filter_map(|r| {
            Some((
                r.first()?.clone(),
                r.get(1)?.parse::<u32>().ok()?,
                r.get(2).and_then(|s| s.parse::<i64>().ok()).unwrap_or(0),
            ))
        })
        .collect())
}

/// 把其他账号在同一工作区的会话改挂到 to_user 的 project（wait 内等目标 project 出现）
pub fn rebind(ide: IdeKind, to_user: &str, wait: Duration) -> Result<LiveReport, String> {
    let paths = IdePaths::detect(ide).ok_or("未检测到 Trae 安装")?;
    let db_path = paths.agent_database();
    if !db_path.is_file() {
        return Err("未找到 AI 对话库".into());
    }
    let folder = current_workspace_folder(&paths).ok_or("读不到当前窗口工作区")?;
    let key = agent_key::load_key(ide).ok_or("尚未采集对话库密钥")?;
    let db = Db::open_live(&db_path, &key)?;

    let start = Instant::now();
    let target = loop {
        if let Some(t) = find_target(&db, to_user, &folder)? {
            break t;
        }
        if start.elapsed() >= wait {
            return Err(format!("当前账号尚未在该工作区建立项目（{folder}）"));
        }
        std::thread::sleep(Duration::from_millis(400));
    };

    let srcs = sources(&db, to_user, &folder)?;
    let mut report = LiveReport {
        target: target.clone(),
        ..Default::default()
    };
    if srcs.is_empty() {
        return Ok(report);
    }

    let mut tx: Vec<String> = vec!["BEGIN IMMEDIATE;".into()];
    let mut accounts = std::collections::BTreeSet::new();
    let mut last_active = 0i64;
    for (src, count, la) in &srcs {
        let users = db.query(&format!(
            "SELECT IFNULL(user_id,'') FROM project WHERE project_id={};",
            q(src)
        ))?;
        for row in users {
            if let Some(u) = row.into_iter().next() {
                if !u.is_empty() {
                    accounts.insert(u);
                }
            }
        }
        last_active = last_active.max(*la);
        tx.push(format!(
            "UPDATE chat_session SET project_id={t} WHERE project_id={s} AND deleted_at=0;",
            t = q(&target),
            s = q(src)
        ));
        report.sessions += count;
        report.projects += 1;
    }
    tx.push(format!(
        "UPDATE project SET last_active_at=MAX(IFNULL(last_active_at,0), {la}) WHERE project_id={t};",
        la = last_active,
        t = q(&target)
    ));
    let now = now_secs();
    tx.push(format!(
        "DELETE FROM session_project WHERE session_id IN (SELECT session_id FROM chat_session WHERE project_id={t}) AND project_id<>{t};",
        t = q(&target)
    ));
    tx.push(format!(
        "INSERT OR IGNORE INTO session_project (project_id, session_id, created_at) \
         SELECT project_id, session_id, {now} FROM chat_session WHERE project_id={t} AND deleted_at=0;",
        t = q(&target)
    ));
    tx.push("COMMIT;".into());

    if let Err(e) = db.exec(&tx.join("\n")) {
        let _ = db.exec("ROLLBACK;");
        return Err(format!("改挂失败已回滚：{e}"));
    }
    report.accounts = accounts.len() as u32;
    logger::log(
        Level::Info,
        Source::Switch,
        format!(
            "无感会话保留：{} 个账号的 {} 条会话已挂到当前账号（项目 {}）",
            report.accounts, report.sessions, report.target
        ),
    );
    Ok(report)
}
