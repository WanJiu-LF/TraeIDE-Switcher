// 增值信息与设备诊断（Cloud-IDE-JWT）：权益包 / 付费状态 / 活动中心 / 账单 / 产品目录查询，
// 账号级设备槽派生，以及 ICDRS 设备日志与运行时配置读取。
// 请求形态全部来自对 Trae CN main.js 的实测提取（接口响应已逐字核对）。
use super::paths::{IdeKind, IdePaths};
use super::quota::{api_host, app_version, post_authed, ug_host, QuotaError};
use serde_json::{json, Value};

/// 账号级稳定设备槽：sha1("tsw:"+userId) 前 8 字节取模到 15 位十进制。
/// 服务端 ug/签到系列只按 x-device-id 字符串判定"该设备是否已签到/绑号是否达上限"，
/// 账号间必须互不挤占（本机 aha 设备被任一账号占用后其余账号一律 9095 拒绝）。
pub fn device_slot(user_id: &str) -> String {
    use sha1::{Digest, Sha1};
    let mut h = Sha1::new();
    h.update(format!("tsw:{user_id}").as_bytes());
    let d = h.finalize();
    let n = u64::from_be_bytes(d[..8].try_into().unwrap_or([0u8; 8])) % 900_000_000_000_000
        + 100_000_000_000_000;
    n.to_string()
}

pub struct AssetsClient {
    /// account 主机（pay 系列）
    base: String,
    /// ug 主机（活动/签到等增长侧，按 bootConfig.ug 独立解析）
    ug_base: String,
    /// 该版本 pay 路径前缀（CN=积分制 v2 / 国际=v1）
    pay_ver: &'static str,
    version: String,
}

impl AssetsClient {
    pub fn for_ide(ide: IdeKind, region: &str) -> Self {
        Self {
            base: api_host(ide, region),
            ug_base: ug_host(ide, region),
            pay_ver: if ide == IdeKind::TraeCn { "v2" } else { "v1" },
            version: app_version(ide),
        }
    }

    /// pay 系列带版本回退：主版本返回 404/405 时换另一版本重试一次
    /// （CN 实测 v2 可用，但个别接口在 v1 下暴露；国际反之）
    fn pay(&self, tail: &str, token: &str, device_id: &str, body: Value) -> Result<Value, QuotaError> {
        let alt = if self.pay_ver == "v2" { "v1" } else { "v2" };
        let primary = format!("/trae/api/{}/pay/{tail}", self.pay_ver);
        match post_authed(&self.base, &primary, token, device_id, &self.version, body.clone()) {
            Err(QuotaError::Http(404 | 405, _)) => post_authed(
                &self.base,
                &format!("/trae/api/{alt}/pay/{tail}"),
                token,
                device_id,
                &self.version,
                body,
            ),
            other => other,
        }
    }

    /// 权益包列表：user_entitlement_pack_list / trial_status / is_pay_freshman
    pub fn entitlement_list(&self, token: &str, device_id: &str) -> Result<Value, QuotaError> {
        self.pay(
            "user_current_entitlement_list",
            token,
            device_id,
            json!({ "require_usage": true, "req_source": 1, "full_data": true, "Request": {} }),
        )
    }

    /// 付费状态：commercial_activities[]（activity_id/Enabled/起止时间）+ detail.can_gen_solo_code
    pub fn pay_status(&self, token: &str, device_id: &str) -> Result<Value, QuotaError> {
        self.pay(
            "ide_user_pay_status",
            token,
            device_id,
            json!({ "trae_client": "IDE", "device_id": device_id }),
        )
    }

    /// 账单流水：data_list / has_more / pi_manage_url
    pub fn billing_history(&self, token: &str, device_id: &str, page: i64, size: i64) -> Result<Value, QuotaError> {
        self.pay(
            "billing_history",
            token,
            device_id,
            json!({ "page_num": page, "page_size": size, "Request": {} }),
        )
    }

    /// 可购产品目录：products[].subscription_extra.quota 内含各能力开关
    pub fn sellable_products(&self, token: &str, device_id: &str) -> Result<Value, QuotaError> {
        self.pay("sellable_product_list", token, device_id, json!({ "Request": {} }))
    }

    /// 活动中心：各活动的 eligible/grantable/granted/work_credits/general_credits（ug 主机）
    pub fn activity_info(&self, token: &str, device_id: &str) -> Result<Value, QuotaError> {
        post_authed(
            &self.ug_base,
            "/trae/api/v2/ug/activity/info",
            token,
            device_id,
            &self.version,
            json!({ "req_source": 1 }),
        )
    }
}

/// 运行时配置视图（A4）：把 IDE product.json 的 iCubeApp/bootConfig 关键项暴露给 UI。
/// 主机表按 IDE 独立解析，effective* 为当前区域实际生效值。
pub fn runtime_config(ide: IdeKind, region: &str) -> Value {
    let cfg = IdePaths::detect(ide)
        .map(|p| p.product_config())
        .unwrap_or_else(|| json!({}));
    let section = |name: &str| {
        cfg.pointer(&format!("/bootConfig/{name}/trae"))
            .cloned()
            .unwrap_or(Value::Null)
    };
    let pick = |ptr: &str| cfg.pointer(ptr).cloned().unwrap_or(Value::Null);
    json!({
        "ide": ide,
        "appVersion": pick("/appVersion"),
        "tronBuildVersion": pick("/tronBuildVersion"),
        "quality": pick("/quality"),
        "applicationName": pick("/applicationName"),
        "effectiveAccountHost": api_host(ide, region),
        "effectiveUgHost": ug_host(ide, region),
        "accountHosts": section("account"),
        "ugHosts": section("ug"),
        "iCubeHosts": section("iCube"),
        "agentHosts": section("agent"),
        "wsHosts": section("ws"),
        "consoleHost": pick("/bootConfig/consoleHost"),
        "cdnCheckList": pick("/bootConfig/cdnCheckList"),
        "authConfig": pick("/iCubeApp/authConfig"),
        "ugAppId": pick("/bootConfig/ug/appId"),
        "commercialConfig": pick("/iCubeApp/commercialConfig"),
    })
}

/// 收集 `activity/info` 中"可领取且未领取"的活动 id（CN 真机实测形态定稿）：
/// `{ commercial_activities: [ { activity_id, activity_type, Enabled, start/end_time_ms,
///    work_extra: { eligible, grantable, granted, work_credits, general_credits, auto_grant } } ] }`
/// 关键点：**activity_id 在父级，grantable/granted 在 work_extra 内**——
/// 旧实现要求两者同层，因而一条也匹配不到（真实响应已逐字核对）。
pub fn collect_grantable(v: &Value, out: &mut Vec<String>) {
    let Some(list) = v.get("commercial_activities").and_then(|l| l.as_array()) else {
        return;
    };
    for item in list {
        let Some(extra) = item.get("work_extra") else { continue };
        if extra.get("grantable").and_then(|b| b.as_bool()) != Some(true) {
            continue;
        }
        if extra.get("granted").and_then(|b| b.as_bool()) == Some(true) {
            continue;
        }
        let id = ["activity_id", "activityId"]
            .iter()
            .find_map(|k| item.get(*k).and_then(|x| x.as_str()))
            .map(String::from);
        if let Some(id) = id {
            if !out.contains(&id) {
                out.push(id);
            }
        }
    }
}

/// 收集 `activity/info` 中**已发放**（`work_extra.granted == true`）的活动 id。
/// 用于领奖后的落地校验：claim 返回 200 只代表服务端"受理"，
/// 是否真到账必须看 granted 是否翻转（实测：student_auth_reward 受理后仍为 false）。
pub fn collect_granted(v: &Value) -> Vec<String> {
    let Some(list) = v.get("commercial_activities").and_then(|l| l.as_array()) else {
        return Vec::new();
    };
    list.iter()
        .filter(|item| {
            item.get("work_extra")
                .and_then(|e| e.get("granted"))
                .and_then(|b| b.as_bool())
                == Some(true)
        })
        .filter_map(|item| {
            ["activity_id", "activityId"]
                .iter()
                .find_map(|k| item.get(*k).and_then(|x| x.as_str()))
                .map(String::from)
        })
        .collect()
}

/// 读取该 IDE 最新一轮 main.log 中的 [ICDRS] 设备日志（did/ldid/rdid）
/// 原生日志形态：`[ICDRS] (constructor) did: <remote>, ldid: aha-<hex>, rdid: <remote>`
pub fn read_icdrs_log(ide: IdeKind) -> Value {
    let Some(paths) = IdePaths::detect(ide) else {
        return json!({ "found": false, "reason": "未检测到安装" });
    };
    let Some(dir) = latest_log_dir(&paths.logs_dir()) else {
        return json!({ "found": false, "reason": "无日志目录" });
    };
    let log_path = dir.join("main.log");
    let Ok(text) = std::fs::read_to_string(&log_path) else {
        return json!({ "found": false, "reason": "无 main.log" });
    };
    let mut did = String::new();
    let mut ldid = String::new();
    let mut rdid = String::new();
    let lines: Vec<String> = text
        .lines()
        .filter(|l| l.contains("[ICDRS]"))
        .map(|l| l.trim().to_string())
        .collect();
    for l in &lines {
        if !l.contains("(constructor)") && !l.contains("initialization done") {
            continue;
        }
        if did.is_empty() {
            did = grab(l, " did: ");
        }
        if ldid.is_empty() {
            ldid = grab(l, " ldid: ");
        }
        if rdid.is_empty() {
            rdid = grab(l, " rdid: ");
        }
    }
    json!({
        "found": true,
        "dir": dir.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default(),
        "path": log_path.to_string_lossy(),
        "did": did,
        "ldid": ldid,
        "rdid": rdid,
        "lines": lines.iter().rev().take(12).cloned().collect::<Vec<_>>(),
        "trailing": lines.last().cloned().unwrap_or_default(),
    })
}

/// 取 <key>: <value> 中首个逗号前的值（tok 含两侧空格，避免 " ldid: " 命中 " did: "）
fn grab(line: &str, tok: &str) -> String {
    line.find(tok)
        .map(|i| line[i + tok.len()..].split(',').next().unwrap_or("").trim().to_string())
        .unwrap_or_default()
}

/// logs 根目录下含 main.log 的最新轮次目录（目录名 = 时间戳，字典序即时间序）
fn latest_log_dir(root: &std::path::Path) -> Option<std::path::PathBuf> {
    std::fs::read_dir(root)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.join("main.log").exists())
        .max()
}
