import type { Plugin } from "vite";

const SIDEBAR_SOURCE_SUFFIX = "/src/components/Sidebar.tsx";

const UPSTREAM_EMPTY_ACCOUNT_FALLBACK = `    } else if (!allAccountsMode && foldersFetched && displayedFolders.length === 0 && activeAccountId && accounts.length > 1) {
      const idx = accounts.findIndex((a) => a.id === activeAccountId);
      const next = accounts[idx + 1] ?? accounts.find((a) => a.id !== activeAccountId);
      if (next) {
        setActiveAccountId(next.id);
      }
    }`;

const WEB_EMPTY_ACCOUNT_FALLBACK = `    } else if (!allAccountsMode && foldersFetched && displayedFolders.length === 0 && activeAccountId && accounts.length > 1) {
      // Two or more accounts can legitimately have no folders while their
      // first sync is pending or has failed. Cycling to another empty account
      // makes this effect alternate forever, so return to the stable combined
      // mailbox until a provider-backed folder exists.
      setActiveAccountId(null);
    }`;

export function patchSidebarEmptyAccounts(source: string): string {
  if (!source.includes(UPSTREAM_EMPTY_ACCOUNT_FALLBACK)) {
    throw new Error("Pebble Sidebar empty-account fallback changed; review the Web compatibility patch");
  }
  return source.replace(UPSTREAM_EMPTY_ACCOUNT_FALLBACK, WEB_EMPTY_ACCOUNT_FALLBACK);
}

export function sidebarEmptyAccountsPatch(): Plugin {
  return {
    name: "pebble-web-sidebar-empty-accounts-patch",
    enforce: "pre",
    transform(source, id) {
      const normalizedId = id.split("?", 1)[0].split("\\").join("/");
      if (!normalizedId.endsWith(SIDEBAR_SOURCE_SUFFIX)) return null;
      return { code: patchSidebarEmptyAccounts(source), map: null };
    },
  };
}
