import upstreamI18n from "../../src/lib/i18n";
import { cloudSyncLocalePatch } from "../patch/issue051_cloud_sync_locales";

const settingsUpdateFailed = {
  en: "Failed to update setting",
  zh: "更新失败",
} as const;

for (const language of ["en", "zh"] as const) {
  upstreamI18n.addResourceBundle(
    language,
    "translation",
    {
      cloudSync: cloudSyncLocalePatch[language],
      settings: { autostartFailed: settingsUpdateFailed[language] },
      web: { settingsUpdateFailed: settingsUpdateFailed[language] },
    },
    true,
    true,
  );
}

export default upstreamI18n;
