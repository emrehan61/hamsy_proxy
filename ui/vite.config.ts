import { defineConfig } from "vite";
import solid from "vite-plugin-solid";

export default defineConfig({
  plugins: [solid()],
  server: {
    port: 5173,
    proxy: {
      "/api": { target: "http://127.0.0.1:9081", changeOrigin: true, ws: true },
      "/cert": { target: "http://127.0.0.1:9081", changeOrigin: true },
    },
  },
  build: {
    outDir: "dist",
    target: "es2022",
    sourcemap: false,
    chunkSizeWarningLimit: 1500,
  },
});
