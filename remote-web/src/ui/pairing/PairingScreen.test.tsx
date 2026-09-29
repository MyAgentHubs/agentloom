// PairingScreen.test.tsx — TDD 覆盖 PairingScreen.tsx（任务书 §2 的"四态渲染 + fragment 引导/
// 清除 + 畸形 payload 错误分支"）。
//
// ============================================================================
// 覆盖表（四态 + 引导分支）
// ============================================================================
// 无 fragment + keyStore 空          → manual-entry（ManualEntryForm）
// 无 fragment + keyStore 有既有凭据  → paired（PairedPlaceholder，冷启动路径）
// 有效 fragment                      → pairing → PairingSession.start() resolve 后落 progress
//                                       （awaiting_accept，PairingProgressView）
// 畸形 fragment（JSON 坏）           → qr-error / category=format
// fragment 里 relay_url 与外层 https origin 不符 → qr-error / category=origin_mismatch
// fragment 卫生                      → clearFragment 恰好调用一次，且发生在 parse 结果落地之前
//                                       （不管 parse 成不成功都清）
// 手输表单：畸形输入 → 内联错误（category=format）；有效输入 → 转 pairing 态
// StrictMode：fragment 引导态不被第二次 effect setup 覆盖（差量返工回归测试，见下）
//
// ============================================================================
// 变异自证（任务书 §4 ④·本文件覆盖的一条，另两条见 usePairingSession.test.tsx /
// qrPayloadErrorClassifier.test.ts 头注）
// ============================================================================
// 手动把 PairingScreen.tsx 里 `resolved.clearFragment()` 那一行注释掉——"fragment 卫生"那组测试
// 转红（`clearFragment` 断言 `toHaveBeenCalledTimes(1)` 收到 0）。改后跑
// `npx vitest run src/ui/pairing/PairingScreen.test.tsx` 确认转红，再改回来复跑转绿；过程与结果
// 记入 worker 报告 ⑤，代码已还原。
//
// ============================================================================
// StrictMode regression: fragment bootstrap state must survive repeated effect execution.
// ============================================================================
// 旧实现把"读 href + parse + 清 hash + setScreen"整段定义并调用在 effect 内部——StrictMode 双调
// 时第二次 setup 会把这整段逻辑重新跑一遍。这段逻辑**不是幂等的**：第一次 setup 已经同步调用过
// `clearFragment()`，真实浏览器里 `location.href` 已经不再含 `#p=`；第二次 setup 重新读
// `getLocationHref()`，落进"没有 fragment"分支，转而查 `keyStore`（这次会话还没配对成功，通常查
// 不到），把已经设成功的 "pairing" screen 覆盖回 "manual-entry"。
//
// 下面这条测试用**有状态**的 `getLocationHref`（调用 `clearFragment()` 之后变'看得见'到"fragment
// 已经不在了"）复刻这个动态——用之前那种"每次都返回同一个固定字符串"的 `getLocationHref` 测不出
// 这个 bug（重新算一遍逻辑，因为看到的还是同一个 href，会算出同一个结果，纯属侥幸躲过）。
//
// 红→绿轨迹：
//   - 旧实现 + 新测试：`waitFor` 断言 progress 态最终稳定，但 3 秒观察窗口内 screen 被第二次 setup
//     覆盖成 manual-entry，断言失败——`npx vitest run src/ui/pairing/PairingScreen.test.tsx -t
//     StrictMode` 实测转红，记录见 worker 报告。
//   - 新实现 + 新测试：同一条用例转绿，且 `clearFragment` 仍只被调用 1 次。

import { StrictMode } from "react";
import { cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";
import { generateKeyPair } from "../../crypto/x25519.ts";
import { bytesToBase64 } from "../../crypto/bytes.ts";
import { importNonExtractableAesGcmKey, InMemoryKeyStore, type StoredPairingCredentials } from "../../store/key-store.ts";
import type { QrPayload } from "../../pairing/qr-payload.ts";
import { createNoopPairingTransport } from "./stubTransport.ts";
import { PairingScreen, type PairingScreenDeps } from "./PairingScreen.tsx";

const RELAY_HOST = "relay.example";
const RELAY_URL = `wss://${RELAY_HOST}`;

function makeQr(overrides: Partial<QrPayload> = {}): QrPayload {
  return {
    v: 1,
    relay_url: RELAY_URL,
    room: "0123456789abcdef0123456789abcdef",
    pairing_token: "a".repeat(64),
    desktop_pub: bytesToBase64(generateKeyPair().publicKey),
    ...overrides,
  };
}

function toBase64Url(text: string): string {
  const binary = btoa(text);
  return binary.replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
}

function fragmentUrl(httpsOrigin: string, payloadJson: string): string {
  return `${httpsOrigin}/#p=${toBase64Url(payloadJson)}`;
}

async function makeStoredCredentials(): Promise<StoredPairingCredentials> {
  const kRoomKey = await importNonExtractableAesGcmKey(new Uint8Array(32).fill(3));
  return {
    deviceId: "11111111-1111-4111-8111-111111111111",
    room: "0123456789abcdef0123456789abcdef",
    relayUrl: RELAY_URL,
    access: "a".repeat(64),
    refresh: "b".repeat(64),
    kRoomKey,
  };
}

/**
 * 模拟真实浏览器里 `clearFragment()` 真的改了地址栏这件事——`getLocationHref()` 在
 * `clearFragment()` 被调用过之后，就切换到返回"fragment 已经不在了"的那个 href。跟测试文件里其它
 * 用例的"固定返回同一个字符串"的 `getLocationHref` 不同，这个专门用来复刻 StrictMode 双调 bug 的
 * 真实前置条件（见文件头注"差量返工"段）。
 */
function statefulLocationHref(hrefWithFragment: string, hrefAfterClear: string) {
  let cleared = false;
  return {
    getLocationHref: () => (cleared ? hrefAfterClear : hrefWithFragment),
    clearFragment: vi.fn(() => {
      cleared = true;
    }),
  };
}

function testDeps(overrides: Partial<PairingScreenDeps> = {}): PairingScreenDeps {
  return {
    keyStore: new InMemoryKeyStore(),
    createTransport: createNoopPairingTransport,
    getLocationHref: () => "https://web.example/",
    clearFragment: () => {},
    ...overrides,
  };
}

describe("PairingScreen()", () => {
  // RTL 不会自动在测试之间清 DOM——本项目的 vitest 配置没开 `test.globals`（保持既有 271 个逻辑
  // 测试的显式 import 习惯不变），RTL 的自动 cleanup 依赖全局 `afterEach` 存在，检测不到就不生效。
  // 不手动清会导致上一条测试渲染出的树留在 document.body 里，后面的 `screen.getByTestId` 撞见重复
  // 元素而报错——这不是"多此一举"，是实测踩出来的（第一版没加这行，四态测试互相打架）。
  afterEach(cleanup);

  it("renders manual-entry when there is no fragment and no stored credentials", async () => {
    render(<PairingScreen deps={testDeps()} />);
    await waitFor(() => expect(screen.getByTestId("pairing-state-manual-entry")).toBeTruthy());
  });

  it("renders the paired placeholder on cold load when keyStore already has credentials", async () => {
    const keyStore = new InMemoryKeyStore();
    await keyStore.saveKeys(await makeStoredCredentials());
    render(<PairingScreen deps={testDeps({ keyStore })} />);

    await waitFor(() => expect(screen.getByTestId("pairing-state-paired")).toBeTruthy());
    expect(screen.getByTestId("pairing-paired-device-id").textContent).toContain(
      "11111111-1111-4111-8111-111111111111",
    );
  });

  it("renders the pairing progress state for a valid #p= fragment, then reaches awaiting_accept", async () => {
    const href = fragmentUrl(`https://${RELAY_HOST}`, JSON.stringify(makeQr()));
    render(<PairingScreen deps={testDeps({ getLocationHref: () => href })} />);

    await waitFor(() => {
      const el = screen.getByTestId("pairing-state-progress");
      expect(el.getAttribute("data-phase")).toBe("awaiting_accept");
    });
  });

  it("renders qr-error/format for a fragment that isn't valid JSON", async () => {
    const href = fragmentUrl(`https://${RELAY_HOST}`, "not json at all");
    render(<PairingScreen deps={testDeps({ getLocationHref: () => href })} />);

    await waitFor(() => {
      const el = screen.getByTestId("pairing-state-error");
      expect(el.getAttribute("data-error-kind")).toBe("qr-error");
      expect(el.getAttribute("data-category")).toBe("format");
    });
  });

  it("renders qr-error/origin_mismatch when the outer https origin doesn't match payload.relay_url", async () => {
    const href = fragmentUrl("https://evil.example", JSON.stringify(makeQr()));
    render(<PairingScreen deps={testDeps({ getLocationHref: () => href })} />);

    await waitFor(() => {
      const el = screen.getByTestId("pairing-state-error");
      expect(el.getAttribute("data-error-kind")).toBe("qr-error");
      expect(el.getAttribute("data-category")).toBe("origin_mismatch");
    });
  });

  it("clears the fragment exactly once whether the payload parses or not", async () => {
    const clearFragment = vi.fn();
    const goodHref = fragmentUrl(`https://${RELAY_HOST}`, JSON.stringify(makeQr()));
    render(<PairingScreen deps={testDeps({ getLocationHref: () => goodHref, clearFragment })} />);
    await waitFor(() => expect(screen.getByTestId("pairing-state-progress")).toBeTruthy());
    expect(clearFragment).toHaveBeenCalledTimes(1);
    cleanup();

    const clearFragmentOnBad = vi.fn();
    const badHref = fragmentUrl(`https://${RELAY_HOST}`, "not json at all");
    render(<PairingScreen deps={testDeps({ getLocationHref: () => badHref, clearFragment: clearFragmentOnBad })} />);
    await waitFor(() => expect(screen.getByTestId("pairing-state-error")).toBeTruthy());
    expect(clearFragmentOnBad).toHaveBeenCalledTimes(1);
  });

  it("does not clear the fragment / touch keyStore.loadKeys twice when there is no #p= marker", async () => {
    const clearFragment = vi.fn();
    const keyStore = new InMemoryKeyStore();
    const loadKeysSpy = vi.spyOn(keyStore, "loadKeys");
    render(<PairingScreen deps={testDeps({ keyStore, getLocationHref: () => "https://web.example/", clearFragment })} />);

    await waitFor(() => expect(screen.getByTestId("pairing-state-manual-entry")).toBeTruthy());
    expect(clearFragment).not.toHaveBeenCalled();
    expect(loadKeysSpy).toHaveBeenCalledTimes(1);
  });

  it("manual entry: malformed paste shows an inline format error and stays on manual-entry", async () => {
    // userEvent.paste()（不是 .type()）——JSON payload 里全是 `{`/`}`，userEvent.type() 会把它们
    // 当成 `{selectall}` 这类特殊按键序列解析，不是字面文本。
    const user = userEvent.setup();
    render(<PairingScreen deps={testDeps()} />);
    await waitFor(() => expect(screen.getByTestId("pairing-state-manual-entry")).toBeTruthy());

    await user.click(screen.getByTestId("pairing-manual-entry-textarea"));
    await user.paste("not json at all");
    await user.click(screen.getByTestId("pairing-manual-entry-submit"));

    const error = await screen.findByTestId("pairing-manual-entry-error");
    expect(error.getAttribute("data-category")).toBe("format");
    expect(screen.getByTestId("pairing-state-manual-entry")).toBeTruthy();
  });

  it("manual entry: pasting a valid bare-JSON payload transitions to the pairing progress state", async () => {
    const user = userEvent.setup();
    render(<PairingScreen deps={testDeps()} />);
    await waitFor(() => expect(screen.getByTestId("pairing-state-manual-entry")).toBeTruthy());

    await user.click(screen.getByTestId("pairing-manual-entry-textarea"));
    await user.paste(JSON.stringify(makeQr()));
    await user.click(screen.getByTestId("pairing-manual-entry-submit"));

    await waitFor(() => expect(screen.getByTestId("pairing-state-progress")).toBeTruthy());
  });

  it("StrictMode: fragment-derived pairing state is not overwritten by the duplicated effect setup", async () => {
    const qr = makeQr();
    const hrefWithFragment = fragmentUrl(`https://${RELAY_HOST}`, JSON.stringify(qr));
    const hrefAfterClear = "https://web.example/"; // 模拟 history.replaceState 清掉 hash 后的真实 URL
    const { getLocationHref, clearFragment } = statefulLocationHref(hrefWithFragment, hrefAfterClear);
    // 没有既有凭据——如果 bug 复现（被第二次 effect 覆盖），会落到 manual-entry 而不是 paired，
    // 跟 progress 态清楚可辨。
    const keyStore = new InMemoryKeyStore();

    render(
      <StrictMode>
        <PairingScreen deps={testDeps({ keyStore, getLocationHref, clearFragment })} />
      </StrictMode>,
    );

    await waitFor(() => expect(screen.getByTestId("pairing-state-progress")).toBeTruthy());
    // 给"被第二次 setup 覆盖"的 bug 留出观察窗口——旧实现下这里 screen 会变回 manual-entry。
    await new Promise((resolve) => setTimeout(resolve, 30));
    expect(screen.getByTestId("pairing-state-progress")).toBeTruthy();
    expect(clearFragment).toHaveBeenCalledTimes(1);
  });
});
