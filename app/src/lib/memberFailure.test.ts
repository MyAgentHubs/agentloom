import { describe, expect, test } from "vitest";
import {
  memberFailureProgress,
  memberFailureReason,
  memberFailureReasonKey,
  memberFailureReasonText,
} from "./memberFailure";
import type { MemberResult, MemberUnit } from "../types/agent";

const failedMember = (output: string): MemberUnit => ({
  participant_id: "w1",
  assignment_id: "a1",
  task_id: "t1",
  name: "Codex",
  status: "failed",
  sub: "run native worker",
  steps_total: 1,
  steps_done: 0,
  cost_usd: null,
  input_tokens: 0,
  output_tokens: 0,
  failed: true,
  blocks: [
    {
      type: "tool",
      id: "tool-1",
      tool: "agent",
      summary: "",
      card: "command",
      status: "failed",
      exit_code: 1,
      output,
    },
  ],
});

function baseResult(overrides: Partial<MemberResult>): MemberResult {
  return {
    schema_version: 1,
    assignment_id: "a1",
    participant_id: "w1",
    status: "failed",
    changed_files: [],
    anchor: { base_sha: "abc", generated_from: "test" },
    command_evidence: [],
    risk_inputs: {
      files_changed: 0,
      cmd_danger: "low",
      reversibility: "reversible",
    },
    result_source: "raw",
    ...overrides,
  };
}

const failedMemberWithResult = (
  overrides: Partial<MemberResult>,
  blocks: MemberUnit["blocks"] = [],
): MemberUnit => ({
  participant_id: "w1",
  assignment_id: "a1",
  task_id: "t1",
  name: "Codex",
  status: "failed",
  sub: "run native worker",
  steps_total: 1,
  steps_done: 0,
  cost_usd: null,
  input_tokens: 0,
  output_tokens: 0,
  failed: true,
  blocks,
  result: baseResult(overrides),
});

describe("memberFailure", () => {
  const t = ((key: string) => key) as Parameters<
    typeof memberFailureReasonText
  >[1];

  test("spawn without detail has no redundant display text", () => {
    expect(memberFailureReasonText({ code: "spawn" }, t)).toBeNull();
  });

  test("spawn with detail keeps the detail in display text", () => {
    expect(
      memberFailureReasonText({ code: "spawn", detail: "typecheck 红" }, t),
    ).toBe("memberFailure.reason.spawn — typecheck 红");
  });

  test("failure detail uses the shared humanized format", () => {
    expect(
      memberFailureReasonText(
        {
          code: "spawn",
          detail: "context_budget_exhausted: 拆小任务",
        },
        t,
      ),
    ).toBe(
      "memberFailure.reason.spawn — stopReason.contextBudgetExhausted: 拆小任务",
    );
  });

  test("non-spawn failure codes keep their display text", () => {
    expect(memberFailureReasonText({ code: "quota" }, t)).toBe(
      "memberFailure.reason.quota",
    );
  });

  test("classifies Slack MCP OAuth auth evidence as local Codex/MCP auth", () => {
    const member = failedMember(
      'AuthRequired(AuthRequiredError { www_authenticate_header: "Bearer resource_metadata=\\"https://mcp.slack.com/.well-known/oauth-protected-resource\\"" })\nAuth(AuthorizationRequired)',
    );

    expect(memberFailureReason(member)).toEqual({
      code: "local_codex_mcp_auth",
    });
    expect(memberFailureProgress(member)).toBe(
      "memberFailure.reason.localCodexMcpAuth",
    );
  });

  test("keeps invalid API key evidence as API auth failure", () => {
    expect(memberFailureReason(failedMember("invalid api key"))).toEqual({
      code: "auth",
    });
  });

  test("keeps 429 quota evidence as API quota or rate-limit failure", () => {
    expect(
      memberFailureReason(
        failedMember("HTTP 429: rate_limit_exceeded: quota exhausted"),
      ),
    ).toEqual({ code: "quota" });
  });

  test("keeps unknown worker evidence as structured spawn detail", () => {
    expect(
      memberFailureReason(failedMember("typecheck 红\nstack trace")),
    ).toEqual({ code: "spawn", detail: "typecheck 红" });
    expect(memberFailureProgress(failedMember("typecheck 红"))).toBe(
      "memberFailure.reason.spawn",
    );
  });

  test("maps stopped and no-final-text fallbacks to stable keys", () => {
    expect(
      memberFailureProgress({ ...failedMember(""), status: "stopped" }),
    ).toBe("memberDrillIn.status.stopped");
    expect(memberFailureProgress(failedMember(""))).toBe(
      "memberFailure.reason.noFinalText",
    );
    expect(memberFailureReasonKey("overload")).toBe(
      "memberFailure.reason.overload",
    );
  });

  // P1-2（opus 对抗审·结构化判据）：failure_kind="stalled" 是后端写的可信硬判据，必须
  // 无条件胜过任何文本正则——哪怕 blocks/failure_reason 里同时混进了看起来像别的分类
  // 的证据（这里故意在 failure_reason 里塞一段能命中 API_AUTH_HINT 的话 + blocks 里塞
  // 429，两条都不该赢）。这是真优先级冲突 fixture，不是「反正没别的能匹配」的假阳性。
  test("failure_kind=stalled wins over auth/quota text hints in the same failure_reason", () => {
    const member = failedMemberWithResult(
      {
        failure_kind: "stalled",
        failure_reason:
          "工人停摆：有问题在等回答，或执行被阻塞（exit status: 3）。这不是环境故障——看它最后的输出，工具报了 401 unauthorized 但那不是这次收工的原因。",
      },
      [
        {
          type: "tool",
          id: "tool-1",
          tool: "agent",
          summary: "",
          card: "command",
          status: "failed",
          exit_code: 1,
          // blocks 里恰好也有个 429 字样——如果实现退化回「先看 blocks」就会误判成 quota。
          output: "HTTP 429: rate_limit_exceeded",
        },
      ],
    );

    expect(memberFailureReason(member)).toEqual({
      code: "stalled",
      detail: member.result?.failure_reason,
    });
    expect(memberFailureProgress(member)).toBe("memberFailure.reason.stalled");
  });

  // 本刀（budget_exhausted 结构化分流）：failure_kind="budget_exhausted" 是后端写的可信
  // 硬判据（AgentEvent::Blocked.reason == "budget_exhausted_still_progressing"），必须
  // 独立于 "stalled" 分类——即便 failure_reason 文本里同时混进了能命中其他正则的字样，
  // 也不能被那些正则抢先分类，也不能被误落进 stalled 桶。
  test("failure_kind=budget_exhausted is a distinct code from stalled, wins over text hints", () => {
    const member = failedMemberWithResult(
      {
        failure_kind: "budget_exhausted",
        failure_reason:
          "工人的轮次预算用完了；任务还没做完，但它在正常推进（不是卡住，也没有问题在等回答）。半成品改动已留在项目里；可以再派一单接着干，或把任务拆小。",
      },
      [
        {
          type: "tool",
          id: "tool-1",
          tool: "agent",
          summary: "",
          card: "command",
          status: "failed",
          exit_code: 1,
          // blocks 里恰好也有个 429 字样——不该被误判成 quota。
          output: "HTTP 429: rate_limit_exceeded",
        },
      ],
    );

    expect(memberFailureReason(member)).toEqual({
      code: "budget_exhausted",
      detail: member.result?.failure_reason,
    });
    expect(memberFailureReason(member)?.code).not.toBe("stalled");
    expect(memberFailureProgress(member)).toBe(
      "memberFailure.reason.budgetExhausted",
    );
  });

  // 第四类（context_exhausted 结构化分流）：failure_kind="context_exhausted" 是后端写的
  // 可信硬判据（AgentEvent::Blocked.reason == "context_budget_exhausted"，单轮上下文
  // token 预算溢出）——必须独立于 "stalled"/"budget_exhausted" 分类，即便 failure_reason
  // 文本里同时混进了能命中其他正则的字样，也不能被那些正则抢先分类，也不能被误落进
  // stalled/budget_exhausted 任一桶。
  test("failure_kind=context_exhausted is a distinct code from stalled/budget_exhausted, wins over text hints", () => {
    const member = failedMemberWithResult(
      {
        failure_kind: "context_exhausted",
        failure_reason:
          "工人的上下文窗口装不下了（单轮 token 预算耗尽）；不是卡住，也没有问题在等回答——但说不清这次是否往前推进过，超限可能在任务一开始就发生。建议把任务拆小，或换一个上下文更大的模型接手；原样重派大概率会再次撞上同一堵墙。",
      },
      [
        {
          type: "tool",
          id: "tool-1",
          tool: "agent",
          summary: "",
          card: "command",
          status: "failed",
          exit_code: 1,
          // blocks 里恰好也有个 429 字样——不该被误判成 quota。
          output: "HTTP 429: rate_limit_exceeded",
        },
      ],
    );

    expect(memberFailureReason(member)).toEqual({
      code: "context_exhausted",
      detail: member.result?.failure_reason,
    });
    expect(memberFailureReason(member)?.code).not.toBe("stalled");
    expect(memberFailureReason(member)?.code).not.toBe("budget_exhausted");
    expect(memberFailureProgress(member)).toBe(
      "memberFailure.reason.contextExhausted",
    );
  });

  // failure_kind="env" 同理：结构化判据优先，即使 failure_reason 文本本身命中更早的具体
  // 正则（这里故意塞一个能命中 API_AUTH_HINT 的 "401 unauthorized"），也不能被那条正则
  // 抢先分类成 auth——真判据只看 failure_kind 字段。
  //
  // D4（delta 复审·实证反例）：上一版这里塞的文本命不中任何具体正则，删掉
  // `if (m.result?.failure_kind === "env")` 整个分支后会落到函数末尾「resultReason 非空
  // → env」的兜底桶，得到同样的 "env"，测试照样绿——根本没验证 env 分支真的被读取
  // （对照：上面 stalled 那条命中的是 API_AUTH_HINT，是真冲突）。现在塞一句会被
  // API_AUTH_HINT 命中的话，删分支就会落到 auth 而不是 env，测试才会真的变红。
  test("failure_kind=env wins over an earlier-checked text hint (auth) — spoof/branch-deletion resistance", () => {
    const member = failedMemberWithResult({
      failure_kind: "env",
      failure_reason:
        "member 进程失败（exit status: 1）：sdk stderr says 401 unauthorized but this is actually a plain crash, not an auth failure",
    });

    expect(memberFailureReason(member)?.code).toBe("env");
    expect(memberFailureProgress(member)).toBe("memberFailure.reason.env");
  });

  // 没有结构化 failure_kind 时（旧快照 / blocking-write 等其他失败源）仍要有兜底：
  // resultReason 非空、又没命中任何具体正则 → env（不是「spawn + blocks 里翻的第一行」，
  // 那条链只服务于 resultReason 为空、纯靠 blocks 扫描的场景）。
  test("no failure_kind but nonempty failure_reason with empty blocks still falls back to env", () => {
    const member = failedMemberWithResult(
      {
        failure_reason:
          "member.spawnFailed: 找不到二进制 myagent-aarch64-apple-darwin",
      },
      [],
    );

    expect(memberFailureReason(member)).toEqual({
      code: "env",
      detail: member.result?.failure_reason,
    });
    expect(memberFailureProgress(member)).toBe("memberFailure.reason.env");
  });

  // P2-4：env/stalled 的 detail 要截断——字符上界，别把超长 stderr 整段灌进摘要行
  // （4096B 内嵌 stderr 是真实场景，不是假设）。
  test("env/stalled detail is clipped to a char cap on very long text", () => {
    const longLine = "x".repeat(300);
    const member = failedMemberWithResult({
      failure_kind: "env",
      failure_reason: `${longLine}\nsecond line gets truncated away too`,
    });

    const reason = memberFailureReason(member);
    expect(reason?.code).toBe("env");
    expect(reason?.detail?.length).toBeLessThanOrEqual(240);
    expect(reason?.detail).not.toContain("second line");
    expect(reason?.detail?.endsWith("…")).toBe(true);
  });

  // D8（delta 复审·实证反例）：clipResultDetail 曾经只取第一行——P2-6 拼在 failure_reason
  // 第二行的真实 harness 缘由（如「卡点：waiting_for_credentials」）会被整句切没，P2-6 的
  // 收益就只剩 TaskInspector/MemberDrillIn 能看见、lead 摘要层看不到。改成「换行折成
  // · 再截断」——短的多行文本应该被拼成一行，不是被砍掉第二行。
  test("env/stalled detail flattens short multi-line text with a middle dot instead of dropping later lines", () => {
    const member = failedMemberWithResult({
      failure_kind: "stalled",
      failure_reason:
        "工人停摆：有问题在等回答，或执行被阻塞（exit status: 3）。这不是环境故障——看它最后的输出。\n卡点：waiting_for_credentials",
    });

    const reason = memberFailureReason(member);
    expect(reason?.code).toBe("stalled");
    expect(reason?.detail).toContain("waiting_for_credentials");
    expect(reason?.detail).toContain(" · ");
  });

  // P2-5（opus 对抗审）：stopped ≠ 失败——即便 blocks/result 里有看起来很「失败」的证据
  // （这里塞了个带 401 的 failure_reason + failure_kind="env"），stopped 态也必须走中性
  // 文案，绝不进失败分类链（否则会渲出类似「worker 调用失败：我已经改好了三个文件」这种
  // 跟 stopped≠失败的项目约定直接冲突的红味文案）。
  test("stopped members always get the neutral stopped key, never the failure classification chain", () => {
    const member = failedMemberWithResult({
      failure_kind: "env",
      failure_reason: "401 unauthorized: token expired",
    });
    member.status = "stopped";

    expect(memberFailureProgress(member)).toBe("memberDrillIn.status.stopped");
  });

  test("stopped member with plain success-looking text block also stays neutral", () => {
    const member: MemberUnit = {
      ...failedMember(""),
      status: "stopped",
      blocks: [{ type: "text", text: "我已经改好了三个文件。" }],
    };

    expect(memberFailureProgress(member)).toBe("memberDrillIn.status.stopped");
  });
});
