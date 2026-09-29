"use strict";
// index.js — Worker 入口：路由 /room/:id 到对应的 RoomDO 实例。
//
// 这一层刻意薄：房间格式校验（S1 的一半）在这里做，因为不合法的房间 id
// 根本不该触发一次 DO 实例化（每个房间 id 都会落到一个独立 DO——不校验
// 格式等于把「枚举房间 id」的攻击面直接甩给 DO 层）；真正的令牌鉴权在
// room-do.js 里做，因为只有房间自己的 DO 存着它登记过的合法令牌。

import { RoomDO } from "./room-do.js";
import { withSecurityHeaders } from "./security-headers.js";

export { RoomDO };

const ROOM_PATH_RE = /^\/room\/([0-9a-f]{32})(\/.*)?$/;
const CLAIM_EDGE_LIMIT = 10;
const CLAIM_EDGE_WINDOW_MS = 60_000;
const claimEdgeBuckets = new Map();
// 桶盐与桶表同寿命：都只活在这个 isolate 的内存里，isolate 一换两者一起重来。
// 有盐之后光凭桶 key 反推不出 IP——裸 sha256(ip) 的取值空间只有全部 IPv4
// 地址那么大，离线枚举就能还原。盐本身不落库、不打日志。
// 惰性初始化（CF 部署校验 10021）：Cloudflare 不允许模块顶层调用
// getRandomValues 这类「生成随机值」的操作——模块顶层只声明占位符，真正
// 取随机值挪到首次请求到达、调用 getClaimEdgeSalt() 时才做；取一次之后
// 缓存在这个 isolate 存活期间，语义与之前的顶层常量完全一致（同寿命、
// 不落库、不打日志），只是生成时机从「import 时」推迟到「首次用到时」。
let claimEdgeSalt = null;

function getClaimEdgeSalt() {
  if (claimEdgeSalt === null) {
    claimEdgeSalt = globalThis.crypto.getRandomValues(new Uint8Array(32));
  }
  return claimEdgeSalt;
}

export async function ipBucketKey(request) {
  const salt = getClaimEdgeSalt();
  const ip = request.headers.get("CF-Connecting-IP") || "unknown";
  const ipBytes = new TextEncoder().encode(ip);
  const material = new Uint8Array(salt.length + ipBytes.length);
  material.set(salt, 0);
  material.set(ipBytes, salt.length);
  const digest = await globalThis.crypto.subtle.digest("SHA-256", material);
  return [...new Uint8Array(digest).slice(0, 16)]
    .map((byte) => byte.toString(16).padStart(2, "0"))
    .join("");
}

function takeClaimEdgeSlot(key, now = Date.now()) {
  const bucket = claimEdgeBuckets.get(key);
  if (!bucket || now >= bucket.startedAt + CLAIM_EDGE_WINDOW_MS) {
    claimEdgeBuckets.set(key, { startedAt: now, attempts: 1 });
    return true;
  }
  if (bucket.attempts >= CLAIM_EDGE_LIMIT) return false;
  bucket.attempts += 1;
  return true;
}

export default {
  async fetch(request, env) {
    const url = new URL(request.url);

    if (url.pathname === "/healthz") {
      return new Response("ok", { status: 200 });
    }

    const match = url.pathname.match(ROOM_PATH_RE);
    if (match) {
      const roomId = match[1];
      if (request.method === "POST" && url.pathname === `/room/${roomId}/claim`) {
        // SEC-2 外层粗网：Cloudflare 原生限速 binding，best-effort、叠加在下面
        // 确定性的 claimEdgeBuckets 内层桶之上（不替换）。`?.limit` 存在性守
        // 护——binding 在 node:test 环境不存在，同 scheduleNextTokenAlarm 的
        // typeof 守护姿势，未注入时行为回退到现有内层桶不变。
        if (env.RL_CLAIM?.limit && !(await env.RL_CLAIM.limit({
          key: `c:${request.headers.get("CF-Connecting-IP") || "unknown"}`,
        })).success) {
          return new Response("rate limited", { status: 429 });
        }
        const key = await ipBucketKey(request);
        if (!takeClaimEdgeSlot(key)) {
          return new Response("rate limited", { status: 429 });
        }
      }

      // SEC-2：WS upgrade 路径此前完全无边缘限速——claim 有 per-isolate 内存
      // 桶，upgrade 没有，刷房洪泛第一道门敞开。这里在取 DO stub（会实例化
      // 一个 DO）之前先挡一道原生限速粗网；未注入 RL_UPGRADE 时（如 node:test）
      // 直接放行，行为不变。
      if (request.method === "GET" && request.headers.get("Upgrade") === "websocket") {
        if (env.RL_UPGRADE?.limit && !(await env.RL_UPGRADE.limit({
          key: `u:${request.headers.get("CF-Connecting-IP") || "unknown"}`,
        })).success) {
          return new Response("rate limited", { status: 429 });
        }
      }

      const id = env.ROOMS.idFromName(roomId);
      const stub = env.ROOMS.get(id);
      return stub.fetch(request);
    }

    // 形似房间路径（`/room` 精确路径本身，或 `/room/...`）但房间 id 格式不合法——不校验格式就放行
    // 到静态资源层，会让一个拼错的 `/room`/`/room/xxx` 悄悄拿到 200 的 SPA 壳（T6g1 新增的 SPA
    // fallback 对任何未命中静态文件的路径都会用 index.html 应答），而不是这条路由本来就该给的清楚
    // 404。`/room`（无尾部内容）在改动前也是落这条 404（不匹配 ROOM_PATH_RE），必须显式列进来——
    // 否则它会跟其它路径一样被当成"其余 GET/HEAD"漏到 ASSETS 分流，回归成 200。这条防线在加静态
    // 托管之前就有（见文件头注：不该把「枚举房间 id」的攻击面甩给 DO 层），这里只是确保 T6g1 新增
    // 的 ASSETS 分流不会把它悄悄绕过。
    if (url.pathname === "/room" || url.pathname.startsWith("/room/")) {
      return new Response("not found", { status: 404 });
    }

    // T6g1 · S4 同域静态托管：其余所有 GET/HEAD 请求交给 env.ASSETS（wrangler.toml [assets] 绑定
    // 指向 remote-web 的构建产物 `../remote-web/dist`；`not_found_handling =
    // "single-page-application"` 让任何没命中真实文件的路径都拿到 index.html，200 而非 3xx——
    // 手机端是纯 fragment 路由的 SPA，唯一会被访问的路径就是根路径本身）。安全头统一叠加在这里，
    // 不依赖 [assets] 配置本身能不能设头（Cloudflare Assets 目前没有给 `_headers` 文件之外的
    // 每请求响应头注入点，且 `_headers` 文件不支持这里需要的「按当前请求 host 动态拼 connect-src」）。
    if (request.method !== "GET" && request.method !== "HEAD") {
      return new Response("not found", { status: 404 });
    }
    const assetResponse = await env.ASSETS.fetch(request);
    return withSecurityHeaders(assetResponse, url);
  },
};
