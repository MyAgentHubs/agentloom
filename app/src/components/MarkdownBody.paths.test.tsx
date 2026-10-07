import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { MarkdownBody } from "./MarkdownBody";

const writeText = vi.fn();
const invoke = vi.fn();
vi.mock("@tauri-apps/api/core", () => ({
  invoke: (...args: unknown[]) => invoke(...args),
}));

beforeEach(() => {
  writeText.mockReset().mockResolvedValue(undefined);
  invoke.mockReset().mockRejectedValue(new Error("outside workspace"));
  vi.stubGlobal("navigator", { clipboard: { writeText } });
});
afterEach(() => vi.unstubAllGlobals());

describe("chat path copying", () => {
  it.each([
    [
      "[report](reports/monthly%20report.md)",
      "report",
      "reports/monthly report.md",
    ],
    ["`/tmp/missing.txt`", "/tmp/missing.txt", "/tmp/missing.txt"],
    ["[报告](outputs/报告.csv)", "报告", "outputs/报告.csv"],
    ["[document](outputs/report.pdf)", "document", "outputs/report.pdf"],
  ])(
    "copies the reference without reading or opening it: %s",
    async (source, label, path) => {
      const onOpenPreview = vi.fn();
      render(
        <MarkdownBody streaming={false} onOpenPreview={onOpenPreview}>
          {source}
        </MarkdownBody>,
      );
      fireEvent.contextMenu(screen.getByText(label));
      fireEvent.click(screen.getByRole("menuitem", { name: "复制路径" }));
      await waitFor(() => expect(writeText).toHaveBeenCalledWith(path));
      expect(onOpenPreview).not.toHaveBeenCalled();
      expect(invoke).not.toHaveBeenCalled();
      expect(screen.getByRole("status")).toHaveTextContent("路径已复制");
    },
  );

  it("works without a preview callback and supports keyboard dismissal", () => {
    render(<MarkdownBody streaming={false}>{"`report.md`"}</MarkdownBody>);
    const path = screen.getByText("report.md");
    path.focus();
    fireEvent.keyDown(path, { key: "F10", shiftKey: true });
    expect(screen.getByRole("menu")).toBeInTheDocument();
    fireEvent.keyDown(document, { key: "Escape" });
    expect(screen.queryByRole("menu")).not.toBeInTheDocument();
    expect(path).toHaveFocus();
  });

  it("reports clipboard failure", async () => {
    writeText.mockRejectedValueOnce(new Error("denied"));
    render(
      <MarkdownBody streaming={false}>{"[file](report.md)"}</MarkdownBody>,
    );
    fireEvent.contextMenu(screen.getByRole("link"));
    fireEvent.click(screen.getByRole("menuitem"));
    await waitFor(() =>
      expect(screen.getByRole("status")).toHaveTextContent("复制失败"),
    );
  });

  it("copies a loaded local image path", async () => {
    invoke.mockResolvedValueOnce({
      kind: "image",
      imageBase64: "aW1hZ2U=",
      mediaType: "image/png",
    });
    render(
      <MarkdownBody streaming={false}>
        {"![chart](/tmp/path-copy-chart.png)"}
      </MarkdownBody>,
    );
    const image = await screen.findByRole("img", { name: "chart" });
    fireEvent.contextMenu(image);
    fireEvent.click(screen.getByRole("menuitem", { name: "复制路径" }));
    await waitFor(() =>
      expect(writeText).toHaveBeenCalledWith("/tmp/path-copy-chart.png"),
    );
  });

  it("copies a failed image reference without trying to read it again", async () => {
    render(
      <MarkdownBody streaming={false}>
        {"![missing](/tmp/path-copy-denied.png)"}
      </MarkdownBody>,
    );
    const path = await screen.findByText("/tmp/path-copy-denied.png", {
      selector: "code",
    });
    const reads = invoke.mock.calls.length;
    fireEvent.contextMenu(path);
    fireEvent.click(screen.getByRole("menuitem"));
    await waitFor(() =>
      expect(writeText).toHaveBeenCalledWith("/tmp/path-copy-denied.png"),
    );
    expect(invoke).toHaveBeenCalledTimes(reads);
  });

  it("leaves external links and non-path code alone", () => {
    render(
      <MarkdownBody streaming={false}>
        {"[site](https://example.com/report.md) and `array.map()`"}
      </MarkdownBody>,
    );
    fireEvent.contextMenu(screen.getByRole("link"));
    expect(screen.queryByRole("menu")).not.toBeInTheDocument();
    fireEvent.contextMenu(screen.getByText("array.map()"));
    expect(screen.queryByRole("menu")).not.toBeInTheDocument();
  });
});
