// SEC-2：边缘原生限速 binding（RL_CLAIM / RL_UPGRADE）守卫式接入的行为测试。
// env 手搓模板照 s1e-abuse.test.js——不自建新框架。这里只测 index.js 这一
// 层的分流逻辑（拒绝时是否提前 return、放行时是否照常转 DO stub），不复现
// room-do.js 内的确定性桶（那部分已有 s1e-abuse.test.js 覆盖）。

import { test } from "node:test";
import assert from "node:assert/strict";
import worker from "../src/index.js";

const ROOM = "abcdef0123456789abcdef0123456789";

function makeRoomsBinding() {
  // counts 是共享的可变对象，不是解构出的原始值快照——闭包递增后，测试侧
  // 读同一个对象的字段才能看到最新计数（解构 getter 到局部变量只会在解构
  // 那一刻求值一次，之后永远读到旧快照，是本文件写这版之前踩过的坑）。
  const counts = { idFromName: 0, fetch: 0 };
  const rooms = {
    idFromName(name) {
      counts.idFromName += 1;
      return name;
    },
    get() {
      return {
        fetch() {
          counts.fetch += 1;
          return new Response("ok", { status: 200 });
        },
      };
    },
  };
  return { rooms, counts };
}

function upgradeRequest(ip) {
  return new Request(`https://relay.example/room/${ROOM}`, {
    method: "GET",
    headers: {
      Upgrade: "websocket",
      "CF-Connecting-IP": ip,
    },
  });
}

function claimRequest(ip) {
  return new Request(`https://relay.example/room/${ROOM}/claim`, {
    method: "POST",
    headers: { "CF-Connecting-IP": ip },
  });
}

test("RL_UPGRADE 拒绝：upgrade 请求断 429 且 DO stub 未被取（idFromName 未调用）", async () => {
  const { rooms, counts } = makeRoomsBinding();
  const env = {
    ROOMS: rooms,
    RL_UPGRADE: { limit: async () => ({ success: false }) },
  };
  const response = await worker.fetch(upgradeRequest("198.51.100.240"), env);
  assert.equal(response.status, 429);
  assert.equal(counts.idFromName, 0);
  assert.equal(counts.fetch, 0);
});

test("RL_UPGRADE 放行：success true 时照常转 DO stub", async () => {
  const { rooms, counts } = makeRoomsBinding();
  const env = {
    ROOMS: rooms,
    RL_UPGRADE: { limit: async () => ({ success: true }) },
  };
  const response = await worker.fetch(upgradeRequest("198.51.100.241"), env);
  assert.equal(response.status, 200);
  assert.equal(counts.idFromName, 1);
  assert.equal(counts.fetch, 1);
});

test("RL_CLAIM 拒绝：claim 请求断 429 且 DO stub 未被取（idFromName 未调用）", async () => {
  const { rooms, counts } = makeRoomsBinding();
  const env = {
    ROOMS: rooms,
    RL_CLAIM: { limit: async () => ({ success: false }) },
  };
  const response = await worker.fetch(claimRequest("198.51.100.242"), env);
  assert.equal(response.status, 429);
  assert.equal(counts.idFromName, 0);
  assert.equal(counts.fetch, 0);
});

test("RL_CLAIM 放行：success true 时照常转 DO stub", async () => {
  const { rooms, counts } = makeRoomsBinding();
  const env = {
    ROOMS: rooms,
    RL_CLAIM: { limit: async () => ({ success: true }) },
  };
  const response = await worker.fetch(claimRequest("198.51.100.243"), env);
  assert.equal(response.status, 200);
  assert.equal(counts.idFromName, 1);
  assert.equal(counts.fetch, 1);
});

test("未注入任何 RL binding：upgrade 与 claim 都回退到现有行为（claimEdgeBuckets 内层桶仍生效）", async () => {
  const { rooms, counts } = makeRoomsBinding();
  const env = { ROOMS: rooms };

  const upgrade = await worker.fetch(upgradeRequest("198.51.100.244"), env);
  assert.equal(upgrade.status, 200);
  assert.equal(counts.idFromName, 1);
  assert.equal(counts.fetch, 1);

  const ip = "198.51.100.245";
  for (let attempt = 1; attempt <= 10; attempt += 1) {
    const response = await worker.fetch(claimRequest(ip), env);
    assert.equal(response.status, 200, `attempt ${attempt}`);
  }
  const limited = await worker.fetch(claimRequest(ip), env);
  assert.equal(limited.status, 429, "既有 claimEdgeBuckets 10/min 内层桶未注入 RL binding 时仍生效");
});
