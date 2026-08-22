/**
 * Web 平台 `invoke` 降级表（阶段七，方案优化）。
 *
 * 把「Web 后端永远没有对应实现」的桌面命令收敛到 invoke 层，返回类型化
 * 降级值——业务组件/API 层无需再加平台判断（与 invoke 统一入口的思路一致）。
 *
 * 收录原则：
 * - 仅收录已确认 Web 端**永远无真实行为**的桌面命令（系统托盘/自动登录/
 *   原生通知/deep-link 挂起队列/桌面窗口与系统默认应用设置）；
 * - 其余命令（账户/邮件/搜索/规则/联系人/同步等）Web 后端已有实现，
 *   绝不放入本表——否则降级会掩盖可用的后端能力；
 * - 返回值的 TS 类型由 invokeWeb<T> 在调用方断言，这里以运行时值为主。
 */
import type { InvokeArgs } from "./invoke";

type NoopFactory = (args?: InvokeArgs) => unknown;

export const WEB_NOOP_COMMANDS: Record<string, NoopFactory> = {
  // 开机自启（OS：Windows 注册表 / LaunchAgent / .desktop）
  get_autostart_enabled: () => false,
  set_autostart_enabled: () => undefined,
  // 系统托盘
  set_tray_menu_labels: () => undefined,
  // deep-link 挂起的 mailto（Web 无系统级 mailto 转发）
  take_pending_mailto_urls: () => [],
  // Windows 系统默认邮件应用设置（Web 无权修改操作系统默认应用）
  open_default_mail_settings: () => undefined,
  // 原生窗口标题栏主题（Web 的主题已直接同步到 DOM）
  sync_titlebar_theme: () => undefined,
};

/** 命令是否命中降级表。 */
export function isWebNoop(command: string): boolean {
  return Object.prototype.hasOwnProperty.call(WEB_NOOP_COMMANDS, command);
}

/** 返回降级值。 */
export function webNoopValue(command: string, args?: InvokeArgs): unknown {
  return WEB_NOOP_COMMANDS[command]?.(args);
}
