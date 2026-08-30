// T4：`updater.rs`（`app/src-tauri/src/updater.rs`）状态机的前端镜像类型 +
// 运行时类型守卫。与 Rust `UpdaterState`/`UpdaterSnapshot` 严格同构：字段名/
// 可选性逐一对齐 serde 内部标签枚举（`#[serde(tag = "kind", rename_all =
// "snake_case")]`）产出的 wire JSON —— 故意不做驼峰化，直接消费后端原样形状。
// 真值来源见 `../../src-tauri/src/fixtures/updater-state.json`（Rust
// `include_str!` 同一份 JSON，`updater.test.ts` 对拍：契约样张必须有真路径消费
// 方，见内部笔记同名教训）。

export type DisabledReason = "dev" | "platform" | "unsigned";

export type UpdaterState =
  | { kind: "disabled"; reason: DisabledReason }
  | { kind: "idle" }
  | { kind: "checking" }
  | { kind: "up_to_date"; checked_at: number }
  | {
      kind: "available";
      version: string;
      notes: string | null;
      pub_date: string | null;
    }
  | { kind: "downloading"; downloaded: number; total: number | null }
  | { kind: "staging" }
  | {
      kind: "ready";
      version: string;
      staged_path: string;
      /** U4 返工：换包（RENAME_SWAP）失败时退回 Ready 带上的可读原因——
       * 后端 `#[serde(default, skip_serializing_if = "Option::is_none")]`，
       * 正常路径不带这个字段。存在时是 `al_err` 信封，走既有解析器渲染。 */
      last_error?: string;
    }
  | { kind: "swapping" }
  | {
      kind: "recovery_offered";
      bundle_path: string;
      staged_path: string;
      target_version: string;
      /** P2 契约断链修复：与 `ready.last_error` 同款——启动期恢复后若用户
       * 又点了一次「一键换回」失败，带上可读原因。Rust 侧
       * `#[serde(default, skip_serializing_if = "Option::is_none")]`，正常
       * 路径不带这个字段；存在时是 `al_err` 信封，走既有解析器渲染。 */
      last_error?: string;
    }
  | {
      kind: "error";
      msg: string;
      checked_at: number;
      retry?: "check" | "reopen";
    };

export type UpdaterStateKind = UpdaterState["kind"];

/** 联合全部成员的 `kind` 集合——供测试逐条构造样本核对覆盖完整。 */
export const UPDATER_STATE_KINDS: readonly UpdaterStateKind[] = [
  "disabled",
  "idle",
  "checking",
  "up_to_date",
  "available",
  "downloading",
  "staging",
  "ready",
  "swapping",
  "recovery_offered",
  "error",
];

/**
 * 每个 kind 允许出现的 wire 字段名全集（含 `kind` 自身 + 全部可选字段）——
 * 供 `updater.test.ts` 逐条对拍 fixture 快照的 `Object.keys(state)`，字段级
 * 锁契约：以后 Rust 侧给某个 kind 加字段、TS 这里没跟上，fixture 一有新字段
 * 出现就立刻红，不必再靠人肉审出（P2-4）。
 */
export const UPDATER_STATE_FIELDS: Readonly<
  Record<UpdaterStateKind, readonly string[]>
> = {
  disabled: ["kind", "reason"],
  idle: ["kind"],
  checking: ["kind"],
  up_to_date: ["kind", "checked_at"],
  available: ["kind", "version", "notes", "pub_date"],
  downloading: ["kind", "downloaded", "total"],
  staging: ["kind"],
  ready: ["kind", "version", "staged_path", "last_error"],
  swapping: ["kind"],
  recovery_offered: [
    "kind",
    "bundle_path",
    "staged_path",
    "target_version",
    "last_error",
  ],
  error: ["kind", "msg", "checked_at", "retry"],
};

/** 外层信封：`revision` 每次迁移单调 +1，见 `updaterStore.ts` 的防丢/防倒退。 */
export type UpdaterSnapshot = {
  revision: number;
  state: UpdaterState;
};

function isDisabledReason(v: unknown): v is DisabledReason {
  return v === "dev" || v === "platform" || v === "unsigned";
}

function isNullableString(v: unknown): v is string | null {
  return v === null || typeof v === "string";
}

export function isUpdaterState(v: unknown): v is UpdaterState {
  if (typeof v !== "object" || v === null) return false;
  const obj = v as Record<string, unknown>;
  switch (obj.kind) {
    case "disabled":
      return isDisabledReason(obj.reason);
    case "idle":
    case "checking":
    case "staging":
    case "swapping":
      return true;
    case "up_to_date":
      return typeof obj.checked_at === "number";
    case "available":
      return (
        typeof obj.version === "string" &&
        isNullableString(obj.notes) &&
        isNullableString(obj.pub_date)
      );
    case "downloading":
      return (
        typeof obj.downloaded === "number" &&
        (obj.total === null || typeof obj.total === "number")
      );
    case "ready":
      return (
        typeof obj.version === "string" &&
        typeof obj.staged_path === "string" &&
        (obj.last_error === undefined || typeof obj.last_error === "string")
      );
    case "recovery_offered":
      return (
        typeof obj.bundle_path === "string" &&
        typeof obj.staged_path === "string" &&
        typeof obj.target_version === "string" &&
        (obj.last_error === undefined || typeof obj.last_error === "string")
      );
    case "error":
      return (
        typeof obj.msg === "string" &&
        typeof obj.checked_at === "number" &&
        (obj.retry === undefined ||
          obj.retry === "check" ||
          obj.retry === "reopen")
      );
    default:
      return false;
  }
}

export function isUpdaterSnapshot(v: unknown): v is UpdaterSnapshot {
  if (typeof v !== "object" || v === null) return false;
  const obj = v as Record<string, unknown>;
  return typeof obj.revision === "number" && isUpdaterState(obj.state);
}
