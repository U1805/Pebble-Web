import { describe, it, expect, vi } from "vitest";

import {
  listen,
  EVENTS,
  buildEventFromWsMessage,
} from "@/lib/platform/events";

/**
 * 事件接口测试（阶段六）：
 * - 事件名表与后端/桌面端事件名一致
 * - payload 桥接纯函数（后端 {type,account_id,payload} → Tauri 同构事件）
 * - tauri 分支透传 @tauri-apps/api/event listen（事件名 + 回调 + 返回契约）
 *
 * Web 分支的 WS 连接/重连生命周期由运行时集成验证（真实服务 + 协议握手），
 * 单测聚焦纯逻辑，避免依赖浏览器全局与 import.meta.env 注入。
 */

const listenMock = vi.hoisted(() => vi.fn());
vi.mock("@tauri-apps/api/event", () => ({ listen: listenMock }));

beforeEach(() => {
  listenMock.mockReset();
  listenMock.mockReturnValue(Promise.resolve(() => {}));
});

describe("platform/events — 事件名表", () => {
  it("统一事件名与桌面端/后端一致", () => {
    expect(EVENTS.syncProgress).toBe("mail:sync-progress");
    expect(EVENTS.syncComplete).toBe("mail:sync-complete");
    expect(EVENTS.mailError).toBe("mail:error");
    expect(EVENTS.mailNew).toBe("mail:new");
    expect(EVENTS.realtimeStatus).toBe("mail:realtime-status");
    expect(EVENTS.folderChanged).toBe("mail:folder-changed");
    expect(EVENTS.pendingOpsChanged).toBe("mail:pending-ops-changed");
    expect(EVENTS.unsnoozed).toBe("mail:unsnoozed");
    expect(EVENTS.notificationOpen).toBe("mail:notification-open");
    expect(EVENTS.downloadProgress).toBe("attachment:download-progress");
    expect(EVENTS.openMailto).toBe("app:open-mailto");
  });
});

describe("platform/events — payload 桥接纯函数", () => {
  it("顶层 account_id 并入 payload，与桌面端结构对齐", () => {
    const ev = buildEventFromWsMessage({
      type: "mail:sync-complete",
      account_id: "acc-1",
      payload: { status: "completed" },
    });
    expect(ev).not.toBeNull();
    expect(ev!.event).toBe("mail:sync-complete");
    expect(ev!.payload).toEqual({ status: "completed", account_id: "acc-1" });
  });

  it("payload 已含 account_id 时不覆盖", () => {
    const ev = buildEventFromWsMessage({
      type: "mail:sync-progress",
      account_id: "acc-2",
      payload: { status: "started", account_id: "acc-2" },
    });
    expect(ev!.payload).toEqual({ status: "started", account_id: "acc-2" });
  });

  it("缺 type 返回 null（忽略非事件帧）", () => {
    expect(buildEventFromWsMessage({ payload: {} })).toBeNull();
    expect(buildEventFromWsMessage({})).toBeNull();
  });

  it("payload 为空对象时仅补 account_id", () => {
    const ev = buildEventFromWsMessage({ type: "mail:new", account_id: "acc-3" });
    expect(ev!.payload).toEqual({ account_id: "acc-3" });
  });
});

describe("platform/events — tauri 分支", () => {
  it("透传事件名并保持 Promise<UnlistenFn> 契约", async () => {
    const cb = vi.fn();
    const unlisten = await listen(EVENTS.syncComplete, cb);
    expect(listenMock).toHaveBeenCalledWith(EVENTS.syncComplete, expect.any(Function), undefined);
    expect(typeof unlisten).toBe("function");
    expect(() => unlisten()).not.toThrow();
  });
  it("透传 options（对齐 Tauri 签名）", async () => {
    const cb = vi.fn();
    await listen(EVENTS.syncComplete, cb, { target: "main" });
    expect(listenMock).toHaveBeenCalledWith(EVENTS.syncComplete, expect.any(Function), {
      target: "main",
    });
  });
});
