import { useEffect, useMemo, useState, type ReactNode } from "react";
import type { Account, Ide } from "../types";
import {
  accountDevices,
  claimActivityRewards,
  errText,
  icdrsLog,
  runtimeConfig,
  shimCommands,
  shimInvoke,
  usageHistory,
  verifyAccount,
  type AccountDevice,
  type AssetBundle,
  type IcdrsLog,
  type RuntimeConfig,
  type SessionUsage,
  type UsageSample,
  type VerifyResult,
} from "../bridge";
import { groupSources } from "../components/AccountCard";
import { Select } from "../components/Select";
import { IconRefresh } from "../icons";

interface AssetsPageProps {
  accounts: Account[];
  targetIde: Ide;
  /** 资产缓存（账号 id → 结果）：由外层后台预取填充，本页进入时不再发请求 */
  assets: Record<string, AssetBundle>;
  /** 会话用量缓存（由外层自动拉取：开机、切版本、换号、30s 轮询）：本页只读 */
  usage: SessionUsage | null;
  usageBusy: boolean;
  usageErr: string | null;
  /** 最近一次会话用量拉取时刻（epoch ms，0 = 尚未拉取） */
  usageAt: number;
  /** 手动刷新会话用量（notify=true 时失败会弹提示） */
  onRefreshUsage: (notify: boolean) => void;
  /** 手动刷新单账号资产（结果写入外层缓存） */
  onRefresh: (id: string) => Promise<AssetBundle>;
  onToast: (kind: "success" | "error" | "info", title: string, desc?: string) => void;
  /** 校验后账号状态已写回后端，通知外层重载账号列表 */
  onReload?: () => void;
}

// ── 活动中文名与说明（activity_id 为服务端口径，此处做面向用户的转译） ──
const ACTIVITIES: Record<string, { name: string; note: string }> = {
  checkin_credits: { name: "每日签到", note: "每天可领取一次，通过账号卡片上的「一键签到」领取" },
  new_user_credits: { name: "新用户奖励", note: "注册后自动发放，无需手动领取" },
  send_message_reward: { name: "发消息奖励", note: "在 Trae 中首次发消息后自动发放" },
  code_fission_202609: { name: "代码分享奖励", note: "需要账号具备代码活动资格，且使用非 Work 版客户端" },
  work_fission_202607: { name: "Work 分享奖励", note: "仅 Work(Lite) 版客户端可领取" },
  work_desktop_download: { name: "下载桌面端奖励", note: "当前不满足领取条件" },
  student_auth_reward: { name: "学生认证奖励", note: "需先完成学生认证，通过后自动发放" },
};

// ── 产品能力开关的中文名。真实响应键形如 enable_solo_web / enable_solo_builder，
//    故先剥前缀再映射；未收录的键直接显示原文，不臆造译名。 ──
const CAP_WORDS: Record<string, string> = {
  web: "网页",
  builder: "构建",
  builder_v1: "构建(旧)",
  coder: "编码",
  lite: "轻量",
  agent: "SOLO 智能体",
  designer: "设计",
  super_model: "超级模型",
  ralph_loop: "Ralph 循环",
  early_access: "抢先体验",
};

function capLabel(key: string): string {
  const bare = key.replace(/^enable_/, "").replace(/^solo_/, "");
  return CAP_WORDS[bare] ?? key;
}

// 能力字典内的常用命令（白名单内的幂等项；forkSession 每次产生新会话，非幂等故不提供）
const SESSION_PRESETS: { label: string; command: string; args: string }[] = [
  { label: "当前会话 id", command: "icube.chat.getCurrentSessionId", args: "[]" },
  { label: "会话运行状态", command: "icube.chat.getSessionRunningStatus", args: "[]" },
  { label: "导出当前会话", command: "workbench.action.icube.aiChatSidebar.exportCurrentSession", args: "[]" },
  { label: "迁移会话（teleport）", command: "icube.session.teleport", args: '[{"user_id":"","remote_session_id":""}]' },
  { label: "会话收回本地", command: "icube.session.returnToLocal", args: "[]" },
  { label: "学生认证", command: "workbench.action.icubeOpenStudentVerification", args: "[]" },
];

// F2 错误码字典：逐条给出码义、判定出处与处置方式（出处均来自源码定位或实测）
const ERROR_CODES: { code: string; meaning: string; source: string; action: string }[] = [
  {
    code: "20401",
    meaning: "设备绑定数量达上限（该设备已绑定过多账号）",
    source: "main.js Handle_Login_DeviceLimitError=20401",
    action: "换设备身份（aha device_id / TinyStorage 快照随号独立），或先解绑旧账号",
  },
  {
    code: "20403",
    meaning: "Token 与设备不匹配（refreshToken 绑定签发时所用设备）",
    source: "marscode CheckLogin 需重登码集合；本工具续期实测返回",
    action: "用签发该 token 的原始设备续期；预刷新失败不阻断切换",
  },
  {
    code: "20324 / 20101 / 20315 / 20125 / 20126",
    meaning: "鉴权失效，服务端判定需重新登录",
    source: "main.js marscode/request.js MM 集合",
    action: "重新走登录采集换取新凭据",
  },
  {
    code: "4015",
    meaning: "设备密钥（ECDSA）重生成触发服务端风控",
    source: "项目实测记录（ECDSA 必须随号保存恢复）",
    action: "保留账号资产包中的 ECDSA 密钥对，禁用重新生成",
  },
  {
    code: "9095 / 9090",
    meaning: "当前设备今日已签到 / 活动暂不可用",
    source: "ug checkin_credits、activity/action 实测",
    action: "每账号独立派生设备槽；仅对可领取活动发起领奖",
  },
  {
    code: "040012 (TokenInvalid)",
    meaning: "续期时随请求携带的 access token 已过期被拒",
    source: "ExchangeToken 实测（仅在 token 仍有效时才能续期）",
    action: "尽早续期（保活已内置 30 分钟预刷新）；已过期只能重新登录采集",
  },
  {
    code: "10001 / 10002",
    meaning: "登录超时 / 登录网络错误",
    source: "main.js Handle_Login_TimeoutError / NetworkError",
    action: "重试或检查网络与代理",
  },
];

function str(v: unknown): string {
  return typeof v === "string" ? v : typeof v === "number" ? String(v) : "";
}
function num(v: unknown): number {
  if (typeof v === "number") return v;
  if (typeof v === "string" && v.trim() !== "" && !isNaN(Number(v))) return Number(v);
  return 0;
}
function bool(v: unknown): boolean | undefined {
  return typeof v === "boolean" ? v : undefined;
}
function obj(v: unknown): Record<string, unknown> | null {
  return v && typeof v === "object" && !Array.isArray(v) ? (v as Record<string, unknown>) : null;
}
function dump(v: unknown): string {
  try {
    return JSON.stringify(v, null, 2) ?? String(v);
  } catch {
    return String(v);
  }
}
function fmt(n: number): string {
  return n.toLocaleString("zh-CN", { minimumFractionDigits: n % 1 === 0 ? 0 : 2, maximumFractionDigits: 2 });
}
function toDate(v: string | number): Date | null {
  if (v === "" || v === null || v === undefined) return null;
  if (typeof v === "number" && !isFinite(v)) return null;
  const s = String(v).trim();
  if (s === "") return null;
  const d = /^\d+$/.test(s) ? new Date(Number(s) < 1e11 ? Number(s) * 1000 : Number(s)) : new Date(s);
  return isNaN(d.getTime()) ? null : d;
}
function day(v: string | number): string {
  const d = toDate(v);
  return d ? d.toLocaleDateString("zh-CN") : "";
}
function stamp(v: string | number): string {
  const d = toDate(v);
  if (!d) return "";
  const p = (n: number) => String(n).padStart(2, "0");
  return `${p(d.getMonth() + 1)}/${p(d.getDate())} ${p(d.getHours())}:${p(d.getMinutes())}`;
}

interface ActivityNode {
  id: string;
  type: number;
  enabled: boolean;
  eligible?: boolean;
  grantable: boolean;
  granted: boolean;
  autoGrant: boolean;
  work: number;
  general: number;
}

/** activity/info 真实形态（CN 真机逐字核对）：
 *  `{ commercial_activities: [ { activity_id, activity_type, Enabled,
 *     work_extra: { eligible, grantable, granted, work_credits, general_credits, auto_grant } } ] }`
 *  —— activity_id 在父级、判定字段在 work_extra 内。 */
function collectActivities(v: unknown): ActivityNode[] {
  const root = obj(v);
  const list = root && Array.isArray(root.commercial_activities) ? root.commercial_activities : [];
  const out: ActivityNode[] = [];
  for (const it of list) {
    const o = obj(it);
    if (!o) continue;
    const extra = obj(o.work_extra);
    out.push({
      id: str(o.activity_id ?? o.activityId),
      type: num(o.activity_type),
      enabled: o.Enabled !== false,
      eligible: bool(extra?.eligible),
      grantable: extra?.grantable === true,
      granted: extra?.granted === true,
      autoGrant: extra?.auto_grant === true,
      work: num(extra?.work_credits),
      general: num(extra?.general_credits),
    });
  }
  return out;
}

type CreditSource = NonNullable<Account["credits"]>["sources"][number];

/** 权益包 → 额度来源：CN 取积分（credits），国际版取基础额度（basic）与赠送额度（bonus）。
 *  dollar = is_dollar_usage_billing，为真时数值以美元计。 */
function packsToSources(v: unknown): { sources: CreditSource[]; dollar: boolean } {
  const root = obj(v);
  const list = root && Array.isArray(root.user_entitlement_pack_list) ? root.user_entitlement_pack_list : [];
  const dollar = root?.is_dollar_usage_billing === true;
  // 国际版 credits_* 恒为 0：任一包带积分才按积分口径渲染
  const creditsBased = list.some((it) => {
    const p = obj(it);
    if (!p) return false;
    const quota = obj(obj(p.entitlement_base_info)?.quota);
    return !!quota && (num(quota.credits_limit) !== 0 || num(obj(p.usage)?.credits_amount) !== 0);
  });
  const out: CreditSource[] = [];
  for (const it of list) {
    const p = obj(it);
    if (!p) continue;
    const info = obj(p.entitlement_base_info);
    const quota = info ? obj(info.quota) : null;
    const usage = obj(p.usage);
    if (!quota) continue;
    const endTime = info ? num(info.end_time) : 0;
    const packExpire = num(p.expire_time);
    // expire_time 为 0/缺失时回落结算周期结束时刻（国际版该字段恒为 0，直接取会显示 1970/1/1）
    const expire = packExpire > 0 ? String(packExpire) : endTime > 0 ? String(endTime) : "";
    if (creditsBased) {
      const limit = num(quota.credits_limit);
      const used = num(usage?.credits_amount);
      const unlimited = limit === -1;
      out.push({
        desc: str(p.display_desc) || "订阅权益",
        remaining: unlimited ? 0 : Math.max(limit - used, 0),
        used,
        limit,
        unlimited,
        expire,
      });
      continue;
    }
    // 国际版：基础额度始终展示；no_bonus_quota=true 表示无赠送额度（IDE 侧同样不展示）
    const basicUsed = num(usage?.basic_usage_amount);
    const basicLimit = num(quota.basic_usage_limit);
    out.push({
      desc: "基础额度",
      remaining: Math.max(basicLimit - basicUsed, 0),
      used: basicUsed,
      limit: basicLimit,
      unlimited: false,
      expire,
    });
    if (quota.no_bonus_quota !== true) {
      const bonusUsed = num(usage?.bonus_usage_amount);
      const bonusLimit = num(quota.bonus_usage_limit);
      out.push({
        desc: "赠送额度",
        remaining: Math.max(bonusLimit - bonusUsed, 0),
        used: bonusUsed,
        limit: bonusLimit,
        unlimited: false,
        expire,
      });
    }
  }
  return { sources: out, dollar };
}

/** 积分来源进度条（与账号卡片同款视觉） */
function CreditBars({ sources, dollar }: { sources: CreditSource[]; dollar?: boolean }) {
  const groups = groupSources(sources);
  if (groups.length === 0) return null;
  const money = (v: number) => (dollar ? `$${v.toFixed(2)}` : fmt(v));
  let sumRemain = 0;
  let sumLimit = 0;
  for (const g of groups) {
    if (g.unlimited) continue;
    sumRemain += g.remaining;
    sumLimit += g.limit;
  }
  const pct = sumLimit > 0 ? (sumRemain / sumLimit) * 100 : 0;
  return (
    <>
      <div className="flex items-end gap-2">
        <b className="text-[26px] leading-none font-bold font-mono">{money(sumRemain)}</b>
        <span className="text-[11.5px] text-ink3 pb-[2px]">
          {dollar ? "可用额度" : "可用积分"} / 共 {money(sumLimit)}
          {groups.some((g) => g.unlimited) && "（另有无限额度）"}
        </span>
      </div>
      <div className="mt-2 h-[7px] bg-[#eef3f6] rounded-full overflow-hidden">
        <div
          className="bar-fill h-full rounded-full"
          style={{ width: `${Math.max(pct, 1.5)}%`, background: pct <= 10 ? "var(--color-warn)" : "var(--color-ok)" }}
        />
      </div>
      <div className="mt-3 flex flex-col gap-2">
        {groups.map((g) => {
          const remainPct = g.limit > 0 ? (g.remaining / g.limit) * 100 : 0;
          const color = g.unlimited
            ? "var(--color-accent)"
            : remainPct <= 0
              ? "var(--color-bad)"
              : remainPct <= 10
                ? "var(--color-warn)"
                : "var(--color-ok)";
          return (
            <div key={g.desc}>
              <div className="flex items-center justify-between text-[11.5px] text-ink2 mb-[5px]">
                <span className="truncate">
                  {g.desc}
                  {g.count > 1 && <span className="text-[9.5px] text-ink3 ml-1">共 {g.count} 笔</span>}
                </span>
                <span className="flex items-center gap-2 flex-none ml-2">
                  <b className={`font-mono ${!g.unlimited && remainPct <= 0 ? "text-bad" : "text-ink"}`}>
                    {g.unlimited ? "无限" : `${money(g.remaining)} / ${money(g.limit)}`}
                  </b>
                  {g.expire && <span className="text-[9.5px] text-ink3">{day(g.expire)} 到期</span>}
                </span>
              </div>
              <div className="h-[5px] bg-[#eef3f6] rounded-full overflow-hidden">
                <div
                  className="bar-fill h-full rounded-full"
                  style={{
                    width: `${g.unlimited ? 100 : remainPct <= 0 ? 0 : Math.max(remainPct, 2)}%`,
                    background: color,
                  }}
                />
              </div>
            </div>
          );
        })}
      </div>
    </>
  );
}

function Section({
  title,
  badge,
  action,
  raw,
  children,
}: {
  title: string;
  badge?: string;
  action?: ReactNode;
  raw?: unknown;
  children: ReactNode;
}) {
  const [showRaw, setShowRaw] = useState(false);
  return (
    <div className="bg-card border border-line rounded-[12px] shadow-card p-4">
      <div className="flex items-center gap-2">
        <span className="text-[13.5px] font-semibold">{title}</span>
        {badge && <span className="text-[10.5px] text-ink3">{badge}</span>}
        {action}
        {raw !== undefined && (
          <button
            className={`ml-auto btn-ghost font-mono text-[11px] flex-none px-2 ${showRaw ? "text-accent" : ""}`}
            title={showRaw ? "收起原始数据" : "查看原始数据"}
            onClick={() => setShowRaw((v) => !v)}
          >
            {"{}"}
          </button>
        )}
      </div>
      <div className="mt-2.5">{children}</div>
      {showRaw && raw !== undefined && (
        <pre className="dropdown-in mt-2.5 bg-[#f7fafb] border border-line-soft rounded-[9px] p-3 text-[10.5px] font-mono text-ink2 overflow-x-auto max-h-[280px]">
          {dump(raw)}
        </pre>
      )}
    </div>
  );
}

/** 消耗趋势折线（纯 SVG，不引图表库） */
function Spark({ data }: { data: UsageSample[] }) {
  const W = 600;
  const H = 56;
  const xs = data.map((d) => d.ts);
  const ys = data.map((d) => d.remain);
  const minX = xs[0];
  const lastX = xs[xs.length - 1];
  const maxX = Math.max(lastX, minX + 1);
  // Y 轴按"数据实际区间"取上下限（不把 0 计入下限），否则几百点量级差异会把折线压成一条直线
  const maxY = Math.max(...ys, 1);
  const minY = Math.min(...ys);
  const span = maxY - minY;
  const px = (t: number) => ((t - minX) / (maxX - minX)) * W;
  // 数据完全水平时居中绘制，避免看起来贴底（像"已耗尽"）
  const py = (v: number) => (span <= 1e-9 ? H / 2 : H - ((v - minY) / span) * H);
  const pts = data.map((d) => `${px(d.ts).toFixed(1)},${py(d.remain).toFixed(1)}`).join(" ");
  const area = `${px(minX).toFixed(1)},${H} ${pts} ${px(lastX).toFixed(1)},${H}`;
  return (
    <>
      <svg viewBox="0 0 600 56" className="mt-2 w-full h-[56px]" preserveAspectRatio="none">
        <polygon points={area} fill="var(--color-accent-soft)" opacity="0.5" />
        <polyline
          points={pts}
          fill="none"
          stroke="var(--color-accent)"
          strokeWidth="1.5"
          strokeLinejoin="round"
          vectorEffect="non-scaling-stroke"
        />
      </svg>
      <div className="flex items-center justify-between text-[10px] text-ink3 mt-1">
        <span>{stamp(minX)}</span>
        <span>
          最高 {fmt(maxY)} ｜ 最低 {fmt(minY)}
        </span>
        <span>{stamp(lastX)}</span>
      </div>
    </>
  );
}

/// 近 24h 斜率 → 预计耗尽时间（样本不足或无明显消耗时给文案说明）
function trendText(data: UsageSample[], currentRemain: number | null): string | null {
  if (data.length < 3) return null;
  const last = data[data.length - 1];
  const win = data.filter((d) => d.ts >= last.ts - 24 * 3_600_000);
  if (win.length < 2) return null;
  const first = win[0];
  const hours = (last.ts - first.ts) / 3_600_000;
  const used = first.remain - last.remain;
  if (hours <= 0) return null;
  if (used <= 0) return "最近 24 小时消耗较少";
  if (currentRemain === null) return `最近 ${hours.toFixed(1)} 小时消耗 ${fmt(used)}`;
  const rate = used / hours;
  const etaHours = last.remain / rate;
  return `最近 ${hours.toFixed(1)} 小时消耗 ${fmt(used)}，按此速率预计 ${
    etaHours > 72 ? `${(etaHours / 24).toFixed(1)} 天` : `${etaHours.toFixed(1)} 小时`
  } 后耗尽（当前可用 ${fmt(currentRemain)}）`;
}

function KeyVals({ data, max = 12 }: { data: Record<string, unknown>; max?: number }) {
  const rows = Object.entries(data)
    .filter(([, v]) => v === null || typeof v !== "object")
    .slice(0, max);
  if (rows.length === 0) return null;
  return (
    <div className="grid grid-cols-2 gap-x-4 gap-y-1 mt-1">
      {rows.map(([k, v]) => (
        <div key={k} className="flex justify-between text-[11px] text-ink2 border-b border-line-soft py-[3px]">
          <span className="truncate mr-2">{k}</span>
          <b className="font-mono text-ink flex-none">{String(v)}</b>
        </div>
      ))}
    </div>
  );
}

function ItemList({ items, limit = 8 }: { items: unknown[]; limit?: number }) {
  const objs = items.map(obj).filter((o): o is Record<string, unknown> => o !== null);
  if (objs.length === 0) return null;
  return (
    <div className="mt-2 flex flex-col gap-1.5">
      {objs.slice(0, limit).map((o, i) => (
        <div key={i} className="bg-[#fbfdfe] border border-line-soft rounded-[9px] px-3 py-2">
          <KeyVals data={o} max={8} />
        </div>
      ))}
      {objs.length > limit && <div className="text-[10.5px] text-ink3">…共 {objs.length} 项</div>}
    </div>
  );
}

function HostTable({ title, hosts }: { title: string; hosts?: Record<string, string> }) {
  if (!hosts || Object.keys(hosts).length === 0) return null;
  return (
    <div className="bg-[#fbfdfe] border border-line-soft rounded-[9px] px-3 py-2">
      <div className="text-[10.5px] text-ink3 mb-1">{title}</div>
      {Object.entries(hosts).map(([k, v]) => (
        <div key={k} className="flex justify-between text-[11px] border-b border-line-soft py-[3px] last:border-b-0">
          <span className="text-ink2">{k}</span>
          <b className="font-mono text-ink truncate ml-2" title={v}>
            {v}
          </b>
        </div>
      ))}
    </div>
  );
}

function Chip({ text, tone = "dim" }: { text: string; tone?: "ok" | "accent" | "dim" | "bad" }) {
  const cls =
    tone === "ok"
      ? "bg-[#e9f9ef] border-[#c3ecd2] text-[#1d9d54]"
      : tone === "accent"
        ? "bg-[#eef2ff] border-[#d4dcff] text-[#4f5fd7]"
        : tone === "bad"
          ? "bg-[#fdeeee] border-[#f6cfcf] text-[#d24545]"
          : "bg-[#fbfdfe] border-line text-ink2";
  return <span className={`text-[10px] px-1.5 py-0.5 rounded-full border flex-none ${cls}`}>{text}</span>;
}

function DeviceCell({ label, value, hint }: { label: string; value?: string; hint: string }) {
  return (
    <div className="bg-[#fbfdfe] border border-line-soft rounded-[9px] px-3 py-2">
      <div className="text-[10.5px] text-ink3">{label}</div>
      <div className="text-[13px] font-mono text-ink truncate" title={value}>
        {value || "—"}
      </div>
      <div className="text-[10px] text-ink3 mt-0.5">{hint}</div>
    </div>
  );
}

export function AssetsPage({
  accounts,
  targetIde,
  assets,
  usage,
  usageBusy,
  usageErr,
  usageAt,
  onRefreshUsage,
  onRefresh,
  onToast,
  onReload,
}: AssetsPageProps) {
  const pool = useMemo(() => accounts.filter((a) => a.ide === targetIde), [accounts, targetIde]);
  const [id, setId] = useState("");
  const [refreshing, setRefreshing] = useState(false);
  const [claiming, setClaiming] = useState(false);
  const [advanced, setAdvanced] = useState(false);
  const [icdrs, setIcdrs] = useState<IcdrsLog | null>(null);
  /** 设备身份管理：该版本全部账号的随号设备资产（本地账号库读取，不发网络请求） */
  const [devices, setDevices] = useState<AccountDevice[] | null>(null);
  const [cfg, setCfg] = useState<RuntimeConfig | null>(null);
  const [verdict, setVerdict] = useState<VerifyResult | null>(null);
  const [verifying, setVerifying] = useState(false);
  /** P1-1 该账号的额度采样序列（本地文件读取，进页面即取） */
  const [series, setSeries] = useState<UsageSample[]>([]);
  const [cmds, setCmds] = useState<string[] | null>(null);
  const [cmdLoading, setCmdLoading] = useState(false);
  const [cmdFilter, setCmdFilter] = useState("");
  const [cmdSel, setCmdSel] = useState("");
  const [cmdArgsText, setCmdArgsText] = useState("[]");
  const [cmdResult, setCmdResult] = useState<string>("");
  const [cmdRunning, setCmdRunning] = useState(false);

  // 目标版本切换后账号池变化：当前选中项失效则回落到首个
  useEffect(() => {
    if (!pool.some((a) => a.id === id)) setId(pool[0]?.id ?? "");
  }, [pool, id]);

  // 选中账号变化只清校验结论；资产来自外层缓存，不做任何拉取
  useEffect(() => {
    setVerdict(null);
  }, [id]);

  // P1-1 采样序列：本地文件读取，随账号切换重新取
  useEffect(() => {
    let disposed = false;
    if (!id) {
      setSeries([]);
      return;
    }
    usageHistory(id)
      .then((s) => {
        if (!disposed) setSeries(s);
      })
      .catch(() => {
        if (!disposed) setSeries([]);
      });
    return () => {
      disposed = true;
    };
  }, [id]);

  useEffect(() => {
    let disposed = false;
    setIcdrs(null);
    setCfg(null);
    setCmds(null);
    setCmdResult("");
    setDevices(null);
    icdrsLog(targetIde)
      .then((v) => {
        if (!disposed) setIcdrs(v);
      })
      .catch(() => {
        if (!disposed) setIcdrs(null);
      });
    accountDevices(targetIde)
      .then((v) => {
        if (!disposed) setDevices(v);
      })
      .catch(() => {
        if (!disposed) setDevices(null);
      });
    runtimeConfig(targetIde)
      .then((v) => {
        if (!disposed) setCfg(v);
      })
      .catch(() => {
        if (!disposed) setCfg(null);
      });
    return () => {
      disposed = true;
    };
  }, [targetIde]);

  const current = pool.find((a) => a.id === id);
  // 资产来自外层缓存（后台预取），进入页面直接渲染既有结果
  const bundle = id ? assets[id] ?? null : null;

  const load = async () => {
    if (!id) return;
    setRefreshing(true);
    try {
      const b = await onRefresh(id);
      const failed = Object.keys(b.errors ?? {});
      if (failed.length > 0) onToast("info", "部分服务接口未返回数据", failed.join("、"));
      else onToast("success", "账号数据已刷新");
    } catch (e) {
      onToast("error", "账号数据刷新失败", errText(e));
    } finally {
      setRefreshing(false);
    }
  };

  const claim = async () => {
    if (!id) return;
    setClaiming(true);
    try {
      const r = await claimActivityRewards(id);
      const landed = r.landed ?? [];
      if (r.grantable.length === 0) {
        onToast("info", "无可领取的奖励", "这些活动当前不可领取，可能已领取过");
      } else if (landed.length > 0) {
        onToast("success", `已到账 ${landed.length}/${r.grantable.length}`, landed.join("、"));
      } else {
        // 实测：接口返回 200 仅代表受理；服务端不发放时 granted 不会翻转
        onToast("info", "已提交，但暂未到账", `${r.grantable.join("、")} 需先满足条件（例如完成学生认证）后方可到账`);
      }
      await load();
    } catch (e) {
      onToast("error", "领取奖励失败", errText(e));
    } finally {
      setClaiming(false);
    }
  };

  const verify = async () => {
    if (!id) return;
    setVerifying(true);
    try {
      const v = await verifyAccount(id);
      setVerdict(v);
      onReload?.();
      if (v.valid === true) onToast("success", "登录状态正常", v.reason);
      else if (v.valid === false) onToast("error", "登录已失效", v.reason);
      else onToast("info", "无法判断", v.reason);
    } catch (e) {
      onToast("error", "检查失败", errText(e));
    } finally {
      setVerifying(false);
    }
  };

  const loadCommands = async () => {
    setCmdLoading(true);
    try {
      const list = await shimCommands(targetIde);
      setCmds(list);
      setCmdSel((prev) => (prev && list.includes(prev) ? prev : list[0] ?? ""));
      onToast("success", `已列出 ${list.length} 条命令`);
    } catch (e) {
      setCmds(null);
      onToast("error", "命令列表读取失败", errText(e));
    } finally {
      setCmdLoading(false);
    }
  };

  const runCommand = async () => {
    if (!cmdSel) return;
    let args: unknown[];
    try {
      const parsed = JSON.parse(cmdArgsText || "[]");
      args = Array.isArray(parsed) ? parsed : [parsed];
    } catch {
      onToast("error", "参数不是合法 JSON 数组");
      return;
    }
    setCmdRunning(true);
    setCmdResult("");
    try {
      setCmdResult(dump(await shimInvoke(targetIde, cmdSel, args)));
    } catch (e) {
      setCmdResult(errText(e));
      onToast("error", "命令调用失败", errText(e));
    } finally {
      setCmdRunning(false);
    }
  };

  const entitlement = bundle?.data?.entitlement;
  const payStatus = bundle?.data?.pay_status;
  const activity = bundle?.data?.activity;
  const billing = bundle?.data?.billing;
  const products = bundle?.data?.products;

  const { sources, dollar } = packsToSources(entitlement);
  const acts = collectActivities(activity);
  const grantable = acts.filter((a) => a.grantable && !a.granted);
  // 国际版无活动中心（后端已跳过请求），UI 需据此说明并禁用领奖入口
  const activitySupported = bundle?.activitySupported ?? targetIde === "trae-cn";
  const billingRoot = obj(billing);
  const billingList = billingRoot && Array.isArray(billingRoot.data_list) ? billingRoot.data_list : [];
  const prodRoot = obj(products);
  const prodList = prodRoot && Array.isArray(prodRoot.products) ? prodRoot.products : [];
  const cmdVisible = (cmds ?? []).filter((c) => !cmdFilter || c.includes(cmdFilter));

  const deviceOk = !!icdrs?.found && !!icdrs.did && icdrs.did === icdrs.rdid;
  const incomplete = (devices ?? []).filter((d) => d.missing.length > 0).length;
  const usageRows = usage?.sessions ?? [];
  const usageTotal = usageRows.reduce((sum, r) => sum + r.totalTokens, 0);

  return (
    <div className="page-in flex-1 min-w-0 p-6 overflow-y-auto">
      <div className="flex items-center mb-[18px]">
        <h3 className="text-[17px] font-bold">积分与诊断</h3>
        <span
          className={`text-[12px] font-semibold px-2.5 py-0.5 rounded-full border ml-3 ${
            targetIde === "trae-cn" ? "bg-[#fff4e8] border-[#ffe0bd] text-[#c2731a]" : "bg-[#fbfdfe] border-line text-ink2"
          }`}
        >
          {targetIde === "trae" ? "Trae 国际版" : "Trae CN"}
        </span>
        <span className="text-[12px] text-ink3 ml-3">积分、会话用量、活动奖励、订阅能力、设备身份</span>
      </div>

      <div className="flex items-center gap-2.5 mb-4">
        <span className="text-[11.5px] text-ink3 flex-none">当前账号</span>
        <Select
          className="w-[250px]"
          value={id}
          onChange={setId}
          placeholder="（该版本暂无账号）"
          options={pool.map((a) => ({ value: a.id, label: `${a.name} · ${a.email}` }))}
        />
        <button className="btn-ghost" disabled={!id || refreshing} onClick={load} title="重新读取该账号数据（后台自动读取）">
          <IconRefresh /> {refreshing ? "刷新中…" : "刷新"}
        </button>
        <button
          className="btn-primary"
          disabled={!id || claiming || !activitySupported}
          onClick={claim}
          title={
            activitySupported
              ? "通过 Trae 自带入口领取奖励（需 Trae 处于运行中，且已安装无感换号支持）"
              : "国际版无活动中心（签到和奖励活动仅 Trae CN 提供）"
          }
        >
          {claiming ? "领取中…" : `一键领取活动奖励${grantable.length ? `（${grantable.length}）` : ""}`}
        </button>
        <button
          className="btn-ghost"
          disabled={!id || verifying}
          onClick={verify}
          title="通过额度服务接口检查账号登录是否有效，并更新账号状态"
        >
          {verifying ? "检查中…" : "检查登录状态"}
        </button>
        {verdict && (
          <Chip
            text={verdict.valid === true ? "登录状态正常" : verdict.valid === false ? "登录已失效" : "无法判断"}
            tone={verdict.valid === true ? "ok" : "dim"}
          />
        )}
      </div>

      {!bundle ? (
        <div className="flex flex-col items-center justify-center h-[300px] gap-3 border border-dashed border-line rounded-[12px]">
          {refreshing ? (
            <>
              <span className="w-5 h-5 rounded-full border-2 border-accent border-t-transparent animate-spin" />
              <div className="text-[12px] text-ink3">正在读取账号数据…</div>
            </>
          ) : (
            <>
              <div className="text-[13px] font-semibold">暂无账号数据</div>
              <div className="text-[11.5px] text-ink3">
                {pool.length === 0
                  ? "该版本暂无账号，请先前往「账号池」页面添加账号"
                  : "数据由后台自动读取；若刚启动应用或读取失败，可点击下方按钮手动读取"}
              </div>
              <button className="btn-ghost" disabled={!id || refreshing} onClick={load}>
                <IconRefresh /> 立即读取
              </button>
            </>
          )}
        </div>
      ) : (
        <div className="stagger-in flex flex-col gap-3.5">
          <Section title={dollar ? "我的额度" : "我的积分"} badge={`账号 ${bundle.name}`} raw={entitlement}>
            {sources.length === 0 ? (
              <div className="text-[11.5px] text-ink3">{dollar ? "未读取到额度信息" : "未读取到积分信息"}</div>
            ) : (
              <>
                <CreditBars sources={sources} dollar={dollar} />
                {series.length >= 2 && (
                  <div className="mt-3 pt-3 border-t border-line-soft">
                    <div className="text-[11px] text-ink3">
                      {dollar ? "额度消耗趋势" : "积分消耗趋势"} · 最近 {series.length} 次记录
                    </div>
                    <Spark data={series} />
                    {(() => {
                      const hasUnlimited = sources.some((s) => s.unlimited);
                      const currentRemain = hasUnlimited
                        ? null
                        : sources.filter((s) => !s.unlimited).reduce((a, s) => a + s.remaining, 0);
                      const t = trendText(series, currentRemain);
                      return t ? <div className="text-[11.5px] text-ink2 mt-1">{t}</div> : null;
                    })()}
                  </div>
                )}
              </>
            )}
          </Section>

          <Section
            title="会话用量"
            badge={
              usageBusy && !usage
                ? "读取中…"
                : usage
                  ? `已读取 ${usageRows.length}/${usage.totalCount} 个会话 · 共消耗 ${fmt(usageTotal)} tokens`
                  : "未读取"
            }
            action={
              <button
                className="btn-ghost ml-3"
                disabled={usageBusy}
                onClick={() => onRefreshUsage(true)}
                title="从 Trae 本地读取当前登录账号的会话及每轮 token 用量（启动、切换版本、切换账号时自动读取，并每 30 秒刷新一次）"
              >
                {usageBusy ? "读取中…" : "刷新"}
              </button>
            }
          >
            {!usage ? (
              <div className="text-[11.5px] text-ink3">
                {usageErr
                  ? `自动读取失败：${usageErr}`
                  : "正在自动读取：从 Trae 本地读取每个会话的 token 用量。需 Trae 处于运行中、切换助手已启用，且无感换号支持文件已重新安装并在重启 Trae 后生效。"}
              </div>
            ) : usageRows.length === 0 ? (
              <div className="text-[11.5px] text-ink3">
                未读取到会话记录（该登录账号在所属项目下无会话，或 Trae 本地数据尚未就绪）。
              </div>
            ) : (
              <>
                <div className="border border-line-soft rounded-[10px] overflow-hidden">
                  <div className="flex items-center gap-3 px-3 py-2 bg-[#fbfdfe] text-[10.5px] text-ink3">
                    <span className="flex-1">会话</span>
                    <span className="w-[104px] flex-none">更新时间</span>
                    <span className="w-[46px] flex-none text-right">轮次</span>
                    <span className="w-[92px] flex-none text-right">输入</span>
                    <span className="w-[92px] flex-none text-right">输出</span>
                    <span className="w-[100px] flex-none text-right">合计</span>
                    <span className="w-[100px] flex-none text-right">缓存命中</span>
                    <span className="w-[96px] flex-none text-right">上下文</span>
                  </div>
                  {usageRows.map((r) => (
                    <div key={r.sessionId} className="flex items-center gap-3 px-3 py-2 border-t border-line-soft">
                      <span
                        className="flex-1 text-[11.5px] text-ink truncate"
                        title={`${r.title || r.sessionId}\n${r.sessionId}`}
                      >
                        {r.title || r.sessionId}
                        {r.truncated && (
                          <span className="text-[10px] text-[#c2731a] ml-1" title="该会话存在未统计的消息（单次最多读取 400 条）">
                            ·仅统计前 400 条
                          </span>
                        )}
                      </span>
                      <span className="w-[104px] flex-none text-[11px] text-ink3 truncate">
                        {r.updatedAt ? stamp(r.updatedAt) : "—"}
                      </span>
                      <span className="w-[46px] flex-none text-right font-mono text-[11px] text-ink2">{r.turns}</span>
                      <span className="w-[92px] flex-none text-right font-mono text-[11px] text-ink2">
                        {fmt(r.promptTokens)}
                      </span>
                      <span className="w-[92px] flex-none text-right font-mono text-[11px] text-ink2">
                        {fmt(r.completionTokens)}
                      </span>
                      <span className="w-[100px] flex-none text-right font-mono text-[11.5px] font-semibold text-ink">
                        {fmt(r.totalTokens)}
                      </span>
                      <span className="w-[100px] flex-none text-right font-mono text-[11px] text-ink2">
                        {r.cacheReadTokens > 0 ? fmt(r.cacheReadTokens) : "—"}
                      </span>
                      <span
                        className="w-[96px] flex-none text-right font-mono text-[11px] text-ink2"
                        title={`最后一轮上下文占用 / 模型上限 ${fmt(r.lastTurnTotalTokens)} / ${fmt(r.maxTokens)}`}
                      >
                        {r.maxTokens > 0 ? `${((r.lastTurnTotalTokens / r.maxTokens) * 100).toFixed(1)}%` : "—"}
                      </span>
                    </div>
                  ))}
                </div>
                <div className="text-[11px] text-ink3 mt-2">
                  统计方式：每轮 token 用量从 Trae 本地记录读取，按会话累加；缓存命中 = 复用已发送的上下文，
                  不重复计费；上下文 = 最后一轮总 token / 模型上限。
                  这些会话属于项目 <span className="font-mono">{usage.projectId || "—"}</span>
                  {usage.totalCount > usageRows.length && `（仅列出最近 ${usageRows.length} 个）`}。
                </div>
                {usage.errors.length > 0 && (
                  <div className="text-[11px] text-bad mt-1">
                    未读取到的会话：{usage.errors.map((e) => e.sessionId).join("、")}
                  </div>
                )}
                {usageAt > 0 && (
                  <div className="text-[11px] text-ink3 mt-1">
                    最近一次自动读取：
                    {new Date(usageAt).toLocaleTimeString("zh-CN", { hour12: false })}
                    {usageBusy ? "（刷新中…）" : ""}
                  </div>
                )}
              </>
            )}
          </Section>

          <Section
            title="活动与奖励"
            badge={!activitySupported ? "仅 Trae CN 提供" : acts.length ? `${acts.length} 项 · 可领取 ${grantable.length}` : undefined}
            raw={activity}
          >
            {!activitySupported ? (
              <div className="text-[11.5px] text-ink3">
                国际版无活动中心 —— 签到和活动奖励仅 Trae CN 提供；本页的积分、订阅能力等内容不受影响，可正常使用。
              </div>
            ) : acts.length === 0 ? (
              <div className="text-[11.5px] text-ink3">未读取到活动信息</div>
            ) : (
              <div className="flex flex-col gap-2">
                {acts.map((a, i) => {
                  const meta = ACTIVITIES[a.id] ?? { name: a.id || "活动", note: "" };
                  const state = a.granted
                    ? { text: "已领取", tone: "ok" as const }
                    : a.grantable
                      ? { text: "可领取", tone: "accent" as const }
                      : a.eligible === false
                        ? { text: "未达标", tone: "dim" as const }
                        : { text: "暂不可领取", tone: "dim" as const };
                  return (
                    <div
                      key={`${a.id}-${i}`}
                      className={`rounded-[10px] border px-3 py-2.5 ${
                        a.grantable && !a.granted ? "border-accent-line bg-[#f3fbf9]" : "border-line-soft bg-[#fbfdfe]"
                      }`}
                    >
                      <div className="flex items-center gap-2">
                        <span className="text-[12.5px] font-semibold truncate">{meta.name}</span>
                        {a.work > 0 && <Chip text={`工作积分 ${fmt(a.work)}`} />}
                        {a.general > 0 && <Chip text={`通用积分 ${fmt(a.general)}`} />}
                        {a.autoGrant && <Chip text="自动发放" />}
                        <span className="ml-auto flex-none">
                          <Chip text={state.text} tone={state.tone} />
                        </span>
                      </div>
                      {meta.note && <div className="text-[11px] text-ink3 mt-1">{meta.note}</div>}
                    </div>
                  );
                })}
              </div>
            )}
          </Section>

          <Section title="订阅能力" badge={prodList.length ? `${prodList.length} 个产品` : undefined} raw={products}>
            {prodList.length === 0 ? (
              <div className="text-[11.5px] text-ink3">未读取到订阅产品</div>
            ) : (
              <div className="flex flex-col gap-2">
                {prodList.map((p, i) => {
                  const o = obj(p) ?? {};
                  const se = obj(o.subscription_extra);
                  const quota = se ? obj(se.quota) : null;
                  const caps = quota
                    ? Object.entries(quota)
                        .filter(([, v]) => v === true)
                        .map(([k]) => k)
                    : [];
                  return (
                    <div key={i} className="flex items-center gap-2 border-b border-line-soft pb-2 last:border-b-0 last:pb-0">
                      <span className="text-[12.5px] font-semibold truncate">
                        {str(o.product_name ?? o.name ?? o.title) || `产品 ${str(o.product_id) || i}`}
                      </span>
                      {(str(o.display_price) || str(o.currency)) && (
                        <span className="text-[11px] text-ink3 font-mono">
                          {str(o.display_price) || str(o.currency)}
                        </span>
                      )}
                      <span className="ml-auto flex flex-wrap gap-1 justify-end">
                        {caps.length === 0 ? (
                          <Chip text="无额外功能" />
                        ) : (
                          caps.map((c) => <Chip key={c} text={capLabel(c)} tone="accent" />)
                        )}
                      </span>
                    </div>
                  );
                })}
              </div>
            )}
          </Section>

          <Section
            title="设备身份"
            badge={
              (devices === null
                ? "读取中"
                : devices.length === 0
                  ? "无账号"
                  : incomplete === 0
                    ? `${devices.length} 个账号的设备身份完整`
                    : `${incomplete} 个账号的设备身份不完整`) + (bundle.device ? ` · 设备编号 ${bundle.device}` : "")
            }
            action={
              <button
                className="btn-ghost ml-3"
                onClick={() =>
                  accountDevices(targetIde)
                    .then(setDevices)
                    .catch((e) => onToast("error", "设备身份读取失败", errText(e)))
                }
              >
                刷新
              </button>
            }
          >
            {devices === null ? (
              <div className="text-[11.5px] text-ink3">正在读取账号的设备身份…</div>
            ) : devices.length === 0 ? (
              <div className="text-[11.5px] text-ink3">该版本暂无账号，添加账号后将自动记录其设备身份。</div>
            ) : (
              <>
                <div className="text-[11.5px] text-ink3 mb-2">
                  每个账号应携带独立的设备身份（设备编号 / 密钥对 / 机器标识），切换账号时一并切换。
                  信息不完整的账号（较早版本添加）沿用本机当前设备身份，设备身份不会互相混用；
                  密钥指纹相同，表示这些账号在服务器侧视为同一台设备。
                </div>
                <div className="border border-line-soft rounded-[10px] overflow-hidden">
                  <div className="flex items-center gap-3 px-3 py-2 bg-[#fbfdfe] text-[10.5px] text-ink3">
                    <span className="flex-1">账号</span>
                    <span className="w-[150px] flex-none">设备编号</span>
                    <span className="w-[104px] flex-none">密钥指纹</span>
                    <span className="w-[240px] flex-none">设备身份完整度</span>
                  </div>
                  {devices.map((d) => (
                    <div key={d.id} className="flex items-center gap-3 px-3 py-2 border-t border-line-soft">
                      <span className="flex-1 text-[11.5px] text-ink truncate" title={`${d.name}（${d.id}）`}>
                        {d.name}
                      </span>
                      <span
                        className="w-[150px] flex-none font-mono text-[11px] text-ink2 truncate"
                        title={d.deviceIdStr ?? "未读取"}
                      >
                        {d.deviceIdStr ?? "—"}
                      </span>
                      <span
                        className="w-[104px] flex-none font-mono text-[11px] text-ink2 truncate"
                        title={`${d.ecdsaKeyCount} 个设备密钥 · ${d.ecdsaFingerprint ?? "未读取"}`}
                      >
                        {d.ecdsaFingerprint ?? "—"}
                      </span>
                      <span className="w-[240px] flex-none flex flex-wrap gap-1 justify-end">
                        <Chip text={`设备编号 ${d.deviceIdStr ? "✓" : "✗"}`} tone={d.deviceIdStr ? "ok" : "bad"} />
                        <Chip text={`密钥 ${d.hasEcdsa ? "✓" : "✗"}`} tone={d.hasEcdsa ? "ok" : "bad"} />
                        <Chip text={`快照 ${d.hasTiny ? "✓" : "✗"}`} tone={d.hasTiny ? "ok" : "bad"} />
                        <Chip text={`开发标识 ${d.hasDev ? "✓" : "✗"}`} tone={d.hasDev ? "ok" : "bad"} />
                        <Chip text={`统计标识 ${d.hasSqm ? "✓" : "✗"}`} tone={d.hasSqm ? "ok" : "bad"} />
                        {d.missing.length > 0 && (
                          <span className="text-[10px] text-[#c2731a]" title={`缺失：${d.missing.join("、")}`}>
                            缺 {d.missing.length} 项
                          </span>
                        )}
                      </span>
                    </div>
                  ))}
                </div>
              </>
            )}
            <div className="mt-3 pt-3 border-t border-line-soft">
              <div className="text-[11px] text-ink3 mb-1.5">
                Trae 实际使用的设备身份（取自该版本最近一次启动日志）
              </div>
              {!icdrs || !icdrs.found ? (
                <div className="text-[11.5px] text-ink3">{icdrs?.reason || "暂无记录（需先启动一次 Trae）"}</div>
              ) : (
                <>
                  <div className="grid grid-cols-3 gap-3">
                    <DeviceCell label="当前生效的设备号" value={icdrs.did} hint="服务器和本机共同认定的设备" />
                    <DeviceCell label="本机记录的设备号" value={icdrs.ldid} hint="本机保存的注册身份" />
                    <DeviceCell label="服务器返回的设备号" value={icdrs.rdid} hint="服务器下发的设备号" />
                  </div>
                  <div className={`mt-2 text-[11.5px] ${deviceOk ? "text-[#1d9d54]" : "text-[#c2731a]"}`}>
                    {deviceOk
                      ? "本机设备号和服务器记录一致：设备身份正常。"
                      : "本机设备号和服务器记录不一致：设备可能被重新注册过，容易触发账号异常，建议留意。"}
                  </div>
                </>
              )}
            </div>
          </Section>

          <div className="bg-card border border-line rounded-[12px] shadow-card px-4 py-3">
            <div className="flex items-center gap-3">
              <span className="text-[13px] font-semibold">高级 / 开发者</span>
              <span className="text-[11px] text-ink3">
                原始服务接口响应、运行时主机表、562 条可调用命令、错误码字典
              </span>
              <button className="ml-auto btn-ghost" onClick={() => setAdvanced((v) => !v)}>
                {advanced ? "收起" : "展开"}
              </button>
            </div>
          </div>

          {advanced && (
            <div className="dropdown-in flex flex-col gap-3.5">
              <Section title="账单流水" badge={billingList.length ? `${billingList.length} 条` : "无记录"} raw={billing}>
                {billingList.length === 0 ? (
                  <div className="text-[11.5px] text-ink3">该账号暂无账单记录</div>
                ) : (
                  <>
                    {billingRoot?.pi_manage_url ? (
                      <div className="text-[10.5px] text-ink3 mb-1">发票管理：{String(billingRoot.pi_manage_url)}</div>
                    ) : null}
                    <ItemList items={billingList} limit={10} />
                  </>
                )}
              </Section>

              <Section title="付费状态（原始）" raw={payStatus}>
                {!payStatus ? (
                  <div className="text-[11.5px] text-ink3">尚未读取</div>
                ) : (
                  <>
                    {(() => {
                      const root = obj(payStatus);
                      const detail = root ? obj(root.detail) : null;
                      return detail ? <KeyVals data={detail} /> : null;
                    })()}
                    <div className="text-[11.5px] text-ink3 mt-1">完整字段见「原始数据」</div>
                  </>
                )}
              </Section>

              <Section title="运行时配置" badge={cfg ? cfg.applicationName : "未读取"} raw={cfg ?? undefined}>
                {!cfg ? (
                  <div className="text-[11.5px] text-ink3">未读取（需先识别该版本安装位置）。</div>
                ) : (
                  <>
                    <div className="grid grid-cols-4 gap-3">
                      <DeviceCell label="appVersion" value={cfg.appVersion} hint="x-app-version 来源" />
                      <DeviceCell label="tronBuildVersion" value={cfg.tronBuildVersion} hint="登录 URL plugin_version" />
                      <DeviceCell label="quality" value={cfg.quality} hint="x-app-type" />
                      <DeviceCell label="ug appId" value={cfg.ugAppId} hint="增长侧服务接口身份" />
                    </div>
                    <div className="grid grid-cols-2 gap-3 mt-2">
                      <DeviceCell label="生效 account 主机（付费/续期）" value={cfg.effectiveAccountHost} hint="product.json 解析" />
                      <DeviceCell label="生效 ug 主机（签到/活动）" value={cfg.effectiveUgHost} hint="与 account 表不同" />
                    </div>
                    <div className="grid grid-cols-2 gap-3 mt-2">
                      <HostTable title="account 主机表" hosts={cfg.accountHosts} />
                      <HostTable title="ug 主机表" hosts={cfg.ugHosts} />
                    </div>
                    <div className="mt-2">
                      <HostTable title="iCube 主机表" hosts={cfg.iCubeHosts} />
                    </div>
                  </>
                )}
              </Section>

              <Section
                title="可调用命令（能力字典）"
                badge={cmds ? `${cmds.length} 条` : "未列出"}
                action={
                  <button className="btn-ghost ml-3" disabled={cmdLoading} onClick={loadCommands}>
                    {cmdLoading ? "读取中…" : "列出命令"}
                  </button>
                }
              >
                {!cmds ? (
                  <div className="text-[11.5px] text-ink3">
                    切换助手安装并启用后，可列出 Trae 中注册的全部 trae/icube 命令，并调用其中只读、可重复执行的命令。
                  </div>
                ) : (
                  <>
                    <div className="flex items-center gap-2.5">
                      <input
                        className="flex-none w-[240px] h-[30px] bg-white border border-line rounded-[10px] px-3 text-[12px] outline-none"
                        placeholder="过滤命令名…"
                        value={cmdFilter}
                        onChange={(e) => setCmdFilter(e.target.value)}
                      />
                      <span className="text-[11px] text-ink3">匹配 {cmdVisible.length} 条</span>
                    </div>
                    <div className="mt-2 grid grid-cols-2 gap-x-4 gap-y-[3px] max-h-[240px] overflow-y-auto">
                      {cmdVisible.slice(0, 200).map((c) => (
                        <button
                          key={c}
                          className={`text-left font-mono text-[11px] px-1.5 py-[3px] rounded-[6px] cursor-pointer hover:bg-[#f0f6f7] ${
                            c === cmdSel ? "bg-accent-soft text-accent" : "text-ink2"
                          }`}
                          onClick={() => setCmdSel(c)}
                          title={c}
                        >
                          {c}
                        </button>
                      ))}
                    </div>
                    <div className="mt-3 bg-[#fbfdfe] border border-line-soft rounded-[9px] p-3">
                      <div className="text-[10.5px] text-ink3 mb-1.5">
                        调用白名单内的只读/幂等命令（写、删除、分叉类命令将被拒绝）
                      </div>
                      <div className="flex flex-wrap gap-1.5 mb-2">
                        {SESSION_PRESETS.map((p) => (
                          <button
                            key={p.command}
                            className="text-[10.5px] px-2 py-[3px] rounded-full border border-line bg-white text-ink2 cursor-pointer hover:border-accent hover:text-accent"
                            onClick={() => {
                              setCmdSel(p.command);
                              setCmdArgsText(p.args);
                            }}
                          >
                            {p.label}
                          </button>
                        ))}
                      </div>
                      <div className="flex items-center gap-2.5">
                        <input
                          className="flex-1 min-w-0 h-[30px] bg-white border border-line rounded-[9px] px-2.5 font-mono text-[11.5px] outline-none"
                          value={cmdSel}
                          onChange={(e) => setCmdSel(e.target.value)}
                          placeholder="命令 id"
                        />
                        <input
                          className="flex-none w-[200px] h-[30px] bg-white border border-line rounded-[9px] px-2.5 font-mono text-[11.5px] outline-none"
                          value={cmdArgsText}
                          onChange={(e) => setCmdArgsText(e.target.value)}
                          placeholder='参数 JSON 数组，如 ["key"]'
                        />
                        <button className="btn-primary" disabled={!cmdSel || cmdRunning} onClick={runCommand}>
                          {cmdRunning ? "调用中…" : "调用"}
                        </button>
                      </div>
                      {cmdResult && (
                        <pre className="mt-2 bg-[#f7fafb] border border-line-soft rounded-[9px] p-3 text-[10.5px] font-mono text-ink2 overflow-x-auto max-h-[240px]">
                          {cmdResult}
                        </pre>
                      )}
                    </div>
                  </>
                )}
              </Section>

              <Section title="错误码字典">
                <div className="flex flex-col">
                  {ERROR_CODES.map((e) => (
                    <div key={e.code} className="flex gap-3 py-2 border-b border-line-soft last:border-b-0">
                      <div className="w-[168px] flex-none font-mono text-[11px] text-bad">{e.code}</div>
                      <div className="min-w-0 flex-1">
                        <div className="text-[11.5px] text-ink">{e.meaning}</div>
                        <div className="text-[10.5px] text-ink3 mt-0.5">出处：{e.source}</div>
                        <div className="text-[10.5px] text-ink2 mt-0.5">处置：{e.action}</div>
                      </div>
                    </div>
                  ))}
                </div>
              </Section>
            </div>
          )}
        </div>
      )}

      {current && (
        <div className="text-[11px] text-ink3 mt-4">
          当前选中：{current.name}（{current.ide === "trae-cn" ? "Trae CN" : "Trae"}）
          {bundle && Object.keys(bundle.errors).length > 0 && (
            <span className="ml-2 text-bad">未返回的服务接口：{Object.keys(bundle.errors).join("、")}</span>
          )}
        </div>
      )}
    </div>
  );
}
