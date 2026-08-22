let cachedVersion: string | null = null;

export async function getVersion(): Promise<string> {
  if (cachedVersion !== null) return cachedVersion;
  const response = await fetch("/api/v1/health");
  if (!response.ok) throw new Error(`HTTP ${response.status}`);
  const body = (await response.json()) as { version?: string };
  cachedVersion = body.version ?? "";
  return cachedVersion;
}
