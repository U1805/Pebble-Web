import i18n from "../i18n";
import { invoke } from "@tauri-apps/api/core";
import { useToastStore } from "@/stores/toast.store";
import { useUIStore } from "@/stores/ui.store";

type DesktopPreferenceCommand =
  | "set_start_hidden_to_tray"
  | "set_keep_running_in_background";

function updateDesktopPreference(command: DesktopPreferenceCommand, enabled: boolean): void {
  void invoke(command, { enabled }).catch(() => {
    useToastStore.getState().addToast({
      type: "error",
      message: i18n.t("web.settingsUpdateFailed"),
    });
  });
}

/** Route upstream's local-only setters through the Web command boundary.
 * Neither a rejected request nor old desktop preferences may enable native
 * window behavior in the browser. This also covers the status-bar shortcut.
 */
export function initializeWebDesktopPreferences(): void {
  useUIStore.setState({
    startHiddenToTray: false,
    keepRunningInBackground: true,
    setStartHiddenToTray: (enabled) =>
      updateDesktopPreference("set_start_hidden_to_tray", enabled),
    setKeepRunningInBackground: (enabled) =>
      updateDesktopPreference("set_keep_running_in_background", enabled),
  });
}
