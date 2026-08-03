import { render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { I18nProvider } from "../i18n";
import type { CodingTaskBlock } from "../types/agent";
import { CodingTaskBar } from "./CodingTaskBar";

const block = {
  type: "coding_task",
  run_id: "r1",
  assignment_id: "a1",
  worker_name: "Codex",
  phase: "ask_apply",
} as CodingTaskBlock;

describe("CodingTaskBar", () => {
  it("在组件端翻译状态与 phase 进展 key", () => {
    render(
      <I18nProvider initialLocale="en">
        <CodingTaskBar block={block} />
      </I18nProvider>,
    );

    expect(screen.getByText("Awaiting input")).toBeInTheDocument();
    expect(
      screen.getByText("Legacy apply confirmation (rerun or leave for now)"),
    ).toBeInTheDocument();
  });
});
