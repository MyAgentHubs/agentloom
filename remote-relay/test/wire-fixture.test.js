import { test } from "node:test";
import assert from "node:assert/strict";
import { createHash, webcrypto } from "node:crypto";
import { readFileSync } from "node:fs";
import { validateEnvelope, buildAAD } from "../src/envelope.js";

const fixtures = JSON.parse(
  readFileSync(new URL("../fixtures/wire-v1.json", import.meta.url), "utf8"),
);

for (const fixture of fixtures.filter((fixture) => !Object.hasOwn(fixture, "layer"))) {
  test(fixture.name, () => {
    const result = validateEnvelope(fixture.envelope);
    assert.equal(result.ok, fixture.expect.valid);

    if (fixture.expect.valid === false) {
      for (const code of fixture.expect.errors) {
        assert.ok(result.errors.includes(code), `expected errors to include ${code}`);
      }
      return;
    }

    assert.equal(buildAAD(fixture.envelope), fixture.expect.aad);
  });
}

const TOKEN_LAYERS = new Set([
  "token-frame",
  "subprotocol",
  "http",
  "desktop-upgrade",
  "inbound-matrix",
  "time-window",
  "ttl-clamp",
  "aad-kat",
  "chain",
]);
const DIRECTIONAL_TOKEN_LAYERS = new Set(
  [...TOKEN_LAYERS].filter((layer) => !["aad-kat", "chain"].includes(layer)),
);
const HEX64 = /^[0-9a-f]{64}$/;
const TTL_CAPS_MS = {
  pairing: 330_000,
  access: 3_900_000,
  prev: 172_800_000,
  refresh_until: 2_592_000_000,
};

function isObject(value) {
  return value !== null && typeof value === "object" && !Array.isArray(value);
}

function collectNamedValues(value, field, values = []) {
  if (Array.isArray(value)) {
    for (const item of value) collectNamedValues(item, field, values);
    return values;
  }
  if (!isObject(value)) return values;
  for (const [key, child] of Object.entries(value)) {
    if (key === field) values.push(child);
    collectNamedValues(child, field, values);
  }
  return values;
}

function subprotocolDecision(offers) {
  const tokenOffers = offers.filter((offer) => offer.startsWith("token."));
  const match = tokenOffers.length === 1
    ? /^token\.([0-9a-f]{64})$/.exec(tokenOffers[0])
    : null;
  return {
    accept: offers.includes("agentloom-rc-v1") && match !== null,
    tokenHex: match?.[1] ?? null,
  };
}

function sha256AsciiHex(value) {
  return createHash("sha256").update(value, "ascii").digest("hex");
}

function desktopUpgradeDecision(fixture) {
  const match = typeof fixture.authorization === "string"
    ? /^Bearer ([0-9a-f]{64})$/.exec(fixture.authorization)
    : null;
  const credentialMatches = match !== null
    && fixture.credential_hex === match[1]
    && sha256AsciiHex(fixture.credential_hex) === fixture.pre_state.owner_credential_hash;

  if (fixture.pre_state.tombstoned) return { accept: false, status: 410 };
  if (!credentialMatches) return { accept: false, status: 401 };
  return { accept: true, role: "desktop", epoch_bump: true };
}

function httpBodyByteLength(body) {
  return Buffer.byteLength(typeof body === "string" ? body : JSON.stringify(body), "utf8");
}

function httpStatusDecision(fixture) {
  const { request, pre_state: preState } = fixture;
  if (preState.rate_limited === true) return 429;
  if (preState.tombstoned === true || preState.owner === "tombstoned") return 410;

  if (request.method === "POST") {
    if (isObject(request.body) && !HEX64.test(request.body.credential_hash)) return 400;
    if (httpBodyByteLength(request.body) > 1024) return 413;
    if (!isObject(request.body)) return 400;
    if (preState.owner === "none") return 200;
    if (["same", "other"].includes(preState.owner)) {
      return request.body.credential_hash === preState.owner_credential_hash ? 200 : 409;
    }
    return 401;
  }

  if (request.method === "DELETE") {
    const authorization = request.headers?.authorization;
    const match = typeof authorization === "string"
      ? /^Bearer ([0-9a-f]{64})$/.exec(authorization)
      : null;
    const credentialMatches = match !== null
      && fixture.credential_hex === match[1]
      && sha256AsciiHex(fixture.credential_hex) === preState.owner_credential_hash;
    return credentialMatches ? 200 : 401;
  }

  return 400;
}

const INBOUND_MATRIX = new Map([
  ["pairing", new Set(["pair.hello", "pair.done"])],
  ["remote", new Set(["input", "control", "presence", "token.refresh"])],
  ["refresh", new Set(["token.refresh"])],
  ["desktop", new Set([
    "event",
    "live",
    "control.notify_hint",
    "input.ack",
    "pair.accept",
    "pair.ready",
    "token.put",
    "token.delete",
    "token.sync",
    "token.reset",
    "token.refresh.ok",
    "token.refresh.fail",
  ])],
]);

function inboundMatrixDecision(scope, frameType) {
  const allowed = INBOUND_MATRIX.get(scope)?.has(frameType) === true;
  return allowed ? { allowed: true } : { allowed: false, error: "role_forbidden" };
}

function timeWindowDecision(nowMs, row) {
  if (row.subject_state !== "active") return "reject:401";
  if (
    Object.hasOwn(row, "generation")
    && row.generation !== row.current_generation
  ) {
    return "reject:stale_generation";
  }

  if (row.scope === "pairing") {
    if (
      row.kind === "current"
      && row.valid_until === row.access_expires
      && nowMs < row.access_expires
    ) {
      return "scope:pairing";
    }
    return "reject:401";
  }

  if (row.kind === "current" && nowMs < row.access_expires) {
    return "scope:remote";
  }
  if (row.kind === "current" && nowMs < row.valid_until) {
    return "scope:refresh";
  }
  if (row.kind === "prev" && nowMs < row.valid_until) {
    return "scope:refresh";
  }
  return "reject:401";
}

function requiredString(object, field, error = `${field}_required`) {
  return typeof object[field] === "string" ? null : error;
}

function positiveInteger(object, field, missing = `${field}_required`) {
  if (!Object.hasOwn(object, field)) return missing;
  return Number.isInteger(object[field]) && object[field] > 0
    ? null
    : `${field}_must_be_positive`;
}

function timestampError(value) {
  if (!Number.isInteger(value) || value <= 0) return "timestamp_must_be_positive";
  return value <= Number.MAX_SAFE_INTEGER ? null : "timestamp_exceeds_json_safe_integer";
}

function subjectError(subject) {
  return subject === "pairing"
    || /^device:[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i.test(subject)
    ? null
    : "subject_invalid";
}

function putBodyError(body) {
  let error = requiredString(body, "subject");
  if (error) return error;
  if ((error = subjectError(body.subject))) return error;
  if ((error = positiveInteger(body, "generation"))) return error;
  if (typeof body.scope !== "string" || !["remote", "pairing"].includes(body.scope)) {
    return "scope_invalid";
  }
  if (body.subject === "pairing" && body.scope !== "pairing") {
    return "pairing_scope_required";
  }
  if (!isObject(body.current)) return "current_required";
  if (!Object.hasOwn(body.current, "token_hash")) return "current_token_hash_required";
  if (typeof body.current.token_hash !== "string" || !HEX64.test(body.current.token_hash)) {
    return "token_hash_invalid";
  }
  if ((error = timestampError(body.current.access_expires))) return error;
  if (body.scope === "remote") {
    if ((error = timestampError(body.current.refresh_until))) return error;
    if (body.current.access_expires > body.current.refresh_until) {
      return "access_expires_after_refresh_until";
    }
  }
  if (Object.hasOwn(body, "prev")) {
    if (body.subject === "pairing") return "pairing_prev_forbidden";
    if (!isObject(body.prev)) return "prev_invalid";
    if (!Object.hasOwn(body.prev, "token_hash")) return "prev_token_hash_required";
    if (typeof body.prev.token_hash !== "string" || !HEX64.test(body.prev.token_hash)) {
      return "token_hash_invalid";
    }
    if ((error = positiveInteger(body.prev, "generation"))) return error;
    if ((error = timestampError(body.prev.prev_expires))) return error;
  }
  return null;
}

function tokenFrameError(frame) {
  const required = (...fields) => {
    for (const field of fields) {
      const error = requiredString(frame, field);
      if (error) return error;
    }
    return null;
  };
  const subject = () => requiredString(frame, "subject") ?? subjectError(frame.subject);
  const generation = (field = "generation") => positiveInteger(frame, field);
  const entries = (limitError) => {
    if (!Array.isArray(frame.entries)) return "entries_required";
    if (frame.entries.length > 256) return limitError;
    for (const entry of frame.entries) {
      if (!isObject(entry)) return "entry_invalid";
      const error = putBodyError(entry);
      if (error) return error;
    }
    return null;
  };

  switch (frame.t) {
    case "token.put":
      return putBodyError(frame);
    case "token.delete":
      return subject() ?? generation()
        ?? (Object.hasOwn(frame, "close") && typeof frame.close !== "boolean"
          ? "close_invalid" : null);
    case "token.ack":
      return subject() ?? generation()
        ?? (typeof frame.result !== "string"
          || !["ok", "idempotent", "rejected"].includes(frame.result)
          ? "result_invalid" : null)
        ?? (Object.hasOwn(frame, "reason") && typeof frame.reason !== "string"
          ? "reason_invalid" : null);
    case "token.sync":
    case "token.reset": {
      const error = positiveInteger(frame, "revision");
      return error ?? entries(frame.t === "token.sync"
        ? "sync_entries_too_many" : "reset_entries_too_many");
    }
    case "token.sync.ack":
      return positiveInteger(frame, "revision")
        ?? (!Number.isInteger(frame.relay_high_water) || frame.relay_high_water < 0
          ? "relay_high_water_required" : null);
    case "token.refresh":
      return required("request_id", "ct", "n");
    case "token.refresh.forward":
      return required("request_id") ?? subject() ?? generation("request_generation")
        ?? required("ct", "n");
    case "token.refresh.ok":
      return required("request_id") ?? subject() ?? generation() ?? required("ct", "n");
    case "token.refresh.fail":
      return required("request_id") ?? subject() ?? required("reason")
        ?? (Object.hasOwn(frame, "close") && typeof frame.close !== "boolean"
          ? "close_invalid" : null);
    // S1i3 K3.5：这两条只校验 wire-v1.json 里静态样张的形状（"origin_connection_id
    // 必须存在"），不驱动、也不消费 room-do.js 真实的转发/盖章/落路由逻辑——「样张有
    // 测试」不等于「转发代码有测试」。真正驱动真路径、钉住 relay 实际转发出的帧的是
    // room-do.test.js 里「pair.hello：relay 转发前盖章 origin_connection_id」与
    // 「pair.done：relay 转发前盖章 origin_connection_id」两条真路径测试（别再写死
    // 行号——行号会随后续插入测试漂移，陈旧的行号引用比没有引用更坏）。
    case "pair.hello":
      return required("room", "remote_pub", "token_ct", "token_n", "origin_connection_id")
        ?? (/^[0-9a-f]{32}$/.test(frame.room) ? null : "room_invalid");
    case "pair.done":
      return required(
        "room",
        "device_id",
        "confirm_ct",
        "confirm_n",
        "origin_connection_id",
      ) ?? (/^[0-9a-f]{32}$/.test(frame.room) ? null : "room_invalid");
    case "pair.ready":
      return required("room", "device_id", "ct", "n")
        ?? (/^[0-9a-f]{32}$/.test(frame.room) ? null : "room_invalid");
    case "pair.accept":
      if (Object.hasOwn(frame, "capability_token") || Object.hasOwn(frame, "refresh_token")) {
        return "plaintext_token_forbidden";
      }
      return required("room", "device_id", "k_room_ct", "k_room_n", "tokens_ct", "tokens_n")
        ?? (/^[0-9a-f]{32}$/.test(frame.room) ? null : "room_invalid");
    default:
      return "frame_type_invalid";
  }
}

test("wire-v1 token-plane layers are structurally valid", () => {
  const names = new Set();
  const directions = new Map(
    [...DIRECTIONAL_TOKEN_LAYERS]
      .map((layer) => [layer, { pass: 0, reject: 0 }]),
  );
  const aadKatKinds = new Set();
  let aadKatCount = 0;

  for (const fixture of fixtures) {
    assert.equal(typeof fixture.name, "string", "fixture name must be a string");
    assert.ok(!names.has(fixture.name), `duplicate fixture name: ${fixture.name}`);
    names.add(fixture.name);

    if (!Object.hasOwn(fixture, "layer")) continue;
    assert.equal(typeof fixture.layer, "string", `${fixture.name}: layer must be a string`);
    assert.ok(TOKEN_LAYERS.has(fixture.layer), `${fixture.name}: unknown layer`);
    assert.ok(isObject(fixture.expect), `${fixture.name}: expect must be an object`);
    const direction = directions.get(fixture.layer);

    if (fixture.layer === "token-frame") {
      assert.ok(isObject(fixture.frame), `${fixture.name}: frame must be an object`);
      assert.equal(typeof fixture.frame.t, "string", `${fixture.name}: frame.t`);
      assert.equal(typeof fixture.expect.valid, "boolean", `${fixture.name}: expect.valid`);
      assert.ok(Array.isArray(fixture.expect.errors), `${fixture.name}: expect.errors`);
      assert.ok(
        fixture.expect.errors.every((error) => typeof error === "string"),
        `${fixture.name}: errors must be strings`,
      );
      assert.equal(
        fixture.expect.valid,
        fixture.expect.errors.length === 0,
        `${fixture.name}: valid/errors disagree`,
      );

      const hashes = collectNamedValues(fixture.frame, "token_hash");
      if (fixture.expect.errors.includes("token_hash_invalid")) {
        assert.ok(hashes.some((hash) => !HEX64.test(hash)), `${fixture.name}: malformed hash missing`);
      } else {
        assert.ok(hashes.every((hash) => HEX64.test(hash)), `${fixture.name}: token_hash`);
      }
      direction[fixture.expect.valid ? "pass" : "reject"] += 1;
    } else if (fixture.layer === "subprotocol") {
      assert.ok(Array.isArray(fixture.offers), `${fixture.name}: offers must be an array`);
      assert.ok(fixture.offers.every((offer) => typeof offer === "string"), `${fixture.name}: offers`);
      assert.equal(typeof fixture.expect.accept, "boolean", `${fixture.name}: expect.accept`);
      if (fixture.expect.accept) {
        assert.equal(fixture.expect.echo, "agentloom-rc-v1", `${fixture.name}: echo`);
        assert.ok(HEX64.test(fixture.expect.token_hex), `${fixture.name}: token_hex`);
      } else {
        assert.equal(fixture.expect.status, 401, `${fixture.name}: reject status`);
      }
      direction[fixture.expect.accept ? "pass" : "reject"] += 1;
    } else if (fixture.layer === "http") {
      assert.ok(isObject(fixture.request), `${fixture.name}: request must be an object`);
      assert.equal(typeof fixture.request.method, "string", `${fixture.name}: request.method`);
      assert.equal(typeof fixture.request.path, "string", `${fixture.name}: request.path`);
      assert.ok(isObject(fixture.pre_state), `${fixture.name}: pre_state must be an object`);
      assert.ok(
        ["none", "same", "other", "tombstoned"].includes(fixture.pre_state.owner),
        `${fixture.name}: pre_state.owner`,
      );
      if (Object.hasOwn(fixture.pre_state, "rate_limited")) {
        assert.equal(
          typeof fixture.pre_state.rate_limited,
          "boolean",
          `${fixture.name}: pre_state.rate_limited`,
        );
      }
      if (Object.hasOwn(fixture.pre_state, "tombstoned")) {
        assert.equal(
          typeof fixture.pre_state.tombstoned,
          "boolean",
          `${fixture.name}: pre_state.tombstoned`,
        );
      }
      if (Object.hasOwn(fixture, "credential_hex")) {
        assert.match(fixture.credential_hex, HEX64, `${fixture.name}: credential_hex`);
      }
      assert.ok(Number.isInteger(fixture.expect.status), `${fixture.name}: expect.status`);
      assert.equal(httpStatusDecision(fixture), fixture.expect.status, fixture.name);
      direction[fixture.expect.status < 400 ? "pass" : "reject"] += 1;
    } else if (fixture.layer === "desktop-upgrade") {
      assert.ok(
        fixture.authorization === null || typeof fixture.authorization === "string",
        `${fixture.name}: authorization`,
      );
      if (Object.hasOwn(fixture, "credential_hex")) {
        assert.match(fixture.credential_hex, HEX64, `${fixture.name}: credential_hex`);
      }
      assert.ok(isObject(fixture.pre_state), `${fixture.name}: pre_state`);
      assert.match(
        fixture.pre_state.owner_credential_hash,
        HEX64,
        `${fixture.name}: owner_credential_hash`,
      );
      assert.equal(typeof fixture.pre_state.tombstoned, "boolean", `${fixture.name}: tombstoned`);
      assert.equal(typeof fixture.expect.accept, "boolean", `${fixture.name}: expect.accept`);
      if (fixture.expect.accept) {
        assert.equal(fixture.expect.role, "desktop", `${fixture.name}: role`);
        assert.equal(fixture.expect.epoch_bump, true, `${fixture.name}: epoch_bump`);
      } else {
        assert.ok([401, 410].includes(fixture.expect.status), `${fixture.name}: status`);
      }
      direction[fixture.expect.accept ? "pass" : "reject"] += 1;
    } else if (fixture.layer === "inbound-matrix") {
      assert.ok(INBOUND_MATRIX.has(fixture.scope), `${fixture.name}: scope`);
      assert.equal(typeof fixture.frame_t, "string", `${fixture.name}: frame_t`);
      assert.equal(typeof fixture.expect.allowed, "boolean", `${fixture.name}: expect.allowed`);
      if (!fixture.expect.allowed) {
        assert.equal(fixture.expect.error, "role_forbidden", `${fixture.name}: expect.error`);
      }
      direction[fixture.expect.allowed ? "pass" : "reject"] += 1;
    } else if (fixture.layer === "time-window") {
      assert.ok(Number.isSafeInteger(fixture.now_ms), `${fixture.name}: now_ms`);
      assert.ok(isObject(fixture.row), `${fixture.name}: row must be an object`);
      assert.ok(["current", "prev"].includes(fixture.row.kind), `${fixture.name}: row.kind`);
      assert.ok(["remote", "pairing"].includes(fixture.row.scope), `${fixture.name}: row.scope`);
      assert.ok(
        ["active", "revoked"].includes(fixture.row.subject_state),
        `${fixture.name}: row.subject_state`,
      );
      assert.ok(Number.isSafeInteger(fixture.row.access_expires), `${fixture.name}: access_expires`);
      assert.ok(Number.isSafeInteger(fixture.row.valid_until), `${fixture.name}: valid_until`);
      const hasGeneration = Object.hasOwn(fixture.row, "generation");
      assert.equal(
        hasGeneration,
        Object.hasOwn(fixture.row, "current_generation"),
        `${fixture.name}: generation fields must appear together`,
      );
      if (hasGeneration) {
        assert.ok(Number.isSafeInteger(fixture.row.generation), `${fixture.name}: generation`);
        assert.ok(
          Number.isSafeInteger(fixture.row.current_generation),
          `${fixture.name}: current_generation`,
        );
      }
      assert.ok(
        [
          "scope:remote",
          "scope:pairing",
          "scope:refresh",
          "reject:401",
          "reject:stale_generation",
        ]
          .includes(fixture.expect.decision),
        `${fixture.name}: expect.decision`,
      );
      if (fixture.expect.decision === "reject:stale_generation") {
        assert.equal(fixture.expect.close, true, `${fixture.name}: stale generation must close`);
      }
      direction[fixture.expect.decision.startsWith("reject:") ? "reject" : "pass"] += 1;
    } else if (fixture.layer === "ttl-clamp") {
      assert.ok(Object.hasOwn(TTL_CAPS_MS, fixture.cap), `${fixture.name}: cap`);
      for (const field of ["cap_ms", "relay_now_ms", "input_ms"]) {
        assert.ok(Number.isSafeInteger(fixture[field]), `${fixture.name}: ${field}`);
      }
      assert.ok(Number.isSafeInteger(fixture.expect.stored_ms), `${fixture.name}: stored_ms`);
      direction[fixture.expect.stored_ms === fixture.input_ms ? "pass" : "reject"] += 1;
    } else if (fixture.layer === "aad-kat") {
      aadKatCount += 1;
      assert.ok(isObject(fixture.meta), `${fixture.name}: meta must be an object`);
      assert.equal(fixture.meta.v, 1, `${fixture.name}: meta.v`);
      assert.match(fixture.meta.room, /^[0-9a-f]{32}$/, `${fixture.name}: meta.room`);
      assert.equal(fixture.meta.epoch, 0, `${fixture.name}: meta.epoch`);
      assert.ok(
        ["pair-ready", "pair-accept-tokens", "token.refresh", "token.refresh.ok"]
          .includes(fixture.meta.kind),
        `${fixture.name}: meta.kind`,
      );
      aadKatKinds.add(fixture.meta.kind);
      assert.equal(fixture.meta.session, fixture.device_id, `${fixture.name}: meta.session/device_id`);
      if (["pair-ready", "pair-accept-tokens"].includes(fixture.meta.kind)) {
        assert.equal(fixture.meta.command_id, null, `${fixture.name}: meta.command_id`);
        assert.equal(
          Object.hasOwn(fixture, "request_id"),
          false,
          `${fixture.name}: request_id must be absent`,
        );
      } else {
        assert.equal(
          fixture.meta.command_id,
          fixture.request_id,
          `${fixture.name}: meta.command_id/request_id`,
        );
      }
      assert.equal(typeof fixture.expect.aad, "string", `${fixture.name}: expect.aad`);
      assert.ok(isObject(fixture.kat), `${fixture.name}: kat must be an object`);
      assert.match(fixture.kat.key_hex, HEX64, `${fixture.name}: kat.key_hex`);
      assert.equal(
        Buffer.from(fixture.kat.n_b64, "base64").length,
        12,
        `${fixture.name}: kat.n_b64 must encode 12 bytes`,
      );
      assert.ok(Buffer.from(fixture.kat.ct_b64, "base64").length > 16, `${fixture.name}: kat.ct_b64`);
      assert.equal(typeof fixture.kat.plaintext, "string", `${fixture.name}: kat.plaintext`);

      if (fixture.meta.kind === "pair-accept-tokens") {
        const plaintext = JSON.parse(fixture.kat.plaintext);
        assert.deepEqual(
          Object.keys(plaintext),
          ["capability_token", "refresh_token"],
          `${fixture.name}: pair.accept plaintext fields`,
        );
        assert.match(plaintext.capability_token, HEX64, `${fixture.name}: capability_token`);
        assert.match(plaintext.refresh_token, HEX64, `${fixture.name}: refresh_token`);
      }
    } else {
      const tokenHex = fixture.connect_token_hex ?? fixture.capability_token_hex;
      assert.match(tokenHex, HEX64, `${fixture.name}: access token`);
      if (Object.hasOwn(fixture, "connect_token_hex")) {
        assert.match(fixture.pairing_token_hex, HEX64, `${fixture.name}: pairing_token_hex`);
      }
      assert.match(fixture.token_hash_hex, HEX64, `${fixture.name}: token_hash_hex`);
      assert.ok(Array.isArray(fixture.subprotocol_offer), `${fixture.name}: subprotocol_offer`);
      assert.ok(isObject(fixture.put_frame), `${fixture.name}: put_frame`);
      assert.ok(isObject(fixture.window), `${fixture.name}: window`);
      assert.ok(["pairing", "remote"].includes(fixture.expect.scope), `${fixture.name}: expect.scope`);
    }
  }

  assert.equal(aadKatCount, 4, "aad-kat: exactly four cases required");
  assert.deepEqual(
    [...aadKatKinds].sort(),
    ["pair-accept-tokens", "pair-ready", "token.refresh", "token.refresh.ok"],
    "aad-kat: §9.5 kind coverage",
  );

  for (const [layer, counts] of directions) {
    assert.ok(counts.pass > 0, `${layer}: at least one passing/unchanged case required`);
    assert.ok(counts.reject > 0, `${layer}: at least one rejecting/clamped case required`);
  }
});

test("wire-v1 token-frame expectations are independently recomputed from §9.3-9.6", () => {
  for (const fixture of fixtures.filter((fixture) => fixture.layer === "token-frame")) {
    const error = tokenFrameError(fixture.frame);
    assert.equal(error === null, fixture.expect.valid, fixture.name);
    if (error !== null) {
      assert.ok(fixture.expect.errors.includes(error), `${fixture.name}: missing ${error}`);
    }
  }
});

test("wire-v1 subprotocol cases match the §9.1 grammar", () => {
  for (const fixture of fixtures.filter(({ layer }) => layer === "subprotocol")) {
    const actual = subprotocolDecision(fixture.offers);
    assert.equal(actual.accept, fixture.expect.accept, fixture.name);
    if (actual.accept) {
      assert.equal(fixture.expect.echo, "agentloom-rc-v1", fixture.name);
      assert.equal(fixture.expect.token_hex, actual.tokenHex, fixture.name);
    }
  }
});

test("wire-v1 desktop-upgrade cases recompute §9.1 Bearer ASCII-hex authorization", () => {
  for (const fixture of fixtures.filter(({ layer }) => layer === "desktop-upgrade")) {
    assert.deepEqual(desktopUpgradeDecision(fixture), fixture.expect, fixture.name);
  }
});

test("wire-v1 inbound-matrix cases match the hard-coded §9.1 fail-closed table", () => {
  for (const fixture of fixtures.filter(({ layer }) => layer === "inbound-matrix")) {
    assert.deepEqual(
      inboundMatrixDecision(fixture.scope, fixture.frame_t),
      fixture.expect,
      fixture.name,
    );
  }
});

test("wire-v1 time-window cases match the §9.1 three-band rule", () => {
  for (const fixture of fixtures.filter(({ layer }) => layer === "time-window")) {
    assert.equal(
      timeWindowDecision(fixture.now_ms, fixture.row),
      fixture.expect.decision,
      fixture.name,
    );
  }
});

test("wire-v1 ttl-clamp cases match the §9.2 cap rule", () => {
  for (const fixture of fixtures.filter(({ layer }) => layer === "ttl-clamp")) {
    assert.equal(fixture.cap_ms, TTL_CAPS_MS[fixture.cap], `${fixture.name}: cap table`);
    assert.equal(
      fixture.expect.stored_ms,
      Math.min(fixture.input_ms, fixture.relay_now_ms + fixture.cap_ms + 120_000),
      fixture.name,
    );
  }
});

test("wire-v1 chain recomputes the §9.1/§9.5 pairing and device paths", () => {
  for (const fixture of fixtures.filter(({ layer }) => layer === "chain")) {
    const tokenHex = fixture.connect_token_hex ?? fixture.capability_token_hex;
    assert.equal(
      sha256AsciiHex(tokenHex),
      fixture.token_hash_hex,
      `${fixture.name}: sha256(access token ASCII)`,
    );
    const offer = subprotocolDecision(fixture.subprotocol_offer);
    assert.deepEqual(
      offer,
      { accept: true, tokenHex },
      `${fixture.name}: subprotocol offer`,
    );
    assert.equal(tokenFrameError(fixture.put_frame), null, `${fixture.name}: put frame`);
    assert.equal(
      fixture.put_frame.current.token_hash,
      fixture.token_hash_hex,
      `${fixture.name}: put token_hash`,
    );
    assert.equal(
      fixture.put_frame.scope,
      fixture.expect.scope,
      `${fixture.name}: put scope`,
    );
    assert.equal(
      fixture.put_frame.current.access_expires,
      fixture.window.access_expires,
      `${fixture.name}: put/window access_expires`,
    );
    const validUntil = fixture.put_frame.scope === "remote"
      ? fixture.put_frame.current.refresh_until
      : fixture.put_frame.current.access_expires;
    assert.equal(validUntil, fixture.window.valid_until, `${fixture.name}: put/window valid_until`);
    assert.equal(
      timeWindowDecision(fixture.window.now_ms, {
        kind: fixture.window.kind ?? "current",
        scope: fixture.window.scope ?? fixture.put_frame.scope,
        subject_state: fixture.window.subject_state ?? "active",
        access_expires: fixture.window.access_expires,
        valid_until: fixture.window.valid_until,
      }),
      `scope:${fixture.expect.scope}`,
      `${fixture.name}: access window`,
    );
  }
});

test("kat_control_stop：AES-GCM 密文可用 fixture 密钥和 AAD 解密", async () => {
  const fixture = fixtures.find(({ name }) => name === "kat_control_stop");
  assert.ok(fixture, "kat_control_stop fixture must exist");

  const key = await webcrypto.subtle.importKey(
    "raw",
    Buffer.from(fixture.kat.k_room_hex, "hex"),
    "AES-GCM",
    false,
    ["decrypt"],
  );
  const plaintext = await webcrypto.subtle.decrypt(
    {
      name: "AES-GCM",
      iv: Buffer.from(fixture.envelope.n, "base64"),
      additionalData: new TextEncoder().encode(buildAAD(fixture.envelope)),
    },
    key,
    Buffer.from(fixture.envelope.ct, "base64"),
  );

  assert.equal(new TextDecoder().decode(plaintext), fixture.kat.plaintext);
});

test("wire-v1 aad-kat cases match AAD and decrypt with AES-256-GCM", async () => {
  const aadFixtures = fixtures.filter(({ layer }) => layer === "aad-kat");
  assert.equal(aadFixtures.length, 4, "exactly four aad-kat fixtures are required");

  for (const fixture of aadFixtures) {
    const aad = buildAAD(fixture.meta);
    assert.equal(aad, fixture.expect.aad, `${fixture.name}: AAD`);
    const key = await webcrypto.subtle.importKey(
      "raw",
      Buffer.from(fixture.kat.key_hex, "hex"),
      "AES-GCM",
      false,
      ["decrypt"],
    );
    const plaintext = await webcrypto.subtle.decrypt(
      {
        name: "AES-GCM",
        iv: Buffer.from(fixture.kat.n_b64, "base64"),
        additionalData: new TextEncoder().encode(aad),
      },
      key,
      Buffer.from(fixture.kat.ct_b64, "base64"),
    );
    assert.equal(
      new TextDecoder().decode(plaintext),
      fixture.kat.plaintext,
      `${fixture.name}: plaintext`,
    );
  }
});
