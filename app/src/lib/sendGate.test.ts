import { describe, it, expect } from "vitest";
import {
  deriveComposerBusy,
  deriveSendGate,
  deriveStickyAgentId,
  resolveFallbackAgentId,
} from "./sendGate";
import type { AgentProfile, ChatMessage } from "../types/agent";

const agent = (id: string): AgentProfile =>
  ({
    id,
    name: id,
    provider: id,
    access: "native",
    enabled: true,
    sort_order: 0,
  }) as AgentProfile;
const asst = (over: Partial<ChatMessage>): ChatMessage =>
  ({ role: "assistant", content: [], ...over }) as ChatMessage;

describe("deriveSendGate", () => {
  const avail = [agent("claude"), agent("codex")];
  it("agents 未 ready → pending", () => {
    expect(
      deriveSendGate({
        messagesLoaded: true,
        loading: false,
        agentsReady: false,
        availableAgents: avail,
        selectedAgentId: "claude",
        memberRunning: false,
      }),
    ).toEqual({ pending: true, effectiveAgentId: null, canSend: false });
  });
  it("messages 未 load → pending", () => {
    expect(
      deriveSendGate({
        messagesLoaded: false,
        loading: false,
        agentsReady: true,
        availableAgents: avail,
        selectedAgentId: "claude",
        memberRunning: false,
      }).pending,
    ).toBe(true);
  });
  it("loading 中 → pending", () => {
    expect(
      deriveSendGate({
        messagesLoaded: true,
        loading: true,
        agentsReady: true,
        availableAgents: avail,
        selectedAgentId: "claude",
        memberRunning: false,
      }).pending,
    ).toBe(true);
  });
  it("selectedAgentId ∈ availableAgents → 可发", () => {
    expect(
      deriveSendGate({
        messagesLoaded: true,
        loading: false,
        agentsReady: true,
        availableAgents: avail,
        selectedAgentId: "codex",
        memberRunning: false,
      }),
    ).toEqual({
      pending: false,
      effectiveAgentId: "codex",
      canSend: true,
    });
  });
  it("selectedAgentId ∉ availableAgents → 不可发", () => {
    expect(
      deriveSendGate({
        messagesLoaded: true,
        loading: false,
        agentsReady: true,
        availableAgents: avail,
        selectedAgentId: "ghost",
        memberRunning: false,
      }),
    ).toEqual({ pending: false, effectiveAgentId: null, canSend: false });
  });

  it("member running 不清空有效 agent，点击自愈由 composer 闸负责", () => {
    const input = {
      messagesLoaded: true,
      loading: false,
      agentsReady: true,
      availableAgents: avail,
      selectedAgentId: "claude",
      memberRunning: false,
    };
    expect(deriveSendGate({ ...input, memberRunning: true }).canSend).toBe(
      true,
    );
    expect(deriveSendGate({ ...input, memberRunning: false }).canSend).toBe(
      true,
    );
  });
});

describe("deriveComposerBusy", () => {
  it("member running 纳入 busy，恢复后释放", () => {
    expect(
      deriveComposerBusy({
        sessionRunning: false,
        loading: false,
        memberRunning: true,
      }),
    ).toBe(true);
    expect(
      deriveComposerBusy({
        sessionRunning: false,
        loading: false,
        memberRunning: false,
      }),
    ).toBe(false);
  });
});

describe("deriveStickyAgentId", () => {
  const avail = [agent("claude"), agent("codex")];
  it("最后 assistant 的 agent_id 精确匹配", () => {
    expect(
      deriveStickyAgentId(
        [asst({ agent_id: "claude" }), asst({ agent_id: "codex" })],
        avail,
      ),
    ).toBe("codex");
  });
  it("取最后一条·非首条", () => {
    expect(
      deriveStickyAgentId(
        [
          asst({ agent_id: "codex" }),
          { role: "user", content: [] } as ChatMessage,
          asst({ agent_id: "claude" }),
        ],
        avail,
      ),
    ).toBe("claude");
  });
  it("仅 agent_id 缺失时退 engine·唯一匹配", () => {
    expect(
      deriveStickyAgentId([asst({ agent_id: null, engine: "codex" })], avail),
    ).toBe("codex");
  });
  it("最后 assistant 的 agent_id 不在 availableAgents → null（fail-closed·不回退更早）", () => {
    expect(
      deriveStickyAgentId(
        [asst({ agent_id: "claude" }), asst({ agent_id: "ghost" })],
        avail,
      ),
    ).toBeNull();
  });
  it('agent_id="" → null', () => {
    expect(deriveStickyAgentId([asst({ agent_id: "" })], avail)).toBeNull();
  });
  it("engine 多匹配 → null", () => {
    expect(
      deriveStickyAgentId(
        [asst({ agent_id: null, engine: "claude" })],
        [agent("claude"), agent("claude")],
      ),
    ).toBeNull();
  });
  it("engine 零匹配 → null", () => {
    expect(
      deriveStickyAgentId([asst({ agent_id: null, engine: "ghost" })], avail),
    ).toBeNull();
  });
  it("无 assistant → null", () => {
    expect(
      deriveStickyAgentId(
        [{ role: "user", content: [] } as ChatMessage],
        avail,
      ),
    ).toBeNull();
  });
});

describe("resolveFallbackAgentId", () => {
  const avail = [agent("claude"), agent("codex")];

  it("storedId 在候选池中 → 返回 storedId", () => {
    expect(resolveFallbackAgentId("codex", avail)).toBe("codex");
  });

  it("storedId 不在候选池 → 返回候选池首个", () => {
    expect(resolveFallbackAgentId("ghost", avail)).toBe("claude");
  });

  it("storedId 为 null → 返回候选池首个", () => {
    expect(resolveFallbackAgentId(null, avail)).toBe("claude");
  });

  it("候选池为空 → 返回 undefined", () => {
    expect(resolveFallbackAgentId("codex", [])).toBeUndefined();
  });
});
