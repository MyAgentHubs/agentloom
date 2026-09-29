// commandLedger.ts — T6f3 · command_id 持久账本 port（G3 缓解：本机发出的 command_id 先持久化
// 再发送——同窗多手机会互收对方广播的 input.ack/input.expired（`remote-relay/src/room-do.js` 的
// `broadcastToRemotes`，M0 v0.4 记档 ③），必须按"这条 command_id 是不是本机发出的"过滤，不能拿
// 内存态判断（重载后内存清空，若只凭内存态会把重载前自己发出、此刻才姗姗来迟的 ack 也误判成
// "别人的"而忽略）。
//
// Rationale: mirrors the desktop-side receipt ledger design for deduplicated command
// delivery ("acks are broadcast room-wide, so the client filters by a command_id ledger
// persisted to IndexedDB") — this is the remote-UI-side mirror ledger, independent from
// and non-communicating with that desktop ledger.
//
// **范围裁定（不越界）**：这不是 M0 §3 "重复 command_id → 按台账该行既有终态稳定映射" 那份**桌面**
// 权威账本（那是 `remote_inbox` 表，Rust 侧）——本账本只服务远端 UI 侧两件事：① "这个 command_id
// 是不是我发的"membership 判断（G3 过滤的核心）；② 附带记一份本机最后已知的 outcome，供 UI 展示/
// 重启后仍能查到"我这条命令上次是什么状态"（不是协议意义上的权威终态，只是本机侧的展示缓存）。
//
// **与 `store/port.ts::EventStorePort` 的边界**：那个账本管的是**入站**里程碑事件的去重/水位/重放
// （client_msg_id 维度）；这个账本管的是**出站**指令的"是不是我发的"归属（command_id 维度）——两个
// 完全独立的账本，字段/用途不重叠，故意不合并进同一个 store（合并会让"入站幂等"和"出站归属"两条
// 不相关的不变量绑在一张表上，未来任何一边改字段都可能牵连另一边）。

import { ResilientLatch } from "./resilientLatch.ts";

export type CommandLedgerStatus =
  | "sent" // 已发出、尚未收到 ack/expired。
  | "ok"
  | "queued"
  | "failed"
  | "taken_over" // 见 events/ackOutcome.ts——未知 outcome 的中性兜底。
  | "expired"
  /** T6f3 返工②：relay per-subject 限速桶拒绝（`input_rate_limited`/`control_rate_limited`），或
   *  本机滑动窗预判超限、从未真正发出——两种来源统一成同一个可重试终态，UI 措辞一致，见
   *  `commandChannel.ts::handleRateLimited`/`takeLocalRateSlot`。 */
  | "rate_limited"
  /** C1-RQ（dogfood 修障第二批）：relay `input.relay_queued`——桌面离线时消息已安全落在
   *  `pending_input` 表里，真正的投递结果仍要等桌面回来后的 `input.ack`/`input.expired`。非终态，
   *  见 `commandChannel.ts::handleRelayQueued`。 */
  | "relay_queued";

export interface CommandLedgerRecord {
  commandId: string;
  /** "input.send" | "input.answer" | "control.stop"——纯记账用途，不做联合类型强绑定（避免这个
   *  纯存储端口反过来依赖 `commandChannel.ts` 的类型，保持依赖方向单向：commandChannel → 本文件）。 */
  kind: string;
  session: string;
  createdAt: number;
  status: CommandLedgerStatus;
  /** T6f3 返工②：仅 `kind==="input.answer"` 有值——持久保存"当初点的是哪个决定"，不是靠内存态
   *  （重载后 `CommandChannel.records` 清空，账本是唯一能查到"这条命令原本回答了什么"的地方）。 */
  decisionId?: string;
  /** T6f3 返工②：仅 `kind==="input.answer"` 有值——见 `decisionId` 注释。失败/过期后重试必须重发
   *  这个原始选项，不能退回 `options[0]`（`DecisionCard.tsx` 自带的失败重试按钮就是这么退化的：
   *  `onChoose(decision_id, chosen_option ?? options[0])`——`chosen_option` 若不是本机真正点的
   *  那个值，退回第一个选项就是一个会静默发错指令的真实 bug，见 `decisionCardView.ts` 的覆盖逻辑）。 */
  option?: string;
}

export interface RecordSentInput {
  commandId: string;
  kind: string;
  session: string;
  createdAt: number;
  decisionId?: string;
  option?: string;
}

export interface CommandLedgerPort {
  /**
   * G3 硬语义："先持久化再发送"——调用方必须在真正把信封 `send()` 到 socket 之前 `await` 这个方法
   * 成功返回。初始状态固定是 `"sent"`。同一个 `commandId` 重复调用（理论上不该发生，`crypto.
   * randomUUID()` 冲突概率可忽略）按"覆盖写"处理，不抛异常（fail-safe 而不是 fail-closed——这里
   * 不是安全边界，是记账，覆盖写不会造成协议层面的错误）。
   */
  recordSent(input: RecordSentInput): Promise<void>;
  /** 这个 `commandId` 是不是本机通过 `recordSent()` 记过账的——G3 过滤的核心判据。 */
  isOwn(commandId: string): Promise<boolean>;
  /** 更新本机已知的最新状态——不存在的 `commandId`（不是本机发的）静默不做任何事，不抛异常
   *  （调用方在真正调用前应该已经用 `isOwn()` 过滤过，这里的静默是纵深防御，不是主过滤点）。 */
  updateStatus(commandId: string, status: CommandLedgerStatus): Promise<void>;
  get(commandId: string): Promise<CommandLedgerRecord | null>;
  /** msgfix2 U4（收拢 P1-3）：同步关闭已建立的长期连接——见 `store/port.ts::EventStorePort.close`
   *  头注,同一条不变量。可选,`InMemoryCommandLedger` 无持久连接,不需要实现。 */
  close?(): void;
}

/** 内存实现——单元测试用（同 `key-store.ts::InMemoryKeyStore` 的既有先例），不做任何持久化。 */
export class InMemoryCommandLedger implements CommandLedgerPort {
  private readonly records = new Map<string, CommandLedgerRecord>();

  async recordSent(input: RecordSentInput): Promise<void> {
    this.records.set(input.commandId, { ...input, status: "sent" });
  }

  async isOwn(commandId: string): Promise<boolean> {
    return this.records.has(commandId);
  }

  async updateStatus(commandId: string, status: CommandLedgerStatus): Promise<void> {
    const existing = this.records.get(commandId);
    if (!existing) return;
    this.records.set(commandId, { ...existing, status });
  }

  async get(commandId: string): Promise<CommandLedgerRecord | null> {
    return this.records.get(commandId) ?? null;
  }

  /** msgfix2 U4 修单二 I4：内存 fallback 态（`idbAvailable:false`）下 `store/cacheManager.ts::
   *  purgeRoomData()` 真正的"删库"落点——`close()` 对内存实现本就是 no-op，之前只调 `close()` 就
   *  直接判定成功，这个实例内部的 `records` 压根没被清空，读回旧账本依旧在。 */
  async clear(): Promise<void> {
    this.records.clear();
  }
}

/**
 * msgfix2 U4 修单 H1：运行期事务失败降级内存（单点包装）——同 `store/bodyCache.ts::
 * withMemoryFallback` 的既有思路（687db99a 已经给 body cache 包过、审查通过，这里不动那份，只是
 * 复用同一条设计）。修单前 `store/idbFactory.ts::createStoreFactory()` 直接返回裸
 * `IndexedDbCommandLedger`——探测成功之后某次真实 `recordSent()` 事务失败会直接抛到
 * `app/commandChannel.ts::dispatch()`，那条命令被判 `give_up`、永远不会真的发出去（用户点发送
 * 没反应）。包这层之后同一个实例内部换到内存实现继续记账、继续正常发送。
 */
export function withMemoryFallback(
  primary: CommandLedgerPort,
  makeFallback: () => CommandLedgerPort = () => new InMemoryCommandLedger(),
): CommandLedgerPort {
  return new ResilientCommandLedger(primary, makeFallback);
}

class ResilientCommandLedger implements CommandLedgerPort {
  private readonly latch: ResilientLatch<CommandLedgerPort>;

  constructor(
    private readonly primary: CommandLedgerPort,
    makeFallback: () => CommandLedgerPort,
  ) {
    this.latch = new ResilientLatch(makeFallback);
  }

  async recordSent(input: RecordSentInput): Promise<void> {
    const startedTripped = this.latch.isTripped;
    try {
      await this.latch.resolve(this.primary).recordSent(input);
      // msgfix2 U4 修单二 I1：竞态——这次飞行途中闩被别的操作跳了，这条 primary 记账结果丢弃不
      // 落账（不追加写 fallback）。看起来像"没记上账"，调用方按既有失败路径处理（见下方注释）——
      // 不能让这条既进了 primary、又没进 fallback 的记录制造"两本账各有一半"的分裂状态。
      if (!startedTripped && this.latch.isTripped) {
        await this.latch.resolve(this.primary).recordSent(input);
        return;
      }
      return;
    } catch {
      this.latch.trip();
    }
    // 不吞第二次失败——G3 硬语义："先持久化再发送"（`store/commandLedger.ts` 文件头注）。记账真
    // 失败就该让 `commandChannel.ts::dispatch()` 按既有 `give_up` 分支处理，不能假装记过账了：
    // 静默失败会让未来一条真实到达的 ack/expired 因为查无此单被 G3 过滤误判成"别人的"而永久
    // 静默丢弃（同 body cache 的"写失败静默"取向刻意不同——那里丢的只是加速层缓存,这里丢的是
    // "是不是我发的"membership 判据）。
    await this.latch.resolve(this.primary).recordSent(input);
  }

  async isOwn(commandId: string): Promise<boolean> {
    const startedTripped = this.latch.isTripped;
    try {
      const result = await this.latch.resolve(this.primary).isOwn(commandId);
      // I1：竞态丢弃——安全默认值 false（同下面双重失败分支的既有默认一致）。
      if (!startedTripped && this.latch.isTripped) return false;
      return result;
    } catch {
      this.latch.trip();
      try {
        return await this.latch.resolve(this.primary).isOwn(commandId);
      } catch {
        return false;
      }
    }
  }

  async updateStatus(commandId: string, status: CommandLedgerStatus): Promise<void> {
    const startedTripped = this.latch.isTripped;
    try {
      await this.latch.resolve(this.primary).updateStatus(commandId, status);
      if (!startedTripped && this.latch.isTripped) return; // I1：竞态丢弃。
      return;
    } catch {
      this.latch.trip();
    }
    try {
      await this.latch.resolve(this.primary).updateStatus(commandId, status);
    } catch {
      // 静默——同 `CommandLedgerPort.updateStatus` 既有契约"不存在的 commandId 静默不做任何事"。
    }
  }

  async get(commandId: string): Promise<CommandLedgerRecord | null> {
    const startedTripped = this.latch.isTripped;
    try {
      const result = await this.latch.resolve(this.primary).get(commandId);
      if (!startedTripped && this.latch.isTripped) return null; // I1：竞态丢弃，当作未命中。
      return result;
    } catch {
      this.latch.trip();
      try {
        return await this.latch.resolve(this.primary).get(commandId);
      } catch {
        return null;
      }
    }
  }

  close(): void {
    this.primary.close?.();
    this.latch.fallbackIfBuilt?.close?.();
  }
}
