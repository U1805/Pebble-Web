import { afterAll, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";

import { clearWebToken, setWebToken } from "@/lib/platform/session";
import {
  WEB_ATTACHMENT_DOWNLOAD_PROGRESS_EVENT,
  WEB_NOTIFICATION_OPEN_EVENT,
} from "@/lib/platform/localEvents";

vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn() }));

class FakeWebSocket {
  static CONNECTING = 0;
  static OPEN = 1;
  static instances: FakeWebSocket[] = [];
  readonly url: string;
  readonly sent: string[] = [];
  readyState = 0;
  onopen: (() => void) | null = null;
  onmessage: ((event: { data: string }) => void) | null = null;
  onclose: ((event: { code: number }) => void) | null = null;
  onerror: (() => void) | null = null;

  constructor(url: string) {
    this.url = url;
    FakeWebSocket.instances.push(this);
  }

  send(data: string) {
    this.sent.push(data);
  }

  close() {
    this.readyState = 3;
    this.onclose?.({ code: 1000 });
  }

  open() {
    this.readyState = 1;
    this.onopen?.();
  }

  receive(message: unknown) {
    this.onmessage?.({ data: JSON.stringify(message) });
  }

  serverClose(code = 1000) {
    this.readyState = 3;
    this.onclose?.({ code });
  }
}

let webEvents: typeof import("@/lib/platform/events");

beforeAll(async () => {
  vi.stubEnv("VITE_PLATFORM", "web");
  vi.resetModules();
  webEvents = await import("@/lib/platform/events");
});

afterAll(() => {
  clearWebToken();
  vi.unstubAllEnvs();
});

beforeEach(() => {
  clearWebToken();
  FakeWebSocket.instances.length = 0;
  vi.stubGlobal("WebSocket", FakeWebSocket);
  vi.useRealTimers();
});

describe("WebSocket event transport", () => {
  it("bridges browser notification clicks into the shared event contract", async () => {
    const handler = vi.fn();
    const stop = await webEvents.listen("mail:notification-open", handler);

    window.dispatchEvent(
      new CustomEvent(WEB_NOTIFICATION_OPEN_EVENT, {
        detail: { account_id: "account-1", message_id: "message-1" },
      }),
    );

    expect(handler).toHaveBeenCalledWith({
      event: "mail:notification-open",
      id: expect.any(Number),
      payload: { account_id: "account-1", message_id: "message-1" },
    });

    stop();
    window.dispatchEvent(
      new CustomEvent(WEB_NOTIFICATION_OPEN_EVENT, {
        detail: { message_id: "message-2" },
      }),
    );
    expect(handler).toHaveBeenCalledOnce();
  });

  it("bridges browser attachment progress into the shared event contract", async () => {
    const handler = vi.fn();
    const stop = await webEvents.listen("attachment:download-progress", handler);

    window.dispatchEvent(
      new CustomEvent(WEB_ATTACHMENT_DOWNLOAD_PROGRESS_EVENT, {
        detail: { attachment_id: "attachment-1", bytes_copied: 4, total_bytes: 8 },
      }),
    );

    expect(handler).toHaveBeenCalledWith({
      event: "attachment:download-progress",
      id: expect.any(Number),
      payload: { attachment_id: "attachment-1", bytes_copied: 4, total_bytes: 8 },
    });

    stop();
  });

  it("authenticates with the web token and bridges broadcast events", async () => {
    setWebToken("web-token");
    const handler = vi.fn();
    const unlisten = await webEvents.listen("mail:new", handler);
    const statusHandler = vi.fn();
    const stopStatus = await webEvents.listen("mail:realtime-status", statusHandler);
    const socket = FakeWebSocket.instances[0];

    expect(socket.url).toBe("ws://localhost:3000/api/v1/ws");
    socket.open();
    expect(socket.sent).toEqual(["web-token"]);

    socket.receive({ type: "authenticated" });
    socket.receive({
      type: "mail:new",
      account_id: "account-1",
      payload: {
        message_id: "message-1",
        thread_id: "thread-1",
        subject: "A new message",
        from: "sender@example.com",
        received_at: 1_700_000_000,
      },
    });

    expect(handler).toHaveBeenCalledWith({
      event: "mail:new",
      id: expect.any(Number),
      payload: {
        message_id: "message-1",
        thread_id: "thread-1",
        subject: "A new message",
        from: "sender@example.com",
        received_at: 1_700_000_000,
        account_id: "account-1",
      },
    });

    socket.receive({
      type: "mail:realtime-status",
      account_id: "account-1",
      payload: {
        account_id: "account-1",
        provider: "imap",
        mode: "polling",
        message: "Polling every 15s",
      },
    });
    expect(statusHandler).toHaveBeenCalledWith({
      event: "mail:realtime-status",
      id: expect.any(Number),
      payload: {
        account_id: "account-1",
        provider: "imap",
        mode: "polling",
        message: "Polling every 15s",
      },
    });

    const stop = await unlisten;
    stop();
    stopStatus();
    expect(socket.readyState).toBe(3);
  });

  it("reconnects after an authenticated socket closes while subscribed", async () => {
    vi.useFakeTimers();
    setWebToken("web-token");
    const handler = vi.fn();
    const stop = await webEvents.listen("mail:sync-complete", handler);
    const first = FakeWebSocket.instances[0];
    first.open();
    first.receive({ type: "authenticated" });
    first.serverClose(1000);

    await vi.advanceTimersByTimeAsync(5000);
    expect(FakeWebSocket.instances).toHaveLength(2);
    const second = FakeWebSocket.instances[1];
    second.open();
    expect(second.sent).toEqual(["web-token"]);
    second.receive({ type: "authenticated" });
    second.receive({ type: "mail:sync-complete", payload: { status: "completed" } });
    expect(handler).toHaveBeenCalledOnce();

    (await stop)();
  });
});
