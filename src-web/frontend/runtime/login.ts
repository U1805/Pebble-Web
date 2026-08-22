import { invokeWeb } from "./invoke";
import { setWebToken } from "./session";

export async function loginWeb(password: string): Promise<void> {
  const result = await invokeWeb<{ token: string }>("login", { password });
  setWebToken(result.token);
}
