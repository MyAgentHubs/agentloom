// inMemoryEventStore.ts — msgfix2 U4 · `EventStorePort` 的纯内存实现——fallback adapter 的一环
// （`store/idbFactory.ts::createStoreFactory()` 探测 IndexedDB 失败时,整套（EventStore/
// CommandLedger/KeyStore/body cache）换成内存实现，见该文件设计稿 §4.2 末段）。
//
// 与 `store/indexeddbEventStore.ts::IndexedDbEventStore` 同构（同一份 `EventStorePort` 契约）：
// `applyEventIfNew` 幂等 + 水位 `max()` 语义（重投/乱序不倒退）+ `listEvents()` 按 `seq` 升序。
// 内存态"同一事务"这条不变量天然成立——JS 单线程、下面每个方法体内没有任何 `await` 打断点，
// 同步代码块之间不可能被别的调用交错进来，不需要真的搬一层 IndexedDB 式的事务 API。
//
// **代价诚实**：这是纯内存，进程/页面刷新即空——`getWatermark()`/`hasAppliedClientMsgId()`/
// `listEvents()` 重载后全部归零，跟 IndexedDB 版本"重载后能从日志重建"的持久化承诺不同。这是
// fallback 场景（探测已经判定 IndexedDB 不可用）下能做到的唯一选择，不是本文件的缺陷——调用方
// （`AppRuntime.tsx` 的冷启动重放逻辑）本就按 `listEvents()` 可能返回空数组的情况正常运作
// （空数组=从头开始，不是异常）。

import type { ApplyEventInput, ApplyEventResult, EventStorePort, StoredEvent } from "./port.ts";
import { ResilientLatch } from "./resilientLatch.ts";

export class InMemoryEventStore implements EventStorePort {
  private readonly applied = new Map<string, StoredEvent>();
  private watermark = 0;

  async applyEventIfNew(input: ApplyEventInput): Promise<ApplyEventResult> {
    const alreadyApplied = this.applied.has(input.clientMsgId);
    // max()，不是"最后写入的那个值"——同 IndexedDB 版本的既有取向，重投/乱序到达不能让水位倒退。
    const nextWatermark = Math.max(this.watermark, input.seq);
    if (!alreadyApplied) {
      this.applied.set(input.clientMsgId, {
        clientMsgId: input.clientMsgId,
        seq: input.seq,
        session: input.session,
        frame: input.frame,
      });
    }
    this.watermark = nextWatermark;
    return { applied: !alreadyApplied, watermark: nextWatermark };
  }

  async getWatermark(): Promise<number> {
    return this.watermark;
  }

  async hasAppliedClientMsgId(clientMsgId: string): Promise<boolean> {
    return this.applied.has(clientMsgId);
  }

  async listEvents(): Promise<StoredEvent[]> {
    return [...this.applied.values()].sort((a, b) => a.seq - b.seq);
  }

  /** 无持久连接可言——no-op，仅满足 `EventStorePort` 可选 `close()` 契约（`store/cacheManager.ts`
   *  统一对所有实现调用 `.close?.()`，不需要区分是不是内存实现）。 */
  close(): void {}

  /** msgfix2 U4 修单二 I4：内存 fallback 态（`idbAvailable:false`）下 `store/cacheManager.ts::
   *  purgeRoomData()` 真正的"删库"落点——`close()` 对内存实现本就是 no-op，之前只调 `close()` 就
   *  直接判定成功，这个实例内部的 `applied`/`watermark` 压根没被清空，读回旧数据依旧在。 */
  async clear(): Promise<void> {
    this.applied.clear();
    this.watermark = 0;
  }
}

/**
 * msgfix2 U4 修单 H1：运行期事务失败降级内存（单点包装）——同 `store/bodyCache.ts::
 * withMemoryFallback` 的既有思路（687db99a 已经给 body cache 包过、审查通过，这里不动那份，只是
 * 复用同一条设计）。修单前 `store/idbFactory.ts::createStoreFactory()` 只给 body cache 包了这层，
 * `createEventStore` 直接返回裸 `IndexedDbEventStore`——探测成功之后某次真实 `applyEventIfNew()`
 * 事务失败（配额耗尽/连接损坏）会直接抛到 `useAppRuntimeFrameIngestion.ts::processIncomingFrame`，那条帧被当
 * `storeError` 丢弃、**永久丢失**（不是"稍后重试"，relay 不会重发已经确认过的帧）。包这层之后
 * 同一个实例内部换到内存实现继续应用，帧不再丢；`getWatermark()`/`hasAppliedClientMsgId()`/
 * `listEvents()` 降级后只能反映"切换之后"内存态里的内容（切换之前已经在 primary 落盘过的行仍然
 * 在磁盘上，只是这个降级后的实例读不到——同探测失败时整套内存 fallback 的既有代价诚实声明，见
 * `inMemoryEventStore.ts` 文件头注"代价诚实"一段）。
 */
export function withMemoryFallback(primary: EventStorePort, makeFallback: () => EventStorePort = () => new InMemoryEventStore()): EventStorePort {
  return new ResilientEventStore(primary, makeFallback);
}

class ResilientEventStore implements EventStorePort {
  private readonly latch: ResilientLatch<EventStorePort>;

  constructor(
    private readonly primary: EventStorePort,
    makeFallback: () => EventStorePort,
  ) {
    this.latch = new ResilientLatch(makeFallback);
  }

  async applyEventIfNew(input: ApplyEventInput): Promise<ApplyEventResult> {
    const startedTripped = this.latch.isTripped;
    try {
      const result = await this.latch.resolve(this.primary).applyEventIfNew(input);
      if (!startedTripped && this.latch.isTripped) {
        // msgfix2 U4 修单二 I1：竞态——这次飞行途中闩被别的操作跳了，primary 这次落地的结果丢弃
        // 不落账；但事件本体不能真的丢（同下面 catch 分支"不能让一次事务失败变成这条消息再也不会
        // 出现"同一条取向）——换到（此刻已经是当前的）fallback 重新应用一次。
        return await this.latch.resolve(this.primary).applyEventIfNew(input);
      }
      return result;
    } catch {
      this.latch.trip();
    }
    try {
      // 降级后必须真的重试——这条事件的去重回执/水位/本体三者若没能落盘，调用方
      // （`processFrame`）会把它当"存储失败"永久丢弃，不能让一次事务失败变成"这条消息再也不会
      // 出现"（同 body cache put() 的"不白白丢掉这次已经拿到手的内容"取向，只是这里换成事件本体）。
      return await this.latch.resolve(this.primary).applyEventIfNew(input);
    } catch {
      // 双重失败（内存实现理论上不会到这一步）——纵深防御：不让异常冒泡撞碎 `processFrame` 的
      // 归约主循环；调用方按既有 `!result.applied` 分支静默丢弃计数，不是"崩"（`result.watermark`
      // 在那条分支上从不被消费，这里给的值只是类型完整，不参与任何判断）。
      return { applied: false, watermark: 0 };
    }
  }

  async getWatermark(): Promise<number> {
    const startedTripped = this.latch.isTripped;
    try {
      const result = await this.latch.resolve(this.primary).getWatermark();
      if (!startedTripped && this.latch.isTripped) return 0; // I1：竞态丢弃，安全默认值。
      return result;
    } catch {
      this.latch.trip();
      try {
        return await this.latch.resolve(this.primary).getWatermark();
      } catch {
        return 0;
      }
    }
  }

  async hasAppliedClientMsgId(clientMsgId: string): Promise<boolean> {
    const startedTripped = this.latch.isTripped;
    try {
      const result = await this.latch.resolve(this.primary).hasAppliedClientMsgId(clientMsgId);
      if (!startedTripped && this.latch.isTripped) return false; // I1：竞态丢弃。
      return result;
    } catch {
      this.latch.trip();
      try {
        return await this.latch.resolve(this.primary).hasAppliedClientMsgId(clientMsgId);
      } catch {
        return false;
      }
    }
  }

  async listEvents(): Promise<StoredEvent[]> {
    const startedTripped = this.latch.isTripped;
    try {
      const result = await this.latch.resolve(this.primary).listEvents();
      if (!startedTripped && this.latch.isTripped) return []; // I1：竞态丢弃。
      return result;
    } catch {
      this.latch.trip();
      try {
        return await this.latch.resolve(this.primary).listEvents();
      } catch {
        return [];
      }
    }
  }

  close(): void {
    this.primary.close?.();
    this.latch.fallbackIfBuilt?.close?.();
  }
}
