// Verifies the Node-only global.css import expansion used by content assertion tests.
// @ts-expect-error - Vitest runs in Node, but this frontend tsconfig has no Node type declarations.
import { mkdtempSync, rmSync, writeFileSync } from "fs";
// @ts-expect-error - Vitest runs in Node, but this frontend tsconfig has no Node type declarations.
import { tmpdir } from "os";
// @ts-expect-error - Vitest runs in Node, but this frontend tsconfig has no Node type declarations.
import { join } from "path";
import { afterEach, describe, expect, it } from "vitest";
import { readGlobalCss } from "./readGlobalCss";

describe("readGlobalCss", () => {
  const temporaryDirectories: string[] = [];

  afterEach(() => {
    for (const directory of temporaryDirectories.splice(0)) {
      rmSync(directory, { recursive: true, force: true });
    }
  });

  function makeTemporaryDirectory(): string {
    const directory = mkdtempSync(join(tmpdir(), "read-global-css-"));
    temporaryDirectories.push(directory);
    return directory;
  }

  it("expands same-directory imports in place and preserves ordinary lines", () => {
    const directory = makeTemporaryDirectory();
    const entryPath = join(directory, "global.css");
    writeFileSync(join(directory, "a.css"), ".a { color: red; }\n\n");
    writeFileSync(join(directory, "b.css"), ".b { color: blue; }\n");
    writeFileSync(join(directory, "c.css"), ".c { color: green; }");
    writeFileSync(
      entryPath,
      '/* before */\n@import "./a.css";\n.middle { display: block; }\n@import "./b.css";\n@import "./c.css";\n/* after */\n',
    );

    expect(readGlobalCss(entryPath)).toBe(
      "/* before */\n.a { color: red; }\n.middle { display: block; }\n.b { color: blue; }\n.c { color: green; }\n/* after */\n",
    );
  });

  it("throws when an imported file does not exist", () => {
    const directory = makeTemporaryDirectory();
    const entryPath = join(directory, "global.css");
    writeFileSync(entryPath, '@import "./missing.css";\n');

    expect(() => readGlobalCss(entryPath)).toThrow();
  });

  it("throws when an imported leaf file contains a nested import", () => {
    const directory = makeTemporaryDirectory();
    const entryPath = join(directory, "global.css");
    writeFileSync(
      join(directory, "leaf.css"),
      '.leaf { color: red; }\n@import "./nested.css";\n',
    );
    writeFileSync(entryPath, '@import "./leaf.css";\n');

    expect(() => readGlobalCss(entryPath)).toThrow(/leaf\.css/);
  });

  it("keeps imported content in entry-file order", () => {
    const directory = makeTemporaryDirectory();
    const firstEntryPath = join(directory, "global-ab.css");
    const secondEntryPath = join(directory, "global-ba.css");
    const aContent = ".order-a { color: red; }";
    const bContent = ".order-b { color: blue; }";
    writeFileSync(join(directory, "a.css"), aContent);
    writeFileSync(join(directory, "b.css"), bContent);
    writeFileSync(firstEntryPath, '@import "./a.css";\n@import "./b.css";\n');
    writeFileSync(secondEntryPath, '@import "./b.css";\n@import "./a.css";\n');

    const firstResult = readGlobalCss(firstEntryPath);
    const secondResult = readGlobalCss(secondEntryPath);

    expect(firstResult).not.toBe(secondResult);
    expect(firstResult.indexOf(aContent)).toBeLessThan(
      firstResult.indexOf(bContent),
    );
    expect(secondResult.indexOf(aContent)).toBeGreaterThan(
      secondResult.indexOf(bContent),
    );
  });
});
