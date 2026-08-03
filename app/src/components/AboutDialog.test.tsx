import { render, screen, fireEvent } from "@testing-library/react";
import { describe, it, expect, vi, beforeEach } from "vitest";
import { AboutDialog } from "./AboutDialog";

vi.mock("@tauri-apps/plugin-opener", () => ({ openUrl: vi.fn() }));

describe("AboutDialog", () => {
  beforeEach(() => {
    // Reset clipboard mock
    Object.defineProperty(navigator, "clipboard", {
      value: {
        writeText: vi.fn().mockResolvedValue(undefined),
      },
      writable: true,
      configurable: true,
    });
  });

  it("open=false 时不渲染", () => {
    const { container } = render(
      <AboutDialog open={false} onClose={() => {}} />,
    );
    expect(container).toBeEmptyDOMElement();
  });

  it("open=true 渲染应用名、版本号和三个链接", () => {
    render(<AboutDialog open={true} onClose={() => {}} />);

    expect(screen.getByText("AgentLoom")).toBeInTheDocument();
    // version text appears in the dialog
    expect(screen.getByText(/^v/)).toBeInTheDocument();
    expect(screen.getByText("www.myagenthubs.com")).toBeInTheDocument();
    expect(
      screen.getByText("github.com/MyAgentHubs/agentloom/issues"),
    ).toBeInTheDocument();
    expect(screen.getByText("panda@myagenthubs.com")).toBeInTheDocument();
  });

  it("按 Esc 触发 onClose", () => {
    const onClose = vi.fn();
    render(<AboutDialog open={true} onClose={onClose} />);

    fireEvent.keyDown(document, { key: "Escape" });
    expect(onClose).toHaveBeenCalledTimes(1);
  });

  it("点遮罩触发 onClose；点卡片内部不触发 onClose", () => {
    const onClose = vi.fn();
    render(<AboutDialog open={true} onClose={onClose} />);
    const backdrop = document.querySelector(".dialog__backdrop")!;
    const dialog = document.querySelector('[role="dialog"]')!;

    fireEvent.click(dialog);
    expect(onClose).not.toHaveBeenCalled();

    fireEvent.click(backdrop);
    expect(onClose).toHaveBeenCalledTimes(1);
  });
});
