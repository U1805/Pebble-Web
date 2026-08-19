/**
 * Web 平台调用实现（计划书 §17）。
 *
 * 把命令调用转换为 `POST /api/v1/command/{command}`，请求参数与返回数据
 * 贴近 Tauri 命令（命令名/参数不变）。错误结构统一，401 抛错供上层
 * （阶段七登录页）接管。
 */
import { getWebToken } from "./session";
import type { InvokeArgs } from "./invoke";

const API_BASE = "/api/v1";

export async function invokeWeb<T>(command: string, args?: InvokeArgs): Promise<T> {
  const token = getWebToken();
  const res = await fetch(`${API_BASE}/command/${encodeURIComponent(command)}`, {
    method: "POST",
    headers: {
      "Content-Type": "application/json",
      ...(token ? { Authorization: `Bearer ${token}` } : {}),
    },
    body: JSON.stringify(args ?? {}),
  });

  if (!res.ok) {
    let message = `HTTP ${res.status}`;
    let code: string | undefined;
    try {
      const body = (await res.json()) as {
        error?: { code?: string; message?: string };
      };
      message = body.error?.message ?? message;
      code = body.error?.code;
    } catch {
      // 响应体非 JSON 时保留默认错误信息
    }
    const err = new Error(message) as Error & { status?: number; code?: string };
    err.status = res.status;
    if (code) err.code = code;
    throw err;
  }

  return (await res.json()) as T;
}