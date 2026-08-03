// @ts-expect-error - Vitest runs in Node, but this frontend tsconfig has no Node type declarations.
import { readFileSync } from "fs";
import { describe, expect, it } from "vitest";
import type { Locale, TranslationKey } from "../i18n";
import { humanizeFailureDetail, humanizeStopReason } from "./stopReason";

function loadI18nMessages(): Record<Locale, Record<string, string>> {
  const source = readFileSync("src/i18n.tsx", "utf-8");
  const match = source.match(/const messages = (\{[\s\S]*?\n\} as const)/);
  if (!match) throw new Error("Could not locate the i18n message tables");
  const literalText = match[1].replace(/\s+as const$/, "");
  return new Function(`"use strict"; return (${literalText});`)() as Record<
    Locale,
    Record<string, string>
  >;
}

const i18nMessages = loadI18nMessages();

const localizedT = (locale: Locale) =>
  ((key: TranslationKey, values?: Record<string, string | number>) => {
    let template = i18nMessages[locale][key] ?? key;
    for (const [name, value] of Object.entries(values ?? {})) {
      template = template.split(`{${name}}`).join(String(value));
    }
    return template;
  }) as (
    key: TranslationKey,
    values?: Record<string, string | number>,
  ) => string;

const zh = localizedT("zh");
const en = localizedT("en");

describe("humanizeStopReason", () => {
  it.each([
    ["blocked_questions", "stopReason.blockedQuestions"],
    ["no_progress", "stopReason.noProgress"],
    ["stuck_repeating", "stopReason.stuckRepeating"],
    [
      "budget_exhausted_still_progressing",
      "stopReason.budgetExhaustedStillProgressing",
    ],
    ["context_budget_exhausted", "stopReason.contextBudgetExhausted"],
    ["approval_unavailable", "stopReason.approvalUnavailable"],
    ["rejected_repeatedly", "stopReason.rejectedRepeatedly"],
  ])("known reason %s → zh human text", (reason, key) => {
    expect(humanizeStopReason(reason, zh)).toBe(zh(key as TranslationKey));
    expect(humanizeStopReason(reason, zh)).not.toBe(reason);
  });

  it("known reason → en human text", () => {
    expect(humanizeStopReason("no_progress", en)).toBe(
      en("stopReason.noProgress"),
    );
  });

  it("未知 reason 原样透传（前向兼容·不静默吞）", () => {
    expect(humanizeStopReason("NO_CHECKPOINT_BLOCKED", zh)).toBe(
      "NO_CHECKPOINT_BLOCKED",
    );
  });

  it("非 reason 形态的人话消息（已经是完整句子）原样透传", () => {
    expect(
      humanizeStopReason("运行已中断（可续跑：agent resume run-1）", zh),
    ).toBe("运行已中断（可续跑：agent resume run-1）");
  });

  it("已知 reason 拼了 harness_blocked_message 中文后缀 → 人话头 + 保留后缀", () => {
    const raw = "no_progress（attempts=2；未过：a,b）";
    expect(humanizeStopReason(raw, zh)).toBe(
      `${zh("stopReason.noProgress")}（attempts=2；未过：a,b）`,
    );
  });

  it("已知 reason 拼了 harness_blocked_message 英文后缀 → 人话头 + 保留后缀", () => {
    const raw = "no_progress (attempts=2; not passed: a,b)";
    expect(humanizeStopReason(raw, en)).toBe(
      `${en("stopReason.noProgress")} (attempts=2; not passed: a,b)`,
    );
  });

  it("已知 reason 拼了 harness_needs_decision_message next_step 后缀 → 人话头 + 保留后缀", () => {
    const raw = "context_budget_exhausted: 拆小任务 / 换更大上下文的模型";
    expect(humanizeStopReason(raw, zh)).toBe(
      `${zh("stopReason.contextBudgetExhausted")}: 拆小任务 / 换更大上下文的模型`,
    );
  });

  it("未知 reason 拼了后缀 → 整串原样透传", () => {
    const raw = "some_future_reason（attempts=1；未过：x）";
    expect(humanizeStopReason(raw, zh)).toBe(raw);
  });

  it("exit3 · approval_unavailable 带 harness_blocked_message 后缀 → 人话头 + 保留后缀", () => {
    const raw = "approval_unavailable（attempts=1；未过：a）";
    expect(humanizeStopReason(raw, zh)).toBe(
      `${zh("stopReason.approvalUnavailable")}（attempts=1；未过：a）`,
    );
  });

  it("exit3 · rejected_repeatedly 无后缀（无 attempts 字段时 harness_blocked_message 只吐裸 reason）→ 人话", () => {
    expect(humanizeStopReason("rejected_repeatedly", zh)).toBe(
      zh("stopReason.rejectedRepeatedly"),
    );
  });

  it("max_eval_attempts 族（未收录·刻意保持裸透传）：带空格的完整短语原样透传", () => {
    // harness-agent 实际吐的是一句话（非状态码）："max_eval_attempts exceeded without
    // progress"，正文里带空格——REASON_HEAD 只捕获 [a-zA-Z_]+ 打头的纯标识符段，遇到
    // 空格就中断匹配，整串落回未知分支原样返回，不会被错误腰斩成 "max_eval_attempts"。
    const raw =
      "max_eval_attempts exceeded without progress（attempts=3；未过：a,b）";
    expect(humanizeStopReason(raw, zh)).toBe(raw);
  });

  it("max_eval_attempts 族：plan 桥裸 reason（无后缀）也原样透传", () => {
    expect(humanizeStopReason("max_eval_attempts", zh)).toBe(
      "max_eval_attempts",
    );
  });
});

// member 终态 failure_reason/detail 是「诚实正文（人话）+ 尾部追加的引擎裸码行」混合体
// （member_runner.rs 用 "\n" 拼接；LeadSummaryBlock 消费的 detail 经
// memberFailure.ts::clipResultDetail 把换行展平成 " · "）——两个消费点（MemberDrillIn 原始
// 换行 / LeadSummaryBlock 展平后的 " · "）都要认。
describe("humanizeFailureDetail", () => {
  it("诚实正文 + 换行 + 尾部裸码 → 裸码段变人话，诚实正文原样保留", () => {
    const raw =
      "工人的上下文窗口装不下了（单轮 token 预算耗尽）；不是卡住，也没有问题在等回答。\ncontext_budget_exhausted: 拆小任务 / 换更大上下文的模型";
    const out = humanizeFailureDetail(raw, zh);
    expect(out).toBe(
      `工人的上下文窗口装不下了（单轮 token 预算耗尽）；不是卡住，也没有问题在等回答。\n${zh("stopReason.contextBudgetExhausted")}: 拆小任务 / 换更大上下文的模型`,
    );
    expect(out).toContain("工人的上下文窗口装不下了");
  });

  it("诚实正文 + 展平分隔符「 · 」+ 尾部裸码（LeadSummaryBlock detail 形态）→ 裸码段变人话", () => {
    const raw =
      "工人的轮次预算用完了；任务还没做完，但它在正常推进。 · budget_exhausted_still_progressing: 发一条消息可继续";
    const out = humanizeFailureDetail(raw, zh);
    expect(out).toBe(
      `工人的轮次预算用完了；任务还没做完，但它在正常推进。 · ${zh("stopReason.budgetExhaustedStillProgressing")}: 发一条消息可继续`,
    );
    expect(out).toContain("工人的轮次预算用完了");
  });

  it("整串以可识别裸码开头（无诚实前缀）→ 整段转人话", () => {
    const raw = "context_budget_exhausted: 拆小任务 / 换更大上下文的模型";
    expect(humanizeFailureDetail(raw, zh)).toBe(
      `${zh("stopReason.contextBudgetExhausted")}: 拆小任务 / 换更大上下文的模型`,
    );
  });

  it("含未知裸码（尾部）→ 原样透传可见（前向兼容·不静默吞）", () => {
    const raw = "诚实正文在这里。\nsome_future_code: xxx";
    expect(humanizeFailureDetail(raw, zh)).toBe(raw);
  });

  it("纯人话文本（无任何裸码）→ 原样透传", () => {
    const raw =
      "工人停摆：有问题在等回答，或执行被阻塞（exit status: 3）。这不是环境故障——看它最后的输出。";
    expect(humanizeFailureDetail(raw, zh)).toBe(raw);
  });

  it("blocked_questions 在 member 上下文用中性措辞，不复用 lead 专属的「lead 停在待决问题上」", () => {
    const raw = "诚实正文。\nblocked_questions: 等待用户确认";
    const out = humanizeFailureDetail(raw, zh);
    expect(out).toContain(zh("memberFailure.code.blockedQuestions"));
    expect(out).not.toContain("lead 停在待决问题上");
  });

  it("空字符串原样返回", () => {
    expect(humanizeFailureDetail("", zh)).toBe("");
  });
});
