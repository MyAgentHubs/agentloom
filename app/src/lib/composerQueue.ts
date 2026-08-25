import type { Mode } from "../components/ModeDropdown";
import type { ComposerRuntimeConfig } from "../types/agent";

/**
 * msgfix2 Q1：运行中 composer 消息排队。
 *
 * 条目核心是 `{id, text}`（text 为入队时已展开附件的 composed 文本）；额外携带
 * `mode` + `agentId`（solo 投递用）+ `config`，是因为递送必须走「sid 显式」通路
 * （不读 currentIdRef、不走当前视图的 sendGate/mode 全局态——投递时用户可能已经
 * 切到别的会话）。这些字段在入队那一刻就把「往哪投、用谁投」钉死，交由 App 层
 * 的投递函数原样消费，不依赖任何会在之后漂移的全局 UI 状态。
 */
export type QueuedMessage = {
  id: string;
  text: string;
  mode: Mode;
  /** solo 投递目标 agent；team 模式下不需要（lead 由 sid 内 runtime team config 解析）。 */
  agentId?: string | null;
  config?: ComposerRuntimeConfig;
};

export type QueueState = ReadonlyMap<string, QueuedMessage[]>;

const EMPTY_LIST: QueuedMessage[] = [];

function newId(): string {
  if (
    typeof crypto !== "undefined" &&
    typeof crypto.randomUUID === "function"
  ) {
    return crypto.randomUUID();
  }
  return `q-${Date.now()}-${Math.random().toString(36).slice(2)}`;
}

export function emptyQueueState(): QueueState {
  return new Map();
}

/** 取某会话的排队列表（引用稳定：空队列恒返回同一个空数组，避免下游 memo 组件白转）。 */
export function listQueue(
  state: QueueState,
  sessionId: string,
): QueuedMessage[] {
  return state.get(sessionId) ?? EMPTY_LIST;
}

export function enqueue(
  state: QueueState,
  sessionId: string,
  entry: Omit<QueuedMessage, "id"> & { id?: string },
): QueueState {
  const next = new Map(state);
  const list = next.get(sessionId) ?? EMPTY_LIST;
  const message: QueuedMessage = { ...entry, id: entry.id ?? newId() };
  next.set(sessionId, [...list, message]);
  return next;
}

export function remove(
  state: QueueState,
  sessionId: string,
  id: string,
): QueueState {
  const list = state.get(sessionId);
  if (!list) return state;
  const filtered = list.filter((m) => m.id !== id);
  if (filtered.length === list.length) return state;
  const next = new Map(state);
  if (filtered.length > 0) next.set(sessionId, filtered);
  else next.delete(sessionId);
  return next;
}

export function clear(state: QueueState, sessionId: string): QueueState {
  if (!state.has(sessionId)) return state;
  const next = new Map(state);
  next.delete(sessionId);
  return next;
}
