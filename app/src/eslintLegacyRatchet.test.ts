// @ts-expect-error - Vitest runs in Node, but this frontend tsconfig has no Node type declarations.
import { existsSync, readFileSync, readdirSync } from "fs";
import { describe, expect, it } from "vitest";
import { ESLint } from "eslint";
import tseslint from "typescript-eslint";
// @ts-expect-error - eslint.config.mjs is an ESLint config file, not part of the app's typed surface.
import { LEGACY_LONG_FUNCTION_FILES } from "../eslint.config.mjs";

// This is a ratchet, not a snapshot: the legacy list may only shrink. Every
// entry must (a) exist and (b) still genuinely violate max-lines-per-function
// once the blanket exemption is lifted. A file that no longer violates must
// be removed from the list, not left in as dead weight.
function forceRuleOnLinter() {
  return new ESLint({
    overrideConfigFile: true,
    overrideConfig: [
      {
        files: ["**/*.ts", "**/*.tsx"],
        languageOptions: { parser: tseslint.parser },
        rules: {
          "max-lines-per-function": [
            "error",
            {
              max: 150,
              skipBlankLines: true,
              skipComments: true,
              IIFEs: true,
            },
          ],
        },
      },
    ],
  });
}

type DirectoryEntry = {
  name: string;
  isDirectory: () => boolean;
  isFile: () => boolean;
};

const LINTED_EXTENSIONS = [".ts", ".tsx", ".js", ".jsx", ".mts"];
const TEST_FILE_PATTERN = /\.(test|spec)\.[^./]+$/;

// Mirrors eslint.config.mjs's own file scope (minus test/spec files), so this
// check covers exactly what the max-lines-per-function rule is supposed to
// see.
function findLintedSourceFiles(
  directory: string,
  relativeDirectory = "",
): string[] {
  const entries = readdirSync(directory, {
    withFileTypes: true,
  }) as DirectoryEntry[];
  return entries.flatMap((entry) => {
    const relativePath = `${relativeDirectory}${entry.name}`;
    if (entry.isDirectory()) {
      return findLintedSourceFiles(
        `${directory}/${entry.name}`,
        `${relativePath}/`,
      );
    }
    if (!entry.isFile() || TEST_FILE_PATTERN.test(entry.name)) return [];
    return LINTED_EXTENSIONS.some((ext) => entry.name.endsWith(ext))
      ? [`src/${relativePath}`]
      : [];
  });
}

describe("eslint legacy long-function ratchet", () => {
  it("has no duplicate entries", () => {
    expect(new Set(LEGACY_LONG_FUNCTION_FILES).size).toBe(
      LEGACY_LONG_FUNCTION_FILES.length,
    );
  });

  it("is sorted alphabetically", () => {
    const sorted = [...LEGACY_LONG_FUNCTION_FILES].sort();
    expect(LEGACY_LONG_FUNCTION_FILES).toEqual(sorted);
  });

  it("only lists files that exist", () => {
    for (const file of LEGACY_LONG_FUNCTION_FILES) {
      expect(existsSync(file), file).toBe(true);
    }
  });

  it("only lists files that still violate max-lines-per-function", async () => {
    const linter = forceRuleOnLinter();
    const results = await linter.lintFiles(LEGACY_LONG_FUNCTION_FILES);
    for (const file of LEGACY_LONG_FUNCTION_FILES) {
      const result = results.find((r) => r.filePath.endsWith(file));
      const violations = (result?.messages ?? []).filter(
        (m) => m.ruleId === "max-lines-per-function",
      );
      expect(
        violations.length,
        `${file} should still violate the rule`,
      ).toBeGreaterThan(0);
    }
  }, 30000);

  it("disable directives for max-lines-per-function are banned outside test files", () => {
    // Covers both eslint-disable* comments and bare `/* eslint <rule>: ... */`
    // inline-config comments -- either one could otherwise silence or
    // reconfigure max-lines-per-function from inside a source file. (The
    // config also sets noInlineConfig, which is the enforcement; this test
    // is the tripwire that catches the attempt in review / CI diffs.)
    const targetsFunctionLengthPattern = /eslint[^\n]*max-lines-per-function/;
    const offenders: string[] = [];
    for (const file of findLintedSourceFiles("src")) {
      const lines = readFileSync(file, "utf-8").split("\n");
      lines.forEach((line: string, index: number) => {
        if (!line.includes("eslint")) return;
        const isDisableDirective = line.includes("eslint-disable");
        // Everything after the disable keyword, with a trailing block-comment
        // close stripped. Empty means a bare disable (silences every rule);
        // non-empty means specific rule names were listed.
        const afterKeyword = isDisableDirective
          ? line
              .replace(/^.*eslint-disable(-next-line|-line)?/, "")
              .replace(/\*\/\s*$/, "")
              .trim()
          : "";
        const isBareDisable = isDisableDirective && afterKeyword.length === 0;
        const targetsFunctionLength = targetsFunctionLengthPattern.test(line);
        if (isBareDisable || targetsFunctionLength) {
          offenders.push(`${file}:${index + 1}: ${line.trim()}`);
        }
      });
    }
    expect(offenders, offenders.join("\n")).toEqual([]);
  });
});
