import { useEffect, useState } from "react";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { IconLogo } from "../icons";

/** 窗口最大化/还原时的内容轻量过渡：移除→重排→重加，保证连续点击也能重启动画 */
function settlePulse() {
  const el = document.documentElement;
  el.classList.remove("win-settle");
  void el.offsetWidth;
  el.classList.add("win-settle");
  window.setTimeout(() => el.classList.remove("win-settle"), 240);
}

const BTN = "w-[42px] h-8 flex items-center justify-center text-ink3 text-[11px] rounded-md transition-all duration-150 active:scale-90";

export function TitleBar() {
  const [maxed, setMaxed] = useState(false);

  useEffect(() => {
    const w = getCurrentWindow();
    let disposed = false;
    let unlisten: (() => void) | undefined;
    const sync = () => {
      w.isMaximized()
        .then((v) => {
          if (!disposed) setMaxed(v);
        })
        .catch(() => undefined);
    };
    sync();
    w.onResized(sync)
      .then((fn) => {
        if (disposed) fn();
        else unlisten = fn;
      })
      .catch(() => undefined);
    return () => {
      disposed = true;
      unlisten?.();
    };
  }, []);

  const safe = (fn: (w: ReturnType<typeof getCurrentWindow>) => void | Promise<unknown>) => {
    try {
      Promise.resolve(fn(getCurrentWindow())).catch(() => {
        // 非 Tauri 环境（纯浏览器预览）下静默忽略窗口控制失败
      });
    } catch {
      // 同步抛出（非 Tauri 环境）同样静默忽略
    }
  };

  return (
    <div
      data-tauri-drag-region
      className="h-[42px] flex-none flex items-center px-2 pl-4 bg-[#fdfefe] border-b border-line-soft"
    >
      <div className="flex items-center gap-2 text-[12.5px] text-ink2" title="TraeIDE-Switcher">
        <IconLogo className="w-[18px] h-[18px] flex-none" />
        TraeIDE-Switcher
      </div>
      <div className="ml-auto flex">
        <button
          className={`${BTN} hover:bg-line-soft hover:text-ink2`}
          title="最小化"
          onClick={() => safe((w) => w.minimize())}
        >
          <span className="block w-3 h-[1.5px] bg-current rounded-full" />
        </button>
        <button
          className={`${BTN} hover:bg-line-soft hover:text-ink2`}
          title={maxed ? "还原" : "最大化"}
          onClick={() => {
            settlePulse();
            safe((w) => w.toggleMaximize());
          }}
        >
          {/* 最大化/还原图标交叉淡入淡出，避免图标瞬时跳变 */}
          <span className="relative block w-[11px] h-[11px]">
            <span
              className={`absolute inset-0 border border-current rounded-[2px] transition-all duration-200 ${
                maxed ? "opacity-0 scale-75" : "opacity-100 scale-100"
              }`}
            />
            <span
              className={`absolute inset-0 transition-all duration-200 ${
                maxed ? "opacity-100 scale-100" : "opacity-0 scale-75"
              }`}
            >
              <span className="absolute inset-0 border border-current rounded-[2px]" />
              <span className="absolute top-[2px] left-[2px] w-[9px] h-[9px] border border-current rounded-[2px] bg-[#fdfefe]" />
            </span>
          </span>
        </button>
        <button
          className={`${BTN} hover:bg-[#e8484c] hover:text-white`}
          title="关闭"
          onClick={() => safe((w) => w.close())}
        >
          ✕
        </button>
      </div>
    </div>
  );
}
