import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

// In dev, proxy the API to a locally running `drawer serve`.
export default defineConfig({
  plugins: [react()],
  server: {
    proxy: {
      "/drawer/": "http://127.0.0.1:8787",
      "/auth/": "http://127.0.0.1:8787",
      "/mcp": "http://127.0.0.1:8787",
      "/s/": "http://127.0.0.1:8787",
      "/u/": "http://127.0.0.1:8787",
      "/drop/": "http://127.0.0.1:8787",
    },
  },
});
