// main.tsx — T6f1 · C1 Web 入口。T6f2 追加已配对冷启动跳转；INT1 把未配对分支的 stub transport
// 换成真 WebSocket（`app/RealPairingFlow.tsx`）；INT1b 接上已配对运行时（`app/AppRuntime.tsx`）；
// INT1c 审查返工把顶层路由状态机整个抽成 `app/RootRouter.tsx`（可注入依赖、可独立测试——本文件
// 现在只是拿真实依赖调用它的薄 bootstrap，不再自己持有任何状态/逻辑）。
//
// msgfix2 U4（收拢 P1-3）：`keyStore`/`createEventStore`/`createCommandLedger`/`createBodyCache`
// 四个存储依赖不再各自零散构造——改由 `store/idbFactory.ts::createStoreFactory()` 统一探测
// （`indexedDB.open` 试开一次）再装配，探测失败时整套换成内存实现（`InMemoryKeyStore`/
// `InMemoryEventStore`/`InMemoryCommandLedger`/`InMemoryBodyCache`），不是某一个存储各自零散
// try/catch。探测是异步的——`render()` 延后到 `createStoreFactory()` resolve 之后，用一个立即执行
// 的 async 函数包裹（顶层 await 在这版 Vite/浏览器目标下可用，但显式 IIFE 更不挑目标环境，同本文件
// 一贯"薄 bootstrap、不引入额外复杂度"的取向）。
import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { createStoreFactory, type StoreFactory } from "./store/idbFactory.ts";
import { InMemoryKeyStore } from "./store/key-store.ts";
import { InMemoryEventStore } from "./store/inMemoryEventStore.ts";
import { InMemoryCommandLedger } from "./store/commandLedger.ts";
import { InMemoryBodyCache } from "./store/bodyCache.ts";
import { RootRouter, type RootRouterDeps } from "./app/RootRouter.tsx";
import { createBrowserWebSocketFactory } from "./app/browserWebSocketFactory.ts";
import { defaultGetLocationHref, defaultClearFragment } from "./app/bootstrapFragment.ts";
import "./ui/tokens.css";

const rootEl = document.getElementById("root");
if (!rootEl) {
  throw new Error("#root element not found");
}

function renderRoot(el: HTMLElement, deps: RootRouterDeps): void {
  createRoot(el).render(
    <StrictMode>
      <RootRouter deps={deps} />
    </StrictMode>,
  );
}

function depsFromStoreFactory(storeFactory: StoreFactory): RootRouterDeps {
  // `keyStore` 同时喂配对阶段（`RealPairingFlow`）与已配对运行时（`AppRuntime`，供
  // `ConnectionSession` 做 refresh 持久化）——两处用同一个实例，无状态，复用无害。
  // `createEventStore`/`createCommandLedger`/`createBodyCache` 按房间派生库名（INT1c P0④ +
  // 本单新增两个：换房不串库），不构造一个跨重配对复用的默认库实例。
  return {
    keyStore: storeFactory.keyStore,
    webSocketFactory: createBrowserWebSocketFactory(),
    createEventStore: storeFactory.createEventStore,
    createCommandLedger: storeFactory.createCommandLedger,
    createBodyCache: storeFactory.createBodyCache,
    getLocationHref: defaultGetLocationHref,
    clearFragment: defaultClearFragment,
    idbAvailable: storeFactory.idbAvailable,
  };
}

void (async () => {
  try {
    // 探测是异步的——`render()` 延后到 `createStoreFactory()` resolve 之后，用一个立即执行的
    // async 函数包裹（顶层 await 在这版 Vite/浏览器目标下可用，但显式 IIFE 更不挑目标环境，同
    // 本文件一贯"薄 bootstrap、不引入额外复杂度"的取向）。
    const storeFactory = await createStoreFactory();
    renderRoot(rootEl, depsFromStoreFactory(storeFactory));
  } catch (error) {
    // msgfix2 F2 S5①：`createStoreFactory()` 理论上不该抛——`store/idbFactory.ts::
    // probeIndexedDb()` 自己已经把"探测失败"收口成降级返回内存实现的工厂集合，不是往外抛错。
    // This catch prevents unexpected store initialization failures from aborting application startup.
    // 项目记忆「mock 比真实运行时宽容」一条：入口模块顶层副作用一律 try/catch，可选设施失败必须
    // 静默降级）。旧版这整段 async IIFE 没有任何 try/catch，一次 reject 就是一条永远没人接的
    // unhandled rejection——`render()` 永远不会被调用，`#root` 永远停在空 `<div>`，用户看到的是
    // 一片白屏且连一条错误信息都没有。这里 catch 住、留一条可见日志，退化成彻底不碰 IndexedDB 的
    // 纯内存 deps 再 render 一次——至少能看到配对屏，不是死白屏。
    console.error("msgfix2 F2 S5①: createStoreFactory() failed unexpectedly; falling back to pure in-memory deps (no IndexedDB at all)", error);
    renderRoot(rootEl, {
      keyStore: new InMemoryKeyStore(),
      webSocketFactory: createBrowserWebSocketFactory(),
      createEventStore: () => new InMemoryEventStore(),
      createCommandLedger: () => new InMemoryCommandLedger(),
      createBodyCache: () => new InMemoryBodyCache(),
      getLocationHref: defaultGetLocationHref,
      clearFragment: defaultClearFragment,
      idbAvailable: false,
    });
  }
})();
