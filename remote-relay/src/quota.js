"use strict";
// quota.js — S3 配额与防滥用（骨架）。
//
// 口径依据母文档 §三·五「配额与防滥用」：按房间「里程碑消息条数/月」计数
// （它正是 DO 行写的成本单位）；超限对该房间的流量降级——但降级的方向是
// 「断 live、保里程碑」（母文档 §6b），不是「event 整体一起丢」：里程碑一旦
// 落库就会被将来的重连回放依赖，丢了等于产品承诺的历史记录出现空洞、且
// 补不回来；live 增量丢了只是当次转发少了一条，客户端重连即可从里程碑
// Preserve milestones so reconnects can restore a complete history after live traffic is shed.
// Dropping live updates is recoverable; dropping persisted history would leave permanent gaps.
//
// 这里的「月度里程碑配额」是**成本闸**（按 DO 行写计费），和另一个尚未做的
// 「每房间速率限流」（H2，见 TODO）是两码事：速率限流管的是「短时间内讲话
// 太快」（例如同一秒钟内 live 增量刷太猛），配额管的是「这个月写太多行」。
// 两者不该塌缩成同一个闸——H2 落地前，本文件只实现配额这一半。
//
// 具体生产数值母文档标注「M1 前定」——这里给一个可跑通的默认值，真实数值
// 上线前必须由产品侧另拍（见 wrangler.toml 里的 MONTHLY_MILESTONE_LIMIT 占位）。

// 骨架默认值，非生产数值。生产口径见 wrangler.toml [vars] 注释与母文档 §三·五。
export const DEFAULT_MONTHLY_MILESTONE_LIMIT = 3000;

// UTC 年-月作为配额统计周期 key，如 "2026-08"。
export function currentPeriod(nowMs = Date.now()) {
  const d = new Date(nowMs);
  return `${d.getUTCFullYear()}-${String(d.getUTCMonth() + 1).padStart(2, "0")}`;
}

/**
 * 纯判定函数：给定当前计数与配额上限，算房间是否超限。
 * @returns {{ allowed: boolean, exceeded: boolean, remaining: number }}
 */
export function evaluateQuota({ count, limit }) {
  const remaining = limit - count;
  return {
    allowed: remaining > 0,
    exceeded: remaining <= 0,
    remaining: Math.max(remaining, 0),
  };
}

/**
 * 超限时，哪些流量该被降级（母文档 §6b：断 live、保里程碑；control 永远放行）。
 * - control（Stop / snapshot 等）：永远放行——插队通道，配额闸不该管得住它，
 *   否则「超额之后连 Stop 都按不动」是更糟的故障模式。
 * - Shed live traffic when the quota is exceeded because these transient updates are
 *   separate from persisted events, so dropping them preserves history completeness.
 * - event（里程碑）：**永远不降级**，即使超限也照常落库——见上方文件头的
 *   理由。超限之后还有里程碑写入，只代表这个房间已经在配额之外产生成本，
 *   计费/告警是另一件事，不该体现成「丢用户的会话记录」。
 * - input / presence：本骨架不在配额闸的管辖范围内（母文档只点名了
 *   event/live 这条写入型流量）。
 */
export function shouldDegrade(kind, quotaState) {
  if (kind === "control") return false;
  if (kind === "live") return Boolean(quotaState && quotaState.exceeded);
  return false; // event（里程碑）/ input / presence：配额闸管不着
}
