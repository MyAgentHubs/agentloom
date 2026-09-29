// backoff.ts — T6c-refresh · 指数退避（有上限有抖动）。
//
// M2 C1 spec §3 v0.5 块："指数退避有上限有抖动"——用于两处：① upgrade 失败分类为 retry_backoff 时
// 的重连节奏；② 未来（本单外）其它需要退避的路径可复用。纯函数,不依赖计时器/随机数全局（抖动源
// 注入,测试可钉死）,方便 connectionSession.ts 与测试都能确定性地推进"下一次尝试隔多久"。

export interface BackoffOptions {
  /** 首次重试前的基准延迟(ms)。 */
  baseMs: number;
  /** 延迟上限(ms)——指数增长到这里就封顶,不再继续翻倍。 */
  capMs: number;
  /** 翻倍因子,默认 2。 */
  factor?: number;
  /**
   * 抖动比例 [0,1]——实际延迟 = base*factor^n 的基础上,再乘以 `1 + jitterRatio*(rand()-0.5)*2`
   * 这类范围;这里用更简单直接的"满抖动"(full jitter,AWS 架构博客的推荐做法):
   * `delay = random(0, min(cap, base*factor^n))`。默认 1(满抖动)。
   */
  jitterRatio?: number;
  /** 抖动随机源,默认 `Math.random`——测试注入可钉死序列,做确定性断言。 */
  random?: () => number;
}

const DEFAULT_FACTOR = 2;
const DEFAULT_JITTER_RATIO = 1;

/**
 * 第 `attempt`(从 0 起)次重试前应该等待的毫秒数——满抖动策略:先算出该次尝试的"理论上限"
 * `min(cap, base*factor^attempt)`,再从 `[0, 理论上限]` 里均匀随机取一个值。`jitterRatio=0` 时
 * 退化成纯确定性指数退避(无抖动,`delay = 理论上限`)——留给需要可预测节奏的调用方/测试。
 */
export function computeBackoffDelayMs(attempt: number, options: BackoffOptions): number {
  if (attempt < 0 || !Number.isInteger(attempt)) {
    throw new Error(`attempt must be a non-negative integer, got ${attempt}`);
  }
  const factor = options.factor ?? DEFAULT_FACTOR;
  const jitterRatio = options.jitterRatio ?? DEFAULT_JITTER_RATIO;
  const random = options.random ?? Math.random;

  const theoretical = Math.min(options.capMs, options.baseMs * factor ** attempt);
  if (jitterRatio <= 0) {
    return theoretical;
  }
  const jitterFloor = theoretical * (1 - jitterRatio);
  const span = theoretical - jitterFloor;
  return jitterFloor + random() * span;
}

/** 累计连续失败次数的小状态机——`recordFailure()` 返回下一次该等多久,`reset()` 清零(连成功一次即清)。 */
export class BackoffTracker {
  private attempt = 0;

  constructor(private readonly options: BackoffOptions) {}

  /** 记一次失败,返回这次失败之后、下一次重试前应等待的毫秒数。 */
  recordFailure(): number {
    const delay = computeBackoffDelayMs(this.attempt, this.options);
    this.attempt += 1;
    return delay;
  }

  reset(): void {
    this.attempt = 0;
  }

  get attemptCount(): number {
    return this.attempt;
  }
}
