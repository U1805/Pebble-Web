import { convertFileSrc } from "@tauri-apps/api/core";
import { invoke, platform } from "@/lib/platform";
import type { ImportedBackgroundImage } from "./ipc-types";

export async function importBackgroundImage(file: File): Promise<ImportedBackgroundImage> {
  const bytes = Array.from(new Uint8Array(await file.arrayBuffer()));
  return invoke<ImportedBackgroundImage>("import_background_image", {
    filename: file.name,
    bytes,
  });
}

export async function deleteBackgroundImage(path: string): Promise<void> {
  return invoke<void>("delete_background_image", { path });
}

export function backgroundImageUrl(path: string): string {
  if (platform === "web") return path;
  try {
    return convertFileSrc(path);
  } catch {
    return path;
  }
}
