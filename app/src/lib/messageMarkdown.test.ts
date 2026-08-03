import { describe, expect, it } from "vitest";
import type { ChatMessage, MemberUnit } from "../types/agent";
import type { I18nKey } from "../i18n";
import { blockToMarkdown, messageToMarkdown } from "./messageMarkdown";

const templates: Partial<Record<I18nKey, string>> = {
  "messageMarkdown.toolStatusWithExit": "[{status} exit {exitCode}]",
  "messageMarkdown.toolStatus": "[{status}]",
  "messageMarkdown.image": "![image](attachment:{attachmentId})",
  "messageMarkdown.teamRun": "[Agent Team · {n} 个子任务（{names}）]",
  "messageMarkdown.runCard": "[本轮改动 {n} 文件 (+{insertions} −{deletions})]",
  "messageMarkdown.approval": "[审批 {status}：{tool} · {command}]",
  "messageMarkdown.scopeChange": "[agent 提议改任务范围]",
  "messageMarkdown.leadSummary": "[Lead 汇总 · {source}]",
  "messageMarkdown.codingTask": "[coding task · {phase}]",
  "messageMarkdown.gateCard": "[计划草案]",
  "messageMarkdown.draftFailed": "[拟失败]",
  "messageMarkdown.dispatchCard": "\n[任务：{name} · {sub}]\n",
  "messageMarkdown.decisionCard": "[决策卡]",
  "messageMarkdown.runTerminalWithMessage": "[{status} · {message}]",
  "messageMarkdown.runTerminal": "[{status}]",
};

const t: Parameters<typeof messageToMarkdown>[1] = (key, values) => {
  let result = templates[key] ?? key;
  for (const [name, value] of Object.entries(values ?? {})) {
    result = result.split(`{${name}}`).join(String(value));
  }
  return result;
};

function msg(content: ChatMessage["content"]): ChatMessage {
  return { role: "assistant", content, engine: "claude" };
}

function member(name: string): MemberUnit {
  return {
    participant_id: name,
    assignment_id: `${name}-assignment`,
    task_id: `${name}-task`,
    name,
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

describe("messageToMarkdown", () => {
  it("text 块原样", () => {
    expect(
      messageToMarkdown(msg([{ type: "text", text: "hello\nworld" }]), t),
    ).toBe("hello\nworld");
  });

  it("thinking 块成引用", () => {
    expect(
      messageToMarkdown(msg([{ type: "thinking", text: "想一想" }]), t),
    ).toContain("> 想一想");
  });

  it("tool 块成代码块（summary + output）", () => {
    const out = messageToMarkdown(
      msg([
        {
          type: "tool",
          id: "t1",
          tool: "Bash",
          summary: "ls -la",
          card: "command",
          status: "ok",
          exit_code: 0,
          output: "a\nb",
        },
      ]),
      t,
    );
    expect(out).toContain("```");
    expect(out).toContain("$ ls -la");
    expect(out).toContain("a\nb");
  });

  it("image 块成 markdown 图片引用", () => {
    expect(
      messageToMarkdown(
        msg([
          { type: "image", attachment_id: "img9", media_type: "image/png" },
        ]),
        t,
      ),
    ).toBe("![image](attachment:img9)");
  });

  it("team_run 块成派单摘要行", () => {
    expect(
      messageToMarkdown(
        msg([
          {
            type: "team_run",
            run_id: "r1",
            goal: null,
            members: [member("worker-1"), member("worker-2")],
          },
        ]),
        t,
      ),
    ).toBe("[Agent Team · 2 个子任务（worker-1 / worker-2）]");
  });

  it("混排块按顺序用空行连接", () => {
    const out = messageToMarkdown(
      msg([
        { type: "text", text: "答" },
        { type: "thinking", text: "思" },
      ]),
      t,
    );
    expect(out).toBe("答\n\n> 思");
  });

  it("空 content → 空串", () => {
    expect(messageToMarkdown(msg([]), t)).toBe("");
  });

  it("gate_card/draft_failed 块导出占位文本（不丢进 text 分支）", () => {
    expect(blockToMarkdown({ type: "gate_card", session_id: "s1" }, t)).toBe(
      "[计划草案]",
    );
    expect(blockToMarkdown({ type: "draft_failed", session_id: "s1" }, t)).toBe(
      "[拟失败]",
    );
  });
});
