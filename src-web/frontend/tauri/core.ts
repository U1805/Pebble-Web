import { invokeWeb, type InvokeArgs } from "../runtime/invoke";

export function invoke<T>(command: string, args?: InvokeArgs): Promise<T> {
  return invokeWeb<T>(command, args);
}

export function convertFileSrc(path: string): string {
  return path;
}
