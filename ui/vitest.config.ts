import { defineConfig } from "vitest/config";

// Component tests render in jsdom; the checksum test asks for the node environment in a
// docblock, because it compares against Node's own crypto implementation. Vite transforms
// the JSX, so the dev-server proxy in vite.config.ts stays out of the test run.
export default defineConfig({
  test: {
    environment: "jsdom",
    include: ["tests/**/*.test.{ts,tsx}"],
    setupFiles: ["tests/setup.ts"],
  },
});
