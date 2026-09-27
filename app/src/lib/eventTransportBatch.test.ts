import { describe, expect, it, vi } from "vitest";
import { applyEventTransportBatch } from "./eventTransportBatch";

describe("applyEventTransportBatch", () => {
  it("passes batch agent identity to each event envelope", () => {
    const applyEvent = vi.fn();

    applyEventTransportBatch(
      {
        batches: [
          {
            session_id: "s1",
            agent_id: "deepseek",
            agent_name_snapshot: "DeepSeek",
            events: [{ seq: 1, kind: "session_started" }],
          },
        ],
      },
      () => new Map<string, unknown[]>(),
      applyEvent,
      () => false,
      vi.fn(),
    );

    expect(applyEvent).toHaveBeenCalledOnce();
    expect(applyEvent.mock.calls[0][0]).toMatchObject({
      session_id: "s1",
      kind: "session_started",
      agent_id: "deepseek",
      agent_name_snapshot: "DeepSeek",
    });
  });

  it("omits agent identity keys when the batch has no agent identity", () => {
    const applyEvent = vi.fn();

    applyEventTransportBatch(
      {
        batches: [
          { session_id: "s1", events: [{ seq: 1, kind: "session_started" }] },
        ],
      },
      () => new Map<string, unknown[]>(),
      applyEvent,
      () => false,
      vi.fn(),
    );

    expect(applyEvent).toHaveBeenCalledOnce();
    const envelope = applyEvent.mock.calls[0][0];
    expect(Object.prototype.hasOwnProperty.call(envelope, "agent_id")).toBe(
      false,
    );
    expect(
      Object.prototype.hasOwnProperty.call(envelope, "agent_name_snapshot"),
    ).toBe(false);
  });
});
