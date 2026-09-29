import { test } from "node:test";
import assert from "node:assert/strict";
import { createHash, hkdfSync } from "node:crypto";
import { readFileSync } from "node:fs";

const fixtures = JSON.parse(
  readFileSync(new URL("../fixtures/connect-kdf-v1.json", import.meta.url), "utf8"),
);
const HEX64 = /^[0-9a-f]{64}$/;

test("connect-kdf-v1 fixtures are structurally valid", () => {
  assert.ok(Array.isArray(fixtures), "fixture root must be an array");
  assert.ok(fixtures.length >= 3, "at least three connect-KDF vectors are required");

  const names = new Set();
  for (const fixture of fixtures) {
    assert.equal(typeof fixture.name, "string", "fixture name must be a string");
    assert.ok(!names.has(fixture.name), `duplicate fixture name: ${fixture.name}`);
    names.add(fixture.name);
    assert.match(fixture.pairing_token_hex, HEX64, `${fixture.name}: pairing_token_hex`);
    assert.equal(
      fixture.pairing_token_hex,
      fixture.pairing_token_hex.toLowerCase(),
      `${fixture.name}: pairing_token_hex must be lowercase`,
    );
    assert.equal(typeof fixture.expect, "object", `${fixture.name}: expect`);
    assert.match(fixture.expect.connect_token_hex, HEX64, `${fixture.name}: connect_token_hex`);
    assert.match(fixture.expect.token_hash_hex, HEX64, `${fixture.name}: token_hash_hex`);
  }
});

test("connect-kdf-v1 vectors match §9.5 ASCII-hex HKDF and token hash", () => {
  for (const fixture of fixtures) {
    const connectTokenHex = Buffer.from(hkdfSync(
      "sha256",
      Buffer.from(fixture.pairing_token_hex, "ascii"),
      Buffer.alloc(0),
      Buffer.from("agentloom-rc-connect-v1", "ascii"),
      32,
    )).toString("hex");
    const tokenHashHex = createHash("sha256")
      .update(connectTokenHex, "ascii")
      .digest("hex");

    assert.equal(connectTokenHex, fixture.expect.connect_token_hex, fixture.name);
    assert.equal(tokenHashHex, fixture.expect.token_hash_hex, fixture.name);
  }
});
