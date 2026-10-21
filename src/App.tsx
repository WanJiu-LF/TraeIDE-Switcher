import { useEffect, useRef, useState } from "react";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { getVersion } from "@tauri-apps/api/app";
import { isPermissionGranted, requestPermission, sendNotification } from "@tauri-apps/plugin-notification";
import type { Account, AccountStatus, Ide, Page, ToastItem } from "./types";
import {
  addAccount,
  appUpdate,
  bridgeStatus,
  claimCheckin,
  coldSwitchRestart,
  deleteAccount,
  drawCheckinLottery,
  DEFAULT_SETTINGS,
  errText,
  getLogs,
  getSettings,
  ideStatus as fetchIdeStatus,
  importLocalAccount,
  installPaths,
  isTauri,
  launchIsolatedInstance,
  listAccounts,
  queryAssets,
  queryCheckin,
  queryQuota,
  sessionUsage,
  setAccountPinned,
  shimStatus,
  switchAccount,
  switchIsolated,
  updateAccount,
  verifyAccount,
  type AccountSummary,
  type AppSettings,
  type AssetBundle,
  type IdeStatus,
  type PackQuota,
  type SessionUsage,
  type ShimStatus,
} from "./bridge";
import { TitleBar } from "./components/TitleBar";
import { Sidebar } from "./components/Sidebar";
import { StatusBar } from "./components/StatusBar";
import { Toasts } from "./components/Toasts";
import { ConfirmDialog } from "./components/ConfirmDialog";
import { AddAccountDialog, type AccountDraft } from "./components/AddAccountDialog";
import type { AddTabRequest } from "./pages/AccountsPage";
import { AccountsPage } from "./pages/AccountsPage";
import { AssetsPage } from "./pages/AssetsPage";
import { HealthPage } from "./pages/HealthPage";
import { LogsPage } from "./pages/LogsPage";
import { SettingsPage } from "./pages/SettingsPage";
import { AboutPage } from "./pages/AboutPage";

const COLLAPSED_KEY = "traeide-switcher.sidebar-collapsed";

const AVATAR_COLORS = [
  "linear-gradient(135deg,#0d9488,#43d3c2)",
  "linear-gradient(135deg,#4f8ef7,#7cb1ff)",
  "linear-gradient(135deg,#8b5cf6,#b79aff)",
  "linear-gradient(135deg,#f59e0b,#fbbf24)",
  "linear-gradient(135deg,#0ea5e9,#67d4ff)",
  "linear-gradient(135deg,#14b8a6,#5eead4)",
];

function idHash(id: string): number {
  let h = 0;
  for (let i = 0; i < id.length; i++) h = (h * 31 + id.charCodeAt(i)) | 0;
  return Math.abs(h);
}

const STATUS_MAP: Record<AccountSummary["status"], AccountStatus> = {
  Normal: "normal",
  Exhausted: "exhausted",
  Error: "error",
};

function toUiAccount(s: AccountSummary): Account {
  return {
    id: s.id,
    name: s.name,
    email: s.email,
    initial: s.name.slice(0, 1),
    color: AVATAR_COLORS[idHash(s.id) % AVATAR_COLORS.length],
    plan: "free",
    ide: s.ide,
    status: STATUS_MAP[s.status],
    // 额度未拉到前用 0/0 占位：避免国际版汇总被"默认 1/1"虚增（0/0 也不判耗尽）
    daily: { used: 0, total: 0 },
    weekly: { used: 0, total: 0 },
    resetText: "",
    note: s.note,
    pinned: s.pinned ?? false,
    threshold: s.threshold ?? null,
    expiresAt: s.expires_at ?? null,
    credExpired: s.cred_expired ?? false,
    refreshError: s.refresh_error ?? null,
    deviceMismatch: s.device_mismatch ?? false,
    instanceExists: s.instance_exists ?? false,
    instanceAlive: s.instance_alive ?? false,
  };
}

/** 用最新摘要覆盖卡片的"摘要字段"，保留已拉取的额度/签到/错误提示（整表重建会清空这些值导致轮询闪烁） */
function mergeSummary(a: Account, s: AccountSummary): Account {
  return {
    ...a,
    name: s.name,
    email: s.email,
    initial: s.name.slice(0, 1),
    note: s.note,
    status: STATUS_MAP[s.status],
    pinned: s.pinned ?? false,
    threshold: s.threshold ?? null,
    expiresAt: s.expires_at ?? null,
    credExpired: s.cred_expired ?? false,
    refreshError: s.refresh_error ?? null,
    deviceMismatch: s.device_mismatch ?? false,
    instanceExists: s.instance_exists ?? false,
    instanceAlive: s.instance_alive ?? false,
  };
}

export default function App() {
  const [page, setPage] = useState<Page>("accounts");
  const [collapsedPref, setCollapsedPref] = useState(() => localStorage.getItem(COLLAPSED_KEY) === "1");
  const [narrow, setNarrow] = useState(() => window.innerWidth < 900);
  const [targetIde, setTargetIde] = useState<Ide>("trae-cn");
  // 状态随结果带上归属 IDE：切目标时无需清空，也不会串显另一版本的数据
  const [status, setStatus] = useState<{ ide: Ide; s: IdeStatus } | null>(null);
  const [accounts, setAccounts] = useState<Account[]>([]);
  // 资产缓存（按账号 id）：后台预取填充，增值页只读它，进入页面不再发请求
  const [assetCache, setAssetCache] = useState<Record<string, AssetBundle>>({});
  // 会话用量缓存（按 IDE）：会话归属"当前登录账号"，故与账号库无关，只在开机/切版本/换号后自动拉取
  const [usageCache, setUsageCache] = useState<Record<string, SessionUsage | null>>({});
  const [usageBusy, setUsageBusy] = useState(false);
  const [usageErr, setUsageErr] = useState<string | null>(null);
  const [usageAt, setUsageAt] = useState(0);
  const [switchingId, setSwitchingId] = useState<string | null>(null);
  const [claimingId, setClaimingId] = useState<string | null>(null);
  /** P0-2 批量操作进行中的动作名（null = 空闲） */
  const [batchBusy, setBatchBusy] = useState<string | null>(null);
  const [unread, setUnread] = useState(0);
  const [settings, setSettings] = useState<AppSettings>(DEFAULT_SETTINGS);
  // 运行检查数据：全局后台轮询，页面只读（不再进入页面才查）
  const [shim, setShim] = useState<ShimStatus | null>(null);
  const [injected, setInjected] = useState<boolean | null>(null);
  const [paths, setPaths] = useState<Record<Ide, string> | null>(null);
  const [healthAt, setHealthAt] = useState(0);
  const [toasts, setToasts] = useState<ToastItem[]>([]);
  const [confirmId, setConfirmId] = useState<string | null>(null);
  const [pendingColdId, setPendingColdId] = useState<string | null>(null);
  const [dialogOpen, setDialogOpen] = useState(false);
  const [dialogTab, setDialogTab] = useState<AddTabRequest["tab"]>("paste");
  const [editing, setEditing] = useState<Account | null>(null);
  const toastSeq = useRef(0);
  const settingsRef = useRef(settings);
  // 账号池镜像：供定时轮询读取最新列表（不把 accounts 放进定时器依赖，避免每次变动重建定时器）
  const accountsRef = useRef<Account[]>([]);

  const collapsed = narrow || collapsedPref;
  const ideStatus = status?.ide === targetIde ? status.s : null;
  // 当前生效账号按目标版本取自该 IDE 自身登录态（国际版/国内版各一套，互不串号）
  const currentId = ideStatus?.currentId ?? "";

  const pushToast = (kind: ToastItem["kind"], title: string, desc?: string) => {
    const id = `t${++toastSeq.current}`;
    setToasts((list) => [...list, { id, kind, title, desc }]);
  };

  const dismissToast = (id: string) => setToasts((list) => list.filter((t) => t.id !== id));

  const notify = (body: string) => {
    if (!settingsRef.current.notifications) return;
    sendNotification({ title: "TraeIDE-Switcher", body });
  };

  const applyQuota = (id: string, packs: PackQuota[]) => {
    const p = packs[0];
    if (!p) return;
    setAccounts((list) =>
      list.map((a) => {
        if (a.id !== id) return a;
        const next: Account = {
          ...a,
          plan: p.product_type === 1 ? "pro" : "free",
        };
        if (a.ide === "trae-cn") {
          const sources = packs.map((q) => ({
            desc: q.desc || "积分",
            remaining: Math.max(q.credits_limit - q.credits_used, 0),
            used: q.credits_used,
            limit: q.credits_limit,
            unlimited: q.credits_unlimited,
            expire: q.expire_time,
          }));
          next.daily = { used: 0, total: 0 };
          next.weekly = { used: 0, total: 0 };
          next.resetText = "";
          next.credits = {
            total: sources.reduce((sum, s) => sum + (s.unlimited ? 0 : s.remaining), 0),
            sources,
          };
        } else {
          // 国际版取 product_type=Free(0) 的权益包（与 IDE 侧选择口径一致）：
          // basic=基础额度、bonus=赠送额度，no_bonus_quota 时无赠送额度
          const pack = packs.find((q) => q.product_type === 0) ?? p;
          next.daily = { used: pack.basic_used, total: pack.basic_limit };
          next.weekly = { used: pack.bonus_used, total: pack.bonus_limit };
          next.noBonus = pack.no_bonus_quota;
          // 逆向核对（intl 3.5.91）：用量面板对基础/赠送额度硬编码 `$` 格式化
          // （O9e: Number.isInteger(i)?`$${i}`:`$${i.toFixed(2)}`，调用点无 is_dollar_usage_billing 分支），
          // 故国际版一律按美元渲染，不设"次数"口径
          next.dollar = true;
          next.resetAt = pack.end_time > 0 ? pack.end_time : undefined;
          next.resetText = "";
        }
        return next;
      }),
    );
  };

  // 额度查询结果落到卡片状态：失败必须显式标红并给出原因。
  // 原先失败被 .catch 静默吞掉，卡片回落成"每日额度 1/1 · 正常"，用户根本看不出是查询失败。
  const markQuota = (id: string, errorText: string | null) => {
    setAccounts((list) =>
      list.map((a) =>
        a.id === id
          ? errorText === null
            ? { ...a, status: a.status === "exhausted" ? "exhausted" : "normal", errorText: undefined }
            : { ...a, status: "error", errorText }
          : a,
      ),
    );
  };

  const loadQuota = async (id: string, ide: Ide) => {
    try {
      // 额度与签到是两个独立请求，CN 账号并发拉取（原先串行，额度可见时间翻倍）
      if (ide !== "trae-cn") {
        applyQuota(id, await queryQuota(id));
      } else {
        const [packs, c] = await Promise.all([queryQuota(id), queryCheckin(id).catch(() => null)]);
        applyQuota(id, packs);
        if (c) {
          setAccounts((list) =>
            list.map((a) => (a.id === id ? { ...a, checkin: { checked: c.checked_in, reward: c.credits } } : a)),
          );
        }
      }
      markQuota(id, null);
    } catch (e) {
      markQuota(id, errText(e));
      throw e;
    }
  };

  // 入库结果入池：同 userId 重复入库（P0-5 去重）时原地更新，不再堆出重复卡片
  const upsertSummary = (s: AccountSummary) => {
    setAccounts((list) => {
      const ui = toUiAccount(s);
      return list.some((a) => a.id === s.id)
        ? list.map((a) =>
            a.id === s.id
              ? { ...a, status: ui.status, pinned: ui.pinned ?? a.pinned, threshold: ui.threshold ?? a.threshold }
              : a,
          )
        : [...list, ui];
    });
    loadQuota(s.id, s.ide).catch(() => undefined);
  };

  useEffect(() => {
    const onResize = () => setNarrow(window.innerWidth < 900);
    window.addEventListener("resize", onResize);
    return () => window.removeEventListener("resize", onResize);
  }, []);

  useEffect(() => {
    settingsRef.current = settings;
  }, [settings]);

  useEffect(() => {
    accountsRef.current = accounts;
  }, [accounts]);

  useEffect(() => {
    getSettings()
      .then(setSettings)
      .catch((e) => pushToast("error", "设置读取失败，请重新打开本软件", errText(e)));
  }, []);

  useEffect(() => {
    if (!isTauri()) return;
    void (async () => {
      if (!(await isPermissionGranted())) await requestPermission();
    })();
    const unlisteners: UnlistenFn[] = [];
    let disposed = false;
    const register = (p: Promise<UnlistenFn>) =>
      p.then((fn) => {
        if (disposed) fn();
        else unlisteners.push(fn);
      });
    register(listen<{ to: string }>("account-switched", (e) => notify(`已自动切换到 ${e.payload.to}`)));
    register(
      listen("log-changed", async () => {
        try {
          const latest = (await getLogs())[0];
          if (!latest) return;
          if (latest.level !== "Info") setUnread((n) => n + 1);
          if (latest.level !== "Info" && (latest.source === "Switch" || latest.source === "Quota")) {
            notify(latest.message);
          }
        } catch {
          // 日志拉取失败静默
        }
      }),
    );
    return () => {
      disposed = true;
      unlisteners.forEach((fn) => fn());
    };
  }, []);

  useEffect(() => {
    if (page === "logs") setUnread(0);
  }, [page]);

  // 目标 IDE 实时状态轮询（运行/CDP/TSW 补丁/shim 心跳/登录态/当前生效账号）
  // 依赖只挂 targetIde：切页面不应重置轮询，否则每次导航都空白一拍
  useEffect(() => {
    if (!isTauri()) return;
    let disposed = false;
    const poll = () =>
      fetchIdeStatus(targetIde)
        .then((s) => {
          if (!disposed) setStatus({ ide: targetIde, s });
        })
        .catch(() => undefined);
    poll();
    const timer = setInterval(poll, 5000);
    return () => {
      disposed = true;
      clearInterval(timer);
    };
  }, [targetIde]);

  // 账号池重载：首次加载与外部触发（如导入账号包、批量校验后状态已写回后端）共用。
  // 与既有卡片做字段级合并而非整表重建：避免把已拉到的额度/签到/错误提示清空造成闪烁
  const reloadAccounts = async () => {
    try {
      const list = await listAccounts();
      setAccounts((prev) =>
        list.map((s) => {
          const old = prev.find((a) => a.id === s.id);
          return old ? mergeSummary(old, s) : toUiAccount(s);
        }),
      );
      // 并发拉取：原先 for-await 串行，账号数 × 每号 1~2 个请求全部排队叠加
      void Promise.all(list.map((s) => loadQuota(s.id, s.ide).catch(() => undefined)));
      prefetchAssets(list);
    } catch (e) {
      pushToast("error", "账号列表读取失败，请稍后重试", errText(e));
    }
  };

  // 账号摘要轮询：只更新名称/备注/置顶/阈值/凭据到期与设备不匹配标记，不触碰额度数值
  const syncSummaries = async () => {
    try {
      const list = await listAccounts();
      setAccounts((prev) =>
        prev.map((a) => {
          const s = list.find((x) => x.id === a.id);
          return s ? mergeSummary(a, s) : a;
        }),
      );
    } catch {
      // 轮询失败静默：不打断界面，下一轮再试
    }
  };

  // 资产后台预取：不阻塞 UI、不进页面才拉；失败静默（页面用缓存渲染，可手动刷新）
  const prefetchAssets = (list: { id: string }[]) => {
    void Promise.all(
      list.map((s) =>
        queryAssets(s.id)
          .then((b) => setAssetCache((m) => ({ ...m, [s.id]: b })))
          .catch(() => undefined),
      ),
    );
  };

  // 手动刷新单账号资产：把结果写入缓存并返回（调用方负责提示）
  const refreshAssets = async (id: string): Promise<AssetBundle> => {
    const b = await queryAssets(id);
    setAssetCache((m) => ({ ...m, [id]: b }));
    return b;
  };

  // 会话用量自动拉取（本机 agent 本地 RPC）。自动路径先过门控：目标 IDE 未运行或
  // 会话补丁未加载时不发请求，避免未就绪时反复失败刷日志；手动路径（notify）无条件执行并弹错误。
  const ideReadyRef = useRef(false);
  useEffect(() => {
    ideReadyRef.current = !!(ideStatus?.running && ideStatus?.chatPinned);
  }, [ideStatus]);

  // 版本号只从打包时注入的 tauri 版本读取，避免多处硬编码导致升级后显示不一致
  const [appVersion, setAppVersion] = useState("");
  useEffect(() => {
    getVersion()
      .then(setAppVersion)
      .catch(() => setAppVersion(""));
  }, []);

  const refreshSessionUsage = async (ide: Ide = targetIde, notify = false) => {
    if (!notify && !ideReadyRef.current) return;
    setUsageBusy(true);
    try {
      const u = await sessionUsage(ide);
      setUsageCache((m) => ({ ...m, [ide]: u }));
      setUsageErr(null);
    } catch (e) {
      setUsageErr(errText(e));
      if (notify) pushToast("error", "会话用量读取失败，请确认 Trae 已启动后重试", errText(e));
    } finally {
      setUsageBusy(false);
      setUsageAt(Date.now());
    }
  };

  // 自动获取触发点：目标版本切换后首拉 + 每 30s 刷新（轮询内被门控，未就绪即空转）
  useEffect(() => {
    if (!isTauri()) return;
    const first = setTimeout(() => void refreshSessionUsage(targetIde), 1500);
    const timer = setInterval(() => void refreshSessionUsage(targetIde), 30000);
    return () => {
      clearTimeout(first);
      clearInterval(timer);
    };
    // refreshSessionUsage 每轮重建，语义上只依赖 targetIde
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [targetIde]);

  // 运行检查后台轮询（15s）：shim 心跳/版本、补丁加载态、安装路径 —— 页面只读这里的结果，
  // 不再"进入页面才查"，切页面也不会中断采集
  const refreshHealth = async () => {
    const [s, i, st] = await Promise.allSettled([
      shimStatus(targetIde),
      bridgeStatus(targetIde),
      fetchIdeStatus(targetIde),
    ]);
    setShim(s.status === "fulfilled" ? s.value : null);
    setInjected(i.status === "fulfilled" ? i.value : null);
    if (st.status === "fulfilled") setStatus({ ide: targetIde, s: st.value });
    setHealthAt(Date.now());
  };

  useEffect(() => {
    if (!isTauri()) return;
    let disposed = false;
    const run = () => {
      if (!disposed) void refreshHealth();
    };
    run();
    installPaths()
      .then((p) => {
        if (!disposed) setPaths(p);
      })
      .catch(() => {
        if (!disposed) setPaths(null);
      });
    const timer = setInterval(run, 15000);
    return () => {
      disposed = true;
      clearInterval(timer);
    };
    // refreshHealth 每轮重建，但语义上只依赖 targetIde
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [targetIde]);

  // 定时轮询（间隔 = 设置里的"额度刷新间隔"）：账号摘要 → 额度 → 资产。
  // 顺序固定：摘要先落盘，额度查询失败的标红状态才不会被摘要里的 vault 状态覆盖掉。
  // 摘要（凭据到期/设备不匹配）、额度（卡片数字/趋势）、资产（增值页）此前都只在首次进入时拉一次
  useEffect(() => {
    if (!isTauri()) return;
    const ms = Math.max(1, settings.refresh_interval) * 60_000;
    const timer = setInterval(() => {
      void (async () => {
        await syncSummaries();
        const list = accountsRef.current;
        await Promise.all(list.map((a) => loadQuota(a.id, a.ide).catch(() => undefined)));
        prefetchAssets(list);
      })();
    }, ms);
    return () => clearInterval(timer);
    // syncSummaries/loadQuota/prefetchAssets 每轮重建，但只依赖 setState 与 ref，语义上只依赖间隔
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [settings.refresh_interval]);

  useEffect(() => {
    void reloadAccounts();
  }, []);

  const handleSwitch = async (id: string) => {
    if (id === currentId || switchingId) return;
    const target = accounts.find((a) => a.id === id);
    if (!target) return;

    setSwitchingId(id);
    try {
      const msg = await switchAccount(id, true);
      pushToast("success", `已切换到 ${target.name}`, msg);
      fetchIdeStatus(target.ide).then((s) => setStatus({ ide: target.ide, s })).catch(() => undefined);
      // 换号后"当前登录账号"已变 → 稍后重取会话用量（agent 接管新账号需要一点时间）
      setTimeout(() => void refreshSessionUsage(target.ide), 3000);
    } catch (e) {
      // 热换号失败（常见：Trae 非换号器启动、无 CDP 通道）→ 提示冷切换重启兜底
      setPendingColdId(id);
    } finally {
      setSwitchingId(null);
    }
  };

  const handleColdSwitchRestart = async () => {
    const id = pendingColdId;
    setPendingColdId(null);
    if (!id) return;
    const target = accounts.find((a) => a.id === id);
    setSwitchingId(id);
    try {
      const msg = await coldSwitchRestart(id);
      pushToast("success", `已切换到 ${target?.name ?? ""}`, msg);
      const ide = target?.ide ?? targetIde;
      fetchIdeStatus(ide)
        .then((s) => setStatus({ ide, s }))
        .catch(() => undefined);
      // 冷切换 = IDE 重启，等其起来后再重取会话用量（此时补丁与新账号才生效）
      for (const delay of [20000, 45000]) {
        setTimeout(() => void refreshSessionUsage(ide), delay);
      }
    } catch (e) {
      pushToast("error", `切换到 ${target?.name ?? ""} 失败，请先关闭 Trae 再重试`, errText(e));
    } finally {
      setSwitchingId(null);
    }
  };

  const handleClaimCheckin = async (id: string) => {
    setClaimingId(id);
    try {
      await claimCheckin(id);
      pushToast("success", "签到成功");
      await handleRefresh(id);
    } catch (e) {
      pushToast("error", "签到失败，请稍后重试", errText(e));
    } finally {
      setClaimingId(null);
    }
  };

  const handleRefresh = async (id: string) => {
    const acc = accounts.find((a) => a.id === id);
    if (!acc) return;
    try {
      await loadQuota(id, acc.ide);
      pushToast("success", "额度已刷新", acc.name);
    } catch (e) {
      pushToast("error", "额度查询失败，请确认账号登录有效后重试", errText(e));
    }
  };

  // P0-6 置顶/取消置顶
  const handlePin = async (id: string, pinned: boolean) => {
    try {
      await setAccountPinned(id, pinned);
      setAccounts((l) => l.map((a) => (a.id === id ? { ...a, pinned } : a)));
    } catch (e) {
      pushToast("error", "置顶设置失败，请重试", errText(e));
    }
  };

  // 多开：为该账号准备独立 user-data 目录并启动第二个 Trae 实例（不影响当前登录态）
  const handleLaunchIsolated = async (id: string) => {
    const acc = accounts.find((a) => a.id === id);
    try {
      const r = await launchIsolatedInstance(id);
      pushToast("success", `${r.name} 已启动独立实例`, `数据目录：${r.dir}`);
      // 实例启动到切换助手写出首个心跳需数秒：分两次刷新摘要，让"独立实例运行中"标记出现
      for (const delay of [8000, 20000]) {
        setTimeout(() => void syncSummaries(), delay);
      }
    } catch (e) {
      pushToast("error", `多开 ${acc?.name ?? ""} 失败，请先关闭 Trae 再重试`, errText(e));
    }
  };

  // 在独立实例内无感换号（走该实例自己的切换助手通道；需实例已启动）
  const handleSwitchIsolated = async (id: string) => {
    const acc = accounts.find((a) => a.id === id);
    try {
      const msg = await switchIsolated(id);
      pushToast("success", `已在独立实例中切换到 ${acc?.name ?? ""}`, msg);
    } catch (e) {
      pushToast("error", "独立实例切换失败，请确认该实例已启动后重试", errText(e));
    }
  };

  // 1024 签到抽奖（CN 专有）：默认直呼活动接口（账号 token + 设备槽），免切换、不依赖 Trae 运行；
  // 中奖以 couponFen>0 判定（抵扣券面额），未中奖时服务端返回安慰文案
  const handleDrawLottery = async (id: string) => {
    const acc = accounts.find((a) => a.id === id);
    try {
      const r = await drawCheckinLottery(id);
      if (!r.ok) {
        pushToast("error", `${acc?.name ?? ""} 抽奖未通过`, `业务码 ${r.code ?? "-"}`);
      } else if ((r.couponFen ?? 0) > 0) {
        pushToast(
          "success",
          `${acc?.name ?? ""} 中奖：${r.prizeText ?? ""}`,
          `${((r.couponFen ?? 0) / 100).toFixed(0)} 元抵扣券已发放至该账号`,
        );
      } else {
        pushToast("info", `${acc?.name ?? ""} 已参与 1024 抽奖`, r.prizeText ?? "未中奖，明日再来");
      }
    } catch (e) {
      pushToast("error", "抽奖失败，请稍后重试", errText(e));
    }
  };

  // P0-2 批量操作：选中的账号并发执行（各命令本身按账号幂等，并发即最短耗时）
  const handleBatch = async (action: "refresh" | "checkin" | "verify" | "delete" | "lottery", ids: string[]) => {
    if (ids.length === 0) return;
    setBatchBusy(action);
    try {
      if (action === "delete") {
        await Promise.all(ids.map((i) => deleteAccount(i).catch(() => undefined)));
        setAccounts((l) => l.filter((a) => !ids.includes(a.id)));
        setAssetCache((m) => {
          const next = { ...m };
          ids.forEach((i) => delete next[i]);
          return next;
        });
        pushToast("success", `已从换号器移除 ${ids.length} 个账号`);
      } else if (action === "refresh") {
        const res = await Promise.all(
          ids.map((i) => {
            const acc = accounts.find((a) => a.id === i);
            return acc ? loadQuota(i, acc.ide).then(() => true).catch(() => false) : Promise.resolve(false);
          }),
        );
        pushToast("info", `额度刷新 ${res.filter(Boolean).length}/${ids.length} 成功`);
      } else if (action === "checkin") {
        const res = await Promise.all(ids.map((i) => claimCheckin(i).then(() => true).catch(() => false)));
        pushToast("info", `签到 ${res.filter(Boolean).length}/${ids.length} 成功`);
        void Promise.all(
          ids.map((i) => {
            const acc = accounts.find((a) => a.id === i);
            return acc ? loadQuota(i, acc.ide).catch(() => undefined) : Promise.resolve();
          }),
        );
      } else if (action === "lottery") {
        // 直呼活动接口（各账号自身 token + 设备槽）：免切换、并发执行
        const res = await Promise.all(
          ids.map((i) => {
            const acc = accounts.find((a) => a.id === i);
            if (!acc || acc.ide !== "trae-cn") return Promise.resolve(null);
            return drawCheckinLottery(i)
              .then((r) => ({ name: acc.name, r }))
              .catch(() => null);
          }),
        );
        const valid = res.filter(
          (x): x is { name: string; r: import("./bridge").LotteryResult } => !!x && x.r.ok,
        );
        const wins = valid.filter((x) => (x.r.couponFen ?? 0) > 0);
        pushToast(
          "info",
          `1024 抽奖完成：${valid.length}/${ids.length} 已参与，中奖 ${wins.length}`,
          wins.length
            ? wins.map((w) => `${w.name}：${w.r.prizeText ?? ""}（${((w.r.couponFen ?? 0) / 100).toFixed(0)} 元）`).join("；")
            : undefined,
        );
      } else {
        const res = await Promise.all(ids.map((i) => verifyAccount(i).then((v) => v.valid === true).catch(() => false)));
        pushToast("info", `登录状态检查 ${res.filter(Boolean).length}/${ids.length} 通过`);
        void reloadAccounts();
      }
    } finally {
      setBatchBusy(null);
    }
  };

  const handleDelete = async () => {
    const id = confirmId;
    setConfirmId(null);
    if (!id) return;
    const acc = accounts.find((a) => a.id === id);
    try {
      await deleteAccount(id);
      const next = accounts.filter((a) => a.id !== id);
      setAccounts(next);
      setAssetCache((m) => {
        const { [id]: _drop, ...rest } = m;
        return rest;
      });
      if (id === currentId) {
        fetchIdeStatus(targetIde).then((s) => setStatus({ ide: targetIde, s })).catch(() => undefined);
      }
      pushToast("success", "账号已删除", acc?.name);
    } catch (e) {
      pushToast("error", "删除失败，请稍后重试", errText(e));
    }
  };

  const openAddDialog = (req: AddTabRequest) => {
    setEditing(null);
    setDialogTab(req.tab);
    setDialogOpen(true);
  };

  const openEditDialog = (id: string) => {
    const acc = accounts.find((a) => a.id === id) ?? null;
    setEditing(acc);
    setDialogTab("paste");
    setDialogOpen(true);
  };

  const handleSaveDraft = async (draft: AccountDraft) => {
    const editingId = editing?.id;
    setDialogOpen(false);
    setEditing(null);

    if (draft.kind === "edit") {
      if (!editingId) return;
      try {
        await updateAccount(editingId, draft.name, draft.note, draft.threshold);
        setAccounts((list) =>
          list.map((a) =>
            a.id === editingId
              ? {
                  ...a,
                  name: draft.name,
                  initial: draft.name.slice(0, 1),
                  note: draft.note,
                  threshold: draft.threshold,
                }
              : a,
          ),
        );
        pushToast("success", "已保存修改", draft.name);
      } catch (e) {
        pushToast("error", "保存失败，请检查填写内容后重试", errText(e));
      }
      return;
    }

    if (draft.kind === "batch") {
      let added = 0;
      let replaced = 0;
      let failed = 0;
      const errs: string[] = [];
      for (const blob of draft.blobs) {
        try {
          const s = await addAccount(draft.ide, "", draft.note, blob);
          upsertSummary(s);
          if (s.replaced) replaced++;
          else added++;
        } catch (e) {
          failed++;
          if (errs.length < 3) errs.push(errText(e));
        }
      }
      const detail = failed ? errs.join("；") : undefined;
      const title = failed
        ? `批量导入完成：成功 ${added + replaced} 个，失败 ${failed} 个`
        : `批量导入完成：新增 ${added} 个${replaced ? `，更新 ${replaced} 个` : ""}`;
      pushToast(failed && !(added + replaced) ? "error" : failed ? "info" : "success", title, detail);
      return;
    }

    if (draft.kind === "paste" || draft.kind === "login") {
      try {
        const s = await addAccount(draft.ide, draft.name, draft.note, draft.authBlob);
        upsertSummary(s);
        pushToast("success", s.replaced ? "已更新该账号的凭据" : "账号已添加", s.name);
      } catch (e) {
        pushToast("error", "添加账号失败，请检查 Token 后重试", errText(e));
      }
      return;
    }

    if (draft.kind === "scan") {
      for (const item of draft.items) {
        try {
          const s = await importLocalAccount(item.ide, item.name, "");
          upsertSummary(s);
          pushToast("success", s.replaced ? "已更新该账号" : "账号已添加", s.name);
        } catch (e) {
          pushToast("error", `导入 ${item.name} 失败，请稍后重试`, errText(e));
        }
      }
    }
  };

  return (
    <div className="h-full flex flex-col bg-win">
      <TitleBar />
      <div className="flex-1 flex min-h-0">
        <Sidebar
          page={page}
          onPage={setPage}
          collapsed={collapsed}
          onToggle={() => setCollapsedPref((v) => !v)}
          targetIde={targetIde}
          onTargetIde={setTargetIde}
          logUnread={unread}
          installPath={ideStatus?.install}
        />
        {page === "accounts" && (
          <AccountsPage
            accounts={accounts}
            currentId={currentId}
            switchingId={switchingId}
            targetIde={targetIde}
            ideStatus={ideStatus}
            onAdd={openAddDialog}
            onSwitch={handleSwitch}
            onEdit={openEditDialog}
            onDelete={setConfirmId}
            onRefresh={handleRefresh}
            claimingId={claimingId}
            onClaimCheckin={handleClaimCheckin}
            onPin={handlePin}
            onRelogin={() => openAddDialog({ tab: "login" })}
            onLaunchIsolated={handleLaunchIsolated}
            onSwitchIsolated={handleSwitchIsolated}
            onDrawLottery={handleDrawLottery}
            onBatch={handleBatch}
            batchBusy={batchBusy}
          />
        )}
        {page === "assets" && (
          // key=targetIde：切换目标版本时重新挂载，彻底清空上一版本的全部页面状态
          <AssetsPage
            key={targetIde}
            accounts={accounts}
            targetIde={targetIde}
            assets={assetCache}
            usage={usageCache[targetIde] ?? null}
            usageBusy={usageBusy}
            usageErr={usageErr}
            usageAt={usageAt}
            onRefreshUsage={(notify) => void refreshSessionUsage(targetIde, notify)}
            onRefresh={refreshAssets}
            onToast={pushToast}
            onReload={reloadAccounts}
            refreshMs={Math.max(1, settings.refresh_interval) * 60_000}
          />
        )}
        {page === "health" && (
          <HealthPage
            targetIde={targetIde}
            status={ideStatus}
            shim={shim}
            injected={injected}
            installPath={paths?.[targetIde] ?? ""}
            at={healthAt}
            onRefresh={refreshHealth}
            onToast={pushToast}
          />
        )}
        {page === "logs" && <LogsPage onToast={pushToast} />}
        {page === "settings" && (
          <SettingsPage
            targetIde={targetIde}
            accounts={accounts}
            onToast={pushToast}
            onSettings={setSettings}
            onReload={() => void reloadAccounts()}
          />
        )}
        {page === "about" && (
          <AboutPage
            appVersion={appVersion}
            onCheckUpdate={async () => {
              try {
                // 后端命令自带代理与自定义更新源；显式限时避免链路半开时"点击后无任何反馈"
                const info = await Promise.race([
                  appUpdate(false),
                  new Promise<never>((_, reject) => setTimeout(() => reject(new Error("timeout")), 20000)),
                ]);
                if (!info.available) {
                  pushToast("info", "已是最新版本", appVersion ? `当前 v${appVersion}` : undefined);
                  return;
                }
                pushToast("info", "发现新版本", `正在下载 v${info.version ?? ""}…`);
                await appUpdate(true);
                pushToast("success", "更新完成", "即将重启应用");
                const { relaunch } = await import("@tauri-apps/plugin-process");
                await relaunch();
              } catch (e) {
                const timedOut = errText(e) === "timeout";
                pushToast(
                  "error",
                  timedOut ? "检查更新超时，请检查网络或代理后重试" : "检查更新失败，请检查网络或代理后重试",
                  timedOut ? undefined : errText(e),
                );
              }
            }}
          />
        )}
      </div>
      <StatusBar
        targetIde={targetIde}
        autoSwitch={settings.auto_switch}
        ideStatus={ideStatus}
        appVersion={appVersion}
      />

      <Toasts items={toasts} onDismiss={dismissToast} />

      <ConfirmDialog
        open={confirmId !== null}
        title="删除账号"
        description="将该账号从换号器移出（仅影响换号器，账号本身不受影响）。该账号已保存的凭据与设备身份将一并清除。"
        onConfirm={handleDelete}
        onCancel={() => setConfirmId(null)}
      />

      <ConfirmDialog
        open={pendingColdId !== null}
        title="关闭 Trae 并重启切换？"
        description="当前 Trae 并非由本换号器启动，无法无感换号。继续将先关闭 Trae（未保存内容可能丢失），再写入目标账号的凭据并重启。"
        confirmText="关闭并切换"
        onConfirm={handleColdSwitchRestart}
        onCancel={() => setPendingColdId(null)}
      />

      <AddAccountDialog
        open={dialogOpen}
        editing={editing}
        initialTab={dialogTab}
        targetIde={targetIde}
        existingIds={accounts.map((a) => a.id)}
        onClose={() => {
          setDialogOpen(false);
          setEditing(null);
        }}
        onSave={handleSaveDraft}
        onToast={pushToast}
      />
    </div>
  );
}
