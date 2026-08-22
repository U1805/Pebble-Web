import { describe, expect, it } from "vitest";

import { capabilities, invoke, platform } from "@/lib/platform";

describe("platform 判定与能力表", () => {
  it("缺省平台为 tauri（桌面行为不变）；未注入 VITE_PLATFORM 时不启用 Web 调用层", () => {
    expect(platform).toBe("tauri");
  });

  it("Web 端差异能力集中收敛在 capabilities 中", () => {
    // 验证真实消费的能力存在且类型稳定（应用功能：测试连接/日志/版本已由后端提供，不再进能力表）
    expect(capabilities).toHaveProperty("windowControls");
    expect(capabilities).toHaveProperty("defaultMailClient");
    expect(capabilities).toHaveProperty("backgroundClose");
    expect(capabilities).toHaveProperty("desktopSettings");
    expect(capabilities).toHaveProperty("browserDownloads");
  });

  it("invoke 是统一命令入口（tauri 平台下可调用）", () => {
    // 只验证接口存在与签名可用；实际网络调用由各平台实现承载
    expect(typeof invoke).toBe("function");
  });
});