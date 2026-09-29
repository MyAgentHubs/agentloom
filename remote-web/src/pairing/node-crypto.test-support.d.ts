// node-crypto.test-support.d.ts — 极简 ambient 类型声明，只覆盖 pairing-session.test.ts 用到的
// `node:crypto` 两个函数（`hkdfSync`/`randomUUID`），供本单假桌面参考实现走一条独立于
// `src/crypto/kdf.ts`（WebCrypto subtle HKDF）的 HKDF 路径。刻意不装 `@types/node`（任务书 §2
// 硬约束：依赖清单只许 `fake-indexeddb` devDep）——跟 `src/test-support/node-builtins.d.ts`
// 同款做法，只是落在本单 SCOPE（`src/pairing/**`）内，不去改那个已有文件。

declare module "node:crypto" {
  export function hkdfSync(digest: string, ikm: Uint8Array, salt: Uint8Array, info: Uint8Array, keylen: number): ArrayBuffer;
  export function randomUUID(): string;
}
