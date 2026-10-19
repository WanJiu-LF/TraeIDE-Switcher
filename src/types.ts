export type Ide = "trae" | "trae-cn";

export type AccountStatus = "normal" | "exhausted" | "error";

export type Plan = "pro" | "free";

export interface Quota {
  used: number;
  total: number;
}

export interface Account {
  id: string;
  name: string;
  email: string;
  initial: string;
  color: string;
  plan: Plan;
  ide: Ide;
  status: AccountStatus;
  /** 最近一次额度查询失败的原因（仅展示用，串起"卡片为何标红"） */
  errorText?: string;
  /** 置顶显示 */
  pinned?: boolean;
  /** 该账号独立耗尽阈值（剩余百分比），undefined = 用全局阈值 */
  threshold?: number | null;
  daily: Quota;
  weekly: Quota;
  resetText: string;
  note: string;
  credits?: {
    total: number;
    sources: { desc: string; remaining: number; used: number; limit: number; unlimited: boolean; expire: string }[];
  };
  checkin?: { checked: boolean; reward: number };
}

export type Page = "accounts" | "assets" | "logs" | "health" | "settings" | "about";

export interface ToastItem {
  id: string;
  kind: "success" | "error" | "info";
  title: string;
  desc?: string;
}
