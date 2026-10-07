import type { Root, RootContent } from "mdast";
import type { Extension as FromMarkdownExtension } from "mdast-util-from-markdown";
import type { State, Tokenizer } from "micromark-util-types";
import type { Plugin } from "unified";
import type {} from "remark-parse";

declare module "micromark-util-types" {
  interface TokenTypeMap {
    chatInlineMath: "chatInlineMath";
  }
}

// Single dollars follow Pandoc-style boundaries: no opening/closing whitespace,
// no digit immediately after the closer. Keep single-dollar math on one line.
// Stop at the first candidate dollar; failed candidates never scan the suffix.
const tokenize: Tokenizer = function (effects, ok, nok) {
  let previous: number | null = null;
  let escaped = false;
  return start;
  function start(code: Parameters<State>[0]): ReturnType<State> {
    effects.enter("chatInlineMath");
    effects.consume(code);
    return first;
  }
  function first(code: Parameters<State>[0]): ReturnType<State> {
    if (code === null || code <= 32 || code === 36) return nok(code);
    return body(code);
  }
  function body(code: Parameters<State>[0]): ReturnType<State> {
    if (code === null || code < -2) return nok(code);
    if (code === 36 && !escaped) {
      if (previous === null || previous <= 32) return nok(code);
      effects.consume(code);
      return after;
    }
    escaped = code === 92 && !escaped;
    previous = code;
    effects.consume(code);
    return body;
  }
  function after(code: Parameters<State>[0]): ReturnType<State> {
    if (code === 36 || (code !== null && code >= 48 && code <= 57))
      return nok(code);
    effects.exit("chatInlineMath");
    return ok(code);
  }
};

const seenFence = new WeakSet<object>();
const closedMath = new WeakSet<object>();

const fromMarkdown: FromMarkdownExtension = {
  enter: {
    mathFlowFenceSequence() {
      // These are committed parser tokens, so quoted/list prefixes and EOF
      // cannot be mistaken for a real closing fence by a source regex.
      const math = [...this.stack]
        .reverse()
        .find((node) => node.type === "math");
      if (!math) return;
      if (seenFence.has(math)) closedMath.add(math);
      else seenFence.add(math);
    },
    chatInlineMath(token) {
      this.enter({ type: "inlineMath", value: "" }, token);
    },
  },
  exit: {
    chatInlineMath(token) {
      const node = this.stack[this.stack.length - 1];
      this.exit(token);
      if (node.type !== "inlineMath") return;
      node.value = this.sliceSerialize(token).slice(1, -1);
      node.data = {
        hName: "span",
        hProperties: { className: ["math-inline"] },
        hChildren: [{ type: "text", value: node.value }],
      };
    },
  },
};

function normalizeMath(node: Root | RootContent, source: string): void {
  if ("children" in node) {
    node.children.forEach((child) => normalizeMath(child, source));
  }
  if (node.type !== "math" && node.type !== "inlineMath") return;
  const start = node.position?.start.offset;
  const end = node.position?.end.offset;
  if (start == null || end == null) return;
  const raw = source.slice(start, end);
  if (node.type === "math") {
    // remark-math accepts EOF as an implicit fence. Chat must wait for a closer.
    if (!closedMath.has(node)) {
      Object.assign(node, {
        type: "paragraph",
        data: undefined,
        children: [{ type: "text", value: raw }],
      });
    }
  } else if (node.type === "inlineMath" && raw.startsWith("$$")) {
    node.data = {
      hName: "span",
      hProperties: { className: ["math-display"] },
      hChildren: [{ type: "text", value: node.value }],
    };
  }
}

// Register after remarkMath({ singleDollarTextMath: false }). The stock block
// parser retains Markdown container handling; this transform delays unclosed blocks.
export const remarkChatMath: Plugin<[], Root> = function () {
  const data = this.data();
  (data.micromarkExtensions ??= []).push({
    text: {
      36: {
        name: "chatInlineMath",
        tokenize,
        previous(code) {
          return (
            code !== 36 ||
            this.events[this.events.length - 1]?.[1].type === "characterEscape"
          );
        },
      },
    },
  });
  (data.fromMarkdownExtensions ??= []).push(fromMarkdown);
  return (tree, file) => normalizeMath(tree, String(file));
};
