import { normalizeBackendArgs, type InvokeArgs } from "./args";
import { API_BASE, authHeaders, webHttpError } from "./http";

const OAUTH_CALLBACK_TIMEOUT_MS = 5 * 60 * 1000;

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
    const oauthState = new URL(body.authorization_url).searchParams.get("state");
    if (!oauthState) {
      throw new Error("OAuth server did not return an authorization state");
    }
    popup.location.href = body.authorization_url;
    return waitForOAuthPopup(popup, oauthState);
  } catch (error) {
    popup.close();
    throw error;
  }
}

type OAuthFlowStatus =
  | { status: "pending" | "processing" | "expired" }
  | { status: "success"; account?: unknown }
  | { status: "error"; message?: string };

async function getOAuthFlowStatus(
  state: string,
  cancelPending = false,
): Promise<OAuthFlowStatus> {
  const response = await fetch(`${API_BASE}/oauth/callback-status`, {
    method: "POST",
    headers: authHeaders(),
    body: JSON.stringify({ state, cancel_pending: cancelPending }),
  });
  if (!response.ok) throw await webHttpError(response);
  return (await response.json()) as OAuthFlowStatus;
}

function waitForOAuthPopup(popup: Window, oauthState: string): Promise<unknown> {
  return new Promise((resolve, reject) => {
    let settled = false;
    let timeoutId = 0;
    let resultPoll: number | null = null;
    let closeCheckInFlight = false;

    const cleanup = () => {
      window.removeEventListener("message", onMessage);
      window.clearInterval(closePoll);
      window.clearTimeout(timeoutId);
      if (resultPoll !== null) window.clearInterval(resultPoll);
    };
    const finish = (callback: () => void) => {
      if (settled) return;
      settled = true;
      cleanup();
      callback();
    };
    const handleStatus = (status: OAuthFlowStatus, pendingOutcome: "cancel" | "timeout") => {
      if (settled) return;
      if (status.status === "success") {
        if (status.account === undefined) {
          finish(() => reject(new Error("OAuth server returned success without an account")));
        } else {
          finish(() => resolve(status.account));
        }
        return;
      }
      if (status.status === "error") {
        finish(() => reject(new Error(status.message || "OAuth authorization failed")));
        return;
      }
      if (status.status === "processing") {
        startResultPolling();
        return;
      }
      if (pendingOutcome === "timeout") {
        popup.close();
        finish(() => reject(new Error("OAuth authorization timed out")));
      } else {
        finish(() => reject(new Error("OAuth authorization was cancelled")));
      }
    };
    const checkStatus = async (pendingOutcome: "cancel" | "timeout") => {
      try {
        handleStatus(await getOAuthFlowStatus(oauthState, true), pendingOutcome);
      } catch (error) {
        if (settled) return;
        popup.close();
        finish(() => reject(error instanceof Error ? error : new Error(String(error))));
      }
    };
    const startResultPolling = () => {
      if (settled || resultPoll !== null) return;
      window.clearInterval(closePoll);
      window.clearTimeout(timeoutId);
      const poll = () => {
        void getOAuthFlowStatus(oauthState)
          .then((status) => {
            if (status.status === "processing") return;
            handleStatus(status, "timeout");
          })
          .catch((error) => {
            if (settled) return;
            finish(() => reject(error instanceof Error ? error : new Error(String(error))));
          });
      };
      resultPoll = window.setInterval(poll, 500);
      poll();
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
      if (!popup.closed || closeCheckInFlight || settled) return;
      closeCheckInFlight = true;
      void checkStatus("cancel").finally(() => {
        closeCheckInFlight = false;
      });
    }, 500);
    timeoutId = window.setTimeout(() => {
      void checkStatus("timeout");
    }, OAUTH_CALLBACK_TIMEOUT_MS);
    window.addEventListener("message", onMessage);
  });
}
