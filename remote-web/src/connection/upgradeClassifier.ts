// upgradeClassifier.ts — T6c-refresh · "无状态码"分类判定表(M0 §3 v0.5 块)。
//
// "浏览器约束：WebSocket upgrade 失败拿不到 HTTP 状态码（WHATWG 只给通用 error）——禁止写
// `if status===401` 式分支，分类只能靠「本地过期钟 + 连接阶段 + 随后 refresh 尝试的结果」"。
//
// **两条独立的分类输入(本文件都覆盖，互相补足，不冲突)**：
//   ① 真正的"upgrade 失败"(WS 从未到达 onopen——浏览器只给一个不带 code/reason 的通用
//      error/close 事件)——此时**没有**服务端给的任何理由字符串，唯一线索就是本地时钟 + 连接
//      阶段，`classifyUpgradeFailure()` 覆盖这条。
//   ② 一条**已经 open 过**的连接被服务端**带理由关闭**(relay 明确给了 `.reason` 字符串，如
//      `token_reauthorization_failed`/`message_rate_limited`——见 `remote-relay/src/room-do.js`
//      `REAUTH_CLOSE_REASON`/`MESSAGE_RATE_LIMIT_CLOSE_REASON`)——这种情况其实**有**信息可用，
//      `classifyCloseReason()` 直接按理由字符串分类，不必退化成纯时钟猜测；理由不认识时返回
//      `"unclassified"`，调用方(`connectionSession.ts`)退回 `classifyUpgradeFailure()` 兜底。
//
// **审查返工（invalid 分类保守化，依据 = app/src-tauri/src/lib.rs:901-929）**：早期版本的判定表有
// 一条"`lastRefreshOutcome === fail_invalid` → 直接 needs_repair"的规则，把普通 `token.refresh.fail
// {reason:"invalid"}`（不带 close）当认证终态的强信号。这是错的——`lib.rs` 的
// `remote_gateway_refresh_handler` 实证：registry 锁中毒 / `Db` state 不可用 / `Db` 连接锁中毒这类
// **桌面自身的内部基建故障**，也会原样发 `refresh_fail_json(request_id, subject, "invalid", false)`
// （`reason:"invalid"`、`close:false`）——这只是那个通用 fail 出口函数的默认文案，跟"你的
// refresh_token 到底对不对"这个认证判断毫无关系。普通 `invalid`（不带 close）因此从判定表里整条
// 删除；唯一权威的认证终态信号是 `close===true`（桌面 `record_refresh_invalid` 真正数过连续 ≥3
// 次判定为无效才会置位——`connectionSession.ts::handleRefreshFail` 直接处理这个信号，不经过本文件）
// 或本文件行 3 的 30 天 refresh_until 纯时钟判定。判定表因此从 6 条精简为 4 条。
//
// **审查返工（49h 阈值改为 30 天 refresh_until 窗）**：`§9.2` TTL clamp 的四个上限分别是
// pairing 5.5min / access 65min / prev 窗 48h / **refresh_until 30d**——早期版本把"access 名义寿命
// + prev 窗(48h)"（≈49h）当"这条凭据彻底死透"的判据，但 `§9.1` 准入表写得很清楚：**current** 别名
// 的 `valid_until` = **`refresh_until`（30 天）**，48h 的 prev 窗只适用于**已经被换代、降级成 prev
// 的旧别名**——本客户端自己手里这枚从未被自己以外的路径轮换过的 access token，只要它还是
// "current"，其可用于 refresh scope 重连的窗口就是完整的 30 天，不是 49 小时。49h 时下判 needs_repair
// 会在凭据其实还有 29 天可用寿命时就误清凭据、逼用户重新扫码。阈值改名 `refreshUntilWindowMs`
// （默认 30 天，从签发时刻起算，不再叠加 accessLifetimeMs——refresh_until 本身就是从签发时刻起算
// 的绝对值，参见 M0 §9.3 `token.put` 的 `current.refresh_until` 字段）。
//
// **审查返工（两处等值边界改为与 relay 的严格 `<` 一致）**：`§9.1` 准入表的"仍然在这个态"条件都是
// 严格 `now < X`（`now < access_expires` 才是 remote scope；`now < valid_until` 才允许 refresh
// scope），换句话说"不再在这个态"是 `now >= X`。早期版本两处判定都用严格 `>` 划界（`elapsed >
// accessLifetimeMs`/`elapsed > accessLifetimeMs + prevWindowMs`），在 `elapsed` 恰好等于阈值那一刻
// 仍判"还安全"——与 relay 的准入表在同一时刻的真实判定（已经不再是 remote scope / 已经不再有效）
// 相反。两处都改成 `>=`（"不再安全"这一侧含边界，"仍安全"这一侧严格小于）。

import type { ConnectionPhase, UpgradeFailureClassification } from "./types.ts";

/** M0 §9.2："上限…access 65min" 的名义值——我们用比 clamp 上限更保守的 1h 做本地估算基准。 */
export const DEFAULT_ACCESS_LIFETIME_MS = 60 * 60 * 1000;
/** M0 §9.2："上限…refresh_until 30d"——current 别名的完整 refresh-scope 可用窗口，从签发时刻起算。 */
export const DEFAULT_REFRESH_UNTIL_WINDOW_MS = 30 * 24 * 60 * 60 * 1000;

export interface UpgradeFailureContext {
  nowMs: number;
  /** `null` = 从未成功拿到过 access(不该在配对完成后发生,防御性地按最坏情况处理)。 */
  accessIssuedAtMs: number | null;
  accessLifetimeMs?: number;
  /** current 别名 refresh_until 宽限窗(从签发时刻起算)，默认 `DEFAULT_REFRESH_UNTIL_WINDOW_MS`。 */
  refreshUntilWindowMs?: number;
  connectionPhase: ConnectionPhase;
}

/**
 * 判定表——按顺序求值,第一条命中的规则生效(审查返工后精简为 4 条,`lastRefreshOutcome` 相关的
 * 认证信号规则已删除,见文件顶注):
 *  1. 从未成功拿到过 access——防御性地按"需要重配对"处理(正常配对后不该发生这个状态)。
 *  2. 本地钟算出的"access 签发以来经过的时间"为负(签发时间在未来)——时钟偏差/异常,不据此下强
 *     结论,按"网络退避重试"处理(不无端跳去 needs_repair/needs_refresh)。
 *  3. 经过时间 **达到或超过** `refreshUntilWindowMs`(默认 30 天)——本地估算这个 token 的 current
 *     别名 refresh scope 窗口也关了,判"需要重配对"(与 relay `now < valid_until` 严格小于对齐:
 *     恰好等于边界即"不再有效")。
 *  4. 经过时间 **达到或超过** `accessLifetimeMs` 但仍在 refresh_until 窗内——大概率落在 refresh
 *     scope 窗口,upgrade 失败更可能是这段"access 已过、还在 refresh 窗内"的过渡期抖动,判"需要
 *     refresh"(重连后应立即尝试;与 relay `now < access_expires` 严格小于对齐)。
 *  5. 其余情况(access 按本地钟应该还在有效期内,upgrade 却失败了)——没有理由怀疑凭据本身,判
 *     "网络退避重试"(relay 抖动/room 刚 claim/registry 还没 ready 等瞬时状况)。
 *
 * `connectionPhase` 目前只影响调用方的退避基准(首连 vs 重连可以配不同的 backoff 参数)，不改变
 * 分类结果本身——上面规则已经完整覆盖"要不要怀疑凭据"这件事，first_connect 时凭据必然是刚发出
 * 的新鲜值（elapsed 天然很小），最后一条规则自然生效，不需要再单独分支。
 *
 * **认证终态不在这里判**:`close===true` 是唯一权威的"连续 ≥3 次无效"信号,由
 * `connectionSession.ts::handleRefreshFail` 收到该帧的当下直接处理(立即 needs_repair + 主动断开
 * socket),不等到下一次 upgrade 失败才经由这张表推断——那样会平白晚一整个重连周期才反应,且如果
 * 用户从此再也没有主动重连过,这张表甚至可能永远没机会被再次求值。
 */
export function classifyUpgradeFailure(context: UpgradeFailureContext): UpgradeFailureClassification {
  if (context.accessIssuedAtMs === null) {
    return "needs_repair";
  }
  const accessLifetimeMs = context.accessLifetimeMs ?? DEFAULT_ACCESS_LIFETIME_MS;
  const refreshUntilWindowMs = context.refreshUntilWindowMs ?? DEFAULT_REFRESH_UNTIL_WINDOW_MS;
  const elapsed = context.nowMs - context.accessIssuedAtMs;
  if (elapsed < 0) {
    return "retry_backoff";
  }
  if (elapsed >= refreshUntilWindowMs) {
    return "needs_repair";
  }
  if (elapsed >= accessLifetimeMs) {
    return "needs_refresh";
  }
  return "retry_backoff";
}

/**
 * 一条**已经 open 过**的连接被服务端带理由关闭时的快捷分类——认识的理由直接给结论,不认识就交
 * 回调用方退化到 `classifyUpgradeFailure()`（纯时钟兜底）。
 */
export function classifyCloseReason(reason: string): UpgradeFailureClassification | "unclassified" {
  if (reason === "token_reauthorization_failed") {
    // §9.1 入站持续再授权闸——access/refresh 窗口在连接期间被越过,钉死重连+refresh,不落回纯时钟
    // 猜测(这是服务端明确给的、不是"猜")。
    return "needs_refresh";
  }
  if (reason === "message_rate_limited") {
    // G8-knife 专属 close reason——"手机端勿误判凭据失效"(M0 v1.8.9 changelog 原话),必须走一般
    // 退避,绝不能被误判成认证问题。
    return "retry_backoff";
  }
  return "unclassified";
}
