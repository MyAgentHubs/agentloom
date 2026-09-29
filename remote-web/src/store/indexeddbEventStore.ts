// indexeddbEventStore.ts — `EventStorePort` 的 IndexedDB 实现。
//
// 权威参照（只读对照，只借它确认"relay 的 seq 从 1 起、0 = 没有"这个假设站得住）：
// `remote-relay/src/room-store.js` `headSeq`/`allocateSeq`——`seq_counter` 初值 0，
// `allocateSeq` 先 `+1` 再写回并返回，故房间内第一条里程碑的 seq 恒为 1。跟 M0 §3
// "through_run_seq 生产序号器先自增后返回、首条事件即 seq=1"是同一个模式，两处独立成立、
// 不是同一个计数器。
//
// **同一事务是硬要求，不是"尽量"**：`applyEventIfNew` 的"查重复 / 按需写回执 / 按需写事件本体 /
// 按需推进水位"四步全部在一次 `db.transaction([...], "readwrite")` 里完成——IndexedDB 事务对它
// 打开的全部 object store 是原子的（要么 `oncomplete`、要么整体回滚），所以不可能出现"记录了
// client_msg_id 但事件本体没落"或"水位推进了但本体没落"这类半成品状态；at-least-once 重投撞见
// 的至多是"整个操作要重新做一次"，不会是"数据处在几步之间的中间态"。**事件本体必须跟回执/水位
// 同一事务**（审查返工·2026-08 校准）：早前版本只落回执 + 水位、事件本体从不持久化——重载后
// 回执+水位组合会让同一条帧永久拒绝重投（"我见过它"），但内容其实从未真正存过，等于永久丢失、
// `applied:true` 这个回执在撒谎。测试用 `fake-indexeddb`（brief 允许的唯一新依赖）在 node 环境
// 驱动真实的 IndexedDB 事务语义，含"强制 abort 后三者均未落"的回滚测试 + "重载后从日志重建
// 内容"的往返测试；真浏览器下的持久化边界（配额/onblocked/版本升级并发）留给 T6g2，这里只保证
// "逻辑同一事务"这条不变量。
//
// 用全局 `indexedDB`（真浏览器 API），不做工厂依赖注入——跟 `crypto/envelope.ts` 用全局
// `crypto`/`crypto.subtle` 是同一种写法（`requireIndexedDB()` 对应它的 `requireCrypto()`）。

import type { ApplyEventInput, ApplyEventResult, EventStorePort, StoredEvent } from "./port.ts";

const DEFAULT_DB_NAME = "agentloom-remote-events";
const DB_VERSION = 1;
const STORE_APPLIED = "appliedClientMsgIds";
const STORE_META = "meta";
const STORE_EVENTS = "events";
const META_KEY_WATERMARK = "watermark";

/**
 * 按房间派生事件库名（INT1c 审查返工·P0）——`main.tsx`/`app/RootRouter.tsx` 用它构造
 * `IndexedDbEventStore`，不再跨重配对复用同一个默认库名（换房后旧房间的里程碑绝不能借着共用库名
 * 混进新房间的归约状态）。房间 id 是 M0 §1 钉死的 32 位小写 hex，天然是安全的字符串片段。
 */
export function deriveEventStoreDbName(room: string): string {
  return `${DEFAULT_DB_NAME}-${room}`;
}

interface AppliedRow {
  clientMsgId: string;
  seq: number;
}

interface MetaRow {
  key: string;
  value: number;
}

interface EventRow {
  clientMsgId: string;
  seq: number;
  /** 可选——本单（INT1c）之前写入的存量行没有这个键，见 `port.ts::StoredEvent.session` 注释。 */
  session?: string | null;
  frame: unknown;
}

function requireIndexedDB(): IDBFactory {
  if (!globalThis.indexedDB) {
    throw new Error("IndexedDB is unavailable in this runtime");
  }
  return globalThis.indexedDB;
}

export class IndexedDbEventStore implements EventStorePort {
  private readonly dbName: string;
  private dbPromise: Promise<IDBDatabase> | null = null;
  /** 已解析的连接句柄——msgfix2 U4（收拢 P1-3）：`close()` 需要同步生效（调用方紧接着就要
   *  `deleteDatabase()`），不能靠"等 `dbPromise` resolve 之后再关"的异步时序。 */
  private dbHandle: IDBDatabase | null = null;

  constructor(dbName: string = DEFAULT_DB_NAME) {
    this.dbName = dbName;
  }

  private openDb(): Promise<IDBDatabase> {
    if (this.dbPromise) return this.dbPromise;
    this.dbPromise = new Promise((resolve, reject) => {
      const request = requireIndexedDB().open(this.dbName, DB_VERSION);
      request.onupgradeneeded = () => {
        const db = request.result;
        if (!db.objectStoreNames.contains(STORE_APPLIED)) {
          db.createObjectStore(STORE_APPLIED, { keyPath: "clientMsgId" });
        }
        if (!db.objectStoreNames.contains(STORE_META)) {
          db.createObjectStore(STORE_META, { keyPath: "key" });
        }
        if (!db.objectStoreNames.contains(STORE_EVENTS)) {
          db.createObjectStore(STORE_EVENTS, { keyPath: "clientMsgId" });
        }
      };
      request.onsuccess = () => {
        const db = request.result;
        // 收拢 P1-3：另一个上下文（另一个标签页/另一次 open 的版本升级，含 `deleteDatabase()`
        // 触发的 versionchange）请求变更时主动放手——不然那边的请求会卡在 `onblocked` 一直悬挂
        // 到本连接关闭为止（跨标签页阻塞面，覆盖"另一个标签页也开着同一个库"的场景）。
        db.onversionchange = () => {
          db.close();
          if (this.dbHandle === db) this.dbHandle = null;
          // msgfix2 F2 S3：`dbPromise` 也要一并清空——不清的话它还缓存着这个已经 resolve 过、
          // 但底层连接已经 close 的 stale `IDBDatabase`,下一次 `openDb()`（`if (this.dbPromise)
          // return this.dbPromise`）会直接把这个死连接原样交出去,对已关闭连接开事务恒抛
          // `InvalidStateError`——下一次操作理应重新 `openDb()` 建一条新连接。
          this.dbPromise = null;
        };
        this.dbHandle = db;
        resolve(db);
      };
      request.onerror = () => reject(request.error ?? new Error("IndexedDB open failed"));
    });
    return this.dbPromise;
  }

  /** 见字段头注——同步关闭已建立的连接，供 `store/cacheManager.ts::purgeRoomData()` 在
   *  `deleteDatabase()` 之前调用。从未建立过连接时是 no-op。关闭后下一次操作会重新 `openDb()`。 */
  close(): void {
    if (this.dbHandle) {
      this.dbHandle.close();
      this.dbHandle = null;
    }
    this.dbPromise = null;
  }

  /**
   * 四步（查重复 / 按需写 appliedClientMsgIds 回执 / 按需写 events 本体 / 按需推进 watermark）
   * 全部挂在同一个 `IDBTransaction` 上——`tx.objectStore(...)` 只是"拿这次事务里已经开好的那个
   * store 句柄"，不会另起事务。真正提交结果的时机是 `tx.oncomplete`，不是各请求各自的
   * `onsuccess`（那些只保证"这一步的读/写已排进这个事务"，事务本身仍可能在提交前失败/被中止）。
   */
  async applyEventIfNew(input: ApplyEventInput): Promise<ApplyEventResult> {
    const db = await this.openDb();
    return new Promise((resolve, reject) => {
      const tx = db.transaction([STORE_APPLIED, STORE_META, STORE_EVENTS], "readwrite");
      const appliedStore = tx.objectStore(STORE_APPLIED);
      const metaStore = tx.objectStore(STORE_META);
      const eventsStore = tx.objectStore(STORE_EVENTS);
      let result: ApplyEventResult | null = null;

      const getAppliedReq = appliedStore.get(input.clientMsgId);
      getAppliedReq.onsuccess = () => {
        const alreadyApplied = getAppliedReq.result !== undefined;

        const getWatermarkReq = metaStore.get(META_KEY_WATERMARK);
        getWatermarkReq.onsuccess = () => {
          const watermarkRow = getWatermarkReq.result as MetaRow | undefined;
          const currentWatermark = watermarkRow?.value ?? 0;
          // max()，不是"最后写入的那个值"——重投/乱序到达都不能让水位倒退或原地不动地丢失
          // 一次本该发生的推进。
          const nextWatermark = Math.max(currentWatermark, input.seq);

          if (!alreadyApplied) {
            const appliedRow: AppliedRow = { clientMsgId: input.clientMsgId, seq: input.seq };
            appliedStore.put(appliedRow);
            // 事件本体与回执同一事务落储——不是"回执说见过就够了"，重载重建靠的是这行数据。
            // `session` 随信封同一事务落盘（INT1c）——冷启动全量重放的归属真相源。
            const eventRow: EventRow = { clientMsgId: input.clientMsgId, seq: input.seq, session: input.session, frame: input.frame };
            eventsStore.put(eventRow);
          }
          if (nextWatermark !== currentWatermark) {
            const metaRow: MetaRow = { key: META_KEY_WATERMARK, value: nextWatermark };
            metaStore.put(metaRow);
          }
          result = { applied: !alreadyApplied, watermark: nextWatermark };
        };
      };

      tx.oncomplete = () => {
        if (result === null) {
          reject(new Error("applyEventIfNew: transaction completed without producing a result"));
          return;
        }
        resolve(result);
      };
      tx.onerror = () => reject(tx.error ?? new Error("applyEventIfNew: transaction failed"));
      tx.onabort = () => reject(tx.error ?? new Error("applyEventIfNew: transaction aborted"));
    });
  }

  async getWatermark(): Promise<number> {
    const db = await this.openDb();
    return new Promise((resolve, reject) => {
      const tx = db.transaction([STORE_META], "readonly");
      const req = tx.objectStore(STORE_META).get(META_KEY_WATERMARK);
      req.onsuccess = () => resolve((req.result as MetaRow | undefined)?.value ?? 0);
      req.onerror = () => reject(req.error ?? new Error("getWatermark failed"));
    });
  }

  async hasAppliedClientMsgId(clientMsgId: string): Promise<boolean> {
    const db = await this.openDb();
    return new Promise((resolve, reject) => {
      const tx = db.transaction([STORE_APPLIED], "readonly");
      const req = tx.objectStore(STORE_APPLIED).get(clientMsgId);
      req.onsuccess = () => resolve(req.result !== undefined);
      req.onerror = () => reject(req.error ?? new Error("hasAppliedClientMsgId failed"));
    });
  }

  async listEvents(): Promise<StoredEvent[]> {
    const db = await this.openDb();
    return new Promise((resolve, reject) => {
      const tx = db.transaction([STORE_EVENTS], "readonly");
      const req = tx.objectStore(STORE_EVENTS).getAll();
      req.onsuccess = () => {
        const rows = (req.result as EventRow[]).slice();
        rows.sort((a, b) => a.seq - b.seq);
        // `row.session` 对本单之前写入的存量行天然是 `undefined`（该键当年从未被序列化过）——不用
        // `?? null` 在这里坍缩掉，让调用方（`app/AppRuntime.tsx` 冷启动重放）自己按
        // `StoredEvent.session` 的三态语义决定"降级只重建 session.index"还是"正常按会话路由"。
        resolve(rows.map((row) => ({ clientMsgId: row.clientMsgId, seq: row.seq, session: row.session, frame: row.frame })));
      };
      req.onerror = () => reject(req.error ?? new Error("listEvents failed"));
    });
  }
}
