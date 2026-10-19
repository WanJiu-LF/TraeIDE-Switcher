// 自动换号引擎：按刷新间隔轮询当前账号额度，耗尽时顺序轮换到下一个可用账号
use super::auth::parse_auth_blob;
use super::logger::{log, Level, Source};
use super::model::{AccountBundle, AccountStatus};
use super::paths::IdeKind;
use super::quota::{summarize_ent_usage, PackQuota, QuotaClient};
use super::settings::{self, AppSettings};
use super::switch;
use std::time::{Duration, Instant};
use tauri::Emitter;

/// 一轮检查：返回 None（未耗尽）/ Some(下一账号)
fn find_exhausted_and_next(
    bundles: &[AccountBundle],
    ide: IdeKind,
    s: &AppSettings,
) -> Option<(String, String)> {
    let current = switch::scan_current(&ide).ok()??;
    let idx = bundles.iter().position(|b| b.id == current.id)?;
    let b = &bundles[idx];
    let usage = match account_usage(b) {
        // token 过期等场景不做续期（续期由查询命令负责），本轮跳过
        Some(u) => u,
        None => return None,
    };
    let packs = summarize_ent_usage(&usage);
    if !is_exhausted(&packs, ide, b.threshold.unwrap_or(s.threshold)) {
        return None;
    }
    // 候选：同版本 + 状态正常 + 排除自身；范围 pinned 时只取置顶账号
    let cand = |x: &AccountBundle| {
        x.ide == ide
            && x.status == AccountStatus::Normal
            && x.id != b.id
            && (s.switch_scope != "pinned" || x.pinned)
    };
    // order 策略：以当前账号在库中的位置为界，先扫其后的候选，再回绕扫其前的（轮换语义）
    let mut tail: Vec<&AccountBundle> = Vec::new();
    let mut head: Vec<&AccountBundle> = Vec::new();
    for (i, x) in bundles.iter().enumerate() {
        if !cand(x) {
            continue;
        }
        if i > idx {
            tail.push(x);
        } else {
            head.push(x);
        }
    }
    if tail.is_empty() && head.is_empty() {
        return None;
    }
    if s.switch_strategy == "remain" {
        // 剩余额度最多优先：候选逐个查剩余（上限 12 个，避免轮询里发起过多请求）
        let mut best: Option<(f64, &AccountBundle)> = None;
        for c in tail.iter().chain(head.iter()).take(12) {
            let Some(remain) = account_remain(c) else { continue };
            if remain <= 0.0 {
                continue;
            }
            if best.map_or(true, |(br, _)| remain > br) {
                best = Some((remain, c));
            }
        }
        return best.map(|(_, c)| (b.name.clone(), c.name.clone()));
    }
    for c in tail.iter().chain(head.iter()) {
        if account_has_quota(c) {
            return Some((b.name.clone(), c.name.clone()));
        }
    }
    None
}

/// 查询单账号额度（失败返回 None，不抛错）
fn account_usage(b: &AccountBundle) -> Option<serde_json::Value> {
    let info = parse_auth_blob(&b.auth_blob).ok()?;
    let client = QuotaClient::for_ide(b.ide, &region_of(&info));
    client.ent_usage(&info.token).ok()
}

/// 单账号剩余额度（remain 策略排序用；查询失败/资产缺失返回 None）
fn account_remain(b: &AccountBundle) -> Option<f64> {
    let packs = summarize_ent_usage(&account_usage(b)?);
    Some(super::quota::remain_of(&packs))
}

/// 判定额度耗尽：国际版按剩余百分比 <= 阈值；CN 积分制按总量归零
/// （threshold 为该账号生效阈值：账号独立阈值优先，否则全局阈值）
fn is_exhausted(packs: &[PackQuota], ide: IdeKind, threshold: f64) -> bool {
    if ide == IdeKind::TraeCn {
        let total: f64 = packs
            .iter()
            .filter(|p| !p.credits_unlimited)
            .map(|p| (p.credits_limit - p.credits_used).max(0.0))
            .sum();
        return total <= 0.0;
    }
    let mut any = false;
    for p in packs {
        for (limit, used) in [(p.basic_limit, p.basic_used), (p.bonus_limit, p.bonus_used)] {
            if limit <= 0.0 {
                continue;
            }
            any = true;
            let remain_pct = (limit - used) / limit * 100.0;
            if remain_pct <= threshold {
                return true;
            }
        }
    }
    !any
}

/// 目标账号是否还有可用额度（轻量：凭据有效性 + 未标记耗尽）
fn account_has_quota(b: &AccountBundle) -> bool {
    b.status == AccountStatus::Normal && parse_auth_blob(&b.auth_blob).is_ok()
}

fn load_bundles() -> Vec<AccountBundle> {
    crate::core::vault::load_json()
        .ok()
        .flatten()
        .and_then(|v| serde_json::from_value(v["accounts"].clone()).ok())
        .unwrap_or_default()
}

fn perform_switch(from_name: &str, to_name: &str, target: &AccountBundle, app: &tauri::AppHandle) {
    let msg_prefix = format!("自动换号：{from_name} → {to_name}");
    let t0 = Instant::now();
    let (ok, note) = match switch::hot_switch_full(&target.ide, target, &target.id) {
        Ok(_) => {
            log(Level::Info, Source::Switch, format!("{msg_prefix}（无感换号完成）"));
            let _ = app.emit("account-switched", serde_json::json!({ "to": to_name }));
            (true, String::new())
        }
        Err(e) => {
            log(
                Level::Err,
                Source::Switch,
                format!("{msg_prefix} 无感换号失败：{e}（下轮重试）"),
            );
            (false, e)
        }
    };
    super::history::record_switch(
        from_name,
        to_name,
        target.ide.label(),
        true,
        ok,
        t0.elapsed().as_millis() as u64,
        &note,
    );
}

/// 保活窗口：token 剩余有效期低于该值时对非当前账号发起预刷新
const KEEPALIVE_WINDOW_MS: i64 = 30 * 60 * 1000;

/// 主动保活预刷新：防止库中账号"静置到过期"。
/// · 跳过当前生效账号——IDE 自己会续期，双端轮换同一 refreshToken 链会互相作废；
/// · 跳过已过期账号——CN 实测 token 过期后 ExchangeToken 必被拒（TokenInvalid/040012），
///   此时只能重新登录采集，不做无效请求；
/// · 走 vault::update 全程持写锁，避免与并发额度查询的写入互相覆盖。
fn keepalive(window_ms: i64) {
    let active: Vec<String> = [IdeKind::Trae, IdeKind::TraeCn]
        .iter()
        .filter_map(|k| switch::scan_current(k).ok().flatten().map(|b| b.id))
        .collect();
    let _ = crate::core::vault::update(|v| {
        let mut bundles: Vec<AccountBundle> =
            serde_json::from_value(v["accounts"].clone()).unwrap_or_default();
        let mut expired = 0usize;
        let mut refreshed = 0usize;
        for b in bundles.iter_mut() {
            if active.contains(&b.id) || !switch::token_near_expiry(&b.auth_blob, window_ms) {
                continue;
            }
            if switch::token_near_expiry(&b.auth_blob, 0) {
                expired += 1;
                continue;
            }
            match super::refresh::refresh_auth_blob(&b.ide, &b.auth_blob) {
                Ok(nb) => {
                    b.auth_blob = nb;
                    refreshed += 1;
                    log(Level::Info, Source::Quota, format!("{} 保活预刷新成功", b.name));
                }
                Err(e) => log(
                    Level::Warn,
                    Source::Quota,
                    format!("{} 保活预刷新失败：{e}", b.name),
                ),
            }
        }
        if refreshed > 0 || expired > 0 {
            log(
                Level::Info,
                Source::Quota,
                format!("保活：预刷新 {refreshed} 个账号，另有 {expired} 个凭据已过期（需重新登录采集）"),
            );
        }
        v["accounts"] = serde_json::to_value(&bundles).unwrap_or_default();
    });
}

/// 每日定时任务：记录最近一次执行日期（本地日）。内存态即可——重启后可能重跑一次，
/// 而签到与刷新额度都幂等（同一设备槽重复签到会被服务端拒绝但不产生副作用）。
static LAST_DAILY: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

/// 本机时区偏移（分钟，东八区 = +480）：注册表 ActiveTimeBias 存的是 UTC = 本地 + bias
fn local_offset_minutes() -> i64 {
    use winreg::enums::{HKEY_LOCAL_MACHINE, KEY_READ};
    use winreg::RegKey;
    RegKey::predef(HKEY_LOCAL_MACHINE)
        .open_subkey_with_flags(r"SYSTEM\CurrentControlSet\Control\TimeZoneInformation", KEY_READ)
        .and_then(|k| k.get_value::<u32, _>("ActiveTimeBias"))
        .map(|raw| -(raw as i32) as i64)
        .unwrap_or(0)
}

fn region_of(info: &crate::core::auth::AuthInfo) -> String {
    info.user_region
        .as_ref()
        .map(|r| r.region.clone())
        .unwrap_or_default()
}

/// 刷新单账号额度并记一次采样（定时任务与趋势数据共用）
fn refresh_and_sample(b: &AccountBundle) -> Result<(), String> {
    let info = parse_auth_blob(&b.auth_blob).map_err(|e| e.to_string())?;
    let client = QuotaClient::for_ide(b.ide, &region_of(&info));
    let usage = client.ent_usage(&info.token).map_err(|e| e.to_string())?;
    let packs = summarize_ent_usage(&usage);
    super::history::record(&b.id, super::quota::remain_of(&packs), super::refresh::now_millis());
    Ok(())
}

/// 每日定点任务（P1-2）：到点后当天只执行一次
fn run_daily(s: &AppSettings) {
    if !s.daily_checkin && !s.daily_refresh {
        return;
    }
    // 本地时间：把 epoch 加上时区偏移后按 UTC 格式化，即可得到本地日期与时刻
    let iso = super::refresh::millis_to_iso(super::refresh::now_millis() + local_offset_minutes() * 60_000);
    let date = iso[0..10].to_string();
    let hh: i64 = iso[11..13].parse().unwrap_or(0);
    let mm: i64 = iso[14..16].parse().unwrap_or(0);
    let now_min = hh * 60 + mm;
    let parts: Vec<i64> = s.daily_time.split(':').filter_map(|x| x.trim().parse().ok()).collect();
    let target_min = parts.first().copied().unwrap_or(9) * 60 + parts.get(1).copied().unwrap_or(0);
    if now_min < target_min {
        return;
    }
    {
        let mut g = LAST_DAILY.lock().unwrap_or_else(|e| e.into_inner());
        if g.as_deref() == Some(date.as_str()) {
            return;
        }
        *g = Some(date);
    }

    let bundles = load_bundles();
    if bundles.is_empty() {
        return;
    }
    if s.daily_refresh {
        let mut ok = 0usize;
        for b in &bundles {
            if refresh_and_sample(b).is_ok() {
                ok += 1;
            }
        }
        log(
            Level::Info,
            Source::Quota,
            format!("定时刷新额度：{ok}/{} 成功", bundles.len()),
        );
    }
    if s.daily_checkin {
        let cn: Vec<&AccountBundle> = bundles
            .iter()
            .filter(|b| b.ide == IdeKind::TraeCn && b.status == AccountStatus::Normal)
            .collect();
        let mut ok = 0usize;
        for b in &cn {
            let Ok(info) = parse_auth_blob(&b.auth_blob) else { continue };
            let client = QuotaClient::for_ide(b.ide, &region_of(&info));
            let did = super::assets::device_slot(&b.user_id);
            match client.checkin_claim(&info.token, Some(&did)) {
                Ok(_) => {
                    ok += 1;
                    log(Level::Info, Source::Quota, format!("定时签到成功：{}", b.name));
                }
                Err(e) => log(Level::Warn, Source::Quota, format!("定时签到失败（{}）：{e}", b.name)),
            }
        }
        log(Level::Info, Source::Quota, format!("定时签到：{ok}/{} 成功", cn.len()));
    }
}

/// 后台循环：30s 一跳，按 refresh_interval 分钟执行一轮检查
pub fn run(app: tauri::AppHandle) {
    let mut last_round = Instant::now() - Duration::from_secs(3600);
    loop {
        std::thread::sleep(Duration::from_secs(30));
        let s = settings::load();
        if last_round.elapsed() < Duration::from_secs(s.refresh_interval.max(1) * 60) {
            continue;
        }
        last_round = Instant::now();

        // 保活与自动换号开关解耦：关掉自动换号不该让账号放任到过期
        keepalive(KEEPALIVE_WINDOW_MS);

        // P1-2 每日定点签到 / 刷新（与自动换号开关无关）
        run_daily(&s);

        if !s.auto_switch {
            continue;
        }

        let bundles = load_bundles();
        if bundles.is_empty() {
            continue;
        }
        for ide in [IdeKind::Trae, IdeKind::TraeCn] {
            // IDE 未运行时不自动换号：无额度消耗，且不应替用户拉起 IDE（冷回退仅限手动切换）
            let Some(paths) = super::paths::IdePaths::detect(ide) else { continue };
            if !super::launcher::is_running(&paths) {
                continue;
            }
            if let Some((from, to)) = find_exhausted_and_next(&bundles, ide, &s) {
                log(
                    Level::Warn,
                    Source::Quota,
                    format!(
                        "{from} 额度耗尽，开始自动换号（策略：{}）",
                        if s.switch_strategy == "remain" { "剩余最多优先" } else { "按顺序" }
                    ),
                );
                // 重新定位目标 bundle（名字可能重复，用顺序结果中的名字再匹配）
                let Some(target) = bundles
                    .iter()
                    .find(|b| b.name == to && b.ide == ide)
                    .cloned()
                else { continue };
                perform_switch(&from, &to, &target, &app);
            }
        }
    }
}
