import type { IdeStatus } from "../bridge";
import type { Ide } from "../types";

interface StatusBarProps {
  targetIde: Ide;
  autoSwitch: boolean;
  ideStatus: IdeStatus | null;
}

export function StatusBar({ targetIde, autoSwitch, ideStatus }: StatusBarProps) {
  const name = targetIde === "trae" ? "Trae" : "Trae CN";
  const installed = ideStatus?.installed ?? false;
  const running = ideStatus?.running ?? false;
  const stale = running && (ideStatus?.stalePatch ?? false);
  const hotReady = !stale && ((ideStatus?.tswPatched && ideStatus?.shimAlive) || ideStatus?.cdpReady);
  const hotText = !installed
    ? "未知（未检测到安装）"
    : stale
      ? "无感换号支持尚未生效（需重启 Trae）"
      : hotReady
        ? "就绪"
        : ideStatus?.tswPatched
          ? "切换助手启动中"
          : "未安装无感换号支持（需管理员权限）";
  return (
    <footer className="h-[30px] flex-none flex items-center gap-4 px-4 bg-[#fdfefe] border-t border-line-soft text-[11.5px] text-ink2">
      <div className="flex items-center">
        <span
          className={`w-[7px] h-[7px] rounded-full inline-block mr-1.5 ${
            !installed ? "bg-bad" : running ? "bg-ok" : "bg-ink3"
          }`}
        />
        {name}：{!installed ? "未安装" : running ? "运行中" : "未运行"}
      </div>
      <span className="text-ink3">|</span>
      <div className="flex items-center">
        <span
          className={`w-[7px] h-[7px] rounded-full inline-block mr-1.5 ${hotReady ? "bg-ok" : "bg-warn"}`}
        />
        无感换号：{hotText}
      </div>
      <span className="text-ink3">|</span>
      <div>自动换号：{autoSwitch ? "开" : "关"}</div>
      <div className="ml-auto flex gap-4 text-ink3">
        <span>v0.1.0</span>
      </div>
    </footer>
  );
}
