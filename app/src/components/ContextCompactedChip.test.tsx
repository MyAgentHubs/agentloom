import { render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { I18nProvider } from "../i18n";
import {
  ContextCompactedChip,
  type ContextChipType,
} from "./ContextCompactedChip";

function renderChip(locale: "zh" | "en", blockType?: ContextChipType) {
  return render(
    <I18nProvider initialLocale={locale}>
      <ContextCompactedChip blockType={blockType} />
    </I18nProvider>,
  );
}

describe("ContextCompactedChip", () => {
  it("context_compacted_zh renders the compacted hint", () => {
    const { container } = renderChip("zh");

    expect(container.querySelector(".context-compacted-chip")).not.toBeNull();
    expect(screen.getByText("会话上下文已自动压实")).toBeInTheDocument();
  });

  it("context_compacted_en renders the localized compacted hint", () => {
    renderChip("en");

    expect(
      screen.getByText("Conversation context compacted"),
    ).toBeInTheDocument();
  });

  it("context_truncated_zh renders the truncated hint in the warning tone", () => {
    const { container } = renderChip("zh", "context_truncated");

    const chip = container.querySelector(".context-truncated-chip");
    expect(chip).not.toBeNull();
    // 截断是有损提示 → 用琥珀警示墨色，与压实的静默 ink-3 区分
    expect(chip as HTMLElement).toHaveStyle({ color: "var(--amber-ink)" });
    expect(container.querySelector(".context-compacted-chip")).toBeNull();
    expect(
      screen.getByText("上下文超出模型窗口，已截断部分早期内容"),
    ).toBeInTheDocument();
  });

  it("context_truncated_en renders the localized truncated hint", () => {
    renderChip("en", "context_truncated");

    expect(
      screen.getByText(
        "Context exceeded the model window; some earlier content was truncated",
      ),
    ).toBeInTheDocument();
  });
});
