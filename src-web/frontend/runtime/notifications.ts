import {
  WEB_NOTIFICATION_DISABLED_EVENT,
  WEB_NOTIFICATION_OPEN_EVENT,
} from "./local-events";

const NOTIFICATIONS_ENABLED_KEY = "pebble-notifications-enabled";
let webNotificationsEnabled = false;
let notificationPermissionRequest: Promise<boolean> | null = null;

export interface WebNotificationTarget {
  account_id?: string;
  message_id?: string;
}

export function initializeWebNotificationPreference(namespace: string): void {
  if (typeof localStorage === "undefined") return;
  const normalized = namespace.trim();
  if (!normalized) return;
  const scopedKey = `pebble:profile:${normalized}:${NOTIFICATIONS_ENABLED_KEY}`;
  try {
    if (localStorage.getItem(scopedKey) === null && localStorage.getItem(NOTIFICATIONS_ENABLED_KEY) === null) {
      localStorage.setItem(scopedKey, "false");
    }
  } catch {
    // Shared profile storage will keep its normal fallback behavior.
  }
}

export function getWebNotificationsEnabled(): boolean {
  return webNotificationsEnabled;
}

export function setWebNotificationsEnabled(enabled: boolean): void {
  webNotificationsEnabled = enabled;
}

export function disableWebNotifications(): void {
  webNotificationsEnabled = false;
  if (typeof window !== "undefined") {
    window.dispatchEvent(new Event(WEB_NOTIFICATION_DISABLED_EVENT));
  }
}

export async function requestWebNotificationPermission(): Promise<boolean> {
  if (typeof Notification === "undefined") return false;
  if (Notification.permission === "granted") return true;
  if (Notification.permission === "denied") return false;
  if (notificationPermissionRequest) return notificationPermissionRequest;

  notificationPermissionRequest = (async () => {
    try {
      return (await Notification.requestPermission()) === "granted";
    } catch {
      return false;
    }
  })();

  try {
    return await notificationPermissionRequest;
  } finally {
    notificationPermissionRequest = null;
  }
}

function createWebNotification(
  title: string,
  body: string,
  target?: WebNotificationTarget,
): void {
  if (typeof Notification === "undefined" || Notification.permission !== "granted") {
    throw new Error("Browser notification permission is not granted");
  }
  const notification = new Notification(title, { body });
  if (target?.message_id) {
    notification.onclick = () => {
      window.dispatchEvent(new CustomEvent(WEB_NOTIFICATION_OPEN_EVENT, { detail: target }));
      notification.close();
    };
  }
}

export async function showWebNotification(
  title: string,
  body: string,
  target?: WebNotificationTarget,
): Promise<void> {
  try {
    createWebNotification(title, body, target);
  } catch {
    if (typeof Notification === "undefined" || Notification.permission !== "granted") {
      disableWebNotifications();
    }
    // User notifications must not make mail processing fail.
  }
}

export async function showWebTestNotification(title: string, body: string): Promise<void> {
  createWebNotification(title, body);
}
