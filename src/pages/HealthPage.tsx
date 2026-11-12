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
  ok: "chip chip-ok",
  warn: "chip chip-warn",
  bad: "chip chip-bad",
  unknown: "chip chip-neutral",
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
        <span className={STATE_CLS[state]}>{text}</span>
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
        <span className="ml-auto text-[11px] text-ink3">
          {at ? `刷新于 ${new Date(at).toLocaleTimeString("zh-CN", { hour12: false })}` : "后台检查中…"}
        </span>
        <button className="btn-ghost ml-2.5" disabled={busy !== null} onClick={() => void onRefresh()}>
          <IconRefresh /> 立即刷新
        </button>
      </div>

      <div className="panel stagger-in mb-4">
        <Row
          title="Trae 安装位置"
          desc={installPath || "未找到，请在设置中指定"}
          state={installed ? "ok" : "bad"}
          text={installed ? "已找到" : "未找到"}
        />
        <Row
          title="Trae 是否正在运行"
          desc={running ? "运行中" : "未运行无法无感换号"}
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
          desc={status?.currentId ? `当前账号：${status.currentId}` : "启动并登录后读取"}
          state={status?.loggedIn ? "ok" : "warn"}
          text={status?.loggedIn ? "已登录" : "未登录"}
        />
      </div>

      <div className="panel stagger-in mb-4">
        <Row
          title="无感换号支持文件"
          desc="写入安装目录，运行中直接换号"
          state={status?.tswPatched ? (status?.stalePatch ? "warn" : "ok") : "bad"}
          text={status?.tswPatched ? (status?.stalePatch ? "已安装未生效" : "已安装") : "未安装"}
          action={
            <button
              className="btn-primary"
              disabled={!installed || running || busy !== null}
              title={running ? "请先关闭 Trae" : "写入安装目录，需管理员权限"}
              onClick={() => void run("inject", () => injectBridge(targetIde), "无感换号支持已安装")}
            >
              {busy === "inject" ? "安装中…" : "安装无感换号支持"}
            </button>
          }
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
          desc="需从本应用启动 Trae"
          state={status?.cdpReady ? "ok" : "unknown"}
          text={status?.cdpReady ? "可用" : "不可用"}
        />
      </div>

      <div className="panel stagger-in">
        <Row
          title="无感换号支持是否已生效"
          desc="补丁是否已加载"
          state={status?.stalePatch ? "warn" : injected ? "ok" : "unknown"}
          text={status?.stalePatch ? "安装时间晚于 Trae 启动，需重启" : injected ? "已生效" : "未生效"}
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
            title="从安装目录移除全部无感换号支持文件"
            onClick={() => void run("uninject", () => removeBridge(targetIde), "无感换号支持已移除")}
          >
            {busy === "uninject" ? "移除中…" : "移除无感换号支持"}
          </button>
          <span className="ml-auto text-[11px] text-ink3">
            升级 Trae 会覆盖安装目录，需重新安装
          </span>
        </div>
      </div>
    </div>
  );
}
