import js from "@eslint/js";
import formatjs from "eslint-plugin-formatjs";
import jsxA11y from "eslint-plugin-jsx-a11y";
import reactHooks from "eslint-plugin-react-hooks";
import globals from "globals";
import tseslint from "typescript-eslint";

import { comments } from "./eslint-rules/comments.js";

const BROWSER_GLOBALS = [
  "window",
  "document",
  "localStorage",
  "sessionStorage",
  "location",
  "navigator",
  "Notification",
  "IntersectionObserver",
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
      ".playwright-browsers/**",
      "src/routeTree.gen.ts",
      "src/api/schema.d.ts",
      "src/wasm/pkg/**",
      // Linted on purpose by scripts/eslint-fixtures.test.ts.
      "eslint-fixtures/**",
    ],
  },
  {
    linterOptions: {
      noInlineConfig: true,
      reportUnusedDisableDirectives: "error",
    },
  },
  {
    files: ["**/*.{ts,tsx,js,mjs}"],
    plugins: { saka: { rules: { comments } } },
    rules: { "saka/comments": "error" },
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
      // A scrollable log must take keyboard focus so it can be scrolled without a pointer.
      "jsx-a11y/no-noninteractive-tabindex": [
        "error",
        { tags: [], roles: ["tabpanel", "log"] },
      ],
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
    files: ["src/app.tsx"],
    // Module augmentation of the router's Register type needs an interface.
    rules: { "@typescript-eslint/consistent-type-definitions": "off" },
  },
  {
    files: ["eslint-fixtures/**", "**/*.js", "**/*.mjs"],
    ...tseslint.configs.disableTypeChecked,
  },
  {
    files: ["**/*.js", "**/*.mjs"],
    languageOptions: { globals: globals.node },
  },
  {
    files: ["public/sw.js"],
    languageOptions: { globals: globals.serviceworker },
  },
);
