/**
 * 跨平台应用版本（计划书 §14 平台层的一部分）。
 *
 * 版本获取从 platform/web.ts 与 tauri.ts 收敛至此：聚合 `getVersion`
 * 按平台分派（Web 读后端 health；桌面走 Tauri app 元信息），两实现各归其位，
 * 业务代码统一从 `@/lib/platform` import `getVersion`。
 */
import { getVersion as tauriGetVersion } from "@tauri-apps/api/app";

import { platform } from "./invoke";

/** 桌面端应用版本：Tauri app 元信息。 */
function getTauriVersion(): Promise<string> {
  return tauriGetVersion();
}

let cachedWebVersion: string | null = null;

/** Web 端应用版本：读取 /api/v1/health（无需鉴权），模块级缓存。 */
async function getWebVersion(): Promise<string> {
  if (cachedWebVersion !== null) return cachedWebVersion;
  const res = await fetch("/api/v1/health");
  if (!res.ok) {
    throw new Error(`HTTP ${res.status}`);
  }
  const body = (await res.json()) as { version?: string };
  cachedWebVersion = body.version ?? "";
  return cachedWebVersion;
}

/** 跨平台应用版本：Web 读后端 health；桌面走 Tauri app 元信息。 */
export async function getVersion(): Promise<string> {
  return platform === "web" ? getWebVersion() : getTauriVersion();
}
