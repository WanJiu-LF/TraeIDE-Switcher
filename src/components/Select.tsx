// 自绘下拉：原生 <select> 的弹出层无法用 CSS 控制（Windows 下是方角 + 系统深色高亮），
// 与应用视觉严重割裂，故统一替换为本组件（按钮 + 绝对定位列表，走同一套设计 token）。
import { useEffect, useRef, useState } from "react";
import { IconChevronDown } from "../icons";

export interface SelectOption {
  value: string;
  label: string;
}

interface SelectProps {
  value: string;
  options: SelectOption[];
  onChange: (value: string) => void;
  /** 外层定位用；宽度可用 className 控制（如 w-[240px]） */
  className?: string;
  placeholder?: string;
  disabled?: boolean;
  /** 列表展开方向：向下（默认）或向上（贴近窗口底部时用） */
  dropUp?: boolean;
}

export function Select({
  value,
  options,
  onChange,
  className = "",
  placeholder = "请选择",
  disabled = false,
  dropUp = false,
}: SelectProps) {
  const [open, setOpen] = useState(false);
  const [closing, setClosing] = useState(false);
  const closeTimer = useRef<number | null>(null);
  const rootRef = useRef<HTMLDivElement>(null);
  const current = options.find((o) => o.value === value);

  useEffect(
    () => () => {
      if (closeTimer.current !== null) window.clearTimeout(closeTimer.current);
    },
    [],
  );

  /** 收起时先播退场动画再卸载（140ms，与退场动画时长一致） */
  const close = () => {
    if (closeTimer.current !== null) window.clearTimeout(closeTimer.current);
    setClosing(true);
    closeTimer.current = window.setTimeout(() => {
      setOpen(false);
      setClosing(false);
      closeTimer.current = null;
    }, 140);
  };
  const toggle = () => {
    if (open) {
      close();
      return;
    }
    if (closeTimer.current !== null) {
      window.clearTimeout(closeTimer.current);
      closeTimer.current = null;
    }
    setClosing(false);
    setOpen(true);
  };

  useEffect(() => {
    if (!open) return;
    const onDocDown = (e: MouseEvent) => {
      if (rootRef.current && !rootRef.current.contains(e.target as Node)) close();
    };
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") close();
    };
    document.addEventListener("mousedown", onDocDown);
    document.addEventListener("keydown", onKey);
    return () => {
      document.removeEventListener("mousedown", onDocDown);
      document.removeEventListener("keydown", onKey);
    };
    // close 每次渲染都会重建，语义上只依赖 open
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [open]);

  return (
    <div ref={rootRef} className={`relative ${className}`}>
      <button
        type="button"
        disabled={disabled}
        onClick={toggle}
        className={`w-full h-[34px] flex items-center gap-2 px-3 bg-white border rounded-[10px] text-[12.5px] transition-colors ${
          disabled
            ? "border-line opacity-60 cursor-default"
            : open
              ? "border-accent cursor-pointer"
              : "border-line hover:border-accent-line cursor-pointer"
        }`}
      >
        <span className={`flex-1 min-w-0 text-left truncate ${current ? "text-ink" : "text-ink3"}`}>
          {current ? current.label : placeholder}
        </span>
        <IconChevronDown
          className={`w-3.5 h-3.5 flex-none text-ink3 transition-transform duration-150 ${open ? "rotate-180" : ""}`}
        />
      </button>

      {open && (
        <div
          className={`absolute z-40 left-0 right-0 bg-white border border-line rounded-[10px] shadow-soft py-1 max-h-[240px] overflow-y-auto ${
            dropUp
              ? `${closing ? "dropdown-out-up" : "dropdown-in-up"} bottom-[38px]`
              : `${closing ? "dropdown-out" : "dropdown-in"} top-[38px]`
          }`}
        >
          {options.length === 0 ? (
            <div className="px-3 py-2 text-[12px] text-ink3">暂无可选项</div>
          ) : (
            options.map((o) => {
              const active = o.value === value;
              return (
                <button
                  key={o.value}
                  type="button"
                  onClick={() => {
                    onChange(o.value);
                    close();
                  }}
                  className={`w-full flex items-center gap-2 text-left px-3 py-[7px] text-[12.5px] cursor-pointer ${
                    active ? "bg-accent-soft text-accent font-semibold" : "text-ink2 hover:bg-[#f0f6f7]"
                  }`}
                >
                  <span className="flex-1 min-w-0 truncate">{o.label}</span>
                  {active && <span className="flex-none text-[11px]">✓</span>}
                </button>
              );
            })
          )}
        </div>
      )}
    </div>
  );
}
