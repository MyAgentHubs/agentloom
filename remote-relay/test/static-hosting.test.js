"use strict";
// static-hosting.test.js — T6g1 · worker 顶层路由分流（src/index.js 的 fetch()）：
//   ① /healthz、② /room/*（含 WS 升级/claim 限速）零回归——继续走原路由、不经过 env.ASSETS；
//   ③ 其余 GET/HEAD 转 env.ASSETS.fetch() 并叠加安全头（SPA fallback、根路径不重定向、
//     /assets/ 与非 /assets/ 的 Cache-Control 分流）；
//   ④ 非 GET/HEAD 的杂项路径维持 404（不误触 ASSETS）。
//
// env.ROOMS 假绑定同 test/s1e-abuse.test.js 的最小形态（只记调没调、返回一个可控 Response）；
// env.ASSETS 假绑定同理——本地 node:test 没有真实 wrangler/Cloudflare Asset Worker 可跑，这里只
// 验证 src/index.js 自己的路由决策（该不该调 ASSETS.fetch、调完有没有正确叠加头），不验证
// Cloudflare 内部 Asset Worker 本身的 SPA fallback/html_handling 实现（那部分是 wrangler 的责任，
// 线上/staging 冒烟验证归 T6g2）。

import { test } from "node:test";
import assert from "node:assert/strict";
import worker from "../src/index.js";
import { buildContentSecurityPolicy } from "../src/security-headers.js";

const ROOM = "0123456789abcdef0123456789abcdef";

// stub 用 200 而不是真实 WS 升级的 101——`new Response(null, {status: 101})` 在 Node/undici 下
// 直接抛 RangeError（101 Switching Protocols 是 Cloudflare Workers 专属的 `{status: 101,
// webSocket}` 构造，Node fetch API 不支持构造这个状态码）。这里只测 src/index.js 的路由决策（该
// 不该把请求转给 DO stub），不测真实 WS 升级握手本身——那部分 Hibernation API 生命周期同
// remote-relay/README.md 文件头注所述，本来就测不进 node:test，只能 `wrangler dev` 手验。
function makeFakeRooms({ stubStatus = 200 } = {}) {
  const idFromNameCalls = [];
  const stubFetchCalls = [];
  return {
    calls: { idFromNameCalls, stubFetchCalls },
    binding: {
      idFromName(name) {
        idFromNameCalls.push(name);
        return { name };
      },
      get(id) {
        return {
          fetch(request) {
            stubFetchCalls.push(request);
            return new Response(null, { status: stubStatus });
          },
        };
      },
    },
  };
}

function makeFakeAssets({ status = 200, contentType = "text/html; charset=utf-8", body = "<html>spa shell</html>" } = {}) {
  const fetchCalls = [];
  return {
    calls: fetchCalls,
    binding: {
      async fetch(request) {
        fetchCalls.push(request);
        return new Response(body, { status, headers: { "content-type": contentType } });
      },
    },
  };
}

function makeEnv(overrides = {}) {
  const rooms = overrides.rooms ?? makeFakeRooms();
  const assets = overrides.assets ?? makeFakeAssets();
  return { env: { ROOMS: rooms.binding, ASSETS: assets.binding }, rooms, assets };
}

// ============================================================================
// ① /healthz — 零回归
// ============================================================================

test("/healthz：不经过 ASSETS/ROOMS，原样 200 ok", async () => {
  const { env, rooms, assets } = makeEnv();
  const response = await worker.fetch(new Request("https://relay.example/healthz"), env);
  assert.equal(response.status, 200);
  assert.equal(await response.text(), "ok");
  assert.equal(rooms.calls.idFromNameCalls.length, 0);
  assert.equal(assets.calls.length, 0);
});

// ============================================================================
// ② /room/* — 零回归（WS 升级路径 + claim 限速 + 房间 id 格式校验），不经过 ASSETS
// ============================================================================

test("/room/<合法id>：分发给 DO stub，不经过 ASSETS（WS 升级路径零回归锚）", async () => {
  const { env, rooms, assets } = makeEnv();
  const response = await worker.fetch(
    new Request(`https://relay.example/room/${ROOM}`, {
      headers: { Upgrade: "websocket", "Sec-WebSocket-Protocol": "agentloom-rc-v1" },
    }),
    env,
  );
  assert.equal(response.status, 200);
  assert.deepEqual(rooms.calls.idFromNameCalls, [ROOM]);
  assert.equal(rooms.calls.stubFetchCalls.length, 1);
  assert.equal(assets.calls.length, 0);
  // 路由决策之外，进一步核实 DO stub 真的原样收到了 WS 升级请求头（不是只测"调没调 fetch"，
  // 而是测传进去的 Request 本身还是完整的 WS 升级请求，没有被 T6g1 新增的分流逻辑吞掉/改写头）。
  const forwardedRequest = rooms.calls.stubFetchCalls[0];
  assert.equal(forwardedRequest.headers.get("Upgrade"), "websocket");
  assert.equal(forwardedRequest.headers.get("Sec-WebSocket-Protocol"), "agentloom-rc-v1");
});

test("/room/<合法id>/claim：POST 仍触发限速账本，不经过 ASSETS", async () => {
  const { env, rooms, assets } = makeEnv();
  const response = await worker.fetch(
    new Request(`https://relay.example/room/${ROOM}/claim`, { method: "POST" }),
    env,
  );
  assert.equal(response.status, 200);
  assert.equal(rooms.calls.stubFetchCalls.length, 1);
  assert.equal(assets.calls.length, 0);
});

test("/room/<不合法格式>：仍是 404，不会被 T6g1 新增的 ASSETS 分流悄悄绕过（不误当 SPA 深链）", async () => {
  const { env, rooms, assets } = makeEnv();
  const response = await worker.fetch(new Request("https://relay.example/room/not-a-valid-room-id"), env);
  assert.equal(response.status, 404);
  assert.equal(rooms.calls.idFromNameCalls.length, 0);
  assert.equal(assets.calls.length, 0, "格式不合法的 /room/ 路径不该触发一次 DO 实例化，也不该被当成静态资源/SPA 深链放行");
});

test("GET /room（精确路径本身，无尾部内容）：仍是 404，不落 T6g1 新增的 SPA fallback 200（差量返工点名回归）", async () => {
  const { env, rooms, assets } = makeEnv();
  const response = await worker.fetch(new Request("https://relay.example/room"), env);
  assert.equal(response.status, 404);
  assert.equal(rooms.calls.idFromNameCalls.length, 0);
  assert.equal(assets.calls.length, 0, "/room 精确路径不该落到 ASSETS 分流拿到 SPA 壳的 200");
});

test("HEAD /room（精确路径本身）：同上，仍是 404", async () => {
  const { env, rooms, assets } = makeEnv();
  const response = await worker.fetch(new Request("https://relay.example/room", { method: "HEAD" }), env);
  assert.equal(response.status, 404);
  assert.equal(rooms.calls.idFromNameCalls.length, 0);
  assert.equal(assets.calls.length, 0);
});

// ============================================================================
// ③ 其余 GET/HEAD → env.ASSETS.fetch + 安全头
// ============================================================================

test("GET /：转发 env.ASSETS.fetch，200、无 3xx/Location，四类安全头齐全", async () => {
  const { env, assets } = makeEnv();
  const response = await worker.fetch(new Request("https://relay.example.com/"), env);

  assert.equal(assets.calls.length, 1);
  assert.equal(response.status, 200);
  assert.equal(response.headers.get("location"), null);
  assert.equal(
    response.headers.get("Content-Security-Policy"),
    buildContentSecurityPolicy("relay.example.com"),
  );
  assert.equal(response.headers.get("Referrer-Policy"), "no-referrer");
  assert.equal(response.headers.get("X-Content-Type-Options"), "nosniff");
  assert.equal(response.headers.get("Cache-Control"), "no-store");
  assert.equal(await response.text(), "<html>spa shell</html>");
});

test("GET /some/deep/fragment-only-spa/path：SPA fallback 场景（模拟 ASSETS 对未命中路径回 index.html·200）同样安全头齐全、no-store", async () => {
  const { env, assets } = makeEnv();
  const response = await worker.fetch(
    new Request("https://relay.example.com/some/deep/fragment-only-spa/path"),
    env,
  );
  assert.equal(assets.calls.length, 1);
  assert.equal(response.status, 200);
  assert.equal(response.headers.get("Cache-Control"), "no-store");
  assert.ok(response.headers.get("Content-Security-Policy"));
});

test("GET /assets/index-BTlQZ0UP.js（真实内容 hash 长度形态）：长缓存 immutable，安全头仍统一叠加", async () => {
  const { env, assets } = makeEnv({
    assets: makeFakeAssets({ contentType: "text/javascript; charset=utf-8", body: "console.log(1)" }),
  });
  const response = await worker.fetch(
    new Request("https://relay.example.com/assets/index-BTlQZ0UP.js"),
    env,
  );
  assert.equal(response.headers.get("Cache-Control"), "public, max-age=31536000, immutable");
  assert.equal(response.headers.get("Referrer-Policy"), "no-referrer");
  assert.equal(response.headers.get("X-Content-Type-Options"), "nosniff");
});

test("GET /assets/index.js（/assets/ 目录下但无内容 hash 后缀）：no-store，不因目录前缀误判 immutable", async () => {
  const { env, assets } = makeEnv({
    assets: makeFakeAssets({ contentType: "text/javascript; charset=utf-8", body: "console.log(1)" }),
  });
  const response = await worker.fetch(
    new Request("https://relay.example.com/assets/index.js"),
    env,
  );
  assert.equal(response.headers.get("Cache-Control"), "no-store");
});

test("GET /assets/logo.svg（/assets/ 目录下但无内容 hash 后缀）：no-store，不因目录前缀误判 immutable", async () => {
  const { env, assets } = makeEnv({
    assets: makeFakeAssets({ contentType: "image/svg+xml", body: "<svg></svg>" }),
  });
  const response = await worker.fetch(
    new Request("https://relay.example.com/assets/logo.svg"),
    env,
  );
  assert.equal(response.headers.get("Cache-Control"), "no-store");
});

test("HEAD /：也转发给 ASSETS（HEAD 是 GET 的自然搭档，静态资源探测/预检常用）", async () => {
  const { env, assets } = makeEnv();
  const response = await worker.fetch(
    new Request("https://relay.example.com/", { method: "HEAD" }),
    env,
  );
  assert.equal(assets.calls.length, 1);
  assert.equal(response.status, 200);
});

// ============================================================================
// ④ 非 GET/HEAD 的杂项路径 — 不误触 ASSETS
// ============================================================================

test("POST /（非 room、非 healthz、非 GET/HEAD）：404，不调用 ASSETS.fetch", async () => {
  const { env, assets } = makeEnv();
  const response = await worker.fetch(new Request("https://relay.example/", { method: "POST" }), env);
  assert.equal(response.status, 404);
  assert.equal(assets.calls.length, 0);
});
