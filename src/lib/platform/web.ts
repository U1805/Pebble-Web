/**
 * Web 平台调用实现（计划书 §17）。
 *
 * 把命令调用转换为 `POST /api/v1/command/{command}`，请求参数与返回数据
 * 贴近 Tauri 命令（命令名/参数不变）。错误结构统一，401 抛错供上层
 * （阶段七登录页）接管。
 */
import { clearWebToken, getWebToken } from "./session";
import { isWebNoop, webNoopValue } from "./webNoop";
import type { InvokeArgs } from "./invoke";
import {
  WEB_ATTACHMENT_DOWNLOAD_PROGRESS_EVENT,
  WEB_NOTIFICATION_OPEN_EVENT,
} from "./localEvents";

const API_BASE = "/api/v1";
export const WEB_SESSION_EXPIRED_EVENT = "pebble:web-session-expired";
export { WEB_ATTACHMENT_DOWNLOAD_PROGRESS_EVENT } from "./localEvents";

export interface WebNotificationTarget {
  account_id?: string;
  message_id?: string;
}

export async function invokeWeb<T>(command: string, args?: InvokeArgs): Promise<T> {
  // 桌面专属命令：Web 端无真实行为 → 直接返回类型化降级值（不发起请求）
  if (isWebNoop(command)) {
    return webNoopValue(command, args) as T;
  }

  // OAuth needs a browser popup and a server-side callback. The command keeps
  // the desktop name, while this Web transport waits for the callback page to
  // post the newly-created account back to the opener.
  if (command === "complete_oauth_flow") {
    return completeOAuthFlowWeb(args) as T;
  }

  // 外部链接打开：Web 用浏览器新标签（桌面走系统默认浏览器命令）
  if (command === "open_external_url") {
    const { url } = (args ?? {}) as { url?: string };
    if (url) window.open(url, "_blank", "noopener,noreferrer");
    return undefined as T;
  }

  // 附件下载：Web 走 /api/v1/attachments/{id}/download（Bearer 在 header，文件名取自响应头）
  if (command === "download_attachment") {
    const { attachmentId } = (args ?? {}) as { attachmentId?: string };
    return downloadAttachmentWeb(attachmentId ?? "") as T;
  }

  // Browser uploads use multipart so binary data is not expanded into a JSON
  // number array (the command endpoint remains available for desktop parity).
  if (command === "stage_compose_attachment") {
    const { filename, bytes } = (args ?? {}) as { filename?: string; bytes?: number[] };
    return stageComposeAttachmentWeb(filename ?? "attachment", bytes ?? []) as T;
  }

  // 系统原生通知：Web 映射到浏览器 Notification API（能力等价，不发起请求）
  if (command === "get_notification_status") {
    const enabled = getWebNotificationsEnabled();
    return { enabled, attention_active: false, platform: "web", app_id: null } as T;
  }
  if (command === "set_notifications_enabled") {
    const { enabled } = (args ?? {}) as { enabled?: boolean };
    setWebNotificationsEnabled(!!enabled);
    if (enabled) void ensureWebNotificationPermission();
    return undefined as T;
  }
  if (command === "show_test_notification") {
    void showWebNotification("Pebble", "Test notification");
    return undefined as T;
  }
  if (command === "clear_notification_attention") {
    return undefined as T;
  }

  const token = getWebToken();
  const res = await fetch(`${API_BASE}/command/${encodeURIComponent(command)}`, {
    method: "POST",
    headers: {
      "Content-Type": "application/json",
      ...(token ? { Authorization: `Bearer ${token}` } : {}),
    },
    body: JSON.stringify(normalizeBackendArgs(command, args)),
  });

  if (!res.ok) {
    // 所有 Web command 都经过此处：401 统一清理会话并通知应用返回登录页，
    // 不依赖 React Query，因此普通 Store 调用和 mutation 行为一致。
    if (res.status === 401) {
      clearWebToken();
      window.dispatchEvent(new Event(WEB_SESSION_EXPIRED_EVENT));
    }
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

async function completeOAuthFlowWeb(args?: InvokeArgs): Promise<unknown> {
  // Open synchronously while still inside the user's click gesture; opening
  // only after the fetch resolves is blocked by browsers' popup policy.
  const popup = window.open("about:blank", "pebble-oauth", "popup,width=520,height=720");
  if (!popup) {
    throw new Error("The OAuth popup was blocked. Allow popups for this site and try again.");
  }
  const token = getWebToken();
  try {
    const res = await fetch(`${API_BASE}/command/complete_oauth_flow`, {
      method: "POST",
      headers: {
        "Content-Type": "application/json",
        ...(token ? { Authorization: `Bearer ${token}` } : {}),
      },
      body: JSON.stringify(normalizeBackendArgs("complete_oauth_flow", args)),
    });
    if (!res.ok) {
      throw await webHttpError(res);
    }
    const body = (await res.json()) as { authorization_url?: string };
    if (!body.authorization_url) {
      throw new Error("OAuth server did not return an authorization URL");
    }
    popup.location.href = body.authorization_url;
  } catch (error) {
    popup.close();
    throw error;
  }
  return waitForOAuthPopup(popup);
}

async function waitForOAuthPopup(popup: Window): Promise<unknown> {
  return new Promise((resolve, reject) => {
    let settled = false;
    const cleanup = () => {
      window.removeEventListener("message", onMessage);
      window.clearInterval(closePoll);
    };
    const finish = (callback: () => void) => {
      if (settled) return;
      settled = true;
      cleanup();
      callback();
    };
    const onMessage = (event: MessageEvent) => {
      if (event.source !== popup || event.origin !== window.location.origin) return;
      const data = event.data as { type?: string; status?: string; account?: unknown; message?: string };
      if (data?.type !== "pebble-oauth") return;
      if (data.status === "success" && data.account) {
        finish(() => resolve(data.account));
      } else {
        finish(() => reject(new Error(data.message || "OAuth authorization failed")));
      }
    };
    const closePoll = window.setInterval(() => {
      if (popup.closed) {
        finish(() => reject(new Error("OAuth authorization was cancelled")));
      }
    }, 500);
    window.addEventListener("message", onMessage);
  });
}

async function webHttpError(res: Response): Promise<Error & { status?: number; code?: string }> {
  if (res.status === 401) {
    clearWebToken();
    window.dispatchEvent(new Event(WEB_SESSION_EXPIRED_EVENT));
  }
  let message = `HTTP ${res.status}`;
  let code: string | undefined;
  try {
    const body = (await res.json()) as { error?: { code?: string; message?: string } };
    message = body.error?.message ?? message;
    code = body.error?.code;
  } catch {
    // Keep the HTTP fallback for non-JSON error responses.
  }
  const error = new Error(message) as Error & { status?: number; code?: string };
  error.status = res.status;
  if (code) error.code = code;
  return error;
}
/**
 * Web 附件下载：GET /api/v1/attachments/{id}/download（Bearer 放 header）。
 * fetch blob → a[download] 触发浏览器保存；文件名取自响应 Content-Disposition。
 * 返回文件名（模拟桌面 download_attachment 的保存路径语义）。
 */
async function downloadAttachmentWeb(attachmentId: string): Promise<string> {
  const token = getWebToken();
  const res = await fetch(
    `${API_BASE}/attachments/${encodeURIComponent(attachmentId)}/download`,
    { headers: token ? { Authorization: `Bearer ${token}` } : {} },
  );
  if (!res.ok) {
    throw await webHttpError(res);
  }

  const filename =
    parseFilenameFromDisposition(res.headers.get("Content-Disposition") ?? "") ?? "attachment";
  const blob = await readAttachmentDownloadBlob(res, attachmentId);
  const url = URL.createObjectURL(blob);
  try {
    const a = document.createElement("a");
    a.href = url;
    a.download = filename;
    document.body.appendChild(a);
    a.click();
    a.remove();
  } finally {
    // 延迟回收 object URL，避免浏览器尚未真正开始保存时即被注销
    setTimeout(() => URL.revokeObjectURL(url), 1000);
  }
  return filename;
}

async function readAttachmentDownloadBlob(res: Response, attachmentId: string): Promise<Blob> {
  const totalBytes = Number(res.headers.get("Content-Length") ?? 0) || 0;
  if (!res.body) return res.blob();

  const reader = res.body.getReader();
  const chunks: ArrayBuffer[] = [];
  let bytesCopied = 0;
  while (true) {
    const { done, value } = await reader.read();
    if (done) break;
    if (!value) continue;
    // Copy the view so Blob receives a regular ArrayBuffer even when a
    // browser implementation exposes an ArrayBufferLike-backed chunk.
    const chunk = new Uint8Array(value.byteLength);
    chunk.set(value);
    chunks.push(chunk.buffer);
    bytesCopied += value.byteLength;
    window.dispatchEvent(
      new CustomEvent(WEB_ATTACHMENT_DOWNLOAD_PROGRESS_EVENT, {
        detail: {
          attachment_id: attachmentId,
          bytes_copied: bytesCopied,
          total_bytes: totalBytes,
        },
      }),
    );
  }
  window.dispatchEvent(
    new CustomEvent(WEB_ATTACHMENT_DOWNLOAD_PROGRESS_EVENT, {
      detail: {
        attachment_id: attachmentId,
        bytes_copied: bytesCopied,
        total_bytes: totalBytes,
      },
    }),
  );
  return new Blob(chunks, { type: res.headers.get("Content-Type") ?? undefined });
}

async function stageComposeAttachmentWeb(filename: string, bytes: number[]): Promise<string> {
  const form = new FormData();
  form.append("file", new Blob([Uint8Array.from(bytes)]), filename);
  const token = getWebToken();
  const res = await fetch(`${API_BASE}/attachments/stage`, {
    method: "POST",
    headers: token ? { Authorization: `Bearer ${token}` } : {},
    body: form,
  });
  if (!res.ok) throw await webHttpError(res);
  const stagedPath = await res.json();
  if (typeof stagedPath !== "string") {
    throw new Error("Attachment upload returned an invalid staged path");
  }
  return stagedPath;
}

function parseFilenameFromDisposition(disposition: string): string | null {
  const m = /filename="?([^";]+)"?/i.exec(disposition);
  return m ? m[1] : null;
}

// ─── 前端(Tauri)形态 → 后端 snake_case 契约转换 ─────────────────────────────
// 上游桌面端里，JS 侧 camelCase 键由 Tauri 框架在 IPC 边界自动映射到 snake_case
// 命令参数（命令签名以 snake_case 为准）。Web 端无此框架层，故在此复刻等价的
// 递归 camel→snake 键名映射（对已是 snake_case 的键幂等）。命令参数形态与上游
// Tauri 命令签名一一对应（含 request/rule/input/config/query 等命名对象参数），
// 后端不做额外转换。

function camelToSnake(key: string): string {
  return key.replace(/[A-Z]/g, (c) => `_${c.toLowerCase()}`);
}

function deepSnakeKeys(value: unknown): unknown {
  if (Array.isArray(value)) return value.map(deepSnakeKeys);
  if (typeof value === "object" && value !== null) {
    const out: Record<string, unknown> = {};
    for (const [k, v] of Object.entries(value)) out[camelToSnake(k)] = deepSnakeKeys(v);
    return out;
  }
  return value;
}

function normalizeBackendArgs(command: string, args?: InvokeArgs): unknown {
  void command;
  if (!args || typeof args !== "object" || Array.isArray(args)) return args ?? {};
  return deepSnakeKeys(args);
}
// 导出供单测直接验证（打包 tree-shake 会剔除，不影响产物体积）
export { normalizeBackendArgs };

// ─── Web 系统原生通知（浏览器 Notification API） ─────────────────────────────
// 开关与 ui.store 共用 profile-scoped localStorage key；Web 启动时通过
// get_profile_storage_namespace 固定为 "web"，与桌面端保持相同存储模型。
const NOTIFICATIONS_ENABLED_KEY = "pebble-notifications-enabled";

export function getWebNotificationsEnabled(): boolean {
  if (typeof localStorage === "undefined") return false;
  return localStorage.getItem(NOTIFICATIONS_ENABLED_KEY) === "true";
}

export function setWebNotificationsEnabled(enabled: boolean): void {
  try {
    localStorage.setItem(NOTIFICATIONS_ENABLED_KEY, String(enabled));
  } catch {
    /* 隐私模式等场景写失败可忽略 */
  }
}

/** 确保通知权限（需用户手势触发；浏览器对 requestPermission 有调用时机约束）。 */
async function ensureWebNotificationPermission(): Promise<boolean> {
  if (typeof Notification === "undefined") return false;
  if (Notification.permission === "granted") return true;
  if (Notification.permission === "denied") return false;
  try {
    return (await Notification.requestPermission()) === "granted";
  } catch {
    return false;
  }
}

/** 弹出浏览器系统原生通知（权限未授予或环境不支持时静默跳过）。 */
export async function showWebNotification(
  title: string,
  body: string,
  target?: WebNotificationTarget,
): Promise<void> {
  if (typeof Notification === "undefined" || Notification.permission !== "granted") return;
  try {
    const notification = new Notification(title, { body });
    if (target?.message_id) {
      notification.onclick = () => {
        window.dispatchEvent(
          new CustomEvent(WEB_NOTIFICATION_OPEN_EVENT, {
            detail: target,
          }),
        );
        notification.close();
      };
    }
  } catch {
    /* 某些平台/隐身模式抛错，静默 */
  }
}

/** 供通知 hook 调用的统一路径：先静默确保权限（若尚未决定）。 */
export async function requestWebNotificationPermission(): Promise<boolean> {
  return ensureWebNotificationPermission();
}
