import { describe, expect, it } from "vitest";
import fixture from "../../src-tauri/src/fixtures/updater-state.json";
import {
  isUpdaterSnapshot,
  isUpdaterState,
  UPDATER_STATE_FIELDS,
  UPDATER_STATE_KINDS,
  type UpdaterState,
  type UpdaterStateKind,
} from "./updater";

// 真路径消费方：这份 JSON 同时被 Rust 侧 `updater.rs` 的
// `include_str!("fixtures/updater-state.json")` 单测读取（校验 serde 反序列
// 化）——本文件是前端那一半消费方，两边同源、同一份文件、不漂移（「契约样张
// 必须有真路径消费方」）。fixture 形状 = `{ snapshots: UpdaterSnapshot[] }`，
// 每种 `kind` 各一条（`disabled` 的三个 `reason` 各占一条）。
const fixtureSnapshots = (fixture as { snapshots: unknown[] }).snapshots;

describe("UpdaterState / UpdaterSnapshot 类型守卫（对拍 Rust wire fixture）", () => {
  it("fixture 是非空数组", () => {
    expect(Array.isArray(fixtureSnapshots)).toBe(true);
    expect(fixtureSnapshots.length).toBeGreaterThan(0);
  });

  it("逐条断言：fixture 里每条快照都能被 isUpdaterSnapshot 接受", () => {
    for (const snap of fixtureSnapshots) {
      expect(isUpdaterSnapshot(snap)).toBe(true);
    }
  });

  it("fixture 里出现的 kind 全集 == UpdaterState 联合全部成员（两边都漏不得）", () => {
    const kindsInFixture = new Set(
      fixtureSnapshots.map(
        (snap) => (snap as { state: { kind: string } }).state.kind,
      ),
    );
    expect([...kindsInFixture].sort()).toEqual([...UPDATER_STATE_KINDS].sort());
  });

  it("ready.last_error（U4 返工·换包失败可重试）：fixture 那条带 last_error 的 ready 样张能被接受且字段读得到", () => {
    const readySnapshots = fixtureSnapshots.filter(
      (snap) => (snap as { state: { kind: string } }).state.kind === "ready",
    ) as { state: { kind: "ready"; last_error?: string } }[];
    const withError = readySnapshots.find((snap) => "last_error" in snap.state);
    expect(withError).toBeDefined();
    expect(isUpdaterSnapshot(withError)).toBe(true);
    expect(typeof withError!.state.last_error).toBe("string");
    expect(withError!.state.last_error).toContain("AL_ERR:updater.swap_failed");

    // fixture 里也留着一条不带 last_error 的正常 ready——两条都必须过守卫，
    // 可选字段缺失不能被误判成非法。
    const withoutError = readySnapshots.find(
      (snap) => !("last_error" in snap.state),
    );
    expect(withoutError).toBeDefined();
    expect(isUpdaterSnapshot(withoutError)).toBe(true);
  });

  it("ready.last_error 类型错误时守卫拒收（不是随便什么值都放行）", () => {
    expect(
      isUpdaterState({
        kind: "ready",
        version: "0.2.9",
        staged_path: "/tmp/x.app",
        last_error: 123,
      }),
    ).toBe(false);
  });

  it("recovery_offered.last_error（P2 契约断链修复）：fixture 那条带 last_error 的 recovery_offered 样张能被接受且字段读得到", () => {
    const recoverySnapshots = fixtureSnapshots.filter(
      (snap) =>
        (snap as { state: { kind: string } }).state.kind === "recovery_offered",
    ) as { state: { kind: "recovery_offered"; last_error?: string } }[];
    const withError = recoverySnapshots.find(
      (snap) => "last_error" in snap.state,
    );
    expect(withError).toBeDefined();
    expect(isUpdaterSnapshot(withError)).toBe(true);
    expect(typeof withError!.state.last_error).toBe("string");
    expect(withError!.state.last_error).toContain("AL_ERR:updater.swap_failed");

    // fixture 里也留着一条不带 last_error 的 recovery_offered——两条都必须
    // 过守卫，可选字段缺失不能被误判成非法。
    const withoutError = recoverySnapshots.find(
      (snap) => !("last_error" in snap.state),
    );
    expect(withoutError).toBeDefined();
    expect(isUpdaterSnapshot(withoutError)).toBe(true);
  });

  it("recovery_offered.last_error 类型错误时守卫拒收（不是随便什么值都放行）", () => {
    expect(
      isUpdaterState({
        kind: "recovery_offered",
        bundle_path: "/Applications/AgentLoom.app",
        staged_path: "/Applications/.agentloom-update-abc123/AgentLoom.app",
        target_version: "0.2.9",
        last_error: 123,
      }),
    ).toBe(false);
  });

  it("error.retry wire：fixture 同时保留缺省样张并接受 reopen 样张", () => {
    const errorSnapshots = fixtureSnapshots.filter(
      (snap) => (snap as { state: { kind: string } }).state.kind === "error",
    ) as {
      state: {
        kind: "error";
        msg: string;
        checked_at: number;
        retry?: "check" | "reopen";
      };
    }[];

    const withoutRetry = errorSnapshots.find(
      (snap) => !("retry" in snap.state),
    );
    expect(withoutRetry).toBeDefined();
    expect(isUpdaterSnapshot(withoutRetry)).toBe(true);

    const reopen = errorSnapshots.find((snap) => snap.state.retry === "reopen");
    expect(reopen).toBeDefined();
    expect(isUpdaterSnapshot(reopen)).toBe(true);
  });

  it("error.retry 类型错误时守卫拒收", () => {
    expect(
      isUpdaterState({
        kind: "error",
        msg: "AL_ERR:updater.check_failed",
        checked_at: 1724569200000,
        retry: "download",
      }),
    ).toBe(false);
  });

  it("字段级对拍（P2-4 契约锁）：fixture 每条快照的字段名全集都被 UPDATER_STATE_FIELDS 覆盖，一个不多一个不少", () => {
    for (const snap of fixtureSnapshots) {
      const state = (snap as { state: { kind: UpdaterStateKind } }).state;
      const allowed = UPDATER_STATE_FIELDS[state.kind];
      const actualFields = Object.keys(state);
      const unknown = actualFields.filter((f) => !allowed.includes(f));
      expect(
        unknown,
        `kind=${state.kind} 出现了 UPDATER_STATE_FIELDS 里没登记的字段：${JSON.stringify(unknown)}——TS 类型/字段清单没跟上 fixture`,
      ).toEqual([]);
    }
  });

  it("拒绝缺字段/错类型的伪快照（守卫真的在检查字段，不是纯粹认 kind 放行）", () => {
    expect(isUpdaterState({ kind: "available", version: 1 })).toBe(false);
    expect(isUpdaterState({ kind: "downloading" })).toBe(false);
    expect(isUpdaterState({ kind: "ready", version: "0.2.9" })).toBe(false);
    expect(isUpdaterState({ kind: "disabled", reason: "nope" })).toBe(false);
    expect(isUpdaterState({ kind: "not_a_real_kind" })).toBe(false);
    expect(isUpdaterState(null)).toBe(false);
    expect(isUpdaterSnapshot({ revision: "1", state: { kind: "idle" } })).toBe(
      false,
    );
    expect(isUpdaterSnapshot({ revision: 1 })).toBe(false);
    expect(isUpdaterSnapshot(null)).toBe(false);
  });

  it("补充：逐态手写构造样本同样通过守卫（覆盖 fixture 未来若漏态也能独立锁住）", () => {
    const samples: Record<UpdaterState["kind"], UpdaterState> = {
      disabled: { kind: "disabled", reason: "dev" },
      idle: { kind: "idle" },
      checking: { kind: "checking" },
      up_to_date: { kind: "up_to_date", checked_at: 1724569200000 },
      available: {
        kind: "available",
        version: "0.2.9",
        notes: "Bug fixes and stability improvements.",
        pub_date: "2026-08-20T00:00:00Z",
      },
      downloading: { kind: "downloading", downloaded: 1024, total: null },
      staging: { kind: "staging" },
      ready: {
        kind: "ready",
        version: "0.2.9",
        staged_path: "/tmp/staged/AgentLoom.app",
      },
      swapping: { kind: "swapping" },
      recovery_offered: {
        kind: "recovery_offered",
        bundle_path: "/Applications/AgentLoom.app",
        staged_path: "/Applications/.agentloom-update-abc123/AgentLoom.app",
        target_version: "0.2.9",
      },
      error: {
        kind: "error",
        msg: "AL_ERR:updater.check_failed",
        checked_at: 1724569200000,
      },
    };
    expect(Object.keys(samples).sort()).toEqual(
      [...UPDATER_STATE_KINDS].sort(),
    );
    for (const kind of UPDATER_STATE_KINDS) {
      expect(isUpdaterState(samples[kind])).toBe(true);
    }
  });
});
