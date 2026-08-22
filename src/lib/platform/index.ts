/**
 * 平台调用层统一出口（计划书 §14）。
 *
 * 业务代码从这里 import `invoke` / `platform` / `capabilities` / `listen`。
 */
export { invoke, platform } from "./invoke";
export type { Platform, InvokeArgs } from "./invoke";
export { capabilities } from "./capabilities";
export type { Capabilities } from "./capabilities";
export { getWebToken, setWebToken, clearWebToken } from "./session";
export { listen, EVENTS } from "./events";
export type { EventName, TauriEvent, EventOptions } from "./events";
export { getVersion } from "./version";
export { getAttachmentSavePath } from "./attachments";
