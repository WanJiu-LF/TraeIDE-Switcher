import { useEffect, useState } from "react";
import type { Ide } from "../types";
import type { AppSettings } from "../bridge";
import {
  autostart,
  bridgeStatus,
  DEFAULT_SETTINGS,
  errText,
  exportPortable,
  exportVaultBackup,
  getSettings,
  importPortable,
  importVaultBackup,
  injectBridge,
  installPaths,
  launchIde,
  listVaultBackups,
  removeBridge,
  shimInstall,
  shimStatus,
  shimUninstall,
  updateSettings,
} from "../bridge";
import { Select } from "../components/Select";
import type { ShimStatus, VaultBackup } from "../bridge";
import { IconDownload } from "../icons";

interface SettingsPageProps {
  targetIde: Ide;
  onToast: (kind: "success" | "error" | "info", title: string, desc?: string) => void;
  onSettings: (s: AppSettings) => void;
  /** 账号库被整体替换（导入/恢复）后触发，用于刷新账号池 */
  onReload: () => void;
}

function Toggle({ on, onToggle }: { on: boolean; onToggle: () => void }) {
  return (
    <button
      onClick={onToggle}
      className={`w-[38px] h-[21px] rounded-full relative cursor-pointer transition-colors duration-200 active:scale-95 ${on ? "bg-accent" : "bg-[#cfdce4]"}`}
    >
      <span
        className={`absolute w-[17px] h-[17px] bg-white rounded-full top-[2px] left-[2px] shadow-[0_1px_3px_rgba(0,0,0,.2)] transition-transform duration-200 ease-out ${
          on ? "translate-x-[17px]" : "translate-x-0"
        }`}
      />
    </button>
  );
}

function Seg<T extends number | string>({
  options,
  value,
  onChange,
}: {
  options: { value: T; label: string }[];
  value: T;
  onChange: (v: T) => void;
}) {
  return (
    <div className="flex bg-[#f1f6f8] rounded-[9px] p-[3px] gap-0.5">
      {options.map((o) => (
        <button
          key={String(o.value)}
          onClick={() => onChange(o.value)}
          className={`px-3 py-1 rounded-[7px] text-[11.5px] cursor-pointer ${
            value === o.value ? "bg-white text-accent font-semibold" : "text-ink2"
          }`}
        >
          {o.label}
        </button>
      ))}
    </div>
  );
}

function Row({ title, desc, children }: { title: string; desc: string; children: React.ReactNode }) {
  return (
    <div className="flex items-center px-[22px] py-4 border-b border-line-soft last:border-b-0">
      <div>
        <div className="text-[13px] font-semibold">{title}</div>
        <div className="text-[11.5px] text-ink3 mt-0.5">{desc}</div>
      </div>
      <div className="ml-auto flex items-center gap-2.5">{children}</div>
    </div>
  );
}

export function SettingsPage({ targetIde, onToast, onSettings, onReload }: SettingsPageProps) {
  const [settings, setSettings] = useState<AppSettings>(DEFAULT_SETTINGS);
  const [injected, setInjected] = useState<boolean | null>(null);
  const [shim, setShim] = useState<ShimStatus | null>(null);
  const [paths, setPaths] = useState<Record<Ide, string> | null>(null);
  const [busy, setBusy] = useState<"inject" | "remove" | "shim" | null>(null);
  const [backups, setBackups] = useState<VaultBackup[]>([]);
  const [backupSel, setBackupSel] = useState("");
  const [backupBusy, setBackupBusy] = useState(false);
  // 可移植账号包：口令、内容/路径、二次确认（导入会覆盖当前账号库）
  const [portablePass, setPortablePass] = useState("");
  const [portableSrc, setPortableSrc] = useState("");
  const [portableBusy, setPortableBusy] = useState(false);
  const [portableConfirm, setPortableConfirm] = useState(false);

  const loadBackups = async () => {
    try {
      const list = await listVaultBackups();
      setBackups(list);
      setBackupSel((prev) => (prev && list.some((b) => b.path === prev) ? prev : list[0]?.path ?? ""));
    } catch {
      setBackups([]);
    }
  };

  useEffect(() => {
    void loadBackups();
  }, []);

  const runExport = async () => {
    setBackupBusy(true);
    try {
      const path = await exportVaultBackup();
      await loadBackups();
      onToast("success", "已导出账号库备份", path);
    } catch (e) {
      onToast("error", "导出备份失败，请检查磁盘空间或权限后重试", errText(e));
    } finally {
      setBackupBusy(false);
    }
  };

  const runImport = async () => {
    if (!backupSel) return;
    setBackupBusy(true);
    try {
      const msg = await importVaultBackup(backupSel);
      onToast("success", "已从备份恢复", msg);
      await loadBackups();
      onReload();
    } catch (e) {
      onToast("error", "恢复失败，请确认备份文件未损坏后重试", errText(e));
    } finally {
      setBackupBusy(false);
    }
  };

  const runPortableExport = async () => {
    setPortableBusy(true);
    try {
      const r = await exportPortable(portablePass);
      // 回填内容到输入框，便于直接复制到剪贴板转存
      setPortableSrc(r.content);
      onToast("success", "已导出账号包", r.path);
    } catch (e) {
      onToast("error", "导出账号包失败", errText(e));
    } finally {
      setPortableBusy(false);
    }
  };

  const runPortableImport = async () => {
    // 导入会整体覆盖账号库：首次点击仅进入确认态，避免误操作
    if (!portableConfirm) {
      setPortableConfirm(true);
      onToast("info", "请再次点击以确认导入", "导入将覆盖当前账号库（原库自动轮转为 accounts.bak）");
      return;
    }
    setPortableConfirm(false);
    setPortableBusy(true);
    try {
      const msg = await importPortable(portablePass, portableSrc);
      onToast("success", "已导入账号包", msg);
      onReload();
    } catch (e) {
      onToast("error", "导入账号包失败，请确认口令与内容后重试", errText(e));
    } finally {
      setPortableBusy(false);
    }
  };

  // 实际生效的安装路径（手填优先 → 注册表探测），用于把"识别到的路径"显示出来
  useEffect(() => {
    installPaths()
      .then(setPaths)
      .catch(() => setPaths({ trae: "", "trae-cn": "" }));
  }, [targetIde]);

  const pathDesc = (p: string | undefined) =>
    p ? `已自动找到：${p}` : "未找到安装位置，请手动填写";

  useEffect(() => {
    getSettings()
      .then((s) => {
        setSettings(s);
        onSettings(s);
      })
      .catch((e) => onToast("error", "设置读取失败，请重新打开本页", errText(e)));
  }, []);

  const apply = async (patch: Partial<AppSettings>) => {
    setSettings((s) => ({ ...s, ...patch }));
    try {
      const next = await updateSettings(patch);
      setSettings(next);
      onSettings(next);
    } catch (e) {
      onToast("error", "设置保存失败，请重试", errText(e));
    }
  };

  // 定时时刻输入：仅当形如 HH:MM 才落盘，避免半成品（如 "9"）被写进设置
  const setDailyTime = (raw: string) => {
    setSettings((s) => ({ ...s, daily_time: raw }));
    const t = raw.trim();
    if (/^([01]\d|2[0-3]):[0-5]\d$/.test(t)) {
      void apply({ daily_time: t });
    }
  };

  const toggleAutoStart = async () => {
    const value = !settings.auto_start;
    try {
      await autostart(value);
    } catch (e) {
      onToast("error", "开机自启设置失败，请稍后重试", errText(e));
      return;
    }
    await apply({ auto_start: value });
  };

  const refreshStatus = async () => {
    try {
      setInjected(await bridgeStatus(targetIde));
    } catch {
      setInjected(false);
    }
    try {
      setShim(await shimStatus(targetIde));
    } catch {
      setShim(null);
    }
  };

  useEffect(() => {
    void refreshStatus();
  }, [targetIde]);

  const runBridgeOp = async (op: "inject" | "remove") => {
    setBusy(op);
    try {
      const msg = op === "inject" ? await injectBridge(targetIde) : await removeBridge(targetIde);
      await refreshStatus();
      onToast("success", op === "inject" ? "无感换号支持已安装" : "无感换号支持已移除", msg);
    } catch (e) {
      onToast("error", op === "inject" ? "安装无感换号支持失败，请以管理员身份运行本软件后重试" : "卸载无感换号支持失败，请以管理员身份运行本软件后重试", errText(e));
    } finally {
      setBusy(null);
    }
  };

  const runShimOp = async () => {
    const uninstalling = shim?.installed === true;
    setBusy("shim");
    try {
      const msg = uninstalling ? await shimUninstall(targetIde) : await shimInstall(targetIde);
      await refreshStatus();
      onToast("success", uninstalling ? "切换助手已卸载" : "切换助手已安装", msg);
    } catch (e) {
      onToast("error", uninstalling ? "卸载切换助手失败，请稍后重试" : "安装切换助手失败，请稍后重试", errText(e));
    } finally {
      setBusy(null);
    }
  };

  const runLaunch = async () => {
    try {
      await launchIde(targetIde);
      onToast("success", "Trae 已启动");
    } catch (e) {
      onToast("error", "启动 Trae 失败，请检查安装路径后重试", errText(e));
    }
  };

  return (
    <div className="page-in flex-1 min-w-0 p-6 overflow-y-auto">
      <div className="flex items-center mb-[18px]">
        <h3 className="text-[17px] font-bold">设置</h3>
        <div className="text-[12px] text-ink3 ml-3">修改后立即生效</div>
      </div>

      <div className="bg-card border border-line rounded-[12px] shadow-card mb-4">
        <Row title="自动换号" desc="账号额度耗尽时自动切换到下一个可用账号，无需手动操作">
          <Toggle on={settings.auto_switch} onToggle={() => apply({ auto_switch: !settings.auto_switch })} />
        </Row>
        <Row title="换号策略" desc="额度耗尽后切换到的账号：按添加顺序轮换，或优先选择剩余额度最多的账号">
          <Seg
            options={[
              { value: "order", label: "按顺序轮换" },
              { value: "remain", label: "剩余额度优先" },
            ]}
            value={settings.switch_strategy}
            onChange={(v) => apply({ switch_strategy: v })}
          />
        </Row>
        <Row title="换号范围" desc="参与自动换号的账号范围：全部账号，或仅置顶账号（可将常用账号置顶固定）">
          <Seg
            options={[
              { value: "all", label: "全部账号" },
              { value: "pinned", label: "仅置顶账号" },
            ]}
            value={settings.switch_scope}
            onChange={(v) => apply({ switch_scope: v })}
          />
        </Row>
        <Row title="自动换号阈值" desc="账号剩余额度低于此百分比时自动切换到下一个账号（单个账号可在编辑时单独设置）">
          <Seg
            options={[
              { value: 0, label: "0%" },
              { value: 2, label: "2%" },
              { value: 5, label: "5%" },
              { value: 10, label: "10%" },
            ]}
            value={settings.threshold}
            onChange={(v) => apply({ threshold: v })}
          />
        </Row>
        <Row title="额度刷新间隔" desc="自动查询各账号剩余额度的间隔">
          <Seg
            options={[
              { value: 1, label: "1 分钟" },
              { value: 5, label: "5 分钟" },
              { value: 15, label: "15 分钟" },
              { value: 30, label: "30 分钟" },
            ]}
            value={settings.refresh_interval}
            onChange={(v) => apply({ refresh_interval: v })}
          />
        </Row>
      </div>

      <div className="bg-card border border-line rounded-[12px] shadow-card mb-4">
        {/* 每日定时签名为 CN 专有活动，国际版不展示该设置 */}
        {targetIde === "trae-cn" && (
          <Row title="每日定时签到" desc="到达设定时间时自动为全部 Trae CN 账号签到">
            <Toggle on={settings.daily_checkin} onToggle={() => apply({ daily_checkin: !settings.daily_checkin })} />
          </Row>
        )}
        <Row title="每日定时刷新额度" desc="到达设定时间时自动刷新全部账号额度，并记录消耗趋势">
          <Toggle on={settings.daily_refresh} onToggle={() => apply({ daily_refresh: !settings.daily_refresh })} />
        </Row>
        <Row title="定时执行时刻" desc="按本机时间执行，格式 HH:MM；每天执行一次">
          <input
            className="input w-[110px] text-center"
            value={settings.daily_time}
            placeholder="09:00"
            onChange={(e) => setDailyTime(e.target.value)}
          />
        </Row>
      </div>

      <div className="bg-card border border-line rounded-[12px] shadow-card mb-4">
        <div className="px-[22px] py-4 border-b border-line-soft">
          <div className="text-[13px] font-semibold">账号库备份与恢复</div>
          <div className="text-[11.5px] text-ink3 mt-0.5">
            导出的是加密备份文件，仅可在本机、当前 Windows 用户下恢复；每次改动账号库前，自动保留上一版备份。
          </div>
        </div>
        <Row title="导出备份" desc={backups.length ? `已有 ${backups.length} 份备份` : "尚无备份"}>
          <button className="btn-primary" disabled={backupBusy} onClick={runExport}>
            <IconDownload /> {backupBusy ? "处理中…" : "导出当前账号库"}
          </button>
        </Row>
        <Row title="从备份恢复" desc="以选中的备份覆盖当前账号库；覆盖前自动保留当前版本">
          <Select
            className="w-[240px]"
            value={backupSel}
            onChange={setBackupSel}
            placeholder={backups.length ? "选择备份" : "暂无备份"}
            disabled={backups.length === 0}
            options={backups.map((b) => ({
              value: b.path,
              label: `${new Date(b.mtime_ms).toLocaleString("zh-CN", { hour12: false })} · ${Math.max(1, Math.round(b.size / 1024))} KB`,
            }))}
          />
          <button className="btn-ghost" disabled={!backupSel || backupBusy} onClick={runImport}>
            恢复
          </button>
        </Row>
      </div>

      <div className="bg-card border border-line rounded-[12px] shadow-card mb-4">
        <div className="px-[22px] py-4 border-b border-line-soft">
          <div className="text-[13px] font-semibold">账号包导入导出（跨机迁移）</div>
          <div className="text-[11.5px] text-ink3 mt-0.5">
            以口令重新加密账号库（含凭据与设备资产），可在任意 Windows 机器或用户下用同一口令恢复；口令遗失无法找回。
          </div>
        </div>
        <Row title="口令" desc="至少 6 位；导出与导入必须使用同一口令">
          <input
            className="input w-[220px]"
            type="password"
            value={portablePass}
            placeholder="账号包口令"
            onChange={(e) => {
              setPortablePass(e.target.value);
              setPortableConfirm(false);
            }}
          />
          <button
            className="btn-primary disabled:opacity-50"
            disabled={portableBusy || portablePass.trim().length < 6}
            onClick={() => void runPortableExport()}
          >
            {portableBusy ? "处理中…" : "导出账号包"}
          </button>
        </Row>
        <div className="px-[22px] py-4">
          <div className="text-[13px] font-semibold">导入账号包</div>
          <div className="text-[11.5px] text-ink3 mt-0.5 mb-2">
            粘贴账号包内容，或填写账号包文件路径；导入将覆盖当前账号库（原库自动轮转为 accounts.bak）。
          </div>
          <textarea
            className="input w-full h-[96px] font-mono text-[11px] resize-none"
            value={portableSrc}
            placeholder="粘贴账号包内容，或填写文件路径（如 %APPDATA%\TraeIDE-Switcher\backup\portable-xxx.tsw）"
            spellCheck={false}
            onChange={(e) => {
              setPortableSrc(e.target.value);
              setPortableConfirm(false);
            }}
          />
          <div className="flex justify-end mt-2">
            <button
              className={
                portableConfirm
                  ? "h-9 px-4 rounded-[10px] bg-bad text-white text-[12.5px] font-semibold cursor-pointer transition-all duration-150 hover:opacity-90 active:scale-[0.97] disabled:opacity-50 disabled:cursor-not-allowed"
                  : "btn-primary"
              }
              disabled={portableBusy || portablePass.trim().length < 6 || !portableSrc.trim()}
              onClick={() => void runPortableImport()}
            >
              {portableBusy ? "处理中…" : portableConfirm ? "确认导入（覆盖当前账号库）" : "导入账号包"}
            </button>
          </div>
        </div>
      </div>

      <div className="bg-card border border-line rounded-[12px] shadow-card mb-4">
        <div className="px-[22px] py-4 border-b border-line-soft">
          <div className="text-[13px] font-semibold">网络与更新</div>
          <div className="text-[11.5px] text-ink3 mt-0.5">
            仅支持 http / https 代理；本机回环请求（如调试端口）始终直连，不经过代理。
          </div>
        </div>
        <Row title="HTTP 代理" desc="额度、签到、活动、登录、自动更新等全部对外请求经此代理；留空为直连">
          <input
            className="input w-[300px]"
            value={settings.http_proxy}
            placeholder="http://127.0.0.1:7890"
            spellCheck={false}
            onChange={(e) => apply({ http_proxy: e.target.value })}
          />
        </Row>
        <Row title="自定义更新源" desc="留空使用内置 GitHub 发布源；填写 latest.json 完整地址可切换为镜像源">
          <input
            className="input w-[300px]"
            value={settings.update_endpoint}
            placeholder="留空 = 内置源"
            spellCheck={false}
            onChange={(e) => apply({ update_endpoint: e.target.value })}
          />
        </Row>
      </div>

      <div className="bg-card border border-line rounded-[12px] shadow-card mb-4">
        <Row title="开机自启" desc="开机时自动启动并最小化至托盘">
          <Toggle on={settings.auto_start} onToggle={toggleAutoStart} />
        </Row>
        <Row title="关闭到托盘" desc="点击关闭按钮时仅隐藏窗口，后台继续自动换号">
          <Toggle on={settings.close_to_tray} onToggle={() => apply({ close_to_tray: !settings.close_to_tray })} />
        </Row>
        <Row title="系统通知" desc="自动换号、额度耗尽或出错时弹出系统通知提醒">
          <Toggle on={settings.notifications} onToggle={() => apply({ notifications: !settings.notifications })} />
        </Row>
      </div>

      <div className="bg-card border border-line rounded-[12px] shadow-card mb-4">
        <Row title="Trae 安装路径" desc={pathDesc(paths?.trae)}>
          <input
            className="input w-[300px]"
            value={settings.trae_path}
            placeholder={paths?.trae || "自动检测"}
            onChange={(e) => apply({ trae_path: e.target.value })}
          />
          <button className="btn-ghost">浏览…</button>
        </Row>
        <Row title="Trae CN 安装路径" desc={pathDesc(paths?.["trae-cn"])}>
          <input
            className="input w-[300px]"
            value={settings.trae_cn_path}
            placeholder={paths?.["trae-cn"] || "自动检测"}
            onChange={(e) => apply({ trae_cn_path: e.target.value })}
          />
          <button className="btn-ghost">浏览…</button>
        </Row>
      </div>

      <div className="bg-card border border-line rounded-[12px] shadow-card">
        <div className="px-[22px] py-4 border-b border-line-soft">
          <div className="text-[13px] font-semibold">无感换号通道</div>
          <div className="text-[11.5px] text-ink3 mt-0.5">
            「无感换号支持 + 切换助手」为推荐组合：先安装无感换号支持，再安装切换助手，重启 Trae 后即可无感换号；若无感换号支持文件未安装成功，将自动转为备用通道（需从本换号器启动 Trae）。安装时需以管理员身份运行本软件，并先关闭 Trae。
          </div>
        </div>
        <Row title="无感换号支持状态" desc={`当前针对：${targetIde === "trae-cn" ? "Trae CN" : "Trae"}`}>
          <span
            className={`text-[10.5px] px-2 py-0.5 rounded-full border ${
              injected ? "border-accent-line text-accent bg-accent-soft" : "border-line text-ink3"
            }`}
          >
            {injected === null ? "检测中…" : injected ? "已安装" : "未安装"}
          </span>
          <button
            className="btn-primary disabled:opacity-50"
            disabled={busy !== null}
            onClick={() => runBridgeOp("inject")}
          >
            {busy === "inject" ? "处理中…" : "安装无感换号支持"}
          </button>
          <button
            className="btn-ghost disabled:opacity-50"
            disabled={busy !== null}
            onClick={() => runBridgeOp("remove")}
          >
            {busy === "remove" ? "处理中…" : "卸载无感换号支持"}
          </button>
        </Row>
        <Row title="切换助手" desc="无感换号主要依赖它，随 Trae 常驻后台，显示在线即可使用">
          <span
            className={`text-[10.5px] px-2 py-0.5 rounded-full border ${
              shim?.installed && shim.alive
                ? "border-accent-line text-accent bg-accent-soft"
                : shim?.installed
                  ? "border-line text-ink2"
                  : "border-line text-ink3"
            }`}
          >
            {shim === null ? "检测中…" : shim.installed ? (shim.alive ? "在线" : "已安装但未运行") : "未安装"}
          </span>
          <button className="btn-primary disabled:opacity-50" disabled={busy !== null} onClick={runShimOp}>
            {busy === "shim" ? "处理中…" : shim?.installed ? "卸载切换助手" : "安装切换助手"}
          </button>
        </Row>
        <div className="flex items-center px-[22px] py-4">
          <button className="btn-ghost" onClick={runLaunch}>
            启动 Trae（无感换号的备用方式）
          </button>
        </div>
      </div>
    </div>
  );
}
