import { WEB_NOTIFICATION_OPEN_EVENT } from "./local-events";

const NOTIFICATIONS_ENABLED_KEY = "pebble-notifications-enabled";

export interface WebNotificationTarget {
  account_id?: string;
  message_id?: string;
}

export function getWebNotificationsEnabled(): boolean {
  if (typeof localStorage === "undefined") return false;
  return localStorage.getItem(NOTIFICATIONS_ENABLED_KEY) === "true";
}

export function setWebNotificationsEnabled(enabled: boolean): void {
  try {
    localStorage.setItem(NOTIFICATIONS_ENABLED_KEY, String(enabled));
  } catch {
    // Private browsing or denied storage can fail without breaking mail.
  }
}

export async function requestWebNotificationPermission(): Promise<boolean> {
  if (typeof Notification === "undefined") return false;
  if (Notification.permission === "granted") return true;
  if (Notification.permission === "denied") return false;
  try {
    return (await Notification.requestPermission()) === "granted";
  } catch {
    return false;
  }
}

export async function showWebNotification(
  title: string,
  body: string,
  target?: WebNotificationTarget,
): Promise<void> {
  if (typeof Notification === "undefined" || Notification.permission !== "granted") return;
  try {
    const notification = new Notification(title, { body });
    if (target?.message_id) {
      notification.onclick = () => {
        window.dispatchEvent(new CustomEvent(WEB_NOTIFICATION_OPEN_EVENT, { detail: target }));
        notification.close();
      };
    }
  } catch {
    // Notification support can be blocked by browser or privacy mode.
  }
}
