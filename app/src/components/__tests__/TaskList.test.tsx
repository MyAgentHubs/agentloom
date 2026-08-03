import { describe, expect, it } from "vitest";
import { render } from "@testing-library/react";
import { TaskList } from "../../components/TaskList";
import { I18nProvider } from "../../i18n";
import type { MemberUnit } from "../../types/agent";

describe("TaskList", () => {
  it("started_at 5分钟前 → 渲出分钟前文字", () => {
    const member: MemberUnit = {
      participant_id: "w1",
      assignment_id: "a1",
      task_id: "t1",
      name: "w1",
      status: "running",
      sub: "",
      steps_total: 1,
      steps_done: 0,
      cost_usd: null,
      input_tokens: 0,
      output_tokens: 0,
      failed: false,
      blocks: [],
      started_at: Date.now() - 5 * 60 * 1000,
    };

    const { container } = render(
      <I18nProvider initialLocale="zh">
        <TaskList workers={[member]} onSelect={() => {}} onStop={() => {}} />
      </I18nProvider>,
    );

    expect(container.textContent).toContain("分钟前");
    expect(container.querySelector(".tcown")).not.toBeNull();
  });
});
