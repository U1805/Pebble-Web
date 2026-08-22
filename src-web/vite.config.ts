import path from "path";
import { defineConfig, mergeConfig, type Plugin } from "vite";
import baseConfig from "../vite.config";

const webPlatformDir = path.join(__dirname, "frontend");

const webEntryPlugin: Plugin = {
  name: "pebble-web-platform-entry",
  enforce: "pre",
  resolveId(source, importer) {
    if (source !== "./App" || !importer) return null;
    const normalizedImporter = importer.split(path.sep).join("/");
    if (!normalizedImporter.endsWith("/src/main.tsx")) return null;
    return path.join(webPlatformDir, "ui", "WebApp.tsx");
  },
};

export default defineConfig(
  mergeConfig(baseConfig, {
    plugins: [webEntryPlugin],
    resolve: {
      alias: [
        {
          find: "@tauri-apps/api/core",
          replacement: path.join(webPlatformDir, "tauri", "core.ts"),
        },
        {
          find: "@tauri-apps/api/event",
          replacement: path.join(webPlatformDir, "tauri", "event.ts"),
        },
        {
          find: "@tauri-apps/api/app",
          replacement: path.join(webPlatformDir, "tauri", "app.ts"),
        },
        {
          find: "@tauri-apps/api/path",
          replacement: path.join(webPlatformDir, "tauri", "path.ts"),
        },
        {
          find: "@tauri-apps/api/window",
          replacement: path.join(webPlatformDir, "tauri", "window.ts"),
        },
      ],
    },
  }),
);
