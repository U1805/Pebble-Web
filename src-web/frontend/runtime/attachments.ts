import { API_BASE, authHeaders, webHttpError } from "./http";
import { WEB_ATTACHMENT_DOWNLOAD_PROGRESS_EVENT } from "./local-events";

export { WEB_ATTACHMENT_DOWNLOAD_PROGRESS_EVENT } from "./local-events";

export async function downloadAttachmentWeb(
  attachmentId: string,
  saveTo: string,
): Promise<string> {
  const response = await fetch(
    `${API_BASE}/attachments/${encodeURIComponent(attachmentId)}/download`,
    { headers: authHeaders(false) },
  );
  if (!response.ok) throw await webHttpError(response);

  const requestedFilename = downloadFilenameFromSaveTo(saveTo);
  const responseFilename =
    parseFilenameFromDisposition(response.headers.get("Content-Disposition") ?? "") ?? "attachment";
  const filename = requestedFilename || responseFilename;
  const blob = await readAttachmentDownloadBlob(response, attachmentId);
  const url = URL.createObjectURL(blob);
  try {
    const anchor = document.createElement("a");
    anchor.href = url;
    anchor.download = filename;
    document.body.appendChild(anchor);
    anchor.click();
    anchor.remove();
  } finally {
    setTimeout(() => URL.revokeObjectURL(url), 1000);
  }
  return filename;
}

export async function stageComposeAttachmentWeb(
  filename: string,
  bytes: number[],
): Promise<string> {
  const form = new FormData();
  form.append("file", new Blob([Uint8Array.from(bytes)]), filename);
  const response = await fetch(`${API_BASE}/attachments/stage`, {
    method: "POST",
    headers: authHeaders(false),
    body: form,
  });
  if (!response.ok) throw await webHttpError(response);
  const stagedPath = await response.json();
  if (typeof stagedPath !== "string") {
    throw new Error("Attachment upload returned an invalid staged path");
  }
  return stagedPath;
}

function downloadFilenameFromSaveTo(saveTo: string): string {
  const normalized = saveTo.replace(/\\/g, "/");
  const filename = normalized.split("/").pop() ?? "";
  if (!filename) {
    throw new Error("Invalid download_attachment args: saveTo must include a filename");
  }
  return filename;
}

async function readAttachmentDownloadBlob(
  response: Response,
  attachmentId: string,
): Promise<Blob> {
  const totalBytes = Number(response.headers.get("Content-Length") ?? 0) || 0;
  if (!response.body) return response.blob();

  const reader = response.body.getReader();
  const chunks: ArrayBuffer[] = [];
  let bytesCopied = 0;
  while (true) {
    const { done, value } = await reader.read();
    if (done) break;
    if (!value) continue;
    const chunk = new Uint8Array(value.byteLength);
    chunk.set(value);
    chunks.push(chunk.buffer);
    bytesCopied += value.byteLength;
    emitProgress(attachmentId, bytesCopied, totalBytes);
  }
  return new Blob(chunks, { type: response.headers.get("Content-Type") ?? undefined });
}

function emitProgress(attachmentId: string, bytesCopied: number, totalBytes: number): void {
  window.dispatchEvent(
    new CustomEvent(WEB_ATTACHMENT_DOWNLOAD_PROGRESS_EVENT, {
      detail: {
        attachment_id: attachmentId,
        bytes_copied: bytesCopied,
        total_bytes: totalBytes,
      },
    }),
  );
}

function parseFilenameFromDisposition(disposition: string): string | null {
  const encoded = /filename\*=UTF-8''([^;]+)/i.exec(disposition);
  if (encoded) {
    try {
      return decodeURIComponent(encoded[1]);
    } catch {
      // Fall back to the ASCII filename below.
    }
  }
  const match = /filename="?([^";]+)"?/i.exec(disposition);
  return match ? match[1] : null;
}
