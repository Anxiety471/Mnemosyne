import { defineConfig, devices } from "@playwright/test";

/**
 * Every spec drives the real UI (ui/dist) served by a real `drawer serve` process
 * on a throwaway store. `global-setup.ts` builds the UI and the binary, then
 * starts the server once for the whole run.
 */
export default defineConfig({
  testDir: "./tests",
  // One worker: the server is shared, and the flows are small. Set E2E_WORKERS
  // locally only if you know the flows are independent.
  workers: Number(process.env.E2E_WORKERS ?? 1),
  // A flow is a browser session against a real server, not a unit test.
  timeout: 60_000,
  expect: { timeout: 15_000 },
  fullyParallel: false,
  forbidOnly: !!process.env.CI,
  globalSetup: "./global-setup.ts",
  globalTeardown: "./global-teardown.ts",
  reporter: process.env.CI ? [["github"], ["html", { open: "never" }]] : [["list"], ["html", { open: "never" }]],
  use: {
    baseURL: process.env.DRAWER_BASE_URL ?? "http://127.0.0.1:8787",
    trace: "retain-on-failure",
    screenshot: "only-on-failure",
    video: "off",
    actionTimeout: 15_000,
    navigationTimeout: 15_000,
  },
  projects: [
    { name: "chromium", use: { ...devices["Desktop Chrome"] } },
  ],
});
