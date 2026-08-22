import { renderHook } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

const mocks = vi.hoisted(() => ({
  listeners: new Map<string, (event: { payload: { account_id?: string; message_id?: string } }) => void>(),
  setActiveAccountId: vi.fn(),
  openMessageInInbox: vi.fn(),
  invalidateQueries: vi.fn(),
}));

vi.mock("@/lib/platform", () => ({
  platform: "web",
  listen: vi.fn((eventName: string, handler: (event: { payload: { account_id?: string; message_id?: string } }) => void) => {
    mocks.listeners.set(eventName, handler);
    return Promise.resolve(vi.fn());
  }),
}));

vi.mock("@tanstack/react-query", () => ({
  useQueryClient: () => ({
    invalidateQueries: mocks.invalidateQueries,
  }),
}));

vi.mock("../../src/stores/mail.store", () => ({
  useMailStore: (selector: (state: { setActiveAccountId: (accountId: string) => void }) => unknown) =>
    selector({ setActiveAccountId: mocks.setActiveAccountId }),
}));

vi.mock("../../src/stores/ui.store", () => ({
  useUIStore: (selector: (state: { openMessageInInbox: (messageId: string) => void }) => unknown) =>
    selector({ openMessageInInbox: mocks.openMessageInInbox }),
}));

import { useNotificationOpenNavigation } from "../../src/app/useNotificationOpenNavigation";

describe("useNotificationOpenNavigation (Web)", () => {
  it("opens the target message from the platform notification-open event", () => {
    renderHook(() => useNotificationOpenNavigation());
    mocks.listeners.get("mail:notification-open")?.({
      payload: { account_id: "account-2", message_id: "message-1" },
    });

    expect(mocks.setActiveAccountId).toHaveBeenCalledWith("account-2");
    expect(mocks.openMessageInInbox).toHaveBeenCalledWith("message-1");
    expect(mocks.invalidateQueries).toHaveBeenCalledWith({ queryKey: ["messages"] });
    expect(mocks.invalidateQueries).toHaveBeenCalledWith({ queryKey: ["threads"] });
    expect(mocks.invalidateQueries).toHaveBeenCalledWith({ queryKey: ["folders", "account-2"] });
  });
});
