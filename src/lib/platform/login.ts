/**
 * Web 登录：POST /api/v1/command/login（公开命令，无需 token）。
 * 成功后将 token 存入会话，供后续受保护命令与 WS 鉴权使用。
 */
import { invokeWeb } from "./web";
import { setWebToken } from "./session";

export async function loginWeb(password: string): Promise<void> {
  const res = await invokeWeb<{ token: string }>("login", { password });
  setWebToken(res.token);
}