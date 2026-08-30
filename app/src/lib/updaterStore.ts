// T4：updater 状态外部 store（同 `chatVerbosity.ts` 姿势——模块级单例 +
// `useSyncExternalStore`，不用 React Context）。
//
// 前端防丢/防倒退（设计 §2D「前端防丢/防倒退」）：`start()` 先
// `listen("updater://state")` 建立订阅、订阅成功后才 `invoke
// ("updater_get_state")` 取一次快照——顺序不可换，否则「拉快照那一刻」与
// 「事件到达那一刻」之间有窗口，窗口内的迁移会被漏掉。无论来源（事件推送 or
// invoke 响应）一律只接受 `revision` 严格更大的快照，防止旧 `invoke` 响应晚于
// 新事件到达时状态倒退。
//
// U9 修复：内部用 `snapshot: UpdaterSnapshot | null`（初始 `null`，代表「尚
// 无快照」）做哨兵，而不是拿 `revision: 0` 的默认对象顶替——后端首个快照的
// `revision` 也是 0（`Disabled`/`Idle` 初态），若用 `candidate.revision >
// snapshot.revision` 严格比较，这个首个快照会因为 `0 > 0` 为假被永久丢弃，
// dev 构建/占位公钥的 `Disabled{dev|unsigned}` 状态就再也上不了 UI。哨兵为
// `null` 时无条件接受第一个快照，之后才转入严格更大比较。

import { useEffect, useSyncExternalStore } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { isUpdaterSnapshot, type UpdaterSnapshot } from "../types/updater";

const EVENT_NAME = "updater://state";

/**
 * R2 P2：健康握手不再在挂载即刻调——延后这么久再调，给「挂载之后才炸」的
 * 失败（DB 迁移 / 引擎 spawn 这类首帧渲染完才会暴露的问题）留出暴露窗口。
 * 在这段时间内 app 若崩掉，旧版备份还在（后端只在收到握手后才敢删），用户
 * 能一键换回；一旦握手发出，备份就没了，之后再崩就只能靠 `recovery_offered`
 * 的启动期判定兜底。
 */
const HEALTH_HANDSHAKE_DELAY_MS = 20_000;

const INITIAL_SNAPSHOT: UpdaterSnapshot = {
  revision: 0,
  state: { kind: "idle" },
};

let snapshot: UpdaterSnapshot | null = null;
let dismissedForRun = false;
let started = false;
let startPromise: Promise<void> | null = null;
let markedHealthy = false;
const listeners = new Set<() => void>();

function notify(): void {
  for (const listener of listeners) listener();
}

/**
 * 尚无快照（`snapshot === null`）时无条件接受第一个快照——哨兵，见上方模块
 * 注释。此后才要求 `revision` 严格更大，旧响应/旧事件原样丢弃，不倒退状态。
 */
function applyIfNewer(candidate: unknown): void {
  if (!isUpdaterSnapshot(candidate)) return;
  if (snapshot !== null && candidate.revision <= snapshot.revision) return;
  snapshot = candidate;
  notify();
}

/**
 * P1 健康握手（配合 Rust 侧 U8）：告诉后端「界面已经能用了」，只有收到这个
 * 信号后端才敢删掉唯一的旧版备份——窗口 `is_visible()` 不可靠（`lib.rs`
 * 有白屏兜底，前端 3 秒没起来也会强制显示窗口，窗口可见不等于新版活着）。
 * 用 `markedHealthy` 去重，保证进程生命周期内最多真正 `invoke` 一次；命令在
 * 旧后端上不存在也不能让 app 崩——失败静默吞掉，不影响其它逻辑。
 */
function markHealthyOnce(): void {
  if (markedHealthy) return;
  markedHealthy = true;
  void (async () => {
    try {
      await invoke("updater_mark_healthy");
    } catch {
      // 旧后端没有这个命令，或握手本身失败——静默降级，不影响 UI。
    }
  })();
}

/**
 * R2 P2：健康握手调度——挂载后延迟 `HEALTH_HANDSHAKE_DELAY_MS`（20s）才真正
 * 握手一次，不再是挂载即刻调。返回的取消函数供调用方（`useUpdaterSnapshot`
 * 的 `useEffect` 清理）在组件卸载时 `clearTimeout`——若卸载发生在 20s 触发
 * 之前，这次挂载排的定时器直接作废，不会再补发；`markHealthyOnce()` 自身的
 * `markedHealthy` 去重则保证即使多个消费者（`UpdateButton` / `UpdateSection`
 * 各自的 `useUpdaterSnapshot()`）各排了一份定时器，全程也只会真正 `invoke`
 * 一次。
 */
export function scheduleMarkHealthy(): () => void {
  const timer = setTimeout(() => {
    markHealthyOnce();
  }, HEALTH_HANDSHAKE_DELAY_MS);
  return () => clearTimeout(timer);
}

export function getUpdaterSnapshot(): UpdaterSnapshot {
  return snapshot ?? INITIAL_SNAPSHOT;
}

export function subscribeUpdater(listener: () => void): () => void {
  listeners.add(listener);
  return () => {
    listeners.delete(listener);
  };
}

export function isUpdaterDismissedForRun(): boolean {
  return dismissedForRun;
}

/** 「稍后」：本次运行不再自动弹 popover（顶栏图标仍在，是否显示由状态决定）。 */
export function dismissUpdaterForRun(): void {
  if (dismissedForRun) return;
  dismissedForRun = true;
  notify();
}

/**
 * 挂载即拉：先订阅 `updater://state`、订阅成功后再拉一次当前快照。幂等——
 * 多个组件各自调用只会真正跑一次（`UpdateButton`/`UpdateSection` 都会调用）。
 * 不保留/调用 `listen()` 返回的 unlisten——本 store 是 app 生命周期内的单例，
 * 没有需要早于进程退出取消订阅的场景（同 `chatVerbosity` 单例不做卸载清理）。
 */
export function start(): Promise<void> {
  if (started) return startPromise ?? Promise.resolve();
  started = true;
  startPromise = (async () => {
    try {
      await listen<unknown>(EVENT_NAME, (event) => {
        applyIfNewer(event.payload);
      });
    } catch {
      // 事件订阅失败（如非 Tauri 运行时/测试环境）——保留默认快照，不阻塞下面的 invoke。
    }
    try {
      const initial = await invoke<unknown>("updater_get_state");
      applyIfNewer(initial);
    } catch {
      // 取初始快照失败——保留已有状态（可能已经被上面的事件更新过）。
    }
    // 首帧关键初始化到此为止——健康握手不在这里触发：R2 起改成挂载延迟 20s
    // 才握手（`scheduleMarkHealthy()`），由 `useUpdaterSnapshot()` 的
    // `useEffect` 负责排定时器，`start()` 自身不再关心握手时机。
  })();
  return startPromise;
}

export async function check(manual: boolean): Promise<void> {
  const result = await invoke<unknown>("updater_check", { manual });
  applyIfNewer(result);
}

export async function downloadAndInstall(): Promise<void> {
  const result = await invoke<unknown>("updater_download_and_install");
  applyIfNewer(result);
}

/**
 * `updater_relaunch` 是 `Result<(), String>`——Ok 时 resolve 到 `undefined`，
 * Err 时 reject 成 `AL_ERR:...` 字符串。不产生新快照（成功即交换+重启+
 * `app.exit(0)`，进程即将消失）；调用方负责 catch 并渲染可读错误。
 */
export async function relaunch(): Promise<void> {
  await invoke("updater_relaunch");
}

export async function reopen(): Promise<void> {
  await invoke("updater_reopen");
}

export async function skipVersion(version: string): Promise<void> {
  const result = await invoke<unknown>("updater_skip_version", { version });
  applyIfNewer(result);
}

/**
 * `updater_swap_back`（`recovery_offered` 态一键换回）：与 `relaunch()` 同款
 * `Result<(), String>` 形状——成功即反向交换 + LaunchServices 打开旧
 * `bundle_path` + `app.exit(0)`，不产生新快照；调用方负责 catch 并渲染可读
 * 错误。`app`/`handle` 两个参数是 Tauri 侧注入的（`AppHandle`/`State`），前端
 * 无参调用即可，与 `relaunch()` 完全对称。
 */
export async function swapBack(): Promise<void> {
  await invoke("updater_swap_back");
}

/**
 * `updater_discard_update`（Rust 侧 R2 新增·仅 `Ready` 可调，`Ready → Idle`）
 * ——「放弃此更新」逃生口：不用等交换失败、也不用重启，直接清掉暂存包退出
 * 已就绪这个钉死态。与 `relaunch()`/`swapBack()` 同款姿势——这里不手动应用
 * 返回值，迁移后的新快照走 `updater://state` 事件推回来（跟状态机其余迁移
 * 一致，见模块顶部「单一状态机真相」）；调用方负责 catch 并渲染可读错误。
 */
export async function discardUpdate(): Promise<void> {
  await invoke("updater_discard_update");
}

export function useUpdaterSnapshot(): UpdaterSnapshot {
  useEffect(() => {
    void start();
    // 健康握手延迟调度——组件卸载（含测试卸载）清掉这次挂载排的定时器，见
    // `scheduleMarkHealthy()` 注释。
    return scheduleMarkHealthy();
  }, []);
  return useSyncExternalStore(
    subscribeUpdater,
    getUpdaterSnapshot,
    () => INITIAL_SNAPSHOT,
  );
}

export function useUpdaterDismissedForRun(): boolean {
  return useSyncExternalStore(
    subscribeUpdater,
    isUpdaterDismissedForRun,
    () => false,
  );
}

/** 仅供测试使用：重置模块级单例状态，不触碰真实 Tauri 运行时。 */
export function __resetForTests(): void {
  snapshot = null;
  dismissedForRun = false;
  started = false;
  startPromise = null;
  markedHealthy = false;
  listeners.clear();
}
