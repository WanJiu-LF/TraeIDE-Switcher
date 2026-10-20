import { useEffect, useRef, useState } from "react";
import type { Account } from "../types";

interface AccountCardProps {
  account: Account;
  current: boolean;
  switching: boolean;
  claiming?: boolean;
  /** P0-2 批量模式：显示勾选框 */
  batchMode?: boolean;
  selected?: boolean;
  onSelect?: (id: string) => void;
  /** P0-6 置顶 */
  onPin?: (id: string, pinned: boolean) => void;
  /** P0-4 重新登录采集 */
  onRelogin?: () => void;
  /** 多开：独立 user-data 实例启动该账号 */
  onLaunchIsolated?: (id: string) => void;
  /** 在该账号的独立实例内无感换号（需先用「多开（独立实例）」启动过） */
  onSwitchIsolated?: (id: string) => void;
  onSwitch: (id: string) => void;
  onEdit: (id: string) => void;
  onDelete: (id: string) => void;
  onRefresh: (id: string) => void;
  onClaimCheckin: (id: string) => void;
}

function QuotaBar({
  label,
  used,
  total,
  dollar,
}: {
  label: string;
  used: number;
  total: number;
  dollar?: boolean;
}) {
  const remain = Math.max(total - used, 0);
  const pct = total > 0 ? (remain / total) * 100 : 0;
  // 无额度（total<=0）时保持中性色，不判红——0/0 并非"已耗尽"
  const color =
    total <= 0
      ? "var(--color-line)"
      : pct <= 0
        ? "var(--color-bad)"
        : pct <= 10
          ? "var(--color-warn)"
          : "var(--color-ok)";
  const fmt = (v: number) => (dollar ? `$${v.toFixed(2)}` : v.toLocaleString("zh-CN"));
  return (
    <div className="mt-2.5">
      <div className="flex justify-between text-[11px] text-ink2 mb-[5px]">
        <span>{label}</span>
        <b className={`font-mono ${total > 0 && pct <= 0 ? "text-bad" : "text-ink"}`}>
          {fmt(remain)} / {fmt(total)}
        </b>
      </div>
      <div className="h-[6px] bg-[#eef3f6] rounded-full overflow-hidden">
        <div
          className="bar-fill h-full rounded-full"
          style={{ width: `${total <= 0 ? 0 : Math.max(pct, 2)}%`, background: color }}
        />
      </div>
    </div>
  );
}

const STATUS_BADGE: Record<Account["status"], { text: string; cls: string }> = {
  normal: { text: "正常", cls: "bg-[#e9f9ef] border-[#c3ecd2] text-[#1d9d54]" },
  exhausted: { text: "额度耗尽", cls: "bg-[#fdeeee] border-[#f6cfcf] text-[#d24545]" },
  error: { text: "登录异常", cls: "bg-[#fdeeee] border-[#f6cfcf] text-[#d24545]" },
};

/** 到期时间归一（epoch 秒/毫秒与 ISO 串都能显示成 YYYY/MM/DD） */
function expireText(expire: string): string {
  if (!expire) return "";
  const s = expire.trim();
  const d = /^\d+$/.test(s) ? new Date(Number(s) < 1e11 ? Number(s) * 1000 : Number(s)) : new Date(s);
  if (isNaN(d.getTime())) return "";
  const p = (n: number) => String(n).padStart(2, "0");
  return `${d.getFullYear()}/${p(d.getMonth() + 1)}/${p(d.getDate())} 到期`;
}

/** 国际版结算周期结束时刻（秒）→ "订阅重置 YYYY/MM/DD HH:mm" */
function resetText(ts?: number): string {
  if (!ts || ts <= 0) return "";
  const d = new Date(ts * 1000);
  const p = (n: number) => String(n).padStart(2, "0");
  return `订阅重置 ${d.getFullYear()}/${p(d.getMonth() + 1)}/${p(d.getDate())} ${p(d.getHours())}:${p(d.getMinutes())}`;
}

type CreditSource = NonNullable<Account["credits"]>["sources"][number];

export interface SourceGroup {
  desc: string;
  remaining: number;
  used: number;
  limit: number;
  unlimited: boolean;
  expire: string;
  count: number;
}

/** 合并同类积分来源（同名多笔 → 一行），按剩余倒序。增值页复用同一口径。 */
export function groupSources(sources: CreditSource[]): SourceGroup[] {
  const map = new Map<string, SourceGroup>();
  for (const s of sources) {
    const g = map.get(s.desc);
    if (g) {
      g.remaining += s.remaining;
      g.used += s.used;
      g.limit += s.limit;
      g.unlimited = g.unlimited || s.unlimited;
      g.count += 1;
      if (s.expire && (!g.expire || s.expire < g.expire)) g.expire = s.expire;
    } else {
      map.set(s.desc, {
        desc: s.desc,
        remaining: s.remaining,
        used: s.used,
        limit: s.limit,
        unlimited: s.unlimited,
        expire: s.expire,
        count: 1,
      });
    }
  }
  return [...map.values()]
    .filter((g) => !(g.limit === 0 && g.used === 0 && !g.unlimited))
    .sort((a, b) => b.remaining - a.remaining);
}

const MAX_SOURCE_ROWS = 6;

function CreditsBlock({ account, claiming, onClaimCheckin }: { account: Account; claiming?: boolean; onClaimCheckin: (id: string) => void }) {
  const [expanded, setExpanded] = useState(false);
  const credits = account.credits;
  if (!credits) return null;
  const hasUnlimited = credits.sources.some((s) => s.unlimited);
  const groups = groupSources(credits.sources);
  const shown = expanded ? groups : groups.slice(0, MAX_SOURCE_ROWS);
  return (
    <div className="mt-2.5">
      <div className="flex items-center justify-between">
        <span className="text-[11px] text-ink2">总可用积分</span>
        {account.checkin &&
          (account.checkin.checked ? (
            <span
              title="今日已签到"
              className="text-[10px] px-2 py-0.5 rounded-full border bg-[#e9f9ef] border-[#c3ecd2] text-[#1d9d54]"
            >
              今日已签到
            </span>
          ) : (
            <button
              title={`今日签到可得 ${account.checkin.reward} 积分`}
              disabled={claiming}
              onClick={() => onClaimCheckin(account.id)}
              className="text-[10px] px-2 py-0.5 rounded-full border bg-[#fff4e8] border-[#ffe0bd] text-[#c2731a] cursor-pointer hover:bg-[#ffe9d2] disabled:opacity-60"
            >
              {claiming ? "签到中…" : "一键签到"}
            </button>
          ))}
      </div>
      <div className="flex items-center gap-1.5 mt-[2px]">
        <b
          key={credits.total}
          className="num-pop text-[23px] leading-tight font-bold font-mono"
        >
          {credits.total.toLocaleString("zh-CN", { minimumFractionDigits: 2, maximumFractionDigits: 2 })}
        </b>
        {hasUnlimited && (
          <span className="text-[10px] px-1.5 py-0.5 rounded-full border bg-[#eef2ff] border-[#d4dcff] text-[#4f5fd7]">
            含无限额度
          </span>
        )}
      </div>
      {shown.map((g) => {
        const remainPct = g.limit > 0 ? (g.remaining / g.limit) * 100 : 0;
        const color = g.unlimited
          ? "var(--color-accent)"
          : remainPct <= 0
            ? "var(--color-bad)"
            : remainPct <= 10
              ? "var(--color-warn)"
              : "var(--color-ok)";
        return (
          <div key={g.desc} className="mt-2">
            <div className="flex items-center justify-between text-[11px] text-ink2 mb-[5px]">
              <span className="truncate">
                {g.desc}
                {g.count > 1 && <span className="text-[9.5px] text-ink3 ml-1">共 {g.count} 项</span>}
              </span>
              <span className="flex items-center gap-1.5 flex-none ml-2">
                <b className={`font-mono ${!g.unlimited && remainPct <= 0 ? "text-bad" : "text-ink"}`}>
                  {g.unlimited ? "无限" : `${g.remaining.toLocaleString("zh-CN")} / ${g.limit.toLocaleString("zh-CN")}`}
                </b>
                {g.expire && <span className="text-[9.5px] text-ink3">{expireText(g.expire)}</span>}
              </span>
            </div>
            <div className="h-[4px] bg-[#eef3f6] rounded-full overflow-hidden">
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
      {groups.length > MAX_SOURCE_ROWS && (
        <button
          className="w-full mt-2 text-[10.5px] text-ink3 hover:text-accent cursor-pointer"
          onClick={() => setExpanded((v) => !v)}
        >
          {expanded ? "收起" : `还有 ${groups.length - MAX_SOURCE_ROWS} 项`}
        </button>
      )}
    </div>
  );
}

export function AccountCard({
  account,
  current,
  switching,
  claiming,
  batchMode,
  selected,
  onSelect,
  onPin,
  onRelogin,
  onLaunchIsolated,
  onSwitchIsolated,
  onSwitch,
  onEdit,
  onDelete,
  onRefresh,
  onClaimCheckin,
}: AccountCardProps) {
  const [menuOpen, setMenuOpen] = useState(false);
  const [menuClosing, setMenuClosing] = useState(false);
  const menuTimer = useRef<number | null>(null);
  useEffect(
    () => () => {
      if (menuTimer.current !== null) window.clearTimeout(menuTimer.current);
    },
    [],
  );

  /** 收起时先播退场动画再卸载（延迟 140ms，与 .dropdown-out 时长一致） */
  const closeMenu = () => {
    if (menuTimer.current !== null) window.clearTimeout(menuTimer.current);
    setMenuClosing(true);
    menuTimer.current = window.setTimeout(() => {
      setMenuOpen(false);
      setMenuClosing(false);
      menuTimer.current = null;
    }, 140);
  };
  const toggleMenu = () => {
    if (menuOpen) {
      closeMenu();
      return;
    }
    if (menuTimer.current !== null) {
      window.clearTimeout(menuTimer.current);
      menuTimer.current = null;
    }
    setMenuClosing(false);
    setMenuOpen(true);
  };

  // 多开实例在线才允许"在独立实例中切换"（同 exe 名无法从进程区分实例，以通道心跳为准）
  const canSwitchIsolated = account.instanceAlive === true;
  const menuItems: {
    label: string;
    action: () => void;
    danger?: boolean;
    disabled?: boolean;
    title?: string;
  }[] = [
    { label: account.pinned ? "取消置顶" : "置顶显示", action: () => onPin?.(account.id, !account.pinned) },
    { label: "切换到此账号", action: () => onSwitch(account.id) },
    { label: "多开（独立实例）", action: () => onLaunchIsolated?.(account.id) },
    {
      label: "在独立实例中切换",
      action: () => onSwitchIsolated?.(account.id),
      disabled: !canSwitchIsolated,
      title: canSwitchIsolated
        ? "在该账号的独立实例内无感换号（不影响主实例）"
        : account.instanceExists
          ? "该账号的独立实例未在运行，先用「多开（独立实例）」启动"
          : "该账号尚未多开过，先用「多开（独立实例）」启动",
    },
    { label: "重新登录采集", action: () => onRelogin?.() },
    { label: "刷新额度", action: () => onRefresh(account.id) },
    { label: "编辑", action: () => onEdit(account.id) },
    { label: "删除", action: () => onDelete(account.id), danger: true },
  ];

  // 凭据状态（按 access token 到期时刻）：已过期 / 即将过期（保活窗口 30 分钟）
  const expLeftMs = account.expiresAt ? account.expiresAt - Date.now() : null;
  const expiringSoon =
    !account.credExpired && expLeftMs !== null && expLeftMs > 0 && expLeftMs <= 30 * 60 * 1000;
  const expLeftMin = expiringSoon ? Math.max(1, Math.round((expLeftMs as number) / 60000)) : 0;

  return (
    <div
      className={`lift group relative h-full flex flex-col bg-card border rounded-[12px] p-4 shadow-card ${
        current ? "border-accent shadow-[0_0_0_2px_var(--color-accent-soft),var(--shadow-card)]" : "border-line"
      }`}
    >
      {current && (
        <div className="absolute -top-[9px] right-3 bg-accent text-white text-[10px] px-2.5 py-0.5 rounded-full font-semibold">
          使用中
        </div>
      )}

      <div className="flex items-center gap-2.5 mb-3">
        {batchMode && (
          <button
            className={`w-[18px] h-[18px] flex-none rounded-[5px] border flex items-center justify-center text-[11px] cursor-pointer ${
              selected ? "bg-accent border-accent text-white" : "border-line bg-white text-transparent"
            }`}
            onClick={() => onSelect?.(account.id)}
            title={selected ? "取消选择" : "选择该账号"}
          >
            ✓
          </button>
        )}
        <div
          className="w-[38px] h-[38px] rounded-[11px] flex-none flex items-center justify-center text-[15px] font-bold text-white transition-transform duration-200 ease-out group-hover:scale-105"
          style={{ background: account.color }}
        >
          {account.initial}
        </div>
        <div className="min-w-0">
          <div className="text-[13.5px] font-semibold truncate">{account.name}</div>
          <div className="text-[11px] text-ink3 truncate">{account.email}</div>
        </div>
        <div className="relative ml-auto">
          <button
            className={`w-7 h-7 rounded-md text-[15px] leading-none cursor-pointer transition-all duration-150 active:scale-90 ${
              menuOpen ? "bg-accent-soft text-accent" : "text-ink3 hover:bg-[#f0f6f7]"
            }`}
            onClick={toggleMenu}
            title="更多操作"
          >
            ···
          </button>
          {menuOpen && (
            <>
              <div className="fixed inset-0 z-10" onClick={closeMenu} />
              <div
                className={`${menuClosing ? "dropdown-out" : "dropdown-in"} absolute right-0 top-8 z-20 w-[140px] bg-card border border-line rounded-[10px] shadow-soft py-1.5`}
              >
                {menuItems.map((m, i) => (
                  <button
                    key={m.label}
                    className={`menu-item-in w-full text-left px-3.5 py-[7px] text-[12px] transition-colors duration-150 ${
                      m.disabled
                        ? "text-ink3 cursor-not-allowed"
                        : `cursor-pointer hover:bg-[#f0f6f7] ${m.danger ? "text-bad" : "text-ink2"}`
                    }`}
                    title={m.title}
                    style={menuClosing ? undefined : { animationDelay: `${i * 18}ms` }}
                    onClick={() => {
                      closeMenu();
                      if (!m.disabled) m.action();
                    }}
                  >
                    {m.label}
                  </button>
                ))}
              </div>
            </>
          )}
        </div>
      </div>

      <div className="flex gap-1.5 flex-wrap mb-1">
        <span
          className={`text-[10.5px] px-2 py-0.5 rounded-full border ${
            account.plan === "pro" ? "bg-[#eef2ff] border-[#d4dcff] text-[#4f5fd7]" : "bg-[#fbfdfe] border-line text-ink2"
          }`}
        >
          {account.plan === "pro" ? "Pro" : "免费版"}
        </span>
        <span
          className={`text-[10.5px] px-2 py-0.5 rounded-full border ${
            account.ide === "trae-cn" ? "bg-[#fff4e8] border-[#ffe0bd] text-[#c2731a]" : "bg-[#fbfdfe] border-line text-ink2"
          }`}
        >
          {account.ide === "trae-cn" ? "Trae CN" : "Trae"}
        </span>
        <span
          className={`text-[10.5px] px-2 py-0.5 rounded-full border ${STATUS_BADGE[account.status].cls}`}
          title={account.errorText}
        >
          {STATUS_BADGE[account.status].text}
        </span>
        {account.threshold != null && (
          <span
            className="text-[10.5px] px-2 py-0.5 rounded-full border bg-[#fbfdfe] border-line text-ink2"
            title="该账号独立设置：剩余额度低于此百分比时自动换号"
          >
            低于 {account.threshold}% 自动换号
          </span>
        )}
        {account.instanceAlive && (
          <span
            className="text-[10.5px] px-2 py-0.5 rounded-full border border-accent-line text-accent bg-accent-soft"
            title="该账号的独立实例正在运行，可在实例内无感换号（不影响主实例）"
          >
            独立实例运行中
          </span>
        )}
      </div>

      {account.errorText && (
        <div
          className="mt-2 text-[10.5px] text-bad bg-[#fdeeee] border border-[#f6cfcf] rounded-[8px] px-2 py-1.5 leading-snug break-all"
          title={account.errorText}
        >
          额度查询失败，可稍后使用“刷新额度”重试：{account.errorText.length > 180 ? `${account.errorText.slice(0, 180)}…` : account.errorText}
        </div>
      )}

      {account.deviceMismatch ? (
        <div
          className="mt-2 text-[10.5px] text-[#b8860b] bg-[#fff8e6] border border-[#f0dca6] rounded-[8px] px-2 py-1.5 leading-snug break-all"
          title={account.refreshError ?? undefined}
        >
          续期失败：refreshToken 与本机设备不匹配（20403），需在本机重新登录采集该账号
        </div>
      ) : account.credExpired ? (
        <div className="mt-2 text-[10.5px] text-bad bg-[#fdeeee] border border-[#f6cfcf] rounded-[8px] px-2 py-1.5 leading-snug">
          凭据已过期，无法自动续期，需重新登录采集
        </div>
      ) : (
        expiringSoon && (
          <div className="mt-2 text-[10.5px] text-[#b8860b] bg-[#fff8e6] border border-[#f0dca6] rounded-[8px] px-2 py-1.5 leading-snug">
            凭据将于约 {expLeftMin} 分钟后到期，请留意自动续期结果
          </div>
        )
      )}

      {account.credits ? (
        <CreditsBlock account={account} claiming={claiming} onClaimCheckin={onClaimCheckin} />
      ) : (
        <>
          {/* 国际版口径：基础额度 basic / 赠送额度 bonus；无赠送额度时不展示该行 */}
          <QuotaBar label="基础额度" used={account.daily.used} total={account.daily.total} dollar={account.dollar} />
          {!account.noBonus && (
            <QuotaBar label="赠送额度" used={account.weekly.used} total={account.weekly.total} dollar={account.dollar} />
          )}
        </>
      )}

      <div className="mt-auto pt-2.5">
        <div className="text-[10.5px] text-ink3">
          {account.credits ? "按积分计算，不同模型消耗不同" : resetText(account.resetAt) || account.resetText}
        </div>

        {!current && (
          <button
            className="mt-3 w-full h-8 rounded-[9px] border border-line bg-white text-[11.5px] text-ink2 cursor-pointer transition-all duration-150 hover:border-accent hover:text-accent hover:shadow-card active:scale-[0.98] disabled:opacity-60 disabled:cursor-default disabled:hover:border-line disabled:hover:text-ink2 disabled:hover:shadow-none"
            disabled={switching}
            onClick={() => onSwitch(account.id)}
          >
            {switching ? (
              <span className="flex items-center justify-center gap-1.5">
                <span className="w-3 h-3 rounded-full border-2 border-accent border-t-transparent animate-spin" />
                切换中…
              </span>
            ) : (
              "切换到此账号"
            )}
          </button>
        )}
      </div>
    </div>
  );
}
