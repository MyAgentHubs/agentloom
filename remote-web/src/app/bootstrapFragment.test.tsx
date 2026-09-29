// bootstrapFragment.test.tsx — T6g1 · 钉死 `defaultGetLocationHref`/`defaultClearFragment` 的
// 「先读 bootstrap 捕获值、退回真实 location」语义，以及既有测试语义不变（`window.__agentloomBoot`
// 不存在时的行为必须跟改动前的 `main.tsx::defaultClearFragment`/`() => window.location.href`
// 完全一致）。jsdom 环境（`.tsx` 后缀落进 vitest.config.ts 的 "ui" project）——本文件不渲染任何
// 组件，只是需要 `window.location`/`window.history`，跟 `src/**/*.test.ts` 的纯 node 环境不兼容。

import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { defaultClearFragment, defaultGetLocationHref } from "./bootstrapFragment.ts";

// jsdom 的 `window.location` 不允许 `history.replaceState` 跨 origin 跳转（同真实浏览器的
// same-origin 限制一致）——所以下面只改同源内的 path/hash，不像别处测试那样自造一个
// `https://relay.example` 假 origin（`getLocationHref`/`clearFragment` 的默认实现本来就不关心
// origin 具体是什么，只是原样转发/清理 `window.location`，用真实 jsdom 默认 origin 测足够）。
function resetLocationPath(pathAndHash: string): void {
  window.history.replaceState(null, "", pathAndHash);
}

describe("defaultGetLocationHref", () => {
  beforeEach(() => {
    delete window.__agentloomBoot;
    resetLocationPath("/");
  });
  afterEach(() => {
    delete window.__agentloomBoot;
  });

  it("优先读 window.__agentloomBoot（bootstrap 内联脚本捕获的原始 href）", () => {
    const captured = `${window.location.origin}/#p=captured-payload`;
    window.__agentloomBoot = captured;
    // 地址栏此刻已经被内联脚本清过——如果实现退回读 location.href 就会读到不含 #p= 的错误值，
    // 这条断言正是要证明它没有退回。
    resetLocationPath("/");
    expect(defaultGetLocationHref()).toBe(captured);
  });

  it("__agentloomBoot 不存在时退回 window.location.href（既有测试语义不变）", () => {
    expect(window.__agentloomBoot).toBeUndefined();
    resetLocationPath("/#p=fallback-payload");
    expect(defaultGetLocationHref()).toBe(`${window.location.origin}/#p=fallback-payload`);
  });

  it("__agentloomBoot 为 null（已被 clearFragment 清过）时退回 window.location.href", () => {
    window.__agentloomBoot = null;
    resetLocationPath("/somewhere");
    expect(defaultGetLocationHref()).toBe(`${window.location.origin}/somewhere`);
  });

  it("__agentloomBoot 为空字符串时视为未捕获，退回 window.location.href", () => {
    window.__agentloomBoot = "";
    resetLocationPath("/somewhere-else");
    expect(defaultGetLocationHref()).toBe(`${window.location.origin}/somewhere-else`);
  });
});

describe("defaultClearFragment", () => {
  afterEach(() => {
    delete window.__agentloomBoot;
  });

  it("清空 window.__agentloomBoot 内存捕获值，防止配对流程重跑时被重复读到", () => {
    window.__agentloomBoot = `${window.location.origin}/#p=captured-payload`;
    defaultClearFragment();
    expect(window.__agentloomBoot).toBeNull();
  });

  it("同时用 history.replaceState 清地址栏 hash（哪怕 __agentloomBoot 本来就没有，仍是幂等兜底）", () => {
    resetLocationPath("/path?x=1#p=stale-hash-in-address-bar");
    defaultClearFragment();
    expect(window.location.hash).toBe("");
    expect(window.location.pathname).toBe("/path");
    expect(window.location.search).toBe("?x=1");
  });
});
