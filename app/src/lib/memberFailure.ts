import type { Block, MemberUnit } from "../types/agent";
import type { I18nKey, TFn } from "../i18n";
import { humanizeFailureDetail } from "./stopReason";

export type MemberFailureCode =
  | "quota"
  | "local_codex_mcp_auth"
  | "auth"
  | "overload"
  | "stalled"
  | "budget_exhausted"
  | "context_exhausted"
  | "env"
  | "spawn"
  | "stopped"
  | "no_final_text";

export type MemberFailureReason = {
  code: MemberFailureCode;
  detail?: string;
};

const FAILURE_REASON_KEYS: Record<MemberFailureCode, I18nKey> = {
  quota: "memberFailure.reason.quota",
  local_codex_mcp_auth: "memberFailure.reason.localCodexMcpAuth",
  auth: "memberFailure.reason.auth",
  overload: "memberFailure.reason.overload",
  stalled: "memberFailure.reason.stalled",
  budget_exhausted: "memberFailure.reason.budgetExhausted",
  context_exhausted: "memberFailure.reason.contextExhausted",
  env: "memberFailure.reason.env",
  spawn: "memberFailure.reason.spawn",
  stopped: "memberDrillIn.status.stopped",
  no_final_text: "memberFailure.reason.noFinalText",
};

const API_LIMIT_HINT =
  /\b(429|rate[\s_-]*limit(?:ed|_exceeded)?|too many requests|quota|insufficient[_\s-]*quota)\b|额度|频控|限流|资源包/i;
const API_AUTH_HINT =
  /\b(401|403|unauthorized|forbidden|authentication|auth|invalid api key|api key)\b|鉴权|认证|无权限/i;
const LOCAL_CODEX_MCP_AUTH_HINT =
  /\b(AuthRequired|AuthorizationRequired)\b|oauth-protected-resource|mcp\.slack\.com/i;
const TRANSPORT_CLOSED_HINT = /Transport channel closed/i;
const AUTH_HINT = /\bauth/i;
const API_OVERLOAD_HINT =
  /\b(503|529|overload(?:ed)?|temporarily unavailable|server busy|capacity)\b|繁忙|过载/i;

// P1-2（opus 对抗审·裁定=判据结构化·2026-07-25 回炉）：这里曾经有一条 STALLED_HINT 正则，
// 匹配 failure_reason 文本里「不是环境故障 / not an environment failure」这句字面短语。
// 实证两条能绕过它的反例：① 后端把 stderr 原样拼进 failure_reason，agent 的 stderr 里
// 恰好含这句英文字样 + 真 exit 1 → 被误判成 stalled（真故障标成停摆）；② 没有
// result.failure_reason 时退回 blocks 正则扫描，agent 自己在输出里写「这不是环境故障」+
// 一个 401 鉴权失败的工具输出 → 同样误判。字符串匹配天生可被产生这段文本的一方（agent 自
// 己的 stdout/stderr）伪造，不能作为分类判据。改用后端 MemberResult.failure_kind 结构化
// 字段（"stalled" / "env"，只由 member_runner.rs 按真实的 saw_blocked/saw_needs_decision
// 标志写）——这个字段不进 agent 可控的文本管道，没有反向伪造通道。

const RESULT_DETAIL_MAX_CHARS = 240;

/** P2-4/D8（delta 复审）：env/stalled 的 detail 来自 result.failure_reason，可能是内嵌
 * 4096B stderr 的多行长文本——加个字符上界，别把摘要行灌爆。**别只取第一行**：P2-6 把
 * harness 的真实缘由（如「卡点：waiting_for_credentials」）拼在诚实措辞后面另起一行，
 * 单纯 `.split("\n")[0]` 会把那句刚拼上的真实缘由整句切没，P2-6 的收益就只剩
 * TaskInspector/MemberDrillIn 能看见、摘要层反而看不到——先把换行折成 "·" 再统一截断，
 * 换行携带的信息保留在同一行里。 */
function clipResultDetail(text: string): string {
  const flattened = text
    .split("\n")
    .map((line) => line.trim())
    .filter((line) => line !== "")
    .join(" · ");
  if (flattened.length <= RESULT_DETAIL_MAX_CHARS) return flattened;
  return `${flattened.slice(0, RESULT_DETAIL_MAX_CHARS - 1)}…`;
}

function blockEvidence(block: Block): string {
  if (block.type === "tool" && block.status === "failed") {
    return [block.summary, block.output].filter(Boolean).join("\n");
  }
  if (block.type === "text") return block.text;
  return "";
}

function failureEvidence(m: MemberUnit): string {
  return m.blocks
    .map(blockEvidence)
    .filter((text) => text.trim() !== "" && text.trim() !== m.sub.trim())
    .join("\n")
    .trim();
}

function latestFailureDetail(m: MemberUnit): string | undefined {
  for (let i = m.blocks.length - 1; i >= 0; i--) {
    const text = blockEvidence(m.blocks[i]).trim();
    if (text !== "" && text !== m.sub.trim()) return text.split("\n")[0];
  }
  return undefined;
}

export function memberFailureReason(m: MemberUnit): MemberFailureReason | null {
  // D1（delta 复审·实证反例）：早退曾经只放在 memberFailureProgress 里——但
  // leadSummary.ts::buildFailureFindings 直接调 memberFailureReason（不经过
  // memberFailureProgress），用的判断也是 `failed || stopped`，于是被用户停掉、汇报
  // 「我已经改好了三个文件」的成员会在 lead 摘要的「失败诚实 findings」里被渲成
  // 「GLM 失败：我已经改好了三个文件。」——跟 stopped ≠ 失败的项目约定直接冲突。下沉到
  // 这里（本函数的唯一入口点）一次性堵住所有调用方；memberFailureProgress 里的早退留着
  // 当双保险，不删。
  if (m.status === "stopped") return { code: "stopped" };
  const resultReason = m.result?.failure_reason?.trim() ?? "";
  // P1-2：failure_kind 是后端写的可信硬判据——命中就直接用，绝不再用正则去嗅
  // failure_reason 文本本身（那条文本的内容方——agent 的 stdout/stderr——是不可信输入）。
  if (m.result?.failure_kind === "stalled") {
    return {
      code: "stalled",
      detail: resultReason !== "" ? clipResultDetail(resultReason) : undefined,
    };
  }
  // 本刀：budget_exhausted 是后端结构化判据（见 AgentEvent::Blocked.reason ==
  // "budget_exhausted_still_progressing"）——跟 stalled 同族但语义不同（预算用完仍在正常
  // 推进，不是卡住/等回答），别再落进 stalled 桶让「等回答/被阻塞」这句话谎报。
  if (m.result?.failure_kind === "budget_exhausted") {
    return {
      code: "budget_exhausted",
      detail: resultReason !== "" ? clipResultDetail(resultReason) : undefined,
    };
  }
  // 第四类（本刀）：context_exhausted 是后端结构化判据（见 AgentEvent::Blocked.reason ==
  // "context_budget_exhausted"）——单轮上下文 token 预算溢出，跟上面按轮次算的
  // budget_exhausted 不是同一件事（没有「一直在正常推进」的证据，也不建议原样重派），
  // 各自独立展示，别混进 stalled/budget_exhausted 任一桶。
  if (m.result?.failure_kind === "context_exhausted") {
    return {
      code: "context_exhausted",
      detail: resultReason !== "" ? clipResultDetail(resultReason) : undefined,
    };
  }
  if (m.result?.failure_kind === "env") {
    return {
      code: "env",
      detail: resultReason !== "" ? clipResultDetail(resultReason) : undefined,
    };
  }
  // 没有结构化 failure_kind（旧快照 / blocking-write / stage1 relay 等其他失败源）——
  // 走既有的文本启发式：result.failure_reason 是后端诊断出的次优先证据源，空了才回落到
  // blocks 正则扫描（zero-reason 路径修复后，这个字段应该总是在真失败时非空）。
  const evidence = resultReason !== "" ? resultReason : failureEvidence(m);
  if (evidence === "") return null;
  if (API_LIMIT_HINT.test(evidence)) return { code: "quota" };
  if (
    LOCAL_CODEX_MCP_AUTH_HINT.test(evidence) ||
    (TRANSPORT_CLOSED_HINT.test(evidence) && AUTH_HINT.test(evidence))
  ) {
    return { code: "local_codex_mcp_auth" };
  }
  if (API_AUTH_HINT.test(evidence)) return { code: "auth" };
  if (API_OVERLOAD_HINT.test(evidence)) return { code: "overload" };
  // 有 result.failure_reason 但没命中上面任何具体分类、也没有结构化 failure_kind →
  // 归到 env（真环境/进程故障的兜底桶），不再落到旧的「spawn + blocks 里翻的第一行」——
  // 那条链是给零 result.failure_reason 场景（下面 evidence 来自 blocks）保留的。
  if (resultReason !== "")
    return { code: "env", detail: clipResultDetail(resultReason) };
  return { code: "spawn", detail: latestFailureDetail(m) };
}

export function memberFailureReasonKey(code: MemberFailureCode): I18nKey {
  return FAILURE_REASON_KEYS[code];
}

/** 失败原因行的统一文案组装：spawn 没有任何具体 detail 时，通用标签只会复述
 * FAILED 徽章，没有信息增量，因此不渲；有 detail 时统一为「标签 — 人话原因」，
 * 其它分类没有 detail 时仍沿用原有 i18n 标签。 */
export function memberFailureReasonText(
  reason: MemberFailureReason,
  t: TFn,
): string | null {
  const detail = reason.detail?.trim() ?? "";
  if (reason.code === "spawn" && detail === "") return null;
  const label = t(memberFailureReasonKey(reason.code));
  if (detail === "") return label;
  return `${label} — ${humanizeFailureDetail(detail, t)}`;
}

export function memberFailureProgress(m: MemberUnit): I18nKey {
  // P2-5（opus 对抗审）：stopped ≠ 失败（用户主动中断，项目明文约定的中性态）——不管
  // blocks/result 里有没有看起来像失败证据的文本，stopped 一律走中性文案，绝不进下面这条
  // 「解释为什么失败」的分类链（那条链是给 failed 态设计的，套在 stopped 头上就是文不对题：
  // 一个被用户主动停掉、可能已经干完大半活的队员，不该被贴上「worker 调用失败」）。
  if (m.status === "stopped") return memberFailureReasonKey("stopped");
  const reason = memberFailureReason(m);
  if (reason != null) return memberFailureReasonKey(reason.code);
  return memberFailureReasonKey("no_final_text");
}
