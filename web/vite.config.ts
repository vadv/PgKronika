import { defineConfig, loadEnv } from "vite";
import react from "@vitejs/plugin-react";

export default defineConfig(({ mode }) => {
  const env = loadEnv(mode, ".", "PGK_");
  return {
    plugins: [react()],
    base: "./",
    build: {
      outDir: "../bins/pg_kronika-web/static",
      emptyOutDir: true,
      sourcemap: true,
    },
    server: {
      proxy: {
        "/v1": env.PGK_API_TARGET ?? "http://127.0.0.1:8080",
      },
    },
  };
});
