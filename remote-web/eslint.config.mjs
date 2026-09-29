// ESLint flat config for remote-web/src, always invoked from the repo root.
//
// remote-web does not install its own eslint/typescript-eslint: this gate
// borrows app's already-installed toolchain (eslint 10 + typescript-eslint 8
// + TypeScript 5.8 parser) because typescript-eslint does not yet support
// the TypeScript 7 compiler that remote-web's own package.json pins (see
// https://github.com/typescript-eslint/typescript-eslint/issues/10940).
// max-lines-per-function is a purely syntactic rule; it needs an AST, not
// type information, so parsing remote-web's sources with app's TS 5.8
// parser is safe for this rule even though remote-web builds with TS 7.
//
// This file lives alongside remote-web/src (not at the repo root or under
// scripts/) so the config sits next to the sources it lints. `files: ["src/**"]`
// below still resolves correctly from a repo-root cwd: ESLint's base path
// is the cwd whenever --config is passed explicitly (see
// remote-web/package.json's "lint" script), not this file's own directory
// -- so the glob must be relative to that cwd. It resolves
// typescript-eslint/globals from app/node_modules via createRequire, so
// remote-web/package.json and package-lock.json stay untouched (zero new
// dependencies).
//
// react-hooks plugin is intentionally not installed; the former
// exhaustive-deps disable sites were removed rather than shimmed,
// re-evaluate them if the plugin is ever adopted.
import { createRequire } from "node:module";

const require = createRequire(new URL("../app/package.json", import.meta.url));
const tseslint = require("typescript-eslint");
const globals = require("globals");

// Legacy files that already exceed the 150-line function limit. This list is
// a ratchet: it may only shrink (a file drops off once its long functions are
// split), never grow with newly-written violations. See
// remote-web/src/eslintLegacyRatchet.test.ts for the enforcement of that
// invariant.
export const LEGACY_LONG_FUNCTION_FILES = [
  // Intentional, permanent exemption, not debt still to be paid down.
  // Already split by responsibility into six useAppRuntime* hooks and two
  // display components; what remains in AppRuntimeConnected is the shared
  // state/ref declarations those hooks all consume (one assembly
  // responsibility). Splitting further only adds an indirection hop, it
  // doesn't reduce what a reader has to hold in mind. Overall file size is
  // still governed by the repo-wide file-size gate.
  "remote-web/src/app/AppRuntime.tsx",
  "remote-web/src/app/RootRouter.tsx",
  "remote-web/src/app/pairingTransport.ts",
  "remote-web/src/ui/stream/SessionStreamContent.tsx",
];

export default tseslint.config(
  {
    ignores: ["remote-web/dist", "remote-web/node_modules"],
  },
  {
    files: ["remote-web/src/**/*.{ts,tsx,js,jsx,mts}"],
    ignores: [
      "remote-web/src/**/*.test.{ts,tsx,js,jsx,mts}",
      "remote-web/src/**/*.spec.{ts,tsx,js,jsx,mts}",
    ],
    languageOptions: {
      parser: tseslint.parser,
      globals: { ...globals.browser, ...globals.node },
    },
    plugins: {
      "@typescript-eslint": tseslint.plugin,
    },
    // Inline `/* eslint ... */` and `eslint-disable` comments could otherwise
    // turn max-lines-per-function off (or silence it) from inside a source
    // file, bypassing the legacy ratchet entirely. Ignoring all inline
    // config, and reporting every now-inert disable comment as a warning
    // (which --max-warnings 0 turns red), closes that hole.
    linterOptions: {
      noInlineConfig: true,
      reportUnusedDisableDirectives: "error",
    },
    rules: {
      "max-lines-per-function": [
        "error",
        { max: 150, skipBlankLines: true, skipComments: true, IIFEs: true },
      ],
    },
  },
  ...(LEGACY_LONG_FUNCTION_FILES.length > 0
    ? [
        {
          files: LEGACY_LONG_FUNCTION_FILES,
          rules: {
            "max-lines-per-function": "off",
          },
        },
      ]
    : []),
);
