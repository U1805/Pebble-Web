import { readFileSync } from "node:fs";
import path from "node:path";
import { describe, expect, it } from "vitest";
import { patchSidebarEmptyAccounts } from "./sidebar_empty_accounts";

describe("Sidebar empty-account compatibility patch", () => {
  it("stabilizes the current upstream Sidebar without copying the component", () => {
    const source = readFileSync(path.join(process.cwd(), "src/components/Sidebar.tsx"), "utf8");
    const patched = patchSidebarEmptyAccounts(source);

    expect(patched).toContain("setActiveAccountId(null)");
    expect(patched).not.toContain("const next = accounts[idx + 1]");
    expect(patched).not.toBe(source);
  });

  it("fails visibly when the upstream code no longer matches", () => {
    expect(() => patchSidebarEmptyAccounts("export default function Sidebar() {}"))
      .toThrow("review the Web compatibility patch");
  });
});
