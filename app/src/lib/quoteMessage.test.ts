import { describe, it, expect } from "vitest";
import type { ChatMessage } from "../types/agent";
import {
  quotableSource,
  canQuote,
  quotePreview,
  quoteBlock,
  quoteLabel,
  quoteTooltip,
} from "./quoteMessage";

const text = (t: string): ChatMessage => ({
  role: "assistant",
  content: [{ type: "text", text: t }],
  engine: "claude",
});

const t: Parameters<typeof quoteLabel>[1] = (key) =>
  key === "stream.role.user" ? "你" : "助手";

const toolMsg: ChatMessage = {
  role: "assistant",
  content: [
    {
      type: "tool",
      id: "t1",
      tool: "Bash",
      summary: "npm test",
      card: "command",
      status: "failed",
      exit_code: 1,
      output: "boom",
    },
  ],
  engine: "codex",
};

describe("quotableSource", () => {
  it("多 text 块拼接（\\n）", () => {
    const m: ChatMessage = {
      role: "assistant",
      content: [
        { type: "text", text: "a" },
        { type: "text", text: "b" },
      ],
    };
    expect(quotableSource(m)).toBe("a\nb");
  });
  it("混合 text+tool 只取 text", () => {
    const m: ChatMessage = {
      role: "assistant",
      content: [{ type: "text", text: "hello" }, toolMsg.content[0]],
    };
    expect(quotableSource(m)).toBe("hello");
  });
  it("纯工具卡走 tool 兜底（命令+状态+exit+output）", () => {
    expect(quotableSource(toolMsg)).toBe("$ npm test\n[failed exit 1]\nboom");
  });
  it("无 exit_code 状态行不带 exit", () => {
    const m: ChatMessage = {
      role: "assistant",
      content: [
        {
          type: "tool",
          id: "t",
          tool: "Bash",
          summary: "ls",
          card: "command",
          status: "running",
          exit_code: null,
          output: null,
        },
      ],
    };
    expect(quotableSource(m)).toBe("$ ls\n[running]");
  });
  it("thinking-only / image-only / 空 → 空串", () => {
    expect(
      quotableSource({
        role: "assistant",
        content: [{ type: "thinking", text: "x" }],
      }),
    ).toBe("");
    expect(
      quotableSource({
        role: "assistant",
        content: [
          { type: "image", attachment_id: "a", media_type: "image/png" },
        ],
      }),
    ).toBe("");
    expect(quotableSource({ role: "assistant", content: [] })).toBe("");
  });
});

describe("canQuote", () => {
  it("text/tool 可引；thinking-only/image-only/空 不可引", () => {
    expect(canQuote(text("hi"))).toBe(true);
    expect(canQuote(toolMsg)).toBe(true);
    expect(
      canQuote({
        role: "assistant",
        content: [{ type: "thinking", text: "x" }],
      }),
    ).toBe(false);
    expect(canQuote({ role: "assistant", content: [] })).toBe(false);
  });
});

describe("quotePreview", () => {
  it("单行短消息：完整、不加 …", () => {
    expect(quotePreview(text("简短一句"))).toBe("简短一句");
  });
  it("取首个非空行，后面还有内容时加 … 提示是片段", () => {
    expect(quotePreview(text("\n\n第一行\n第二行"))).toBe("第一行…");
  });
  it("首行短但仅有空白尾行：不加 …（无更多内容）", () => {
    expect(quotePreview(text("只有一行\n   \n"))).toBe("只有一行");
  });
  it("码点截断到 60 + …", () => {
    const long = "字".repeat(80);
    const out = quotePreview(text(long));
    expect(Array.from(out)).toHaveLength(61); // 60 + …
    expect(out.endsWith("…")).toBe(true);
  });
  it("代理对（emoji）不被切坏", () => {
    const out = quotePreview(text("😀".repeat(70)));
    expect(
      Array.from(out)
        .slice(0, 60)
        .every((c) => c === "😀"),
    ).toBe(true);
  });
  it("空消息 → 空串", () => {
    expect(quotePreview({ role: "assistant", content: [] })).toBe("");
  });
});

describe("quoteBlock", () => {
  it("每行加 > 前缀 + 尾部 \\n\\n", () => {
    expect(quoteBlock(text("a\nb"))).toBe("> a\n> b\n\n");
  });
  it("超 3 行截断 + …", () => {
    const out = quoteBlock(text("l1\nl2\nl3\nl4\nl5"));
    expect(out).toBe("> l1\n> l2\n> l3…\n\n");
  });
  it("超 180 码点截断 + …", () => {
    const out = quoteBlock(text("x".repeat(300)));
    const body = out.replace(/^> /, "").replace(/\n\n$/, "");
    expect(Array.from(body.replace(/…$/, ""))).toHaveLength(180);
    expect(body.endsWith("…")).toBe(true);
  });
  it("tool 兜底也受截断（超大 output）", () => {
    const big: ChatMessage = {
      role: "assistant",
      content: [
        {
          type: "tool",
          id: "t",
          tool: "Bash",
          summary: "run",
          card: "command",
          status: "ok",
          exit_code: 0,
          output: "y".repeat(500),
        },
      ],
    };
    const out = quoteBlock(big);
    expect(out.endsWith("…\n\n")).toBe(true);
  });
  it("空消息 → 空串", () => {
    expect(quoteBlock({ role: "assistant", content: [] })).toBe("");
  });
});

describe("quoteTooltip", () => {
  it("短消息保留全文 + 换行", () => {
    expect(quoteTooltip(text("第一行\n第二行"))).toBe("第一行\n第二行");
  });
  it("超 280 码点截断 + …", () => {
    const out = quoteTooltip(text("字".repeat(400)));
    expect(Array.from(out)).toHaveLength(281); // 280 + …
    expect(out.endsWith("…")).toBe(true);
  });
});

describe("quoteLabel", () => {
  it("quote_label_uses_agent_name", () => {
    expect(
      quoteLabel(
        {
          role: "assistant",
          content: [{ type: "text", text: "a" }],
          engine: "claude",
          agent_id: "codex",
          agent_name_snapshot: "Code Agent",
        },
        t,
      ),
    ).toBe("Code Agent");
    expect(
      quoteLabel(
        {
          role: "assistant",
          content: [{ type: "text", text: "a" }],
          agent_id: "codex",
        },
        t,
      ),
    ).toBe("codex");
    expect(quoteLabel(text("a"), t)).toBe("claude");
  });

  it("user→你 / assistant+engine→engine / assistant 无 engine→助手", () => {
    expect(
      quoteLabel({ role: "user", content: [{ type: "text", text: "q" }] }, t),
    ).toBe("你");
    expect(quoteLabel(text("a"), t)).toBe("claude");
    expect(
      quoteLabel(
        { role: "assistant", content: [{ type: "text", text: "a" }] },
        t,
      ),
    ).toBe("助手");
  });
});
