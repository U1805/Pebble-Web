import { useEffect, useState, type ReactNode } from "react";
import { platform, getWebToken } from "@/lib/platform";
import LoginPage from "@/features/auth/LoginPage";
import { WEB_SESSION_EXPIRED_EVENT } from "@/lib/platform/web";

/**
 * Web 会话门（阶段七）：Web 平台下无 token 先渲染登录页，
 * 登录成功后进入主应用。桌面平台直通（无登录概念）。
 * 401 过期由 query 全局 onError 清除 token 后 reload，重新进入本门。
 */
export default function WebAuthGate({ children }: { children: ReactNode }) {
  const [token, setToken] = useState<string | null>(() => getWebToken());

  useEffect(() => {
    if (platform !== "web") return;
    const returnToLogin = () => setToken(null);
    window.addEventListener(WEB_SESSION_EXPIRED_EVENT, returnToLogin);
    return () => window.removeEventListener(WEB_SESSION_EXPIRED_EVENT, returnToLogin);
  }, []);

  if (platform !== "web") {
    return <>{children}</>;
  }
  if (!token) {
    return <LoginPage onSuccess={() => setToken(getWebToken())} />;
  }
  return <>{children}</>;
}
