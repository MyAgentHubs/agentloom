// bodyCache.indexeddb.ts — msgfix2 U4 · `BodyCachePort` 的 IndexedDB 实现（生产用，设计稿
// §4.2 全节）。
//
// 两个 object store：`bodies`（keyPath `key` = `room|session|messageId|contentSha256`，见
// `bodyCache.ts::bodyCacheKeyString`）+ `meta`（单条 `totalBytes` 行——O(1) 判断是否超过 LRU 上限，
// 不必每次 `put()` 都扫全表求和）。`bodies` 上建 `cachedAt` 索引，淘汰按它升序找最久未用的行。
//
// **单一串行队列（设计稿 §4.2："写入/计量/淘汰在单一串行队列内原子化"）**：`get`（含 touch 更新
// `cachedAt`）/`put`/`clear` 全部经 `enqueue()` 排队——不是"每次操作各自开一个新 IndexedDB
// 事务就叫原子"，是"同一时刻只有一个逻辑操作在跑，后一个必须等前一个（含淘汰循环）完全落地才
// 开始"，避免"总量刚读出来、还没来得及淘汰，下一个 put() 已经把总量往上叠"这类交错。
//
// **close-then-delete（收拢 P1-3）**：`close()` 同步关闭已建立的连接（`store/indexeddbEventStore.ts`
// 同款补丁——见该文件"补 close()/reset"一条），供 `store/cacheManager.ts::purgeRoomData()` 在
// `deleteDatabase()` 之前调用，避免本标签页自己的活跃连接把删库请求卡在 `onblocked`；`onversionchange`
// 处理另一个上下文（另一个标签页/另一次 open 的版本升级，含别的标签页触发的 `deleteDatabase()`）
// 触发的放手，覆盖"跨标签页阻塞面"。

import type { BodyCacheKey, BodyCachePort, CachedBody } from "./bodyCache.ts";
import { bodyCacheKeyString } from "./bodyCache.ts";

const DEFAULT_DB_NAME = "agentloom-body-cache";
const DB_VERSION = 1;
const STORE_BODIES = "bodies";
const STORE_META = "meta";
const META_KEY_TOTAL_BYTES = "totalBytes";
const INDEX_CACHED_AT = "cachedAt";

/** 设计稿 §4.2："LRU：总量 50MiB"。 */
export const BODY_CACHE_LRU_CAP_BYTES = 50 * 1024 * 1024;

/** 按房间派生库名——同 `indexeddbEventStore.ts::deriveEventStoreDbName`/
 *  `commandLedger.indexeddb.ts::deriveCommandLedgerDbName` 的既有惯例，换房不串库。 */
export function deriveBodyCacheDbName(room: string): string {
  return `${DEFAULT_DB_NAME}-${room}`;
}

interface BodyRow {
  key: string;
  bytes: Uint8Array;
  blocks: unknown[];
  cachedAt: number;
  sizeBytes: number;
}

interface MetaRow {
  key: string;
  value: number;
}

function requireIndexedDB(factory?: IDBFactory): IDBFactory {
  const resolved = factory ?? globalThis.indexedDB;
  if (!resolved) {
    throw new Error("IndexedDB is unavailable in this runtime");
  }
  return resolved;
}

export class IndexedDbBodyCache implements BodyCachePort {
  private readonly dbName: string;
  private readonly idbFactory: IDBFactory;
  private readonly capBytes: number;
  private readonly now: () => number;
  private dbPromise: Promise<IDBDatabase> | null = null;
  /** 已解析的连接句柄——`close()` 需要同步生效（调用方紧接着就要 `deleteDatabase()`），不能靠
   *  "等 `dbPromise` resolve 之后再关"的异步时序（那样调用方还没等到关闭就已经发起删库请求，
   *  一样会撞 `onblocked`）。只有真正建立过连接（`dbPromise` 已 resolve 过）才有值可关。 */
  private dbHandle: IDBDatabase | null = null;
  private queue: Promise<void> = Promise.resolve();

  constructor(
    dbName: string = DEFAULT_DB_NAME,
    idbFactory?: IDBFactory,
    capBytes: number = BODY_CACHE_LRU_CAP_BYTES,
    now: () => number = Date.now,
  ) {
    this.dbName = dbName;
    this.idbFactory = requireIndexedDB(idbFactory);
    this.capBytes = capBytes;
    this.now = now;
  }

  private enqueue<T>(work: () => Promise<T>): Promise<T> {
    const result = this.queue.then(work, work);
    this.queue = result.then(
      () => undefined,
      () => undefined,
    );
    return result;
  }

  private openDb(): Promise<IDBDatabase> {
    if (this.dbPromise) return this.dbPromise;
    this.dbPromise = new Promise((resolve, reject) => {
      const request = this.idbFactory.open(this.dbName, DB_VERSION);
      request.onupgradeneeded = () => {
        const db = request.result;
        if (!db.objectStoreNames.contains(STORE_BODIES)) {
          const store = db.createObjectStore(STORE_BODIES, { keyPath: "key" });
          store.createIndex(INDEX_CACHED_AT, "cachedAt");
        }
        if (!db.objectStoreNames.contains(STORE_META)) {
          db.createObjectStore(STORE_META, { keyPath: "key" });
        }
      };
      request.onsuccess = () => {
        const db = request.result;
        db.onversionchange = () => {
          db.close();
          if (this.dbHandle === db) this.dbHandle = null;
          // msgfix2 F2 S3：`dbPromise` 也要一并清空——同 `indexeddbEventStore.ts`/
          // `commandLedger.indexeddb.ts` 同名注释，不清的话下一次 `openDb()` 会直接复用这个已
          // close 的 stale 连接，恒抛 InvalidStateError；这条路径还包着
          // `withMemoryFallback()`，一旦触发会被那层降级闩静默吞成"切内存·历史清空零信号"
          // （见调用方 `AppRuntime.tsx` 头注）。
          this.dbPromise = null;
        };
        this.dbHandle = db;
        resolve(db);
      };
      request.onerror = () => reject(request.error ?? new Error("IndexedDB open failed"));
      request.onblocked = () => reject(new Error("IndexedDB open blocked by another connection"));
    });
    return this.dbPromise;
  }

  /** 见字段头注——同步关闭已建立的连接；从未建立过连接时是 no-op（没有什么可挡住随后的
   *  `deleteDatabase()`）。关闭后下一次操作会重新 `openDb()`（`dbPromise` 一并清空）。 */
  close(): void {
    if (this.dbHandle) {
      this.dbHandle.close();
      this.dbHandle = null;
    }
    this.dbPromise = null;
  }

  /**
   * msgfix2 U4 修单 H6：读 + touch 合并进**同一个** `readwrite` 事务（同 `key-store.indexeddb.ts::
   * savePendingRefresh` 的既有 read-modify-write 手法）——原先分两个独立事务（先 `readonly` 读，
   * 再另开一个 `readwrite` 写回 `cachedAt`），两个事务之间存在一个真空窗口：另一个标签页
   * （独立 `IndexedDbBodyCache` 连接）的 `evictIfNeeded()` 若恰好在这个窗口里把同一行删掉并从
   * `meta.totalBytes` 里扣掉它的大小，这里的 touch 写会在窗口关闭后原样把这一行"复活"回
   * `bodies` 表——`meta.totalBytes` 却已经算过一次扣减，两者从此对不上账，缓存实际占用会悄悄超过
   * 声明的上限而不再被发现。合并成一个事务后，`get()` 与另一个标签页的 `evictIfNeeded()`
   * （同样是单事务、且 scope 都包含 `bodies`）互斥执行——要么整段读+touch 先做完（那一刻这行还没
   * 被删,touch 合法,之后淘汰若还要选它,会看到刚更新过的 `cachedAt`,大概率不再选中它——它本就
   * 应该活下来）,要么淘汰先做完（这行已经真的没了,`get()` 只会读到 `undefined`,返回未命中,不会
   * 凭空写出一行没被计费的正文）——两种交织顺序结果都自洽,不再有"账本对不上"的中间态。
   *
   * **残余竞态（本单不做跨标签页锁，见任务书 H6 明确授权的范围）**：`evictIfNeeded()`（下方）自己
   * 的循环仍然是"读 `meta.totalBytes` 决定要不要继续淘汰"（一个独立只读事务）→"淘汰一条+回写
   * `meta.totalBytes`"（另一个独立读写事务）两段分开——两个标签页各自的淘汰循环之间仍可能交错
   * （谁先读到"超限"就都各自去删一条,理论上可能比严格必要的次数多删一两条）,但每一轮"删+扣费"
   * 本身仍是单事务原子的,不会再产生"删了却没扣费"或"扣了费却没删"这类真正的账本不一致——多删
   * A few extra evictions here are acceptable — not a correctness gap.
   * Cross-tab strict-consistency locking is left for later work.
   */
  async get(key: BodyCacheKey): Promise<CachedBody | null> {
    return this.enqueue(async () => {
      const db = await this.openDb();
      const k = bodyCacheKeyString(key);
      const touched = await new Promise<BodyRow | null>((resolve, reject) => {
        const tx = db.transaction(STORE_BODIES, "readwrite");
        const store = tx.objectStore(STORE_BODIES);
        const getReq = store.get(k);
        let result: BodyRow | null = null;
        getReq.onsuccess = () => {
          const row = getReq.result as BodyRow | undefined;
          if (!row) {
            result = null;
            return;
          }
          const next: BodyRow = { ...row, cachedAt: this.now() };
          store.put(next);
          result = next;
        };
        getReq.onerror = () => reject(getReq.error ?? new Error("get failed"));
        tx.oncomplete = () => resolve(result);
        tx.onerror = () => reject(tx.error ?? new Error("get: transaction failed"));
        tx.onabort = () => reject(tx.error ?? new Error("get: transaction aborted"));
      });
      if (!touched) return null;
      return { blocks: touched.blocks, bytes: touched.bytes, cachedAt: touched.cachedAt };
    });
  }

  async put(key: BodyCacheKey, blocks: unknown[], bytes: Uint8Array): Promise<void> {
    return this.enqueue(async () => {
      const db = await this.openDb();
      const k = bodyCacheKeyString(key);
      // msgfix2 F2 S5③：`sizeBytes` 只算了 `bytes`（编码后的原始正文），没算这一行同时留着的
      // `blocks`（反序列化后的对象副本，`CachedBody.blocks` 直接读它，不是每次现算）——LRU 上限
      // 保护的是"这套缓存实际占用的内存/磁盘"，只报小半个真实占用会让实际用量悄悄超过声明的
      // `BODY_CACHE_LRU_CAP_BYTES`。`blocks` 是任意结构的 JS 对象，没有一个便宜、精确的字节数
      // （要精确只能重新 `JSON.stringify` 再算一次，白白多一次序列化开销），这里改成保守估算：
      // 按 `bytes.length` 的两倍记账（`blocks` 反序列化后通常比它编码前的字节表示更占内存，两倍
      // 是"宁可少存一点、也不假装占用比实际小"的保守系数，不是精确值）。
      const sizeBytes = bytes.length * 2;
      await new Promise<void>((resolve, reject) => {
        const tx = db.transaction([STORE_BODIES, STORE_META], "readwrite");
        const bodyStore = tx.objectStore(STORE_BODIES);
        const metaStore = tx.objectStore(STORE_META);
        const getExisting = bodyStore.get(k);
        getExisting.onsuccess = () => {
          const existing = getExisting.result as BodyRow | undefined;
          const getTotal = metaStore.get(META_KEY_TOTAL_BYTES);
          getTotal.onsuccess = () => {
            const totalRow = getTotal.result as MetaRow | undefined;
            const currentTotal = totalRow?.value ?? 0;
            const nextTotal = Math.max(0, currentTotal - (existing?.sizeBytes ?? 0) + sizeBytes);
            const row: BodyRow = { key: k, bytes, blocks, cachedAt: this.now(), sizeBytes };
            bodyStore.put(row);
            metaStore.put({ key: META_KEY_TOTAL_BYTES, value: nextTotal });
          };
          getTotal.onerror = () => reject(getTotal.error ?? new Error("put: read total failed"));
        };
        getExisting.onerror = () => reject(getExisting.error ?? new Error("put: read existing failed"));
        tx.oncomplete = () => resolve();
        tx.onerror = () => reject(tx.error ?? new Error("put: transaction failed"));
        tx.onabort = () => reject(tx.error ?? new Error("put: transaction aborted"));
      });
      await this.evictIfNeeded(db);
    });
  }

  /**
   * msgfix2 U4 修单 H4：按单个 key 精确删除——`clear()`（整库清空,给缓存开关关闭用）和淘汰循环
   * （按 `cachedAt` 淘汰最旧的,不针对特定 key）都不是这里要的语义。调用方
   * （`app/AppRuntime.tsx::loadFullTextViaCacheOrFetch`）在投影因 revision 不符拒绝一条缓存命中
   * 时用它清掉这条已确定过期的孤儿条目,不留着占位等 LRU 慢慢淘汰。单事务读+删+回写总量
   * （同 `put()`/合并后的 `get()` 既有的 read-modify-write 手法）——key 不存在时是 no-op,不抛错
   * （调用方总是"我以为这条存在,删就是了",不该因为它已经被淘汰过一次而报错）。
   */
  async delete(key: BodyCacheKey): Promise<void> {
    return this.enqueue(async () => {
      const db = await this.openDb();
      const k = bodyCacheKeyString(key);
      await new Promise<void>((resolve, reject) => {
        const tx = db.transaction([STORE_BODIES, STORE_META], "readwrite");
        const bodyStore = tx.objectStore(STORE_BODIES);
        const metaStore = tx.objectStore(STORE_META);
        const getExisting = bodyStore.get(k);
        getExisting.onsuccess = () => {
          const existing = getExisting.result as BodyRow | undefined;
          if (!existing) return; // 本就不存在——no-op。
          bodyStore.delete(k);
          const getTotal = metaStore.get(META_KEY_TOTAL_BYTES);
          getTotal.onsuccess = () => {
            const current = (getTotal.result as MetaRow | undefined)?.value ?? 0;
            metaStore.put({ key: META_KEY_TOTAL_BYTES, value: Math.max(0, current - existing.sizeBytes) });
          };
          getTotal.onerror = () => reject(getTotal.error ?? new Error("delete: read total failed"));
        };
        getExisting.onerror = () => reject(getExisting.error ?? new Error("delete: read existing failed"));
        tx.oncomplete = () => resolve();
        tx.onerror = () => reject(tx.error ?? new Error("delete: transaction failed"));
        tx.onabort = () => reject(tx.error ?? new Error("delete: transaction aborted"));
      });
    });
  }

  /** 淘汰循环——每轮一个独立事务：读总量 → 超限则按 `cachedAt` 升序删最旧一条 → 回写总量；
   *  仍在 `put()` 外层的 `enqueue()` 包裹范围内执行（`put()` 内部 `await` 了这个方法），不会跟另一个
   *  并发发起的 `get()`/`put()` 交错。淘汰只删 `bodies` 表的行（正文），不动任何游标/索引类数据——
   *  body cache 本就是唯一持有这份数据的地方，没有别的表需要同步。 */
  private async evictIfNeeded(db: IDBDatabase): Promise<void> {
    for (;;) {
      const totalBytes = await new Promise<number>((resolve, reject) => {
        const tx = db.transaction(STORE_META, "readonly");
        const req = tx.objectStore(STORE_META).get(META_KEY_TOTAL_BYTES);
        req.onsuccess = () => resolve((req.result as MetaRow | undefined)?.value ?? 0);
        req.onerror = () => reject(req.error ?? new Error("evict: read total failed"));
      });
      if (totalBytes <= this.capBytes) return;

      const evictedSize = await new Promise<number | null>((resolve, reject) => {
        const tx = db.transaction([STORE_BODIES, STORE_META], "readwrite");
        const bodyStore = tx.objectStore(STORE_BODIES);
        const metaStore = tx.objectStore(STORE_META);
        let result: number | null = null;
        const cursorReq = bodyStore.index(INDEX_CACHED_AT).openCursor();
        cursorReq.onsuccess = () => {
          const cursor = cursorReq.result;
          if (!cursor) {
            result = null; // 表已空——纵深防御，理论上总量归零后上面的循环已经退出。
            return;
          }
          const row = cursor.value as BodyRow;
          bodyStore.delete(row.key);
          const getTotal = metaStore.get(META_KEY_TOTAL_BYTES);
          getTotal.onsuccess = () => {
            const current = (getTotal.result as MetaRow | undefined)?.value ?? 0;
            metaStore.put({ key: META_KEY_TOTAL_BYTES, value: Math.max(0, current - row.sizeBytes) });
            result = row.sizeBytes;
          };
        };
        cursorReq.onerror = () => reject(cursorReq.error ?? new Error("evict: cursor failed"));
        tx.oncomplete = () => resolve(result);
        tx.onerror = () => reject(tx.error ?? new Error("evict: transaction failed"));
        tx.onabort = () => reject(tx.error ?? new Error("evict: transaction aborted"));
      });
      if (evictedSize === null) return;
    }
  }

  async clear(): Promise<void> {
    return this.enqueue(async () => {
      const db = await this.openDb();
      await new Promise<void>((resolve, reject) => {
        const tx = db.transaction([STORE_BODIES, STORE_META], "readwrite");
        tx.objectStore(STORE_BODIES).clear();
        tx.objectStore(STORE_META).clear();
        tx.oncomplete = () => resolve();
        tx.onerror = () => reject(tx.error ?? new Error("clear failed"));
        tx.onabort = () => reject(tx.error ?? new Error("clear aborted"));
      });
    });
  }
}
