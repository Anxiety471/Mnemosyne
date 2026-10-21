import { test, expect } from "@playwright/test";

/** Each flow gets a folder of its own: `/e2e/<name>-<run stamp>`. */
export function flowFolder(name: string): string {
  return `/e2e/${name}-${Date.now()}`;
}
