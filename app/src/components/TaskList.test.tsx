import { describe, it, expect, vi } from "vitest";
import { render } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { TaskList } from "./TaskList";
import { I18nProvider } from "../i18n";
import type { MemberUnit } from "../types/agent";

const makeWorker = (aid: string, status: MemberUnit["status"]): MemberUnit => ({
  participant_id: aid,
  assignment_id: aid,
  task_id: aid,
  name: `Worker ${aid}`,
  status,
  sub: `subtask for ${aid}`,
  steps_total: 0,
  steps_done: 0,
  cost_usd: null,
  input_tokens: 0,
  output_tokens: 0,
  failed: status === "failed",
  blocks: [],
});

describe("TaskList", () => {
  const workers: MemberUnit[] = [
    makeWorker("a1", "running"),
    makeWorker("a2", "needs_input"),
    makeWorker("a3", "done"),
  ];

  it("renders tstate classes correctly", () => {
    const { container } = render(
      <I18nProvider>
        <TaskList workers={workers} onSelect={vi.fn()} onStop={vi.fn()} />
      </I18nProvider>,
    );
    const cards = container.querySelectorAll(".task-card");
    expect(cards).toHaveLength(3);
    expect(cards[0].querySelector(".tstate.run")).toBeTruthy();
    expect(cards[1].querySelector(".tstate.wait")).toBeTruthy();
    expect(cards[2].querySelector(".tstate.done")).toBeTruthy();
  });

  it("only running worker has .tc-stop button", () => {
    const { container } = render(
      <I18nProvider>
        <TaskList workers={workers} onSelect={vi.fn()} onStop={vi.fn()} />
      </I18nProvider>,
    );
    const stopBtns = container.querySelectorAll(".tc-stop");
    expect(stopBtns).toHaveLength(1);
    const firstCard = container.querySelectorAll(".task-card")[0];
    expect(firstCard.querySelector(".tc-stop")).toBeTruthy();
  });

  it("clicking a card calls onSelect with assignment_id", async () => {
    const onSelect = vi.fn();
    const { container } = render(
      <I18nProvider>
        <TaskList workers={workers} onSelect={onSelect} onStop={vi.fn()} />
      </I18nProvider>,
    );
    const cards = container.querySelectorAll(".task-card");
    await userEvent.click(cards[1] as HTMLElement);
    expect(onSelect).toHaveBeenCalledWith("a2");
  });

  it("clicking tc-stop calls onStop and does NOT call onSelect", async () => {
    const onSelect = vi.fn();
    const onStop = vi.fn();
    const { container } = render(
      <I18nProvider>
        <TaskList workers={workers} onSelect={onSelect} onStop={onStop} />
      </I18nProvider>,
    );
    const stopBtn = container.querySelector(".tc-stop") as HTMLElement;
    await userEvent.click(stopBtn);
    expect(onStop).toHaveBeenCalledWith("a1");
    expect(onSelect).not.toHaveBeenCalled();
  });
});
