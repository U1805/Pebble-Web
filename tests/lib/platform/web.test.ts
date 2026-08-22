import { afterEach, describe, expect, it, vi } from "vitest";

import {
  invokeWeb,
  normalizeBackendArgs,
  showWebNotification,
  WEB_ATTACHMENT_DOWNLOAD_PROGRESS_EVENT,
  WEB_SESSION_EXPIRED_EVENT,
} from "@/lib/platform/web";
import { WEB_NOTIFICATION_OPEN_EVENT } from "@/lib/platform/localEvents";
import { clearWebToken, getWebToken, setWebToken } from "@/lib/platform/session";

function jsonResponse(body: unknown, ok: boolean, status = 200) {
  return { ok, status, json: async () => body };
}

describe("platform/session", () => {
  afterEach(() => clearWebToken());

  it("round-trips the web token via localStorage", () => {
    expect(getWebToken()).toBeNull();
    setWebToken("tok-abc");
    expect(getWebToken()).toBe("tok-abc");
    clearWebToken();
    expect(getWebToken()).toBeNull();
  });
});

describe("invokeWeb (Web 调用实现)", () => {
  afterEach(() => {
    vi.unstubAllGlobals();
    clearWebToken();
    localStorage.removeItem("pebble-notifications-enabled");
  });

  it("posts to /api/v1/command/{command} and returns parsed result", async () => {
    const fetchMock = vi.fn().mockResolvedValue(jsonResponse({ id: "m1" }, true));
    vi.stubGlobal("fetch", fetchMock);

    await invokeWeb<{ id: string }>("get_message", { messageId: "m1" });

    expect(fetchMock).toHaveBeenCalledWith(
      "/api/v1/command/get_message",
      expect.objectContaining({
        method: "POST",
        headers: expect.objectContaining({ "Content-Type": "application/json" }),
        body: JSON.stringify({ message_id: "m1" }),
      }),
    );
  });

  it("omits Authorization header when no token is stored", async () => {
    const fetchMock = vi.fn().mockResolvedValue(jsonResponse({}, true));
    vi.stubGlobal("fetch", fetchMock);

    await invokeWeb("list_accounts");

    const [, init] = fetchMock.mock.calls[0] as [string, RequestInit];
    expect((init.headers as Record<string, string>).Authorization).toBeUndefined();
  });

  it("attaches Bearer token when present", async () => {
    setWebToken("tok-123");
    const fetchMock = vi.fn().mockResolvedValue(jsonResponse({}, true));
    vi.stubGlobal("fetch", fetchMock);

    await invokeWeb("list_accounts");

    const [, init] = fetchMock.mock.calls[0] as [string, RequestInit];
    expect((init.headers as Record<string, string>).Authorization).toBe("Bearer tok-123");
  });

  it("uploads compose attachments as multipart instead of JSON", async () => {
    setWebToken("tok-upload");
    const fetchMock = vi.fn().mockResolvedValue(jsonResponse("/data/compose_staging/file.txt", true));
    vi.stubGlobal("fetch", fetchMock);

    await expect(
      invokeWeb<string>("stage_compose_attachment", {
        filename: "report.txt",
        bytes: [1, 2, 3],
      }),
    ).resolves.toBe("/data/compose_staging/file.txt");

    const [url, init] = fetchMock.mock.calls[0] as [string, RequestInit];
    expect(url).toBe("/api/v1/attachments/stage");
    expect((init.headers as Record<string, string>).Authorization).toBe("Bearer tok-upload");
    expect((init.headers as Record<string, string>)['Content-Type']).toBeUndefined();
    expect(init.body).toBeInstanceOf(FormData);
    const uploaded = (init.body as FormData).get("file") as File;
    expect(uploaded.name).toBe("report.txt");
    expect(uploaded.size).toBe(3);
  });

  it("reports browser attachment download progress while reading the response stream", async () => {
    const chunks = [new Uint8Array([1, 2]), new Uint8Array([3, 4])];
    const reader = {
      read: vi.fn()
        .mockResolvedValueOnce({ done: false, value: chunks[0] })
        .mockResolvedValueOnce({ done: false, value: chunks[1] })
        .mockResolvedValueOnce({ done: true, value: undefined }),
    };
    const response = {
      ok: true,
      status: 200,
      headers: new Headers({
        "Content-Disposition": 'attachment; filename="report.txt"',
        "Content-Length": "4",
        "Content-Type": "text/plain",
      }),
      body: { getReader: () => reader },
      blob: vi.fn(),
    } as unknown as Response;
    vi.stubGlobal("fetch", vi.fn().mockResolvedValue(response));
    vi.stubGlobal("URL", {
      ...URL,
      createObjectURL: vi.fn().mockReturnValue("blob:report"),
      revokeObjectURL: vi.fn(),
    });
    const anchor = document.createElement("a");
    const click = vi.spyOn(anchor, "click").mockImplementation(() => {});
    vi.spyOn(document, "createElement").mockReturnValue(anchor);
    const progress = vi.fn();
    window.addEventListener(WEB_ATTACHMENT_DOWNLOAD_PROGRESS_EVENT, progress);

    await invokeWeb<string>("download_attachment", { attachmentId: "att-1" });

    expect(progress).toHaveBeenCalledTimes(3);
    expect((progress.mock.calls[0][0] as CustomEvent).detail).toEqual({
      attachment_id: "att-1",
      bytes_copied: 2,
      total_bytes: 4,
    });
    expect((progress.mock.calls[2][0] as CustomEvent).detail.bytes_copied).toBe(4);
    expect(click).toHaveBeenCalledOnce();
    window.removeEventListener(WEB_ATTACHMENT_DOWNLOAD_PROGRESS_EVENT, progress);
  });

  it("uses the shared session-expired path for attachment download 401s", async () => {
    setWebToken("expired-attachment-token");
    const expired = vi.fn();
    window.addEventListener(WEB_SESSION_EXPIRED_EVENT, expired, { once: true });
    vi.stubGlobal(
      "fetch",
      vi.fn().mockResolvedValue(jsonResponse({ error: { message: "expired" } }, false, 401)),
    );

    await expect(invokeWeb("download_attachment", { attachmentId: "att-401" })).rejects.toMatchObject({
      status: 401,
      message: "expired",
    });
    expect(getWebToken()).toBeNull();
    expect(expired).toHaveBeenCalledOnce();
  });

  it("throws structured error on non-ok with JSON error body", async () => {
    const fetchMock = vi.fn().mockResolvedValue(
      jsonResponse({ error: { code: "BAD_REQUEST", message: "bad args" } }, false, 400),
    );
    vi.stubGlobal("fetch", fetchMock);

    await expect(invokeWeb("get_message", {})).rejects.toMatchObject({
      message: "bad args",
      status: 400,
      code: "BAD_REQUEST",
    });
  });

  it("throws with HTTP status message when error body is not JSON", async () => {
    const fetchMock = vi.fn().mockResolvedValue({
      ok: false,
      status: 500,
      json: async () => {
        throw new Error("not json");
      },
    });
    vi.stubGlobal("fetch", fetchMock);

    await expect(invokeWeb("send_email", {})).rejects.toMatchObject({
      message: "HTTP 500",
      status: 500,
    });
  });

  it("clears the session and signals the login gate on every 401", async () => {
    setWebToken("expired-token");
    const expired = vi.fn();
    window.addEventListener(WEB_SESSION_EXPIRED_EVENT, expired, { once: true });
    vi.stubGlobal(
      "fetch",
      vi.fn().mockResolvedValue(jsonResponse({ error: { message: "expired" } }, false, 401)),
    );

    await expect(invokeWeb("list_accounts")).rejects.toMatchObject({ status: 401 });

    expect(getWebToken()).toBeNull();
    expect(expired).toHaveBeenCalledOnce();
  });

  it("completes OAuth through a same-origin popup callback", async () => {
    const popup = { closed: false, location: { href: "" } } as unknown as Window;
    vi.stubGlobal(
      "fetch",
      vi.fn().mockResolvedValue(jsonResponse({ authorization_url: "https://accounts.example/auth" }, true)),
    );
    vi.stubGlobal("open", vi.fn(() => popup));

    const accountPromise = invokeWeb<{ id: string }>("complete_oauth_flow", {
      provider: "gmail",
      email: "fallback@example.com",
      displayName: "Fallback",
    });
    await new Promise((resolve) => setTimeout(resolve, 0));
    const successMessage = new Event("message");
    Object.defineProperties(successMessage, {
      source: { value: popup },
      origin: { value: window.location.origin },
      data: { value: { type: "pebble-oauth", status: "success", account: { id: "oauth-1" } } },
    });
    window.dispatchEvent(successMessage);

    await expect(accountPromise).resolves.toEqual({ id: "oauth-1" });
    expect(window.open).toHaveBeenCalledWith(
      "about:blank",
      "pebble-oauth",
      "popup,width=520,height=720",
    );
    expect(popup.location.href).toBe("https://accounts.example/auth");
  });

  it("rejects OAuth messages from a different origin", async () => {
    const popup = { closed: false, location: { href: "" } } as unknown as Window;
    vi.stubGlobal(
      "fetch",
      vi.fn().mockResolvedValue(jsonResponse({ authorization_url: "https://accounts.example/auth" }, true)),
    );
    vi.stubGlobal("open", vi.fn(() => popup));

    const accountPromise = invokeWeb("complete_oauth_flow", { provider: "outlook" });
    await new Promise((resolve) => setTimeout(resolve, 0));
    const forgedMessage = new Event("message");
    Object.defineProperties(forgedMessage, {
      source: { value: popup },
      origin: { value: "https://attacker.example" },
      data: { value: { type: "pebble-oauth", status: "success", account: { id: "forged" } } },
    });
    window.dispatchEvent(forgedMessage);
    popup.closed = true;

    await expect(accountPromise).rejects.toThrow("OAuth authorization was cancelled");
  });
});

describe("Web browser notifications", () => {
  afterEach(() => {
    vi.unstubAllGlobals();
    localStorage.removeItem("pebble-notifications-enabled");
  });

  it("persists the preference and reports Web capability without a network request", async () => {
    const fetchMock = vi.fn();
    vi.stubGlobal("fetch", fetchMock);

    const initial = await invokeWeb<{
      enabled: boolean;
      attention_active: boolean;
      platform: string;
      app_id: string | null;
    }>("get_notification_status");
    expect(initial).toEqual({ enabled: false, attention_active: false, platform: "web", app_id: null });

    await invokeWeb("set_notifications_enabled", { enabled: true });
    expect(localStorage.getItem("pebble-notifications-enabled")).toBe("true");
    const enabled = await invokeWeb<{ enabled: boolean }>("get_notification_status");
    expect(enabled.enabled).toBe(true);
    expect(fetchMock).not.toHaveBeenCalled();

    await invokeWeb("set_notifications_enabled", { enabled: false });
    expect(localStorage.getItem("pebble-notifications-enabled")).toBe("false");
  });

  it("requests permission when enabled and shows a granted notification", async () => {
    const NotificationMock = vi.fn();
    Object.defineProperty(NotificationMock, "permission", { value: "default", configurable: true });
    NotificationMock.requestPermission = vi.fn().mockResolvedValue("granted");
    vi.stubGlobal("Notification", NotificationMock);

    await invokeWeb("set_notifications_enabled", { enabled: true });
    await Promise.resolve();
    expect(NotificationMock.requestPermission).toHaveBeenCalledOnce();

    Object.defineProperty(NotificationMock, "permission", { value: "granted", configurable: true });
    await invokeWeb("show_test_notification");
    expect(NotificationMock).toHaveBeenCalledWith("Pebble", { body: "Test notification" });
  });

  it("opens the target message when a browser notification is clicked", async () => {
    const close = vi.fn();
    const notification = { close, onclick: null as (() => void) | null };
    const NotificationMock = vi.fn(() => notification);
    Object.defineProperty(NotificationMock, "permission", { value: "granted", configurable: true });
    vi.stubGlobal("Notification", NotificationMock);
    const opened = vi.fn();
    window.addEventListener(WEB_NOTIFICATION_OPEN_EVENT, opened);

    await showWebNotification("New mail", "Subject", {
      account_id: "account-1",
      message_id: "message-1",
    });
    notification.onclick?.();

    expect(opened).toHaveBeenCalledOnce();
    expect((opened.mock.calls[0][0] as CustomEvent).detail).toEqual({
      account_id: "account-1",
      message_id: "message-1",
    });
    expect(close).toHaveBeenCalledOnce();
    window.removeEventListener(WEB_NOTIFICATION_OPEN_EVENT, opened);
  });

  it("silently handles denied permission and browsers without Notification", async () => {
    const DeniedNotification = vi.fn();
    Object.defineProperty(DeniedNotification, "permission", { value: "denied", configurable: true });
    DeniedNotification.requestPermission = vi.fn();
    vi.stubGlobal("Notification", DeniedNotification);
    await invokeWeb("set_notifications_enabled", { enabled: true });
    await invokeWeb("show_test_notification");
    expect(DeniedNotification.requestPermission).not.toHaveBeenCalled();
    expect(DeniedNotification).not.toHaveBeenCalled();

    vi.stubGlobal("Notification", undefined);
    await expect(invokeWeb("set_notifications_enabled", { enabled: true })).resolves.toBeUndefined();
    await expect(invokeWeb("show_test_notification")).resolves.toBeUndefined();
  });
});

describe("normalizeBackendArgs (前端形态→后端 snake 契约)", () => {
  it("converts top-level camelCase keys to snake_case", () => {
    expect(normalizeBackendArgs("list_messages", { folderId: "f", folderIds: ["a", "b"], limit: 10 })).toEqual({
      folder_id: "f",
      folder_ids: ["a", "b"],
      limit: 10,
    });
  });

  it("keeps add_account request wrapper", () => {
    expect(
      normalizeBackendArgs("add_account", {
        request: { email: "a@b.c", display_name: "A", provider: "imap" },
      }),
    ).toEqual({ request: { email: "a@b.c", display_name: "A", provider: "imap" } });
  });

  it("keeps advanced_search query nested and converts query internals", () => {
    expect(
      normalizeBackendArgs("advanced_search", {
        query: { text: "hi", dateFrom: 1, hasAttachment: true, folderId: "f" },
        limit: 5,
      }),
    ).toEqual({ query: { text: "hi", date_from: 1, has_attachment: true, folder_id: "f" }, limit: 5 });
  });

  it("keeps batch_star starred as-is starred", () => {
    expect(normalizeBackendArgs("batch_star", { messageIds: ["m"], starred: true })).toEqual({
      message_ids: ["m"],
      starred: true,
    });
  });

  it("converts nested objects recursively", () => {
    expect(
      normalizeBackendArgs("send_email", { accountId: "a", attachmentPaths: ["/x"] }),
    ).toEqual({ account_id: "a", attachment_paths: ["/x"] });
  });

  it("is idempotent for already snake_case keys", () => {
    expect(normalizeBackendArgs("delete_account", { account_id: "id" })).toEqual({
      account_id: "id",
    });
  });

  it("leaves non-object args as-is", () => {
    expect(normalizeBackendArgs("list_accounts", undefined)).toEqual({});
  });
});
