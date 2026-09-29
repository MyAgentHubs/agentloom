// commandLedger.indexeddb.ts — T6f3 · CommandLedgerPort 的 IndexedDB 实现（生产用）。
//
// 同 `store/indexeddbEventStore.ts`/`store/key-store.indexeddb.ts` 的既有落储手法：单个
// object store（keyPath `commandId`）、每次操作一个独立事务、`tx.oncomplete` 才算数（不是各请求
// 各自的 `onsuccess`）。按房间派生库名（同 `indexeddbEventStore.ts::deriveEventStoreDbName` 的既有
// 惯例——换房不串库：新配对到别的房间后，旧房间发出过的 command_id 不该继续被当"本机命令"识别，
// 虽然 command_id 本身是全局唯一的 UUID、跨房间也不会真的撞号，但语义上"这是不是我在**当前房间**
// 发出的指令"这条边界值得保持——按房间隔离是免费的，不隔离也谈不上有实际收益）。

import type { CommandLedgerPort, CommandLedgerRecord, CommandLedgerStatus, RecordSentInput } from "./commandLedger.ts";

const DEFAULT_DB_NAME = "agentloom-remote-commands";
const DB_VERSION = 1;
const STORE_COMMANDS = "commands";

/** 按房间派生账本库名——见文件头注"按房间隔离"一节。 */
export function deriveCommandLedgerDbName(room: string): string {
  return `${DEFAULT_DB_NAME}-${room}`;
}

function requireIndexedDB(factory?: IDBFactory): IDBFactory {
  const resolved = factory ?? globalThis.indexedDB;
  if (!resolved) {
    throw new Error("IndexedDB is unavailable in this runtime");
  }
  return resolved;
}

export class IndexedDbCommandLedger implements CommandLedgerPort {
  private readonly dbName: string;
  private readonly idbFactory: IDBFactory;
  private dbPromise: Promise<IDBDatabase> | null = null;
  /** 已解析的连接句柄——msgfix2 U4（收拢 P1-3）：同 `store/indexeddbEventStore.ts::dbHandle`
   *  头注，`close()` 必须同步生效。 */
  private dbHandle: IDBDatabase | null = null;

  constructor(dbName: string = DEFAULT_DB_NAME, idbFactory?: IDBFactory) {
    this.dbName = dbName;
    this.idbFactory = requireIndexedDB(idbFactory);
  }

  private openDb(): Promise<IDBDatabase> {
    if (this.dbPromise) return this.dbPromise;
    this.dbPromise = new Promise((resolve, reject) => {
      const request = this.idbFactory.open(this.dbName, DB_VERSION);
      request.onupgradeneeded = () => {
        const db = request.result;
        if (!db.objectStoreNames.contains(STORE_COMMANDS)) {
          db.createObjectStore(STORE_COMMANDS, { keyPath: "commandId" });
        }
      };
      request.onsuccess = () => {
        const db = request.result;
        // 收拢 P1-3：见 `indexeddbEventStore.ts::openDb` 同款注释，同一条不变量。
        db.onversionchange = () => {
          db.close();
          if (this.dbHandle === db) this.dbHandle = null;
          // msgfix2 F2 S3：`dbPromise` 也要一并清空——同 `indexeddbEventStore.ts` 同名注释，不清
          // 的话下一次 `openDb()` 会直接复用这个已 close 的 stale 连接，恒抛 InvalidStateError。
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

  /** 见字段头注——同步关闭已建立的连接；no-op 若从未建立过连接。 */
  close(): void {
    if (this.dbHandle) {
      this.dbHandle.close();
      this.dbHandle = null;
    }
    this.dbPromise = null;
  }

  async recordSent(input: RecordSentInput): Promise<void> {
    const db = await this.openDb();
    const record: CommandLedgerRecord = { ...input, status: "sent" };
    await new Promise<void>((resolve, reject) => {
      const tx = db.transaction(STORE_COMMANDS, "readwrite");
      tx.objectStore(STORE_COMMANDS).put(record);
      tx.oncomplete = () => resolve();
      tx.onerror = () => reject(tx.error ?? new Error("recordSent: transaction failed"));
      tx.onabort = () => reject(tx.error ?? new Error("recordSent: transaction aborted"));
    });
  }

  async isOwn(commandId: string): Promise<boolean> {
    return (await this.get(commandId)) !== null;
  }

  async updateStatus(commandId: string, status: CommandLedgerStatus): Promise<void> {
    const db = await this.openDb();
    await new Promise<void>((resolve, reject) => {
      const tx = db.transaction(STORE_COMMANDS, "readwrite");
      const store = tx.objectStore(STORE_COMMANDS);
      const getRequest = store.get(commandId);
      getRequest.onsuccess = () => {
        const existing = getRequest.result as CommandLedgerRecord | undefined;
        // 不是本机记过账的 command_id——静默不做任何事（纵深防御，见 port.ts 接口注释）。
        if (existing) {
          store.put({ ...existing, status });
        }
      };
      getRequest.onerror = () => reject(getRequest.error ?? new Error("updateStatus: get failed"));
      tx.oncomplete = () => resolve();
      tx.onerror = () => reject(tx.error ?? new Error("updateStatus: transaction failed"));
      tx.onabort = () => reject(tx.error ?? new Error("updateStatus: transaction aborted"));
    });
  }

  async get(commandId: string): Promise<CommandLedgerRecord | null> {
    const db = await this.openDb();
    return new Promise((resolve, reject) => {
      const tx = db.transaction(STORE_COMMANDS, "readonly");
      const req = tx.objectStore(STORE_COMMANDS).get(commandId);
      req.onsuccess = () => resolve((req.result as CommandLedgerRecord | undefined) ?? null);
      req.onerror = () => reject(req.error ?? new Error("get failed"));
    });
  }
}
