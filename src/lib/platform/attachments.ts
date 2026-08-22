import { sanitizeFilename } from "@/lib/sanitizeFilename";
import { platform } from "./invoke";

/** Resolve the path argument while keeping Tauri filesystem details out of shared UI. */
export async function getAttachmentSavePath(filename: string): Promise<string> {
  if (platform === "web") return "";

  const safeName = sanitizeFilename(filename);
  const { downloadDir } = await import("@tauri-apps/api/path");
  return `${await downloadDir()}/${safeName}`;
}
