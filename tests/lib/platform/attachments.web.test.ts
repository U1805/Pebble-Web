import { afterAll, beforeAll, describe, expect, it, vi } from "vitest";

let getAttachmentSavePath: typeof import("@/lib/platform/attachments").getAttachmentSavePath;

beforeAll(async () => {
  vi.stubEnv("VITE_PLATFORM", "web");
  vi.resetModules();
  ({ getAttachmentSavePath } = await import("@/lib/platform/attachments"));
});

afterAll(() => {
  vi.unstubAllEnvs();
});

describe("platform/attachments", () => {
  it("does not require a Tauri filesystem path for browser downloads", async () => {
    await expect(getAttachmentSavePath("report:final?.pdf")).resolves.toBe("");
  });
});
