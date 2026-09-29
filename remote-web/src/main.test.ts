// @vitest-environment jsdom
// These tests cover the top-level async IIFE fallback in main.tsx to prevent startup failures from leaving a blank screen.
// 同族教训：入口模块顶层副作用一律 try/catch，可选设施失败必须静默降级）。
//
// `createStoreFactory()` 理论上不该抛（`store/idbFactory.ts::probeIndexedDb()` 自己已经把"探测
// 失败"收口成降级返回内存实现的工厂集合）——这里防的是"万一还有什么没预料到的东西炸了"。旧版
// `main.tsx` 整段 async IIFE 没有 try/catch，一次 reject 就是一条永远没人接的 unhandled
// rejection：`render()` 永远不会被调用，`#root` 永远停在空 `<div>`，用户看到的是一片白屏。
//
// 落在 `src/` 顶层（不在 `src/ui/**`/`src/app/**`），按 `vitest.config.ts` 的 project 划分默认走
// "logic" project 的 node 环境——用文件头 `@vitest-environment jsdom` 指令覆盖成 jsdom（`main.tsx`
// 需要 `document.getElementById("root")`），不需要为这一个文件改动 project include glob。

import { beforeEach, describe, expect, it, vi } from "vitest";
import type { ReactElement } from "react";

interface CapturedRootRouterProps {
  deps: {
    idbAvailable: boolean;
    keyStore: { constructor: { name: string } };
    createEventStore: (room: string) => { constructor: { name: string } };
  };
}

const renders: CapturedRootRouterProps[] = [];

vi.mock("react-dom/client", () => ({
  createRoot: () => ({
    render: (node: ReactElement) => {
      // node 是 `<StrictMode><RootRouter deps={...} /></StrictMode>`——`props.children` 是内层的
      // `<RootRouter>` 元素，`.props` 就是本文件关心的 `{ deps }`。不需要真的挂载/渲染，只是拿到
      // `main.tsx` 传给 `RootRouter` 的那份 props 原样断言。
      const child = (node.props as { children: ReactElement }).children;
      renders.push(child.props as CapturedRootRouterProps);
    },
  }),
}));

vi.mock("./app/browserWebSocketFactory.ts", () => ({
  createBrowserWebSocketFactory: () => () => {
    throw new Error("not used in this test — main.test.ts never opens a real socket");
  },
}));

vi.mock("./app/bootstrapFragment.ts", () => ({
  defaultGetLocationHref: () => "http://localhost/",
  defaultClearFragment: () => {},
}));

vi.mock("./store/idbFactory.ts", () => ({
  createStoreFactory: () => Promise.reject(new Error("boom (test): unexpected probe crash, not the known probe-failure-degrades-to-memory path")),
}));

describe("main.tsx 顶层 async IIFE（msgfix2 F2 S5①）", () => {
  beforeEach(() => {
    renders.length = 0;
    document.body.innerHTML = '<div id="root"></div>';
  });

  it("createStoreFactory() 抛出一个意外错误——不再是永远白屏的未处理 rejection，catch 住后退化成纯内存 deps 再 render 一次", async () => {
    const consoleErrorSpy = vi.spyOn(console, "error").mockImplementation(() => {});

    await import("./main.tsx");
    // async IIFE 里的 `await createStoreFactory()` 拒绝后还要走 catch 分支再 render 一次——给
    // 微任务队列一点机会落地，不假设 `import()` resolve 那一刻同步做完了。
    await vi.waitFor(() => expect(renders.length).toBeGreaterThan(0));

    // 核心断言①：`render()` 真的被调用过——不是永远停在空 `<div>` 的白屏。
    const { deps } = renders[0]!;
    // 核心断言②：退化路径干净利落——彻底不碰 IndexedDB（`idbAvailable:false` + 内存实现），
    // 不是"假装探测成功但半吊子"。
    expect(deps.idbAvailable).toBe(false);
    expect(deps.keyStore.constructor.name).toBe("InMemoryKeyStore");
    expect(deps.createEventStore("room-x").constructor.name).toBe("InMemoryEventStore");
    // 核心断言③：留了一条可见日志（不是静默吞掉——同 CLAUDE.md「静默 fail-open 最危险」教训）。
    expect(consoleErrorSpy).toHaveBeenCalled();

    consoleErrorSpy.mockRestore();
  });
});
