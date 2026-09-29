// usePairingSession.test.tsx — TDD 覆盖 usePairingSession.ts。
//
// 这里不重新证明 PairingSession 本身的协议正确性（pairing-session.test.ts 已经用独立复算的假桌面
// 详尽覆盖过状态表）——只证明"React 包装层会不会正确反应 phase 变化"这条本单新增的接线：
//   1. start() 完成后 phase 从 idle 变成 awaiting_accept，hook 使用者能看到重渲染后的新值；
//   2. dispatch() 喂一帧不需要密码学的 error{device_revoked} 帧，phase 变成 needs_repair 并带出
//      revocationReason，同样能看到重渲染；
//   3. StrictMode 下 effect 双调不会导致 PairingSession.start() 被真的调用两次（不会抛"phase 不是
//      idle"），出站帧只发一次；且第二次 setup 的 cleanup 不会把回调链路"消音"——phase 最终必须
//      到达 awaiting_accept（差量返工修的那个 bug：见下）。
//
// **StrictMode 测试用 `render()` 包一个探针组件，不用 `renderHook()`（差量返工踩出的坑）**：实测
// `@testing-library/react` v16.3.2 的 `renderHook()` 配 `wrapper: <StrictMode>` 时，effect 只跑
// 了一次，没有触发 React 18/19 StrictMode 该有的"setup→cleanup→setup"双调（原因不明，可能是
// `renderHook` 内部 TestHook 组件与 StrictMode 双调机制的已知落差）；同样的探针逻辑换成
// `render(<StrictMode><Probe/></StrictMode>)` 就能实测到双调（`setup#1 → cleanup → setup#2`，
// 已用最小复现验证过）。旧版本这里用的正是 `renderHook`，导致"StrictMode 下 phase 永远卡 idle"
// 这个真 bug 完全没被测试网住——测试本身是假绿，不是产代码没问题。
//
// ============================================================================
// Regression coverage prevents subscription loss under StrictMode.
// ============================================================================
// 旧实现：`useEffect` 用一个 `startedRef`（boolean）当"只调一次 start()"的闸——但 StrictMode 双调
// 时，第二次 setup 发现 `startedRef.current` 已经是 true，直接跳过整段 `if` 块，**根本不会给
// `session.start()` 的 promise 挂上第二个 `.then()`**。第一次 setup 挂的那个 `.then()` 闭包里的
// `cancelled` 在 cleanup 时已经被置 true，promise resolve 时判断 `if (!cancelled)` 直接跳过
// `setPhase`——两次 setup 加起来，没有任何一次真正把 `phase` state 更新，UI 永远停在 "idle"。
// 新实现：把"是否已经调用 start()"与"这次 effect 要不要订阅结果"拆成两件事——`startPromiseRef`
// 缓存**promise 本身**（只创建一次），但**每次 effect setup 都对这个缓存的 promise 挂一个新的
// `.then()`**（各自用自己这次调用的 `cancelled` 闭包）。这样不管 StrictMode 触发几次 setup，最后
// 一次（真正存活、没被 cleanup 的那次）的订阅一定会在 promise resolve 时把 phase 更新到位。
//
// 红→绿轨迹（本文件最后一条 "StrictMode: ..." 用例）：
//   - 旧实现 + 新测试：`waitFor(() => phase === "awaiting_accept")` 超时失败（10s 门禁内 phase 停
//     留 "idle" 不动）——`npx vitest run src/ui/pairing/usePairingSession.test.tsx -t StrictMode`
//     实测转红，记录见 worker 报告。
//   - 新实现 + 新测试：同一条用例转绿，且 `transport.sent` 长度仍为 1（`session.start()` 本身没有
//     被多调——只是订阅链路补上了），证明修复没有引入"双发 pair.hello"这个新问题。

import { StrictMode } from "react";
import { act, render, renderHook, waitFor } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { generateKeyPair } from "../../crypto/x25519.ts";
import { bytesToBase64 } from "../../crypto/bytes.ts";
import { InMemoryKeyStore } from "../../store/key-store.ts";
import type { KeyStorePort } from "../../store/key-store.ts";
import type { QrPayload } from "../../pairing/qr-payload.ts";
import type { PairingTransportPort } from "../../pairing/pairing-session.ts";
import { createNoopPairingTransport } from "./stubTransport.ts";
import { usePairingSession, type UsePairingSessionResult } from "./usePairingSession.ts";

function makeQr(): QrPayload {
  return {
    v: 1,
    relay_url: "wss://relay.example",
    room: "0123456789abcdef0123456789abcdef",
    pairing_token: "a".repeat(64),
    desktop_pub: bytesToBase64(generateKeyPair().publicKey),
  };
}

/** StrictMode 探针——见文件头注："renderHook() 配 StrictMode 不会真的双调 effect"，只有把 hook
 *  包进一个用 `render()` 挂载的普通组件里才能实测到双调。`onUpdate` 在每次渲染时都把最新结果同步
 *  写进调用方传入的可变容器，供测试用 `waitFor` 轮询。 */
function ProbeUsePairingSession({
  qr,
  keyStore,
  transport,
  onUpdate,
}: {
  qr: QrPayload;
  keyStore: KeyStorePort;
  transport: PairingTransportPort;
  onUpdate: (result: UsePairingSessionResult) => void;
}) {
  const result = usePairingSession(qr, keyStore, transport);
  onUpdate(result);
  return null;
}

describe("usePairingSession()", () => {
  it("moves idle -> awaiting_accept once start() resolves, and sends exactly one pair.hello", async () => {
    const transport = createNoopPairingTransport();
    const { result } = renderHook(() => usePairingSession(makeQr(), new InMemoryKeyStore(), transport));

    expect(result.current.phase).toBe("idle");

    await waitFor(() => expect(result.current.phase).toBe("awaiting_accept"));

    expect(transport.sent).toHaveLength(1);
    expect(transport.sent[0]?.t).toBe("pair.hello");
    expect(result.current.fatalError).toBeNull();
  });

  it("dispatch() driving a device_revoked error frame re-renders into needs_repair", async () => {
    const transport = createNoopPairingTransport();
    const { result } = renderHook(() => usePairingSession(makeQr(), new InMemoryKeyStore(), transport));
    await waitFor(() => expect(result.current.phase).toBe("awaiting_accept"));

    let outcome;
    await act(async () => {
      outcome = await result.current.dispatch({ t: "error", reason: "device_revoked" });
    });

    expect(outcome).toEqual({ status: "applied" });
    expect(result.current.phase).toBe("needs_repair");
    expect(result.current.revocationReason).toBe("device_revoked");
  });

  it("dispatch() with an unrelated error reason is ignored and phase stays put", async () => {
    const transport = createNoopPairingTransport();
    const { result } = renderHook(() => usePairingSession(makeQr(), new InMemoryKeyStore(), transport));
    await waitFor(() => expect(result.current.phase).toBe("awaiting_accept"));

    await act(async () => {
      await result.current.dispatch({ t: "error", reason: "bad_json" });
    });

    expect(result.current.phase).toBe("awaiting_accept");
    expect(result.current.revocationReason).toBeNull();
  });

  it("StrictMode: phase reaches awaiting_accept despite the duplicated effect setup, and start() is not double-invoked", async () => {
    const transport = createNoopPairingTransport();
    const qr = makeQr();
    const keyStore = new InMemoryKeyStore();
    // 用可变对象的属性而不是裸 `let` 变量承接闭包写入的最新结果——`let` 变量只在闭包外的
    // 直线控制流里被读时，tsc 有时会按"这个作用域里没看到别的赋值"把类型收窄掉（在这个文件里实测
    // 触发过"收窄成 never、后续属性访问报错"的坑），对象属性访问不吃这个收窄规则。
    const captured: { latest: UsePairingSessionResult | null } = { latest: null };

    render(
      <StrictMode>
        <ProbeUsePairingSession
          qr={qr}
          keyStore={keyStore}
          transport={transport}
          onUpdate={(result) => {
            captured.latest = result;
          }}
        />
      </StrictMode>,
    );

    // 旧实现下这里会一直卡 idle 直到 waitFor 超时（见文件头注的红→绿轨迹）。
    await waitFor(() => expect(captured.latest?.phase).toBe("awaiting_accept"));

    // session.start() 本身必须只被真正调用一次——修复的是"订阅丢失"，不是靠"多发一次 pair.hello"
    // 侥幸把 phase 带对；StrictMode 双调时第二次 setup 必须跳过重新调用 start()。
    expect(transport.sent).toHaveLength(1);
    expect(transport.sent[0]?.t).toBe("pair.hello");
    expect(captured.latest?.fatalError).toBeNull();
  });
});
