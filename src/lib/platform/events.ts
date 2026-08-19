/**
 * 平台事件接口（计划书 §21/§22；阶段 6.5 命名对齐）。
 *
 * 统一事件名表 + `listen(event, handler, options?)`：
 * - 命名与签名对齐 Tauri `@tauri-apps/api/event` 的 `listen`，
 *   使上游业务组件的事件监听调用点（含卸载逻辑）保持零改动，
 *   仅 import 来源指向本平台层（计划书 §44 优先兼容上游事件）。
 * - 桌面端分支直接透传 Tauri `listen`（含 options.target）。
 * - Web 端分支走 WebSocket（单例连接 /api/v1/ws，首消息裸 token 鉴权，
 *   服务器广播 {type, account_id, payload} 桥接为 Tauri 同构事件；
 *   options.target 在 Web 端无对应语义，忽略）。
 *
 * 事件名与桌面端 Tauri event 名完全一致（统一事件名称表）。
 */
import { listen as tauriListen, type UnlistenFn } from "@tauri-apps/api/event";
import { getWebToken } from "./session";

/**
 * 事件名登记表（阶段 6.6 起定位调整为「登记/动态订阅用」）。
 *
 * 业务组件监听事件时**直接使用与桌面端一致的字符串事件名**（如
 * `listen("mail:error", cb)`），与上游 Tauri 写法一字不差，便于上游同步
 * 时调用点零改动。本表仅用于：集中可见 Web 端全部事件、单测断言事件名
 * 与后端广播 type、以及需要动态/通配订阅的少数场景。
 *
 * 事件名与桌面端 Tauri event 名、后端 ws 广播 type 完全一致。
 */
export const EVENTS = {
  syncProgress: "mail:sync-progress",
  syncComplete: "mail:sync-complete",
  mailError: "mail:error",
  mailNew: "mail:new",
  realtimeStatus: "mail:realtime-status",
  folderChanged: "mail:folder-changed",
  pendingOpsChanged: "mail:pending-ops-changed",
  unsnoozed: "mail:unsnoozed",
  notificationOpen: "mail:notification-open",
  downloadProgress: "attachment:download-progress",
  openMailto: "app:open-mailto",
} as const;
export type EventName = (typeof EVENTS)[keyof typeof EVENTS];

/** 与 @tauri-apps/api/event 的 Event<T> 结构同构。 */
export interface TauriEvent<T> {
  event: string;
  id: number;
  payload: T;
}

/** 与 Tauri `Options` 对齐；Web 端忽略 target（WS 全局单播无窗口语义）。 */
export interface EventOptions {
  target?: string;
}

/**
 * 把后端 WS 广播消息 {type, account_id, payload} 桥接为 Tauri 同构事件。
 * 顶层 account_id 并入 payload（桌面端事件 payload 内嵌 account_id，对齐结构）。
 * 纯函数，便于单测。
 */
export function buildEventFromWsMessage(msg: {
  type?: string;
  account_id?: string;
  payload?: unknown;
}): TauriEvent<unknown> | null {
  if (!msg.type) return null;

  let payload = msg.payload ?? {};
  if (
    msg.account_id != null &&
    (typeof payload !== "object" || payload === null || !("account_id" in payload))
  ) {
    const base: Record<string, unknown> =
      typeof payload === "object" && payload !== null ? { ...(payload as object) } : {};
    base.account_id = msg.account_id;
    payload = base;
  }

  return { event: msg.type, id: 0, payload };
}

// ---------------------------------------------------------------------------
// WebSocket 客户端（单例，Web 平台）
// ---------------------------------------------------------------------------

let ws: WebSocket | null = null;
let wsAuthenticated = false;
let wsSeq = 0;
let reconnectTimer: ReturnType<typeof setTimeout> | null = null;
const wsHandlers = new Map<string, Set<(event: TauriEvent<unknown>) => void>>();

/** 是否需要连接：有 token 且有订阅者。 */
function shouldConnect(): boolean {
  return !!getWebToken() && wsHandlers.size > 0;
}

function connectWebSocket(): void {
  if (ws && (ws.readyState === WebSocket.CONNECTING || ws.readyState === WebSocket.OPEN)) {
    return;
  }
  if (!shouldConnect()) return;

  const protocol = window.location.protocol === "https:" ? "wss:" : "ws:";
  const url = `${protocol}//${window.location.host}/api/v1/ws`;

  ws = new WebSocket(url);
  wsAuthenticated = false;

  ws.onopen = () => {
    ws?.send(getWebToken() ?? "");
  };

  ws.onmessage = (raw) => {
    let msg: { type?: string; account_id?: string; payload?: unknown };
    try {
      msg = JSON.parse(String(raw.data));
    } catch {
      return; // 非 JSON 帧忽略（含二进制/心跳）
    }

    if (!wsAuthenticated) {
      if (msg.type === "authenticated") {
        wsAuthenticated = true;
      } else if (msg.type === "error") {
        // 鉴权失败：由服务器关闭，不重连
        ws?.close();
      }
      return;
    }
    const event = buildEventFromWsMessage(msg);
    if (!event) return;
    event.id = ++wsSeq;
    const handlers = wsHandlers.get(event.event);
    handlers?.forEach((cb) => cb(event));
  };

  ws.onclose = (closeEvent) => {
    const wasAuthenticated = wsAuthenticated;
    ws = null;
    wsAuthenticated = false;
    // 鉴权失败（4001）或从未认证成功 → 不重连；其余按指数退避重连
    if (wasAuthenticated && closeEvent.code !== 4001 && shouldConnect()) {
      scheduleReconnect();
    }
  };

  ws.onerror = () => {
    ws?.close();
  };
}

function scheduleReconnect(): void {
  if (reconnectTimer) return;
  reconnectTimer = setTimeout(() => {
    reconnectTimer = null;
    connectWebSocket();
  }, 5000);
}

// ---------------------------------------------------------------------------
// listen
// ---------------------------------------------------------------------------

/**
 * 订阅事件。签名与 Tauri `listen` 对齐：
 * `listen<T>(event, handler, options?): Promise<UnlistenFn>`。
 *
 * - 桌面端：直接透传 Tauri listen（含 options）。
 * - Web 端：基于 WebSocket 全局事件，options.target 忽略（无窗口语义）。
 *
 * 返回 `Promise<UnlistenFn>`，业务组件卸载逻辑（`unlisten.then(fn => fn())`）
 * 与上游一致，无需改动。
 */
export function listen<T>(
  event: string,
  handler: (event: TauriEvent<T>) => void,
  options?: EventOptions,
): Promise<UnlistenFn> {
  if (!import.meta.env.VITE_PLATFORM) {
    return tauriListen<T>(event, handler, options);
  }

  // Web 平台：先注册再连接（确保首帧到来前订阅已就位）
  let handlers = wsHandlers.get(event);
  if (!handlers) {
    handlers = new Set();
    wsHandlers.set(event, handlers);
  }
  handlers.add(handler as (e: TauriEvent<unknown>) => void);
  connectWebSocket();

  return Promise.resolve(() => {
    handlers.delete(handler as (e: TauriEvent<unknown>) => void);
    if (handlers.size === 0) {
      wsHandlers.delete(event);
    }
    if (wsHandlers.size === 0) {
      ws?.close();
      ws = null;
    }
  });
}