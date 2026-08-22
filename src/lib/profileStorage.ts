import { invoke } from "@tauri-apps/api/core";

type StorageLike = Pick<Storage, "getItem" | "setItem" | "removeItem">;
type NamespaceResolver = () => Promise<string>;

let profileStorageNamespace: string | null = null;

function normalizeNamespace(namespace: string | null | undefined): string | null {
  const trimmed = namespace?.trim() ?? "";
  return trimmed ? trimmed : null;
}

function scopedKey(key: string): string {
  return profileStorageNamespace ? `pebble:profile:${profileStorageNamespace}:${key}` : key;
}

function createProfileStorage(storage: StorageLike): StorageLike {
  return {
    getItem: (key) => storage.getItem(scopedKey(key)),
    setItem: (key, value) => storage.setItem(scopedKey(key), value),
    removeItem: (key) => storage.removeItem(scopedKey(key)),
  };
}

export const profileLocalStorage = createProfileStorage(localStorage);
export const profileSessionStorage = createProfileStorage(sessionStorage);

// localStorage keys written by releases before profile-scoped storage existed.
// On upgrades the scoped key starts out missing, so without copying these over
// every user's theme, language, and other preferences would silently reset.
const LEGACY_MIGRATED_MARKER = "pebble:profile-storage-migrated";
const LEGACY_LOCAL_KEYS = [
  "pebble-language",
  "pebble-theme",
  "pebble-privacy-mode",
  "pebble-start-hidden-to-tray",
  "pebble-shortcuts",
  "pebble-cloud-sync-last-backup",
  "pebble-translate-privacy-ack",
  "pebble-background-image",
  "pebble-realtime-mode",
  "pebble-notifications-enabled",
  "pebble-keep-running-background",
  "pebble-poll-interval",
  "pebble-show-unread-count",
] as const;
const LEGACY_SESSION_KEYS = ["pebble-settings-tab"] as const;

function migrateLegacyKeysOnce(namespace: string) {
  if (localStorage.getItem(LEGACY_MIGRATED_MARKER) === namespace) return;
  for (const key of LEGACY_LOCAL_KEYS) {
    const value = localStorage.getItem(key);
    if (value === null) continue;
    const scoped = `pebble:profile:${namespace}:${key}`;
    if (localStorage.getItem(scoped) === null) {
      localStorage.setItem(scoped, value);
    }
    localStorage.removeItem(key);
  }
  for (const key of LEGACY_SESSION_KEYS) {
    const value = sessionStorage.getItem(key);
    if (value === null) continue;
    const scoped = `pebble:profile:${namespace}:${key}`;
    if (sessionStorage.getItem(scoped) === null) {
      sessionStorage.setItem(scoped, value);
    }
    sessionStorage.removeItem(key);
  }
  localStorage.setItem(LEGACY_MIGRATED_MARKER, namespace);
}

export function getProfileStorageNamespace(): string | null {
  return profileStorageNamespace;
}

export async function initializeProfileStorageNamespace(
  resolver: NamespaceResolver = () => invoke<string>("get_profile_storage_namespace"),
) {
  try {
    profileStorageNamespace = normalizeNamespace(await resolver());
  } catch {
    profileStorageNamespace = null;
  }
  if (profileStorageNamespace) {
    try {
      migrateLegacyKeysOnce(profileStorageNamespace);
    } catch (err) {
      console.warn("Failed to migrate legacy localStorage keys", err);
    }
  }
}

export function resetProfileStorageNamespaceForTests() {
  profileStorageNamespace = null;
}
