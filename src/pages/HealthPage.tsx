// P1-3 运行检查：把散落的链路状态集中一页，并给出对应的一键修复动作。
// 数据由 App 层全局后台轮询（15s）提供，本页只读 —— 不进页面才开始查，切页面也不中断采集。
import { useState, type ReactNode } from "react";
import type { Ide } from "../types";
import {
  errText,
  exportLogs,
  injectBridge,
  launchIde,
  removeBridge,
  shimInstall,
  type IdeStatus,
  type ShimStatus,
} from "../bridge";
import { IconRefresh } from "../icons";

interface HealthPageProps {
  targetIde: Ide;
  /** 来自 App 的全局轮询结果 */
  status?: IdeStatus | null;
  shim?: ShimStatus | null;
  injected?: boolean | null;
  installPath: string;
  /** 上次采集完成时间（ms） */
  at: number;
  onRefresh: () => Promise<void>;
  onToast: (kind: "success" | "error" | "info", title: string, desc?: string) => void;
}

interface RowProps {
  title: string;
  desc: string;
  state: "ok" | "warn" | "bad" | "unknown";
  text: string;
  action?: ReactNode;
}

const STATE_CLS: Record<RowProps["state"], string> = {
  ok: "bg-[#e9f9ef] border-[#c3ecd2] text-[#1d9d54]",
  warn: "bg-[#fff4e5] border-[#ffe0bd] text-[#c2731a]",
  bad: "bg-[#fdeeee] border-[#f6cfcf] text-[#d24545]",
  unknown: "bg-[#fbfdfe] border-line text-ink3",
};

function Row({ title, desc, state, text, action }: RowProps) {
  return (
    <div className="flex items-center px-[22px] py-3.5 border-b border-line-soft last:border-b-0">
      <div className="min-w-0">
        <div className="text-[13px] font-semibold">{title}</div>
        <div className="text-[11.5px] text-ink3 mt-0.5 truncate" title={desc}>
          {desc}
        </div>
      </div>
      <div className="ml-auto flex items-center gap-2.5 flex-none">
        <span className={`text-[10.5px] px-2 py-0.5 rounded-full border ${STATE_CLS[state]}`}>{text}</span>
        {action}
      </div>
    </div>
  );
}

export function HealthPage({
  targetIde,
  status,
  shim,
  injected,
  installPath,
  at,
  onRefresh,
  onToast,
}: HealthPageProps) {
  const [busy, setBusy] = useState<string | null>(null);

  const installed = status?.installed ?? false;
  const running = status?.running ?? false;
  const shimVersionStale = !!shim?.installed && shim.installed_version !== shim.target_version;
  const shimAlive = shim?.alive ?? false;
  const heartbeatAgo = shim?.last_heartbeat ? Math.max(0, Math.round((Date.now() - shim.last_heartbeat) / 1000)) : -1;

  const run = async (key: string, fn: () => Promise<unknown>, title: string) => {
    setBusy(key);
    try {
      const r = await fn();
      onToast("success", title, typeof r === "string" ? r : undefined);
      await onRefresh();
    } catch (e) {
      onToast("error", `${title}失败`, errText(e));
    } finally {
      setBusy(null);
    }
  };

  return (
    <div className="page-in flex-1 min-w-0 p-6 overflow-y-auto">
      <div className="flex items-center mb-[18px]">
        <h3 className="text-[17px] font-bold">运行检查</h3>
        <span className="text-[12px] text-ink3 ml-3">
          换号所需条件，逐项检查 · {targetIde === "trae-cn" ? "Trae CN" : "Trae 国际版"}
        </span>
        <span className="ml-auto text-[11px] text-ink3">
          {at ? `自动刷新于 ${new Date(at).toLocaleTimeString("zh-CN", { hour12: false })}` : "后台检查中…"}
        </span>
        <button className="btn-ghost ml-2.5" disabled={busy !== null} onClick={() => void onRefresh()}>
          <IconRefresh /> 立即刷新
        </button>
      </div>

      <div className="bg-card border border-line rounded-[12px] shadow-card mb-4">
        <Row
          title="Trae 安装位置"
          desc={installPath || "未找到 Trae 安装位置，请在设置中选择 Trae 目录"}
          state={installed ? "ok" : "bad"}
          text={installed ? "已找到" : "未找到"}
        />
        <Row
          title="Trae 是否正在运行"
          desc={installed ? "未运行不影响安装无感换号支持与修改设置，但无法无感换号" : "需先安装 Trae"}
          state={running ? "ok" : "warn"}
          text={running ? "正在运行" : "未运行"}
          action={
            <button
              className="btn-ghost"
              disabled={!installed || busy !== null}
              onClick={() => void run("launch", () => launchIde(targetIde), "已启动 Trae")}
            >
              {busy === "launch" ? "启动中…" : "启动 Trae"}
            </button>
          }
        />
        <Row
          title="登录状态"
          desc={status?.currentId ? `当前账号：${status.currentId}` : "未读取到登录信息，请先启动 Trae 并登录"}
          state={status?.loggedIn ? "ok" : "warn"}
          text={status?.loggedIn ? "已登录" : "未登录"}
        />
      </div>

      <div className="bg-card border border-line rounded-[12px] shadow-card mb-4">
        <Row
          title="无感换号支持文件"
          desc="安装至 Trae 安装目录，Trae 运行中即可直接换号，并同步账号的设备信息"
          state={status?.tswPatched ? (status?.stalePatch ? "warn" : "ok") : "bad"}
          text={status?.tswPatched ? (status?.stalePatch ? "已安装未生效" : "已安装") : "未安装"}
          action={
            <button
              className="btn-primary"
              disabled={!installed || running || busy !== null}
              title={running ? "Trae 正在运行，请先关闭 Trae 再安装" : "将写入 Trae 安装目录，需管理员权限"}
              onClick={() => void run("inject", () => injectBridge(targetIde), "无感换号支持已安装")}
            >
              {busy === "inject" ? "安装中…" : "安装无感换号支持"}
            </button>
          }
        />
        <Row
          title="会话按账号归类"
          desc="聊天记录与任务按账号分别归类；未安装时换号后会话记录会互相混淆"
          state={status?.chatPinned ? "ok" : "warn"}
          text={status?.chatPinned ? "已安装" : "未安装"}
        />
        <Row
          title="切换助手（无感换号通道）"
          desc={
            shim?.installed
              ? `已安装 ${shim.installed_version ?? "?"}（最新 ${shim.target_version}）· 最近响应：${
                  shimAlive ? `${heartbeatAgo} 秒前` : "已超时"
                }`
              : "未安装；未安装时仅可使用备用通道（需由本应用启动 Trae）"
          }
          state={shimAlive && !shimVersionStale ? "ok" : shim?.installed ? "warn" : "bad"}
          text={shimAlive && !shimVersionStale ? "已就绪" : shim?.installed ? (shimVersionStale ? "版本过旧" : "已安装未生效") : "未安装"}
          action={
            <button
              className="btn-ghost"
              disabled={busy !== null}
              title="安装或更新后需重启 Trae 方可生效"
              onClick={() => void run("shim", () => shimInstall(targetIde), "切换助手已安装（重启 Trae 后生效）")}
            >
              {busy === "shim" ? "处理中…" : shim?.installed ? "重新安装" : "安装"}
            </button>
          }
        />
        <Row
          title="备用通道"
          desc="未安装无感换号支持时的备用方案；需由本应用启动 Trae 后可用"
          state={status?.cdpReady ? "ok" : "unknown"}
          text={status?.cdpReady ? "可用" : "不可用"}
        />
      </div>

      <div className="bg-card border border-line rounded-[12px] shadow-card">
        <Row
          title="无感换号支持是否已生效"
          desc="检查已安装的文件是否已在 Trae 中生效"
          state={status?.stalePatch ? "warn" : injected ? "ok" : "unknown"}
          text={status?.stalePatch ? "安装时间晚于 Trae 启动，需重启 Trae" : injected ? "已生效" : "未生效"}
        />
        <div className="flex items-center gap-2.5 px-[22px] py-4">
          <button
            className="btn-ghost"
            disabled={busy !== null}
            onClick={() => void run("export", () => exportLogs(), "日志已导出")}
          >
            {busy === "export" ? "导出中…" : "导出日志"}
          </button>
          <button
            className="btn-ghost"
            disabled={!installed || running || busy !== null}
            title="从安装目录移除全部无感换号支持文件，供升级 Trae 前或排查异常时使用"
            onClick={() => void run("uninject", () => removeBridge(targetIde), "无感换号支持已移除")}
          >
            {busy === "uninject" ? "移除中…" : "移除无感换号支持"}
          </button>
          <span className="ml-auto text-[11px] text-ink3">
            升级 Trae 会覆盖安装目录，无感换号支持文件与切换助手均需重新安装
          </span>
        </div>
      </div>
    </div>
  );
}
