import { useEffect, useState } from "react";

interface ConfirmDialogProps {
  open: boolean;
  title: string;
  description: string;
  confirmText?: string;
  onConfirm: () => void;
  onCancel: () => void;
}

/** 关闭时先播退场动画再卸载（延迟 140ms，与 .dialog-out 时长一致） */
const EXIT_MS = 140;

export function ConfirmDialog({ open, title, description, confirmText = "删除", onConfirm, onCancel }: ConfirmDialogProps) {
  const [visible, setVisible] = useState(open);
  const [closing, setClosing] = useState(false);

  useEffect(() => {
    if (open) {
      setVisible(true);
      setClosing(false);
      return;
    }
    if (!visible) return;
    setClosing(true);
    const t = window.setTimeout(() => {
      setVisible(false);
      setClosing(false);
    }, EXIT_MS);
    return () => window.clearTimeout(t);
  }, [open, visible]);

  if (!visible) return null;

  return (
    <div
      className={`${closing ? "overlay-out" : "overlay-in"} fixed inset-0 z-40 flex items-center justify-center bg-[#1e2d3d]/25 backdrop-blur-[2px]`}
      onClick={onCancel}
    >
      <div
        className={`${closing ? "dialog-out" : "dialog-in"} bg-card rounded-[16px] shadow-soft border border-line w-[380px] p-6`}
        onClick={(e) => e.stopPropagation()}
      >
        <div className="text-[15px] font-bold">{title}</div>
        <div className="text-[12.5px] text-ink2 mt-2.5 leading-relaxed">{description}</div>
        <div className="flex justify-end gap-2.5 mt-6">
          <button
            className="h-9 px-4 rounded-[10px] border border-line bg-white text-[12.5px] text-ink2 cursor-pointer transition-all duration-150 hover:bg-[#f4f8fa] active:scale-[0.97]"
            onClick={onCancel}
          >
            取消
          </button>
          <button
            className="h-9 px-4 rounded-[10px] bg-bad text-white text-[12.5px] font-semibold cursor-pointer transition-all duration-150 hover:opacity-90 active:scale-[0.97]"
            onClick={onConfirm}
          >
            {confirmText}
          </button>
        </div>
      </div>
    </div>
  );
}
