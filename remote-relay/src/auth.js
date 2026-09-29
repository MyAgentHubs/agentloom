"use strict";
// auth.js — S1 接入与鉴权（骨架）。
//
// relay 只做「校验」：请求带的房间能力令牌是否等于该房间登记过的合法令牌之一。
// 真正的令牌签发发生在桌面侧配对握手完成后（母文档 §5，属于组件 D3，
// 本单不做）——这里只实现骨架必须有的一半：默认拒绝 + 比对。
//
// 默认拒绝（母文档 §6b「防打爆」）：没带 token、或房间压根没登记任何合法 token、
// 或 token 对不上，一律拒绝——不存在「匿名也能连上看看」的路径。

/**
 * §9.1 正式远端连接只接受恰一个版本项与恰一个 token.hex64 项。返回值绝不
 * 包含原始 header，调用方也不得记录 token；语法有任何歧义都 fail closed。
 */
export function parseRemoteSubprotocol(request) {
  const header = request.headers.get("Sec-WebSocket-Protocol");
  if (!header) return { provided: false, ok: false, token: null };
  const offers = header.split(",").map((item) => item.trim());
  if (offers.length !== 2 || offers.some((item) => item.length === 0)) {
    return { provided: true, ok: false, token: null };
  }
  const versionOffers = offers.filter((item) => item === "agentloom-rc-v1");
  const tokenOffers = offers.filter((item) => item.startsWith("token."));
  if (versionOffers.length !== 1 || tokenOffers.length !== 1) {
    return { provided: true, ok: false, token: null };
  }
  const match = tokenOffers[0].match(/^token\.([0-9a-f]{64})$/);
  if (!match) return { provided: true, ok: false, token: null };
  return { provided: true, ok: true, token: match[1] };
}

export function parseBearerAuthorization(request) {
  const header = request.headers.get("Authorization");
  if (!header || !header.startsWith("Bearer ")) {
    return { provided: false, credential: null };
  }
  const credential = header.slice("Bearer ".length).trim();
  return { provided: true, credential: credential || null };
}

export async function sha256AsciiHex(value) {
  const bytes = new TextEncoder().encode(value);
  const digest = await globalThis.crypto.subtle.digest("SHA-256", bytes);
  return [...new Uint8Array(digest)].map((byte) => byte.toString(16).padStart(2, "0")).join("");
}

export function constantTimeEqualHex64(left, right) {
  if (!/^[0-9a-f]{64}$/.test(left || "") || !/^[0-9a-f]{64}$/.test(right || "")) {
    return false;
  }

  const leftBytes = new Uint8Array(32);
  const rightBytes = new Uint8Array(32);
  for (let i = 0; i < 32; i += 1) {
    const offset = i * 2;
    leftBytes[i] = Number.parseInt(left.slice(offset, offset + 2), 16);
    rightBytes[i] = Number.parseInt(right.slice(offset, offset + 2), 16);
  }

  let diff = 0;
  for (let i = 0; i < 32; i += 1) {
    diff |= leftBytes[i] ^ rightBytes[i];
  }
  return diff === 0;
}

export async function matchesOwnerCredential(credential, ownerCredentialHash) {
  if (!/^[0-9a-f]{64}$/.test(credential || "") || !/^[0-9a-f]{64}$/.test(ownerCredentialHash || "")) {
    return false;
  }
  return constantTimeEqualHex64(await sha256AsciiHex(credential), ownerCredentialHash);
}
