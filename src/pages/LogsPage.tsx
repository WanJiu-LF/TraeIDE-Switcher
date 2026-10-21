import { useEffect, useState } from "react";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import type { LogEntry, SwitchRecord } from "../bridge";
import { errText, exportLogs, getLogs, isTauri, switchHistory } from "../bridge";
import { Select } from "../components/Select";
import { IconDownload, IconSearch } from "../icons";

interface LogsPageProps {
  onToast: (kind: "success" | "error" | "info", title: string, desc?: string) => void;
}

const LEVEL_META: Record<LogEntry["level"], { text: string; cls: string }> = {
  Info: { text: "信息", cls: "chip chip-neutral" },
  Warn: { text: "警告", cls: "chip chip-warn" },
  Err: { text: "错误", cls: "chip chip-bad" },
};

const SOURCE_TEXT: Record<LogEntry["source"], string> = {
  Switch: "换号",
  Quota: "额度",
  Inject: "无感换号支持",
  Account: "账号",
  System: "系统",
};

function fmtTime(tsMs: number): string {
  return new Date(tsMs).toLocaleTimeString("zh-CN", { hour12: false });
}

function fmtDateTime(tsMs: number): string {
  const d = new Date(tsMs);
  const md = `${String(d.getMonth() + 1).padStart(2, "0")}/${String(d.getDate()).padStart(2, "0")}`;
  return `${md} ${d.toLocaleTimeString("zh-CN", { hour12: false })}`;
}

export function LogsPage({ onToast }: LogsPageProps) {
  const [view, setView] = useState<"logs" | "switches">("logs");
  const [logs, setLogs] = useState<LogEntry[]>([]);
  const [records, setRecords] = useState<SwitchRecord[]>([]);
  const [search, setSearch] = useState("");
  const [level, setLevel] = useState<"all" | LogEntry["level"]>("all");
  const [source, setSource] = useState("all");

  useEffect(() => {
    const reload = async () => {
      try {
        setLogs(await getLogs());
      } catch (e) {
        onToast("error", "运行记录加载失败，请稍后重试", errText(e));
      }
      try {
        setRecords(await switchHistory());
      } catch {
        // 切换记录读取失败静默（首次运行尚无文件属正常）
      }
    };
    void reload();
    if (!isTauri()) return;
    let unlisten: UnlistenFn | undefined;
    let disposed = false;
    listen("log-changed", () => void reload()).then((fn) => {
      if (disposed) fn();
      else unlisten = fn;
    });
    return () => {
      disposed = true;
      unlisten?.();
    };
  }, []);

  const sources = Array.from(new Set(logs.map((l) => l.source)));
  const visible = logs.filter((l) => {
    if (level !== "all" && l.level !== level) return false;
    if (source !== "all" && l.source !== source) return false;
    if (search && !l.message.toLowerCase().includes(search.toLowerCase())) return false;
    return true;
  });

  const history = [...records].reverse();
  const okCount = records.filter((r) => r.ok).length;

  const handleExport = async () => {
    try {
      const path = await exportLogs();
      onToast("success", `已导出到 ${path}`);
    } catch (e) {
      onToast("error", "导出失败，请稍后重试或更换可写入的目录", errText(e));
    }
  };

  return (
    <div className="page-in flex-1 min-w-0 p-6 overflow-y-auto">
      <div className="flex items-center mb-[18px]">
        <h3 className="text-[17px] font-bold">运行记录</h3>
        <div className="seg ml-3">
          {(
            [
              { key: "logs", label: `运行日志` },
              { key: "switches", label: `换号记录` },
            ] as const
          ).map((t) => (
            <button
              key={t.key}
              onClick={() => setView(t.key)}
              className={`seg-tab ${view === t.key ? "seg-tab-active" : ""}`}
            >
              {t.label}
            </button>
          ))}
        </div>
        <div className="text-[12px] text-ink3 ml-3">
          {view === "logs"
            ? `软件运行过程记录，可用于排查问题 · 共 ${logs.length} 条 · 保留 30 天`
            : `每次换号的存档记录 · 共 ${records.length} 次 · 成功 ${okCount} · 失败 ${records.length - okCount}`}
        </div>
      </div>

      {view === "logs" ? (
        <>
          <div className="flex items-center gap-2.5 mb-4">
            <div className="flex-none w-[240px] input">
              <IconSearch className="w-3.5 h-3.5 text-ink3" />
              <input
                className="flex-1 min-w-0 outline-none bg-transparent text-ink text-[12.5px]"
                placeholder="搜索记录内容…"
                value={search}
                onChange={(e) => setSearch(e.target.value)}
              />
            </div>
            <Select
              className="w-[140px]"
              value={level}
              onChange={(v) => setLevel(v as typeof level)}
              options={[
                { value: "all", label: "级别：全部" },
                { value: "Info", label: "级别：信息" },
                { value: "Warn", label: "级别：警告" },
                { value: "Err", label: "级别：错误" },
              ]}
            />
            <Select
              className="w-[150px]"
              value={source}
              onChange={setSource}
              options={[
                { value: "all", label: "来源：全部" },
                ...sources.map((s) => ({ value: s, label: `来源：${SOURCE_TEXT[s]}` })),
              ]}
            />
            <span className="input w-auto flex-none font-sans text-ink3">
              时间：今天
            </span>
            <button className="btn-ghost ml-auto" onClick={handleExport}>
              <IconDownload /> 导出日志
            </button>
          </div>

          <div className="panel overflow-hidden">
            {visible.length === 0 ? (
              <div className="h-[200px] flex items-center justify-center text-[12.5px] text-ink3">无匹配记录，可清空搜索条件或将级别、来源筛选改为“全部”</div>
            ) : (
              visible.map((l) => (
                <div key={l.id} className="flex items-center gap-3 px-[18px] py-[11px] border-b border-line-soft last:border-b-0">
                  <div className={`${LEVEL_META[l.level].cls} w-[52px] flex-none justify-center`}>
                    {LEVEL_META[l.level].text}
                  </div>
                  <div className="font-mono text-[11.5px] text-ink3 flex-none">{fmtTime(l.ts_ms)}</div>
                  <div className="w-[86px] flex-none text-[11.5px] text-ink2">[{SOURCE_TEXT[l.source]}]</div>
                  <div className="flex-1 text-[12px] text-ink truncate">{l.message}</div>
                </div>
              ))
            )}
          </div>
        </>
      ) : (
        <div className="panel overflow-hidden">
          {history.length === 0 ? (
            <div className="h-[200px] flex items-center justify-center text-[12.5px] text-ink3">
              暂无换号记录。每次手动或自动换号成功后，此处将保留一条记录
            </div>
          ) : (
            history.map((r, i) => (
              <div key={`${r.ts}-${i}`} className="flex items-center gap-3 px-[18px] py-[11px] border-b border-line-soft last:border-b-0">
                <div
                  className={`chip ${r.ok ? "chip-ok" : "chip-bad"} w-[52px] flex-none justify-center`}
                >
                  {r.ok ? "成功" : "失败"}
                </div>
                <div className="font-mono text-[11.5px] text-ink3 flex-none w-[76px]">{fmtDateTime(r.ts)}</div>
                <div className="w-[86px] flex-none text-[11.5px] text-ink2">{r.hot ? "无感换号" : "重启切换"}</div>
                <div className="flex items-center gap-1.5 min-w-0 flex-1">
                  <span className="text-[12px] text-ink2 truncate max-w-[160px]" title={r.from || "（未知）"}>
                    {r.from || "（未知）"}
                  </span>
                  <span className="text-ink3 flex-none">→</span>
                  <span className="text-[12px] font-semibold text-ink truncate max-w-[160px]" title={r.to}>
                    {r.to}
                  </span>
                  <span className="chip chip-neutral flex-none">
                    {r.ide}
                  </span>
                  {r.ms > 0 && <span className="text-[10.5px] text-ink3 flex-none">{r.ms} ms</span>}
                </div>
                {r.note && !r.ok && (
                  <div className="text-[11px] text-bad truncate max-w-[300px] flex-none" title={r.note}>
                    {r.note}
                  </div>
                )}
              </div>
            ))
          )}
        </div>
      )}
    </div>
  );
}
