// Lint config for the UI. Flat config, one entry per file group: the app and its tests share
// the TypeScript setup, and react-hooks guards the component code.
//
// Only the core react-hooks rules are enabled (rules-of-hooks, exhaustive-deps). The plugin's
// `recommended` preset also turns on the React Compiler rules (refs, purity, set-state-in-effect,
// preserve-manual-memoization); the compiler is not wired into the build, so those rules report
// "Compilation Skipped" against hand-written memoization and would only be satisfiable by
// restructuring the components around a compiler this project does not run.
import js from "@eslint/js";
import reactHooks from "eslint-plugin-react-hooks";
import tseslint from "typescript-eslint";

export default tseslint.config(
  { ignores: ["dist", "node_modules"] },
  js.configs.recommended,
  tseslint.configs.recommended,
  {
    files: ["**/*.{ts,tsx}"],
    plugins: { "react-hooks": reactHooks },
    rules: {
      "react-hooks/rules-of-hooks": "error",
      "react-hooks/exhaustive-deps": "warn",
    },
  },
);
