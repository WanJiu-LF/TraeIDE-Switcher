import { useEffect, useRef, useState } from "react";
import { WebviewWindow } from "@tauri-apps/api/webviewWindow";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import type { Account, Ide } from "../types";
import { cancelLogin, errText, pollLogin, scanLocalAccount, startLogin, type AccountSummary } from "../bridge";
import { IconClose, IconSearch } from "../icons";

export type AccountDraft =
  | { kind: "paste"; ide: Ide; name: string; note: string; authBlob: string }
  | { kind: "scan"; items: { ide: Ide; name: string }[] }
  | { kind: "login"; ide: Ide; name: string; note: string; authBlob: string }
  | { kind: "edit"; name: string; note: string; threshold: number | null };

interface AddAccountDialogProps {
  open: boolean;
  editing: Account | null;
  initialTab?: Tab;
  targetIde: Ide;
  existingIds: string[];
  onClose: () => void;
  onSave: (draft: AccountDraft) => void;
  onToast: (kind: "success" | "error" | "info", title: string, desc?: string) => void;
}

type Tab = "paste" | "scan" | "login";

type CapturePhase = "idle" | "pending" | "done" | "error";

const TABS: { key: Tab; label: string }[] = [
  { key: "paste", label: "粘贴登录信息" },
  { key: "scan", label: "读取本机登录信息" },
  { key: "login", label: "登录窗口添加" },
];

function detectIde(cred: string, fallback: Ide): Ide {
  return /cn|china|volc|bytedance/i.test(cred) ? "trae-cn" : fallback;
}

export function AddAccountDialog({
  open,
  editing,
  initialTab = "paste",
  targetIde,
  existingIds,
  onClose,
  onSave,
  onToast,
}: AddAccountDialogProps) {
  const [tab, setTab] = useState<Tab>(initialTab);
  const [cred, setCred] = useState("");
  const [name, setName] = useState("");
  const [note, setNote] = useState("");
  const [scanning, setScanning] = useState(false);
  const [results, setResults] = useState<AccountSummary[] | null>(null);
  const [scanError, setScanError] = useState("");
  const [checked, setChecked] = useState<string[]>([]);
  const [capturePhase, setCapturePhase] = useState<CapturePhase>("idle");
  const [captureBlob, setCaptureBlob] = useState("");
  const [captureError, setCaptureError] = useState("");
  const [threshold, setThreshold] = useState("");
  const pollTimer = useRef<number | null>(null);

  const stopPolling = () => {
    if (pollTimer.current !== null) {
      window.clearInterval(pollTimer.current);
      pollTimer.current = null;
    }
  };

  const closeLoginWindow = () => {
    void WebviewWindow.getByLabel("login-capture")
      .then((w) => w?.close())
      .catch(() => undefined);
  };

  const cleanupCapture = () => {
    stopPolling();
    closeLoginWindow();
    // 必须通知后端：只关窗会让回调服务器残留，二次采集被"已有登录采集在进行中"拒绝
    void cancelLogin().catch(() => undefined);
  };

  useEffect(() => {
    if (open) {
      setTab(initialTab);
      setCred("");
      setName(editing?.name ?? "");
      setNote(editing?.note ?? "");
      setScanning(false);
      setResults(null);
      setScanError("");
      setChecked([]);
      setCapturePhase("idle");
      setCaptureBlob("");
      setCaptureError("");
      setThreshold(editing?.threshold == null ? "" : String(editing.threshold));
    }
  }, [open, editing, initialTab]);

  useEffect(() => {
    if (open) return;
    cleanupCapture();
  }, [open]);

  useEffect(() => () => cleanupCapture(), []);

  if (!open) return null;

  const editingEmail = editing?.email ?? "";

  const submitPaste = () => {
    const authBlob = cred.trim();
    if (!authBlob || !name.trim()) return;
    onSave({ kind: "paste", ide: detectIde(authBlob, targetIde), name, note, authBlob });
  };

  const startScan = async () => {
    setScanning(true);
    setScanError("");
    try {
      const found: AccountSummary[] = [];
      for (const ide of ["trae", "trae-cn"] as const) {
        const s = await scanLocalAccount(ide);
        if (s) found.push(s);
      }
      setResults(found);
    } catch (e) {
      setScanError(errText(e));
    } finally {
      setScanning(false);
    }
  };

  const submitScan = () => {
    const items = (results ?? [])
      .filter((s) => checked.includes(s.id))
      .map((s) => ({ ide: s.ide, name: s.name }));
    if (items.length === 0) return;
    onSave({ kind: "scan", items });
  };

  const startCapture = async () => {
    try {
      // 清除 webview 登录会话：否则第二次采集会带着上次登录 cookie 直接回调旧账号
      try {
        await getCurrentWebview().clearAllBrowsingData();
      } catch {
        // 权限/环境不支持时忽略，最多需要用户在登录页手动切换账号
      }
      const { url } = await startLogin(targetIde);
      new WebviewWindow("login-capture", { url, width: 480, height: 640, title: "登录 Trae" });
      setCaptureError("");
      setCaptureBlob("");
      setCapturePhase("pending");
      pollTimer.current = window.setInterval(async () => {
        try {
          const r = await pollLogin();
          if (r.status === "pending") return;
          stopPolling();
          closeLoginWindow();
          if (r.status === "done") {
            setCaptureBlob(r.authBlob ?? "");
            setName(r.name || r.email || "");
            setCapturePhase("done");
            // 捕获完成后立即清除登录 cookie，保证下次采集是干净会话
            try {
              await getCurrentWebview().clearAllBrowsingData();
            } catch {
              // 忽略
            }
          } else {
            const msg = r.message || "登录失败";
            setCaptureError(msg);
            setCapturePhase("error");
            onToast("error", "登录失败", msg);
          }
        } catch (e) {
          stopPolling();
          closeLoginWindow();
          const msg = errText(e);
          setCaptureError(msg);
          setCapturePhase("error");
          onToast("error", "登录失败", msg);
        }
      }, 1500);
    } catch (e) {
      onToast("error", "无法打开登录窗口", errText(e));
    }
  };

  const cancelCapture = () => {
    cleanupCapture();
    setCapturePhase("idle");
    setCaptureBlob("");
    setCaptureError("");
  };

  const submitLogin = () => {
    if (!captureBlob || !name.trim()) return;
    onSave({ kind: "login", ide: targetIde, name, note, authBlob: captureBlob });
  };

  return (
    <div
      className="overlay-in fixed inset-0 z-40 flex items-center justify-center bg-[#1e2d3d]/25 backdrop-blur-[2px]"
      onClick={onClose}
    >
      <div
        className="dialog-in bg-card rounded-[16px] shadow-soft border border-line w-[560px] overflow-hidden"
        onClick={(e) => e.stopPropagation()}
      >
        <div className="flex items-center px-6 pt-5 pb-4">
          <div className="text-[15px] font-bold">{editing ? "编辑账号" : "添加账号"}</div>
          <button className="ml-auto text-ink3 hover:text-ink cursor-pointer" onClick={onClose} title="关闭">
            <IconClose />
          </button>
        </div>

        {editing ? (
          <div className="px-6 pb-6">
            <Field label="名称">
              <input
                className="input"
                value={name}
                onChange={(e) => setName(e.target.value)}
                placeholder="账号名称"
              />
            </Field>
            <Field label="邮箱">
              <input className="input opacity-60" value={editingEmail} disabled />
            </Field>
            <Field label="备注">
              <input className="input" value={note} onChange={(e) => setNote(e.target.value)} placeholder="选填" />
            </Field>
            <Field label="剩余额度低于此值自动换号（%）">
              <input
                className="input"
                type="number"
                min={0}
                max={100}
                value={threshold}
                onChange={(e) => setThreshold(e.target.value)}
                placeholder="留空则使用设置页默认值"
              />
            </Field>
            <div className="text-[11px] text-ink3 -mt-2">
              该账号剩余额度低于上述百分比时，自动换号将其视为已耗尽（仅对 Trae 每日额度生效；留空则使用设置页默认值）
            </div>
            <div className="flex justify-end gap-2.5 mt-6">
              <button className="btn-ghost" onClick={onClose}>
                取消
              </button>
              <button
                className="btn-primary"
                onClick={() =>
                  onSave({
                    kind: "edit",
                    name,
                    note,
                    threshold: threshold.trim() === "" ? null : Number(threshold),
                  })
                }
              >
                保存
              </button>
            </div>
          </div>
        ) : (
          <>
            <div className="flex gap-1.5 px-6">
              {TABS.map((t) => (
                <button
                  key={t.key}
                  className={`px-3.5 py-[7px] rounded-[9px] text-[12.5px] cursor-pointer ${
                    tab === t.key ? "bg-accent-soft text-accent font-semibold" : "text-ink2 hover:bg-[#f0f6f7]"
                  }`}
                  onClick={() => setTab(t.key)}
                >
                  {t.label}
                </button>
              ))}
            </div>

            <div className="px-6 py-5 min-h-[300px]">
              {tab === "paste" && (
                <div>
                  <Field label="登录信息">
                    <textarea
                      className="input h-[96px] py-2.5 resize-none items-start"
                      value={cred}
                      onChange={(e) => setCred(e.target.value)}
                      placeholder="粘贴从浏览器复制的登录信息"
                    />
                  </Field>
                  <div className="flex gap-3">
                    <Field label="名称" className="flex-1">
                      <input className="input" value={name} onChange={(e) => setName(e.target.value)} placeholder="留空自动识别" />
                    </Field>
                    <Field label="备注" className="flex-1">
                      <input className="input" value={note} onChange={(e) => setNote(e.target.value)} placeholder="选填" />
                    </Field>
                  </div>
                  <div className="text-[11px] text-ink3 mt-1">
                    程序将根据登录信息自动识别 Trae / Trae CN；无法识别时，使用当前选中版本（{targetIde === "trae-cn" ? "Trae CN" : "Trae"}）
                  </div>
                  <div className="flex justify-end gap-2.5 mt-6">
                    <button className="btn-ghost" onClick={onClose}>
                      取消
                    </button>
                    <button
                      className="btn-primary disabled:opacity-50"
                      disabled={!cred.trim()}
                      onClick={submitPaste}
                    >
                      识别并添加
                    </button>
                  </div>
                </div>
              )}

              {tab === "scan" && (
                <div>
                  {results === null ? (
                    <div className="flex flex-col items-center justify-center h-[220px] gap-4">
                      <div className="text-[12.5px] text-ink2">
                        扫描本机，识别 Trae / Trae CN 中已登录的账号
                      </div>
                      <button className="btn-primary mx-auto" disabled={scanning} onClick={startScan}>
                        {scanning ? (
                          <span className="flex items-center gap-1.5">
                            <span className="w-3.5 h-3.5 rounded-full border-2 border-white border-t-transparent animate-spin" />
                            扫描中…
                          </span>
                        ) : (
                          <>
                            <IconSearch /> 开始扫描
                          </>
                        )}
                      </button>
                      {scanError && <div className="text-[11.5px] text-bad">{scanError}</div>}
                    </div>
                  ) : results.length === 0 ? (
                    <div className="flex flex-col items-center justify-center h-[220px] gap-4">
                      <div className="text-[12.5px] text-ink2">未发现本机已登录的账号</div>
                      {scanError && <div className="text-[11.5px] text-bad">{scanError}</div>}
                      <button className="btn-ghost" onClick={() => setResults(null)}>
                        重新扫描
                      </button>
                    </div>
                  ) : (
                    <div>
                      <div className="text-[12px] text-ink2 mb-2.5">
                        找到 {results.length} 个已登录账号，勾选后加入账号池（已存在的将自动跳过）
                      </div>
                      <div className="border border-line rounded-[10px] divide-y divide-line-soft">
                        {results.map((s) => {
                          const dup = existingIds.includes(s.id);
                          return (
                            <label
                              key={s.id}
                              className={`flex items-center gap-3 px-4 py-2.5 ${dup ? "opacity-45" : "cursor-pointer hover:bg-[#f4fafa]"}`}
                            >
                              <input
                                type="checkbox"
                                className="accent-accent"
                                disabled={dup}
                                checked={checked.includes(s.id)}
                                onChange={(e) =>
                                  setChecked((c) => (e.target.checked ? [...c, s.id] : c.filter((x) => x !== s.id)))
                                }
                              />
                              <div>
                                <div className="text-[12.5px] font-semibold">{s.name}</div>
                                <div className="text-[11px] text-ink3">{s.email}</div>
                              </div>
                              <div className="ml-auto flex gap-1.5">
                                <span className="text-[10.5px] px-2 py-0.5 rounded-full border border-line text-ink2">
                                  {s.ide === "trae-cn" ? "Trae CN" : "Trae"}
                                </span>
                                {dup && (
                                  <span className="text-[10.5px] px-2 py-0.5 rounded-full border border-line text-ink3">
                                    已存在
                                  </span>
                                )}
                              </div>
                            </label>
                          );
                        })}
                      </div>
                      <div className="flex justify-end gap-2.5 mt-6">
                        <button
                          className="btn-ghost"
                          onClick={() => {
                            setResults(null);
                            setChecked([]);
                          }}
                        >
                          重新扫描
                        </button>
                        <button className="btn-primary disabled:opacity-50" disabled={checked.length === 0} onClick={submitScan}>
                          添加所选（{checked.length}）
                        </button>
                      </div>
                    </div>
                  )}
                </div>
              )}

              {tab === "login" && (
                <div>
                  <div className="rounded-[10px] border border-line overflow-hidden">
                    <div className="flex items-center gap-2 px-3.5 py-2 bg-[#f4fafa] border-b border-line-soft">
                      <span
                        className={`w-[7px] h-[7px] rounded-full ${
                          capturePhase === "idle"
                            ? "bg-ink3"
                            : capturePhase === "pending"
                              ? "bg-warn animate-pulse"
                              : capturePhase === "done"
                                ? "bg-ok"
                                : "bg-bad"
                        }`}
                      />
                      <span className={`text-[11.5px] ${capturePhase === "error" ? "text-bad" : "text-ink2"}`}>
                        {capturePhase === "idle"
                          ? "未开始 · 点击下方按钮打开登录窗口"
                          : capturePhase === "pending"
                            ? "等待登录…"
                            : capturePhase === "done"
                              ? "已获取登录信息"
                              : captureError}
                      </span>
                    </div>
                    <div className="h-[150px] flex items-center justify-center text-[12px] text-ink3 bg-[#fbfdfd]">
                      {capturePhase === "idle" && (
                        <button className="btn-primary" onClick={startCapture}>
                          打开登录窗口
                        </button>
                      )}
                      {capturePhase === "pending" && (
                        <div className="text-center">
                          <div className="text-[12.5px] text-ink2">登录窗口已打开，请在新窗口中完成 Trae 登录</div>
                          <div className="text-[11px] text-ink3 mt-1">通过官方登录页完成登录，将自动保存登录信息</div>
                          <button className="btn-ghost mt-3" onClick={cancelCapture}>
                            取消
                          </button>
                        </div>
                      )}
                      {capturePhase === "done" && (
                        <div className="text-[12.5px] text-ink2">已获取登录信息，请在下方填写名称后保存到账号池</div>
                      )}
                      {capturePhase === "error" && (
                        <div className="text-center">
                          <div className="text-[11.5px] text-bad">{captureError}</div>
                          <button className="btn-ghost mt-3" onClick={() => setCapturePhase("idle")}>
                            重试
                          </button>
                        </div>
                      )}
                    </div>
                  </div>

                  {capturePhase === "done" && (
                    <div className="mt-4">
                      <div className="flex gap-3">
                        <Field label="名称" className="flex-1">
                          <input className="input" value={name} onChange={(e) => setName(e.target.value)} placeholder="账号名称" />
                        </Field>
                        <Field label="备注" className="flex-1">
                          <input className="input" value={note} onChange={(e) => setNote(e.target.value)} placeholder="选填" />
                        </Field>
                      </div>
                      <div className="flex justify-end gap-2.5 mt-5">
                        <button className="btn-ghost" onClick={onClose}>
                          取消
                        </button>
                        <button className="btn-primary disabled:opacity-50" disabled={!name.trim()} onClick={submitLogin}>
                          保存到账号池
                        </button>
                      </div>
                    </div>
                  )}
                </div>
              )}
            </div>
          </>
        )}
      </div>
    </div>
  );
}

function Field({ label, className = "", children }: { label: string; className?: string; children: React.ReactNode }) {
  return (
    <div className={`mb-4 ${className}`}>
      <div className="text-[11.5px] text-ink2 mb-1.5">{label}</div>
      {children}
    </div>
  );
}
