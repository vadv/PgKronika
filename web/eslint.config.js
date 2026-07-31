import reactHooks from "eslint-plugin-react-hooks";
import globals from "globals";
import tseslint from "typescript-eslint";

export default tseslint.config(
  { ignores: ["dist/", "node_modules/", "../bins/**"] },
  ...tseslint.configs.recommended,
  ...tseslint.configs.strict,
  reactHooks.configs["recommended-latest"],
  {
    languageOptions: {
      globals: globals.browser,
    },
  },
);
