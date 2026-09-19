import { screen, fireEvent, waitFor, within } from "@testing-library/react";
import { expect, type vi } from "vitest";
import type { Block, ChatMessage } from "../../types/agent";
import type { createAppTestFixtures } from "./appTestFixtures";

export function createAppTestInteractions(
  listenMock: ReturnType<typeof vi.fn>,
  fixtures: ReturnType<typeof createAppTestFixtures>,
) {
  const { escapeRegExp } = fixtures;

  async function openReviewPanel() {
    fireEvent.click(await screen.findByRole("button", { name: "展开右面板" }));
    fireEvent.click(await screen.findByRole("button", { name: "打开 Review" }));
    await screen.findByText("+hello");
  }

  async function configureTeamLead(
    leadName = "Claude Code",
    memberName?: string,
  ) {
    fireEvent.click(
      screen.getByRole("button", { name: `选择 agent：${leadName}` }),
    );
    fireEvent.click(
      screen.getByRole("button", { name: `设为队长 ${leadName}` }),
    );
    const teamTriggerName = new RegExp(
      `选择 agent：队长 ${escapeRegExp(leadName)}`,
    );
    await waitFor(() =>
      expect(
        screen.getByRole("button", { name: teamTriggerName }),
      ).toBeInTheDocument(),
    );

    if (!memberName) return;

    let memberToggle = screen.queryByRole("button", {
      name: `成员 ${memberName}`,
    });
    if (!memberToggle) {
      fireEvent.click(screen.getByRole("button", { name: teamTriggerName }));
      memberToggle = screen.getByRole("button", { name: `成员 ${memberName}` });
    }
    if (memberToggle.getAttribute("aria-pressed") !== "true") {
      fireEvent.click(memberToggle);
    }
    await waitFor(() =>
      expect(
        screen.getByRole("button", {
          name: new RegExp(
            `选择 agent：队长 ${escapeRegExp(leadName)}，成员 1`,
          ),
        }),
      ).toBeInTheDocument(),
    );
  }

  function agentEventCb() {
    const handler = listenMock.mock.calls.find(
      (c) => c[0] === "agent-event",
    )?.[1];
    if (!handler) throw new Error("agent-event listener 未注册");
    return handler as (e: { payload: unknown }) => void;
  }

  function agentEventBatchCb() {
    const handler = listenMock.mock.calls.find(
      (c) => c[0] === "agent-event-batch",
    )?.[1];
    if (!handler) throw new Error("agent-event-batch listener 未注册");
    return handler as (e: { payload: unknown }) => void;
  }

  function emitAgentEventBatch(
    events: Array<{ kind: string; [key: string]: unknown }>,
  ) {
    agentEventBatchCb()({
      payload: {
        batches: [
          {
            session_id: "s1",
            events: events.map((event, index) => ({
              seq: index + 1,
              ...event,
            })),
          },
        ],
      },
    });
  }

  function leadDecisionCardCb() {
    const handler = listenMock.mock.calls.find(
      (c) => c[0] === "lead-decision-card",
    )?.[1];
    if (!handler) throw new Error("lead-decision-card listener 未注册");
    return handler as (e: {
      payload: {
        session_id: string;
        block: Extract<Block, { type: "decision_card" }>;
      };
    }) => void;
  }

  // 决策打扰收敛刀 T1·症状 B：镜像 leadDecisionCardCb，取 lead-message-appended listener
  // 的回调直接手动触发（App 收到后端 append_decision_echo 写库成功的 emit）。
  function leadMessageAppendedCb() {
    const handler = listenMock.mock.calls.find(
      (c) => c[0] === "lead-message-appended",
    )?.[1];
    if (!handler) throw new Error("lead-message-appended listener 未注册");
    return handler as (e: {
      payload: {
        session_id: string;
        message: ChatMessage & { id: number };
      };
    }) => void;
  }

  function decisionCardResolvedCb() {
    const handler = listenMock.mock.calls.find(
      (c) => c[0] === "decision-card-resolved",
    )?.[1];
    if (!handler) throw new Error("decision-card-resolved listener 未注册");
    return handler as (e: {
      payload: {
        session_id: string;
        decision_id: string;
        status: "chosen";
        chosen_option: string | null;
      };
    }) => void;
  }

  function inlineDecisionCard() {
    const card = document.querySelector<HTMLElement>(".decision-card");
    if (!card) throw new Error("inline decision card 未渲染");
    return within(card);
  }

  function findInlineDecisionButton(name: RegExp) {
    return waitFor(() => inlineDecisionCard().getByRole("button", { name }));
  }

  async function clickDecisionOption(option: string) {
    await waitFor(() =>
      expect(document.querySelector(".decision-card")).not.toBeNull(),
    );
    fireEvent.click(
      inlineDecisionCard().getByRole("button", {
        name: new RegExp(escapeRegExp(option)),
      }),
    );
  }

  return {
    openReviewPanel,
    configureTeamLead,
    agentEventCb,
    agentEventBatchCb,
    emitAgentEventBatch,
    leadDecisionCardCb,
    leadMessageAppendedCb,
    decisionCardResolvedCb,
    inlineDecisionCard,
    findInlineDecisionButton,
    clickDecisionOption,
  };
}
