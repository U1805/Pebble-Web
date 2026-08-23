/**
 * Web 事件兼容层。
 *
 * 统一事件名表 + `listen(event, handler, options?)`：
 * - 命名与签名对齐 Tauri `@tauri-apps/api/event` 的 `listen`，
 *   使上游业务组件的事件监听调用点（含卸载逻辑）保持零改动，
 *   仅 import 来源指向本平台层（计划书 §44 优先兼容上游事件）。
 * - Web 构建通过 Vite alias 将 `@tauri-apps/api/event` 指向本文件。
 * - 事件 transport 走 WebSocket（单例连接 /api/v1/ws，首消息裸 token 鉴权，
 *   服务器广播中的 `payload` 原样桥接为 Tauri 同构事件；
 *   envelope 的 Web transport 元数据不并入 payload。
 *   options.target 在 Web 端无对应语义，忽略）。
 *
 * 事件名与桌面端 Tauri event 名完全一致（统一事件名称表）。
 */
export type UnlistenFn = () => void;
import { expireWebSession, getWebToken } from "./session";
import {
  WEB_ATTACHMENT_DOWNLOAD_PROGRESS_EVENT,
  WEB_NOTIFICATION_OPEN_EVENT,
} from "./local-events";

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
 * 把后端 WS 广播消息桥接为 Tauri 同构事件。
 * `payload` 必须原样保留。WebSocket envelope 的 `account_id` 只是 transport 元数据。
 */
export function buildEventFromWsMessage(msg: {
  type?: string;
  account_id?: string;
  payload?: unknown;
}): TauriEvent<unknown> | null {
  if (!msg.type) return null;

  const payload: unknown = msg.payload === undefined ? null : msg.payload;
  return { event: msg.type, id: 0, payload };
}

// ---------------------------------------------------------------------------
// WebSocket 客户端（单例，Web 平台）
// ---------------------------------------------------------------------------

let ws: WebSocket | null = null;
let wsSeq = 0;
let reconnectTimer: ReturnType<typeof setTimeout> | null = null;
const wsHandlers = new Map<string, Set<(event: TauriEvent<unknown>) => void>>();
const WEB_LOCAL_EVENT_NAMES: Partial<Record<string, string>> = {
  [EVENTS.notificationOpen]: WEB_NOTIFICATION_OPEN_EVENT,
  [EVENTS.downloadProgress]: WEB_ATTACHMENT_DOWNLOAD_PROGRESS_EVENT,
};

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

  const socket = new WebSocket(url);
  ws = socket;
  let authenticated = false;
  let authRejected = false;

  socket.onopen = () => {
    socket.send(getWebToken() ?? "");
  };

  socket.onmessage = (raw) => {
    let msg: { type?: string; account_id?: string; payload?: unknown };
    try {
      msg = JSON.parse(String(raw.data));
    } catch {
      return; // 非 JSON 帧忽略（含二进制/心跳）
    }

    if (!authenticated) {
      if (msg.type === "authenticated") {
        authenticated = true;
      } else if (msg.type === "error") {
        // 认证失败是会话错误，不属于可重试的 WebSocket 传输故障。
        authRejected = true;
        expireWebSession();
        socket.close(4001, "unauthorized");
      }
      return;
    }
    const event = buildEventFromWsMessage(msg);
    if (!event) return;
    event.id = ++wsSeq;
    const handlers = wsHandlers.get(event.event);
    handlers?.forEach((cb) => cb(event));
  };

  socket.onclose = (closeEvent) => {
    const isCurrentSocket = ws === socket;
    if (isCurrentSocket) {
      ws = null;
    }
    if (authRejected || closeEvent.code === 4001) {
      expireWebSession();
      return;
    }
    // Ignore a stale socket's close after a newer connection already replaced it.
    if (isCurrentSocket && shouldConnect()) {
      scheduleReconnect();
    }
  };

  socket.onerror = () => {
    socket.close();
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
 * Web 端基于 WebSocket 全局事件，options.target 忽略（无窗口语义）。
 *
 * 返回 `Promise<UnlistenFn>`，业务组件卸载逻辑（`unlisten.then(fn => fn())`）
 * 与上游一致，无需改动。
 */
export function listen<T>(
  event: string,
  handler: (event: TauriEvent<T>) => void,
  _options?: EventOptions,
): Promise<UnlistenFn> {
  // Web 平台：先注册再连接（确保首帧到来前订阅已就位）
  let handlers = wsHandlers.get(event);
  if (!handlers) {
    handlers = new Set();
    wsHandlers.set(event, handlers);
  }
  handlers.add(handler as (e: TauriEvent<unknown>) => void);

  // Browser-only APIs that cannot publish through the WebSocket transport
  // still enter the same Tauri-shaped event contract at this boundary.
  const localEventName =
    typeof window !== "undefined" ? WEB_LOCAL_EVENT_NAMES[event] : undefined;
  const onLocalEvent = localEventName
    ? (raw: Event) => {
        const payload = (raw as CustomEvent<unknown>).detail ?? {};
        handler({ event, id: ++wsSeq, payload } as TauriEvent<T>);
      }
    : null;
  if (localEventName && onLocalEvent) {
    window.addEventListener(localEventName, onLocalEvent);
  }

  connectWebSocket();

  return Promise.resolve(() => {
    if (localEventName && onLocalEvent) {
      window.removeEventListener(localEventName, onLocalEvent);
    }
    handlers.delete(handler as (e: TauriEvent<unknown>) => void);
    if (handlers.size === 0) {
      wsHandlers.delete(event);
    }
    if (wsHandlers.size === 0) {
      const socket = ws;
      ws = null;
      socket?.close();
    }
  });
}
