import type { AgentProfile } from "../types/agent";

// 必须与后端 lead_engine_for_profile（lib.rs）严格一致——两端各自维护同一份支持矩阵，
// 改一处忘改另一处会出现「UI 可点后端拒」或反过来「UI 不给选、后端其实支持」。
// 当前矩阵（L1b + L3 A1）：native claude 支持；borrow（借壳 claude，如 DeepSeek/GLM）支持，
// harness（myagent 引擎）支持——均与 provider 无关、也不看 cap_lead；其余（codex native）不支持。
export function hasLeadCapability(
  agent: Pick<AgentProfile, "enabled" | "provider" | "access">,
): boolean {
  if (!agent.enabled) return false;
  return (
    (agent.provider === "claude" && agent.access === "native") ||
    agent.access === "borrow" ||
    agent.access === "harness"
  );
}
