// qr-payload.test.ts — TDD 覆盖 src/pairing/qr-payload.ts。
//
// 刻意不用 Node `Buffer`（项目未装 @types/node）——走 `crypto/bytes.ts` 的 base64/base64url 编解码
// 工具，跟生产代码同一套。

import { describe, expect, it } from "vitest";
import { bytesToBase64, bytesToBase64Url, utf8Bytes } from "../crypto/bytes.ts";
import {
  desktopPublicKeyBytes,
  extractFragmentValue,
  parseQrPayload,
  parseValidRelayUrl,
  QrPayloadError,
  relayHttpsOrigin,
  type QrPayload,
} from "./qr-payload.ts";

const ROOM = "0123456789abcdef0123456789abcdef";
const PAIRING_TOKEN = "a".repeat(64);
const DESKTOP_PUB_B64 = bytesToBase64(new Uint8Array(32).fill(7));

function validPayload(): QrPayload {
  return { v: 1, relay_url: "wss://relay.example", room: ROOM, pairing_token: PAIRING_TOKEN, desktop_pub: DESKTOP_PUB_B64 };
}

function toBase64Url(json: string): string {
  return bytesToBase64Url(utf8Bytes(json));
}

describe("parseQrPayload() · 三种输入形状", () => {
  it("parses a full URL with #p=<base64url> fragment", () => {
    const payload = validPayload();
    const url = `https://relay.example/#p=${toBase64Url(JSON.stringify(payload))}`;
    expect(parseQrPayload(url)).toEqual(payload);
  });

  it("parses a bare base64url fragment value (no #p= marker)", () => {
    const payload = validPayload();
    expect(parseQrPayload(toBase64Url(JSON.stringify(payload)))).toEqual(payload);
  });

  it("parses legacy bare JSON (粘贴配对串兜底)", () => {
    const payload = validPayload();
    expect(parseQrPayload(JSON.stringify(payload))).toEqual(payload);
  });

  it("round trip: URL fragment produced by extractFragmentValue matches direct base64url parse", () => {
    const payload = validPayload();
    const encoded = toBase64Url(JSON.stringify(payload));
    const url = `https://relay.example/#p=${encoded}`;
    expect(extractFragmentValue(url)).toBe(encoded);
  });
});

describe("parseQrPayload() · shape 校验拒绝", () => {
  const cases: Array<{ name: string; mutate: (p: Record<string, unknown>) => void }> = [
    { name: "wrong v", mutate: (p) => (p.v = 2) },
    { name: "empty relay_url", mutate: (p) => (p.relay_url = "") },
    { name: "room not 32 hex", mutate: (p) => (p.room = "not-hex") },
    { name: "room uppercase (non-canonical)", mutate: (p) => (p.room = ROOM.toUpperCase()) },
    { name: "pairing_token wrong length", mutate: (p) => (p.pairing_token = "abcd") },
    { name: "pairing_token uppercase", mutate: (p) => (p.pairing_token = PAIRING_TOKEN.toUpperCase()) },
    { name: "desktop_pub not base64-decodable to 32 bytes", mutate: (p) => (p.desktop_pub = "short") },
    { name: "missing desktop_pub", mutate: (p) => delete p.desktop_pub },
  ];

  for (const { name, mutate } of cases) {
    it(`rejects: ${name}`, () => {
      const payload: Record<string, unknown> = { ...validPayload() };
      mutate(payload);
      expect(() => parseQrPayload(JSON.stringify(payload))).toThrow(QrPayloadError);
    });
  }

  it("rejects malformed JSON", () => {
    expect(() => parseQrPayload("{not json")).toThrow(QrPayloadError);
  });

  it("rejects malformed base64url fragment", () => {
    expect(() => parseQrPayload("https://relay.example/#p=not!!valid!!base64url")).toThrow(QrPayloadError);
  });
});

describe("desktopPublicKeyBytes()", () => {
  it("decodes desktop_pub into exactly 32 raw bytes", () => {
    const payload = validPayload();
    const bytes = desktopPublicKeyBytes(payload);
    expect(bytes.length).toBe(32);
    expect(Array.from(bytes)).toEqual(Array.from(new Uint8Array(32).fill(7)));
  });
});

// ============================================================================
// parseValidRelayUrl() · 逐条复刻桌面同组畸形语料
// ============================================================================
// 语料来源：app/src/components/settings/SettingsRemoteControl.test.tsx:864-880
// （`buildPairingQrUrl` 的 `it.each` 畸形 relay_url 表）——同一套标签/值，不自创新语料，确保两端
// 对"合法 relay_url 长什么样"判定完全一致。

describe("parseValidRelayUrl() · 桌面同组畸形语料（SettingsRemoteControl.test.tsx:864-880）", () => {
  it.each([
    ["http:// 前缀", "http://relay.example.com"],
    ["带非根 path", "wss://relay.example.com/room"],
    ["带 query", "wss://relay.example.com?x=1"],
    ["带 userinfo", "wss://user:pass@relay.example.com"],
    ["无法 parse", "not a url"],
    ["显式 hash", "wss://relay.example.com#x"],
    ["空 hash（归一化会悄悄吃掉 #）", "wss://relay.example.com#"],
    ["空 userinfo（归一化会悄悄吃掉 @）", "wss://@relay.example.com"],
    ["空 query（归一化会悄悄吃掉 ?）", "wss://relay.example.com?"],
    ["斜杠后空 query（canonical 等值检查测不出）", "wss://relay.example.com/?"],
    ["斜杠后空 hash（canonical 等值检查测不出）", "wss://relay.example.com/#"],
  ])("形态非法（%s）时返回 null", (_label, badRelayUrl) => {
    expect(parseValidRelayUrl(badRelayUrl)).toBeNull();
  });

  it.each([
    ["http:// 前缀", "http://relay.example.com"],
    ["带非根 path", "wss://relay.example.com/room"],
    ["带 query", "wss://relay.example.com?x=1"],
    ["带 userinfo", "wss://user:pass@relay.example.com"],
  ])("同一畸形 relay_url（%s）经 parseQrPayload 整条 payload 一起被拒", (_label, badRelayUrl) => {
    const payload = { ...validPayload(), relay_url: badRelayUrl };
    expect(() => parseQrPayload(JSON.stringify(payload))).toThrow(QrPayloadError);
  });

  it("accepts a bare wss host (no path/query/hash)", () => {
    const url = parseValidRelayUrl("wss://relay.example.com");
    expect(url).not.toBeNull();
    expect(url?.host).toBe("relay.example.com");
  });

  it("accepts a wss host with a port", () => {
    const url = parseValidRelayUrl("wss://relay.example.com:8443");
    expect(url).not.toBeNull();
    expect(url?.host).toBe("relay.example.com:8443");
  });

  it("accepts an explicit root path", () => {
    expect(parseValidRelayUrl("wss://relay.example.com/")).not.toBeNull();
  });
});

describe("relayHttpsOrigin()", () => {
  it("derives https://<host> verbatim, port included, no default-port leniency", () => {
    const url = parseValidRelayUrl("wss://relay.example.com:8443");
    expect(url).not.toBeNull();
    expect(relayHttpsOrigin(url!)).toBe("https://relay.example.com:8443");
  });
});

// ============================================================================
// parseQrPayload() · 完整 QR URL 场景：https origin 与 payload 的 wss origin 一一对应
// ============================================================================

describe("parseQrPayload() · 完整 URL origin 一一对应校验", () => {
  it("accepts when the outer https origin matches the payload's derived wss origin (host only)", () => {
    const payload = { ...validPayload(), relay_url: "wss://relay.example.com" };
    const url = `https://relay.example.com/#p=${toBase64Url(JSON.stringify(payload))}`;
    expect(parseQrPayload(url)).toEqual(payload);
  });

  it("accepts when host+port both match", () => {
    const payload = { ...validPayload(), relay_url: "wss://relay.example.com:8443" };
    const url = `https://relay.example.com:8443/#p=${toBase64Url(JSON.stringify(payload))}`;
    expect(parseQrPayload(url)).toEqual(payload);
  });

  it("rejects when the outer host differs from the payload's relay_url host (phishing-style mismatch)", () => {
    const payload = { ...validPayload(), relay_url: "wss://real-relay.example.com" };
    const url = `https://attacker.example/#p=${toBase64Url(JSON.stringify(payload))}`;
    expect(() => parseQrPayload(url)).toThrow(QrPayloadError);
  });

  it("rejects when only the port differs between outer origin and payload's wss origin", () => {
    const payload = { ...validPayload(), relay_url: "wss://relay.example.com:9999" };
    const url = `https://relay.example.com/#p=${toBase64Url(JSON.stringify(payload))}`;
    expect(() => parseQrPayload(url)).toThrow(QrPayloadError);
  });

  it("rejects a non-https outer scheme even if the host matches", () => {
    const payload = { ...validPayload(), relay_url: "wss://relay.example.com" };
    const url = `http://relay.example.com/#p=${toBase64Url(JSON.stringify(payload))}`;
    expect(() => parseQrPayload(url)).toThrow(QrPayloadError);
  });

  it("rejects an unparseable outer URL prefix", () => {
    expect(() => parseQrPayload(`not a url#p=${toBase64Url(JSON.stringify(validPayload()))}`)).toThrow(QrPayloadError);
  });

  it("bare fragment value (no outer URL) is not subject to origin checks", () => {
    // 没有外层 URL 可比对——只要 payload 自身 relay_url 严格合法即可通过。
    const payload = { ...validPayload(), relay_url: "wss://totally-different-host.example" };
    expect(parseQrPayload(toBase64Url(JSON.stringify(payload)))).toEqual(payload);
  });
});
