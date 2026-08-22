import { useEffect, useState, type ReactNode } from "react";
import { getWebToken, WEB_SESSION_EXPIRED_EVENT } from "../runtime/session";
import LoginPage from "./LoginPage";

export default function AuthGate({ children }: { children: ReactNode }) {
  const [token, setToken] = useState<string | null>(() => getWebToken());

  useEffect(() => {
    const returnToLogin = () => setToken(null);
    window.addEventListener(WEB_SESSION_EXPIRED_EVENT, returnToLogin);
    return () => window.removeEventListener(WEB_SESSION_EXPIRED_EVENT, returnToLogin);
  }, []);

  if (!token) {
    return <LoginPage onSuccess={() => setToken(getWebToken())} />;
  }
  return <>{children}</>;
}
