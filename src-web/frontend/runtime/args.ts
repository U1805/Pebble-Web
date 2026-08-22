export interface InvokeArgs {
  [key: string]: unknown;
}

function camelToSnake(key: string): string {
  return key.replace(/[A-Z]/g, (c) => `_${c.toLowerCase()}`);
}

function deepSnakeKeys(value: unknown): unknown {
  if (Array.isArray(value)) return value.map(deepSnakeKeys);
  if (typeof value === "object" && value !== null) {
    const out: Record<string, unknown> = {};
    for (const [key, child] of Object.entries(value)) {
      out[camelToSnake(key)] = deepSnakeKeys(child);
    }
    return out;
  }
  return value;
}

/** Mirror Tauri's camelCase JavaScript argument to snake_case Rust boundary. */
export function normalizeBackendArgs(_command: string, args?: InvokeArgs): unknown {
  if (!args || typeof args !== "object" || Array.isArray(args)) return args ?? {};
  return deepSnakeKeys(args);
}
