// Expands split global.css imports for Vitest tests that assert on its complete content.
// @ts-expect-error - Vitest runs in Node, but this frontend tsconfig has no Node type declarations.
import { readFileSync } from "fs";
// @ts-expect-error - Vitest runs in Node, but this frontend tsconfig has no Node type declarations.
import { dirname, join } from "path";

export function readGlobalCss(
  entryPath: string = "src/styles/global.css",
): string {
  const entryContent = readFileSync(entryPath, "utf8");
  const entryDirectory = dirname(entryPath);
  const parts = entryContent.split(/(\r?\n)/);

  for (let index = 0; index < parts.length; index += 2) {
    const line = parts[index];
    if (!/^\s*@import\b/.test(line)) continue;

    const importMatch = line.match(/^\s*@import\s+["']([^"']+)["'];\s*$/);
    const importPath = importMatch?.[1] ?? line.trim();

    if (!importMatch || !/^\.\/[^/\\]+\.css$/.test(importPath)) {
      throw new Error(
        `Unsupported CSS import "${importPath}" in entry file "${entryPath}"; expected a same-directory ./filename.css path.`,
      );
    }

    const leafPath = join(entryDirectory, importPath.slice(2));
    let leafContent: string;

    try {
      leafContent = readFileSync(leafPath, "utf8");
    } catch (error) {
      const detail = error instanceof Error ? `: ${error.message}` : "";
      throw new Error(
        `Failed to read CSS import "${importPath}" from entry file "${entryPath}"${detail}`,
      );
    }

    if (leafContent.split(/\r?\n/).some((line) => /^\s*@import\b/.test(line))) {
      throw new Error(
        `Unexpected nested @import in leaf file "${leafPath}"; leaf CSS files may not themselves use @import.`,
      );
    }

    parts[index] = leafContent.replace(/(?:\r?\n)+$/, "");
  }

  return parts.join("");
}
