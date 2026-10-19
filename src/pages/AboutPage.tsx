import { useState } from "react";

interface AboutPageProps {
  onCheckUpdate: () => Promise<void> | void;
}

const CREDITS = [
  { name: "Tauri", desc: "将界面打包为桌面程序的底层框架", license: "MIT / Apache-2.0" },
  { name: "React", desc: "界面渲染库", license: "MIT" },
  { name: "Vite", desc: "将界面代码打包为程序的构建工具", license: "MIT" },
  { name: "Tailwind CSS", desc: "界面样式（外观排版）工具", license: "MIT" },
];

export function AboutPage({ onCheckUpdate }: AboutPageProps) {
  // 检查更新耗时（网络往返/下载校验）可能数秒：给出明确进行中状态，避免点击后无任何反馈
  const [checking, setChecking] = useState(false);
  const run = async () => {
    if (checking) return;
    setChecking(true);
    try {
      await onCheckUpdate();
    } finally {
      setChecking(false);
    }
  };

  return (
    <div className="page-in flex-1 min-w-0 p-6 overflow-y-auto">
      <div className="flex items-center mb-[18px]">
        <h3 className="text-[17px] font-bold">关于</h3>
      </div>

      <div className="bg-card border border-line rounded-[12px] shadow-card p-6 mb-4">
        <div className="flex items-center gap-4">
          <div className="w-[56px] h-[56px] rounded-[16px] bg-gradient-to-br from-accent to-[#43d3c2] flex items-center justify-center text-white text-[22px] font-bold">
            T
          </div>
          <div>
            <div className="text-[16px] font-bold">TraeIDE-Switcher</div>
            <div className="text-[12px] text-ink3 mt-0.5">Trae / Trae CN 多账号切换器 · 开源软件</div>
          </div>
          <div className="ml-auto flex items-center gap-2.5">
            <div className="text-right">
              <div className="text-[11px] text-ink3">当前版本</div>
              <div className="font-mono text-[13.5px] font-semibold">v0.1.0</div>
            </div>
            <button className="btn-primary disabled:opacity-50" disabled={checking} onClick={() => void run()}>
              {checking ? "检查中…" : "检查更新"}
            </button>
          </div>
        </div>

        <div className="mt-5 pt-5 border-t border-line-soft grid grid-cols-2 gap-y-2.5 text-[12px]">
          <div className="flex gap-2">
            <span className="text-ink3 w-[72px] flex-none">开源仓库</span>
            <span className="font-mono text-ink2">github.com/WanJiu-LF/TraeIDE-Switcher</span>
          </div>
          <div className="flex gap-2">
            <span className="text-ink3 w-[72px] flex-none">更新来源</span>
            <span className="text-ink2">GitHub 官方发布页</span>
          </div>
          <div className="flex gap-2">
            <span className="text-ink3 w-[72px] flex-none">运行平台</span>
            <span className="text-ink2">Windows x64</span>
          </div>
          <div className="flex gap-2">
            <span className="text-ink3 w-[72px] flex-none">开源许可</span>
            <span className="text-ink2">MIT License</span>
          </div>
        </div>
      </div>

      <div className="bg-card border border-line rounded-[12px] shadow-card">
        <div className="px-[22px] py-3.5 border-b border-line-soft">
          <div className="text-[13px] font-semibold">开源鸣谢</div>
          <div className="text-[11.5px] text-ink3 mt-0.5">本项目基于以下开源项目构建，感谢其作者与贡献者</div>
        </div>
        {CREDITS.map((c) => (
          <div key={c.name} className="flex items-center px-[22px] py-3 border-b border-line-soft last:border-b-0">
            <div className="font-mono text-[12.5px] font-semibold w-[130px] flex-none">{c.name}</div>
            <div className="text-[12px] text-ink2 flex-1">{c.desc}</div>
            <div className="text-[11px] text-ink3">{c.license}</div>
          </div>
        ))}
      </div>
    </div>
  );
}
