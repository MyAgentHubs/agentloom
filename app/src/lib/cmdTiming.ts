/**
 * 性能第二轮临时仪表（埋点三件之二）：单点 patch `window.__TAURI_INTERNALS__.invoke`，
 * 计时所有 Tauri command 往返耗时，超过阈值才复用现成的 `boot_trace` 通道上报落盘
 * （`<app_data_dir>/logs/boot-trace.log`，含 256KB 截断兜底）。数据够了这个文件可以整个拆除。
 *
 * 之所以在这一个点 patch：仓内 20+ 处 `invoke(...)` 调用没有中间封装层，全部落到
 * `@tauri-apps/api/core` 的 `invoke()`，而它每次调用都现读 `window.__TAURI_INTERNALS__.invoke`
 * （不在模块加载时缓存引用）——所以只要在任何业务 invoke 发生前完成 patch，就能捕获全部往返，
 * 不用碰任何业务调用点。
 */

export const CMD_TIMING_THRESHOLD_MS = 100;

/** boot_trace 自身（以及未来任何新增的 trace 类命令）：不计时、不上报，防自记录循环。 */
const EXCLUDED_COMMANDS = new Set(["boot_trace"]);

export type RawInvoke = (
  cmd: string,
  args?: unknown,
  options?: unknown,
) => Promise<unknown>;

/**
 * 纯工厂：把计时逻辑包进 rawInvoke。只在超阈值时调用 report，label 固定为 `cmd:{命令名}`。
 * 原 promise 的 resolve/reject 语义逐字保持——上报本身绝不影响返回值/异常透传，
 * reject 时也会先判定计时（可能上报）再原样 reject。
 */
export function wrapInvokeWithTiming(
  rawInvoke: RawInvoke,
  report: (label: string, ms: number) => void,
  now: () => number = () => performance.now(),
): RawInvoke {
  return function timedInvoke(cmd: string, args?: unknown, options?: unknown) {
    if (EXCLUDED_COMMANDS.has(cmd)) {
      return rawInvoke(cmd, args, options);
    }
    const start = now();
    return rawInvoke(cmd, args, options).finally(() => {
      const elapsed = now() - start;
      if (elapsed > CMD_TIMING_THRESHOLD_MS) {
        report(`cmd:${cmd}`, elapsed);
      }
    });
  };
}

type TauriInternals = { invoke: RawInvoke };

/**
 * 已完成 patch 的 `__TAURI_INTERNALS__` 对象登记表：用模块级 WeakSet 代替在对象上
 * 写标记属性——真实 Tauri runtime 里该对象可能整体 frozen，写属性本身也会抛错
 * （踩过一次：曾经的 `internals[PATCHED_FLAG] = true` 就是这类风险点之一）。
 */
const patchedInternals = new WeakSet<object>();

/**
 * 实际执行 patch：幂等（重复调用不叠包），非 Tauri 环境（没有 `__TAURI_INTERNALS__`，
 * 例如测试）静默 no-op。生产/开发都调用——阈值过滤已经控制了噪声。
 *
 * P0 教训：真实 Tauri runtime 里 `window.__TAURI_INTERNALS__.invoke` 可能是不可写
 * （甚至整个对象 frozen）属性，裸赋值 `internals.invoke = wrapped` 在 ES 模块严格
 * 模式下会抛 `TypeError: Cannot assign to read only property`，且发生在 main.tsx
 * 模块求值期、早于 React 挂载——整个 app 直接死掉、连兜底报错页都来不及挂。
 *
 * 因此这里：① 全函数体 try/catch 兜底，任何异常都不能逃逸出去；② 打补丁前用
 * `Object.getOwnPropertyDescriptor` 探测 writable/configurable，按能力选路径
 * （可写直接赋值 / 不可写但 configurable 用 defineProperty 重新定义 / 两者都不
 * 具备则放弃打补丁、静默 no-op——仪表停用不影响 app 正常运行）。
 */
export function installCmdTiming(): void {
  try {
    const internals = (
      window as unknown as { __TAURI_INTERNALS__?: TauriInternals }
    ).__TAURI_INTERNALS__;
    if (!internals || typeof internals.invoke !== "function") return;
    if (patchedInternals.has(internals)) return;

    const rawInvoke = internals.invoke.bind(internals) as RawInvoke;
    const report = (label: string, ms: number) => {
      try {
        void rawInvoke("boot_trace", { label, ms }).catch(() => {});
      } catch {
        // 上报通道本身失败绝不能影响业务 invoke。
      }
    };

    const wrapped = wrapInvokeWithTiming(rawInvoke, report);
    const descriptor = Object.getOwnPropertyDescriptor(internals, "invoke");

    if (!descriptor || descriptor.writable) {
      // 没有自有属性描述符（继承）或明确可写：走原本的直接赋值路径。
      internals.invoke = wrapped;
    } else if (descriptor.configurable) {
      // 不可写但可配置：用 defineProperty 重新定义整个属性描述符。
      Object.defineProperty(internals, "invoke", {
        value: wrapped,
        writable: true,
        configurable: true,
        enumerable: descriptor.enumerable,
      });
    } else {
      // 既不可写也不可配置（甚至整个对象被 freeze）：无法打补丁，放弃。
      // 性能仪表是可选设施，停用不影响 app 正常运行。
      return;
    }

    patchedInternals.add(internals);
  } catch (err) {
    // 兜底红线：性能仪表在任何情况下都不能让 app 启动失败。
    console.warn("[cmdTiming] installCmdTiming failed, timing disabled", err);
  }
}
