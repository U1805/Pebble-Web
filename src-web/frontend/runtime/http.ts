import { normalizeBackendArgs, type InvokeArgs } from "./args";
import { expireWebSession, getWebToken } from "./session";

export const API_BASE = "/api/v1";

export function authHeaders(contentType = true): Record<string, string> {
  const token = getWebToken();
  return {
    ...(contentType ? { "Content-Type": "application/json" } : {}),
    ...(token ? { Authorization: `Bearer ${token}` } : {}),
  };
}

export async function webHttpError(
  response: Response,
): Promise<Error & { status?: number; code?: string }> {
  if (response.status === 401) expireWebSession();

  let message = `HTTP ${response.status}`;
  let code: string | undefined;
  try {
    const body = (await response.json()) as { error?: { code?: string; message?: string } };
    message = body.error?.message ?? message;
    code = body.error?.code;
  } catch {
    // Keep the HTTP fallback for non-JSON responses.
  }

  const error = new Error(message) as Error & { status?: number; code?: string };
  error.status = response.status;
  if (code) error.code = code;
  return error;
}

export async function postCommand<T>(command: string, args?: InvokeArgs): Promise<T> {
  const response = await fetch(`${API_BASE}/command/${encodeURIComponent(command)}`, {
    method: "POST",
    headers: authHeaders(),
    body: JSON.stringify(normalizeBackendArgs(command, args)),
  });
  if (!response.ok) throw await webHttpError(response);
  return (await response.json()) as T;
}
