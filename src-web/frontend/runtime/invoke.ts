import type { InvokeArgs } from "./args";
import { downloadAttachmentWeb, stageComposeAttachmentWeb } from "./attachments";
import { postCommand } from "./http";
import { invokeDesktopOnlyCommand, isDesktopOnlyCommand } from "./desktop";
import {
  getWebNotificationsEnabled,
  requestWebNotificationPermission,
  setWebNotificationsEnabled,
  showWebNotification,
} from "./notifications";
import { completeOAuthFlowWeb } from "./oauth";

export type { InvokeArgs } from "./args";

/** Web implementation of Tauri's `invoke(command, args)` contract. */
export async function invokeWeb<T>(command: string, args?: InvokeArgs): Promise<T> {
  if (isDesktopOnlyCommand(command)) return invokeDesktopOnlyCommand(command, args) as T;

  switch (command) {
    case "complete_oauth_flow":
      return completeOAuthFlowWeb(args) as T;
    case "open_external_url": {
      const { url } = (args ?? {}) as { url?: string };
      if (url) window.open(url, "_blank", "noopener,noreferrer");
      return undefined as T;
    }
    case "download_attachment": {
      const { attachmentId } = (args ?? {}) as { attachmentId?: string };
      return downloadAttachmentWeb(attachmentId ?? "") as T;
    }
    case "stage_compose_attachment": {
      const { filename, bytes } = (args ?? {}) as { filename?: string; bytes?: number[] };
      return stageComposeAttachmentWeb(filename ?? "attachment", bytes ?? []) as T;
    }
    case "get_notification_status":
      return {
        enabled: getWebNotificationsEnabled(),
        attention_active: false,
        platform: "web",
        app_id: null,
      } as T;
    case "set_notifications_enabled": {
      const { enabled } = (args ?? {}) as { enabled?: boolean };
      setWebNotificationsEnabled(!!enabled);
      if (enabled) void requestWebNotificationPermission();
      return undefined as T;
    }
    case "show_test_notification":
      void showWebNotification("Pebble", "Test notification");
      return undefined as T;
    case "clear_notification_attention":
      return undefined as T;
    default:
      return postCommand<T>(command, args);
  }
}
