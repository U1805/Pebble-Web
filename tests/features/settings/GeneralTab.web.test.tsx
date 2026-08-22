import { render, screen } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import GeneralTab from "../../../src/features/settings/GeneralTab";

vi.mock("../../../src/lib/platform", () => ({
  capabilities: {
    platform: "web",
    windowControls: false,
    defaultMailClient: false,
    backgroundClose: false,
    desktopSettings: false,
    browserDownloads: true,
  },
}));

vi.mock("../../../src/lib/api", () => ({
  showTestNotification: vi.fn().mockResolvedValue(undefined),
  openDefaultMailSettings: vi.fn().mockResolvedValue(undefined),
  getAutostartEnabled: vi.fn().mockResolvedValue(false),
  setAutostartEnabled: vi.fn().mockResolvedValue(undefined),
}));

vi.mock("react-i18next", () => ({
  initReactI18next: { type: "3rdParty", init: vi.fn() },
  useTranslation: () => ({
    t: (_key: string, fallback?: string) => fallback ?? _key,
  }),
}));

describe("GeneralTab Web capabilities", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    localStorage.clear();
  });

  it("hides desktop-only tray, startup, close, and realtime settings", () => {
    render(<GeneralTab />);

    expect(screen.queryByRole("group", { name: "Realtime Mode" })).toBeNull();
    expect(screen.queryByRole("checkbox", { name: "Quit app when window is closed" })).toBeNull();
    expect(screen.queryByRole("checkbox", { name: "Start hidden to tray" })).toBeNull();
    expect(screen.queryByRole("checkbox", { name: "Launch Pebble at system startup" })).toBeNull();
  });
});
