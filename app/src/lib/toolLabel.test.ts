// @ts-expect-error - Vitest runs in Node, but this frontend tsconfig has no Node type declarations.
import { readFileSync } from "fs";
import { describe, expect, it } from "vitest";
import type { Locale, TranslationKey } from "../i18n";
import { humanizeToolName } from "./toolLabel";

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

describe("humanizeToolName", () => {
  it.each([
    ["Bash", "toolCard.name.bash"],
    ["Read", "toolCard.name.read"],
    ["Write", "toolCard.name.write"],
    ["Edit", "toolCard.name.edit"],
    ["Glob", "toolCard.name.glob"],
    ["Grep", "toolCard.name.grep"],
    ["Task", "toolCard.name.task"],
    ["TodoWrite", "toolCard.name.todoWrite"],
    ["WebFetch", "toolCard.name.webFetch"],
    ["WebSearch", "toolCard.name.webSearch"],
    ["NotebookEdit", "toolCard.name.notebookEdit"],
    ["BashOutput", "toolCard.name.bashOutput"],
    ["KillShell", "toolCard.name.killShell"],
  ])("claude 原名 %s → zh 人话", (raw, key) => {
    expect(humanizeToolName(raw, zh)).toBe(zh(key as TranslationKey));
    expect(humanizeToolName(raw, zh)).not.toBe(raw);
  });

  it.each([
    ["fs_read", "toolCard.name.read"],
    ["fs_edit", "toolCard.name.edit"],
    ["fs_write", "toolCard.name.write"],
    ["shell_exec", "toolCard.name.bash"],
    ["grep", "toolCard.name.grep"],
    ["glob", "toolCard.name.glob"],
    ["ls", "toolCard.name.ls"],
    ["web_search", "toolCard.name.webSearch"],
  ])("myagent 名 %s → zh 人话", (raw, key) => {
    expect(humanizeToolName(raw, zh)).toBe(zh(key as TranslationKey));
  });

  it.each(["memory_set", "memory_get", "memory_add", "memory_set_extra"])(
    "myagent memory_* 前缀 %s → 记笔记（前缀匹配）",
    (raw) => {
      expect(humanizeToolName(raw, zh)).toBe(zh("toolCard.name.memory"));
    },
  );

  it.each([
    ["command", "toolCard.name.bash"],
    ["file", "toolCard.name.edit"],
    ["image_gen", "toolCard.name.imageGen"],
  ])("codex 伪名 %s → zh 人话", (raw, key) => {
    expect(humanizeToolName(raw, zh)).toBe(zh(key as TranslationKey));
  });

  it.each([
    ["mcp__agentloom__commit", "toolCard.name.commit"],
    ["mcp__agentloom__push", "toolCard.name.push"],
    ["mcp__agentloom__create_pr", "toolCard.name.createPr"],
    ["mcp__agentloom__publish", "toolCard.name.publish"],
  ])("交付四件套 %s → zh 人话", (raw, key) => {
    expect(humanizeToolName(raw, zh)).toBe(zh(key as TranslationKey));
  });

  it("verifier（决策打扰收敛刀 T2 改款·fold-default）→ zh/en 人话", () => {
    expect(humanizeToolName("verifier", zh)).toBe(zh("toolCard.name.verifier"));
    expect(humanizeToolName("verifier", zh)).not.toBe("verifier");
    expect(humanizeToolName("verifier", en)).toBe(en("toolCard.name.verifier"));
  });

  it("en 侧同样给对应人话", () => {
    expect(humanizeToolName("Bash", en)).toBe(en("toolCard.name.bash"));
    expect(humanizeToolName("mcp__agentloom__commit", en)).toBe(
      en("toolCard.name.commit"),
    );
  });

  it("未收录的工具名原样透传（前向兼容·不静默吞）", () => {
    expect(humanizeToolName("mcp__agentloom__dispatch_worker", zh)).toBe(
      "mcp__agentloom__dispatch_worker",
    );
    expect(humanizeToolName("SomeFutureTool", zh)).toBe("SomeFutureTool");
  });
});
