import { API_BASE, authHeaders, webHttpError } from "./http";

export interface ImportedBackgroundImageWeb {
  path: string;
  filename: string;
  size: number;
}

export async function importBackgroundImageWeb(
  filename: string,
  bytes: number[],
): Promise<ImportedBackgroundImageWeb> {
  const form = new FormData();
  form.append("file", new Blob([Uint8Array.from(bytes)]), filename || "background");
  const response = await fetch(`${API_BASE}/background-images/import`, {
    method: "POST",
    headers: authHeaders(false),
    body: form,
  });
  if (!response.ok) throw await webHttpError(response);
  return (await response.json()) as ImportedBackgroundImageWeb;
}
