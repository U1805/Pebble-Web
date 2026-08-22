const TOKEN_KEY = "pebble_token";
export const WEB_SESSION_EXPIRED_EVENT = "pebble:web-session-expired";

export function getWebToken(): string | null {
  return localStorage.getItem(TOKEN_KEY);
}

export function setWebToken(token: string): void {
  localStorage.setItem(TOKEN_KEY, token);
}

export function clearWebToken(): void {
  localStorage.removeItem(TOKEN_KEY);
}

export function expireWebSession(): void {
  clearWebToken();
  window.dispatchEvent(new Event(WEB_SESSION_EXPIRED_EVENT));
}
