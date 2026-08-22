import type { InvokeArgs } from "./args";

type DesktopCommandHandler = (args?: InvokeArgs) => unknown;

/**
 * Browser compatibility handlers for commands whose underlying concept is
 * specific to the native desktop shell.
 *
 * Keep application/business commands out of this table. Those commands must
 * have a real Web backend implementation.
 */
const DESKTOP_ONLY_COMMANDS: Record<string, DesktopCommandHandler> = {
  get_autostart_enabled: () => false,
  set_autostart_enabled: () => {
    throw new Error("Launch at startup is only available in the desktop app");
  },
  set_tray_menu_labels: () => undefined,
  take_pending_mailto_urls: () => [],
  open_default_mail_settings: () => {
    window.alert("Default mail app settings are only available in the desktop app.");
  },
  sync_titlebar_theme: () => undefined,
};

export function isDesktopOnlyCommand(command: string): boolean {
  return Object.prototype.hasOwnProperty.call(DESKTOP_ONLY_COMMANDS, command);
}

export function invokeDesktopOnlyCommand(command: string, args?: InvokeArgs): unknown {
  return DESKTOP_ONLY_COMMANDS[command]?.(args);
}
