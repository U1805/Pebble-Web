/**
 * Browser-only event names used to bridge APIs that have no WebSocket frame.
 *
 * Kept separate from `events.ts` so the Web transport can emit these events
 * without creating a dependency cycle through the platform selector.
 */
export const WEB_NOTIFICATION_OPEN_EVENT = "pebble:web-notification-open";
export const WEB_ATTACHMENT_DOWNLOAD_PROGRESS_EVENT =
  "pebble:web-attachment-download-progress";
