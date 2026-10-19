interface ConfirmDialogProps {
  open: boolean;
  title: string;
  description: string;
  confirmText?: string;
  onConfirm: () => void;
  onCancel: () => void;
}

export function ConfirmDialog({ open, title, description, confirmText = "删除", onConfirm, onCancel }: ConfirmDialogProps) {
  if (!open) return null;

  return (
    <div className="fixed inset-0 z-40 flex items-center justify-center bg-[#1e2d3d]/25" onClick={onCancel}>
      <div
        className="dialog-in bg-card rounded-[16px] shadow-soft border border-line w-[380px] p-6"
        onClick={(e) => e.stopPropagation()}
      >
        <div className="text-[15px] font-bold">{title}</div>
        <div className="text-[12.5px] text-ink2 mt-2.5 leading-relaxed">{description}</div>
        <div className="flex justify-end gap-2.5 mt-6">
          <button
            className="h-9 px-4 rounded-[10px] border border-line bg-white text-[12.5px] text-ink2 cursor-pointer hover:bg-[#f4f8fa]"
            onClick={onCancel}
          >
            取消
          </button>
          <button
            className="h-9 px-4 rounded-[10px] bg-bad text-white text-[12.5px] font-semibold cursor-pointer hover:opacity-90"
            onClick={onConfirm}
          >
            {confirmText}
          </button>
        </div>
      </div>
    </div>
  );
}
