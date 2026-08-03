import type { AgentProfile } from "../types/agent";

/** provider → 该原生 CLI 是否已装。undefined = 整体尚未检测（乐观显示原生）。 */
export type RuntimeDetect = Record<string, boolean>;

/**
 * 输入区可用性：
 * - native：runtime 未加载(undefined)→乐观 true；加载后查表，无此 provider→false（保守）。
 * - harness：CLI sidecar（myagent）。operator 经 MYAGENT_BIN + env/keychain 配置，
 *   可用性不绑 has_key（key 走 env 兜底）→ enabled 即乐观可用，同 CLI 哲学。
 * - borrow：has_key === true。
 * - enabled=false 一律 false。
 */
export function isAgentAvailable(
  agent: AgentProfile,
  runtime: RuntimeDetect | undefined,
): boolean {
  if (!agent.enabled) return false;
  if (agent.access === "native") {
    if (runtime === undefined) return true;
    return runtime[agent.provider] === true;
  }
  if (agent.access === "harness") return true;
  return agent.has_key === true;
}
