import path from "path";
import { defineConfig, mergeConfig, type Plugin } from "vite";
import baseConfig from "../vite.config";
import { sidebarEmptyAccountsPatch } from "./patch/sidebar_empty_accounts";

const webPlatformDir = path.join(__dirname, "frontend");
const webFavicon =
  "data:image/svg+xml,%3Csvg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 64 64'%3E%3Cpath fill='%23d4714e' d='M32 8C18 8 8 20 10 34c2 12 12 22 22 22s20-10 22-22C56 20 46 8 32 8z'/%3E%3C/svg%3E";

const webEntryPlugin: Plugin = {
  name: "pebble-web-platform-entry",
  enforce: "pre",
  transformIndexHtml() {
    return [
      {
        tag: "link",
        attrs: { rel: "icon", href: webFavicon, type: "image/svg+xml" },
        injectTo: "head",
      },
    ];
  },
  resolveId(source, importer) {
    if (source !== "./App" || !importer) return null;
    const normalizedImporter = importer.split(path.sep).join("/");
    if (!normalizedImporter.endsWith("/src/main.tsx")) return null;
    return path.join(webPlatformDir, "ui", "WebApp.tsx");
  },
};

export default defineConfig(
  mergeConfig(baseConfig, {
    plugins: [webEntryPlugin, sidebarEmptyAccountsPatch()],
    resolve: {
      alias: [
        {
          find: "@/lib/i18n",
          replacement: path.join(webPlatformDir, "i18n.ts"),
        },
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
