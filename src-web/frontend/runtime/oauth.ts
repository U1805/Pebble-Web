import { normalizeBackendArgs, type InvokeArgs } from "./args";
import { API_BASE, authHeaders, webHttpError } from "./http";

export async function completeOAuthFlowWeb(args?: InvokeArgs): Promise<unknown> {
  const popup = window.open("about:blank", "pebble-oauth", "popup,width=520,height=720");
  if (!popup) {
    throw new Error("The OAuth popup was blocked. Allow popups for this site and try again.");
  }

  try {
    const response = await fetch(`${API_BASE}/command/complete_oauth_flow`, {
      method: "POST",
      headers: authHeaders(),
      body: JSON.stringify(normalizeBackendArgs("complete_oauth_flow", args)),
    });
    if (!response.ok) throw await webHttpError(response);

    const body = (await response.json()) as { authorization_url?: string };
    if (!body.authorization_url) {
      throw new Error("OAuth server did not return an authorization URL");
    }
    popup.location.href = body.authorization_url;
  } catch (error) {
    popup.close();
    throw error;
  }

  return waitForOAuthPopup(popup);
}

function waitForOAuthPopup(popup: Window): Promise<unknown> {
  return new Promise((resolve, reject) => {
    let settled = false;
    const cleanup = () => {
      window.removeEventListener("message", onMessage);
      window.clearInterval(closePoll);
    };
    const finish = (callback: () => void) => {
      if (settled) return;
      settled = true;
      cleanup();
      callback();
    };
    const onMessage = (event: MessageEvent) => {
      if (event.source !== popup || event.origin !== window.location.origin) return;
      const data = event.data as {
        type?: string;
        status?: string;
        account?: unknown;
        message?: string;
      };
      if (data?.type !== "pebble-oauth") return;
      if (data.status === "success" && data.account) {
        finish(() => resolve(data.account));
      } else {
        finish(() => reject(new Error(data.message || "OAuth authorization failed")));
      }
    };
    const closePoll = window.setInterval(() => {
      if (popup.closed) finish(() => reject(new Error("OAuth authorization was cancelled")));
    }, 500);
    window.addEventListener("message", onMessage);
  });
}
