import type { AgentProfile, ChatMessage } from "../types/agent";

export type SendGate = {
  /** agents/messages 仍在加载 → gate UI（dropdown 显「…」禁用） */
  pending: boolean;
  /** 实际发送用的 agent id；不可发时 null */
  effectiveAgentId: string | null;
  /** agent 层是否可发（不含 draft/composerBusy） */
  canSend: boolean;
};

export type DeriveSendGateInput = {
  messagesLoaded: boolean;
  loading: boolean;
  agentsReady: boolean;
  availableAgents: AgentProfile[];
  selectedAgentId: string;
  memberRunning: boolean;
};

export function deriveSendGate(input: DeriveSendGateInput): SendGate {
  const {
    messagesLoaded,
    loading,
    agentsReady,
    availableAgents,
    selectedAgentId,
  } = input;
  if (!agentsReady || loading || !messagesLoaded) {
    return { pending: true, effectiveAgentId: null, canSend: false };
  }
  const valid = availableAgents.some((a) => a.id === selectedAgentId);
  return {
    pending: false,
    effectiveAgentId: valid ? selectedAgentId : null,
    // memberRunning 由 composerBusy 呈现；保留有效 agent，供点击后端真相自愈后发送。
    canSend: valid,
  };
}

export function deriveComposerBusy(input: {
  sessionRunning: boolean;
  loading: boolean;
  memberRunning: boolean;
}): boolean {
  return input.sessionRunning || input.loading || input.memberRunning;
}

export function resolveFallbackAgentId(
  storedId: string | null,
  enabledAgents: AgentProfile[],
): string | undefined {
  if (storedId && enabledAgents.some((agent) => agent.id === storedId)) {
    return storedId;
  }
  return enabledAgents[0]?.id;
}

/** agent_id 精确匹配；仅 agent_id 缺失时退 engine 且仅唯一精确匹配（不按 provider 猜/不取首个）。 */
function resolveAgentId(
  m: ChatMessage,
  availableAgents: AgentProfile[],
): string | null {
  if (m.agent_id != null) {
    return availableAgents.some((a) => a.id === m.agent_id) ? m.agent_id : null;
  }
  if (m.engine) {
    const matches = availableAgents.filter((a) => a.id === m.engine);
    if (matches.length === 1) return matches[0].id;
  }
  return null;
}

/** 该会话**最后一条** assistant 的 agent，在 availableAgents 内可解析时返回其 id，否则 null（fail-closed·不回退更早 assistant）。 */
export function deriveStickyAgentId(
  messages: ChatMessage[],
  availableAgents: AgentProfile[],
): string | null {
  for (let i = messages.length - 1; i >= 0; i--) {
    if (messages[i].role === "assistant") {
      return resolveAgentId(messages[i], availableAgents);
    }
  }
  return null;
}
