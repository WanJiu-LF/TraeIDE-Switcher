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
  /** 凭据 access token 到期时刻（epoch 毫秒）；无法解析时缺省 */
  expiresAt?: number | null;
  /** 凭据已过期（需重新登录采集） */
  credExpired?: boolean;
  /** 上次续期失败原因（如 20403 Token device not match） */
  refreshError?: string | null;
  /** 上次续期失败为设备不匹配 */
  deviceMismatch?: boolean;
  /** 已创建过独立实例数据目录（多开过） */
  instanceExists?: boolean;
  /** 独立实例当前在运行（切换助手通道在线） */
  instanceAlive?: boolean;
  daily: Quota;
  weekly: Quota;
  resetText: string;
  /** 国际版：无赠送额度（隐藏赠送额度行） */
  noBonus?: boolean;
  /** 国际版：额度以美元计 */
  dollar?: boolean;
  /** 国际版：结算周期结束时刻（秒） */
  resetAt?: number;
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
