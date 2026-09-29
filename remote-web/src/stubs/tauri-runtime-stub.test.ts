// tauri-runtime-stub.test.ts — T6f2 · 「stub 模块具名导出被调用即 throw」单测
// （任务书 §2 硬要求：guard 精化后 `@app` 域 importer 的 @tauri-apps/* import 会 resolve 到
// `tauri-runtime-stub.ts`；这份测试证明该模块本身的契约成立——每个具名导出被调用时都清晰报错，
// 不会静默返回、不会在渲染路径里制造"看起来正常，实际打不开"的假象）。
//
// 覆盖两类真实调用形状（见 tauri-runtime-stub.ts 头注"返回形态钉死为拒绝态 Promise"一节）：
//   - async 符号（invoke/listen/open/save/openPath/openUrl）：调用返回一个 rejected promise，
//     兼容 `await ... catch{}` 与 `.then().catch()` 两种既有调用形状——不能同步 throw（那会在
//     `.then` 链上之前就把异常摔出调用者函数体，见头注）。
//   - `getCurrentWindow`：真实签名同步返回 `Window` 实例，桩同步 throw 忠实契约。
//
// 变异自证（worker 报告 ⑤）：把 `asyncThrowStub` 的 `Promise.reject(...)` 手动改成
// `Promise.resolve(undefined)` 观察下面「每个 async 符号都必须 reject」的测试转红（验完已还原，
// 不作为提交内容）——证明这条测试真的在断言"拒绝"而不是"resolves 到某个值"这种弱化写法也能通过。

import { describe, expect, it } from "vitest";
import * as stub from "./tauri-runtime-stub.ts";

const ASYNC_EXPORT_NAMES = [
  "invoke",
  "listen",
  "open",
  "save",
  "openPath",
  "openUrl",
  "writeImage",
] as const;

describe("tauri-runtime-stub: every named export throws a clear error when actually called", () => {
  it.each(ASYNC_EXPORT_NAMES)("%s(...) rejects (does not resolve, does not throw synchronously)", async (name) => {
    const fn = stub[name] as (...args: unknown[]) => Promise<never>;

    // 必须不同步 throw——同步抛出会让下面的 rejects 断言本身都跑不到（证明"返回 rejected promise
    // 而不是同步 throw"这条约束的负例：如果哪天有人把某个符号改回同步 throw，这一行会先炸掉）。
    let returned: Promise<never>;
    expect(() => {
      returned = fn("some-command", { some: "arg" });
    }).not.toThrow();

    await expect(returned!).rejects.toThrow(/desktop-only Tauri API/);
    await expect(returned!).rejects.toThrow(new RegExp(`"${name}"`));
  });

  it("getCurrentWindow() throws synchronously (real API is sync, stub mirrors that)", () => {
    expect(() => stub.getCurrentWindow()).toThrow(/desktop-only Tauri API/);
    expect(() => stub.getCurrentWindow()).toThrow(/"getCurrentWindow"/);
  });

  it("isTauri() is a truthful sync query, not an action — always returns false, never throws", () => {
    expect(stub.isTauri()).toBe(false);
  });

  it("Image.fromBytes(...) rejects like the other async stubs", async () => {
    await expect(stub.Image.fromBytes(new Uint8Array())).rejects.toThrow(
      /desktop-only Tauri API/,
    );
    await expect(stub.Image.fromBytes(new Uint8Array())).rejects.toThrow(
      /"Image\.fromBytes"/,
    );
  });

  it("sanity: covers every symbol app/src actually imports from @tauri-apps/*", () => {
    // 见 tauri-runtime-stub.ts 头注"导出符号清单来源"——这条测试只是把那份 grep 结果钉成可回归的
    // 断言：往 app/src 加一个新的 @tauri-apps/* 符号却忘了在这里补桩，本测试不会自动发现（grep 是
    // 人工跑的），但至少保证现有清单不会被悄悄削掉。
    const exported = Object.keys(stub).sort();
    expect(exported).toEqual(
      [
        "getCurrentWindow",
        "invoke",
        "listen",
        "open",
        "openPath",
        "openUrl",
        "save",
        "isTauri",
        "Image",
        "writeImage",
      ].sort(),
    );
  });
});
