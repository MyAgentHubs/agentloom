import { render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { I18nProvider } from "../i18n";
import { PreviewPanel } from "./PreviewPanel";

declare const process: { env: { VITEST_DEFER_INVOKE?: string } };

const invokeMock = vi.fn();

// VITEST_DEFER_INVOKE=1 makes every invoke settle one macrotask later, which
// deterministically exposes assertions that read state landing from a *different*
// async source than the one they awaited. CI runners are ~12x slower than a dev
// machine and lose those races for real; this switch reproduces it on purpose.
function __deferInvoke<T>(p: T): T | Promise<Awaited<T>> {
  return process.env.VITEST_DEFER_INVOKE
    ? new Promise((r) => setTimeout(r, 0)).then(() => p as Promise<Awaited<T>>)
    : p;
}

vi.mock("@tauri-apps/api/core", () => ({
  invoke: (...a: unknown[]) =>
    a[0] === "set_ui_locale"
      ? __deferInvoke(Promise.resolve())
      : __deferInvoke(invokeMock(...a)),
}));
vi.mock("./MessageContent", () => ({
  MessageContent: ({ blocks }: { blocks: { text: string }[] }) => (
    <div data-testid="md">{blocks[0].text}</div>
  ),
}));
vi.mock("./CodeBlock", () => ({
  CodeBlock: ({ code, lang }: { code: string; lang?: string }) => (
    <div data-testid="code" data-lang={lang}>
      {code}
    </div>
  ),
}));

function renderPanel(path: string | null) {
  return render(
    <I18nProvider initialLocale="zh">
      <PreviewPanel path={path} />
    </I18nProvider>,
  );
}

beforeEach(() => {
  invokeMock.mockReset();
});

describe("PreviewPanel", () => {
  it("renders Markdown text with MessageContent", async () => {
    invokeMock.mockResolvedValueOnce({
      name: "a.md",
      kind: "text",
      content: "# Title",
      truncated: false,
      byteLen: 7,
    });

    renderPanel("a.md");

    await waitFor(() => {
      expect(screen.getByTestId("md")).toHaveTextContent("# Title");
    });
  });

  it("renders TypeScript text with the TypeScript language hint", async () => {
    invokeMock.mockResolvedValueOnce({
      name: "a.ts",
      kind: "text",
      content: "const x=1",
      truncated: false,
      byteLen: 9,
    });

    renderPanel("a.ts");

    await waitFor(() => {
      const code = screen.getByTestId("code");
      expect(code).toHaveAttribute("data-lang", "typescript");
      expect(code).toHaveTextContent("const x=1");
    });
  });

  it("renders SVG text as an image data URL", async () => {
    invokeMock.mockResolvedValueOnce({
      name: "a.svg",
      kind: "text",
      content: "<svg/>",
      truncated: false,
      byteLen: 6,
    });

    const { container } = renderPanel("a.svg");

    await waitFor(() => {
      expect(container.querySelector("img")?.getAttribute("src")).toMatch(
        /^data:image\/svg\+xml/,
      );
    });
  });

  it("renders a bitmap image from backend base64 data", async () => {
    invokeMock.mockResolvedValueOnce({
      name: "a.png",
      kind: "image",
      content: "",
      truncated: false,
      byteLen: 100,
      imageBase64: "iVBORw0KGgo=",
      mediaType: "image/png",
    });

    renderPanel("a.png");

    await waitFor(() => {
      expect(screen.getByRole("img", { name: "a.png" })).toHaveAttribute(
        "src",
        "data:image/png;base64,iVBORw0KGgo=",
      );
    });
  });

  it("renders the unavailable placeholder when image bytes are absent", async () => {
    invokeMock.mockResolvedValueOnce({
      name: "a.png",
      kind: "image",
      content: "",
      truncated: false,
      byteLen: 100,
    });

    renderPanel("a.png");

    await waitFor(() => {
      expect(screen.getByText("图片无法预览")).toBeInTheDocument();
    });
  });

  it("renders the error state when the backend rejects", async () => {
    invokeMock.mockRejectedValueOnce(new Error("boom"));

    renderPanel("a.bin");

    await waitFor(() => {
      expect(screen.getByText(/无法打开/)).toBeInTheDocument();
    });
  });

  it("renders a localized detail for an AL_ERR envelope", async () => {
    invokeMock.mockRejectedValueOnce(
      'AL_ERR:file.ambiguousBasename:{"0":"logo.png","1":"src/logo.png, public/logo.png"}',
    );

    renderPanel("logo.png");

    expect(
      await screen.findByText(
        "同名文件有多个，请用更完整的路径：logo.png → src/logo.png, public/logo.png",
      ),
    ).toBeInTheDocument();
    expect(screen.queryByText(/AL_ERR:/)).not.toBeInTheDocument();
  });

  it("renders a plain backend error detail unchanged", async () => {
    const detail =
      "cannot read file metadata /x/y.png: No such file or directory (os error 2)";
    invokeMock.mockRejectedValueOnce(detail);

    renderPanel("/x/y.png");

    expect(await screen.findByText(detail)).toBeInTheDocument();
  });

  it("truncates a long backend error detail to 300 characters", async () => {
    const detail = "x".repeat(301);
    invokeMock.mockRejectedValueOnce(detail);

    const { container } = renderPanel("a.bin");

    await screen.findByText("x".repeat(300));
    expect(
      container.querySelector(".preview-panel__error-detail"),
    ).toHaveTextContent(/^x{300}$/);
  });

  it("omits the detail line when the backend error is empty", async () => {
    invokeMock.mockRejectedValueOnce("");

    const { container } = renderPanel("a.bin");

    await screen.findByText(/无法打开/);
    expect(container.querySelector(".preview-panel__error-detail")).toBeNull();
  });

  it("renders the empty state without invoking the backend", () => {
    renderPanel(null);

    expect(screen.getByText("选择一个文件预览")).toBeInTheDocument();
    expect(invokeMock).not.toHaveBeenCalled();
  });
});
