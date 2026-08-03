import type {
  TeamRun,
  MemberUnit,
  LeadSummaryBlock,
  SummarySection as PersistedSummarySection,
  SummaryStatus,
  Finding,
  CriterionTrust,
} from "../types/agent";
import type { I18nKey } from "../i18n";
import { memberFailureReason, type MemberFailureReason } from "./memberFailure";

export type SummarySectionId =
  | "result"
  | "changes"
  | "verify"
  | "risk"
  | "llm"
  | "fallback";

/**
 * D7 措辞更正（delta 复审终轮·2026-07-26）：用户摘要里的「风险」段只放行这几个 id——
 * 同款白名单已经在后端 member_runner.rs::render_member_result_report 里存在
 * （只挑 transient_error / git_write_blocked 两个 id 拼进 DB 消息），这里镜像同一份
 * 白名单挡住 UI 渲染路径。默认不外显：新增一种 risk 时，除非显式把 id 加进这份白名单 +
 * 配好 i18n，否则不会出现在用户摘要里（避免像 stalled_narrative_on_clean_exit 那样——
 * 排查用的内部黑话中文句子被原样糊进成功 run 的用户摘要、en locale 下还会显中文）。
 * risks 数组本身、DB 里的原始记录不受影响——只是不进这条 UI 渲染路径。
 */
const USER_FACING_RISK_IDS = new Set(["transient_error", "git_write_blocked"]);

export type SummaryI18nText = {
  key: I18nKey;
  values?: Record<string, string | number>;
};

type SummarySectionBase = Omit<PersistedSummarySection, "heading"> & {
  heading_values?: Record<string, string | number>;
  body_i18n?: SummaryI18nText[];
  failure_reason?: MemberFailureReason;
};

export type SummarySection =
  | (SummarySectionBase & { id: "llm"; heading: string })
  | (SummarySectionBase & {
      id: Exclude<SummarySectionId, "llm">;
      heading: I18nKey | "";
    });

export type KeyedSummarySection = SummarySection;

export type KeyedFinding = Finding & {
  text_i18n?: SummaryI18nText;
  failure_reason?: MemberFailureReason;
};

export type KeyedLeadSummaryBlock = Omit<
  LeadSummaryBlock,
  "sections" | "findings"
> & {
  sections: KeyedSummarySection[];
  findings: KeyedFinding[];
};

export type KeyedCriterionTrust = Omit<CriterionTrust, "label"> & {
  label: I18nKey;
};

export function summaryStatusOf(run: TeamRun): SummaryStatus {
  const total = run.members.length;
  const failed = run.members.filter(
    (m) => m.status === "failed" || m.status === "stopped",
  ).length;
  const succeeded_count = total - failed;
  const kind =
    failed === 0
      ? "all_succeeded"
      : succeeded_count === 0
        ? "failed"
        : "partial";
  return { kind, succeeded_count, total };
}

/** worker final 文本：拼 text 块·剥掉开头 = sub 的派单 subtask（codex P1-11·reducer 会把 subtask 写进 blocks）。 */
export function memberFinalText(m: MemberUnit): string {
  const texts = m.blocks
    .filter((b): b is { type: "text"; text: string } => b.type === "text")
    .map((b) => b.text);
  if (texts.length && m.sub && texts[0].trim() === m.sub.trim()) texts.shift();
  return texts.join("\n\n").trim();
}

// P2-4（opus 对抗审）+ 对抗审补丁（budget_exhausted/context_exhausted）：
// stalled/budget_exhausted/context_exhausted 复用「worker 调用失败：{reason}」会渲出
// 「worker 调用失败：工人停摆……这不是环境故障」这种自相矛盾的话——诚实停摆 / 预算耗尽
// 仍在推进 / 上下文窗口装不下都不是调用失败，得各自查表用独立措辞的 key；表里没有的码
// 落回通用「调用失败」模板。
// ★这张表是第二个消费点——上一刀（budget_exhausted）就是漏了这里被对抗审打回，第四类
// （context_exhausted）务必跟 LeadSummaryBlock.tsx 的 FAILURE_HEADLINE_KEYS 一起改，别再漏。
const WORKER_FAILURE_TRACE_KEYS: Partial<
  Record<MemberFailureReason["code"], I18nKey>
> = {
  stalled: "leadSummary.workerFailure.stalledTrace",
  budget_exhausted: "leadSummary.workerFailure.budgetExhaustedTrace",
  context_exhausted: "leadSummary.workerFailure.contextExhaustedTrace",
};

function memberSummaryContent(
  m: MemberUnit,
  emptyFallback: I18nKey,
): Pick<KeyedSummarySection, "body_richtext" | "body_i18n" | "failure_reason"> {
  const failed = m.status === "failed";
  const reason = failed ? memberFailureReason(m) : null;
  if (reason != null) {
    const key: I18nKey =
      WORKER_FAILURE_TRACE_KEYS[reason.code] ??
      "leadSummary.workerFailure.trace";
    return {
      body_i18n: [{ key }],
      failure_reason: reason,
    };
  }
  const finalText = memberFinalText(m);
  if (finalText !== "") return { body_richtext: finalText };
  return {
    body_i18n: [
      {
        key: failed ? "leadSummary.workerFailure.noResultTrace" : emptyFallback,
      },
    ],
    ...(failed ? { failure_reason: { code: "no_final_text" } as const } : {}),
  };
}

/** verdict 骨架派生事实 section（改动表/验证/风险）·模板+事实·非 LLM 散文（真叙事归后端·§7.2）。
 * 只用 member.result 的硬事实·无数据的节不出（诚实·不编）。 */
export function buildVerdictSections(run: TeamRun): KeyedSummarySection[] {
  const aids = run.members.map((m) => m.assignment_id);
  const trace_ref = { run_id: run.run_id, assignment_ids: aids };
  const mk = (
    id: Extract<SummarySectionId, "changes" | "verify" | "risk">,
    heading: I18nKey,
    content: Pick<KeyedSummarySection, "body_richtext" | "body_i18n">,
  ): KeyedSummarySection => ({
    id,
    heading,
    ...content,
    findings: [],
    attribution: aids,
    trace_ref,
  });
  const out: KeyedSummarySection[] = [];

  // 改动：文件 | 改了什么 | 变更·三列对齐原型 DOM〔「改了什么」白话=lead LLM·后端·本程填占位 — ·留结构后端补〕
  const files = run.members.flatMap((m) => m.result?.changed_files ?? []);
  if (files.length > 0) {
    const rows = files
      .map((f) => `| ${f.path} | — | +${f.insertions} −${f.deletions} |`)
      .join("\n");
    out.push(
      mk("changes", "leadSummary.section.changes", {
        body_i18n: [
          {
            key: "leadSummary.section.changes.table",
            values: { rows },
          },
        ],
      }),
    );
  }

  // 验证：命令证据（cmd + 退出码）。只显**有真退出码**的验证命令·滤掉无退出码的工具噪音
  // （WebSearch 等被后端记进 command_evidence·exit_code=null → 旧版显「退出码 --」垃圾·块B 修）。
  const cmds = run.members
    .flatMap((m) => m.result?.command_evidence ?? [])
    .filter((c) => c.exit_code != null);
  if (cmds.length > 0) {
    out.push(
      mk("verify", "leadSummary.section.verify", {
        body_i18n: cmds.map((c) => ({
          key: "leadSummary.section.verify.command",
          values: { cmd: c.cmd, code: c.exit_code! },
        })),
      }),
    );
  }

  // 风险：member.result.risks（无则不出·不编）——只渲白名单内的 id（USER_FACING_RISK_IDS）。
  // 非白名单的 risk（如排查用的 stalled_narrative_on_clean_exit）留在 risks 数组/DB 里，
  // 但不进这条用户可见的摘要渲染路径。
  const risks = run.members
    .flatMap((m) => m.result?.risks ?? [])
    .filter((r) => USER_FACING_RISK_IDS.has(r.id));
  if (risks.length > 0) {
    out.push(
      mk("risk", "leadSummary.section.risk", {
        body_richtext: risks.map((r) => `- ${r.text}`).join("\n"),
      }),
    );
  }

  return out;
}

export function buildSinglePassthroughSummary(
  run: TeamRun,
): KeyedLeadSummaryBlock {
  const m = run.members[0];
  return {
    type: "lead_summary",
    run_id: run.run_id,
    summary_source: "single_passthrough",
    status: summaryStatusOf(run),
    sections: [
      {
        id: "result",
        heading: "",
        ...memberSummaryContent(
          m,
          "leadSummary.workerFailure.emptyPassthroughTrace",
        ),
        findings: [],
        attribution: [m.assignment_id],
        trace_ref: { run_id: run.run_id, assignment_ids: [m.assignment_id] },
      },
      ...buildVerdictSections(run),
    ],
    findings: [],
    artifact_refs: [],
  };
}

/** 路 B（coding 闭环）完成态 verdict·块 B。复用 buildVerdictSections 取改动/风险节·
 * 「验证」节用用户走的 coding 验证（verifyCmd + lastVerdict）·非派单期 command_evidence（P1-1）。
 * 结果节 = worker 文本 + 终态模板句（真叙事归后端·§7.2）。coding 路 shouldEnterCodingLoop 保证恰 1 member。 */
export function buildCodingVerdictSummary(
  run: TeamRun,
  coding: {
    verifyCmd: string;
    lastVerdict?: string | null;
    phase: "applied" | "shelved" | "landing_blocked";
  },
): KeyedLeadSummaryBlock {
  const m = run.members[0];
  const aids = run.members.map((x) => x.assignment_id);
  const trace_ref = { run_id: run.run_id, assignment_ids: aids };
  const tail: I18nKey =
    coding.phase === "applied"
      ? "leadSummary.coding.applied"
      : coding.phase === "landing_blocked"
        ? "leadSummary.coding.landingBlocked"
        : "leadSummary.coding.shelved";
  const worker = memberFinalText(m);
  const base = buildVerdictSections(run); // [改动?, 验证(派单期)?, 风险?]
  const change = base.find((s) => s.id === "changes");
  const risk = base.find((s) => s.id === "risk");
  const sections: KeyedSummarySection[] = [
    {
      id: "result",
      heading: "",
      ...(worker ? { body_richtext: worker } : {}),
      body_i18n: [{ key: tail }],
      findings: [],
      attribution: [m.assignment_id],
      trace_ref: { run_id: run.run_id, assignment_ids: [m.assignment_id] },
    },
  ];
  if (change) sections.push(change);
  if (coding.verifyCmd.trim())
    sections.push({
      id: "verify",
      heading: "leadSummary.section.verify",
      body_i18n: [
        coding.lastVerdict != null
          ? {
              key: "leadSummary.coding.verify.verdict",
              values: { cmd: coding.verifyCmd, verdict: coding.lastVerdict },
            }
          : {
              key: "leadSummary.coding.verify.executed",
              values: { cmd: coding.verifyCmd },
            },
      ],
      findings: [],
      attribution: aids,
      trace_ref,
    });
  if (risk) sections.push(risk);
  return {
    type: "lead_summary",
    run_id: run.run_id,
    summary_source: "single_passthrough",
    status: summaryStatusOf(run),
    sections,
    findings: [],
    artifact_refs: [],
  };
}

/** lead 综合进行中的占位 block（前端瞬态·await lead_summarize 期间显示·不持久化·综合回来即替换）。 */
export function buildPendingSummary(run: TeamRun): LeadSummaryBlock {
  return {
    type: "lead_summary",
    run_id: run.run_id,
    summary_source: "pending",
    status: summaryStatusOf(run),
    sections: [],
    findings: [],
    artifact_refs: [],
  };
}

export function parseSynthesisMarkdown(
  md: string,
  runId: string,
  aids: string[],
): KeyedSummarySection[] {
  const trace_ref = { run_id: runId, assignment_ids: aids };
  const sec = (heading: string, body: string): KeyedSummarySection => ({
    id: "llm",
    heading,
    body_richtext: body.trim(),
    findings: [],
    attribution: aids,
    trace_ref,
  });
  if (!/^##\s+/m.test(md)) return [sec("", md.trim())];
  const firstH = md.search(/^##\s+/m);
  const preamble = md.slice(0, firstH).trim();
  const out: KeyedSummarySection[] = preamble ? [sec("", preamble)] : [];
  md.slice(firstH)
    .split(/^##\s+/m)
    .map((s) => s.trim())
    .filter(Boolean)
    .forEach((p) => {
      const nl = p.indexOf("\n");
      out.push(
        sec(
          (nl < 0 ? p : p.slice(0, nl)).trim(),
          nl < 0 ? "" : p.slice(nl + 1),
        ),
      );
    });
  return out;
}

export function buildSynthesisSummary(
  run: TeamRun,
  markdown: string,
): KeyedLeadSummaryBlock {
  const aids = run.members.map((m) => m.assignment_id);
  const st = summaryStatusOf(run);
  return {
    type: "lead_summary",
    run_id: run.run_id,
    summary_source: "lead_synthesis",
    status: st,
    sections: parseSynthesisMarkdown(markdown, run.run_id, aids),
    findings: st.kind !== "all_succeeded" ? buildFailureFindings(run) : [],
    artifact_refs: [],
  };
}

export function buildFallbackRawSummary(run: TeamRun): KeyedLeadSummaryBlock {
  const st = summaryStatusOf(run);
  return {
    type: "lead_summary",
    run_id: run.run_id,
    summary_source: "fallback_raw",
    status: st,
    sections: run.members.map((m) => ({
      id: "fallback" as const,
      heading: "leadSummary.section.fallback" as const,
      heading_values: { name: m.name },
      ...memberSummaryContent(
        m,
        "leadSummary.workerFailure.emptyFallbackTrace",
      ),
      findings: [],
      attribution: [m.assignment_id],
      trace_ref: { run_id: run.run_id, assignment_ids: [m.assignment_id] },
    })),
    findings: st.kind !== "all_succeeded" ? buildFailureFindings(run) : [],
    artifact_refs: [],
  };
}

export function pickDensityTier(n: number): "short" | "brief" | "long" {
  if (n <= 1) return "short";
  if (n <= 6) return "brief";
  return "long";
}

/** 屏⑩ 失败诚实：每 worker 一条 finding（成=done / 败=miss）。 */
export function buildFailureFindings(run: TeamRun): KeyedFinding[] {
  return run.members.map((m) => {
    const failed = m.status === "failed" || m.status === "stopped";
    const reason = failed
      ? (memberFailureReason(m) ?? {
          code: m.status === "stopped" ? "stopped" : "no_final_text",
        })
      : null;
    return {
      status: failed ? "miss" : "done",
      text: failed ? "" : m.sub || m.name,
      ...(reason != null
        ? {
            text_i18n: {
              key: "leadSummary.finding.failure" as const,
              values: { name: m.name },
            },
            failure_reason: reason,
          }
        : {}),
      assignment_id: m.assignment_id,
    };
  });
}

type TrustInput = {
  status: string;
  evidence?: string | null;
  verifier?: string | null;
};
// fail 信号：行首/独立的 failed/error·避开「没有 failed 项」这类否定语境（N3）
const FAIL_HINT = /(^|\n)\s*(\d+\s+failed|error:|FAILED\b|✗|✘)/i;
const CMD_HINT = /(^|\n)\s*(\$|>|\d+\s+(passed|failed)|exit code)/i;

export function criterionTrust(c: TrustInput): KeyedCriterionTrust {
  const ev = (c.evidence ?? "").trim();
  if (c.status === "passed" && (ev === "" || FAIL_HINT.test(ev))) {
    return {
      tier: "unverified",
      degraded: true,
      label: "leadSummary.trust.insufficientEvidence",
    };
  }
  if (c.status === "passed" || c.status === "failed") {
    return CMD_HINT.test(ev)
      ? {
          tier: "command_trace",
          degraded: false,
          label: "leadSummary.trust.commandTrace",
        }
      : {
          tier: "self_report",
          degraded: false,
          label: "leadSummary.trust.workerReport",
        };
  }
  if (c.status === "waived")
    return {
      tier: "unverified",
      degraded: false,
      label: "leadSummary.trust.waived",
    };
  return {
    tier: "unverified",
    degraded: false,
    label: "leadSummary.trust.unverified",
  };
}
