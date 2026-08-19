import { describe, expect, it } from "vitest";

import { capabilities, invoke, platform } from "@/lib/platform";

describe("platform 判定与能力表", () => {
  it("缺省平台为 tauri（桌面行为不变）；未注入 VITE_PLATFORM 时不启用 Web 调用层", () => {
    expect(platform).toBe("tauri");
  });

  it("Web 端缺失的能力集中收敛在 capabilities 中", () => {
    // tauri 平台下这些能力为 false 会破坏桌面端，这里只验证结构存在且类型稳定
    expect(capabilities).toHaveProperty("tray");
    expect(capabilities).toHaveProperty("nativeNotifications");
    expect(capabilities).toHaveProperty("windowControls");
    expect(capabilities).toHaveProperty("nativeFileSystem");
    expect(capabilities).toHaveProperty("browserDownloads");
  });

  it("invoke 是统一命令入口（tauri 平台下可调用）", () => {
    // 只验证接口存在与签名可用；实际网络调用由各平台实现承载
    expect(typeof invoke).toBe("function");
  });
});