// webLocks.ts — T6c-refresh · Web Locks 多标签单飞行(M2 C1 spec §3 v0.5 块「多标签单飞行」段)。
//
// "多标签 = Web Locks API(navigator.locks·测试注入假实现·不装 polyfill——若环境无 locks 则单标签
// 直通并记录)、保证同 origin 多标签只有一个连接/一次 refresh 飞行"。
//
// 两个独立锁用途:
//   ① 连接领导权(`withConnectionLeadership`)——非阻塞探测(`ifAvailable:true`);拿不到锁的标签页
//      直接判定"这次不是我当连接持有者",不打开自己的 WebSocket(避免同源多标签各开一条连接)。
//   ② refresh 单飞行(`withRefreshSingleFlight`)——阻塞式独占锁,保证同一时刻同源只有一次
//      `token.refresh` 飞行中,不管发起飞行的是哪个标签页。
//
// 没有 `navigator.locks`(`locks` 形参为 `undefined`)时两者都退化成"直接跑 task、不做跨标签互斥"
// (单标签直通),并在返回值里标出 `viaFallback: true`,调用方(`connectionSession.ts`)据此过
// `redact()` 记一条日志——这正是"记录"这半句要求。

import type { LocksPort } from "./types.ts";

export interface LockRunResult<T> {
  result: T;
  /** true = 环境没有 `navigator.locks`,这次调用没有真正的跨标签互斥保证(单标签直通)。 */
  viaFallback: boolean;
}

export interface LeadershipResult<T> extends LockRunResult<T> {
  /** false = 探测到锁已被别的标签页持有,`task` 没有被执行,`result` 是 `undefined`。 */
  acquired: boolean;
}

/**
 * 非阻塞探测连接领导权——`task()` 只在拿到锁时执行,并持有锁直到 `task()` 的 promise 结算
 * (调用方应该把"维持这条连接直到它关闭"的 promise 传进来,不是一个立刻 resolve 的短任务——这意味
 * 着本函数返回的 promise 通常要等到整条连接生命周期结束才 resolve)。
 *
 * `onAcquired`(可选)在"是否拿到锁"这件事**刚确定的那一刻**同步触发(拿到锁→在调用 `task()`
 * 之前;没拿到→在 `null` 分支里),不等 `task()` 跑完——调用方(`ConnectionSession.start()`)需要
 * 这个更早的信号来判断"该不该转入 idle 态",而不能傻等整条生命周期结束。
 */
export async function withConnectionLeadership<T>(
  locks: LocksPort | undefined,
  lockName: string,
  task: () => Promise<T>,
  onAcquired?: (acquired: boolean, viaFallback: boolean) => void,
): Promise<LeadershipResult<T | undefined>> {
  if (!locks) {
    onAcquired?.(true, true);
    const result = await task();
    return { acquired: true, result, viaFallback: true };
  }
  let acquired = false;
  let result: T | undefined;
  await locks.request(lockName, { ifAvailable: true }, async (lock) => {
    if (lock === null) {
      acquired = false;
      onAcquired?.(false, false);
      return undefined;
    }
    acquired = true;
    onAcquired?.(true, false);
    result = await task();
    return result;
  });
  return { acquired, result, viaFallback: false };
}

/** 阻塞式独占锁——同名锁的并发调用天然排队,保证 `task` 全局(跨标签)单飞行。 */
export async function withRefreshSingleFlight<T>(
  locks: LocksPort | undefined,
  lockName: string,
  task: () => Promise<T>,
): Promise<LockRunResult<T>> {
  if (!locks) {
    return { result: await task(), viaFallback: true };
  }
  const result = await locks.request(lockName, { mode: "exclusive" }, async () => task());
  return { result, viaFallback: false };
}

export function connectLockName(room: string): string {
  return `agentloom-remote-connect:${room}`;
}

export function refreshLockName(room: string, deviceId: string): string {
  return `agentloom-remote-refresh:${room}:${deviceId}`;
}
