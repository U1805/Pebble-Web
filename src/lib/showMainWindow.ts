import { getCurrentWindow } from "@tauri-apps/api/window";
import { shouldShowMainWindowOnStartup } from "@/lib/startupVisibility";
import { logStartupTiming } from "@/lib/startupTiming";

export async function showMainWindow() {
  if (!shouldShowMainWindowOnStartup()) {
    logStartupTiming("main window left hidden for tray startup");
    return;
  }

  try {
    await getCurrentWindow().show();
    logStartupTiming("main window shown");
  } catch (err) {
    console.warn("Failed to show main window", err);
  }
}
