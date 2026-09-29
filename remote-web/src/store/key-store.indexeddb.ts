// key-store.indexeddb.ts — T6b · KeyStorePort 的 IndexedDB 实现（生产用）。
//
// M0 §9.5 手机侧落盘契约："收 ready…一个 IndexedDB 事务落盘 {access, refresh} 后才许拿 access
// 重连"——`saveKeys` 用单个 readwrite 事务的单条 `put` 一次性落盘整条记录（access/refresh 与其余
// 字段绑在同一个对象里，天然满足"一个事务"）。K_room 按 M2 C1 spec §3.7 落成 non-extractable
// AES-256-GCM CryptoKey（`key-store.ts::importNonExtractableAesGcmKey`）——IndexedDB 的结构化克隆
// 算法原生支持 CryptoKey（W3C Web Crypto API §10 序列化步骤），浏览器（Chrome/Firefox/Safari）与
// 本单验证过的 Node `fake-indexeddb`（结构化克隆走 Node 内置 `structuredClone`）都完整支持存取一
// 个仍可正常 encrypt/decrypt 的 non-extractable CryptoKey，见 key-store.test.ts 的落地验证。

import type { KeyStorePort, PendingRefresh, StoredPairingCredentials } from "./key-store.ts";

const DB_NAME = "agentloom-remote-pairing";
const DB_VERSION = 1;
const STORE_NAME = "credentials";
/**
 * MVP 单房间单设备——一台手机同一时间只配对一个 AgentLoom 桌面房间（M2 C1 spec §4b「MVP 明确砍
 * 项目切换」同一取舍），记录用固定 key，不建索引。
 */
const RECORD_KEY = "current";

export class IndexedDbKeyStore implements KeyStorePort {
  constructor(
    private readonly idbFactory: IDBFactory = requireGlobalIndexedDb(),
    private readonly dbName: string = DB_NAME,
  ) {}

  async saveKeys(creds: StoredPairingCredentials): Promise<void> {
    const db = await this.openDb();
    try {
      await runTransaction(db, "readwrite", (store) => {
        store.put(creds, RECORD_KEY);
      });
    } finally {
      db.close();
    }
  }

  async loadKeys(): Promise<StoredPairingCredentials | null> {
    const db = await this.openDb();
    try {
      const result = await runTransactionWithResult<StoredPairingCredentials | undefined>(db, "readonly", (store) =>
        store.get(RECORD_KEY),
      );
      return result ?? null;
    } finally {
      db.close();
    }
  }

  async clear(): Promise<void> {
    const db = await this.openDb();
    try {
      await runTransaction(db, "readwrite", (store) => {
        store.delete(RECORD_KEY);
      });
    } finally {
      db.close();
    }
  }

  /**
   * T6c-refresh 新增：read-modify-write 在**同一个** readwrite 事务里完成——`get` 的 `onsuccess`
   * 回调里同步调用 `put`（IndexedDB 事务在有未完成请求时保持存活，这是标准的事务内读改写模式，不
   * 是"两个事务拼出来的伪原子"）。没有已存在的凭据记录时拒绝——pending_refresh 依附在凭据记录
   * 上，配对激活之前不可能有 refresh 请求。
   */
  async savePendingRefresh(pending: PendingRefresh): Promise<void> {
    const db = await this.openDb();
    try {
      await new Promise<void>((resolve, reject) => {
        const tx = db.transaction(STORE_NAME, "readwrite");
        const store = tx.objectStore(STORE_NAME);
        const getRequest = store.get(RECORD_KEY);
        getRequest.onsuccess = () => {
          const existing = getRequest.result as StoredPairingCredentials | undefined;
          if (!existing) {
            reject(new Error("savePendingRefresh() called before any credentials were saved"));
            return;
          }
          store.put({ ...existing, pendingRefresh: pending }, RECORD_KEY);
        };
        getRequest.onerror = () => reject(getRequest.error ?? new Error("IndexedDB get failed"));
        tx.oncomplete = () => resolve();
        tx.onerror = () => reject(tx.error ?? new Error("IndexedDB readwrite transaction failed"));
        tx.onabort = () => reject(tx.error ?? new Error("IndexedDB readwrite transaction aborted"));
      });
    } finally {
      db.close();
    }
  }

  async loadPendingRefresh(): Promise<PendingRefresh | null> {
    const creds = await this.loadKeys();
    return creds?.pendingRefresh ?? null;
  }

  private openDb(): Promise<IDBDatabase> {
    return new Promise((resolve, reject) => {
      const request = this.idbFactory.open(this.dbName, DB_VERSION);
      request.onupgradeneeded = () => {
        const db = request.result;
        if (!db.objectStoreNames.contains(STORE_NAME)) {
          db.createObjectStore(STORE_NAME);
        }
      };
      request.onsuccess = () => resolve(request.result);
      request.onerror = () => reject(request.error ?? new Error("IndexedDB open failed"));
      request.onblocked = () => reject(new Error("IndexedDB open blocked by another connection"));
    });
  }
}

function runTransaction(
  db: IDBDatabase,
  mode: IDBTransactionMode,
  work: (store: IDBObjectStore) => void,
): Promise<void> {
  return new Promise((resolve, reject) => {
    const tx = db.transaction(STORE_NAME, mode);
    work(tx.objectStore(STORE_NAME));
    tx.oncomplete = () => resolve();
    tx.onerror = () => reject(tx.error ?? new Error(`IndexedDB ${mode} transaction failed`));
    tx.onabort = () => reject(tx.error ?? new Error(`IndexedDB ${mode} transaction aborted`));
  });
}

function runTransactionWithResult<T>(
  db: IDBDatabase,
  mode: IDBTransactionMode,
  work: (store: IDBObjectStore) => IDBRequest<T>,
): Promise<T> {
  return new Promise((resolve, reject) => {
    const tx = db.transaction(STORE_NAME, mode);
    const request = work(tx.objectStore(STORE_NAME));
    tx.onerror = () => reject(tx.error ?? new Error(`IndexedDB ${mode} transaction failed`));
    tx.onabort = () => reject(tx.error ?? new Error(`IndexedDB ${mode} transaction aborted`));
    tx.oncomplete = () => resolve(request.result);
  });
}

function requireGlobalIndexedDb(): IDBFactory {
  const factory = (globalThis as { indexedDB?: IDBFactory }).indexedDB;
  if (!factory) {
    throw new Error("IndexedDB is unavailable in this runtime");
  }
  return factory;
}
