import { useState, type FormEvent } from "react";
import { useTranslation } from "react-i18next";
import { loginWeb } from "@/lib/platform/login";

/**
 * Web 登录页（阶段七）。
 * 桌面端无登录概念；Web 端用 PEBBLE_PASSWORD 换取会话 JWT，
 * 未经授权的页面无法访问任何受保护命令。
 */
export default function LoginPage({ onSuccess }: { onSuccess: () => void }) {
  const { t } = useTranslation();
  const [password, setPassword] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [submitting, setSubmitting] = useState(false);

  async function handleSubmit(e: FormEvent) {
    e.preventDefault();
    if (submitting) return;
    setError(null);
    setSubmitting(true);
    try {
      await loginWeb(password);
      onSuccess();
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
      setSubmitting(false);
    }
  }

  return (
    <div
      className="flex h-screen w-full items-center justify-center"
      style={{
        backgroundColor: "var(--color-bg, #fff)",
        color: "var(--color-text-primary, #111)",
      }}
    >
      <form
        onSubmit={handleSubmit}
        style={{
          width: 320,
          padding: 32,
          borderRadius: 12,
          border: "1px solid var(--color-border, #e5e7eb)",
          backgroundColor: "var(--color-bg-raised, #fff)",
          boxShadow: "0 8px 24px rgba(0,0,0,0.06)",
        }}
      >
        <h1 style={{ margin: "0 0 4px", fontSize: 20, fontWeight: 700 }}>Pebble</h1>
        <p style={{ margin: "0 0 20px", fontSize: 13, color: "var(--color-text-secondary, #666)" }}>
          {t("login.signIn", "Sign in to access your mail")}
        </p>

        <label
          htmlFor="pebble-password"
          style={{ display: "block", marginBottom: 6, fontSize: 13, fontWeight: 600 }}
        >
          {t("login.password", "Password")}
        </label>
        <input
          id="pebble-password"
          type="password"
          autoFocus
          value={password}
          onChange={(e) => setPassword(e.target.value)}
          placeholder="••••••••"
          style={{
            width: "100%",
            boxSizing: "border-box",
            padding: "10px 12px",
            borderRadius: 8,
            border: "1px solid var(--color-border, #d1d5db)",
            fontSize: 14,
            backgroundColor: "var(--color-bg, #fff)",
            color: "var(--color-text-primary, #111)",
          }}
        />

        {error && (
          <p
            role="alert"
            style={{ margin: "10px 0 0", fontSize: 13, color: "var(--color-danger, #dc2626)" }}
          >
            {error}
          </p>
        )}

        <button
          type="submit"
          disabled={submitting || !password}
          style={{
            marginTop: 20,
            width: "100%",
            padding: "10px 12px",
            borderRadius: 8,
            border: "none",
            fontSize: 14,
            fontWeight: 600,
            cursor: "pointer",
            color: "#fff",
            backgroundColor: "var(--color-accent, #2563eb)",
            opacity: submitting ? 0.7 : 1,
          }}
        >
          {submitting
            ? t("login.signingIn", "Signing in…")
            : t("login.submit", "Sign in")}
        </button>
      </form>
    </div>
  );
}