import App from "@/App";
import { useEffect } from "react";
import { logStartupTiming } from "@/lib/startupTiming";
import AuthGate from "./AuthGate";
import { useWebNotifications } from "./Notifications";
import { initializeWebDesktopPreferences } from "../runtime/desktop-preferences";
import "./web.css";

initializeWebDesktopPreferences();
document.documentElement.dataset.pebblePlatform = "web";

function AuthenticatedApp() {
  useWebNotifications();
  return <App />;
}

export default function WebApp() {
  // splash 移除必须在本层完成：未认证时 AuthGate 不渲染 <App/>，
  // 而 App.tsx 内的 splash 清理逻辑不会执行，导致登录页被 splash 遮挡。
  useEffect(() => {
    const splash = document.getElementById("splash");
    if (!splash) return;
    const splashStart =
      (window as unknown as Record<string, number>).__splashStart || Date.now();
    const remaining = Math.max(0, 2200 - (Date.now() - splashStart));
    const timer = setTimeout(() => {
      splash.classList.add("fade-out");
      setTimeout(() => {
        splash.remove();
        document.getElementById("splash-style")?.remove();
        logStartupTiming("splash removed (web)");
      }, 500);
    }, remaining);
    return () => clearTimeout(timer);
  }, []);

  return (
    <AuthGate>
      <AuthenticatedApp />
    </AuthGate>
  );
}
