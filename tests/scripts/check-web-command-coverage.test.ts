import { describe, expect, it } from "vitest";
import { contractReport } from "../../scripts/check-web-command-coverage.mjs";

describe("Web command contract", () => {
  it("classifies every static frontend invoke exactly once", () => {
    expect(contractReport.missing).toEqual([]);
    expect(contractReport.duplicates).toEqual([]);
    expect(contractReport.dynamic).toEqual([]);
    expect(contractReport.unsupported).toEqual([]);
    expect(contractReport.noops).not.toContain("complete_oauth_flow");
    expect(contractReport.noops).toContain("open_default_mail_settings");
    expect(contractReport.noops).toContain("sync_titlebar_theme");
  });
});
