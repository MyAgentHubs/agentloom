// RootRouter.test.tsx — INT1c 审查返工（P1）· needs_repair 清核：Root 自己再清一次 + 读回验证，
// 清除失败停在可重试的 repair_failed 态（不假装已清），重试成功后才真正切回 unpaired。
//
// 触发 `onNeedsRepair` 的手法：不 mock `ConnectionSession`——种一个 `accessIssuedAtMs` 落在
// `refreshUntilWindowMs`（默认 30 天）之外的凭据，upgrade 从未成功就失败（`simulateUpgradeFailure`）
// 时 `classifyUpgradeFailure()`（`connection/upgradeClassifier.ts`，只读引用，未改动）判定
// "needs_repair"——`ConnectionSession` 自己的既有状态机真的走一遍认证终态，不是绕过它伪造回调。

import "fake-indexeddb/auto";
import { hkdfSync } from "node:crypto";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { ReadyState, type WebSocketCloseInfo, type WebSocketFactory, type WebSocketLike } from "../connection/types.ts";
import { bytesToBase64, bytesToBase64Url, utf8Bytes } from "../crypto/bytes.ts";
import type { KeyStorePort, StoredPairingCredentials } from "../store/key-store.ts";
import { importNonExtractableAesGcmKey, InMemoryKeyStore } from "../store/key-store.ts";
import { deriveEventStoreDbName, IndexedDbEventStore } from "../store/indexeddbEventStore.ts";
import { deriveCommandLedgerDbName, IndexedDbCommandLedger } from "../store/commandLedger.indexeddb.ts";
import { deriveBodyCacheDbName, IndexedDbBodyCache } from "../store/bodyCache.indexeddb.ts";
import {
  PENDING_ROOM_PURGES_STORAGE_KEY,
  readPendingRoomPurges,
  RootRouter,
  purgeRoomDataWithTimeout,
  type RootRouterDeps,
} from "./RootRouter.tsx";

// msgfix2 U4 修单三 J3：`window.localStorage` escape hatch——本仓踩出的 Node 26 + 这版 vitest 环境
// 坑（同 `ui/settings/verbosePreference.test.tsx` 头注全文，这里不重复）：Node 26 自带一个原生
// `globalThis.localStorage` getter（未传 `--localstorage-file` 时恒解出 `undefined`），vitest 的
// jsdom 环境搭建发现这个 key 已经在 Node 全局上存在，就不会用 jsdom 真实实现覆盖它——`window`
// 在 jsdom 环境下就是 `globalThis`，于是 `window.localStorage` 实际读到的也是那个恒 `undefined`
// 的原生 getter。测试需要一个真的能读写的 `localStorage` 才能验证"标记真的落盘/真的被摘除"，每个
// 测试前把 jsdom 内部真正持有的那份实例显式覆盖回 `window.localStorage`。
function installRealLocalStorage(): void {
  const dom = (window as unknown as { jsdom?: { window: Window } }).jsdom;
  if (!dom) throw new Error("window.jsdom (vitest jsdom environment handle) not found — has the environment changed?");
  Object.defineProperty(window, "localStorage", { value: dom.window.localStorage, configurable: true, writable: true });
}

beforeEach(() => {
  installRealLocalStorage();
  window.localStorage.clear();
});

afterEach(() => {
  cleanup();
  window.localStorage.clear();
});

const ROOM = "0123456789abcdef0123456789abcdef";
const THIRTY_ONE_DAYS_MS = 31 * 24 * 60 * 60 * 1000;

function makeValidPairingHref(): string {
  const payload = {
    v: 1,
    relay_url: "wss://relay.example",
    room: ROOM,
    pairing_token: "c".repeat(64),
    desktop_pub: bytesToBase64(new Uint8Array(32).fill(7)),
  };
  return `https://relay.example/#p=${bytesToBase64Url(utf8Bytes(JSON.stringify(payload)))}`;
}

// ============================================================================
// 假 WebSocket（同 pairingTransport.test.ts 的姊妹实现）
// ============================================================================

class FakeSocket implements WebSocketLike {
  readyState: number = ReadyState.CONNECTING;
  onopen: (() => void) | null = null;
  onclose: ((event: WebSocketCloseInfo) => void) | null = null;
  onerror: (() => void) | null = null;
  onmessage: ((event: { data: string }) => void) | null = null;

  constructor(
    public readonly url: string,
    public readonly protocols: string[],
  ) {}

  send(): void {}

  close(): void {
    if (this.readyState === ReadyState.CLOSED) return;
    this.readyState = ReadyState.CLOSING;
    queueMicrotask(() => {
      this.readyState = ReadyState.CLOSED;
      this.onclose?.({ code: 1000, reason: "", wasClean: true });
    });
  }

  /** 从未 open 过就失败——真实浏览器行为：没有状态码，close 事件 code=1006/reason=""。 */
  simulateUpgradeFailure(): void {
    this.readyState = ReadyState.CLOSED;
    this.onerror?.();
    this.onclose?.({ code: 1006, reason: "", wasClean: false });
  }
}

class FakeWebSocketFactory {
  sockets: FakeSocket[] = [];
  factory: WebSocketFactory = (url, protocols) => {
    const socket = new FakeSocket(url, protocols);
    this.sockets.push(socket);
    return socket;
  };
  get last(): FakeSocket {
    const socket = this.sockets.at(-1);
    if (!socket) throw new Error("no socket created yet");
    return socket;
  }
}

/** `clear()` 可控成功/失败的 KeyStore 测试替身——`saveKeys`/`loadKeys` 是真实的内存实现。 */
class FlakyClearKeyStore implements KeyStorePort {
  private record: StoredPairingCredentials | null;
  shouldFailClear: boolean;
  clearCalls = 0;

  constructor(initial: StoredPairingCredentials | null, shouldFailClear: boolean) {
    this.record = initial;
    this.shouldFailClear = shouldFailClear;
  }

  async saveKeys(creds: StoredPairingCredentials): Promise<void> {
    this.record = creds;
  }

  async loadKeys(): Promise<StoredPairingCredentials | null> {
    return this.record;
  }

  async clear(): Promise<void> {
    this.clearCalls += 1;
    if (this.shouldFailClear) {
      throw new Error("clear failed (test)");
    }
    this.record = null;
  }

  // `ConnectionSession` 的 fail-closed 构造期检查要求这两个方法存在。
  async savePendingRefresh(): Promise<void> {}
  async loadPendingRefresh(): Promise<null> {
    return null;
  }
}

async function makeExpiredStoredCredentials(): Promise<StoredPairingCredentials> {
  const kRoomRaw = new Uint8Array(32);
  crypto.getRandomValues(kRoomRaw);
  const kPair = new Uint8Array(32);
  crypto.getRandomValues(kPair);
  return {
    deviceId: "device-1",
    room: ROOM,
    relayUrl: "wss://relay.example",
    access: "a".repeat(64),
    refresh: "b".repeat(64),
    kRoomKey: await importNonExtractableAesGcmKey(kRoomRaw),
    kPair,
    // 落在 refreshUntilWindowMs（默认 30 天）之外——upgrade 从未成功就失败时
    // `classifyUpgradeFailure()` 判定 "needs_repair"（`connection/upgradeClassifier.ts`，只读）。
    accessIssuedAtMs: Date.now() - THIRTY_ONE_DAYS_MS,
  };
}

function makeDeps(overrides: Partial<RootRouterDeps> & { keyStore: KeyStorePort }): RootRouterDeps {
  return {
    webSocketFactory: () => {
      throw new Error("webSocketFactory not provided in this test");
    },
    createEventStore: (room) => new IndexedDbEventStore(`root-router-test-${room}-${crypto.randomUUID()}`),
    // msgfix2 U4：故意**不**加随机后缀——同生产命名（`deriveCommandLedgerDbName`/
    // `deriveBodyCacheDbName`），下方的原始探针（`seedRawProbeRecord`/`rawProbeDbHasRecord`）按
    // 生产库名直接读写，必须跟 RootRouter 真正装配出来的实例落进同一个库，才能验证"三库清除齐套"
    // 真的删掉了 RootRouter 自己在用的那个库，不是探针自说自话另开了一个不相干的库。
    createCommandLedger: (room) => new IndexedDbCommandLedger(deriveCommandLedgerDbName(room)),
    createBodyCache: (room) => new IndexedDbBodyCache(deriveBodyCacheDbName(room)),
    getLocationHref: () => "http://localhost/",
    clearFragment: () => {},
    ...overrides,
  };
}

// ============================================================================
// FIX2 P2-7：真实 IndexedDB 探针——独立于 `store/commandLedger.indexeddb.ts` 的 schema，只用来
// 证明"这个库名底下确实有数据 / 删库之后确实没了"，不依赖那个模块导出任何内部细节。用完即
// `db.close()`（这几个探针函数本身不留任何悬挂连接——不同于 `IndexedDbEventStore`/
// `IndexedDbCommandLedger` 那两个生产实现，它们的连接会一直留着，本文件不碰、不模拟那层）。
// ============================================================================

function openRawProbeDb(name: string): Promise<IDBDatabase> {
  return new Promise((resolve, reject) => {
    const request = indexedDB.open(name, 1);
    request.onupgradeneeded = () => {
      if (!request.result.objectStoreNames.contains("probe")) {
        request.result.createObjectStore("probe");
      }
    };
    request.onsuccess = () => resolve(request.result);
    request.onerror = () => reject(request.error);
  });
}

async function seedRawProbeRecord(name: string): Promise<void> {
  const db = await openRawProbeDb(name);
  await new Promise<void>((resolve, reject) => {
    const tx = db.transaction("probe", "readwrite");
    tx.objectStore("probe").put("marker", "k");
    tx.oncomplete = () => resolve();
    tx.onerror = () => reject(tx.error);
  });
  db.close();
}

async function rawProbeDbHasRecord(name: string): Promise<boolean> {
  const db = await openRawProbeDb(name);
  const has = await new Promise<boolean>((resolve, reject) => {
    const tx = db.transaction("probe", "readonly");
    const req = tx.objectStore("probe").get("k");
    req.onsuccess = () => resolve(req.result !== undefined);
    req.onerror = () => reject(req.error);
  });
  db.close();
  return has;
}

describe("RootRouter · needs_repair 清核（INT1c 审查返工·P1）", () => {
  it("clear() 成功（真实 ConnectionSession needs_repair 触发）→ Root 自己再清一次并读回验证 → 切回 unpaired（配对屏）；FIX2 P2-7 + msgfix2 U4：按房间派生的 commands 库与 body-cache 库也被真的删掉", async () => {
    const stored = await makeExpiredStoredCredentials();
    const keyStore = new FlakyClearKeyStore(stored, false);
    const factory = new FakeWebSocketFactory();

    // FIX2 P2-7 + msgfix2 U4：真实 IndexedDB 里预先塞一条数据到"这个房间的 commands 库"/"body-cache
    // 库"该有的库名下（`makeDeps()` 的 `createCommandLedger`/`createBodyCache` 用的正是同一组
    // `deriveCommandLedgerDbName(room)`/`deriveBodyCacheDbName(room)`）——探针用完即关连接（见文件头
    // `openRawProbeDb`/`seedRawProbeRecord` 注释），不会挡住随后 repair 真正删库时的
    // `indexedDB.deleteDatabase()`。
    const commandsDbName = deriveCommandLedgerDbName(ROOM);
    const bodyCacheDbName = deriveBodyCacheDbName(ROOM);
    await seedRawProbeRecord(commandsDbName);
    await seedRawProbeRecord(bodyCacheDbName);
    expect(await rawProbeDbHasRecord(commandsDbName)).toBe(true);
    expect(await rawProbeDbHasRecord(bodyCacheDbName)).toBe(true);

    render(<RootRouter deps={makeDeps({ keyStore, webSocketFactory: factory.factory })} />);

    // 先经过 "paired" 态（AppRuntime 已经在尝试连接）。
    await waitFor(() => expect(factory.sockets).toHaveLength(1));

    // upgrade 从未成功就失败——ConnectionSession 自己的状态机判定 needs_repair，自己先清一次
    // key-store，再调 onNeedsRepair 回调。
    await act(async () => {
      factory.last.simulateUpgradeFailure();
    });

    // Root 收到回调后独立再清一次 + 读回验证——确认真的清空了才切 unpaired，落到配对屏的手输态
    // （测试环境的 getLocationHref 没有 `#p=`，天然落 manual-entry）。
    await screen.findByTestId("pairing-manual-entry-textarea");
    expect(await keyStore.loadKeys()).toBeNull();
    // Root 自己确实调用过 clear()（不止信任 ConnectionSession 内部那一次）——
    // ConnectionSession 内部一次 + Root 侧再一次，至少 2 次。
    expect(keyStore.clearCalls).toBeGreaterThanOrEqual(2);

    // FIX2 P2-7 + msgfix2 U4 核心断言：真的删库了，不是只清了 key-store——重新用同一个派生库名打开，
    // 刚才种下的那条记录已经不在了（`indexedDB.deleteDatabase()` 把整个库连同它的 object store 一起
    // 清空，重开是全新空库）。三库（events 隐含在内，未单独种子——events 库本单不新增断言，events
    // 库删除已由既有断言序列覆盖）都删了。
    expect(await rawProbeDbHasRecord(commandsDbName)).toBe(false);
    expect(await rawProbeDbHasRecord(bodyCacheDbName)).toBe(false);
  });

  it("清除失败路径：clear() 抛错 → 停在可重试的 repair_failed 态，不假装已清；重试成功后才真正切回 unpaired；FIX2 P2-7：重试仍然用『那次进入 repair_failed 时』的房间删库，不会因为状态已经切离 paired 就丢了房间信息(退化成删错库名)", async () => {
    const stored = await makeExpiredStoredCredentials();
    const keyStore = new FlakyClearKeyStore(stored, true); // 一直失败，直到测试手动翻开关。
    const factory = new FakeWebSocketFactory();

    // 同上：预先塞一条真实数据到这个房间的 commands 库——用来验证"重试"这条路径最终真的删对了
    // 库（不是退化成用空字符串房间名删了个不存在的库、假装成功）。
    const commandsDbName = deriveCommandLedgerDbName(ROOM);
    await seedRawProbeRecord(commandsDbName);

    render(<RootRouter deps={makeDeps({ keyStore, webSocketFactory: factory.factory })} />);

    await waitFor(() => expect(factory.sockets).toHaveLength(1));
    await act(async () => {
      factory.last.simulateUpgradeFailure();
    });

    // clear() 失败——必须停在 repair_failed，不能假装已经清干净了直接切 unpaired。
    await screen.findByTestId("repair-failed");
    expect(screen.getByTestId("repair-failed-message").textContent).toContain("clear failed (test)");
    // 没有静默吞掉：凭据其实还在（Root 侧没有伪造"已清"状态）。
    expect(await keyStore.loadKeys()).not.toBeNull();
    // 这一步之所以失败是 key-store 那关没过——三库清除按顺序执行、key-store 先行，还没轮到删库。
    expect(await rawProbeDbHasRecord(commandsDbName)).toBe(true);

    // 现在放行 clear()，点重试——此刻 `state.kind` 已经是 "repair_failed"，不再是 "paired"。
    keyStore.shouldFailClear = false;
    fireEvent.click(screen.getByTestId("repair-retry"));

    await screen.findByTestId("pairing-manual-entry-textarea");
    expect(await keyStore.loadKeys()).toBeNull();
    // 重试这一轮真的把（正确房间的）commands 库删掉了——证明房间信息在 repair_failed 状态下没有
    // 丢失（若退化用了空字符串房间名，这条数据会原封不动地还在）。
    expect(await rawProbeDbHasRecord(commandsDbName)).toBe(false);
  });
});

describe("RootRouter · msgfix2 U4 修单三 J2（IDB 永久坏不得困死用户）", () => {
  it("启动读取凭据失败（keyStore.loadKeys() 抛错）——不停留在『checking』（永远渲染 null/白屏），走既有 repair/re-pair UI 路径", async () => {
    const keyStore: KeyStorePort = {
      saveKeys: async () => {},
      loadKeys: async () => {
        throw new Error("boot loadKeys failed (test)");
      },
      clear: async () => {},
    };
    const consoleErrorSpy = vi.spyOn(console, "error").mockImplementation(() => {});

    render(<RootRouter deps={makeDeps({ keyStore })} />);

    // 核心断言：没有卡在 checking（无限期渲染 null）——承接住了 rejection，走进既有的可展示、
    // 可重试的 repair_failed 页面。
    await screen.findByTestId("repair-failed");
    expect(screen.getByTestId("repair-failed-message").textContent).toContain("boot loadKeys failed (test)");
    expect(consoleErrorSpy).toHaveBeenCalled();

    consoleErrorSpy.mockRestore();
  });

  it("repair 路径 clear() 持续抛错（IDB 永久坏）——第一次仍停在可重试的 repair_failed（既有行为不变），重试一次仍失败就不再死循环展示同一个页面，降级切回配对屏（内存模式，引导用户重新配对）；console.error 保留可见性", async () => {
    const stored = await makeExpiredStoredCredentials();
    const keyStore = new FlakyClearKeyStore(stored, true); // 一直失败——测试全程不翻开关，模拟 IDB 永久坏、不会自愈。
    const factory = new FakeWebSocketFactory();
    const consoleErrorSpy = vi.spyOn(console, "error").mockImplementation(() => {});

    render(<RootRouter deps={makeDeps({ keyStore, webSocketFactory: factory.factory })} />);

    await waitFor(() => expect(factory.sockets).toHaveLength(1));
    await act(async () => {
      factory.last.simulateUpgradeFailure();
    });

    // 第一次失败——同既有行为，停在可重试的 repair_failed，不是本单要改的部分。
    await screen.findByTestId("repair-failed");

    // 用户点重试——clear() 仍然失败（IDB 永久坏，不会因为重试就自愈）。
    fireEvent.click(screen.getByTestId("repair-retry"));

    // 核心断言：不再第二次展示 repair_failed 陷入死循环——降级切回配对屏，用户能走通重新配对，
    // 而不是永远卡在一个不断失败的重试页面。
    await screen.findByTestId("pairing-manual-entry-textarea");
    expect(keyStore.clearCalls).toBeGreaterThanOrEqual(2);
    expect(consoleErrorSpy).toHaveBeenCalled();

    consoleErrorSpy.mockRestore();
  });
});

describe("RootRouter · 基本引导（checking → unpaired / paired）", () => {
  it("keyStore 为空 + URL 无 #p= → 落配对屏（manual-entry）", async () => {
    const keyStore = new FlakyClearKeyStore(null, false);
    render(<RootRouter deps={makeDeps({ keyStore })} />);
    await screen.findByTestId("pairing-manual-entry-textarea");
    expect(screen.queryByTestId("session-list-screen")).toBeNull();
  });

  it("keyStore 已有凭据 + URL 无 #p= → 落已配对运行时（渲染 SessionListScreen，不是配对屏）", async () => {
    const stored = await makeExpiredStoredCredentials();
    const keyStore = new FlakyClearKeyStore(stored, false);
    const factory = new FakeWebSocketFactory();
    render(<RootRouter deps={makeDeps({ keyStore, webSocketFactory: factory.factory })} />);
    await screen.findByTestId("session-list-screen");
    expect(screen.queryByTestId("pairing-state-progress")).toBeNull();
  });

  it("keyStore 已有凭据 + URL 含有效 #p= → 扫码意图优先，落配对流程而不渲染已配对运行时", async () => {
    const stored = await makeExpiredStoredCredentials();
    const keyStore = new FlakyClearKeyStore(stored, false);
    const factory = new FakeWebSocketFactory();

    render(
      <RootRouter
        deps={makeDeps({
          keyStore,
          webSocketFactory: factory.factory,
          getLocationHref: makeValidPairingHref,
        })}
      />,
    );

    await waitFor(() =>
      expect(screen.getByTestId("pairing-state-progress").getAttribute("data-phase")).toBe("awaiting_accept"),
    );
    expect(screen.queryByTestId("session-list-screen")).toBeNull();
  });
});

// ============================================================================
// msgfix2 U4（四触发点③"re-pair/room 切换"）：`RootRouter.tsx::handleActivated` 检测到新激活的
// 凭据换了房间时，主动清掉**旧**房间的三库——本节此前完全没有测试覆盖（`git status` 核对过：这个
// describe 块是本 task 新增的，不是继承 WIP 的一部分）。走真实完整的配对协议帧序（同
// `pairingTransport.e2e.test.ts` 的独立假桌面手法：不 import `pairing/key-wrap.ts`/
// `pairing/meta.ts`/`crypto/kdf.ts`，避免假桌面与生产代码共享 bug），驱动 `RootRouter` 从"已配对
// 房间 A，URL 带 #p= 指向房间 B"一路 `activated`，断言房间 A 的 events/commands/body-cache 三库
// 真的被删了、房间 B 的新凭据完好保留。
// ============================================================================

const ROOM_B = "fedcba9876543210fedcba9876543210";
const K_PAIR_INFO = utf8Bytes("agentloom-rc-v1");

interface HandshakeMeta {
  v: number;
  room: string;
  epoch: number;
  kind: string;
  session: string | null;
  command_id: string | null;
}

function toBufferSource(bytes: Uint8Array): Uint8Array<ArrayBuffer> {
  return Uint8Array.from(bytes);
}

function randomHex64(): string {
  const bytes = new Uint8Array(32);
  crypto.getRandomValues(bytes);
  return Array.from(bytes)
    .map((b) => b.toString(16).padStart(2, "0"))
    .join("");
}

function buildAadIndependent(meta: HandshakeMeta): string {
  const part = (v: unknown) => (v === null || v === undefined ? "" : String(v));
  return [part(meta.v), part(meta.room), part(meta.epoch), part(meta.kind), part(meta.session), part(meta.command_id)].join("|");
}

async function importAesKey(raw: Uint8Array, usages: KeyUsage[]): Promise<CryptoKey> {
  return crypto.subtle.importKey("raw", toBufferSource(raw), "AES-GCM", false, usages);
}

async function sealAadIndependent(rawKey: Uint8Array, meta: HandshakeMeta, plaintext: Uint8Array): Promise<{ ct: string; n: string }> {
  const key = await importAesKey(rawKey, ["encrypt"]);
  const nonce = new Uint8Array(12);
  crypto.getRandomValues(nonce);
  const ciphertext = await crypto.subtle.encrypt(
    { name: "AES-GCM", iv: toBufferSource(nonce), additionalData: toBufferSource(utf8Bytes(buildAadIndependent(meta))) },
    key,
    toBufferSource(plaintext),
  );
  return { ct: bytesToBase64(new Uint8Array(ciphertext)), n: bytesToBase64(nonce) };
}

async function openAadIndependent(rawKey: Uint8Array, meta: HandshakeMeta, ctB64: string, nB64: string): Promise<Uint8Array> {
  const key = await importAesKey(rawKey, ["decrypt"]);
  const plaintext = await crypto.subtle.decrypt(
    { name: "AES-GCM", iv: toBufferSource(base64Decode(nB64)), additionalData: toBufferSource(utf8Bytes(buildAadIndependent(meta))) },
    key,
    toBufferSource(base64Decode(ctB64)),
  );
  return new Uint8Array(plaintext);
}

async function wrapKeyNoAadIndependent(kek: Uint8Array, keyBytes: Uint8Array): Promise<{ ct: string; n: string }> {
  const key = await importAesKey(kek, ["encrypt"]);
  const nonce = new Uint8Array(12);
  crypto.getRandomValues(nonce);
  const ciphertext = await crypto.subtle.encrypt({ name: "AES-GCM", iv: toBufferSource(nonce) }, key, toBufferSource(keyBytes));
  return { ct: bytesToBase64(new Uint8Array(ciphertext)), n: bytesToBase64(nonce) };
}

function base64Decode(value: string): Uint8Array {
  const binary = atob(value);
  const out = new Uint8Array(binary.length);
  for (let i = 0; i < binary.length; i += 1) out[i] = binary.charCodeAt(i);
  return out;
}

function deriveKPairIndependent(sharedSecret: Uint8Array, pairingTokenHex: string): Uint8Array {
  return new Uint8Array(hkdfSync("sha256", sharedSecret, utf8Bytes(pairingTokenHex), K_PAIR_INFO, 32));
}

async function generateDesktopX25519(): Promise<{ privateKey: CryptoKey; publicKeyBase64: string }> {
  const keyPair = await crypto.subtle.generateKey({ name: "X25519" }, true, ["deriveBits"]);
  const publicKeyBytes = new Uint8Array(await crypto.subtle.exportKey("raw", keyPair.publicKey));
  return { privateKey: keyPair.privateKey, publicKeyBase64: bytesToBase64(publicKeyBytes) };
}

async function importRemotePublicKey(base64: string): Promise<CryptoKey> {
  return crypto.subtle.importKey("raw", toBufferSource(base64Decode(base64)), { name: "X25519" }, true, []);
}

interface FakeDesktop {
  href: string;
  verifyHello(helloFrame: Record<string, unknown>): Promise<Uint8Array>;
  buildAccept(kPair: Uint8Array, deviceId: string, kRoom: Uint8Array): Promise<Record<string, unknown>>;
  verifyDone(kRoom: Uint8Array, deviceId: string, doneFrame: Record<string, unknown>): Promise<void>;
  buildReady(kPair: Uint8Array, deviceId: string): Promise<Record<string, unknown>>;
}

/** 同 `pairingTransport.e2e.test.ts::makeFakeDesktop` 的独立假桌面手法，参数化 room（本单需要
 *  "换到一个跟当前已配对不同的房间"这个场景，不能复用文件顶部固定 `ROOM` 的那个）。 */
async function makeFakeDesktopForRoom(room: string, relayUrl = "wss://relay.example"): Promise<FakeDesktop> {
  const { privateKey, publicKeyBase64 } = await generateDesktopX25519();
  const pairingTokenHex = randomHex64();
  const payload = { v: 1, relay_url: relayUrl, room, pairing_token: pairingTokenHex, desktop_pub: publicKeyBase64 };
  const href = `https://relay.example/#p=${bytesToBase64Url(utf8Bytes(JSON.stringify(payload)))}`;

  return {
    href,
    async verifyHello(helloFrame) {
      const remotePub = String(helloFrame.remote_pub);
      const remotePublicKey = await importRemotePublicKey(remotePub);
      const shared = new Uint8Array(await crypto.subtle.deriveBits({ name: "X25519", public: remotePublicKey }, privateKey, 256));
      const kPair = deriveKPairIndependent(shared, pairingTokenHex);
      const helloMeta: HandshakeMeta = { v: 1, room, epoch: 0, kind: "control", session: null, command_id: null };
      const plaintext = await openAadIndependent(kPair, helloMeta, String(helloFrame.token_ct), String(helloFrame.token_n));
      if (new TextDecoder().decode(plaintext) !== pairingTokenHex) {
        throw new Error("hello token proof mismatch");
      }
      return kPair;
    },
    async buildAccept(kPair, deviceId, kRoom) {
      const { ct: kRoomCt, n: kRoomN } = await wrapKeyNoAadIndependent(kPair, kRoom);
      const tokensMeta: HandshakeMeta = { v: 1, room, epoch: 0, kind: "pair-accept-tokens", session: deviceId, command_id: null };
      const tokensPlain = utf8Bytes(JSON.stringify({ capability_token: randomHex64(), refresh_token: randomHex64() }));
      const { ct: tokensCt, n: tokensN } = await sealAadIndependent(kPair, tokensMeta, tokensPlain);
      return { t: "pair.accept", room, device_id: deviceId, k_room_ct: kRoomCt, k_room_n: kRoomN, tokens_ct: tokensCt, tokens_n: tokensN };
    },
    async verifyDone(kRoom, deviceId, doneFrame) {
      const confirmMeta: HandshakeMeta = { v: 1, room, epoch: 0, kind: "pair-confirm", session: deviceId, command_id: null };
      const plaintext = await openAadIndependent(kRoom, confirmMeta, String(doneFrame.confirm_ct), String(doneFrame.confirm_n));
      if (new TextDecoder().decode(plaintext) !== deviceId) {
        throw new Error("done confirm mismatch");
      }
    },
    async buildReady(kPair, deviceId) {
      const readyMeta: HandshakeMeta = { v: 1, room, epoch: 0, kind: "pair-ready", session: deviceId, command_id: null };
      const { ct, n } = await sealAadIndependent(kPair, readyMeta, utf8Bytes(deviceId));
      return { t: "pair.ready", room, device_id: deviceId, ct, n };
    },
  };
}

/** 独立实现（不 `extends FakeSocket`）——文件顶部共用的 `FakeSocket.send()` 签名是 `(): void`
 *  （既有测试从不检查 `sent` 内容，只用 `simulateOpen`/`simulateMessage`/`simulateUpgradeFailure`），
 *  子类化并把 `send` 收紧成 `(data: string) => void` 会撞 TS 方法重写的逆变检查（"Target signature
 *  provides too few arguments"）——本节需要真的读到 `pair.hello`/`pair.done` 的内容才能驱动假桌面
 *  一侧的响应,独立写一份比强改共用基类签名（可能影响其它既有测试的类型推断）更安全。 */
class RecordingFakeSocket implements WebSocketLike {
  readyState: number = ReadyState.CONNECTING;
  onopen: (() => void) | null = null;
  onclose: ((event: WebSocketCloseInfo) => void) | null = null;
  onerror: (() => void) | null = null;
  onmessage: ((event: { data: string }) => void) | null = null;
  sent: string[] = [];

  constructor(
    public readonly url: string,
    public readonly protocols: string[],
  ) {}

  send(data: string): void {
    this.sent.push(data);
  }
  close(): void {
    if (this.readyState === ReadyState.CLOSED) return;
    this.readyState = ReadyState.CLOSING;
    queueMicrotask(() => {
      this.readyState = ReadyState.CLOSED;
      this.onclose?.({ code: 1000, reason: "", wasClean: true });
    });
  }
  simulateOpen(): void {
    this.readyState = ReadyState.OPEN;
    this.onopen?.();
  }
  simulateMessage(frame: unknown): void {
    this.onmessage?.({ data: JSON.stringify(frame) });
  }
}

class RecordingFakeWebSocketFactory {
  sockets: RecordingFakeSocket[] = [];
  factory: WebSocketFactory = (url, protocols) => {
    const socket = new RecordingFakeSocket(url, protocols);
    this.sockets.push(socket);
    return socket;
  };
  get last(): RecordingFakeSocket {
    const socket = this.sockets.at(-1);
    if (!socket) throw new Error("no socket created yet");
    return socket;
  }
}

describe("RootRouter · msgfix2 U4 四触发点③（re-pair/room 切换）", () => {
  it("已配对房间 A，URL 带 #p= 扫码指向房间 B → activated 后旧房间 A 的 events/commands/body-cache 三库被清除，B 的新凭据完好保留", async () => {
    const roomAStored = await makeExpiredStoredCredentials(); // room = 顶部固定 `ROOM`（room A）。
    const keyStore = new InMemoryKeyStore();
    await keyStore.saveKeys(roomAStored);

    // 房间 A 的三库真的种一条数据——验证"真的删了"，不是只信任 handleActivated 调用过
    // purgeRoomData()（同既有 repair 测试的取向）。
    const eventsDbNameA = deriveEventStoreDbName(ROOM);
    const commandsDbNameA = deriveCommandLedgerDbName(ROOM);
    const bodyCacheDbNameA = deriveBodyCacheDbName(ROOM);
    await seedRawProbeRecord(eventsDbNameA);
    await seedRawProbeRecord(commandsDbNameA);
    await seedRawProbeRecord(bodyCacheDbNameA);
    expect(await rawProbeDbHasRecord(eventsDbNameA)).toBe(true);
    expect(await rawProbeDbHasRecord(commandsDbNameA)).toBe(true);
    expect(await rawProbeDbHasRecord(bodyCacheDbNameA)).toBe(true);

    const desktopB = await makeFakeDesktopForRoom(ROOM_B);
    const factory = new RecordingFakeWebSocketFactory();

    render(
      <RootRouter
        deps={makeDeps({
          keyStore,
          webSocketFactory: factory.factory,
          getLocationHref: () => desktopB.href,
        })}
      />,
    );

    // URL 带 #p= → RootRouter 直接落 "unpaired"（记住 previousRoomRef=room A）→ RealPairingFlow
    // 解析出房间 B 的 QR → RealPairingSessionHost 建立配对连接（第一个 socket）。
    await waitFor(() => expect(factory.sockets).toHaveLength(1));
    await act(async () => {
      factory.last.simulateOpen();
    });

    const helloFrame = await waitFor(() => {
      if (factory.last.sent.length === 0) throw new Error("pair.hello not sent yet");
      return JSON.parse(factory.last.sent[0]!) as Record<string, unknown>;
    });
    expect(helloFrame.t).toBe("pair.hello");
    const kPair = await desktopB.verifyHello(helloFrame);

    const deviceId = "device-room-switch-test";
    const kRoomB = new Uint8Array(32);
    crypto.getRandomValues(kRoomB);
    const acceptFrame = await desktopB.buildAccept(kPair, deviceId, kRoomB);
    await act(async () => {
      factory.last.simulateMessage(acceptFrame);
    });

    const doneFrame = await waitFor(() => {
      if (factory.last.sent.length < 2) throw new Error("pair.done not sent yet");
      return JSON.parse(factory.last.sent[1]!) as Record<string, unknown>;
    });
    expect(doneFrame.t).toBe("pair.done");
    await desktopB.verifyDone(kRoomB, deviceId, doneFrame); // 抛异常即测试失败——AEAD 认证必须真的过。

    const readyFrame = await desktopB.buildReady(kPair, deviceId);
    await act(async () => {
      factory.last.simulateMessage(readyFrame);
    });

    // activated → RealPairingSessionHost 关闭配对连接 + 调 onActivated → RootRouter.handleActivated
    // 重新 loadKeys()（拿到刚落盘的房间 B 完整凭据）→ 换房检测：previousRoom(A) !== stored.room(B)
    // → purgeRoomData({room: A, ...})（三库先 close 后 delete；本场景没有活跃的房间 A 实例可
    // close——`#p=` 分支从未构造过房间 A 的 eventStore/commandLedger/bodyCache，直接进删库阶段）。
    // 落定后不再展示任何配对屏 UI（AppRuntime 接手渲染房间 B 的运行时）。
    await waitFor(() => expect(screen.queryByTestId("pairing-state-progress")).toBeNull());
    await waitFor(() => expect(screen.queryByTestId("pairing-manual-entry-textarea")).toBeNull());

    // 核心断言①：房间 A 的三库真的被删了。
    await waitFor(async () => expect(await rawProbeDbHasRecord(eventsDbNameA)).toBe(false));
    expect(await rawProbeDbHasRecord(commandsDbNameA)).toBe(false);
    expect(await rawProbeDbHasRecord(bodyCacheDbNameA)).toBe(false);

    // 核心断言②：key-store 现在存的是房间 B 的新凭据，没有被上面那次"旧房间清理"误伤（清理只
    // 动三个 IndexedDB 库，不碰 key-store——新凭据已经落盘）。
    const finalStored = await keyStore.loadKeys();
    expect(finalStored?.room).toBe(ROOM_B);
    expect(finalStored?.deviceId).toBe(deviceId);
  });

  it("msgfix2 U4 修单 H2：旧房间清理真的失败（真实 onblocked，不是注入假失败）——不阻塞切到房间 B，但失败必须可见（console.error），不能像旧版那样静默吞掉", async () => {
    // 更长的超时——`blockingConn` 全程不关闭期间，fake-indexeddb 内部
    // `waitForOthersClosedDelete()` 会一直排队重试轮询任务（等它关闭），跟测试里其它异步工作
    // （WS 握手/加密）抢占事件循环调度,拖慢整体但不改变最终结果（`onblocked` 本身已经在早期就让
    // `purgeRoomData()` 的 promise 定型,这条轮询循环不影响正确性,只是让这条测试本身跑得慢)。
    const roomAStored = await makeExpiredStoredCredentials();
    const keyStore = new InMemoryKeyStore();
    await keyStore.saveKeys(roomAStored);

    const eventsDbNameA = deriveEventStoreDbName(ROOM);
    await seedRawProbeRecord(eventsDbNameA);
    expect(await rawProbeDbHasRecord(eventsDbNameA)).toBe(true);

    // 留一个不关闭的连接——真实的 IndexedDB `deleteDatabase()` 会卡在 `onblocked`，
    // `store/cacheManager.ts::defaultDeleteIndexedDb` 把 `onblocked` 当失败处理，制造一次
    // 真实的（不是注入假失败的）room-switch 清理失败。
    const blockingConn = await openRawProbeDb(eventsDbNameA);

    const consoleErrorSpy = vi.spyOn(console, "error").mockImplementation(() => {});

    const desktopB = await makeFakeDesktopForRoom(ROOM_B);
    const factory = new RecordingFakeWebSocketFactory();

    render(
      <RootRouter
        deps={makeDeps({
          keyStore,
          webSocketFactory: factory.factory,
          getLocationHref: () => desktopB.href,
        })}
      />,
    );

    await waitFor(() => expect(factory.sockets).toHaveLength(1));
    await act(async () => {
      factory.last.simulateOpen();
    });
    const helloFrame = await waitFor(() => {
      if (factory.last.sent.length === 0) throw new Error("pair.hello not sent yet");
      return JSON.parse(factory.last.sent[0]!) as Record<string, unknown>;
    });
    const kPair = await desktopB.verifyHello(helloFrame);
    const deviceId = "device-room-switch-blocked-test";
    const kRoomB = new Uint8Array(32);
    crypto.getRandomValues(kRoomB);
    const acceptFrame = await desktopB.buildAccept(kPair, deviceId, kRoomB);
    await act(async () => {
      factory.last.simulateMessage(acceptFrame);
    });
    const doneFrame = await waitFor(() => {
      if (factory.last.sent.length < 2) throw new Error("pair.done not sent yet");
      return JSON.parse(factory.last.sent[1]!) as Record<string, unknown>;
    });
    await desktopB.verifyDone(kRoomB, deviceId, doneFrame);
    const readyFrame = await desktopB.buildReady(kPair, deviceId);
    await act(async () => {
      factory.last.simulateMessage(readyFrame);
    });

    // 核心断言①：即使旧房间清理失败，仍然正常切到房间 B（不被这件后台清理小事拦在配对屏）。
    await waitFor(() => expect(screen.queryByTestId("pairing-state-progress")).toBeNull());
    await waitFor(() => expect(screen.queryByTestId("pairing-manual-entry-textarea")).toBeNull());
    const finalStored = await keyStore.loadKeys();
    expect(finalStored?.room).toBe(ROOM_B);

    // 核心断言②：失败真的发生了——旧房间的库没被删掉（onblocked 挡住了，不是删成功了）。直接用
    // `blockingConn` 这条已经开着的连接查（不能像 `rawProbeDbHasRecord()` 那样再对同一个库名
    // 开一个*新*连接——fake-indexeddb 把同一 db 名下的 open/delete 请求排进同一条连接队列,
    // `purgeRoomData()` 那次 `deleteDatabase()` 请求本身要等 `blockingConn` 关闭才能真正"完成"
    // 排队,此刻新开一个 open() 会排在它后面永远等不到,真的会把测试拖到超时——这正是本单在写这条
    // 测试时踩到的真实教训,不是猜测)。
    const stillHasRecord = await new Promise<boolean>((resolve, reject) => {
      const tx = blockingConn.transaction("probe", "readonly");
      const req = tx.objectStore("probe").get("k");
      req.onsuccess = () => resolve(req.result !== undefined);
      req.onerror = () => reject(req.error);
    });
    expect(stillHasRecord).toBe(true);

    // 核心断言③：失败可见——console.error 真的被调用过，不是像旧版 `void purgeRoomData(...)`
    // 那样连一行日志都不留、彻底静默。
    await waitFor(() => expect(consoleErrorSpy).toHaveBeenCalled());
    const loggedMessages = consoleErrorSpy.mock.calls.map((call) => String(call[0]));
    expect(loggedMessages.some((message) => message.includes(ROOM))).toBe(true);

    consoleErrorSpy.mockRestore();
    blockingConn.close();
  }, 15_000);

  describe("msgfix2 U4 修单二 I3：room-switch purge 不挂激活（先激活新房间，purge 异步跑 + 超时上限）", () => {
    it("先激活新房间，purge 异步跑，不卡激活——purge 迟迟不落地也不妨碍立刻切到房间 B（不再是旧版 `await purgeRoomData(...)` 挡在 `setState` 之前）", async () => {
      const roomAStored = await makeExpiredStoredCredentials();
      const keyStore = new InMemoryKeyStore();
      await keyStore.saveKeys(roomAStored);

      const desktopB = await makeFakeDesktopForRoom(ROOM_B);
      const factory = new RecordingFakeWebSocketFactory();

      render(
        <RootRouter
          deps={makeDeps({
            keyStore,
            webSocketFactory: factory.factory,
            getLocationHref: () => desktopB.href,
          })}
        />,
      );

      await waitFor(() => expect(factory.sockets).toHaveLength(1));
      await act(async () => {
        factory.last.simulateOpen();
      });
      const helloFrame = await waitFor(() => {
        if (factory.last.sent.length === 0) throw new Error("pair.hello not sent yet");
        return JSON.parse(factory.last.sent[0]!) as Record<string, unknown>;
      });
      const kPair = await desktopB.verifyHello(helloFrame);
      const deviceId = "device-room-switch-nonblocking-test";
      const kRoomB = new Uint8Array(32);
      crypto.getRandomValues(kRoomB);
      const acceptFrame = await desktopB.buildAccept(kPair, deviceId, kRoomB);
      await act(async () => {
        factory.last.simulateMessage(acceptFrame);
      });
      const doneFrame = await waitFor(() => {
        if (factory.last.sent.length < 2) throw new Error("pair.done not sent yet");
        return JSON.parse(factory.last.sent[1]!) as Record<string, unknown>;
      });
      await desktopB.verifyDone(kRoomB, deviceId, doneFrame);
      const readyFrame = await desktopB.buildReady(kPair, deviceId);
      await act(async () => {
        factory.last.simulateMessage(readyFrame);
      });

      // 核心断言：正常切到房间 B——`handleActivated` 里 `setState({kind:"paired", ...})` 现在排在
      // purge 发起之前（同步），不需要等 purge 落地（这里没有制造任何清理失败/阻塞，纯粹验证正常
      // 路径下"先激活"这条时序本身不会被 purge 卡住——I3 修单要解的正是"慢删除卡死激活"）。
      await waitFor(() => expect(screen.queryByTestId("pairing-state-progress")).toBeNull());
      await waitFor(() => expect(screen.queryByTestId("pairing-manual-entry-textarea")).toBeNull());
      const finalStored = await keyStore.loadKeys();
      expect(finalStored?.room).toBe(ROOM_B);
    });
  });
});

describe("RootRouter · msgfix2 U4 修单三 J3（pendingRoomPurges 启动补清）", () => {
  it("room-switch purge 真的失败（真实 onblocked，不是注入假失败）——标记仍然留在 localStorage，供下次启动补清，不因为这次失败/超时就被摘除", async () => {
    const roomAStored = await makeExpiredStoredCredentials();
    const keyStore = new InMemoryKeyStore();
    await keyStore.saveKeys(roomAStored);

    const eventsDbNameA = deriveEventStoreDbName(ROOM);
    await seedRawProbeRecord(eventsDbNameA);

    // 留一个不关闭的连接——同既有 H2 手法，制造一次真实的（不是注入假失败的）room-switch 清理
    // 失败（`onblocked`）。
    const blockingConn = await openRawProbeDb(eventsDbNameA);

    const consoleErrorSpy = vi.spyOn(console, "error").mockImplementation(() => {});

    const desktopB = await makeFakeDesktopForRoom(ROOM_B);
    const factory = new RecordingFakeWebSocketFactory();

    render(
      <RootRouter
        deps={makeDeps({
          keyStore,
          webSocketFactory: factory.factory,
          getLocationHref: () => desktopB.href,
        })}
      />,
    );

    await waitFor(() => expect(factory.sockets).toHaveLength(1));
    await act(async () => {
      factory.last.simulateOpen();
    });
    const helloFrame = await waitFor(() => {
      if (factory.last.sent.length === 0) throw new Error("pair.hello not sent yet");
      return JSON.parse(factory.last.sent[0]!) as Record<string, unknown>;
    });
    const kPair = await desktopB.verifyHello(helloFrame);
    const deviceId = "device-pending-purge-marker-test";
    const kRoomB = new Uint8Array(32);
    crypto.getRandomValues(kRoomB);
    const acceptFrame = await desktopB.buildAccept(kPair, deviceId, kRoomB);
    await act(async () => {
      factory.last.simulateMessage(acceptFrame);
    });
    const doneFrame = await waitFor(() => {
      if (factory.last.sent.length < 2) throw new Error("pair.done not sent yet");
      return JSON.parse(factory.last.sent[1]!) as Record<string, unknown>;
    });
    await desktopB.verifyDone(kRoomB, deviceId, doneFrame);
    const readyFrame = await desktopB.buildReady(kPair, deviceId);
    await act(async () => {
      factory.last.simulateMessage(readyFrame);
    });

    await waitFor(() => expect(screen.queryByTestId("pairing-manual-entry-textarea")).toBeNull());
    await waitFor(() => expect(consoleErrorSpy).toHaveBeenCalled());

    // 核心断言：这次 purge 真的失败了（onblocked），标记仍然留在 localStorage——不能因为这次
    // 失败/超时就悄悄摘掉，下次启动还要靠它补跑。
    expect(readPendingRoomPurges()).toContain(ROOM);

    consoleErrorSpy.mockRestore();
    blockingConn.close();
  }, 15_000);

  it("模拟重启——localStorage 里遗留着上次未清完的房间标记，新一次挂载读到它、补跑 purge，成功后房间库真的被删掉、标记也被摘除", async () => {
    const staleRoom = "aaaa1111aaaa1111aaaa1111aaaa1111";
    const eventsDbName = deriveEventStoreDbName(staleRoom);
    await seedRawProbeRecord(eventsDbName);
    expect(await rawProbeDbHasRecord(eventsDbName)).toBe(true);

    localStorage.setItem(PENDING_ROOM_PURGES_STORAGE_KEY, JSON.stringify([staleRoom]));

    // 未配对——落配对屏，纯粹验证启动补清跟当前是否已配对、走哪条路由完全无关（并行独立跑）。
    const keyStore = new InMemoryKeyStore();
    render(<RootRouter deps={makeDeps({ keyStore })} />);

    await screen.findByTestId("pairing-manual-entry-textarea");

    // 核心断言：补清真的把上次遗留的房间库删了，标记也从 localStorage 摘除。
    await waitFor(async () => expect(await rawProbeDbHasRecord(eventsDbName)).toBe(false));
    await waitFor(() => expect(readPendingRoomPurges()).not.toContain(staleRoom));
  });

  it("msgfix2 F2 S2：pending 标记指向的正是当前配对回的这个房间——不删（正在用的库不能删），只摘除过期标记", async () => {
    const stored = await makeExpiredStoredCredentials(); // room = ROOM
    const keyStore = new InMemoryKeyStore();
    await keyStore.saveKeys(stored);

    // 这个房间的 events 库里确实有数据（模拟"正在用"）——若 sweep 没有跳过当前房间，这条数据会被
    // 错误地删掉。
    const eventsDbName = deriveEventStoreDbName(ROOM);
    await seedRawProbeRecord(eventsDbName);
    expect(await rawProbeDbHasRecord(eventsDbName)).toBe(true);

    // 残留标记——模拟上一轮 room-switch purge 失败/超时后留下的、指向的房间恰好就是这次又配回来的
    // 那个（用户配对→切换→又配回同一个房间，或者一次失败的自我 purge 残留）。
    localStorage.setItem(PENDING_ROOM_PURGES_STORAGE_KEY, JSON.stringify([ROOM]));

    const factory = new FakeWebSocketFactory();
    render(<RootRouter deps={makeDeps({ keyStore, webSocketFactory: factory.factory })} />);

    // 路由到 paired（AppRuntime 已经在尝试连接）——启动补清 effect 与这条路由判定并行跑。
    await waitFor(() => expect(factory.sockets).toHaveLength(1));

    // 核心断言①：过期标记被摘除（不是永远留着——同 J3 既有幂等口径，这次"待办"已经不成立了）。
    await waitFor(() => expect(readPendingRoomPurges()).not.toContain(ROOM));

    // 给"万一真的删了"的异步 purge 一点时间落地，再确认真的没删——不是靠 waitFor 的第一次轮询就
    // 提前判定通过。
    await new Promise((resolve) => setTimeout(resolve, 100));
    // 核心断言②：正在用的库真的没被删——这是本用例要防的那个 bug。
    expect(await rawProbeDbHasRecord(eventsDbName)).toBe(true);
  });
});

/** 同 `makeExpiredStoredCredentials()`，但 `accessIssuedAtMs` 是"刚签发"——本组用例要的是"一条健康
 *  活跃的连接"这个前提本身（不是 needs_repair 分类的触发条件），用过期凭据会引入不相关的噪音
 *  （`ConnectionSession` 可能因为"名义寿命过去太久"主动发起 proactive refresh）。 */
async function makeFreshStoredCredentials(): Promise<StoredPairingCredentials> {
  const kRoomRaw = new Uint8Array(32);
  crypto.getRandomValues(kRoomRaw);
  const kPair = new Uint8Array(32);
  crypto.getRandomValues(kPair);
  return {
    deviceId: "device-unpair-test",
    room: ROOM,
    relayUrl: "wss://relay.example",
    access: "a".repeat(64),
    refresh: "b".repeat(64),
    kRoomKey: await importNonExtractableAesGcmKey(kRoomRaw),
    kPair,
    accessIssuedAtMs: Date.now(),
  };
}

describe("RootRouter · msgfix2 F2 S4（解除配对必须先停连接再 purge，不能让活着的连接跟 purge 撞车）", () => {
  it("点击「解除配对」时连接还活着——AppRuntime 立即卸载（连接同步 close，settings 屏立刻从 DOM 消失），purge 完成后才切回配对屏；真的删干净了房间三库", async () => {
    const stored = await makeFreshStoredCredentials();
    const keyStore = new InMemoryKeyStore();
    await keyStore.saveKeys(stored);

    // 房间的 events 库里确实有数据——验证 purge 最终真的把它删了（不是只验证 UI 转场）。
    const eventsDbName = deriveEventStoreDbName(ROOM);
    await seedRawProbeRecord(eventsDbName);
    expect(await rawProbeDbHasRecord(eventsDbName)).toBe(true);

    const factory = new RecordingFakeWebSocketFactory();
    render(<RootRouter deps={makeDeps({ keyStore, webSocketFactory: factory.factory })} />);

    await waitFor(() => expect(factory.sockets).toHaveLength(1));
    await act(async () => {
      factory.last.simulateOpen();
    });
    expect(factory.last.readyState).toBe(ReadyState.OPEN);

    // 设置入口只挂在会话列表屏（同 `AppRuntime.bodyCache.e2e.test.tsx` 既有导航路径）。
    fireEvent.click(await screen.findByTestId("session-list-settings-button"));
    const unpairButton = await screen.findByTestId("settings-unpair-button");

    fireEvent.click(unpairButton);

    // 核心断言①：点击解除配对之后，连接立即被关掉（不是等 purge 跑完才关）——`ConnectionSession.
    // stop()` 同步调用 `socket.close()`，`fireEvent.click()` 触发的 React 状态更新 + AppRuntime
    // 卸载在这一行之前已经同步 flush 完（RTL 的 `fireEvent` 自带 `act()` 包裹）。
    expect(factory.last.readyState).not.toBe(ReadyState.OPEN);
    // 核心断言②：settings 屏（连同整个 AppRuntime）立即从 DOM 消失——不是等 purge 完成才卸载；
    // 旧版 bug 恰恰是 purge 跑的时候 AppRuntime（连同这个活跃 WebSocket）仍然挂在树上。
    expect(screen.queryByTestId("settings-unpair-button")).toBeNull();

    // purge 完成后落回配对屏。
    await screen.findByTestId("pairing-manual-entry-textarea");
    expect(await keyStore.loadKeys()).toBeNull();
    // 核心断言③：房间三库真的被删干净了（不是卸载了 AppRuntime 就假装完事，purge 本身要真的跑完）。
    await waitFor(async () => expect(await rawProbeDbHasRecord(eventsDbName)).toBe(false));
  });
});

describe("purgeRoomDataWithTimeout() · msgfix2 U4 修单二 I3", () => {
  it("purge 正常落地（远早于超时）——原样透传结果，不等到超时", async () => {
    const outcome = await purgeRoomDataWithTimeout({ room: "room-fast", deleteIndexedDb: async () => {} }, 5000);
    expect(outcome).toEqual({ ok: true });
  });

  it("purge 迟迟不 settle——超过上限后返回失败态（可见的『timed out』诊断信息），不无限期挂着", async () => {
    // 永不 resolve/reject 的 deleteIndexedDb——模拟一次异常慢/卡死的真实删除。
    const outcome = await purgeRoomDataWithTimeout({ room: "room-hangs", deleteIndexedDb: () => new Promise<void>(() => {}) }, 50);
    expect(outcome.ok).toBe(false);
    expect(!outcome.ok && outcome.error).toMatch(/timed out/);
  });

  it("purge 失败（不是挂起，是真的报错）——原样透传失败结果，不被误判成『超时』", async () => {
    const outcome = await purgeRoomDataWithTimeout(
      {
        room: "room-fails",
        deleteIndexedDb: async () => {
          throw new Error("delete failed for real (test)");
        },
      },
      5000,
    );
    expect(outcome).toEqual({ ok: false, error: "delete failed for real (test)" });
  });
});
