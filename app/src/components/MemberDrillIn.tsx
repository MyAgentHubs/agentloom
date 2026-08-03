import { useMemo, type ReactNode } from "react";
import { useI18n, type I18nKey } from "../i18n";
import type {
  AcceptanceCriterion,
  Criterion,
  GoalContract,
  MemberUnit,
} from "../types/agent";
import { MessageContent } from "./MessageContent";
import { humanizeFailureDetail } from "../lib/stopReason";

export type DrillProps = {
  members: MemberUnit[];
  selectedId: string | null;
  onSelect: (assignmentId: string) => void;
  onBack: () => void;
  onStop?: (assignmentId: string) => void;
  goal?: GoalContract | null;
  criteria?: AcceptanceCriterion[];
};

function tokLabel(m: MemberUnit, noTokens: string): string {
  const tot = m.input_tokens + m.output_tokens;
  if (tot <= 0) return noTokens;
  return tot >= 1000 ? `${Math.round(tot / 1000)}k tok` : `${tot} tok`;
}

const MEMBER_STATUS_KEY: Record<MemberUnit["status"], I18nKey> = {
  running: "memberDrillIn.status.running",
  needs_input: "memberDrillIn.status.needsInput",
  done: "memberDrillIn.status.done",
  failed: "memberDrillIn.status.failed",
  stopped: "memberDrillIn.status.stopped",
};

const CRIT_KEY: Record<Criterion["status"], I18nKey> = {
  pending: "memberDrillIn.criterion.pending",
  passed: "memberDrillIn.criterion.passed",
  failed: "memberDrillIn.criterion.failed",
  waived: "memberDrillIn.criterion.waived",
  uncertain: "memberDrillIn.criterion.uncertain",
};

function fileStatLabel(insertions: number, deletions: number): string {
  const parts = [];
  if (insertions > 0) parts.push(`+${insertions}`);
  if (deletions > 0) parts.push(`-${deletions}`);
  return parts.join(" ");
}

function compactText(text: string, max = 128): string {
  const cleaned = text
    .replace(/```[\s\S]*?```/g, " ")
    .replace(/(^|\n)\s{0,3}#{1,6}\s+/g, " ")
    .replace(/(^|\n)\s*[-*]\s+/g, " ")
    .replace(/\s+/g, " ")
    .trim();
  if (cleaned.length <= max) return cleaned;
  return `${cleaned.slice(0, max - 1).trimEnd()}…`;
}

function taskPackSummary(taskPack: string | undefined): string | null {
  if (!taskPack) return null;
  const task = taskPack.match(
    /(?:^|\n)##\s*你的子任务\s*\n([\s\S]*?)(?=\n##\s+|$)/,
  );
  const summary = compactText(task?.[1] ?? taskPack);
  return summary || null;
}

function memberSummary(selected: MemberUnit): string {
  return taskPackSummary(selected.taskPack) ?? compactText(selected.sub);
}

function inlineTokenRegex(): RegExp {
  return /(^|[\s(["'“‘])((?:~|\/(?!\/))[^\s,，。；;：:）)\]}>]+|(?:\.{1,2}\/|[\w@.-]+\/)[^\s,，。；;：:）)\]}>]+|[\w@.-]+\.(?:md|tsx?|jsx?|rs|ya?ml|json|toml|css|html|sh|lock|sql|go|py|rb|java|kt|swift|c|cpp|h|hpp))(?=$|[\s,，。；;：:）)\]}>])/g;
}

function renderInlineTokens(text: string): ReactNode[] {
  const nodes: ReactNode[] = [];
  let last = 0;
  text.replace(
    inlineTokenRegex(),
    (match, prefix: string, token: string, offset: number) => {
      const start = offset + prefix.length;
      if (start > last) nodes.push(text.slice(last, start));
      nodes.push(
        <code className="drillin__inline-code" key={`${token}-${offset}`}>
          {token}
        </code>,
      );
      last = start + token.length;
      return match;
    },
  );
  if (last < text.length) nodes.push(text.slice(last));
  return nodes;
}

function codeInlineTokensInMarkdown(markdown: string): string {
  return markdown
    .split(/(```[\s\S]*?```|`[^`\n]*`)/g)
    .map((part) => {
      if (part.startsWith("`")) return part;
      return part.replace(
        inlineTokenRegex(),
        (_match: string, prefix: string, token: string) => {
          return `${prefix}\`${token}\``;
        },
      );
    })
    .join("");
}

export function MemberDrillIn({
  members,
  selectedId,
  onSelect,
  onBack,
  onStop,
  goal,
  criteria = [],
}: DrillProps) {
  const { t } = useI18n();
  const selected =
    members.find((m) => m.assignment_id === selectedId) ?? members[0];
  // 稳定引用：brief 的 blocks 按 taskPack 缓存·否则每次渲染新建数组字面量会破 MessageContent 的 memo →
  // running 队员流式狂刷事件时每个 tick 重解析 TaskPack markdown → 卡死（读码坐实根因）。
  const briefBlocks = useMemo(
    () =>
      selected?.taskPack
        ? [
            {
              type: "text" as const,
              text: codeInlineTokensInMarkdown(selected.taskPack),
            },
          ]
        : [],
    [selected?.taskPack],
  );
  if (!selected) return null;
  const changedFiles = selected.result?.changed_files ?? [];
  const commandEvidence = selected.result?.command_evidence ?? [];
  const hasCodingDetails = changedFiles.length > 0;
  const criteriaRows = criteria.length > 0 ? criteria : (goal?.criteria ?? []);
  return (
    <div className="drillin">
      <div className="drillin__crumb">
        <button
          type="button"
          className="drillin__back"
          onClick={onBack}
          aria-label={t("memberDrillIn.backToLead")}
        >
          ‹ Lead
        </button>
        <span className="drillin__sep">·</span>
        <span className="drillin__switch">
          {members.map((m) => (
            <button
              type="button"
              key={m.assignment_id}
              className={`drillin__sw${m.assignment_id === selected.assignment_id ? " is-active" : ""}`}
              onClick={() => onSelect(m.assignment_id)}
            >
              {m.name}
            </button>
          ))}
        </span>
      </div>
      {/* §二.3：drill 头部 token 行（状态 · 步 x/y · Nk tok） */}
      <div className="drillin__head">
        <span className={`drillin__status is-${selected.status}`}>
          {t(MEMBER_STATUS_KEY[selected.status])}
        </span>
        <span className="drillin__meta">
          {t("memberDrillIn.steps", {
            done: selected.steps_done,
            total: selected.steps_total,
            tokens: tokLabel(selected, t("memberDrillIn.noTokens")),
          })}
        </span>
        {selected.status === "running" && onStop && (
          <button
            type="button"
            className="member-card__stop"
            aria-label={t("memberDrillIn.stopAria", { name: selected.name })}
            onClick={() => onStop(selected.assignment_id)}
          >
            {t("memberDrillIn.stop")}
          </button>
        )}
      </div>
      {/* P1（member 失败原因透出）：失败态在头部状态行下补一行原因，不动 hasCodingDetails
          语义——失败原因跟「有没有代码改动详情」是两回事，别混进同一段判断。 */}
      {selected.status === "failed" && selected.result?.failure_reason && (
        <div className="drillin__failure">
          <span className="drillin__failure-label">
            {t("memberDrillIn.failureReason")}
          </span>
          <span className="drillin__failure-text">
            {humanizeFailureDetail(selected.result.failure_reason, t)}
          </span>
        </div>
      )}
      <div className="drillin__body">
        <div className="drillin__sec">{t("memberDrillIn.overview")}</div>
        <p className="drillin__summary">
          {renderInlineTokens(memberSummary(selected))}
        </p>
        {hasCodingDetails && (
          <section
            className="drillin__coding"
            aria-label={t("memberDrillIn.taskDetails")}
          >
            {goal && (
              <>
                <div className="drillin__sec">{t("memberDrillIn.goal")}</div>
                <div className="drillin__goal">
                  {renderInlineTokens(goal.goal_title ?? goal.goal)}
                </div>
              </>
            )}
            {criteriaRows.length > 0 && (
              <>
                <div className="drillin__sec">
                  {t("memberDrillIn.acceptance")}
                </div>
                <div className="drillin__criteria">
                  {criteriaRows.map((c) => (
                    <div
                      className={`drillin__criterion is-${c.status}`}
                      key={c.id}
                    >
                      <span
                        className={`goal-crit__stat is-${c.status}`}
                        aria-hidden
                      >
                        <svg
                          viewBox="0 0 24 24"
                          {...(c.status === "pending"
                            ? { strokeDasharray: "3 3" }
                            : {})}
                        >
                          {c.status === "pending" && (
                            <circle cx="12" cy="12" r="8.5" />
                          )}
                          {c.status === "waived" && (
                            <>
                              <circle cx="12" cy="12" r="8.5" />
                              <path d="M6.5 6.5l11 11" />
                            </>
                          )}
                          {c.status === "passed" && <path d="M5 13l4 4L19 7" />}
                          {c.status === "failed" && (
                            <path d="M7 7l10 10M17 7L7 17" />
                          )}
                        </svg>
                      </span>
                      <span className="drillin__criterion-text">
                        {renderInlineTokens(c.claim)}
                      </span>
                      <span className={`goal-crit__stag is-${c.status}`}>
                        {t(CRIT_KEY[c.status])}
                      </span>
                    </div>
                  ))}
                </div>
              </>
            )}
            <div className="drillin__sec">
              {t("memberDrillIn.changedFiles")}
            </div>
            <div className="drillin__files">
              {changedFiles.map((f) => (
                <div className="drillin__file" key={f.path}>
                  <span className="drillin__file-path">{f.path}</span>
                  {fileStatLabel(f.insertions, f.deletions) && (
                    <span className="drillin__file-stat">
                      {fileStatLabel(f.insertions, f.deletions)}
                    </span>
                  )}
                </div>
              ))}
            </div>
            {changedFiles.length > 0 && (
              <div className="drillin__meta">
                {t("memberDrillIn.changedFilesCaveat")}
              </div>
            )}
            {commandEvidence.length > 0 && (
              <>
                <div className="drillin__sec">
                  {t("memberDrillIn.verification")}
                </div>
                <div className="drillin__cmds">
                  {commandEvidence.map((e, idx) => {
                    const exitCode = e.exit_code ?? null;
                    const ok = exitCode === 0;
                    return (
                      <div className="drillin__cmd" key={`${e.cmd}-${idx}`}>
                        <code className="drillin__cmd-text">{e.cmd}</code>
                        <span
                          className={`drillin__exit ${ok ? "is-ok" : "is-fail"}`}
                        >
                          {t("memberDrillIn.exitCode", {
                            code: exitCode ?? "--",
                          })}
                        </span>
                      </div>
                    );
                  })}
                </div>
              </>
            )}
          </section>
        )}
        {selected.taskPack && (
          <details className="drillin__fold drillin__brief">
            <summary className="drillin__fold-sum drillin__brief-sum">
              {t("memberDrillIn.viewAssignment")}
            </summary>
            <div className="drillin__fold-body drillin__brief-body">
              <MessageContent blocks={briefBlocks} />
            </div>
          </details>
        )}
        {selected.blocks.length > 0 && (
          <details className="drillin__fold drillin__raw">
            <summary className="drillin__fold-sum drillin__raw-sum">
              {t("memberDrillIn.rawTrace")}
            </summary>
            <div className="drillin__fold-body drillin__raw-body">
              <MessageContent blocks={selected.blocks} />
            </div>
          </details>
        )}
      </div>
    </div>
  );
}
