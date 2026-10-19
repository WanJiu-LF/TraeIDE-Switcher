import { useState } from "react";
import type { Account, AccountStatus, Ide, Plan } from "../types";
import type { IdeStatus } from "../bridge";
import { AccountCard } from "../components/AccountCard";
import { Select } from "../components/Select";
import { IconPlus, IconSearch } from "../icons";

export interface AddTabRequest {
  tab: "paste" | "scan" | "login";
}

interface AccountsPageProps {
  accounts: Account[];
  currentId: string;
  switchingId: string | null;
  targetIde: Ide;
  ideStatus?: IdeStatus | null;
  onAdd: (req: AddTabRequest) => void;
  onSwitch: (id: string) => void;
  onEdit: (id: string) => void;
  onDelete: (id: string) => void;
  onRefresh: (id: string) => void;
  claimingId: string | null;
  onClaimCheckin: (id: string) => void;
  /** P0-6 置顶/取消置顶 */
  onPin: (id: string, pinned: boolean) => void;
  /** P0-4 重新登录采集（同账号重登会原地覆盖凭据） */
  onRelogin: () => void;
  /** 多开：独立 user-data 实例启动该账号 */
  onLaunchIsolated: (id: string) => void;
  /** P0-2 批量操作 */
  onBatch: (action: "refresh" | "checkin" | "verify" | "delete", ids: string[]) => void;
  batchBusy: string | null;
}

function StatCard({
  label,
  value,
  sub,
  highlight,
  mono = true,
}: {
  label: string;
  value: string;
  sub: string;
  highlight?: boolean;
  mono?: boolean;
}) {
  return (
    <div
      className={`bg-card border rounded-[12px] px-4 py-3.5 shadow-card overflow-hidden ${
        highlight ? "bg-gradient-to-br from-[#f0fbf9] to-white border-accent-line" : "border-line"
      }`}
    >
      <div className="text-[11.5px] text-ink3 truncate" title={label}>
        {label}
      </div>
      {/* 名字/数字都可能很长（如 user3336560160093 这类国际版自动名）：必须截断，
          否则会顶出卡片边界；完整内容走 title 悬停查看 */}
      <div
        className={`mt-[3px] text-[21px] font-bold truncate ${mono ? "font-mono" : ""} ${highlight ? "text-accent" : ""}`}
        title={value}
      >
        {value}
      </div>
      <div className="text-[11px] text-ink3 mt-[3px] truncate" title={sub}>
        {sub}
      </div>
    </div>
  );
}

export function AccountsPage(props: AccountsPageProps) {
  const {
    accounts,
    currentId,
    switchingId,
    targetIde,
    ideStatus,
    onAdd,
    onSwitch,
    onEdit,
    onDelete,
    onRefresh,
    claimingId,
    onClaimCheckin,
    onPin,
    onRelogin,
    onLaunchIsolated,
    onBatch,
    batchBusy,
  } = props;
  const [search, setSearch] = useState("");
  const [statusFilter, setStatusFilter] = useState<"all" | AccountStatus>("all");
  const [planFilter, setPlanFilter] = useState<"all" | Plan>("all");
  const [sort, setSort] = useState<"default" | "remain" | "name">("default");
  const [batchMode, setBatchMode] = useState(false);
  const [sel, setSel] = useState<string[]>([]);

  const current = accounts.find((a) => a.id === currentId);
  const ideName = targetIde === "trae" ? "Trae" : "Trae CN";
  const runtimeSub = !ideStatus?.running
    ? `● ${ideName} 未运行`
    : ideStatus.stalePatch
      ? `● ${ideName} 运行中 · 无感换号支持尚未生效（需重启 Trae 后生效）`
      : ideStatus.tswPatched && ideStatus.shimAlive
        ? `● ${ideName} 运行中 · 无感换号通道就绪`
        : ideStatus.cdpReady
          ? `● ${ideName} 运行中 · 备用通道可用`
          : ideStatus.tswPatched
            ? `● ${ideName} 运行中 · 切换助手未启动`
            : `● ${ideName} 运行中 · 未安装无感换号支持（切换需重启 Trae）`;
  // 账号池跟随左下角"当前目标"切换：只显示目标版本的账号
  const pool = accounts.filter((a) => a.ide === targetIde);
  const visible = pool.filter((a) => {
    if (statusFilter !== "all" && a.status !== statusFilter) return false;
    if (planFilter !== "all" && a.plan !== planFilter) return false;
    if (search && !`${a.name}${a.email}${a.note}`.toLowerCase().includes(search.toLowerCase())) return false;
    return true;
  });

  const isCn = targetIde === "trae-cn";
  const remainOf = (a: Account) =>
    isCn ? a.credits?.total ?? 0 : Math.max(a.daily.total - a.daily.used, 0);
  // 置顶恒在最前，其余按所选维度排序
  const sorted = [...visible].sort((a, b) => {
    if ((a.pinned ?? false) !== (b.pinned ?? false)) return a.pinned ? -1 : 1;
    if (sort === "remain") return remainOf(b) - remainOf(a);
    if (sort === "name") return a.name.localeCompare(b.name, "zh-CN");
    return 0;
  });
  const poolRemain = isCn
    ? pool.reduce((sum, a) => sum + (a.credits?.total ?? 0), 0)
    : pool.reduce((sum, a) => sum + Math.max(a.daily.total - a.daily.used, 0), 0);
  // 国际版额度以美元计（is_dollar_usage_billing）时带 $ 前缀，否则按次数
  const dollar = !isCn && pool.some((a) => a.dollar === true);
  const fmtRemain = (v: number) =>
    isCn
      ? v.toLocaleString("zh-CN", { minimumFractionDigits: 2, maximumFractionDigits: 2 })
      : dollar
        ? `$${v.toFixed(2)}`
        : v.toLocaleString("zh-CN");
  const stamp = (ts: number) => {
    const d = new Date(ts * 1000);
    const p = (n: number) => String(n).padStart(2, "0");
    return `${p(d.getMonth() + 1)}/${p(d.getDate())} ${p(d.getHours())}:${p(d.getMinutes())}`;
  };
  // 下次额度重置：国际版取最早的订阅周期结束时刻；CN 取最近的活动到期时间
  const businessExpire = pool
    .flatMap((a) => a.credits?.sources ?? [])
    .map((s) => s.expire)
    .filter(Boolean)
    .sort()[0];
  const resetTs = pool.reduce(
    (min, a) => (a.resetAt && a.resetAt > 0 && (min === 0 || a.resetAt < min) ? a.resetAt : min),
    0,
  );
  const resetValue = isCn ? (businessExpire ? businessExpire.slice(0, 10).replace(/-/g, "/") : "—") : resetTs > 0 ? stamp(resetTs) : "—";
  const resetSub = isCn ? "积分按活动到期时间失效" : "按订阅周期重置（最早到期账号）";

  return (
    <div className="page-in flex-1 min-w-0 p-6 overflow-y-auto">
      <div className="flex items-center mb-[18px]">
        <h3 className="text-[17px] font-bold">账号池</h3>
        <span
          className={`text-[12px] font-semibold px-2.5 py-0.5 rounded-full border ml-3 ${
            isCn ? "bg-[#fff4e8] border-[#ffe0bd] text-[#c2731a]" : "bg-[#fbfdfe] border-line text-ink2"
          }`}
        >
          {isCn ? "Trae CN" : "Trae 国际版"}
        </span>
        <div className="text-[12px] text-ink3 ml-3">
          {pool.length} 个账号 · 可用 {pool.filter((a) => a.status === "normal").length} · 耗尽{" "}
          {pool.filter((a) => a.status === "exhausted").length} · 异常 {pool.filter((a) => a.status === "error").length}
        </div>
        <div className="ml-auto text-[11px] text-ink3">
          {isCn ? "总积分 " : "剩余额度 "}
          <b className="font-mono text-ink text-[12px]">
            {isCn || dollar ? fmtRemain(poolRemain) : `${poolRemain.toLocaleString("zh-CN")} 次`}
          </b>
        </div>
      </div>

      <div className="stagger-in grid grid-cols-4 gap-3.5 mb-[18px]">
        <StatCard
          highlight
          mono={false}
          label="当前生效账号"
          value={current ? current.name : "—"}
          sub={runtimeSub}
        />
        <StatCard label="账号总数" value={String(pool.length)} sub={`${isCn ? "Trae CN" : "Trae"} 账号`} />
        <StatCard
          label="剩余总量"
          value={isCn ? `${fmtRemain(poolRemain)} 积分` : dollar ? fmtRemain(poolRemain) : `${poolRemain.toLocaleString("zh-CN")} 次`}
          sub={isCn ? "全部 CN 账号积分合计" : "全部账号基础额度合计"}
        />
        <StatCard label="下次额度重置" value={resetValue} sub={resetSub} />
      </div>

      {/* 筛选行：整行占满（搜索框 flex-1 吸收全部剩余宽度，行尾齐平右边界），
          其余子项固定宽度且不可收缩 + whitespace-nowrap；窗口过窄时整体换行而非挤压折字 */}
      <div className="flex items-center gap-2 mb-4 flex-wrap">
        <div className="flex-1 min-w-[130px] h-[34px] bg-white border border-line rounded-[10px] flex items-center gap-2 px-3 text-[12.5px] text-ink3">
          <IconSearch />
          <input
            className="flex-1 min-w-0 outline-none bg-transparent text-ink text-[12.5px]"
            placeholder="搜索名称/邮箱"
            value={search}
            onChange={(e) => setSearch(e.target.value)}
          />
        </div>
        <Select
          className="w-[138px] shrink-0"
          value={statusFilter}
          onChange={(v) => setStatusFilter(v as typeof statusFilter)}
          options={[
            { value: "all", label: "状态：全部" },
            { value: "normal", label: "状态：正常" },
            { value: "exhausted", label: "状态：额度耗尽" },
            { value: "error", label: "状态：登录异常" },
          ]}
        />
        <Select
          className="w-[138px] shrink-0"
          value={planFilter}
          onChange={(v) => setPlanFilter(v as typeof planFilter)}
          options={[
            { value: "all", label: "套餐：全部" },
            { value: "pro", label: "套餐：Pro" },
            { value: "free", label: "套餐：免费版" },
          ]}
        />
        <Select
          className="w-[138px] shrink-0"
          value={sort}
          onChange={(v) => setSort(v as typeof sort)}
          options={[
            { value: "default", label: "排序：默认" },
            { value: "remain", label: "排序：剩余额度" },
            { value: "name", label: "排序：名称" },
          ]}
        />
        <div className="flex-none flex items-center gap-2.5">
          <button
            className={`${batchMode ? "btn-primary" : "btn-ghost"} flex-none whitespace-nowrap`}
            onClick={() => {
              setBatchMode((v) => !v);
              setSel([]);
            }}
          >
            {batchMode ? "退出批量" : "批量操作"}
          </button>
          <button className="btn-primary flex-none whitespace-nowrap" onClick={() => onAdd({ tab: "paste" })}>
            <IconPlus /> 添加账号
          </button>
        </div>
      </div>

      {batchMode && (
        <div className="dropdown-in flex flex-wrap items-center gap-2.5 mb-4 px-3.5 py-2.5 rounded-[10px] border border-accent-line bg-[#f3fbf9]">
          <span className="text-[12px] text-ink2 flex-none whitespace-nowrap">
            已选 {sel.length} / {sorted.length}
          </span>
          <button
            className="btn-ghost flex-none whitespace-nowrap"
            onClick={() => setSel(sel.length === sorted.length ? [] : sorted.map((a) => a.id))}
          >
            {sel.length === sorted.length && sorted.length > 0 ? "取消全选" : "全选"}
          </button>
          <button
            className="btn-ghost flex-none whitespace-nowrap disabled:opacity-50"
            disabled={!sel.length || batchBusy !== null}
            onClick={() => onBatch("refresh", sel)}
          >
            {batchBusy === "refresh" ? "刷新中…" : "批量刷新额度"}
          </button>
          <button
            className="btn-ghost flex-none whitespace-nowrap disabled:opacity-50"
            disabled={!sel.length || batchBusy !== null || !isCn}
            title={isCn ? "仅 CN 账号支持签到" : "国际版无签到活动"}
            onClick={() => onBatch("checkin", sel)}
          >
            {batchBusy === "checkin" ? "签到中…" : "批量签到"}
          </button>
          <button
            className="btn-ghost flex-none whitespace-nowrap disabled:opacity-50"
            disabled={!sel.length || batchBusy !== null}
            onClick={() => onBatch("verify", sel)}
          >
            {batchBusy === "verify" ? "检查中…" : "批量检查登录状态"}
          </button>
          <button
            className="btn-ghost text-bad flex-none whitespace-nowrap disabled:opacity-50"
            disabled={!sel.length || batchBusy !== null}
            onClick={() => onBatch("delete", sel)}
          >
            {batchBusy === "delete" ? "移除中…" : "批量移除"}
          </button>
          <span className="text-[11px] text-ink3 flex-none">批量移除仅从换号器移出，不影响账号本身</span>
        </div>
      )}

      {accounts.length === 0 ? (
        <div className="flex flex-col items-center justify-center h-[380px] gap-4">
          <div className="w-16 h-16 rounded-[20px] bg-accent-soft text-accent flex items-center justify-center">
            <IconSearch className="w-7 h-7" />
          </div>
          <div className="text-[14px] font-semibold">账号池为空</div>
          <div className="text-[12px] text-ink3">添加首个账号，开始管理多个 Trae 账号</div>
          <div className="flex gap-2.5 mt-2">
            <button className="btn-primary" onClick={() => onAdd({ tab: "paste" })}>
              粘贴 Token 添加
            </button>
            <button className="btn-ghost" onClick={() => onAdd({ tab: "scan" })}>
              扫描本机已登录账号
            </button>
            <button className="btn-ghost" onClick={() => onAdd({ tab: "login" })}>
              登录一个新账号
            </button>
          </div>
        </div>
      ) : visible.length === 0 ? (
        <div className="flex flex-col items-center justify-center h-[280px] gap-2 text-ink3 text-[12.5px]">
          {pool.length === 0 ? `${isCn ? "Trae CN" : "Trae"} 尚无账号，可使用上方「添加账号」添加` : "无匹配的账号"}
          {pool.length > 0 && (
            <button className="btn-ghost mt-2" onClick={() => { setSearch(""); setStatusFilter("all"); setPlanFilter("all"); }}>
              清除筛选
            </button>
          )}
        </div>
      ) : (
        <div className="stagger-in grid grid-cols-3 gap-3.5 pb-4">
          {sorted.map((a) => (
            <AccountCard
              key={a.id}
              account={a}
              current={a.id === currentId}
              switching={a.id === switchingId}
              claiming={a.id === claimingId}
              batchMode={batchMode}
              selected={sel.includes(a.id)}
              onSelect={(id) => setSel((s) => (s.includes(id) ? s.filter((x) => x !== id) : [...s, id]))}
              onPin={onPin}
              onRelogin={onRelogin}
              onLaunchIsolated={onLaunchIsolated}
              onSwitch={onSwitch}
              onEdit={onEdit}
              onDelete={onDelete}
              onRefresh={onRefresh}
              onClaimCheckin={onClaimCheckin}
            />
          ))}
        </div>
      )}
    </div>
  );
}
