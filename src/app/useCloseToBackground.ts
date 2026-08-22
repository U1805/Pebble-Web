import { useEffect } from "react";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { useUIStore } from "@/stores/ui.store";
import { startSync } from "@/lib/api";
import { useAccountsQuery } from "@/hooks/queries";

export function useCloseToBackground() {
  const { data: accounts } = useAccountsQuery();
  const pollInterval = useUIStore((s) => s.pollInterval);
  const realtimeMode = useUIStore((s) => s.realtimeMode);

  useEffect(() => {
    const appWindow = getCurrentWindow();
    let unlisten: (() => void) | undefined;
    let disposed = false;

    appWindow
      .onCloseRequested((event) => {
        if (!useUIStore.getState().keepRunningInBackground) {
          return;
        }

        event.preventDefault();
        void appWindow
          .hide()
          .catch((err) => console.warn("Failed to hide window on close", err));
      })
      .then((fn) => {
        if (disposed) {
          fn();
          return;
        }
        unlisten = fn;
      })
      .catch((err) => console.warn("Failed to register close-to-background handler", err));

    return () => {
      disposed = true;
      unlisten?.();
    };
  }, []);

  // Resume sync workers when window regains visibility after being hidden to tray
  useEffect(() => {
    const appWindow = getCurrentWindow();
    let unlisten: (() => void) | undefined;
    let disposed = false;

    appWindow
      .onFocusChanged(({ payload: focused }) => {
        if (!focused) return;
        if (realtimeMode === "manual") return;
        const ids = accounts?.map((a) => a.id) ?? [];
        for (const id of ids) {
          startSync(id, pollInterval).catch(() => {});
        }
      })
      .then((fn) => {
        if (disposed) {
          fn();
          return;
        }
        unlisten = fn;
      })
      .catch(() => {});

    return () => {
      disposed = true;
      unlisten?.();
    };
  }, [accounts, pollInterval, realtimeMode]);
}
