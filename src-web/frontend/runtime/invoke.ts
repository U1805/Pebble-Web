import type { InvokeArgs } from "./args";
import { downloadAttachmentWeb, stageComposeAttachmentWeb } from "./attachments";
import { importBackgroundImageWeb } from "./appearance";
import { postCommand } from "./http";
import { invokeDesktopOnlyCommand, isDesktopOnlyCommand } from "./desktop";
import {
  disableWebNotifications,
  getWebNotificationsEnabled,
  initializeWebNotificationPreference,
  requestWebNotificationPermission,
  setWebNotificationsEnabled,
  showWebTestNotification,
} from "./notifications";
import { completeOAuthFlowWeb } from "./oauth";

export type { InvokeArgs } from "./args";

function requireStringArg(command: string, args: InvokeArgs | undefined, key: string): string {
  const value = args?.[key];
  if (typeof value !== "string") {
    throw new Error(`Invalid ${command} args: ${key} must be a string`);
  }
  return value;
}

function requireBooleanArg(command: string, args: InvokeArgs | undefined, key: string): boolean {
  const value = args?.[key];
  if (typeof value !== "boolean") {
    throw new Error(`Invalid ${command} args: ${key} must be a boolean`);
  }
  return value;
}

function requireU8ArrayArg(command: string, args: InvokeArgs | undefined, key: string): number[] {
  const value = args?.[key];
  if (!Array.isArray(value)) {
    throw new Error(`Invalid ${command} args: ${key} must be an array of bytes`);
  }
  for (let index = 0; index < value.length; index += 1) {
    if (!(index in value)) {
      throw new Error(`Invalid ${command} args: ${key} must be an array of bytes`);
    }
    const item = value[index];
    if (typeof item !== "number" || !Number.isInteger(item) || item < 0 || item > 255) {
      throw new Error(`Invalid ${command} args: ${key} must be an array of bytes`);
    }
  }
  return value as number[];
}

/** Web implementation of Tauri's `invoke(command, args)` contract. */
export async function invokeWeb<T>(command: string, args?: InvokeArgs): Promise<T> {
  if (isDesktopOnlyCommand(command)) return invokeDesktopOnlyCommand(command, args) as T;

  switch (command) {
    case "get_profile_storage_namespace": {
      const namespace = await postCommand<string>(command, args);
      initializeWebNotificationPreference(namespace);
      return namespace as T;
    }
    case "complete_oauth_flow":
      return completeOAuthFlowWeb(args) as T;
    case "open_external_url": {
      const raw = requireStringArg(command, args, "url");
      if (!raw.startsWith("https://") && !raw.startsWith("http://") && !raw.startsWith("mailto:")) {
        throw new Error("Only https://, http://, and mailto: URLs are permitted");
      }
      try {
        if (raw.startsWith("mailto:")) {
          window.location.href = raw;
        } else {
          window.open(raw, "_blank", "noopener,noreferrer");
        }
      } catch (error) {
        const message = error instanceof Error ? error.message : String(error);
        throw new Error(`Failed to open URL: ${message}`);
      }
      return undefined as T;
    }
    case "download_attachment": {
      const attachmentId = requireStringArg(command, args, "attachmentId");
      const saveTo = requireStringArg(command, args, "saveTo");
      return downloadAttachmentWeb(attachmentId, saveTo) as T;
    }
    case "stage_compose_attachment": {
      const filename = requireStringArg(command, args, "filename");
      const bytes = requireU8ArrayArg(command, args, "bytes");
      return stageComposeAttachmentWeb(filename, bytes) as T;
    }
    case "import_background_image": {
      const filename = requireStringArg(command, args, "filename");
      const bytes = requireU8ArrayArg(command, args, "bytes");
      return importBackgroundImageWeb(filename, bytes) as T;
    }
    case "get_notification_status": {
      const permissionGranted =
        typeof Notification !== "undefined" && Notification.permission === "granted";
      const enabled = getWebNotificationsEnabled() && permissionGranted;
      if (!enabled && getWebNotificationsEnabled()) disableWebNotifications();
      return {
        enabled,
        attention_active: false,
        platform: "web",
        app_id: null,
      } as T;
    }
    case "set_notifications_enabled": {
      const enabled = requireBooleanArg(command, args, "enabled");
      if (!enabled) {
        setWebNotificationsEnabled(false);
        return undefined as T;
      }
      const granted = await requestWebNotificationPermission();
      if (!granted) {
        disableWebNotifications();
        throw new Error("Browser notification permission was not granted");
      }
      setWebNotificationsEnabled(true);
      return undefined as T;
    }
    case "show_test_notification":
      if (!getWebNotificationsEnabled()) {
        throw new Error("Browser notifications are disabled");
      }
      if (typeof Notification === "undefined" || Notification.permission !== "granted") {
        disableWebNotifications();
        throw new Error("Browser notification permission is not granted");
      }
      await showWebTestNotification("Pebble - Test Notification", "Browser notifications are working.");
      return undefined as T;
    case "clear_notification_attention":
      return undefined as T;
    default:
      return postCommand<T>(command, args);
  }
}
