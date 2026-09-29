// bodyCache.ts — msgfix2 U4 · body cache port（设计稿 §4.2 全节）：可随时 LRU 淘汰的「正文」层，
// 跟 `store/port.ts::EventStorePort`（durable compact index，游标/去重回执，不参与 LRU）是两个独立
// 账本——见该文件设计稿 §F/§4.2 头两段的"纠错"分层论述。这里只存**已经过 fetch 重组成功 + SHA-256
// 校验过**的全文（`events/msgFetch.ts::MsgChunkReassembler` 产出的 `outcome.bytes`/`outcome.
// contentRef`），淘汰只丢正文——游标/索引不动，重看时按 `content_ref` 重拉，永不因淘汰产生"以为已
// 同步"的假水位。
//
// key = `{room, session, messageId, contentSha256}`——revision 变化天然换 key（新 revision 的
// `content_sha256` 必然不同，除非内容真的字节级相同——那种情况下"复用旧缓存"反而是对的），孤儿
// （旧 revision 遗留的行）交给 LRU 自然淘汰，不需要显式按 revision 清理。

import { ResilientLatch } from "./resilientLatch.ts";

export interface BodyCacheKey {
  room: string;
  session: string;
  messageId: number;
  contentSha256: string;
}

export interface CachedBody {
  /** `msgFetch.ts` 重组完成后已经解出的 blocks 数组——直接喂 `MilestoneProjection.applyFullText()`，
   *  不需要调用方重新 `JSON.parse(bytes)`。 */
  blocks: unknown[];
  /** SHA-256 校验过的原始字节（非再序列化）——LRU 计量按这个字段的长度算,不是 `blocks` 的
   *  JSON.stringify 长度（设计稿 §4.2："缓存存 SHA 校验后的原始 bytes（非再序列化）"）。 */
  bytes: Uint8Array;
  cachedAt: number;
}

export interface BodyCachePort {
  get(key: BodyCacheKey): Promise<CachedBody | null>;
  /** 只存"完整全文"——没有部分写入语义（重组失败/校验失败的半成品不该调用这个方法，调用方
   *  `msgFetch.ts` 只在 `outcome.status==="complete"` 时才触发缓存写钩子）。命中已有 key 时整体
   *  覆盖（同 revision 内容理论上字节相同,覆盖是安全的 no-op；不同 revision 天然是不同 key,不会
   *  撞进这条覆盖路径）。 */
  put(key: BodyCacheKey, blocks: unknown[], bytes: Uint8Array): Promise<void>;
  /**
   * msgfix2 U4 修单 H4：按单个 key 精确删除——`clear()` 是整库清空（缓存开关关闭用），四触发点的
   * `purgeRoomData()` 是整库连库带删（`deleteDatabase()`），都不是这里要的粒度。这个方法服务的是
   * "已经确定这一条缓存命中对不上当前 revision 了，把这条孤儿清掉、不留着占位等 LRU 慢慢淘汰"这个
   * 单一场景（`app/AppRuntime.tsx::loadFullTextViaCacheOrFetch`）——key 不存在时是 no-op，不抛错。
   */
  delete(key: BodyCacheKey): Promise<void>;
  /** 缓存开关关闭时"立即清"落点，或四触发点（repair/显式解除配对/re-pair 换房/device_revoked 后
   *  refresh 走死）统一走 `store/cacheManager.ts::purgeRoomData()`（那条路径直接
   *  `deleteDatabase()`，不经过这个方法——这个方法是"清空但保留库"的语义，供设置开关关闭时用，
   *  不需要真的删库）。 */
  clear(): Promise<void>;
  /** 供 `store/cacheManager.ts` 的 close-then-delete 统一调用——可选：内存实现没有持久连接，
   *  不需要真的做什么，省略即可（cache manager 用 `?.()` 调用，不强制所有实现都提供）。 */
  close?(): void;
}

export function bodyCacheKeyString(key: BodyCacheKey): string {
  return `${key.room}|${key.session}|${key.messageId}|${key.contentSha256}`;
}

/** fallback adapter 的内存实现上限（设计稿 §4.2："body cache 内存版 = Map 上限 8MiB"）——比
 *  IndexedDB 版本的 50MiB 小得多,内存态本就该更保守（页面刷新即清空,没必要留太多）。 */
export const IN_MEMORY_BODY_CACHE_CAP_BYTES = 8 * 1024 * 1024;

interface InMemoryEntry {
  blocks: unknown[];
  bytes: Uint8Array;
  cachedAt: number;
}

/**
 * 纯内存实现——JS 单线程、`get`/`put` 内部没有任何 `await` 打断点,同步代码块之间不可能交错,天然
 * 满足"写入/计量/淘汰单一串行队列内原子化"这条不变量，不需要真的搬一层排队机制。用 `Map` 的插入
 * 顺序表达 LRU（命中/写入都"删了再插回去"，队首=最久未用，`evictIfNeeded()` 从队首开始丢）。
 */
export class InMemoryBodyCache implements BodyCachePort {
  private readonly entries = new Map<string, InMemoryEntry>();
  private totalBytes = 0;

  constructor(
    private readonly capBytes: number = IN_MEMORY_BODY_CACHE_CAP_BYTES,
    private readonly now: () => number = Date.now,
  ) {}

  async get(key: BodyCacheKey): Promise<CachedBody | null> {
    const k = bodyCacheKeyString(key);
    const entry = this.entries.get(k);
    if (!entry) return null;
    this.entries.delete(k);
    const touched: InMemoryEntry = { ...entry, cachedAt: this.now() };
    this.entries.set(k, touched);
    return { blocks: touched.blocks, bytes: touched.bytes, cachedAt: touched.cachedAt };
  }

  async put(key: BodyCacheKey, blocks: unknown[], bytes: Uint8Array): Promise<void> {
    const k = bodyCacheKeyString(key);
    const existing = this.entries.get(k);
    if (existing) {
      this.totalBytes -= existing.bytes.length;
      this.entries.delete(k);
    }
    this.entries.set(k, { blocks, bytes, cachedAt: this.now() });
    this.totalBytes += bytes.length;
    this.evictIfNeeded();
  }

  async delete(key: BodyCacheKey): Promise<void> {
    const k = bodyCacheKeyString(key);
    const existing = this.entries.get(k);
    if (!existing) return;
    this.entries.delete(k);
    this.totalBytes -= existing.bytes.length;
  }

  private evictIfNeeded(): void {
    for (const [k, entry] of this.entries) {
      if (this.totalBytes <= this.capBytes) break;
      this.entries.delete(k);
      this.totalBytes -= entry.bytes.length;
    }
  }

  async clear(): Promise<void> {
    this.entries.clear();
    this.totalBytes = 0;
  }

  /** 测试/诊断用——不是端口契约的一部分。 */
  get size(): number {
    return this.totalBytes;
  }
}

/**
 * 运行期事务失败降级内存（设计稿 §4.2「cache manager」一条，单点包装）——`primary`（生产装配点是
 * IndexedDB 实现）任何一次操作抛错，之后的调用统一换到一个（懒构造、只建一次的）内存实例，不
 * 每次都重新尝试大概率还会失败的 IndexedDB（配额耗尽/连接损坏这类问题通常不会在下一次调用自愈）。
 * 这不是"探测失败 fallback"（那是 `idbFactory.ts::createStoreFactory()` 的启动期整套降级）——这里
 * 处理的是"探测成功、用着用着某次事务却失败了"这个更晚的时间点，同一份 body cache 实例内部完成
 * 降级，调用方（`AppRuntime.tsx`）拿到的始终是同一个 `BodyCachePort` 引用，不需要感知切换。
 */
export function withMemoryFallback(primary: BodyCachePort, makeFallback: () => BodyCachePort = () => new InMemoryBodyCache()): BodyCachePort {
  return new ResilientBodyCache(primary, makeFallback);
}

class ResilientBodyCache implements BodyCachePort {
  private readonly latch: ResilientLatch<BodyCachePort>;
  /** msgfix2 U4 修单二 I6：clear 世代计数——每次 `clear()` 被调用都 +1。真正防止"迟到的慢 clear 删掉
   *  clear 发起之后才写入的新数据"这条不变量靠下面 `pendingClear` 排队；这个计数器只服务 `clear()`
   *  自己判断"我完成的时候是不是已经有更晚一次 clear() 顶替了我"（避免旧的 clear 完成时把
   *  `pendingClear` 指针清空成 `null`、误伤刚开始的新一次 clear() 的排队占位）。 */
  private clearEpoch = 0;
  /** msgfix2 U4 修单二 I6：当前正在飞行的 clear()（`null` = 没有）——`put()`/`delete()` 在真正写
   *  之前先等它落地，保证"off→clear 慢→on→写入"这种时序下,新写入必然发生在旧 clear 完成之后,不会
   *  被迟到的 clear 顺手清掉（bodyCache.ts:199-209 锚点——旧版 `clear()` 跟并发写入完全没有排队
   *  关系）。恒不 reject（内部已经 `.catch()` 过一层),调用方可以放心 `await` 不需要再包一层。 */
  private pendingClear: Promise<void> | null = null;

  constructor(
    private readonly primary: BodyCachePort,
    makeFallback: () => BodyCachePort,
  ) {
    this.latch = new ResilientLatch(makeFallback);
  }

  async get(key: BodyCacheKey): Promise<CachedBody | null> {
    const startedTripped = this.latch.isTripped;
    try {
      const result = await this.latch.resolve(this.primary).get(key);
      // msgfix2 U4 修单二 I1：竞态——这次飞行途中闩被另一个并发操作跳了，primary 这次拿到的结果
      // 丢弃不落账（不能当真：后续所有读写都只看 fallback 了，这条数据若被当命中返回，跟 fallback
      // 侧的账本对不上）。当作未命中，同下面 catch 分支的既有取向一致。
      if (!startedTripped && this.latch.isTripped) return null;
      return result;
    } catch {
      this.latch.trip();
      // 降级后这次读也当作未命中——正文本就是可随时丢失重拉的加速层，不值得为了这一次读再重试。
      return null;
    }
  }

  async put(key: BodyCacheKey, blocks: unknown[], bytes: Uint8Array): Promise<void> {
    if (this.pendingClear) await this.pendingClear;
    const startedTripped = this.latch.isTripped;
    try {
      await this.latch.resolve(this.primary).put(key, blocks, bytes);
      // msgfix2 U4 修单二 I1：竞态丢弃——不追加写 fallback（避免这条数据同时"看似"进了两本账）。
      if (!startedTripped && this.latch.isTripped) return;
      return;
    } catch {
      this.latch.trip();
    }
    try {
      // 降级后把这次已经拿到手的内容换到内存版本重试一次——不白白丢掉这次成功拉取的全文。
      await this.latch.resolve(this.primary).put(key, blocks, bytes);
    } catch {
      // 内存版本理论上不会失败——纵深防御，写失败静默（设计稿"失败静默"）。
    }
  }

  /** msgfix2 U4 修单 H4：新增方法，同 `put()` 既有取向——primary 失败就切内存重试一次,这次已经
   *  拿到手的"要删哪条"不该因为 primary 一次事务失败就放弃,重试失败再静默（纵深防御，删除失败
   *  的后果最多是这条孤儿多留一轮直到下次 LRU 自然淘汰,不是什么必须让调用方知道的错误）。 */
  async delete(key: BodyCacheKey): Promise<void> {
    if (this.pendingClear) await this.pendingClear;
    const startedTripped = this.latch.isTripped;
    try {
      await this.latch.resolve(this.primary).delete(key);
      if (!startedTripped && this.latch.isTripped) return; // I1：同 put() 的竞态丢弃取向。
      return;
    } catch {
      this.latch.trip();
    }
    try {
      await this.latch.resolve(this.primary).delete(key);
    } catch {
      // 同 put() 的纵深防御静默——见该方法注释。
    }
  }

  /**
   * msgfix2 U4 修单二 I6：① 错误不再吞——第一个出现的失败（primary 或 fallback）冒泡给调用方,让
   * `app/AppRuntime.tsx` 已有的 toggle 层 `console.error` 路径真的收到失败（旧版无论成败恒 resolve,
   * 上层可见性形同虚设）。② `pendingClear` 排队占位——`put()`/`delete()` 在这次 clear 完成前发起,
   * 会先等这次落地再动手写,天然保证"迟到的慢 clear 不会删掉它开始之后才写入的新数据"（不需要给
   * 每条缓存记录额外挂一个 epoch 字段这么重的手段）。
   */
  async clear(): Promise<void> {
    const epochAtStart = ++this.clearEpoch;
    const run = (async () => {
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
    })();
    this.pendingClear = run.catch(() => {});
    try {
      await run;
    } finally {
      if (this.clearEpoch === epochAtStart) this.pendingClear = null;
    }
  }

  close(): void {
    this.primary.close?.();
    this.latch.fallbackIfBuilt?.close?.();
  }
}
