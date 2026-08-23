/**
 * Locale entries used by the automatic WebDAV backup UI added for upstream
 * issue #51 but missing from both upstream locale files.
 */
export const cloudSyncLocalePatch = {
  en: {
    autoBackupConfigSaveFailed:
      "Failed to save automatic backup configuration: {{error}}",
    autoBackupConfigSaved: "Automatic backup configuration saved",
    autoBackupCredentialsRequired:
      "Enter a WebDAV URL, username, and password before enabling automatic backup.",
    autoBackupSecretPassphraseRequired:
      "Enter an encryption password before including secrets.",
    saveAutoBackupConfig: "Save Auto-Backup Configuration",
  },
  zh: {
    autoBackupConfigSaveFailed: "自动备份配置保存失败：{{error}}",
    autoBackupConfigSaved: "自动备份配置已保存",
    autoBackupCredentialsRequired:
      "启用自动备份前，请输入 WebDAV 地址、用户名和密码。",
    autoBackupSecretPassphraseRequired:
      "包含私密数据前，请输入备份加密密码。",
    saveAutoBackupConfig: "保存自动备份配置",
  },
} as const;
