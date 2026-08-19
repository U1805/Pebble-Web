/**
 * Web 会话（token）管理。
 *
 * Web 端通过 `POST /api/v1/command/{command}` 调用后端，携带 Bearer token。
 * token 由 login 命令签发，存 localStorage（key 与参考仓库 B 一致）。
 * 登录页与会话状态在阶段七完善；此处先提供读写原语。
 */
const TOKEN_KEY = "pebble_token";

export function getWebToken(): string | null {
  return localStorage.getItem(TOKEN_KEY);
}

export function setWebToken(token: string): void {
  localStorage.setItem(TOKEN_KEY, token);
}

export function clearWebToken(): void {
  localStorage.removeItem(TOKEN_KEY);
}