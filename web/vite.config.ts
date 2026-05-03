import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import path from "node:path";

// During `vite dev`, /api and /ws are forwarded to the bifrost-server
// running on 127.0.0.1:8080. In production, the same host serves both
// the static SPA and the API on the same port — so no proxy is needed.
const BACKEND = process.env.BIFROST_BACKEND ?? "http://127.0.0.1:8080";

export default defineConfig({
  plugins: [react()],
  resolve: {
    alias: { "@": path.resolve(__dirname, "./src") },
  },
  server: {
    host: "127.0.0.1",
    port: 5173,
    proxy: {
      "/api": { target: BACKEND, changeOrigin: false },
      "/ws": { target: BACKEND, ws: true, changeOrigin: false },
    },
  },
  build: {
    outDir: "dist",
    sourcemap: true,
  },
});
