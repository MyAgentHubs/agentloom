import type { Root } from "mdast";
import type { Extension as FromMarkdownExtension } from "mdast-util-from-markdown";
import { markdownLineEnding } from "micromark-util-character";
import type { State, Tokenizer, TokenizeContext } from "micromark-util-types";
import type { Plugin } from "unified";
import type {} from "remark-parse";

declare module "micromark-util-types" {
  interface TokenTypeMap {
    latexMath: "latexMath";
    latexMathData: "latexMathData";
  }
}

// Recognize TeX delimiters before CommonMark consumes their backslashes.
// A tokenizer (rather than a source-wide replacement) leaves code, URLs,
// escaped backslashes, and ordinary brackets to the Markdown parser.
// Failed suffixes belong to one inline parser context, not a processor or message.
// Once no closer exists in a suffix, later openers cannot need another full scan.
const failedSuffixes = new WeakMap<TokenizeContext, Map<number, number>>();
const tokenize: Tokenizer = function (effects, ok, nok) {
  const context = this;
  let closing = 0;
  return start;

  function start(code: Parameters<State>[0]): ReturnType<State> {
    effects.enter("latexMath");
    effects.enter("latexMathData");
    effects.consume(code);
    return opening;
  }

  function body(code: Parameters<State>[0]): ReturnType<State> {
    if (code === null) {
      const failed = failedSuffixes.get(context) ?? new Map<number, number>();
      failed.set(closing, context.now().offset);
      failedSuffixes.set(context, failed);
      return nok(code);
    }
    // Micromark splits inline content into linked chunks at every line ending.
    // Emit each break so subtokenize can map our events back to those chunks.
    if (markdownLineEnding(code)) {
      effects.exit("latexMathData");
      effects.enter("lineEnding");
      effects.consume(code);
      effects.exit("lineEnding");
      return afterLineEnding;
    }
    effects.consume(code);
    return code === 92 ? afterSlash : body;
  }

  function afterLineEnding(code: Parameters<State>[0]): ReturnType<State> {
    effects.enter("latexMathData");
    return body(code);
  }

  function afterSlash(code: Parameters<State>[0]): ReturnType<State> {
    if (code === null || markdownLineEnding(code)) return body(code);
    effects.consume(code);
    if (code === closing) {
      effects.exit("latexMathData");
      effects.exit("latexMath");
      return ok;
    }
    // Consume a pair of backslashes as data, not as a closing delimiter.
    return body;
  }

  function opening(code: Parameters<State>[0]): ReturnType<State> {
    if (code !== 40 && code !== 91) return nok(code);
    closing = code === 40 ? 41 : 93;
    if (context.now().offset < (failedSuffixes.get(context)?.get(closing) ?? 0))
      return nok(code);
    effects.consume(code);
    return body;
  }
};

const fromMarkdown: FromMarkdownExtension = {
  enter: {
    latexMath(token) {
      this.enter({ type: "inlineMath", value: "" }, token);
    },
  },
  exit: {
    latexMath(token) {
      const node = this.stack[this.stack.length - 1];
      this.exit(token);
      if (node.type !== "inlineMath") return;
      const source = this.sliceSerialize(token);
      node.value = source.slice(2, -2).trim();
      node.data = {
        hName: "span",
        hProperties: {
          className: [source[1] === "[" ? "math-display" : "math-inline"],
        },
        hChildren: [{ type: "text", value: node.value }],
      };
    },
  },
};

export const remarkLatexDelimiters: Plugin<[], Root> = function () {
  const data = this.data();
  (data.micromarkExtensions ??= []).push({
    text: { 92: { name: "latexMath", tokenize } },
  });
  (data.fromMarkdownExtensions ??= []).push(fromMarkdown);
};
