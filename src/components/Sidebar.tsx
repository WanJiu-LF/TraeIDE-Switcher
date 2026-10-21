import type { Page, Ide } from "../types";
import {
  IconGrid,
  IconList,
  IconGear,
  IconInfo,
  IconPulse,
  IconSpark,
  IconChevronLeft,
  IconChevronRight,
  IconLogo,
} from "../icons";

interface SidebarProps {
  page: Page;
  onPage: (p: Page) => void;
  collapsed: boolean;
  onToggle: () => void;
  targetIde: Ide;
  onTargetIde: (ide: Ide) => void;
  logUnread: number;
  /** 目标版本实际生效的安装路径（未检测到为空串） */
  installPath?: string;
}

const NAV: { key: Page; label: string; icon: typeof IconGrid }[] = [
  { key: "accounts", label: "账号池", icon: IconGrid },
  { key: "assets", label: "积分与诊断", icon: IconSpark },
  { key: "logs", label: "日志", icon: IconList },
  { key: "health", label: "运行检查", icon: IconPulse },
  { key: "settings", label: "设置", icon: IconGear },
  { key: "about", label: "关于", icon: IconInfo },
];

export function Sidebar({ page, onPage, collapsed, onToggle, targetIde, onTargetIde, logUnread }: SidebarProps) {
  return (
    <aside
      className="sidebar-width flex-none bg-[#fbfdfd] border-r border-line-soft flex flex-col gap-0.5 py-4 px-3"
      style={{ width: collapsed ? 64 : 200 }}
    >
      <div className={`flex items-center ${collapsed ? "justify-center px-0" : "gap-2.5 px-2.5"} pb-4`}>
        <IconLogo className="w-[26px] h-[26px] flex-none" />
        {!collapsed && <span className="fade-in text-[14.5px] font-bold whitespace-nowrap">Trae 换号器</span>}
      </div>

      {NAV.map((item) => {
        const active = page === item.key;
        return (
          <button
            key={item.key}
            title={collapsed ? item.label : undefined}
            onClick={() => onPage(item.key)}
            className={`nav-item flex items-center ${collapsed ? "justify-center px-0" : "gap-2.5 px-3"} py-[9px] rounded-[10px] text-[13px] cursor-pointer ${
              active ? "bg-accent-soft text-accent font-semibold" : "text-ink2 hover:bg-[#f0f6f7]"
            }`}
          >
            <item.icon />
            {!collapsed && <span className="fade-in whitespace-nowrap">{item.label}</span>}
            {!collapsed && item.key === "logs" && logUnread > 0 && (
              <span key={logUnread} className="num-pop ml-auto bg-bad text-white text-[10.5px] rounded-full px-1.5">
                {logUnread}
              </span>
            )}
          </button>
        );
      })}

      <div className="h-px bg-line-soft mx-2 my-1.5" />

      <button
        title={collapsed ? "展开侧栏" : undefined}
        onClick={onToggle}
        className={`flex items-center ${collapsed ? "justify-center px-0" : "gap-2.5 px-3"} py-[9px] rounded-[10px] text-[13px] text-ink3 hover:bg-[#f0f6f7] hover:text-ink2 cursor-pointer transition-colors duration-150`}
      >
        {collapsed ? <IconChevronRight /> : <IconChevronLeft />}
        {!collapsed && <span className="fade-in">收起侧栏</span>}
      </button>

      <div className="mt-auto">
        {collapsed ? (
          <button
            title={`目标版本：${targetIde === "trae" ? "Trae" : "Trae CN"}（点击切换）`}
            onClick={() => onTargetIde(targetIde === "trae" ? "trae-cn" : "trae")}
            className="w-full py-2 rounded-[10px] bg-accent-soft text-accent text-[11.5px] font-bold cursor-pointer transition-colors duration-150 hover:bg-accent-line"
          >
            {targetIde === "trae" ? "TR" : "CN"}
          </button>
        ) : (
          <div className="mx-1.5 p-2.5 rounded-[12px] bg-white border border-line">
            <div className="text-[11px] text-ink3 mb-1.5">目标版本</div>
            <div className="seg flex w-full">
              {(
                [
                  { key: "trae", label: "Trae" },
                  { key: "trae-cn", label: "Trae CN" },
                ] as const
              ).map((o) => (
                <button
                  key={o.key}
                  onClick={() => onTargetIde(o.key)}
                  className={`seg-tab flex-1 px-2 ${targetIde === o.key ? "seg-tab-active" : ""}`}
                >
                  {o.label}
                </button>
              ))}
            </div>
          </div>
        )}
      </div>
    </aside>
  );
}
