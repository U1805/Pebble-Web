import { renderHook, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { useWebNotifications } from "../../src/app/useWebNotifications";

const mocks = vi.hoisted(() => ({
  listeners: new Map<string, (event: { payload: Record<string, unknown> }) => void>(),
  getWebNotificationsEnabled: vi.fn(() => true),
  showWebNotification: vi.fn(() => Promise.resolve()),
}));

vi.mock("../../src/lib/platform", () => ({
  platform: "web",
  listen: vi.fn((event: string, handler: (event: { payload: Record<string, unknown> }) => void) => {
    mocks.listeners.set(event, handler);
    return Promise.resolve(() => mocks.listeners.delete(event));
  }),
}));

vi.mock("../../src/lib/platform/web", () => ({
  getWebNotificationsEnabled: mocks.getWebNotificationsEnabled,
  showWebNotification: mocks.showWebNotification,
}));

vi.mock("../../src/stores/ui.store", () => ({
  useUIStore: (selector: (state: { notificationsEnabled: boolean }) => unknown) =>
    selector({ notificationsEnabled: true }),
}));

vi.mock("react-i18next", () => ({
  useTranslation: () => ({ t: (_key: string, fallback?: string) => fallback ?? _key }),
}));

describe("useWebNotifications", () => {
  beforeEach(() => {
    mocks.listeners.clear();
    mocks.getWebNotificationsEnabled.mockReset();
    mocks.getWebNotificationsEnabled.mockReturnValue(true);
    mocks.showWebNotification.mockReset();
    mocks.showWebNotification.mockResolvedValue(undefined);
  });

  it("shows a browser notification for a new mail event", async () => {
    const { unmount } = renderHook(() => useWebNotifications());

    await waitFor(() => expect(mocks.listeners.has("mail:new")).toBe(true));
    mocks.listeners.get("mail:new")?.({
      payload: {
        account_id: "account-1",
        message_id: "message-1",
        subject: "Quarterly report",
        from: "alice@example.com",
      },
    });

    await waitFor(() => expect(mocks.showWebNotification).toHaveBeenCalledWith(
      "New mail",
      "alice@example.com: Quarterly report",
      { account_id: "account-1", message_id: "message-1" },
    ));
    unmount();
  });

  it("does not notify when the preference is disabled at event time", async () => {
    const { unmount } = renderHook(() => useWebNotifications());
    await waitFor(() => expect(mocks.listeners.has("mail:new")).toBe(true));

    mocks.getWebNotificationsEnabled.mockReturnValue(false);
    mocks.listeners.get("mail:new")?.({ payload: { subject: "Hidden" } });
    await Promise.resolve();

    expect(mocks.showWebNotification).not.toHaveBeenCalled();
    unmount();
  });
});
