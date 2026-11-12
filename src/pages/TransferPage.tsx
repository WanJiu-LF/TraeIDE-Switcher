import { useEffect, useState } from "react";
import type { Ide } from "../types";
import {
  errText,
  listSessionBackups,
  listTransferCandidates,
  restoreSessionBackup,
  scanLocalAccount,
  transferSessions,
} from "../bridge";
import type { AccountSummary, SessionBackup, TransferCandidate } from "../bridge";
import { Select } from "../components/Select";

interface TransferPageProps {
  targetIde: Ide;
  /** 目标版本是否在运行（迁移要求完全退出 IDE） */
  running?: boolean;
  onToast: (kind: "success" | "error" | "info", title: string, desc?: string) => void;
}

function Row({ title, desc, children }: { title: string; desc: string; children?: React.ReactNode }) {
  return (
    <div className="flex items-center px-[22px] py-4 border-b border-line-soft last:border-b-0">
      <div>
        <div className="text-[13px] font-semibold">{title}</div>
        <div className="text-[11.5px] text-ink3 mt-0.5">{desc}</div>
      </div>
      {children && <div className="ml-auto flex items-center gap-2.5">{children}</div>}
    </div>
  );
}

function fmtTime(secs: number): string {
  if (!secs) return "—";
  return new Date(secs * 1000).toLocaleString("zh-CN", { hour12: false });
}

export function TransferPage({ targetIde, running, onToast }: TransferPageProps) {
  const ideLabel = targetIde === "trae-cn" ? "Trae CN" : "Trae";
  const [current, setCurrent] = useState<AccountSummary | null>(null);
  const [busy, setBusy] = useState(false);
  const [backups, setBackups] = useState<SessionBackup[]>([]);
  const [sel, setSel] = useState("");
  const [cands, setCands] = useState<TransferCandidate[]>([]);
  const [selIds, setSelIds] = useState<string[]>([]);
  const [candErr, setCandErr] = useState("");
  const [loadingCands, setLoadingCands] = useState(false);

  const loadCurrent = async () => {
    try {
      setCurrent(await scanLocalAccount(targetIde));
    } catch {
      setCurrent(null);
    }
  };

  const loadBackups = async () => {
    try {
      const list = await listSessionBackups(targetIde);
      setBackups(list);
      setSel((prev) => (prev && list.some((b) => b.path === prev) ? prev : list[0]?.path ?? ""));
    } catch {
      setBackups([]);
    }
  };

  const loadCands = async () => {
    setLoadingCands(true);
    setCandErr("");
    try {
      const list = await listTransferCandidates(targetIde);
      setCands(list);
      setSelIds((prev) => prev.filter((id) => list.some((c) => c.session_id === id && c.migratable)));
    } catch (e) {
      setCands([]);
      setCandErr(errText(e));
    } finally {
      setLoadingCands(false);
    }
  };

  useEffect(() => {
    void loadCurrent();
    void loadBackups();
    void loadCands();
  }, [targetIde]);

  const runTransfer = async (only?: string[]) => {
    setBusy(true);
    try {
      const r = await transferSessions(targetIde, only);
      onToast("success", r.summary, r.backup ? `迁移前整库备份：${r.backup}` : undefined);
      setSelIds([]);
      await Promise.all([loadBackups(), loadCurrent(), loadCands()]);
    } catch (e) {
      const msg = errText(e);
      // 运行中点击的一次只负责采集对话库密钥，不是失败
      if (msg.startsWith("已获取对话库密钥")) onToast("info", "已采集对话库密钥", msg);
      else onToast("error", "会话迁移失败", msg);
    } finally {
      setBusy(false);
    }
  };

  const runRestore = async () => {
    if (!sel) return;
    setBusy(true);
    try {
      const msg = await restoreSessionBackup(targetIde, sel);
      onToast("success", "已回滚对话库", msg);
      await Promise.all([loadBackups(), loadCands()]);
    } catch (e) {
      onToast("error", "回滚对话库失败", errText(e));
    } finally {
      setBusy(false);
    }
  };

  const selectAllMigratable = () => setSelIds(cands.filter((c) => c.migratable).map((c) => c.session_id));
  const canRun = !busy && !!current && !running;
  const migratableCount = cands.filter((c) => c.migratable).length;

  return (
    <div className="page-in flex-1 min-w-0 p-6 overflow-y-auto">
      <div className="flex items-center mb-[18px]">
        <h3 className="text-[17px] font-bold">会话迁移</h3>
      </div>

      <div className="panel mb-4">
        <Row
          title="目标版本"
          desc={`仅作用于 ${ideLabel} 自己的对话库`}
        >
          <span className="chip chip-neutral">{ideLabel}</span>
        </Row>
        <Row
          title="当前登录账号"
          desc={current ? "会话将挂到该账号" : "未检测到登录态"}
        >
          <span className={current ? "chip chip-accent" : "chip chip-neutral"}>
            {current ? current.name : "未登录"}
          </span>
        </Row>
        <Row
          title="IDE 运行状态"
          desc={running ? "需先完全退出 Trae" : "已退出，可以执行迁移"}
        >
          <span className={running ? "chip chip-warn" : "chip chip-accent"}>{running ? "运行中" : "未运行"}</span>
        </Row>
      </div>

      <div className="panel mb-4">
        <div className="flex items-center px-[22px] py-4 border-b border-line-soft">
          <div className="min-w-0">
            <div className="text-[13px] font-semibold">选择会话</div>
            <div className="text-[11.5px] text-ink3 mt-0.5 truncate" title={candErr || undefined}>
              {loadingCands
                ? "正在读取对话库…"
                : candErr
                  ? candErr
                  : cands.length
                    ? `其他账号 ${cands.length} 条 · 可迁移 ${migratableCount} · 已选 ${selIds.length}`
                    : "无其他账号会话"}
            </div>
          </div>
          <div className="ml-auto flex items-center gap-2 shrink-0">
            <button className="btn-ghost" disabled={busy || loadingCands} onClick={() => void loadCands()}>
              刷新
            </button>
            <button className="btn-ghost" disabled={busy || migratableCount === 0} onClick={selectAllMigratable}>
              全选可迁移
            </button>
            <button className="btn-ghost" disabled={busy || selIds.length === 0} onClick={() => setSelIds([])}>
              清空
            </button>
          </div>
        </div>
        {cands.length > 0 && (
          <div className="max-h-[300px] overflow-y-auto">
            {cands.map((c) => (
              <label
                key={c.session_id}
                className={`flex items-center gap-3 px-[22px] py-2.5 border-b border-line-soft last:border-b-0 ${
                  c.migratable ? "cursor-pointer hover:bg-[#f4fafa]" : "opacity-45"
                }`}
              >
                <input
                  type="checkbox"
                  className="accent-accent"
                  disabled={!c.migratable || busy}
                  checked={selIds.includes(c.session_id)}
                  onChange={(e) =>
                    setSelIds((prev) =>
                      e.target.checked ? [...prev, c.session_id] : prev.filter((x) => x !== c.session_id),
                    )
                  }
                />
                <div className="min-w-0 flex-1">
                  <div className="text-[12.5px] font-medium truncate" title={c.title}>
                    {c.title}
                  </div>
                  <div className="text-[11px] text-ink3 truncate" title={`${c.account} · ${c.workspace}`}>
                    {c.account} · {c.workspace || "未知工作区"}
                  </div>
                </div>
                <div className="text-[11px] text-ink3 shrink-0">{fmtTime(c.updated_at)}</div>
                {!c.migratable && <span className="chip chip-neutral shrink-0">无对应工作区</span>}
              </label>
            ))}
          </div>
        )}
        <div className="flex items-center px-[22px] py-3">
          <div className="text-[11.5px] text-ink3">
            置灰：当前账号未打开过该工作区，本次跳过
          </div>
          <div className="ml-auto flex items-center gap-2 shrink-0">
            <button className="btn-ghost" disabled={!canRun} onClick={() => void runTransfer()}>
              全部迁移
            </button>
            <button
              className="btn-primary disabled:opacity-50"
              disabled={!canRun || selIds.length === 0}
              onClick={() => void runTransfer(selIds)}
            >
              {busy ? "处理中…" : `迁移选中（${selIds.length}）`}
            </button>
          </div>
        </div>
      </div>

      <div className="panel mb-4">
        <div className="px-[22px] py-4 border-b border-line-soft">
          <div className="text-[13px] font-semibold">迁移规则</div>
          <div className="text-[11.5px] text-ink3 mt-0.5 leading-relaxed">
            按工作区对位；当前账号没打开过的文件夹，会话留在原账号。
            <br />
            不新建项目、不改消息；迁移前自动整库备份，可用下方回滚。
            <br />
            首次使用需在 {ideLabel} 运行时点一次迁移以采集对话库密钥。
          </div>
        </div>
        <Row
          title="回滚到备份"
          desc={backups.length ? `${backups.length} 份备份` : "暂无备份（迁移前自动生成）"}
        >
          <Select
            className="w-[260px]"
            value={sel}
            onChange={setSel}
            placeholder={backups.length ? "选择备份" : "暂无备份"}
            disabled={backups.length === 0}
            options={backups.map((b) => ({
              value: b.path,
              label: `${new Date(b.mtime_ms).toLocaleString("zh-CN", { hour12: false })} · ${Math.max(1, Math.round(b.size / 1024))} KB`,
            }))}
          />
          <button className="btn-ghost" disabled={!sel || busy || running} onClick={() => void runRestore()}>
            恢复
          </button>
        </Row>
      </div>
    </div>
  );
}
