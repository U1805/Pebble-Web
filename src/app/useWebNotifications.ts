import { useEffect } from "react";
import { useTranslation } from "react-i18next";
import { listen, platform } from "@/lib/platform";
import { getWebNotificationsEnabled, showWebNotification } from "@/lib/platform/web";

interface MailNewEventPayload {
  account_id?: string;
  message_id?: string;
  thread_id?: string | null;
  subject?: string;
  from?: string;
}

/**
 * Web 平台系统通知：桌面端由后端在同步到新消息时弹系统通知（IndexingWorker），
 * Web 端无 tauri AppHandle，改为前端监听 `mail:new` / `mail:error` 事件后用
 * 浏览器 Notification API 渲染（与桌面端同类的系统原生通知）。
 *
 * 仅在 Web 平台挂载（桌面端由后端通知，前端不重复弹）。
 */
export function useWebNotifications() {
  const { t } = useTranslation();

  useEffect(() => {
    if (platform !== "web") return;
    // 始终保持事件订阅；处理事件时读取最新偏好，确保用户从关闭切换到开启
    // 后无需刷新页面即可收到通知。桌面端由后端通知，仍不会挂载此 hook。
    // 权限：启动时不主动 requestPermission，首个 user gesture（设置页开关）时申请。

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
    unlisteners.push(() => {
      newMail.then((un) => un()).catch(() => {});
    });

    const error = listen<{ message?: string }>("mail:error", ({ payload }) => {
      if (!getWebNotificationsEnabled()) return;
      void showWebNotification(
        t("webNotifications.syncErrorTitle", "Mail sync failed"),
        payload.message ?? "",
      );
    });
    unlisteners.push(() => {
      error.then((un) => un()).catch(() => {});
    });

    return () => {
      unlisteners.forEach((un) => un());
    };
  }, [t]);
}
