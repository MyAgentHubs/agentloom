import { render, screen } from "@testing-library/react";
import { describe, expect, it, test } from "vitest";
import { LeadSummaryBlock } from "./LeadSummaryBlock";
import type { LeadSummaryBlock as LSB } from "../types/agent";

const lsb = (o: Partial<LSB> = {}): LSB => ({
  type: "lead_summary",
  run_id: "r1",
  summary_source: "single_passthrough",
  status: { kind: "all_succeeded", succeeded_count: 1, total: 1 },
  sections: [
    {
      heading: "查 bind",
      body_richtext: "**bind = sandbox 权限**。",
      findings: [],
      attribution: ["a1"],
      trace_ref: { run_id: "r1", assignment_ids: ["a1"] },
    },
  ],
  findings: [],
  artifact_refs: [],
  ...o,
});

describe("LeadSummaryBlock", () => {
  test("pending summary 渲染紧凑综合中占位且不渲染正常区块入口", () => {
    render(
      <LeadSummaryBlock
        block={lsb({
          summary_source: "pending",
          sections: [
            {
              heading: "不应出现",
              body_richtext: "正式内容还未返回。",
              findings: [],
              attribution: ["a1"],
              trace_ref: { run_id: "r1", assignment_ids: ["a1"] },
            },
          ],
        })}
      />,
    );

    expect(screen.getByText("lead 正在综合各成员产出…")).toBeInTheDocument();
    expect(screen.queryByText("不应出现")).not.toBeInTheDocument();
    expect(
      screen.queryByRole("button", { name: /查看 trace/ }),
    ).not.toBeInTheDocument();
  });

  test("prose 小节正文直接渲染（去卡）", () => {
    render(<LeadSummaryBlock block={lsb()} />);
    expect(screen.getByText(/sandbox 权限/)).toBeInTheDocument();
  });
  test("全成功不显示成败首句", () => {
    render(<LeadSummaryBlock block={lsb()} />);
    expect(screen.queryByText(/没做到|部分完成/)).not.toBeInTheDocument();
  });
  it("完成态 verdict 富文本·无 TOC/origin/查看 trace chrome（旧实现会渲这三种 chrome）", () => {
    // lead_synthesis + 7 个非空 heading 节 → 旧实现 tier=long 会渲 TOC；
    // 跨 3 个 assignment_id → 旧实现会渲 origin「综合自 3 份产出 · 可回溯」按钮；
    // 每个非空节旧实现都渲「查看 trace ›」按钮。去粗框后三者都不应出现。
    const sections = [
      {
        heading: "",
        body_richtext: "结论：先处理全局风险。",
        findings: [],
        attribution: ["a1", "a2", "a3"],
        trace_ref: { run_id: "r1", assignment_ids: ["a1", "a2", "a3"] },
      },
      ...Array.from({ length: 7 }, (_, i) => ({
        heading: `小节 ${i + 1}`,
        body_richtext: `内容 ${i + 1}`,
        findings: [],
        attribution: ["a1", "a2", "a3"],
        trace_ref: { run_id: "r1", assignment_ids: ["a1", "a2", "a3"] },
      })),
    ];
    render(
      <LeadSummaryBlock
        block={lsb({ summary_source: "lead_synthesis", sections })}
      />,
    );
    expect(document.querySelector(".lead-summary__toc")).toBeNull();
    expect(document.querySelector(".lead-summary__origin")).toBeNull();
    expect(screen.queryByRole("button", { name: /查看 trace/ })).toBeNull();
  });
  test("partial 状态首句 = 完成数/总数（opus P1-1）", () => {
    render(
      <LeadSummaryBlock
        block={lsb({
          status: { kind: "partial", succeeded_count: 2, total: 3 },
        })}
      />,
    );
    expect(screen.getByText("部分完成 · 2/3")).toBeInTheDocument();
  });
  test("失败态只给情况和建议，不再渲染点击式补救入口", () => {
    render(
      <LeadSummaryBlock
        block={lsb({
          status: { kind: "partial", succeeded_count: 1, total: 2 },
          findings: [
            {
              status: "miss",
              text: "GLM 4.7：API 额度/频控限制",
              assignment_id: "a1",
            },
          ],
        })}
      />,
    );

    expect(
      screen.getByText(/建议：换一个有额度的模型重派/),
    ).toBeInTheDocument();
    expect(screen.queryByText(/下一步怎么办/)).not.toBeInTheDocument();
    expect(screen.queryByText(/我接手/)).not.toBeInTheDocument();
    expect(screen.queryByText(/从头干净重派/)).not.toBeInTheDocument();
    expect(
      screen.queryByText(/M3 上线|即将到来|敬请期待/),
    ).not.toBeInTheDocument();
  });
  test("普通失败态建议用户换 worker 或回 Normal 处理，不给按钮", () => {
    render(
      <LeadSummaryBlock
        block={lsb({
          status: { kind: "failed", succeeded_count: 0, total: 1 },
          findings: [
            { status: "miss", text: "worker 未返回结果", assignment_id: "a1" },
          ],
        })}
      />,
    );

    expect(
      screen.getByText("worker 调用失败：worker 未返回结果"),
    ).toBeInTheDocument();
    expect(
      screen.getByText(/建议：换一个可用 worker 重派/),
    ).toBeInTheDocument();
    expect(screen.queryByText("没做到")).not.toBeInTheDocument();
    expect(
      screen.queryByRole("button", { name: /重派|接手|下一步/ }),
    ).toBeNull();
  });
  test("结构化 spawn 无 detail 时不渲失败原因行，但保留失败容器与建议", () => {
    const { container } = render(
      <LeadSummaryBlock
        block={lsb({
          status: { kind: "failed", succeeded_count: 0, total: 1 },
          findings: [
            {
              status: "miss",
              text: "",
              text_i18n: {
                key: "leadSummary.finding.failure",
                values: { name: "GLM" },
              },
              failure_reason: { code: "spawn" },
              assignment_id: "a1",
            } as any,
          ],
        })}
      />,
    );

    expect(container.querySelector(".lead-summary__failbox")).not.toBeNull();
    expect(container.querySelector(".lead-summary__failtitle")).toBeNull();
    expect(screen.queryByText(/worker 调用失败/)).toBeNull();
    expect(
      screen.getByText(/建议：换一个可用 worker 重派/),
    ).toBeInTheDocument();
  });

  test("结构化 spawn 有 detail 时与卡片同格式并 humanize detail", () => {
    render(
      <LeadSummaryBlock
        block={lsb({
          status: { kind: "failed", succeeded_count: 0, total: 1 },
          findings: [
            {
              status: "miss",
              text: "",
              text_i18n: {
                key: "leadSummary.finding.failure",
                values: { name: "GLM" },
              },
              failure_reason: {
                code: "spawn",
                detail: "context_budget_exhausted: 拆小任务",
              },
              assignment_id: "a1",
            } as any,
          ],
        })}
      />,
    );

    expect(
      screen.getByText(
        "worker 调用失败 — 上下文用满，已收工——发一条消息可继续: 拆小任务",
      ),
    ).toBeInTheDocument();
  });
  test("P2：单 worker 全失败首句=未完成·非部分完成", () => {
    render(
      <LeadSummaryBlock
        block={lsb({
          status: { kind: "failed", succeeded_count: 0, total: 1 },
        })}
      />,
    );
    expect(screen.getByText("未完成 · 0/1")).toBeInTheDocument();
    expect(screen.queryByText(/部分完成/)).not.toBeInTheDocument();
  });
  test("finding 行按 done/miss 分组「已完成/没做到」", () => {
    render(
      <LeadSummaryBlock
        block={lsb({
          status: { kind: "partial", succeeded_count: 1, total: 2 },
          findings: [
            { status: "done", text: "实现完成", assignment_id: "a1" },
            { status: "miss", text: "typecheck 红", assignment_id: "a2" },
          ],
        })}
      />,
    );
    expect(screen.getByText("已完成")).toBeInTheDocument();
    expect(screen.getByText("没做到")).toBeInTheDocument();
    expect(screen.getByText("typecheck 红")).toBeInTheDocument();
  });
  test("B7 转述行渲染·不说外部验证过（原型 line 1265）", () => {
    render(<LeadSummaryBlock block={lsb()} />);
    expect(screen.queryByText(/外部验证过|系统已复验/)).not.toBeInTheDocument();
  });
  test("heading 空的前导节只渲染 prose，不渲染 h4", () => {
    render(
      <LeadSummaryBlock
        block={lsb({
          sections: [
            {
              heading: "",
              body_richtext: "结论：先处理全局风险。",
              findings: [],
              attribution: ["a1"],
              trace_ref: { run_id: "r1", assignment_ids: ["a1"] },
            },
            {
              heading: "全球",
              body_richtext: "全局影响。",
              findings: [],
              attribution: ["a1"],
              trace_ref: { run_id: "r1", assignment_ids: ["a1"] },
            },
          ],
        })}
      />,
    );
    expect(screen.getByText("结论：先处理全局风险。")).toBeInTheDocument();
    expect(screen.getByRole("heading", { name: "全球" })).toBeInTheDocument();
  });
  test("heading 空的正文 GFM 表格也用 MessageContent 同款 table 包裹并左对齐", () => {
    const md = "| item | count |\n|---|---:|\n| apples | 12 |";
    const { container } = render(
      <LeadSummaryBlock
        block={lsb({
          sections: [
            {
              heading: "",
              body_richtext: md,
              findings: [],
              attribution: ["a1"],
              trace_ref: { run_id: "r1", assignment_ids: ["a1"] },
            },
          ],
        })}
      />,
    );

    expect(
      container.querySelector(".lead-summary__say .mm-table-wrap table"),
    ).not.toBeNull();
    expect(container.querySelectorAll("th")[1]).toHaveStyle({
      textAlign: "left",
    });
    expect(container.querySelectorAll("td")[1]).toHaveStyle({
      textAlign: "left",
    });
  });

  test("新形态 section 按稳定 id/key 翻译标题与正文", () => {
    render(
      <LeadSummaryBlock
        block={lsb({
          sections: [
            {
              id: "changes",
              heading: "leadSummary.section.changes",
              body_i18n: [
                {
                  key: "leadSummary.section.changes.table",
                  values: { rows: "| README.md | — | +1 −0 |" },
                },
              ],
              findings: [],
              attribution: ["a1"],
              trace_ref: { run_id: "r1", assignment_ids: ["a1"] },
            } as any,
          ],
        })}
      />,
    );

    expect(screen.getByRole("heading", { name: "改动" })).toBeInTheDocument();
    expect(screen.getByRole("cell", { name: "README.md" })).toBeInTheDocument();
  });

  // 对抗审补丁：section 正文走 body_i18n（leadSummary.ts::memberSummaryContent 产的
  // budgetExhaustedTrace key），不是 findings 那条路径——两条路径都得核过，别只测 findings
  // 那条就当整个链路都对了。body 绝不能出现「worker 调用失败」字样（那是自相矛盾的话）。
  test("section 的 budget_exhausted 失败原因走卡片同款标签格式，不出现「worker 调用失败」字样", () => {
    render(
      <LeadSummaryBlock
        block={lsb({
          status: { kind: "failed", succeeded_count: 0, total: 1 },
          sections: [
            {
              id: "result",
              heading: "",
              body_i18n: [
                { key: "leadSummary.workerFailure.budgetExhaustedTrace" },
              ],
              failure_reason: {
                code: "budget_exhausted",
                detail:
                  "工人的轮次预算用完了；任务还没做完，但它在正常推进（不是卡住，也没有问题在等回答）。半成品改动已留在项目里；可以再派一单接着干，或把任务拆小。",
              },
              findings: [],
              attribution: ["a1"],
              trace_ref: { run_id: "r1", assignment_ids: ["a1"] },
            } as any,
          ],
        })}
      />,
    );

    expect(
      screen.getByText(
        "工人轮次预算耗尽：任务还在正常推进，不是卡住 — 工人的轮次预算用完了；任务还没做完，但它在正常推进（不是卡住，也没有问题在等回答）。半成品改动已留在项目里；可以再派一单接着干，或把任务拆小。",
      ),
    ).toBeInTheDocument();
    expect(screen.queryByText(/worker 调用失败/)).toBeNull();
  });

  // 第四类（context_exhausted 结构化分流）：跟上面 budget_exhausted 是第二个消费点
  // （section body_i18n=contextExhaustedTrace）——上一刀（budget_exhausted）就是漏了这个
  // 消费点被对抗审打回，本刀务必一起补。body 绝不能出现「worker 调用失败」字样。
  test("section 的 context_exhausted 失败原因走卡片同款标签格式，不出现「worker 调用失败」字样", () => {
    render(
      <LeadSummaryBlock
        block={lsb({
          status: { kind: "failed", succeeded_count: 0, total: 1 },
          sections: [
            {
              id: "result",
              heading: "",
              body_i18n: [
                { key: "leadSummary.workerFailure.contextExhaustedTrace" },
              ],
              failure_reason: {
                code: "context_exhausted",
                detail:
                  "工人的上下文窗口装不下了（单轮 token 预算耗尽）；不是卡住，也没有问题在等回答——但说不清这次是否往前推进过，超限可能在任务一开始就发生。建议把任务拆小，或换一个上下文更大的模型接手；原样重派大概率会再次撞上同一堵墙。",
              },
              findings: [],
              attribution: ["a1"],
              trace_ref: { run_id: "r1", assignment_ids: ["a1"] },
            } as any,
          ],
        })}
      />,
    );

    expect(
      screen.getByText(
        "工人上下文窗口装不下了：单轮 token 预算耗尽，不是卡住 — 工人的上下文窗口装不下了（单轮 token 预算耗尽）；不是卡住，也没有问题在等回答——但说不清这次是否往前推进过，超限可能在任务一开始就发生。建议把任务拆小，或换一个上下文更大的模型接手；原样重派大概率会再次撞上同一堵墙。",
      ),
    ).toBeInTheDocument();
    expect(screen.queryByText(/worker 调用失败/)).toBeNull();
  });

  test("结构化失败按 code 命中原因与额度建议，不复解析中文 reason", () => {
    render(
      <LeadSummaryBlock
        block={lsb({
          status: { kind: "failed", succeeded_count: 0, total: 1 },
          findings: [
            {
              status: "miss",
              text: "",
              text_i18n: {
                key: "leadSummary.finding.failure",
                values: { name: "GLM" },
              },
              failure_reason: { code: "quota" },
              assignment_id: "a1",
            } as any,
          ],
        })}
      />,
    );

    expect(screen.getByText("API 额度/频控限制")).toBeInTheDocument();
    expect(
      screen.getByText(/建议：换一个有额度的模型重派/),
    ).toBeInTheDocument();
  });

  // P2-4（opus 对抗审）：stalled 用独立措辞——绝不能渲出「worker 调用失败：工人停摆……
  // 这不是环境故障」这种自相矛盾的话（诚实停摆不是「调用失败」）。
  test("结构化失败 code=stalled 时走标签 — detail 同源格式", () => {
    render(
      <LeadSummaryBlock
        block={lsb({
          status: { kind: "failed", succeeded_count: 0, total: 1 },
          findings: [
            {
              status: "miss",
              text: "",
              text_i18n: {
                key: "leadSummary.finding.failure",
                values: { name: "GLM" },
              },
              failure_reason: {
                code: "stalled",
                detail:
                  "工人停摆：有问题在等回答，或执行被阻塞（exit status: 3）。这不是环境故障——看它最后的输出。",
              },
              assignment_id: "a1",
            } as any,
          ],
        })}
      />,
    );

    expect(
      screen.getByText(
        "工人停摆：等回答或被阻塞，不是环境故障 — 工人停摆：有问题在等回答，或执行被阻塞（exit status: 3）。这不是环境故障——看它最后的输出。",
      ),
    ).toBeInTheDocument();
    expect(screen.queryByText(/worker 调用失败/)).toBeNull();
  });

  // 本刀（budget_exhausted 结构化分流）：跟上面的 stalled 同族但独立措辞——绝不能渲出
  // 「worker 调用失败：……」或「worker 停摆：……」，预算耗尽仍在推进不是调用失败也不是
  // 停摆。
  test("结构化失败 code=budget_exhausted 时走标签 — detail 同源格式", () => {
    render(
      <LeadSummaryBlock
        block={lsb({
          status: { kind: "failed", succeeded_count: 0, total: 1 },
          findings: [
            {
              status: "miss",
              text: "",
              text_i18n: {
                key: "leadSummary.finding.failure",
                values: { name: "GLM" },
              },
              failure_reason: {
                code: "budget_exhausted",
                detail:
                  "工人的轮次预算用完了；任务还没做完，但它在正常推进（不是卡住，也没有问题在等回答）。半成品改动已留在项目里；可以再派一单接着干，或把任务拆小。",
              },
              assignment_id: "a1",
            } as any,
          ],
        })}
      />,
    );

    expect(
      screen.getByText(
        "工人轮次预算耗尽：任务还在正常推进，不是卡住 — 工人的轮次预算用完了；任务还没做完，但它在正常推进（不是卡住，也没有问题在等回答）。半成品改动已留在项目里；可以再派一单接着干，或把任务拆小。",
      ),
    ).toBeInTheDocument();
    expect(screen.queryByText(/worker 调用失败/)).toBeNull();
    expect(screen.queryByText(/^worker 停摆/)).toBeNull();
  });

  // 第四类（context_exhausted 结构化分流）：跟 stalled/budget_exhausted 同族但独立措辞——
  // 绝不能渲出「worker 调用失败：……」/「worker 停摆：……」/「worker 预算耗尽：……」，单轮
  // 上下文预算耗尽是三者之外的第四种情形。
  test("结构化失败 code=context_exhausted 时走标签 — detail 同源格式", () => {
    render(
      <LeadSummaryBlock
        block={lsb({
          status: { kind: "failed", succeeded_count: 0, total: 1 },
          findings: [
            {
              status: "miss",
              text: "",
              text_i18n: {
                key: "leadSummary.finding.failure",
                values: { name: "GLM" },
              },
              failure_reason: {
                code: "context_exhausted",
                detail:
                  "工人的上下文窗口装不下了（单轮 token 预算耗尽）；不是卡住，也没有问题在等回答——但说不清这次是否往前推进过，超限可能在任务一开始就发生。建议把任务拆小，或换一个上下文更大的模型接手；原样重派大概率会再次撞上同一堵墙。",
              },
              assignment_id: "a1",
            } as any,
          ],
        })}
      />,
    );

    expect(
      screen.getByText(
        "工人上下文窗口装不下了：单轮 token 预算耗尽，不是卡住 — 工人的上下文窗口装不下了（单轮 token 预算耗尽）；不是卡住，也没有问题在等回答——但说不清这次是否往前推进过，超限可能在任务一开始就发生。建议把任务拆小，或换一个上下文更大的模型接手；原样重派大概率会再次撞上同一堵墙。",
      ),
    ).toBeInTheDocument();
    expect(screen.queryByText(/worker 调用失败/)).toBeNull();
    expect(screen.queryByText(/^worker 停摆/)).toBeNull();
    expect(screen.queryByText(/^worker 预算耗尽/)).toBeNull();
  });

  // 人话映射接线（LeadSummaryBlock::failureReasonText detail 路径）：detail 混合文本
  // （诚实正文 + memberFailure.ts::clipResultDetail 展平后的 " · " + 尾部裸码）只
  // humanize 裸码段，诚实正文原样保留。
  test("结构化失败 detail 混合文本：尾部裸码变人话，诚实正文原样保留", () => {
    render(
      <LeadSummaryBlock
        block={lsb({
          status: { kind: "failed", succeeded_count: 0, total: 1 },
          findings: [
            {
              status: "miss",
              text: "",
              text_i18n: {
                key: "leadSummary.finding.failure",
                values: { name: "GLM" },
              },
              failure_reason: {
                code: "context_exhausted",
                detail:
                  "工人的上下文窗口装不下了（单轮 token 预算耗尽）；不是卡住，也没有问题在等回答。 · context_budget_exhausted: 拆小任务 / 换更大上下文的模型",
              },
              assignment_id: "a1",
            } as any,
          ],
        })}
      />,
    );

    expect(
      screen.getByText(
        "工人上下文窗口装不下了：单轮 token 预算耗尽，不是卡住 — 工人的上下文窗口装不下了（单轮 token 预算耗尽）；不是卡住，也没有问题在等回答。 · 上下文用满，已收工——发一条消息可继续: 拆小任务 / 换更大上下文的模型",
      ),
    ).toBeInTheDocument();
    expect(screen.queryByText(/context_budget_exhausted:/)).toBeNull();
  });

  test("结构化失败 detail 含未知裸码 → 原样透传可见（前向兼容·不静默吞）", () => {
    render(
      <LeadSummaryBlock
        block={lsb({
          status: { kind: "failed", succeeded_count: 0, total: 1 },
          findings: [
            {
              status: "miss",
              text: "",
              text_i18n: {
                key: "leadSummary.finding.failure",
                values: { name: "GLM" },
              },
              failure_reason: {
                code: "env",
                detail: "诚实正文在这里。 · some_future_code: xxx",
              },
              assignment_id: "a1",
            } as any,
          ],
        })}
      />,
    );

    expect(
      screen.getByText(/诚实正文在这里。 · some_future_code: xxx/),
    ).toBeInTheDocument();
  });

  test("旧会话 Block fixture：无 id 的中文 heading/body 原样渲染", () => {
    render(
      <LeadSummaryBlock
        block={lsb({
          sections: [
            {
              heading: "改动",
              body_richtext: "旧会话里的原始改动正文。",
              findings: [],
              attribution: ["a1"],
              trace_ref: { run_id: "r1", assignment_ids: ["a1"] },
            },
          ],
        })}
      />,
    );

    expect(screen.getByRole("heading", { name: "改动" })).toBeInTheDocument();
    expect(screen.getByText("旧会话里的原始改动正文。")).toBeInTheDocument();
  });

  test("旧会话 Block fixture：无 id 的未知自由 heading/body 原样渲染", () => {
    render(
      <LeadSummaryBlock
        block={lsb({
          sections: [
            {
              heading: "旧模型自由标题",
              body_richtext: "API 鉴权失败已拼在旧正文里。",
              findings: [],
              attribution: ["a1"],
              trace_ref: { run_id: "r1", assignment_ids: ["a1"] },
            },
          ],
        })}
      />,
    );

    expect(
      screen.getByRole("heading", { name: "旧模型自由标题" }),
    ).toBeInTheDocument();
    expect(
      screen.getByText("API 鉴权失败已拼在旧正文里。"),
    ).toBeInTheDocument();
  });
});
