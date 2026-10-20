// Tauri 后端命令封装（invoke），非 Tauri 环境返回空数据或抛出友好错误
import { invoke } from "@tauri-apps/api/core";
import type { Ide } from "./types";

export interface AccountSummary {
  id: string;
  name: string;
  email: string;
  note: string;
  ide: Ide;
  status: "Normal" | "Exhausted" | "Error";
  user_id: string;
  /** 置顶显示 */
  pinned?: boolean;
  /** 该账号独立耗尽阈值（剩余百分比），null/undefined = 用全局阈值 */
  threshold?: number | null;
  /** 本次入库是否覆盖了已存在的同 userId 账号 */
  replaced?: boolean;
  /** 凭据 access token 到期时刻（epoch 毫秒） */
  expires_at?: number | null;
  /** 凭据已过期 */
  cred_expired?: boolean;
  /** 上次续期失败原因（如 20403 Token device not match） */
  refresh_error?: string | null;
  /** 上次续期失败为设备不匹配 */
  device_mismatch?: boolean;
}

export interface PackQuota {
  product_type: number;
  source_type: number;
  desc: string;
  basic_limit: number;
  basic_used: number;
  bonus_limit: number;
  bonus_used: number;
  credits_limit: number;
  credits_used: number;
  credits_unlimited: boolean;
  /** 国际版：无赠送额度（IDE 侧不展示赠送额度行） */
  no_bonus_quota: boolean;
  /** 国际版：额度以美元计 */
  is_dollar_billing: boolean;
  /** 结算周期结束时刻（秒），额度重置基准 */
  end_time: number;
  expire_time: string;
}

export interface CheckinStatus {
  enable: boolean;
  checked_in: boolean;
  did_checked_in?: boolean;
  credits: number;
}

export interface IdeStatus {
  installed: boolean;
  install?: string;
  running?: boolean;
  cdpReady?: boolean;
  tswPatched?: boolean;
  shimAlive?: boolean;
  loggedIn?: boolean;
  /** 该 IDE 自身登录态对应的账号 id（"0-<uid>"=国际版，"1-<uid>"=国内版） */
  currentId?: string;
  /** 磁盘补丁晚于运行实例启动 = 补丁未加载进内存，需重启 IDE 激活 */
  stalePatch?: boolean;
  /** 会话/任务归属补丁（workbench + ai-modules-chat）是否已注入 */
  chatPinned?: boolean;
}

const PREVIEW_ERROR = "当前为浏览器预览环境，该功能仅在 Trae 换号器桌面程序中使用";

export function isTauri(): boolean {
  return typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;
}

function ensureTauri(): void {
  if (!isTauri()) throw new Error(PREVIEW_ERROR);
}

export function errText(e: unknown): string {
  return e instanceof Error ? e.message : String(e);
}

// Rust 端 IdeKind 序列化为 "Trae"/"TraeCn"，统一归一为 "trae"/"trae-cn"
function toIde(value: string): Ide {
  return value.toLowerCase().replace(/[-_]/g, "") === "traecn" ? "trae-cn" : "trae";
}

type RawSummary = Omit<AccountSummary, "ide"> & { ide: string };

function normalizeSummary(raw: RawSummary): AccountSummary {
  return { ...raw, ide: toIde(raw.ide) };
}

export async function listAccounts(): Promise<AccountSummary[]> {
  if (!isTauri()) return [];
  const raw = await invoke<RawSummary[]>("list_accounts");
  return raw.map(normalizeSummary);
}

export async function scanLocalAccount(ide: Ide): Promise<AccountSummary | null> {
  if (!isTauri()) return null;
  const raw = await invoke<RawSummary | null>("scan_local_account", { ide });
  return raw ? normalizeSummary(raw) : null;
}

export async function importLocalAccount(ide: Ide, name: string, note: string): Promise<AccountSummary> {
  ensureTauri();
  return normalizeSummary(await invoke<RawSummary>("import_local_account", { ide, name, note }));
}

export async function addAccount(ide: Ide, name: string, note: string, authBlob: string): Promise<AccountSummary> {
  ensureTauri();
  return normalizeSummary(await invoke<RawSummary>("add_account", { ide, name, note, authBlob }));
}

export async function deleteAccount(id: string): Promise<void> {
  ensureTauri();
  await invoke("delete_account", { id });
}

export async function updateAccount(
  id: string,
  name: string,
  note: string,
  threshold: number | null,
): Promise<void> {
  ensureTauri();
  await invoke("update_account", { id, name, note, threshold });
}

/** P0-6 置顶/取消置顶 */
export async function setAccountPinned(id: string, pinned: boolean): Promise<void> {
  ensureTauri();
  await invoke("set_account_pinned", { id, pinned });
}

// ============ P0-1 账号库备份 / 恢复 ============

export interface VaultBackup {
  path: string;
  mtime_ms: number;
  size: number;
}

/** 导出加密备份包，返回文件路径 */
export async function exportVaultBackup(): Promise<string> {
  ensureTauri();
  return invoke<string>("export_vault_backup");
}

/** 列出可用备份包（新→旧） */
export async function listVaultBackups(): Promise<VaultBackup[]> {
  if (!isTauri()) return [];
  return JSON.parse(await invoke<string>("list_vault_backups")) as VaultBackup[];
}

/** 从备份包恢复（原账号库自动轮转为 accounts.bak） */
export async function importVaultBackup(path: string): Promise<string> {
  ensureTauri();
  return invoke<string>("import_vault_backup", { path });
}

// ============ 可移植账号包（口令加密，跨机迁移）============

export interface PortableExport {
  path: string;
  content: string;
}

/** 导出账号包：以口令重加密账号库，返回文件路径与账号包内容 */
export async function exportPortable(passphrase: string): Promise<PortableExport> {
  ensureTauri();
  return JSON.parse(await invoke<string>("export_portable", { passphrase })) as PortableExport;
}

/** 导入账号包：source 为账号包内容或文件路径 */
export async function importPortable(passphrase: string, source: string): Promise<string> {
  ensureTauri();
  return invoke<string>("import_portable", { passphrase, source });
}

// ============ P1-1 额度采样历史 ============

export interface UsageSample {
  ts: number;
  remain: number;
}

/** 读取该账号的额度采样序列（升序） */
export async function usageHistory(id: string): Promise<UsageSample[]> {
  if (!isTauri()) return [];
  return JSON.parse(await invoke<string>("usage_history", { id })) as UsageSample[];
}

export async function queryQuota(id: string): Promise<PackQuota[]> {
  if (!isTauri()) return [];
  return invoke<PackQuota[]>("query_quota", { id });
}

// ============ 切换历史时间线 ============

export interface SwitchRecord {
  ts: number;
  from: string;
  to: string;
  ide: string;
  /** 是否为热切换 */
  hot: boolean;
  ok: boolean;
  /** 耗时（毫秒） */
  ms: number;
  note: string;
}

/** 读取切换历史（按发生顺序，前端自行倒序展示） */
export async function switchHistory(): Promise<SwitchRecord[]> {
  if (!isTauri()) return [];
  return JSON.parse(await invoke<string>("switch_history")) as SwitchRecord[];
}

// ============ 设备身份管理 ============

export interface AccountDevice {
  id: string;
  name: string;
  /** 该账号随包的 aha 设备 id（TinyStorage aha.device.device_id） */
  deviceIdStr: string | null;
  /** ECDSA 设备密钥指纹（sha256(键名+publicKeyPEM) 前 16 hex） */
  ecdsaFingerprint: string | null;
  ecdsaKeyCount: number;
  hasTiny: boolean;
  hasEcdsa: boolean;
  hasDev: boolean;
  hasSqm: boolean;
  /** 缺失的设备资产项（旧版入库账号会缺项） */
  missing: string[];
}

/** 列出指定版本全部账号的随号设备资产 */
export async function accountDevices(ide: Ide): Promise<AccountDevice[]> {
  if (!isTauri()) return [];
  return JSON.parse(await invoke<string>("account_devices", { ide })) as AccountDevice[];
}

/** 多开：为该账号准备独立 user-data 目录并以第二个实例启动 */
export async function launchIsolatedInstance(
  id: string,
): Promise<{ pid: number; dir: string; name: string }> {
  ensureTauri();
  return JSON.parse(await invoke<string>("launch_isolated_instance", { id })) as {
    pid: number;
    dir: string;
    name: string;
  };
}

/** 在该账号的独立实例内无感换号（走实例自己的切换助手通道） */
export async function switchIsolated(id: string): Promise<string> {
  ensureTauri();
  return invoke<string>("switch_isolated", { id });
}

// ============ 会话级用量明细（agent 本地 RPC）============

export interface SessionUsageRow {
  sessionId: string;
  title: string;
  /** 会话最近更新时间（epoch ms） */
  updatedAt: number;
  /** 含 token 用量的助手轮次数 */
  turns: number;
  messageCount: number;
  promptTokens: number;
  completionTokens: number;
  totalTokens: number;
  cacheReadTokens: number;
  cacheCreationTokens: number;
  reasoningTokens: number;
  /** 最后一轮总 token（上下文占用口径） */
  lastTurnTotalTokens: number;
  /** 最后一轮的模型上下文上限 */
  maxTokens: number;
  /** 分页触顶：该会话仍有未统计的消息 */
  truncated: boolean;
}

/** 单模型用量累计（本轮拉取到的全部会话按模型汇总） */
export interface ModelUsageRow {
  name: string;
  turns: number;
  promptTokens: number;
  completionTokens: number;
  totalTokens: number;
}

export interface SessionUsage {
  sessions: SessionUsageRow[];
  /** 服务端会话总数 */
  totalCount: number;
  /** 会话所属 project（补丁固定后为账号共享 project） */
  projectId: string;
  /** 按模型汇总的用量 */
  models: ModelUsageRow[];
  /** 单个会话拉取失败的明细 */
  errors: { sessionId: string; error: string }[];
}

/** 会话级用量明细：需 Trae 运行中 + shim 已激活 + 补丁已注入并重启生效 */
export async function sessionUsage(ide: Ide): Promise<SessionUsage> {
  if (!isTauri()) return { sessions: [], totalCount: 0, projectId: "", models: [], errors: [] };
  return JSON.parse(await invoke<string>("session_usage", { ide })) as SessionUsage;
}

export async function switchAccount(id: string, hot: boolean): Promise<string> {
  ensureTauri();
  return invoke<string>("switch_account", { id, hot });
}

export async function coldSwitchRestart(id: string): Promise<string> {
  ensureTauri();
  return invoke<string>("cold_switch_restart", { id });
}

export async function ideStatus(ide: Ide): Promise<IdeStatus> {
  if (!isTauri()) return { installed: false };
  return JSON.parse(await invoke<string>("ide_status", { ide })) as IdeStatus;
}

/** 两个版本各自实际生效的安装路径（手填优先 → 注册表探测） */
export async function installPaths(): Promise<Record<Ide, string>> {
  if (!isTauri()) return { trae: "", "trae-cn": "" };
  return JSON.parse(await invoke<string>("install_paths")) as Record<Ide, string>;
}

export async function launchIde(ide: Ide): Promise<number> {
  ensureTauri();
  return invoke<number>("launch_ide", { ide });
}

export async function injectBridge(ide: Ide): Promise<string> {
  ensureTauri();
  return invoke<string>("inject_bridge", { ide });
}

export async function removeBridge(ide: Ide): Promise<string> {
  ensureTauri();
  return invoke<string>("remove_bridge", { ide });
}

export async function bridgeStatus(ide: Ide): Promise<boolean> {
  if (!isTauri()) return false;
  return invoke<boolean>("bridge_status", { ide });
}

// ============ shim 扩展（热换号主通道）============

export interface ShimStatus {
  installed: boolean;
  installed_version?: string;
  target_version: string;
  alive: boolean;
  last_heartbeat?: number;
}

export async function shimStatus(ide: Ide): Promise<ShimStatus> {
  if (!isTauri()) return { installed: false, target_version: "", alive: false };
  return invoke<ShimStatus>("shim_status", { ide });
}

export async function shimInstall(ide: Ide): Promise<string> {
  ensureTauri();
  return invoke<string>("shim_install", { ide });
}

export async function shimUninstall(ide: Ide): Promise<string> {
  ensureTauri();
  return invoke<string>("shim_uninstall", { ide });
}

export interface LoginStart {
  url: string;
  port: number;
}

export interface LoginPoll {
  status: "pending" | "done" | "error";
  authBlob?: string;
  name?: string;
  email?: string;
  message?: string;
}

export async function startLogin(ide: Ide): Promise<LoginStart> {
  ensureTauri();
  return JSON.parse(await invoke<string>("start_login", { ide })) as LoginStart;
}

export async function pollLogin(): Promise<LoginPoll> {
  ensureTauri();
  return JSON.parse(await invoke<string>("poll_login")) as LoginPoll;
}

/** 取消登录采集：必须调用，否则后端回调服务器残留会让二次采集被"占用"拒绝 */
export async function cancelLogin(): Promise<void> {
  if (!isTauri()) return;
  await invoke<void>("cancel_login");
}

export async function queryCheckin(id: string): Promise<CheckinStatus> {
  ensureTauri();
  return JSON.parse(await invoke<string>("query_checkin", { id })) as CheckinStatus;
}

export async function claimCheckin(id: string): Promise<CheckinStatus> {
  ensureTauri();
  return JSON.parse(await invoke<string>("claim_checkin", { id })) as CheckinStatus;
}

// ============ 增值信息（权益/付费状态/活动/账单/产品目录/会话用量）与设备诊断 ============

export interface AssetBundle {
  id: string;
  name: string;
  ide: Ide;
  /** 本账号专属设备槽（x-device-id） */
  device: string;
  /** 各接口原始响应：entitlement / pay_status / activity / billing / products */
  data: Record<string, unknown>;
  /** 失败接口的 key → 错误信息（单接口失败不阻断整体） */
  errors: Record<string, string>;
  /** 该版本是否支持活动中心（活动/签到为 CN 专有） */
  activitySupported?: boolean;
}

export interface ActivityClaimResult {
  /** 本次判定为"可领取且未领取"的活动 id 列表 */
  grantable: string[];
  /** 领奖后 granted 真正翻转（实际到账）的活动 id */
  landed: string[];
  results: { activity_id: string; ok: boolean; result: unknown }[];
}

export interface IcdrsLog {
  found: boolean;
  reason?: string;
  dir?: string;
  path?: string;
  did?: string;
  ldid?: string;
  rdid?: string;
  lines?: string[];
}

/** 聚合拉取单账号增值信息（权益/付费状态/活动/账单/产品目录） */
export async function queryAssets(id: string): Promise<AssetBundle> {
  ensureTauri();
  const raw = JSON.parse(await invoke<string>("query_assets", { id })) as AssetBundle & { ide: string };
  return { ...raw, ide: toIde(String(raw.ide)) };
}

/** 活动领奖：仅对 grantable && !granted 的活动发起（走原生命令，需 Trae 运行中 + shim 激活） */
export async function claimActivityRewards(id: string): Promise<ActivityClaimResult> {
  ensureTauri();
  return JSON.parse(await invoke<string>("claim_activity_rewards", { id })) as ActivityClaimResult;
}

/** ICDRS 设备日志（did/ldid/rdid），读该 IDE 最新一轮 main.log */
export async function icdrsLog(ide: Ide): Promise<IcdrsLog> {
  if (!isTauri()) return { found: false };
  return JSON.parse(await invoke<string>("icdrs_log", { ide })) as IcdrsLog;
}

export interface VerifyResult {
  /** true=凭据可用 / false=失效 / null=无法判定（网络或服务异常，不改动账号状态） */
  valid: boolean | null;
  reason: string;
  /** 后端写回的账号状态（null = 未改动） */
  status: "Normal" | "Exhausted" | "Error" | null;
}

/** A2 账号有效性校验：用**经验证可用**的额度接口判定（CheckLogin 在 CN 上对有效账号也返回
 *  IsLogin=false，只会产生假阴性，已弃用），结果写回账号库状态 */
export async function verifyAccount(id: string): Promise<VerifyResult> {
  ensureTauri();
  return JSON.parse(await invoke<string>("verify_account", { id })) as VerifyResult;
}

export interface RuntimeConfig {
  ide: string;
  appVersion?: string;
  tronBuildVersion?: string;
  quality?: string;
  applicationName?: string;
  effectiveAccountHost?: string;
  effectiveUgHost?: string;
  accountHosts?: Record<string, string>;
  ugHosts?: Record<string, string>;
  iCubeHosts?: Record<string, string>;
  agentHosts?: Record<string, string>;
  wsHosts?: Record<string, string>;
  consoleHost?: string;
  cdnCheckList?: string[];
  authConfig?: Record<string, Record<string, string>>;
  ugAppId?: string;
  commercialConfig?: Record<string, unknown>;
}

/** A4 运行时配置视图（product.json 的 bootConfig/iCubeApp） */
export async function runtimeConfig(ide: Ide): Promise<RuntimeConfig> {
  if (!isTauri()) return { ide };
  return JSON.parse(await invoke<string>("runtime_config", { ide })) as RuntimeConfig;
}

/** A3 能力字典：shim 实时列出 IDE 已注册的 trae/icube 命令 */
export async function shimCommands(ide: Ide): Promise<string[]> {
  ensureTauri();
  return JSON.parse(await invoke<string>("shim_commands", { ide })) as string[];
}

/** 调用一条只读命令（后端有前缀白名单；非白名单直接被拒绝） */
export async function shimInvoke(ide: Ide, command: string, args: unknown[] = []): Promise<unknown> {
  ensureTauri();
  const raw = JSON.parse(await invoke<string>("shim_invoke", { ide, command, args })) as { value: unknown };
  return raw.value;
}

// ============ 日志 ============

export interface LogEntry {
  id: number;
  ts_ms: number;
  level: "Info" | "Warn" | "Err";
  source: "Switch" | "Quota" | "Inject" | "Account" | "System";
  message: string;
}

export async function getLogs(): Promise<LogEntry[]> {
  if (!isTauri()) return [];
  return invoke<LogEntry[]>("get_logs");
}

export async function exportLogs(): Promise<string> {
  if (!isTauri()) return "";
  return invoke<string>("export_logs");
}

// ============ 设置 ============

export interface AppSettings {
  auto_switch: boolean;
  threshold: number;
  refresh_interval: number;
  auto_start: boolean;
  close_to_tray: boolean;
  notifications: boolean;
  trae_path: string;
  trae_cn_path: string;
  /** P1-2 每日定点批量签到（仅 CN 账号） */
  daily_checkin: boolean;
  /** P1-2 每日定点全量刷新额度 */
  daily_refresh: boolean;
  /** 定时任务时刻（本地时间 "HH:MM"） */
  daily_time: string;
  /** 自动换号策略："order"=按入库顺序 / "remain"=剩余额度最多优先 */
  switch_strategy: string;
  /** 自动换号范围："all"=全池 / "pinned"=只用置顶账号 */
  switch_scope: string;
  /** HTTP 代理地址（如 http://127.0.0.1:7890）；空串 = 直连 */
  http_proxy: string;
  /** 自定义更新源（latest.json 地址）；空串 = 内置默认端点 */
  update_endpoint: string;
}

export const DEFAULT_SETTINGS: AppSettings = {
  auto_switch: true,
  threshold: 2,
  refresh_interval: 5,
  auto_start: false,
  close_to_tray: true,
  notifications: true,
  trae_path: "",
  trae_cn_path: "",
  daily_checkin: false,
  daily_refresh: false,
  daily_time: "09:00",
  switch_strategy: "order",
  switch_scope: "all",
  http_proxy: "",
  update_endpoint: "",
};

export async function getSettings(): Promise<AppSettings> {
  if (!isTauri()) return { ...DEFAULT_SETTINGS };
  return invoke<AppSettings>("get_settings");
}

export async function updateSettings(patch: Partial<AppSettings>): Promise<AppSettings> {
  if (!isTauri()) return { ...DEFAULT_SETTINGS, ...patch };
  return invoke<AppSettings>("update_settings", { patch });
}

export async function autostart(enable: boolean): Promise<void> {
  if (!isTauri()) return;
  await invoke("autostart", { enable });
}

// ============ 应用内更新（走设置中的代理与自定义更新源）============

export interface UpdateInfo {
  available: boolean;
  version?: string;
  date?: string | null;
  body?: string | null;
  installed?: boolean;
}

/** 检查（install=false）或下载并安装（install=true）应用更新 */
export async function appUpdate(install: boolean): Promise<UpdateInfo> {
  ensureTauri();
  return JSON.parse(await invoke<string>("app_update", { install })) as UpdateInfo;
}
