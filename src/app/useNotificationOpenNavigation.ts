import { useEffect } from "react";
import { listen } from "@tauri-apps/api/event";
import { useQueryClient } from "@tanstack/react-query";
import { useMailStore } from "../stores/mail.store";
import { useUIStore } from "../stores/ui.store";

interface NotificationOpenPayload {
  account_id?: string;
  message_id?: string;
}

export function useNotificationOpenNavigation() {
  const setActiveAccountId = useMailStore((s) => s.setActiveAccountId);
  const openMessageInInbox = useUIStore((s) => s.openMessageInInbox);
  const queryClient = useQueryClient();

  useEffect(() => {
    const unlisten = listen<NotificationOpenPayload>("mail:notification-open", (event) => {
      const accountId = event.payload.account_id;
      const messageId = event.payload.message_id;
      if (!messageId) return;

      queryClient.invalidateQueries({ queryKey: ["messages"] });
      queryClient.invalidateQueries({ queryKey: ["threads"] });
      if (accountId) {
        setActiveAccountId(accountId);
        queryClient.invalidateQueries({ queryKey: ["folders", accountId] });
      }
      openMessageInInbox(messageId);
    });
    return () => {
      unlisten.then((fn) => fn());
    };
  }, [openMessageInInbox, queryClient, setActiveAccountId]);
}
