import type { I18nKey } from "../i18n";

type Translate = (
  key: I18nKey,
  values?: Record<string, string | number>,
) => string;

/**
 * myagent lead run 收工（exit 3/4）时后端给出的 reason 码 → 人话 i18n key。
 * 后端来源：agent_event.rs `harness_blocked_message`（exit3·run.blocked）/
 * `harness_needs_decision_message`（exit4·run.needs_decision，已改为在白名单内
 * 优先用更具体的 blocked_reason 顶替笼统的顶层 reason="blocked_questions"）。
 *
 * 覆盖范围（对齐 harness-agent 实际会发的 reason 值·2026-07-25 opus 对抗审核过一遍）：
 * - exit4（run.needs_decision）：blocked_questions / no_progress / stuck_repeating /
 *   budget_exhausted_still_progressing / context_budget_exhausted。
 * - exit3（run.blocked）：approval_unavailable / rejected_repeatedly。同样是 exit3 的
 *   `max_eval_attempts exceeded without progress`（及裸 `max_eval_attempts`，plan 桥用）
 *   **没有**收录——它是一句话而非状态码、且不带占比高的行动指引，先保持裸透传，别硬凑人话。
 *
 * 没收录的 reason（含上面提到的 max_eval_attempts 族）原样透传（裸串）——这是前向兼容的
 * 硬要求：未来引擎新增的 reason 码在这张表补上之前必须保持可见，绝不能被静默吞掉或译错
 * （见 App.test.tsx 里 NO_CHECKPOINT_BLOCKED 那条断言）。
 */
const STOP_REASON_KEYS: Record<string, I18nKey> = {
  blocked_questions: "stopReason.blockedQuestions",
  no_progress: "stopReason.noProgress",
  stuck_repeating: "stopReason.stuckRepeating",
  budget_exhausted_still_progressing:
    "stopReason.budgetExhaustedStillProgressing",
  context_budget_exhausted: "stopReason.contextBudgetExhausted",
  approval_unavailable: "stopReason.approvalUnavailable",
  rejected_repeatedly: "stopReason.rejectedRepeatedly",
};

// 后端在 reason 后可能拼了后缀：
//   harness_blocked_message  → "{reason}（attempts=N；未过：ids）" / "{reason} (attempts=N; not passed: ids)"
//   harness_needs_decision_message → "{reason}: {next_step}"
// 按首段（纯 ascii 字母+下划线）匹配 reason、原样保留后缀（诊断信息不能被吞）。
const REASON_HEAD = /^([a-zA-Z_]+)(\s?[:：（(].*)?$/s;

/** 把收工/待决 reason 裸串过一遍人话映射；未收录的 reason（或非 reason 形态的消息，
 * 如已经是人话的中断提示）原样返回，不改动。 */
export function humanizeStopReason(raw: string, t: Translate): string {
  const match = REASON_HEAD.exec(raw);
  if (!match) return raw;
  const [, head, suffix = ""] = match;
  const key = STOP_REASON_KEYS[head];
  if (!key) return raw;
  return `${t(key)}${suffix}`;
}

// member 终态 failure_reason/detail 是「诚实正文（人话）+ 尾部追加的引擎裸码行」混合体
// （member_runner.rs 把 member_context_exhausted_failure_message 这类人话段落，跟
// harness_needs_decision_message/harness_blocked_message 吐出的裸 "reason: next_step" 用
// "\n" 拼在一起——见 member_runner.rs 里 `message.push('\n'); message.push_str(detail)`
// 那处）。MemberDrillIn 展示原始 failure_reason（换行未展平），LeadSummaryBlock 展示的
// detail 经 memberFailure.ts::clipResultDetail 把换行展平成 " · "——两种分隔符都要认。
//
// 只在分隔符（字符串起始 / 换行 / 展平后的 " · "）之后精确匹配 STOP_REASON_KEYS 里的已知
// 裸码**字面量**（不用宽松的 `[a-zA-Z_]+`），避免诚实正文里偶然出现的英文单词+冒号被误判
// 成裸码头、进而错误吞掉后面的真实裸码段。
const KNOWN_REASON_CODE_PATTERN = Object.keys(STOP_REASON_KEYS)
  // 防御性按长度降序：万一未来加码出现前缀关系，更长的字面量优先匹配。
  .sort((a, b) => b.length - a.length)
  .join("|");
const TRAILING_REASON = new RegExp(
  `(^|\\n|\\s·\\s)(${KNOWN_REASON_CODE_PATTERN})(\\s?[:：（(].*)?$`,
  "s",
);

// STOP_REASON_KEYS 里 blocked_questions 那条写死了「lead 停在待决问题上」（给 lead/solo 会话
// 用）——原样复用会在 member 卡片上显示主体错位的话（member 不是 lead）。这条单独换一个对
// member 中性的 key，其余 6 码复用同一份 STOP_REASON_KEYS（都没有主体指代问题）。
const MEMBER_REASON_KEY_OVERRIDES: Partial<Record<string, I18nKey>> = {
  blocked_questions: "memberFailure.code.blockedQuestions",
};

/** member 失败文案（MemberDrillIn 裸显的 failure_reason / LeadSummaryBlock 的
 * failureReasonText detail）专用：只 humanize 尾部追加的裸码段，前面的诚实正文原样保留；
 * 没有可识别裸码（纯人话文本，或裸码不在收录表里）原样透传——透传纪律同 humanizeStopReason。 */
export function humanizeFailureDetail(raw: string, t: Translate): string {
  if (!raw) return raw;
  const match = TRAILING_REASON.exec(raw);
  if (!match) return raw;
  const [, sep, head, suffix = ""] = match;
  const key = MEMBER_REASON_KEY_OVERRIDES[head] ?? STOP_REASON_KEYS[head];
  if (!key) return raw;
  const prefix = raw.slice(0, match.index);
  return `${prefix}${sep}${t(key)}${suffix}`;
}
