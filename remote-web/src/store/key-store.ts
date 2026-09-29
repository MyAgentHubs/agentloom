// key-store.ts — T6b · KeyStore port（`saveKeys`/`loadKeys`/`clear`）+ 内存实现。
//
// Rationale: the protocol contract for on-device credential storage —
// "K_room、能力令牌、device_id、relay_url、room 存 IndexedDB…K_room 同样建议以 non-extractable
// AES-256-GCM CryptoKey 存"。M0 §9.5 手机侧落盘契约："收 ready…一个 IndexedDB 事务落盘
// {access, refresh} 后才许拿 access 重连"。
//
// **范围裁定**：spec §3.7 还提到"由 K_pair 派生 per-device rewrap key，以 non-extractable
// CryptoKey 存"——这把 rewrap key 用于设备撤销后的 K_room 重新包裹，spec 自己把它标注"进 Wave 2
// 加固批 G6"（未定稿的推导公式，不属于本单 M0 §9.5 激活流程需要的材料）。本单不发明一个没有依据
// 的 HKDF info 字符串去派生它——KeyStore 只落储 PairingSession 在 M0 §9.5 激活时点真正拿到手的
// 材料（access/refresh/deviceId/room/relayUrl + K_room 本体，K_room 按 §3.7"建议"落成
// non-extractable CryptoKey）。rewrap key 的落储留给 G6 单，届时若接口需要扩字段直接加。

import { ResilientLatch } from "./resilientLatch.ts";

/**
 * T6c-refresh 新增：手机首次发送 `token.refresh` 前必须原子落盘的凭据代（M0 §9.5/9.6·§3.7 落盘
 * 契约第②条）。`generation` 是**信息性可选**字段（M0 §9.6 原文："pending_refresh {request_id,
 * generation(所属凭据代·信息性)}"）——手机不是 generation 的权威来源（那是桌面 app DB 的单调计数
 * 器，§9.2 末句），这里如果有值也只是客户端自己认为"这次轮换是对着第几代凭据发的"的记账，不参与
 * 任何服务端校验。
 */
export interface PendingRefresh {
  requestId: string;
  generation?: number;
  /**
   * T6c-refresh 审查返工新增：本次请求最初发出时的本地时钟毫秒时间戳。跨页面重载后，
   * `ConnectionSession` 内存里的 `refreshSentAtMs` 会丢失（初值 `null`）——若不把发送时刻一并
   * 落盘，重载恢复的 pending 收到回执时用 `refreshSentAtMs ?? now` 算出的往返耗时恒为 0，"过期
   * 重放二次轮换"（M2 C1 spec §3.7 第③条）的判定条件永远不成立、一个真正陈旧的重放回执会被当
   * 成全新鲜的凭据接受。落盘这个时刻后重载恢复也能正确测到"这次请求其实发出去很久了"。
   */
  sentAtMs?: number;
}

/** PairingSession 在 M0 §9.5 激活屏障通过后落盘的凭据。 */
export interface StoredPairingCredentials {
  deviceId: string;
  room: string;
  relayUrl: string;
  access: string;
  refresh: string;
  /** K_room 落储为 non-extractable AES-256-GCM CryptoKey（M2 C1 spec §3.7）。 */
  kRoomKey: CryptoKey;
  /**
   * T6c-refresh 新增（可选字段）：refresh 请求/回执密文体用的长期设备密钥原始字节
   * （`token.refresh`/`token.refresh.ok` 按 M0 §9.5 末句 + `remote_pairing.rs`
   * `open_token_refresh_request`/`seal_token_refresh_ok` 用的是 **K_pair**，不是 K_room——桌面侧
   * 对称持久化在 `remote_pairing::store::load_k_pair`，见 `app/src-tauri/src/lib.rs:695`）。
   *
   * **已知缺口（本单实勘发现，任务书 §4⑥ 记档，非本单越界修）**：`PairingSession`
   * （`remote-web/src/pairing/pairing-session.ts`·T6b·本单 SCOPE 外不可改）当前的
   * `persistActivation()` 落盘时只传 `{deviceId, room, relayUrl, access, refresh, kRoomKey}`，
   * 从未把它内存里短暂持有的 `kPair` 一并交给 `saveKeys()`——K_pair 在 pairing 流程结束后就从
   * 内存中丢失了。本字段只打通"存哪"的通道；"谁来存"（给 `pairing-session.ts` 补一行
   * `kPair: this.kPair` 传参）留给后续小刀补上。`ConnectionSession` 因此把 `kPair` 设计成显式注入
   * 依赖（构造函数必填参数，不是从 `loadKeys()` 静默摸出来的可选字段）——调用方现阶段必须自己想
   * 办法拿到它（如整合 PairingSession 与 ConnectionSession 时在内存里直接传递），直到上述小刀落地
   * 为止。
   */
  kPair?: Uint8Array;
  /** T6c-refresh 新增（可选字段）：见 `PendingRefresh` 注释。`null`/缺省 = 当前无飞行中的 refresh。 */
  pendingRefresh?: PendingRefresh | null;
  /**
   * T6c-refresh 新增（可选字段）：`access` 最近一次（重新）签发时的本地时钟毫秒时间戳——
   * `ConnectionSession` 用它估算"access 大概还有多久到期"（M0 §3 v0.5 块："浏览器约束…分类只能靠
   * 本地过期钟"）。这**不是**服务端认证的到期时间（协议里 `pair.accept`/`token.refresh.ok` 的密文
   * 明文都只有令牌本身,没有到期时间戳——relay/桌面从不告诉手机 access 精确何时过期),只是"我最后
   * 一次拿到新 access 是什么时候"的本地记账，配合 M0 §9.2 的名义值（access 1h 名义寿命、prev 别名
   * 48h 宽限窗）做保守估算,过 reload 需要它才能延续估算,故随凭据一起落盘。
   */
  accessIssuedAtMs?: number;
}

export interface KeyStorePort {
  saveKeys(creds: StoredPairingCredentials): Promise<void>;
  loadKeys(): Promise<StoredPairingCredentials | null>;
  clear(): Promise<void>;
  /**
   * T6c-refresh 新增（**可选方法**——见下）：`pendingRefresh` 单独落盘（在已有凭据记录上原地改写
   * 这一个字段，仍是单个 IndexedDB 事务）——M0 §9.5 落盘契约第②条"首次发送前原子落盘"要求的正是
   * 这一步，早于任何 `token.refresh.ok` 回执到达、早于 `saveKeys()` 会被再次调用。调用前必须已有
   * `saveKeys()` 落过的记录（配对激活之后才可能发 refresh），否则实现应当拒绝（没有凭据记录可挂载）。
   *
   * **为什么是可选的（非本单越界改动 `pairing/` 的直接后果）**：`remote-web/src/pairing/` 目录本单
   * 不可改动——若把这两个方法定成必填，`pairing/pairing-session.test.ts` 里已有的 `DeferredKeyStore`
   * 测试替身（不需要、也不该被要求实现 refresh 相关方法）就会类型报错。两个真实生产实现
   * （`InMemoryKeyStore`/`IndexedDbKeyStore`，见下）都完整实现了它们；`ConnectionSession`
   * （`connection/connectionSession.ts`）用可选链调用，方法缺失时记日志降级（不落盘 pending_refresh，
   * 但不阻塞 refresh 本身的收发）。
   */
  savePendingRefresh?(pending: PendingRefresh): Promise<void>;
  /** T6c-refresh 新增（可选方法，理由同上）：读当前挂着的 pending_refresh（无记录/未挂/方法缺失 = null）。 */
  loadPendingRefresh?(): Promise<PendingRefresh | null>;
}

/** 内存实现——单元测试用；不做任何持久化。 */
export class InMemoryKeyStore implements KeyStorePort {
  private record: StoredPairingCredentials | null = null;

  async saveKeys(creds: StoredPairingCredentials): Promise<void> {
    this.record = creds;
  }

  async loadKeys(): Promise<StoredPairingCredentials | null> {
    return this.record;
  }

  async clear(): Promise<void> {
    this.record = null;
  }

  async savePendingRefresh(pending: PendingRefresh): Promise<void> {
    if (!this.record) {
      throw new Error("savePendingRefresh() called before any credentials were saved");
    }
    this.record = { ...this.record, pendingRefresh: pending };
  }

  async loadPendingRefresh(): Promise<PendingRefresh | null> {
    return this.record?.pendingRefresh ?? null;
  }
}

/**
 * msgfix2 U4 修单 H1：运行期事务失败降级内存（单点包装）——同 `store/bodyCache.ts::
 * withMemoryFallback` 的既有思路（687db99a 已经给 body cache 包过、审查通过，这里不动那份，只是
 * 复用同一条设计：懒构造、只建一次的内存兜底实例；一次操作失败就整体切换到内存，不是"这次失败下次
 * 还试 primary"）——`primary` 探测阶段判定为可用（`store/idbFactory.ts::createStoreFactory()` 的
 * `idbAvailable:true` 分支）之后，某次真实事务仍可能失败（配额耗尽/连接损坏），这层兜底防止那次
 * 失败直接抛到调用方（配对/重登路由 `app/RootRouter.tsx` 会因为一次 `loadKeys()` 抛错卡在
 * `"checking"` 态出不来）。
 */
export function withMemoryFallback(primary: KeyStorePort, makeFallback: () => KeyStorePort = () => new InMemoryKeyStore()): KeyStorePort {
  return new ResilientKeyStore(primary, makeFallback);
}

/**
 * msgfix2 U4 修单三 J1·**固有局限（BACKLOG，本刀不做，勿实现）**：一次 `withMemoryFallback(...)`
 * 调用对应的闩状态（`ResilientLatch.tripped`）只存在于这次运行期（内存里），不会跨重启存活——
 * `store/idbFactory.ts::createStoreFactory()` 每次启动都会重新 probe 一次 primary 是否可用，probe
 * 结果与"上次运行期是否曾经降级过"完全无关。这意味着：若某次运行期真的降级过（写进了 fallback，
 * primary 完全没收到那次更新），下次启动 probe 判定 primary 又可用了，新一轮 `ResilientKeyStore`
 * 会读到 primary 上那份"降级前的旧快照"——不知道、也无从知道 fallback（内存，进程一死就没了）上
 * 曾经有过一份更新的数据。这是"复活旧态"，不是"丢数据"（primary 磁盘上那份数据本身没错，只是不是
 * 最新的）。本刀不做恢复期重同步（跨重启把降级期间的写补写回 primary）——那需要把降级期间的写也
 * 落一份可跨重启存活的持久层（不能只是内存 fallback），改动面超出这轮"闩语义收口"的范围，留给后续
 * 单独的刀（若产品判定这个边界情形值得投入）。`key-store.test.ts` 里锁住的是这条局限本身（数据
 * 会复活），不是锁"不会复活"。
 */

class ResilientKeyStore implements KeyStorePort {
  private readonly latch: ResilientLatch<KeyStorePort>;

  constructor(
    private readonly primary: KeyStorePort,
    makeFallback: () => KeyStorePort,
  ) {
    this.latch = new ResilientLatch(makeFallback);
  }

  async saveKeys(creds: StoredPairingCredentials): Promise<void> {
    const startedTripped = this.latch.isTripped;
    try {
      await this.latch.resolve(this.primary).saveKeys(creds);
      // msgfix2 U4 修单二 I1：竞态——这次飞行途中闩被别的操作跳了，primary 这次落地的结果不算数
      // （两本账本已经分裂）；但刚配对成功的凭据不能真的丢——换到（此刻已经是当前的）fallback
      // 重新落一次，跟下面 catch 分支"不白白丢掉刚配对成功的凭据"同一条取向。
      if (!startedTripped && this.latch.isTripped) {
        await this.latch.resolve(this.primary).saveKeys(creds);
        return;
      }
      return;
    } catch {
      this.latch.trip();
    }
    try {
      // 降级后把这次已经拿到手的凭据换到内存版本重试一次——不白白丢掉刚配对成功的凭据。
      await this.latch.resolve(this.primary).saveKeys(creds);
    } catch {
      // 内存版本理论上不会失败——纵深防御，写失败静默（同 body cache 既有取向）。
    }
  }

  /**
   * msgfix2 U4 修单二 I2：读取失败必须如实上抛——凭据库不适用"静默降级继续跑"这条 body
   * cache/commandLedger/eventStore 三库共享的取向（宁可走 repair/重配对，也不能让旧凭据静默消失、
   * 或让调用方误以为"这台设备目前没有凭据"而被错误地打回配对屏）。**降级只影响写入新凭据这个
   * 场景**（`saveKeys`/`savePendingRefresh`）——读取从不因为一次失败而切到内存 fallback，也不吞错
   * 返回 `null` 掩盖 primary 里其实还在的旧凭据；`resolve()` 仍然遵循当前闩状态（若之前某次
   * `saveKeys()` 已经把闩跳到了 fallback，读取自然跟着读 fallback——这是"读当前真相"，不是"读取
   * 自己触发了降级"）。
   */
  async loadKeys(): Promise<StoredPairingCredentials | null> {
    const startedTripped = this.latch.isTripped;
    const result = await this.latch.resolve(this.primary).loadKeys();
    if (!startedTripped && this.latch.isTripped) {
      // msgfix2 U4 修单三 J1：闩在这次读取的飞行途中被另一次并发写跳闸了——这次读的是 primary
      // 上跳闸前那一刻的快照，跳闸之后落地的新数据只进了 fallback，primary 完全不知道。若直接把
      // 这份快照返回给调用方，就会出现"读己之写"窟窿：调用方以为自己读到的是当前状态，实际上错过
      // 了刚刚发生的降级写（同 `saveKeys()` 竞态分支"两本账本同数据"要解的是同一类问题，这里的
      // 对应动作是"丢弃这次结果，改从此刻已经是当前的 fallback 重读一次"）。
      return this.latch.resolve(this.primary).loadKeys();
    }
    return result;
  }

  /**
   * msgfix2 U4 修单二 I2：同 `loadKeys()`——清除失败必须上抛，不能吞掉后误报"已经清空了"（`app/
   * repair.ts::attemptRepairClear()` 靠这个方法是否抛错判定要不要真的把状态切回配对屏；静默吞错
   * 会让"旧凭据其实还在 primary 里、却被判定成已清空"这个安全缺口悄悄存在）。仍然对 primary 与
   * 已构造的 fallback 都尝试清一遍（纵深防御，防止 fallback 里也残留过旧数据），但第一个出现的
   * 错误必须冒泡给调用方，不能因为"后面那步碰巧成功了"就把前面真实的失败盖过去。
   */
  async clear(): Promise<void> {
    let error: unknown;
    try {
      await this.primary.clear();
    } catch (err) {
      error = err;
    }
    try {
      await this.latch.fallbackIfBuilt?.clear();
    } catch (err) {
      if (error === undefined) error = err;
    }
    if (error !== undefined) throw error;
  }

  async savePendingRefresh(pending: PendingRefresh): Promise<void> {
    const startedTripped = this.latch.isTripped;
    try {
      await this.latch.resolve(this.primary).savePendingRefresh?.(pending);
      if (!startedTripped && this.latch.isTripped) {
        // I1：竞态——同 `saveKeys()`，不吞真数据，换到当前 fallback 重新落一次。
        await this.latch.resolve(this.primary).savePendingRefresh?.(pending);
        return;
      }
      return;
    } catch {
      this.latch.trip();
    }
    // 不吞第二次失败——调用方（`connection/connectionSession.ts`）把这个方法当"可能失败，降级
    // 记日志"处理（可选链调用），需要真的知道 pending_refresh 到底有没有落盘，这里静默假装成功
    // 会让重载后的"过期重放二次轮换"判定失去时间基准（`PendingRefresh.sentAtMs` 头注）。
    await this.latch.resolve(this.primary).savePendingRefresh?.(pending);
  }

  /** msgfix2 U4 修单二 I2：同 `loadKeys()`——`pendingRefresh` 是挂在凭据记录上的字段，读取失败同样
   *  如实上抛，不切降级、不吞错误。方法本身缺失（可选方法契约）仍然按既有取向静默为 `null`，那不是
   *  错误，是"这套 primary 实现压根没打算实现刷新态"这个已知设计边界。 */
  async loadPendingRefresh(): Promise<PendingRefresh | null> {
    return (await this.latch.resolve(this.primary).loadPendingRefresh?.()) ?? null;
  }
}

/** K_room 原始字节 → non-extractable AES-256-GCM CryptoKey（M2 C1 spec §3.7 落储前置步骤）。 */
export async function importNonExtractableAesGcmKey(rawKeyBytes: Uint8Array): Promise<CryptoKey> {
  const subtle = globalThis.crypto?.subtle;
  if (!subtle) {
    throw new Error("WebCrypto SubtleCrypto is unavailable in this runtime");
  }
  if (rawKeyBytes.length !== 32) {
    throw new Error(`AES-256-GCM key must be 32 bytes, got ${rawKeyBytes.length}`);
  }
  return subtle.importKey("raw", Uint8Array.from(rawKeyBytes), "AES-GCM", false, ["encrypt", "decrypt"]);
}
