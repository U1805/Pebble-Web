import upstreamI18n from "../../src/lib/i18n";
import { cloudSyncLocalePatch } from "../patch/issue051_cloud_sync_locales";

for (const language of ["en", "zh"] as const) {
  upstreamI18n.addResourceBundle(
    language,
    "translation",
    { cloudSync: cloudSyncLocalePatch[language] },
    true,
    true,
  );
}

export default upstreamI18n;
