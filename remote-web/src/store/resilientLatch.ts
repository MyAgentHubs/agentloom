// resilientLatch.ts — msgfix2 U4 修单二 I1：四个「运行期事务失败降级内存」wrapper（`bodyCache.ts::
// ResilientBodyCache` / `commandLedger.ts::ResilientCommandLedger` / `key-store.ts::ResilientKeyStore`
// / `inMemoryEventStore.ts::ResilientEventStore`）共用的单向闩——一旦某次操作探测到 primary 失败就
// 永久切换到内存 fallback，不回切（IndexedDB 配额耗尽/连接损坏这类问题通常不会在下一次调用自愈，见
// 四个文件各自 `withMemoryFallback()` 头注，591a5c98 已有的既有设计）。
//
// **这份修单新增的部分——竞态检测**：光"单向"还不够。四库原来的写法里，`target()`（这里改名
// `resolve()`）只在**发起**操作那一刻决定走 primary 还是 fallback；一次耗时的 primary 操作在飞行
// 途中，另一个并发操作可能先一步失败触发降级。原来那次仍在飞的 primary 操作事后成功落地时，两个
// 账本就分裂了：这条数据进了 primary，但闩已经跳、后续所有读写都只看 fallback，永远读不到这条数据
// ——不是"丢"（primary 磁盘上仍在），是"两本账对不上"（同一份逻辑状态一部分在 primary、一部分在
// fallback，谁都不完整）。
//
// `resolve()` 之外暴露 `isTripped`——调用方在发起操作前读一次记下来，操作落地后再读一次比对：变了
// 就说明降级发生在这次飞行途中，调用方据此把这次已经拿到手的 primary 结果丢弃不落账（`get` 类操作
// 丢弃即"当作未命中"，`put`/`delete` 类操作丢弃即"当作没发生过,不追加写 fallback"——这个类本身不替
// 调用方做"丢弃"这个动作，因为丢弃语义随每个方法的既有契约而不同，留给各自的方法体按既有取向处理）。
export class ResilientLatch<T> {
  private tripped = false;
  private fallbackInstance: T | null = null;

  constructor(private readonly makeFallback: () => T) {}

  get isTripped(): boolean {
    return this.tripped;
  }

  /** 当前应该操作哪个 store：闩没跳用 primary；跳了就懒构造（只建一次）fallback。 */
  resolve(primary: T): T {
    if (!this.tripped) return primary;
    if (!this.fallbackInstance) this.fallbackInstance = this.makeFallback();
    return this.fallbackInstance;
  }

  /** 已经构造过的 fallback 实例（未降级过、或降级过但还没真正调用过 `resolve()` 时是 `null`）——
   *  供 `close()` 这类"只透传给已经存在的实例、不该现在才强行构造一个"的场景读。 */
  get fallbackIfBuilt(): T | null {
    return this.fallbackInstance;
  }

  /** 跳闸——单向，重复调用是 no-op（已经跳过再跳不改变状态，也不重新构造 fallback）。 */
  trip(): void {
    this.tripped = true;
  }
}
