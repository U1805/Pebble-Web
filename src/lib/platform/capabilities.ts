/**
 * 平台能力表（计划书 §23-24）。
 *
 * 差异集中在能力表中表达，业务组件按能力开关功能，不再散落平台判断。
 * Tauri 侧取桌面真实能力；Web 侧为 false 的项（tray/窗口控制/原生文件系统等）
 * 在阶段七接入组件判定。
 */
import { platform } from "./invoke";

const isWeb = platform === "web";

export const capabilities = {
  platform,
  tray: false,
  nativeNotifications: !isWeb,
  defaultMailClient: !isWeb,
  windowControls: !isWeb,
  nativeFileSystem: !isWeb,
  backgroundClose: !isWeb,
  deepLink: !isWeb,
  /** Web 端以浏览器下载/上传替代本地文件系统 */
  browserDownloads: isWeb,
  browserUploads: isWeb,
} as const;

export type Capabilities = typeof capabilities;
