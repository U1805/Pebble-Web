import { useEffect } from "react";
import { listen } from "../runtime/events";
import { getWebNotificationsEnabled, showWebNotification } from "../runtime/notifications";
import { WEB_NOTIFICATION_DISABLED_EVENT } from "../runtime/local-events";
import { useUIStore } from "@/stores/ui.store";

interface WebNotificationEventPayload {
  title?: string;
  body?: string;
  account_id?: string | null;
  message_id?: string | null;
}

export function useWebNotifications() {
  useEffect(() => {
    const onDisabled = () => {
      if (useUIStore.getState().notificationsEnabled) {
        useUIStore.getState().setNotificationsEnabled(false);
      }
    };
    window.addEventListener(WEB_NOTIFICATION_DISABLED_EVENT, onDisabled);

    const notification = listen<WebNotificationEventPayload>("web:notification", ({ payload }) => {
      if (!getWebNotificationsEnabled() || !payload.title) return;
      void showWebNotification(
        payload.title,
        payload.body ?? "",
        payload.message_id
          ? {
              account_id: payload.account_id ?? undefined,
              message_id: payload.message_id,
            }
          : undefined,
      );
    });

    return () => {
      window.removeEventListener(WEB_NOTIFICATION_DISABLED_EVENT, onDisabled);
      notification.then((un) => un()).catch(() => {});
    };
  }, []);
}
