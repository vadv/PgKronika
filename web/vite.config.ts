import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

export default defineConfig({
  plugins: [react()],
  base: "./",
  build: {
    outDir: "../bins/pg_kronika-web/static",
    emptyOutDir: true,
    sourcemap: true,
  },
  server: {
    proxy: { "/v1": "http://127.0.0.1:8080" },
  },
});
