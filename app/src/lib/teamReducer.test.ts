import { describe, expect, it, test } from "vitest";
import {
  applyMemberEvent as reduceMemberEvent,
  applyTeamEvent as reduceTeamEvent,
  compareErrorBlocks,
  isDispatchEnvelope,
  isOrchestratedDispatch,
  isTeamRunComplete,
  teamRunToBlock,
} from "./teamReducer";
import type {
  AgentEventEnvelope,
  Block,
  MemberUnit,
  TeamRun,
} from "../types/agent";

const applyMemberEvent = (
  member: MemberUnit,
  event: AgentEventEnvelope,
  errorPrefix = "错误：",
) => reduceMemberEvent(member, event, errorPrefix);

const applyTeamEvent = (
  run: TeamRun | null,
  event: AgentEventEnvelope,
  errorPrefix = "错误：",
) => reduceTeamEvent(run, event, errorPrefix);

// R1：派单维度在嵌套 dispatch 下
const env = (
  dispatch: AgentEventEnvelope["dispatch"],
  event: Record<string, unknown>,
): AgentEventEnvelope =>
  ({ session_id: "s", dispatch, ...event }) as AgentEventEnvelope;

const mkRun = (over: Partial<TeamRun> = {}): TeamRun => ({
  run_id: "r1",
  goal: null,
  lead: null,
  members: [
    {
      participant_id: "w1",
      assignment_id: "a1",
      task_id: "t1",
      name: "w1",
      status: "done",
      sub: "",
      steps_total: 1,
      steps_done: 1,
      cost_usd: null,
      input_tokens: 0,
      output_tokens: 0,
      failed: false,
      blocks: [],
    },
    {
      participant_id: "w2",
      assignment_id: "a2",
      task_id: "t2",
      name: "w2",
      status: "running",
      sub: "",
      steps_total: 2,
      steps_done: 1,
      cost_usd: null,
      input_tokens: 0,
      output_tokens: 0,
      failed: false,
      blocks: [],
    },
  ],
  ...over,
});

describe("teamReducer", () => {
  test("normal envelope（无 dispatch）不算 dispatch", () => {
    expect(
      isDispatchEnvelope(env(undefined, { kind: "text_delta", text: "hi" })),
    ).toBe(false);
  });

  test("goal_declared 收进 TeamRun.goal（方案 A）", () => {
    let run: TeamRun | null = null;
    run = applyTeamEvent(
      run,
      env(
        { run_id: "r1" },
        {
          kind: "goal_declared",
          goal: "实现 stage 2",
          status: "frozen",
          lead: "Claude",
          criteria: [
            {
              id: "ac1",
              claim: "测试通过",
              verifier: null,
              evidence: null,
              status: "pending",
              scope: "task",
            },
          ],
        },
      ),
    );
    expect(run!.run_id).toBe("r1");
    expect(run!.goal?.goal).toBe("实现 stage 2");
    expect(run!.goal?.status).toBe("frozen");
    expect(run!.goal?.criteria).toHaveLength(1);
    expect(run!.lead).toBe("Claude");
    expect(run!.members).toHaveLength(0); // goal 事件不建队员
  });

  test("非 goal 派单事件保留 TeamRun.lead", () => {
    let run: TeamRun = mkRun({ members: [], lead: "Claude" });

    run = applyTeamEvent(
      run,
      env(
        {
          run_id: "r1",
          assignment_id: "a1",
          task_id: "t1",
          origin_participant_id: "worker-1",
          status_transition: "dispatched",
        },
        { kind: "text_delta", text: "做 X" },
      ),
    );

    expect(run.lead).toBe("Claude");
  });

  test("开场派单事件的 task_pack 存进 member.taskPack（#3）", () => {
    const run = applyTeamEvent(
      null,
      env(
        {
          run_id: "r1",
          assignment_id: "a1",
          origin_participant_id: "w1",
          status_transition: "dispatched",
          task_pack: "## 总目标\n看下 X\n## 你的子任务\n看下 X\n",
        },
        { kind: "text_delta", text: "看下 X" },
      ),
    );
    const m = run.members.find((x) => x.assignment_id === "a1")!;
    expect(m.taskPack).toContain("总目标");
    expect(m.sub).toBe("看下 X"); // 卡片仍短 subtask·不受影响
    expect(m.blocks).toEqual([]); // 题面只进 sub，不重复进入「过程」
  });

  test("dispatched 题面 delta 只填 sub，后续普通 delta 正常进入 blocks", () => {
    let run: TeamRun | null = null;
    run = applyTeamEvent(
      run,
      env(
        {
          run_id: "r1",
          assignment_id: "a1",
          origin_participant_id: "w1",
          status_transition: "dispatched",
        },
        { kind: "text_delta", text: "检索 X" },
      ),
    );
    const dispatched = run.members.find((x) => x.assignment_id === "a1")!;
    expect(dispatched.sub).toBe("检索 X");
    expect(dispatched.blocks).toEqual([]);

    run = applyTeamEvent(
      run,
      env(
        { run_id: "r1", assignment_id: "a1" },
        { kind: "text_delta", text: "答案正文" },
      ),
    );
    expect(run.members.find((x) => x.assignment_id === "a1")!.blocks).toEqual([
      { type: "text", text: "答案正文" },
    ]);
  });

  test("队员首见时 name 优先使用 member_name 且 participant_id 保留 worker id", () => {
    const dispatch = {
      run_id: "r1",
      assignment_id: "a1",
      task_id: "t1",
      origin_participant_id: "worker-1",
      member_name: "Claude",
      status_transition: "dispatched",
    } as AgentEventEnvelope["dispatch"] & { member_name: string };

    const run = applyTeamEvent(
      null,
      env(dispatch, { kind: "text_delta", text: "做 X" }),
    );

    expect(run.members[0].participant_id).toBe("worker-1");
    expect(run.members[0].name).toBe("Claude");
  });

  test("派单流分组成队员单元 + 进度 derived + token 累加", () => {
    let run: TeamRun | null = null;
    const stream: AgentEventEnvelope[] = [
      env(
        {
          run_id: "r1",
          assignment_id: "a1",
          task_id: "t1",
          origin_participant_id: "worker-1",
          status_transition: "dispatched",
        },
        { kind: "text_delta", text: "做 X" },
      ),
      env(
        { run_id: "r1", assignment_id: "a1" },
        {
          kind: "tool_started",
          id: "a1-t0",
          tool: "command",
          summary: "step 1",
          card: "command",
        },
      ),
      env(
        { run_id: "r1", assignment_id: "a1" },
        {
          kind: "tool_completed",
          id: "a1-t0",
          status: "ok",
          exit_code: 0,
          output: "ok",
        },
      ),
      env(
        { run_id: "r1", assignment_id: "a1", status_transition: "done" },
        {
          kind: "completed",
          cost_usd: 0.12,
          input_tokens: 8000,
          output_tokens: 1500,
          final_text: null,
        },
      ),
    ];
    for (const e of stream) run = applyTeamEvent(run, e);
    const m = run!.members[0];
    expect(m.name).toBe("worker-1");
    expect(m.sub).toBe("做 X");
    expect(m.status).toBe("done");
    expect(m.steps_total).toBe(1);
    expect(m.steps_done).toBe(1);
    expect(m.cost_usd).toBe(0.12);
    expect(m.input_tokens).toBe(8000);
    expect(m.output_tokens).toBe(1500);
    expect(m.failed).toBe(false);
    expect(m.blocks.filter((b) => b.type === "tool")).toHaveLength(1);
  });

  test("completed.final_text 在无流式答案时补成队员 text block", () => {
    let run: TeamRun | null = null;
    run = applyTeamEvent(
      run,
      env(
        {
          run_id: "r1",
          assignment_id: "a1",
          origin_participant_id: "w1",
          status_transition: "dispatched",
        },
        { kind: "text_delta", text: "检索X" },
      ),
    );
    run = applyTeamEvent(
      run,
      env(
        { run_id: "r1", assignment_id: "a1", status_transition: "done" },
        {
          kind: "completed",
          cost_usd: null,
          input_tokens: 0,
          output_tokens: 0,
          final_text: "答案正文",
        },
      ),
    );

    const m = run!.members.find((x) => x.assignment_id === "a1")!;
    expect(m.blocks).toContainEqual({ type: "text", text: "答案正文" });
  });

  test("completed.final_text 在完全零流式（member 首个事件即 completed）时也补进 blocks", () => {
    // TaskInspector UX①的兜底依据：worker 若一条 text_delta 都没吐（如极短任务、直接终态），
    // done 后 blocks 里仍应能看到 final_text，而不是空白。
    let run: TeamRun | null = null;
    run = applyTeamEvent(
      run,
      env(
        {
          run_id: "r1",
          assignment_id: "a1",
          origin_participant_id: "w1",
          status_transition: "done",
        },
        {
          kind: "completed",
          cost_usd: null,
          input_tokens: 0,
          output_tokens: 0,
          final_text: "零流式的最终答案",
        },
      ),
    );

    const m = run!.members.find((x) => x.assignment_id === "a1")!;
    expect(m.sub).toBe("");
    expect(m.blocks).toContainEqual({
      type: "text",
      text: "零流式的最终答案",
    });
  });

  test("completed.final_text 在已有流式答案时不重复补 text block", () => {
    let run: TeamRun | null = null;
    const stream: AgentEventEnvelope[] = [
      env(
        {
          run_id: "r1",
          assignment_id: "a1",
          origin_participant_id: "w1",
          status_transition: "dispatched",
        },
        { kind: "text_delta", text: "检索X" },
      ),
      env(
        { run_id: "r1", assignment_id: "a1" },
        { kind: "text_delta", text: "流式答案" },
      ),
      env(
        { run_id: "r1", assignment_id: "a1", status_transition: "done" },
        {
          kind: "completed",
          cost_usd: null,
          input_tokens: 0,
          output_tokens: 0,
          final_text: "流式答案",
        },
      ),
    ];
    for (const e of stream) run = applyTeamEvent(run, e);

    const text = run!.members
      .find((x) => x.assignment_id === "a1")!
      .blocks.filter((b) => b.type === "text")
      .map((b) => b.text)
      .join("");
    expect(text.match(/流式答案/g)).toHaveLength(1);
  });

  test("completed.final_text 为空时不新增 text block", () => {
    let run: TeamRun | null = null;
    run = applyTeamEvent(
      run,
      env(
        {
          run_id: "r1",
          assignment_id: "a1",
          origin_participant_id: "w1",
          status_transition: "dispatched",
        },
        { kind: "text_delta", text: "检索X" },
      ),
    );
    run = applyTeamEvent(
      run,
      env(
        { run_id: "r1", assignment_id: "a1", status_transition: "done" },
        {
          kind: "completed",
          cost_usd: null,
          input_tokens: 0,
          output_tokens: 0,
          final_text: "",
        },
      ),
    );

    const m = run!.members.find((x) => x.assignment_id === "a1")!;
    expect(m.blocks.filter((b) => b.type === "text")).toEqual([]);
  });

  test("failed 终态 → 队员 failed=true + status=failed（§三.3/二.4）", () => {
    let run: TeamRun | null = null;
    run = applyTeamEvent(
      run,
      env(
        {
          run_id: "r1",
          assignment_id: "a1",
          origin_participant_id: "w1",
          status_transition: "dispatched",
        },
        { kind: "text_delta", text: "X" },
      ),
    );
    run = applyTeamEvent(
      run,
      env(
        {
          run_id: "r1",
          assignment_id: "a1",
          status_transition: "failed",
        },
        { kind: "text_delta", text: "失败" },
      ),
    );
    expect(run!.members[0].status).toBe("failed");
    expect(run!.members[0].failed).toBe(true);
  });

  test("dispatch error 事件写入 member blocks，失败汇总/任务条可读取真实错误", () => {
    let run: TeamRun | null = null;
    run = applyTeamEvent(
      run,
      env(
        {
          run_id: "r1",
          assignment_id: "a1",
          origin_participant_id: "w1",
          status_transition: "dispatched",
        },
        { kind: "text_delta", text: "用 GLM 补配置" },
      ),
    );
    run = applyTeamEvent(
      run,
      env(
        {
          run_id: "r1",
          assignment_id: "a1",
          status_transition: "failed",
        },
        {
          kind: "error",
          message: "429 Too Many Requests: rate limit exceeded",
        },
      ),
      "Error: ",
    );

    const texts = run!.members[0].blocks
      .filter((b) => b.type === "text")
      .map((b) => b.text);
    expect(texts.join("\n")).toContain(
      "Error: 429 Too Many Requests: rate limit exceeded",
    );
  });

  test("两队员按 assignment 分组、needs_input/running 正确", () => {
    let run: TeamRun | null = null;
    const stream: AgentEventEnvelope[] = [
      env(
        {
          run_id: "r1",
          assignment_id: "a1",
          origin_participant_id: "w1",
          status_transition: "dispatched",
        },
        { kind: "text_delta", text: "X" },
      ),
      env(
        {
          run_id: "r1",
          assignment_id: "a2",
          origin_participant_id: "w2",
          status_transition: "dispatched",
        },
        { kind: "text_delta", text: "Y" },
      ),
      env(
        {
          run_id: "r1",
          assignment_id: "a2",
          status_transition: "needs_input",
        },
        { kind: "text_delta", text: "等你拍" },
      ),
    ];
    for (const e of stream) run = applyTeamEvent(run, e);
    expect(run!.members).toHaveLength(2);
    expect(run!.members.find((m) => m.assignment_id === "a2")!.status).toBe(
      "needs_input",
    );
    expect(run!.members.find((m) => m.assignment_id === "a1")!.status).toBe(
      "running",
    );
  });

  test("isTeamRunComplete：全员 done/failed/stopped 才完成（needs_input 非终态）", () => {
    const run = mkRun();
    expect(isTeamRunComplete(run)).toBe(false);
    run.members[1].status = "failed";
    expect(isTeamRunComplete(run)).toBe(true);
    run.members[1].status = "needs_input";
    expect(isTeamRunComplete(run)).toBe(false);
  });

  test("Stopped status_transition → 队员 status=stopped 且计入终态", () => {
    let run = applyTeamEvent(
      null,
      env(
        { run_id: "r1" },
        {
          kind: "goal_declared",
          goal: "g",
          status: "frozen",
          lead: "L",
          criteria: [],
        },
      ),
    );
    run = applyTeamEvent(
      run,
      env(
        {
          run_id: "r1",
          assignment_id: "a1",
          origin_participant_id: "w1",
          status_transition: "dispatched",
        },
        { kind: "text_delta", text: "sub" },
      ),
    );
    run = applyTeamEvent(
      run,
      env(
        {
          run_id: "r1",
          assignment_id: "a1",
          origin_participant_id: "w1",
          status_transition: "stopped",
        },
        {
          kind: "completed",
          cost_usd: null,
          input_tokens: 0,
          output_tokens: 0,
        },
      ),
    );
    const m = run!.members.find((x) => x.assignment_id === "a1")!;
    expect(m.status).toBe("stopped");
    expect(isTeamRunComplete(run!)).toBe(true);
  });

  test("teamRunToBlock：转成可持久化 team_run Block（带 goal 快照）", () => {
    const goal = { goal: "g", status: "frozen" as const, criteria: [] };
    const run = mkRun({ goal, lead: "Claude" });
    const block = teamRunToBlock(run);
    expect(block).toMatchObject({
      type: "team_run",
      run_id: "r1",
      goal,
      lead: "Claude",
    });
    expect(block.type === "team_run" && block.members).toHaveLength(2);
  });

  test("终态 completed 事件的 MemberResult 并进 member 快照 + 随 block 落库（§5.1）", () => {
    let run: TeamRun | null = null;
    run = applyTeamEvent(
      run,
      env(
        {
          run_id: "r1",
          assignment_id: "a1",
          origin_participant_id: "w1",
          status_transition: "dispatched",
        },
        { kind: "text_delta", text: "做 X" },
      ),
    );
    run = applyTeamEvent(
      run,
      env(
        { run_id: "r1", assignment_id: "a1", status_transition: "done" },
        {
          kind: "completed",
          cost_usd: null,
          input_tokens: 0,
          output_tokens: 0,
          final_text: null,
          result: {
            schema_version: 1,
            assignment_id: "a1",
            participant_id: "w1",
            status: "done",
            changed_files: [{ path: "src/a.rs", insertions: 3, deletions: 1 }],
            anchor: { base_sha: "abc", generated_from: "worktree_diff" },
            command_evidence: [],
            risk_inputs: {
              files_changed: 1,
              cmd_danger: "low",
              reversibility: "high",
            },
            result_source: "worker_tail",
          },
        },
      ),
    );
    const m = run!.members.find((x) => x.assignment_id === "a1")!;
    expect(m.result?.changed_files[0].path).toBe("src/a.rs");
    expect(m.result?.schema_version).toBe(1);
    // 随 teamRunToBlock 序列化进 team_run block（持久化路径自动带）
    const block = teamRunToBlock(run!);
    expect(
      block.type === "team_run" &&
        block.members.find((x) => x.assignment_id === "a1")?.result
          ?.result_source,
    ).toBe("worker_tail");
  });

  test("completed 事件无 result（空轮/Normal）不写 member.result（§5.1 向后兼容）", () => {
    let run: TeamRun | null = null;
    run = applyTeamEvent(
      run,
      env(
        {
          run_id: "r1",
          assignment_id: "a1",
          origin_participant_id: "w1",
          status_transition: "dispatched",
        },
        { kind: "text_delta", text: "X" },
      ),
    );
    run = applyTeamEvent(
      run,
      env(
        { run_id: "r1", assignment_id: "a1", status_transition: "done" },
        {
          kind: "completed",
          cost_usd: null,
          input_tokens: 0,
          output_tokens: 0,
          final_text: null,
        },
      ),
    );
    expect(run!.members[0].result).toBeUndefined();
  });

  // ---- applyMemberEvent ----
  function emptyMember(aid: string): MemberUnit {
    return {
      participant_id: aid,
      assignment_id: aid,
      task_id: aid,
      name: "Codex",
      status: "running",
      sub: "",
      steps_total: 0,
      steps_done: 0,
      cost_usd: null,
      input_tokens: 0,
      output_tokens: 0,
      failed: false,
      blocks: [],
    };
  }
  const meta = (aid: string, extra: Record<string, unknown> = {}) =>
    ({
      session_id: "s",
      dispatch: { run_id: "w", assignment_id: aid, ...extra },
    }) as AgentEventEnvelope;

  test("applyMemberEvent 折叠：tool_started + tool_completed → 1 tool block, steps_done==1", () => {
    let m = emptyMember("a1");
    m = applyMemberEvent(m, {
      ...meta("a1"),
      kind: "tool_started",
      id: "tid",
      tool: "command",
      summary: "step",
      card: "command",
    } as AgentEventEnvelope);
    m = applyMemberEvent(m, {
      ...meta("a1"),
      kind: "tool_completed",
      id: "tid",
      status: "ok",
      exit_code: 0,
      output: "ok",
    } as AgentEventEnvelope);
    expect(m.blocks.filter((b) => b.type === "tool")).toHaveLength(1);
    expect(m.steps_done).toBe(1);
  });

  test("member 首次创建时有 started_at 时间戳·后续事件不覆盖", () => {
    let run: TeamRun | null = null;
    run = applyTeamEvent(
      run,
      env(
        {
          run_id: "r1",
          assignment_id: "ax1",
          origin_participant_id: "w1",
          status_transition: "dispatched",
        },
        { kind: "text_delta", text: "做 X" },
      ),
    );

    const m = run!.members.find((x) => x.assignment_id === "ax1")!;
    expect(typeof m.started_at).toBe("number");
    expect(m.started_at).toBeGreaterThan(0);
    const startedAt = m.started_at;

    run = applyTeamEvent(
      run,
      env(
        { run_id: "r1", assignment_id: "ax1" },
        { kind: "text_delta", text: "后续文本" },
      ),
    );

    expect(run.members.find((x) => x.assignment_id === "ax1")!.started_at).toBe(
      startedAt,
    );
  });
});

describe("Error 块保守去重（刀 errdedupe·compareErrorBlocks）", () => {
  const EP = "错误：";
  const textBlock = (text: string): Block => ({ type: "text", text });

  test("裸原文先到 + 含它的诚实正文后到 → 只剩诚实正文一块（吸收）", () => {
    let m: MemberUnit = {
      participant_id: "w1",
      assignment_id: "a1",
      task_id: "t1",
      name: "w1",
      status: "running",
      sub: "",
      steps_total: 0,
      steps_done: 0,
      cost_usd: null,
      input_tokens: 0,
      output_tokens: 0,
      failed: false,
      blocks: [],
    };
    const meta = (extra: Record<string, unknown> = {}) =>
      ({
        session_id: "s",
        dispatch: { run_id: "r1", assignment_id: "a1", ...extra },
      }) as AgentEventEnvelope;

    // 裸原文（实时到达）
    m = reduceMemberEvent(
      m,
      {
        ...meta(),
        kind: "error",
        message: "429 Too Many Requests: rate limit exceeded",
      } as AgentEventEnvelope,
      EP,
    );
    // 诚实正文（终态，整段包住裸原文重发）
    m = reduceMemberEvent(
      m,
      {
        ...meta({ status_transition: "failed" }),
        kind: "error",
        message:
          "可以再派一单重试\n引擎另报：429 Too Many Requests: rate limit exceeded",
      } as AgentEventEnvelope,
      EP,
    );

    const textBlocks = m.blocks.filter((b) => b.type === "text");
    expect(textBlocks).toHaveLength(1);
    expect(textBlocks[0].text).toBe(
      "错误：可以再派一单重试\n引擎另报：429 Too Many Requests: rate limit exceeded",
    );
  });

  test("两条互不包含的不同 Error → 都保留", () => {
    expect(
      compareErrorBlocks(textBlock(`${EP}原因 A`), `${EP}原因 B`, EP),
    ).toBe("append");
  });

  test("新条被旧条完整包含 → 丢弃新条", () => {
    const prev = textBlock(`${EP}诚实正文\n引擎另报：raw`);
    expect(compareErrorBlocks(prev, `${EP}raw`, EP)).toBe("drop");
  });

  test("新条完整包含旧条 → 替换旧条", () => {
    const prev = textBlock(`${EP}raw`);
    expect(compareErrorBlocks(prev, `${EP}诚实正文\n引擎另报：raw`, EP)).toBe(
      "replace",
    );
  });

  test("非相邻（中间隔了别的块）→ 不去重（保守边界）", () => {
    let m: MemberUnit = {
      participant_id: "w1",
      assignment_id: "a1",
      task_id: "t1",
      name: "w1",
      status: "running",
      sub: "",
      steps_total: 0,
      steps_done: 0,
      cost_usd: null,
      input_tokens: 0,
      output_tokens: 0,
      failed: false,
      blocks: [],
    };
    const meta = (extra: Record<string, unknown> = {}) =>
      ({
        session_id: "s",
        dispatch: { run_id: "r1", assignment_id: "a1", ...extra },
      }) as AgentEventEnvelope;

    m = reduceMemberEvent(
      m,
      { ...meta(), kind: "error", message: "raw" } as AgentEventEnvelope,
      EP,
    );
    m = reduceMemberEvent(
      m,
      {
        ...meta(),
        kind: "tool_started",
        id: "tid",
        tool: "command",
        summary: "step",
        card: "command",
      } as AgentEventEnvelope,
      EP,
    );
    m = reduceMemberEvent(
      m,
      {
        ...meta({ status_transition: "failed" }),
        kind: "error",
        message: "诚实正文\n引擎另报：raw",
      } as AgentEventEnvelope,
      EP,
    );

    const textBlocks = m.blocks.filter((b) => b.type === "text");
    expect(textBlocks).toHaveLength(2);
    expect(textBlocks[0].text).toBe("错误：raw");
    expect(textBlocks[1].text).toBe("错误：诚实正文\n引擎另报：raw");
  });
});

describe("isOrchestratedDispatch", () => {
  it("returns true when dispatch.orchestrated is true", () => {
    expect(
      isOrchestratedDispatch(
        env({ run_id: "r1", orchestrated: true }, { kind: "completed" }),
      ),
    ).toBe(true);
  });

  it("returns false when dispatch exists but orchestrated is absent", () => {
    expect(
      isOrchestratedDispatch(env({ run_id: "r1" }, { kind: "completed" })),
    ).toBe(false);
  });

  it("returns false when dispatch is absent", () => {
    expect(isOrchestratedDispatch(env(undefined, { kind: "completed" }))).toBe(
      false,
    );
  });
});
