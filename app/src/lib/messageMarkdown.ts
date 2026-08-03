import type { Block, ChatMessage } from "../types/agent";
import type { I18nKey } from "../i18n";

type Translate = (
  key: I18nKey,
  values?: Record<string, string | number>,
) => string;

export function blockToMarkdown(block: Block, t: Translate): string {
  switch (block.type) {
    case "text":
      return block.text;
    case "thinking":
      return block.text
        .split("\n")
        .map((line) => `> ${line}`)
        .join("\n");
    case "tool": {
      const fallback =
        block.exit_code !== null
          ? t("messageMarkdown.toolStatusWithExit", {
              status: block.status,
              exitCode: block.exit_code,
            })
          : t("messageMarkdown.toolStatus", { status: block.status });
      const body = block.output ?? fallback;
      return ["```", `$ ${block.summary}`, body, "```"].join("\n");
    }
    case "image":
      return t("messageMarkdown.image", {
        attachmentId: block.attachment_id,
      });
    case "team_run":
      return t("messageMarkdown.teamRun", {
        n: block.members.length,
        names: [...new Set(block.members.map((m) => m.name))].join(" / "),
      });
    case "run_card":
      return t("messageMarkdown.runCard", {
        n: block.files_changed,
        insertions: block.insertions,
        deletions: block.deletions,
      });
    case "approval":
      return t("messageMarkdown.approval", {
        status: block.status,
        tool: block.tool,
        command: block.command,
      });
    case "scope_change":
      return t("messageMarkdown.scopeChange");
    case "lead_summary":
      return t("messageMarkdown.leadSummary", {
        source: block.summary_source,
      });
    case "coding_task":
      return t("messageMarkdown.codingTask", { phase: block.phase });
    case "gate_card":
      return t("messageMarkdown.gateCard");
    case "draft_failed":
      return t("messageMarkdown.draftFailed");
    case "dispatch_card":
      return t("messageMarkdown.dispatchCard", {
        name: block.member.name,
        sub: block.member.sub,
      });
    case "decision_card":
      return t("messageMarkdown.decisionCard");
    case "run_terminal":
      return block.message
        ? t("messageMarkdown.runTerminalWithMessage", {
            status: block.status,
            message: block.message,
          })
        : t("messageMarkdown.runTerminal", { status: block.status });
  }
}

export function messageToMarkdown(message: ChatMessage, t: Translate): string {
  return message.content
    .map((block) => blockToMarkdown(block, t))
    .filter((content) => content !== "")
    .join("\n\n");
}
