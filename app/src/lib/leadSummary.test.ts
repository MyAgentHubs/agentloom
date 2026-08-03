import { describe, expect, it, test } from "vitest";
import {
  criterionTrust,
  summaryStatusOf,
  memberFinalText,
  buildSinglePassthroughSummary,
  buildCodingVerdictSummary,
  buildPendingSummary,
  buildFailureFindings,
  buildVerdictSections,
  parseSynthesisMarkdown,
  buildSynthesisSummary,
  buildFallbackRawSummary,
  pickDensityTier,
} from "./leadSummary";
import type { TeamRun, MemberUnit } from "../types/agent";

const mem = (o: Partial<MemberUnit> = {}): MemberUnit => ({
  participant_id: "w1",
  assignment_id: "a1",
  task_id: "t1",
  name: "codex",
  status: "done",
  sub: "查 bind 原因",
  steps_total: 1,
  steps_done: 1,
  cost_usd: null,
  input_tokens: 0,
  output_tokens: 0,
  failed: false,
  blocks: [{ type: "text", text: "bind 失败 = sandbox 权限。" }],
  ...o,
});
const run = (members: MemberUnit[]): TeamRun => ({
  run_id: "r1",
  goal: null,
  lead: "Claude",
  members,
});

describe("summaryStatusOf", () => {
  test("全 done → all_succeeded·succeeded=total", () => {
    expect(summaryStatusOf(run([mem()]))).toEqual({
      kind: "all_succeeded",
      succeeded_count: 1,
      total: 1,
    });
  });
  test("1 成 1 败 → partial·succeeded=1", () => {
    expect(
      summaryStatusOf(
        run([
          mem(),
          mem({ assignment_id: "a2", status: "failed", failed: true }),
        ]),
      ),
    ).toEqual({ kind: "partial", succeeded_count: 1, total: 2 });
  });
});

describe("memberFinalText 剥 subtask（codex P1-11）", () => {
  test("开头等于 sub 的文本被剥掉", () => {
    const m = mem({
      sub: "查 bind 原因",
      blocks: [
        { type: "text", text: "查 bind 原因" },
        { type: "text", text: "结论：sandbox 权限。" },
      ],
    });
    expect(memberFinalText(m)).toBe("结论：sandbox 权限。");
  });
});

const c = (
  o: Partial<{
    status: string;
    evidence: string | null;
    verifier: string | null;
  }>,
) => ({ status: "pending", evidence: null, verifier: "npm test", ...o }) as any;

describe("criterionTrust", () => {
  test("passed + 命令输出 → command_trace", () =>
    expect(
      criterionTrust(c({ status: "passed", evidence: "$ npm test\n2 passed" })),
    ).toEqual({
      tier: "command_trace",
      degraded: false,
      label: "leadSummary.trust.commandTrace",
    }));
  test("passed + 纯文字 → self_report", () =>
    expect(
      criterionTrust(c({ status: "passed", evidence: "通过了" })).tier,
    ).toBe("self_report"));
  test("pending → unverified", () =>
    expect(criterionTrust(c({ status: "pending" }))).toEqual({
      tier: "unverified",
      degraded: false,
      label: "leadSummary.trust.unverified",
    }));
  test("passed 无 evidence → 强制降级", () => {
    const t = criterionTrust(c({ status: "passed", evidence: null }));
    expect(t.tier).toBe("unverified");
    expect(t.degraded).toBe(true);
    expect(t.label).toBe("leadSummary.trust.insufficientEvidence");
  });
  test("passed 但 evidence 显示 fail → 强制降级", () => {
    const t = criterionTrust(c({ status: "passed", evidence: "1 failed" }));
    expect(t.tier).toBe("unverified");
    expect(t.degraded).toBe(true);
  });
  test("反例：evidence 含「没有 failed 项」不误判（N3）", () =>
    expect(
      criterionTrust(
        c({
          status: "passed",
          evidence: "$ npm test\n没有 failed 项 · 2 passed",
        }),
      ).tier,
    ).toBe("command_trace"));
});

describe("buildSinglePassthroughSummary", () => {
  test("单 worker → prose 小节 raw 透传", () => {
    const s = buildSinglePassthroughSummary(run([mem()]));
    expect(s.summary_source).toBe("single_passthrough");
    expect(s.sections[0].body_richtext).toContain("sandbox 权限");
    expect(s.sections[0].attribution).toEqual(["a1"]);
    expect(s.findings).toEqual([]);
  });
  // opus P2-1：钉死「summary 块的 status ↔ summaryStatusOf 同源」·防两条路径口径漂移
  test("status 与 summaryStatusOf 同源", () => {
    const r = run([
      mem(),
      mem({ assignment_id: "a2", status: "failed", failed: true }),
    ]);
    expect(buildSinglePassthroughSummary(r).status).toEqual(summaryStatusOf(r));
  });

  test("failed worker 无正文但工具输出是 API limit → 汇总识别为额度/频控，不只说未产出文本", () => {
    const s = buildSinglePassthroughSummary(
      run([
        mem({
          status: "failed",
          failed: true,
          blocks: [
            {
              type: "tool",
              id: "t1",
              tool: "agent",
              summary: "GLM4.7",
              card: "command",
              status: "failed",
              exit_code: 1,
              output:
                "HTTP 429: rate_limit_exceeded: You exceeded your current quota.",
            },
          ],
        }),
      ]),
    );

    expect(s.sections[0]).toEqual(
      expect.objectContaining({
        id: "result",
        body_i18n: [{ key: "leadSummary.workerFailure.trace" }],
        failure_reason: { code: "quota" },
      }),
    );
    expect(s.sections[0].body_richtext).toBeUndefined();
  });

  // P2-4（opus 对抗审）：stalled 不该复用「worker 调用失败」那句模板——那会渲出
  // 「worker 调用失败：工人停摆……这不是环境故障」这种自相矛盾的话。
  test("failed worker 的 failure_kind=stalled → 汇总用独立的 stalledTrace key，不是「调用失败」模板", () => {
    const s = buildSinglePassthroughSummary(
      run([
        mem({
          status: "failed",
          failed: true,
          blocks: [],
          result: {
            schema_version: 1,
            assignment_id: "a1",
            participant_id: "w1",
            status: "failed",
            failure_kind: "stalled",
            failure_reason:
              "工人停摆：有问题在等回答，或执行被阻塞（exit status: 3）。这不是环境故障——看它最后的输出。",
            changed_files: [],
            anchor: { base_sha: "abc", generated_from: "test" },
            command_evidence: [],
            risk_inputs: {
              files_changed: 0,
              cmd_danger: "low",
              reversibility: "reversible",
            },
            result_source: "raw",
          },
        }),
      ]),
    );

    expect(s.sections[0]).toEqual(
      expect.objectContaining({
        id: "result",
        body_i18n: [{ key: "leadSummary.workerFailure.stalledTrace" }],
        failure_reason: { code: "stalled", detail: expect.any(String) },
      }),
    );
  });

  // 对抗审补丁：budget_exhausted 同理——「预算耗尽仍在正常推进」不是「调用失败」，
  // 落进通用 leadSummary.workerFailure.trace 会渲出自相矛盾的话。查表命中独立的
  // budgetExhaustedTrace key，且绝不能退回泛化的 trace key。
  test("failed worker 的 failure_kind=budget_exhausted → 汇总用独立的 budgetExhaustedTrace key，不是「调用失败」模板", () => {
    const s = buildSinglePassthroughSummary(
      run([
        mem({
          status: "failed",
          failed: true,
          blocks: [],
          result: {
            schema_version: 1,
            assignment_id: "a1",
            participant_id: "w1",
            status: "failed",
            failure_kind: "budget_exhausted",
            failure_reason:
              "工人的轮次预算用完了；任务还没做完，但它在正常推进（不是卡住，也没有问题在等回答）。半成品改动已留在项目里；可以再派一单接着干，或把任务拆小。",
            changed_files: [],
            anchor: { base_sha: "abc", generated_from: "test" },
            command_evidence: [],
            risk_inputs: {
              files_changed: 0,
              cmd_danger: "low",
              reversibility: "reversible",
            },
            result_source: "raw",
          },
        }),
      ]),
    );

    expect(s.sections[0]).toEqual(
      expect.objectContaining({
        id: "result",
        body_i18n: [{ key: "leadSummary.workerFailure.budgetExhaustedTrace" }],
        failure_reason: {
          code: "budget_exhausted",
          detail: expect.any(String),
        },
      }),
    );
    expect(s.sections[0].body_i18n?.[0]?.key).not.toBe(
      "leadSummary.workerFailure.trace",
    );
  });

  // 第四类（context_exhausted）：跟 budget_exhausted 一样，这张查表是第二个消费点——上一刀
  // 就是漏了这里被对抗审打回。「上下文窗口装不下」也不是「调用失败」，落进通用
  // leadSummary.workerFailure.trace 会渲出自相矛盾的话。查表命中独立的
  // contextExhaustedTrace key，且绝不能退回泛化的 trace key。
  test("failed worker 的 failure_kind=context_exhausted → 汇总用独立的 contextExhaustedTrace key，不是「调用失败」模板", () => {
    const s = buildSinglePassthroughSummary(
      run([
        mem({
          status: "failed",
          failed: true,
          blocks: [],
          result: {
            schema_version: 1,
            assignment_id: "a1",
            participant_id: "w1",
            status: "failed",
            failure_kind: "context_exhausted",
            failure_reason:
              "工人的上下文窗口装不下了（单轮 token 预算耗尽）；不是卡住，也没有问题在等回答——但说不清这次是否往前推进过，超限可能在任务一开始就发生。建议把任务拆小，或换一个上下文更大的模型接手；原样重派大概率会再次撞上同一堵墙。",
            changed_files: [],
            anchor: { base_sha: "abc", generated_from: "test" },
            command_evidence: [],
            risk_inputs: {
              files_changed: 0,
              cmd_danger: "low",
              reversibility: "reversible",
            },
            result_source: "raw",
          },
        }),
      ]),
    );

    expect(s.sections[0]).toEqual(
      expect.objectContaining({
        id: "result",
        body_i18n: [{ key: "leadSummary.workerFailure.contextExhaustedTrace" }],
        failure_reason: {
          code: "context_exhausted",
          detail: expect.any(String),
        },
      }),
    );
    expect(s.sections[0].body_i18n?.[0]?.key).not.toBe(
      "leadSummary.workerFailure.trace",
    );
    expect(s.sections[0].body_i18n?.[0]?.key).not.toBe(
      "leadSummary.workerFailure.budgetExhaustedTrace",
    );
  });

  it("buildSinglePassthroughSummary 出 结果 + 改动表 + 验证 section（派生事实）", () => {
    const run = {
      run_id: "r1",
      goal: { goal: "加日期" },
      lead: "Claude",
      members: [
        {
          assignment_id: "a1",
          name: "DeepSeekFlash",
          sub: "改 README",
          status: "done",
          blocks: [{ type: "text", text: "已在 README 末尾加上今天日期。" }],
          steps_done: 2,
          steps_total: 2,
          input_tokens: 0,
          output_tokens: 0,
          failed: false,
          result: {
            changed_files: [{ path: "README.md", insertions: 5, deletions: 0 }],
            command_evidence: [
              {
                cmd: "date +%F",
                exit_code: 0,
                status: "passed",
                source_provider: "deepseek",
              },
            ],
            risks: [],
          },
        },
      ],
    } as any;
    const sum = buildSinglePassthroughSummary(run);
    expect(sum.sections).toEqual(
      expect.arrayContaining([
        expect.objectContaining({
          id: "changes",
          heading: "leadSummary.section.changes",
        }),
        expect.objectContaining({
          id: "verify",
          heading: "leadSummary.section.verify",
        }),
      ]),
    );
    const change = sum.sections.find((s) => s.id === "changes");
    expect(change?.body_i18n?.[0].key).toBe(
      "leadSummary.section.changes.table",
    );
    expect(change?.body_i18n?.[0].values?.rows).toContain("README.md");
    expect(change?.body_i18n?.[0].values?.rows).toContain("+5");
    const result = sum.sections.find((s) => s.id === "result");
    expect(result?.body_richtext).toContain("已在 README 末尾加上今天日期");
  });
});

describe("buildVerdictSections 验证节过滤工具噪音（块B·退出码 -- 修）", () => {
  it("验证节只显有退出码的真验证命令·滤掉 WebSearch 等无退出码工具噪音", () => {
    const run = {
      run_id: "r1",
      goal: { goal: "检索新闻" },
      lead: "Claude",
      members: [
        {
          assignment_id: "a1",
          name: "DeepSeekFlash",
          sub: "检索",
          status: "done",
          blocks: [{ type: "text", text: "汇总完成。" }],
          steps_done: 4,
          steps_total: 4,
          input_tokens: 0,
          output_tokens: 0,
          failed: false,
          result: {
            changed_files: [],
            command_evidence: [
              {
                cmd: "top news today June 16 2026",
                exit_code: null,
                status: "done",
                source_provider: "websearch",
              },
              {
                cmd: "npm test",
                exit_code: 0,
                status: "passed",
                source_provider: "shell",
              },
            ],
            risks: [],
          },
        },
      ],
    } as any;
    const sum = buildSinglePassthroughSummary(run);
    const verify = sum.sections.find((s) => s.id === "verify");
    expect(verify?.body_i18n).toEqual([
      {
        key: "leadSummary.section.verify.command",
        values: { cmd: "npm test", code: 0 },
      },
    ]);
  });

  it("验证证据全是无退出码工具噪音 → 不出验证节", () => {
    const run = {
      run_id: "r2",
      goal: null,
      lead: "Claude",
      members: [
        {
          assignment_id: "a1",
          name: "X",
          sub: "检索",
          status: "done",
          blocks: [],
          steps_done: 1,
          steps_total: 1,
          input_tokens: 0,
          output_tokens: 0,
          failed: false,
          result: {
            changed_files: [],
            command_evidence: [
              {
                cmd: "search foo",
                exit_code: null,
                status: "done",
                source_provider: "websearch",
              },
            ],
            risks: [],
          },
        },
      ],
    } as any;
    const sum = buildSinglePassthroughSummary(run);
    expect(sum.sections.find((s) => s.id === "verify")).toBeUndefined();
  });
});

// D7 措辞更正（delta 复审终轮）：risk 段之前不分青红皂白把 member.result.risks 全部
// 原样糊进用户摘要——排查用的内部 risk（比如 stalled_narrative_on_clean_exit，措辞是
// 「叙事事件」「契约退出码语义」这类术语黑话、还是后端硬编码中文，en locale 下也会露
// 中文）就这样出现在了成功 run 的摘要里。改成白名单制：只有 transient_error /
// git_write_blocked 这两个已知要给用户看的 id 才渲进摘要，其余留在 risks 数组/DB 里
// 供排查，不进这条 UI 渲染路径。
describe("buildVerdictSections 风险段白名单（D7 措辞更正·delta 复审终轮）", () => {
  it("白名单外的 risk（如 stalled_narrative_on_clean_exit）不进风险段，白名单内的 transient_error 仍照常外显", () => {
    const sections = buildVerdictSections(
      run([
        mem({
          result: {
            changed_files: [],
            command_evidence: [],
            risks: [
              {
                id: "transient_error",
                text: "worker 中途报过一次错，但最终完成了。",
              },
              {
                id: "stalled_narrative_on_clean_exit",
                text: "队员进程干净退出（exit 0），但过程里见过 Blocked/NeedsDecision 叙事事件（harness 契约退出码 3/4 语义）——终态仍按 Done 处理（维持既有基线行为），这里留痕供排查。",
              },
            ],
          } as any,
        }),
      ]),
    );

    const riskSection = sections.find((s) => s.id === "risk");
    expect(riskSection).toBeDefined();
    expect(riskSection?.body_richtext).toContain(
      "worker 中途报过一次错，但最终完成了。",
    );
    expect(riskSection?.body_richtext).not.toContain(
      "stalled_narrative_on_clean_exit",
    );
    expect(riskSection?.body_richtext).not.toContain("叙事事件");
    expect(riskSection?.body_richtext).not.toContain("干净退出");
  });

  it("risks 里只有白名单外的 id 时，不出「风险」段——不是渲一个只有过滤剩余内容的空段", () => {
    const sections = buildVerdictSections(
      run([
        mem({
          result: {
            changed_files: [],
            command_evidence: [],
            risks: [
              {
                id: "stalled_narrative_on_clean_exit",
                text: "队员进程干净退出（exit 0），但过程里见过 Blocked/NeedsDecision 叙事事件——这里留痕供排查。",
              },
            ],
          } as any,
        }),
      ]),
    );

    expect(sections.find((s) => s.id === "risk")).toBeUndefined();
  });
});

describe("buildCodingVerdictSummary（路 B coding 闭环·块 B）", () => {
  it("结果(模板)+改动+coding验证(非派单期)+风险", () => {
    const run = {
      run_id: "rc",
      goal: { goal: "改 README" },
      lead: "Claude",
      members: [
        {
          assignment_id: "a1",
          name: "DeepSeekFlash",
          sub: "改 README",
          status: "done",
          blocks: [{ type: "text", text: "已加日期。" }],
          steps_done: 3,
          steps_total: 3,
          input_tokens: 0,
          output_tokens: 0,
          failed: false,
          result: {
            changed_files: [{ path: "README.md", insertions: 5, deletions: 0 }],
            command_evidence: [
              {
                cmd: "OLD 派单期命令",
                exit_code: 0,
                status: "passed",
                source_provider: "x",
              },
            ],
            risks: [],
          },
        },
      ],
    } as any;
    const sum = buildCodingVerdictSummary(run, {
      verifyCmd: "npm test",
      lastVerdict: "passed",
      phase: "applied",
    });
    const verify = sum.sections.find((s) => s.id === "verify");
    expect(verify?.body_i18n).toEqual([
      {
        key: "leadSummary.coding.verify.verdict",
        values: { cmd: "npm test", verdict: "passed" },
      },
    ]);
    const change = sum.sections.find((s) => s.id === "changes");
    expect(change?.body_i18n?.[0].values?.rows).toContain("README.md");
    expect(change?.body_i18n?.[0].values?.rows).toContain("+5");
    const result = sum.sections.find((s) => s.id === "result");
    expect(result?.body_richtext).toContain("已加日期");
    expect(result?.body_i18n).toEqual([{ key: "leadSummary.coding.applied" }]);
    expect(sum.summary_source).toBe("single_passthrough");
  });

  it("landing_blocked 保留 worker 成功状态，只提示 Review 不触发失败重派", () => {
    const r = run([mem({ blocks: [{ type: "text", text: "已完成改动。" }] })]);

    const sum = buildCodingVerdictSummary(r, {
      verifyCmd: "",
      lastVerdict: null,
      phase: "landing_blocked",
    });

    expect(sum.status).toEqual(summaryStatusOf(r));
    expect(sum.sections[0].body_i18n).toEqual([
      { key: "leadSummary.coding.landingBlocked" },
    ]);
    expect(sum.sections.find((s) => s.id === "verify")).toBeUndefined();
  });
});

describe("buildPendingSummary", () => {
  test("构造 lead 综合进行中的瞬态占位 block", () => {
    const r = run([
      mem(),
      mem({ assignment_id: "a2", participant_id: "w2", name: "gemini" }),
    ]);
    const s = buildPendingSummary(r);
    expect(s.summary_source).toBe("pending");
    expect(s.sections).toEqual([]);
    expect(s.run_id).toBe("r1");
    expect(s.status).toEqual(summaryStatusOf(r));
  });
});

describe("parseSynthesisMarkdown", () => {
  test("首个 ## 前的结论先行段保留为 heading 空的前导节", () => {
    const sections = parseSynthesisMarkdown(
      "结论：先上线全局修复。\n\n## 全球\n全局影响。\n\n## 国内\n国内影响。",
      "r1",
      ["a1", "a2"],
    );
    expect(sections.every((section) => section.id === "llm")).toBe(true);
    expect(sections[0].heading).toBe("");
    expect(sections[0].body_richtext).toBe("结论：先上线全局修复。");
    expect(sections[1].heading).toBe("全球");
    expect(sections[1].body_richtext).toBe("全局影响。");
  });

  test("零 ## 时整篇当 heading 空的 preamble", () => {
    const sections = parseSynthesisMarkdown("结论：无结构输出。", "r1", ["a1"]);
    expect(sections).toHaveLength(1);
    expect(sections[0].heading).toBe("");
    expect(sections[0].body_richtext).toBe("结论：无结构输出。");
  });

  test("空 preamble 不产空节", () => {
    const sections = parseSynthesisMarkdown("## 全球\n全局影响。", "r1", [
      "a1",
    ]);
    expect(sections).toHaveLength(1);
    expect(sections[0].heading).toBe("全球");
  });

  test("非 ## 标题层级不崩并整体降级为 preamble", () => {
    const sections = parseSynthesisMarkdown(
      "# 结论\n一级标题。\n\n### 细节\n三级标题。",
      "r1",
      ["a1"],
    );
    expect(sections).toHaveLength(1);
    expect(sections[0].heading).toBe("");
    expect(sections[0].body_richtext).toContain("# 结论");
    expect(sections[0].body_richtext).toContain("### 细节");
  });
});

describe("buildSynthesisSummary", () => {
  test("多 worker 综合来自 parse 结果并标记 lead_synthesis", () => {
    const r = run([
      mem(),
      mem({ assignment_id: "a2", participant_id: "w2", name: "gemini" }),
    ]);
    const s = buildSynthesisSummary(r, "结论：合并。\n\n## 全球\n全局。");
    expect(s.summary_source).toBe("lead_synthesis");
    expect(s.sections[0].id).toBe("llm");
    expect(s.sections[1].id).toBe("llm");
    expect(s.sections[0].heading).toBe("");
    expect(s.sections[1].heading).toBe("全球");
    expect(s.sections[1].trace_ref.assignment_ids).toEqual(["a1", "a2"]);
    expect(s.findings).toEqual([]);
  });

  test("部分失败时 findings 同 buildFailureFindings 规则", () => {
    const r = run([
      mem(),
      mem({ assignment_id: "a2", status: "failed", failed: true }),
    ]);
    const s = buildSynthesisSummary(r, "## 结论\n完成部分。");
    expect(s.summary_source).toBe("lead_synthesis");
    expect(s.findings).toEqual(buildFailureFindings(r));
  });
});

describe("buildFallbackRawSummary", () => {
  test("综合失败时每 worker 一节透传 memberFinalText", () => {
    const r = run([
      mem({
        assignment_id: "a1",
        name: "codex",
        blocks: [
          { type: "text", text: "查 bind 原因" },
          { type: "text", text: "codex 真答案" },
        ],
      }),
      mem({
        assignment_id: "a2",
        participant_id: "w2",
        name: "gemini",
        blocks: [{ type: "text", text: "gemini 真答案" }],
      }),
    ]);
    const s = buildFallbackRawSummary(r);
    expect(s.summary_source).toBe("fallback_raw");
    expect(s.sections).toHaveLength(2);
    expect(s.sections[0]).toEqual(
      expect.objectContaining({
        id: "fallback",
        heading: "leadSummary.section.fallback",
        heading_values: { name: "codex" },
      }),
    );
    expect(s.sections[0].body_richtext).toBe("codex 真答案");
    expect(s.sections[1].body_richtext).toBe("gemini 真答案");
  });
});

describe("pickDensityTier", () => {
  test("按 sections.length 切三档密度", () => {
    expect(pickDensityTier(1)).toBe("short");
    expect(pickDensityTier(4)).toBe("brief");
    expect(pickDensityTier(8)).toBe("long");
  });
});

describe("buildFailureFindings（屏⑩ 已完成/没做到）", () => {
  test("成→done·败→miss·带归属", () => {
    const fs = buildFailureFindings(
      run([
        mem(),
        mem({
          assignment_id: "a2",
          name: "deepseek",
          status: "failed",
          failed: true,
        }),
      ]),
    );
    expect(fs).toContainEqual(
      expect.objectContaining({ status: "done", assignment_id: "a1" }),
    );
    expect(fs).toContainEqual(
      expect.objectContaining({ status: "miss", assignment_id: "a2" }),
    );
  });
  // opus P1-2·缺口7：Phase 1 无 LLM·finding 文案 = 子任务名（m.sub）·非原型屏⑩ 成果叙事·钉死防误当成果描述
  test("finding text = 子任务名 sub（降级·非成果叙事）", () => {
    const fs = buildFailureFindings(
      run([mem({ assignment_id: "a1", sub: "实现 mood-record" })]),
    );
    expect(fs[0].text).toBe("实现 mood-record");
  });

  test("failed worker 有 API limit 证据时 finding 用可识别失败原因替代长子任务", () => {
    const fs = buildFailureFindings(
      run([
        mem({
          name: "GLM4.7",
          sub: "使用 GLM4.7 模型执行。目标：按 B 完整方向补全 Clash 配置。",
          status: "failed",
          failed: true,
          blocks: [
            {
              type: "tool",
              id: "t1",
              tool: "agent",
              summary: "GLM4.7",
              card: "command",
              status: "failed",
              exit_code: 1,
              output: "429 Too Many Requests: rate limit exceeded",
            },
          ],
        }),
      ]),
    );

    expect(fs[0]).toEqual(
      expect.objectContaining({
        text: "",
        text_i18n: {
          key: "leadSummary.finding.failure",
          values: { name: "GLM4.7" },
        },
        failure_reason: { code: "quota" },
      }),
    );
  });

  // D1（delta 复审·实证反例）：buildFailureFindings 直接调 memberFailureReason（不经过
  // memberFailureProgress），旧实现里早退只挡在 memberFailureProgress——一个被用户停掉、
  // 汇报「我已经改好了三个文件」的 stopped 成员会被渲成「GLM 失败：我已经改好了三个
  // 文件。」，跟 stopped ≠ 失败的项目约定直接冲突。下沉到 memberFailureReason 本身后，
  // 这里也该拿到中性的 stopped code，不管 blocks 里有没有看起来像成果/失败的文本。
  test("stopped worker 即便有看似成果的文本，finding 也该是中性 stopped code，不是「失败」", () => {
    const fs = buildFailureFindings(
      run([
        mem({
          name: "GLM",
          status: "stopped",
          failed: false,
          blocks: [{ type: "text", text: "我已经改好了三个文件。" }],
        }),
      ]),
    );

    expect(fs[0]).toEqual(
      expect.objectContaining({
        text: "",
        text_i18n: {
          key: "leadSummary.finding.failure",
          values: { name: "GLM" },
        },
        failure_reason: { code: "stopped" },
      }),
    );
  });

  // 同一个反例的第二半：stopped 成员即便 result 里带着 failure_kind="env" 的诊断信息
  // （比如上一轮 auth 重试失败留下的残留），也不该被渲成「env 环境故障」。
  test("stopped worker 即便 result 带 failure_kind=env 残留，finding 也该是中性 stopped code", () => {
    const fs = buildFailureFindings(
      run([
        mem({
          name: "GLM",
          status: "stopped",
          failed: false,
          blocks: [],
          result: {
            schema_version: 1,
            assignment_id: "a1",
            participant_id: "w1",
            status: "stopped",
            failure_kind: "env",
            failure_reason: "401 unauthorized: token expired",
            changed_files: [],
            anchor: { base_sha: "abc", generated_from: "test" },
            command_evidence: [],
            risk_inputs: {
              files_changed: 0,
              cmd_danger: "low",
              reversibility: "reversible",
            },
            result_source: "raw",
          },
        }),
      ]),
    );

    expect(fs[0].failure_reason).toEqual({ code: "stopped" });
  });
});
