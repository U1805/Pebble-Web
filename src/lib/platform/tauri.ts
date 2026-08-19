/**
 * Tauri 平台调用实现（计划书 §16）。
 *
 * 只封装 Tauri `invoke`，参数/命令名原样透传，不做任何转换，
 * 保证桌面端行为与迁移前完全一致。
 */
import { invoke as tauriInvoke } from "@tauri-apps/api/core";

import type { InvokeArgs } from "./invoke";

export async function invokeTauri<T>(command: string, args?: InvokeArgs): Promise<T> {
  // args 为 undefined 时保持与上游一致的 1 参数调用形态（invoke(cmd)）
  return args === undefined ? tauriInvoke<T>(command) : tauriInvoke<T>(command, args);
}