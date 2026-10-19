import { getCurrentWindow } from "@tauri-apps/api/window";

export function TitleBar() {
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
      <div className="flex items-center gap-2 text-[12.5px] text-ink2">
        <div title="Trae 换号器" className="w-4 h-4 rounded-[5px] bg-gradient-to-br from-accent to-[#3ecfc0]" />
        Trae 换号器
      </div>
      <div className="ml-auto flex">
        <button
          className="w-[42px] h-8 flex items-center justify-center text-ink3 text-[11px] rounded-md hover:bg-line-soft"
          onClick={() => safe((w) => w.minimize())}
        >
          —
        </button>
        <button
          className="w-[42px] h-8 flex items-center justify-center text-ink3 text-[11px] rounded-md hover:bg-line-soft"
          onClick={() => safe((w) => w.toggleMaximize())}
        >
          ▢
        </button>
        <button
          className="w-[42px] h-8 flex items-center justify-center text-ink3 text-[11px] rounded-md hover:bg-[#e8484c] hover:text-white"
          onClick={() => safe((w) => w.close())}
        >
          ✕
        </button>
      </div>
    </div>
  );
}
