export interface InvokeArgs {
  [key: string]: unknown;
}

function camelToSnake(key: string): string {
  return key.replace(/[A-Z]/g, (c) => `_${c.toLowerCase()}`);
}

/**
 * Mirror Tauri's command boundary: only top-level JavaScript argument names
 * are mapped from camelCase to Rust snake_case. Nested DTOs are left intact
 * and are decoded by their own Serde rules.
 */
export function normalizeBackendArgs(_command: string, args?: InvokeArgs): unknown {
  if (!args || typeof args !== "object" || Array.isArray(args)) return args ?? {};
  const out: Record<string, unknown> = {};
  for (const [key, value] of Object.entries(args)) {
    out[camelToSnake(key)] = value;
  }
  return out;
}
