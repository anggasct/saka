// ESLint flat config. Feature code reaches browser and desktop APIs only through
// `src/platform/`, and shows text only through the message catalog.

import js from "@eslint/js";
import formatjs from "eslint-plugin-formatjs";
import jsxA11y from "eslint-plugin-jsx-a11y";
import reactHooks from "eslint-plugin-react-hooks";
import globals from "globals";
import tseslint from "typescript-eslint";

const BROWSER_GLOBALS = [
  "window",
  "document",
  "localStorage",
  "sessionStorage",
  "location",
  "navigator",
  "Notification",
].map((name) => ({
  name,
  message: "Use the platform layer (src/platform/) instead.",
}));

export default tseslint.config(
  {
    ignores: [
      "dist/**",
      "node_modules/**",
      "playwright-report/**",
      "test-results/**",
      "src/routeTree.gen.ts",
      "src/api/schema.d.ts",
      // Linted on purpose by scripts/eslint-fixtures.test.ts.
      "eslint-fixtures/**",
    ],
  },
  js.configs.recommended,
  ...tseslint.configs.strictTypeChecked,
  ...tseslint.configs.stylisticTypeChecked,
  {
    languageOptions: {
      globals: globals.browser,
      parserOptions: {
        projectService: true,
        tsconfigRootDir: import.meta.dirname,
      },
    },
    rules: {
      "@typescript-eslint/consistent-type-definitions": ["error", "type"],
      "@typescript-eslint/switch-exhaustiveness-check": "error",
      "@typescript-eslint/restrict-template-expressions": [
        "error",
        { allowNumber: true },
      ],
    },
  },
  {
    files: ["**/*.tsx"],
    plugins: { "react-hooks": reactHooks, "jsx-a11y": jsxA11y, formatjs },
    rules: {
      ...reactHooks.configs.recommended.rules,
      ...jsxA11y.flatConfigs.strict.rules,
      "formatjs/no-literal-string-in-jsx": "error",
    },
  },
  {
    files: ["src/**/*.{ts,tsx}", "eslint-fixtures/**/*.{ts,tsx}"],
    ignores: ["src/platform/**", "src/test/**"],
    rules: {
      "no-restricted-globals": ["error", ...BROWSER_GLOBALS],
      "no-restricted-imports": [
        "error",
        {
          patterns: [
            {
              group: ["@tauri-apps/*"],
              message: "Use the platform layer (src/platform/) instead.",
            },
          ],
        },
      ],
    },
  },
  {
    files: ["eslint-fixtures/**", "**/*.js", "**/*.mjs"],
    ...tseslint.configs.disableTypeChecked,
  },
  {
    files: ["**/*.js", "**/*.mjs"],
    languageOptions: { globals: globals.node },
  },
);
