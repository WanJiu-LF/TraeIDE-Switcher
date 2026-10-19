import { useEffect } from "react";
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

function ToastCard({ item, onDismiss }: { item: ToastItem; onDismiss: (id: string) => void }) {
  // 全部 3 秒自动消失（含错误），可手动提前关闭
  useEffect(() => {
    const t = setTimeout(() => onDismiss(item.id), 3000);
    return () => clearTimeout(t);
  }, [item.id, onDismiss]);

  return (
    <div className="toast-in flex items-stretch bg-card rounded-[10px] shadow-soft border border-line overflow-hidden max-w-[320px]">
      <div className={`w-[3px] flex-none ${KIND_STYLE[item.kind].bar}`} />
      <div className="px-3.5 py-2.5">
        <div className={`text-[11px] font-semibold ${KIND_STYLE[item.kind].badge}`}>{KIND_LABEL[item.kind]}</div>
        <div className="text-[12.5px] text-ink mt-0.5">{item.title}</div>
        {item.desc && <div className="text-[11px] text-ink3 mt-0.5">{item.desc}</div>}
      </div>
      <button
        className="px-2.5 text-ink3 hover:text-ink cursor-pointer"
        onClick={() => onDismiss(item.id)}
        title="关闭"
      >
        <IconClose />
      </button>
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
