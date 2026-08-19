import { afterEach, describe, expect, it, vi } from "vitest";

import { invokeWeb } from "@/lib/platform/web";
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
        body: JSON.stringify({ messageId: "m1" }),
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
});