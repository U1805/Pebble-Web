/**
 * 统一后端调用层（计划书 §15）。
 *
 * 业务代码只通过 `invoke(command, args)` 与后端交互，不感知具体平台。
 * 平台判定：构建时注入 `VITE_PLATFORM=web` 走 Web 调用层（HTTP），
 * 缺省视为 Tauri（桌面端，保证桌面行为不变）。
 */
import { invokeTauri } from "./tauri";
import { invokeWeb } from "./web";

export type Platform = "tauri" | "web";

export const platform: Platform =
  (import.meta.env.VITE_PLATFORM as string | undefined) === "web" ? "web" : "tauri";

export interface InvokeArgs {
  [key: string]: unknown;
}

/**
 * 命令调用统一入口。命令名/参数与桌面端 Tauri command 保持一致。
 * 返回类型由调用方以泛型声明。
 */
export async function invoke<T>(command: string, args?: InvokeArgs): Promise<T> {
  if (platform === "web") {
    return invokeWeb<T>(command, args);
  }
  return invokeTauri<T>(command, args);
}