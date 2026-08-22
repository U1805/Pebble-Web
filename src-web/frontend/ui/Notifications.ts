import { useEffect } from "react";
import { useTranslation } from "react-i18next";
import { listen } from "../runtime/events";
import { getWebNotificationsEnabled, showWebNotification } from "../runtime/notifications";

interface MailNewEventPayload {
  account_id?: string;
  message_id?: string;
  thread_id?: string | null;
  subject?: string;
  from?: string;
}

export function useWebNotifications() {
  const { t } = useTranslation();

  useEffect(() => {
    const unlisteners: Array<() => void> = [];

    const newMail = listen<MailNewEventPayload>("mail:new", ({ payload }) => {
      if (!getWebNotificationsEnabled()) return;
      const body = payload.subject
        ? (payload.from ? `${payload.from}: ${payload.subject}` : payload.subject)
        : t("webNotifications.newMailBody", "A new message has arrived");
      void showWebNotification(t("webNotifications.newMailTitle", "New mail"), body, {
        account_id: payload.account_id,
        message_id: payload.message_id,
      });
    });
    unlisteners.push(() => { newMail.then((un) => un()).catch(() => {}); });

    const error = listen<{ message?: string }>("mail:error", ({ payload }) => {
      if (!getWebNotificationsEnabled()) return;
      void showWebNotification(
        t("webNotifications.syncErrorTitle", "Mail sync failed"),
        payload.message ?? "",
      );
    });
    unlisteners.push(() => { error.then((un) => un()).catch(() => {}); });

    return () => { unlisteners.forEach((un) => un()); };
  }, [t]);
}
