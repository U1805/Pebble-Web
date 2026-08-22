/**
 * 平台能力表（计划书 §23-24）。
 *
 * 差异集中在能力表中表达，业务组件按能力开关功能，不再散落平台判断。
 * 仅收录两平台**真实存在差异**的能力；两平台均有等价能力（通知、WebDAV/备份）
 * 不做差异，由各自平台实现承接，不占用能力表。
 */
import { platform } from "./invoke";

const isWeb = platform === "web";

export const capabilities = {
  platform,
  /** Web 无自定义窗口栏（浏览器标签页即窗口控制） */
  windowControls: !isWeb,
  /** 注册为系统默认邮件客户端 */
  defaultMailClient: !isWeb,
  /** 关闭窗口时后台驻留（托盘） */
  backgroundClose: !isWeb,
  /** 桌面系统级设置组（启动行为/关闭行为/桌面实时偏好等），Web 无对应概念 */
  desktopSettings: !isWeb,
  /** Web 端以浏览器下载替代本地文件保存 */
  browserDownloads: isWeb,
} as const;

export type Capabilities = typeof capabilities;
