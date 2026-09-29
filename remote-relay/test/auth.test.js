import { test } from "node:test";
import assert from "node:assert/strict";
import { constantTimeEqualHex64, matchesOwnerCredential } from "../src/auth.js";

const OWNER_CREDENTIAL = "1".repeat(64);
const OWNER_CREDENTIAL_HASH = "3138bb9bc78df27c473ecfd1410f7bd45ebac1f59cf3ff9cfe4db77aab7aedd3";

function replaceHashByte(hash, byteIndex, replacement) {
  const offset = byteIndex * 2;
  return `${hash.slice(0, offset)}${replacement}${hash.slice(offset + 2)}`;
}

test("owner hash 首字节不匹配 = 拒", async () => {
  const mismatchedHash = replaceHashByte(OWNER_CREDENTIAL_HASH, 0, "00");
  assert.equal(await matchesOwnerCredential(OWNER_CREDENTIAL, mismatchedHash), false);
});

test("owner hash 中间字节不匹配 = 拒", async () => {
  const mismatchedHash = replaceHashByte(OWNER_CREDENTIAL_HASH, 16, "00");
  assert.equal(await matchesOwnerCredential(OWNER_CREDENTIAL, mismatchedHash), false);
});

test("owner hash 末字节不匹配 = 拒", async () => {
  const mismatchedHash = replaceHashByte(OWNER_CREDENTIAL_HASH, 31, "00");
  assert.equal(await matchesOwnerCredential(OWNER_CREDENTIAL, mismatchedHash), false);
});

test("owner hash 完全匹配 = 通过，非法长度 = 拒", async () => {
  assert.equal(await matchesOwnerCredential(OWNER_CREDENTIAL, OWNER_CREDENTIAL_HASH), true);
  assert.equal(constantTimeEqualHex64(OWNER_CREDENTIAL_HASH.slice(2), OWNER_CREDENTIAL_HASH), false);
  assert.equal(constantTimeEqualHex64(OWNER_CREDENTIAL_HASH, `${OWNER_CREDENTIAL_HASH}00`), false);
});
