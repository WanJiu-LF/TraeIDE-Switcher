import { useCallback, useEffect, useRef, useState } from "react";
import type { ToastItem } from "../types";
import { IconClose } from "../icons";

interface ToastsProps {
  items: ToastItem[];
  onDismiss: (id: string) => void;
}

const KIND_STYLE: Record<ToastItem["kind"], { bar: string; badge: string }> = {
  success: { bar: "bg-ok", badge: "text-ok" },
  error: { bar: "bg-bad", badge: "text-bad" },
  info: { bar: "bg-accent", badge: "text-accent" },
};

const KIND_LABEL: Record<ToastItem["kind"], string> = {
  success: "成功",
  error: "错误",
  info: "提示",
};

const EXIT_MS = 180;

function ToastCard({ item, onDismiss }: { item: ToastItem; onDismiss: (id: string) => void }) {
  const [leaving, setLeaving] = useState(false);
  const [hovered, setHovered] = useState(false);
  /** 每次恢复倒计时递增：同时重挂进度条与计时器，保证二者同步 */
  const [run, setRun] = useState(0);
  const aliveRef = useRef(true);
  // onDismiss 每次渲染都会变化，用 ref 持有避免倒计时被父组件重渲染打断
  const dismissRef = useRef(onDismiss);
  useEffect(() => {
    dismissRef.current = onDismiss;
  });

  const close = useCallback(() => {
    if (!aliveRef.current) return;
    aliveRef.current = false;
    setLeaving(true);
    window.setTimeout(() => dismissRef.current(item.id), EXIT_MS);
  }, [item.id]);

  useEffect(() => {
    if (hovered) return;
    const t = window.setTimeout(close, 3000);
    return () => window.clearTimeout(t);
  }, [hovered, run, close]);

  return (
    <div
      className={`${leaving ? "toast-out" : "toast-in"} relative flex items-stretch bg-card rounded-[12px] shadow-soft border border-line overflow-hidden max-w-[320px]`}
      onMouseEnter={() => setHovered(true)}
      onMouseLeave={() => {
        setHovered(false);
        setRun((v) => v + 1);
      }}
    >
      <div className={`w-[3px] flex-none ${KIND_STYLE[item.kind].bar}`} />
      <div className="flex-1 px-3.5 py-2.5">
        <div className={`text-[11px] font-semibold ${KIND_STYLE[item.kind].badge}`}>{KIND_LABEL[item.kind]}</div>
        <div className="text-[12.5px] text-ink mt-0.5">{item.title}</div>
        {item.desc && <div className="text-[11.5px] text-ink3 mt-0.5">{item.desc}</div>}
      </div>
      <button
        className="px-2.5 text-ink3 hover:text-ink cursor-pointer transition-colors duration-150"
        onClick={close}
        title="关闭"
      >
        <IconClose />
      </button>
      <div className="absolute bottom-0 left-0 right-0 h-[2px] bg-line-soft/60">
        <div
          key={run}
          className={`life-bar h-full ${KIND_STYLE[item.kind].bar}`}
          style={{ animationPlayState: hovered ? "paused" : "running" }}
        />
      </div>
    </div>
  );
}

export function Toasts({ items, onDismiss }: ToastsProps) {
  return (
    <div className="fixed right-5 bottom-10 flex flex-col gap-2.5 z-50">
      {items.map((t) => (
        <ToastCard key={t.id} item={t} onDismiss={onDismiss} />
      ))}
    </div>
  );
}
