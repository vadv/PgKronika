import { defineConfig } from "vitest/config";
import react from "@vitejs/plugin-react";

export default defineConfig({
  plugins: [react()],
  test: {
    environment: "jsdom",
    globals: true,
    coverage: {
      provider: "v8",
      include: ["src/**/*.{ts,tsx}"],
      // Bootstrap and type-only modules have no behaviour to assert. Leaving
      // them in the denominator would buy the threshold with fake tests.
      exclude: [
        "src/main.tsx",
        "src/i18n/index.ts",
        "src/api/types.ts",
        "src/api/schema.d.ts",
      ],
      thresholds: {
        statements: 90,
        branches: 85,
        functions: 80,
        lines: 90,
      },
    },
  },
});
