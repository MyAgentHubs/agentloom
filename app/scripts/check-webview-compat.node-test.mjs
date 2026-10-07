import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";
import test from "node:test";

test("WebView gate accepts Safari 16 APIs and rejects unsupported features", () => {
  const script = fileURLToPath(
    new URL("./check-webview-compat.sh", import.meta.url),
  );
  const result = spawnSync("bash", [script, "--self-test"], {
    encoding: "utf8",
    timeout: 60_000,
  });
  assert.ifError(result.error);
  assert.equal(result.status, 0, `${result.stdout}\n${result.stderr}`);
});
