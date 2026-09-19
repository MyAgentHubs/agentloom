import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { openUrl } from "@tauri-apps/plugin-opener";
import { clearAttachmentCache } from "../lib/attachmentCache";
import { AttachmentPortContext } from "../lib/attachmentPortContext";
import type { AttachmentPort } from "../lib/remoteSessionPort";
import { MarkdownBody } from "./MarkdownBody";

const invokeMock = vi.fn();

vi.mock("@tauri-apps/api/core", () => ({
  invoke: (...args: unknown[]) => invokeMock(...args),
}));

vi.mock("@tauri-apps/plugin-opener", () => ({
  openUrl: vi.fn().mockResolvedValue(undefined),
}));

beforeEach(() => {
  invokeMock.mockReset();
  vi.mocked(openUrl).mockClear();
  clearAttachmentCache();
});

function stubAttachmentPort(
  overrides: Partial<AttachmentPort> = {},
): AttachmentPort {
  return {
    resolveImageSrc: vi.fn().mockResolvedValue(null),
    openExternal: vi.fn().mockResolvedValue(undefined),
    openUrl: vi.fn().mockResolvedValue(undefined),
    ...overrides,
  };
}

describe("MarkdownBody links", () => {
  it.each([
    ["relative", "[report](artifacts/report.html)", "artifacts/report.html"],
    ["absolute", "[report](</tmp/report.HTML>)", "/tmp/report.HTML"],
  ])(
    "opens a %s markdown HTML link externally",
    async (_kind, markdown, path) => {
      invokeMock.mockResolvedValueOnce(undefined);
      const onOpenPreview = vi.fn();

      render(
        <MarkdownBody
          streaming={false}
          sessionId="session-html"
          onOpenPreview={onOpenPreview}
        >
          {markdown}
        </MarkdownBody>,
      );

      fireEvent.click(screen.getByRole("link", { name: "report" }));

      await waitFor(() =>
        expect(invokeMock).toHaveBeenCalledWith("open_attachment_external", {
          sessionId: "session-html",
          path,
        }),
      );
      expect(onOpenPreview).not.toHaveBeenCalled();
    },
  );

  it.each([
    [
      "image",
      "[campus](morning-school-campus.jpg)",
      "morning-school-campus.jpg",
    ],
    ["text", "[notes](notes.md)", "notes.md"],
  ])("previews a local %s markdown link", (_kind, markdown, path) => {
    const onOpenPreview = vi.fn();

    render(
      <MarkdownBody streaming={false} onOpenPreview={onOpenPreview}>
        {markdown}
      </MarkdownBody>,
    );

    fireEvent.click(screen.getByRole("link"));

    expect(onOpenPreview).toHaveBeenCalledWith(path);
    expect(invokeMock).not.toHaveBeenCalled();
    expect(openUrl).not.toHaveBeenCalled();
  });

  it.each(["http://example.com/report", "https://example.com/report"])(
    "keeps opening an external markdown link with openUrl: %s",
    (href) => {
      const onOpenPreview = vi.fn();

      render(
        <MarkdownBody streaming={false} onOpenPreview={onOpenPreview}>
          {`[external](${href})`}
        </MarkdownBody>,
      );

      fireEvent.click(screen.getByRole("link", { name: "external" }));

      expect(openUrl).toHaveBeenCalledWith(href);
      expect(onOpenPreview).not.toHaveBeenCalled();
      expect(invokeMock).not.toHaveBeenCalled();
    },
  );

  it.each([
    ["anchor", "[anchor](#details)"],
    ["empty href", "[empty]()"],
    ["mailto", "[email](mailto:report.html)"],
  ])("ignores a non-file %s markdown link", (_kind, markdown) => {
    const onOpenPreview = vi.fn();

    render(
      <MarkdownBody streaming={false} onOpenPreview={onOpenPreview}>
        {markdown}
      </MarkdownBody>,
    );

    const link = screen
      .getByText(_kind === "empty href" ? "empty" : /.+/)
      .closest("a");
    expect(link).not.toBeNull();
    fireEvent.click(link!);

    expect(openUrl).not.toHaveBeenCalled();
    expect(onOpenPreview).not.toHaveBeenCalled();
    expect(invokeMock).not.toHaveBeenCalled();
  });

  it("decodes an escaped markdown file link before opening it", async () => {
    invokeMock.mockResolvedValueOnce(undefined);

    render(
      <MarkdownBody streaming={false} sessionId="session-escaped">
        {"[report](artifacts/campus%20handoff.htm)"}
      </MarkdownBody>,
    );

    fireEvent.click(screen.getByRole("link", { name: "report" }));

    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("open_attachment_external", {
        sessionId: "session-escaped",
        path: "artifacts/campus handoff.htm",
      }),
    );
  });

  it("shows the existing backend error shape when external opening fails", async () => {
    invokeMock.mockRejectedValueOnce(
      'AL_ERR:file.openExternalFailed:{"detail":"boom"}',
    );

    render(
      <MarkdownBody streaming={false} sessionId="session-error">
        {"[report](report.html)"}
      </MarkdownBody>,
    );

    fireEvent.click(screen.getByRole("link", { name: "report" }));

    expect(
      await screen.findByRole("status", {
        name: "无法在系统浏览器打开文件：boom",
      }),
    ).toBeInTheDocument();
  });
});

describe("MarkdownBody images", () => {
  it("loads a relative image path through read_attachment", async () => {
    invokeMock.mockResolvedValueOnce({
      kind: "image",
      imageBase64: "cmVsYXRpdmU=",
      mediaType: "image/png",
    });

    render(
      <MarkdownBody streaming={false} sessionId="session-relative">
        {"![chart](assets/x.png)"}
      </MarkdownBody>,
    );

    expect(await screen.findByRole("img", { name: "chart" })).toHaveAttribute(
      "src",
      "data:image/png;base64,cmVsYXRpdmU=",
    );
    expect(invokeMock).toHaveBeenCalledWith("read_attachment", {
      path: "assets/x.png",
      sessionId: "session-relative",
    });
  });

  it("renders HTTPS images directly without overflowing", () => {
    render(
      <MarkdownBody streaming={false}>
        {"![remote](https://example.com/image.png)"}
      </MarkdownBody>,
    );

    expect(screen.getByRole("img", { name: "remote" })).toHaveAttribute(
      "src",
      "https://example.com/image.png",
    );
    // 统一样式类（规则 C）——尺寸约束交给 .al-chat-image，不再各处内联。
    expect(screen.getByRole("img", { name: "remote" })).toHaveClass(
      "al-chat-image",
    );
    expect(invokeMock).not.toHaveBeenCalled();
  });

  it("renders data images directly without reading an attachment", () => {
    render(
      <MarkdownBody streaming={false}>
        {"![inline](data:image/png;base64,aW5saW5l)"}
      </MarkdownBody>,
    );

    expect(screen.getByRole("img", { name: "inline" })).toBeInTheDocument();
    expect(invokeMock).not.toHaveBeenCalled();
  });

  it("loads an absolute local image path through read_attachment", async () => {
    invokeMock.mockResolvedValueOnce({
      name: "chart.png",
      kind: "image",
      content: "",
      truncated: false,
      byteLen: 8,
      imageBase64: "iVBORw0KGgo=",
      mediaType: "image/png",
    });

    render(
      <MarkdownBody streaming={false} sessionId="session-1">
        {"![chart](/tmp/chart.png)"}
      </MarkdownBody>,
    );

    await waitFor(() => {
      expect(screen.getByRole("img", { name: "chart" })).toHaveAttribute(
        "src",
        "data:image/png;base64,iVBORw0KGgo=",
      );
    });
    expect(invokeMock).toHaveBeenCalledWith("read_attachment", {
      path: "/tmp/chart.png",
      sessionId: "session-1",
    });
  });

  it("reuses a cached local image immediately after remounting", async () => {
    invokeMock.mockResolvedValueOnce({
      kind: "image",
      imageBase64: "Y2FjaGVk",
      mediaType: "image/png",
    });
    const props = {
      children: "![chart](/tmp/cached-chart.png)",
      sessionId: "session-cache",
      streaming: false,
    };

    const first = render(<MarkdownBody {...props} />);
    await screen.findByRole("img", { name: "chart" });
    first.unmount();

    render(<MarkdownBody {...props} />);

    expect(screen.getByRole("img", { name: "chart" })).toHaveAttribute(
      "src",
      "data:image/png;base64,Y2FjaGVk",
    );
    expect(invokeMock).toHaveBeenCalledTimes(1);
  });

  it("opens a loaded local image in the lightbox", async () => {
    invokeMock.mockResolvedValueOnce({
      kind: "image",
      imageBase64: "aW1hZ2U=",
      mediaType: "image/png",
    });
    const onOpenLightbox = vi.fn();

    render(
      <MarkdownBody
        streaming={false}
        sessionId="session-lightbox"
        onOpenLightbox={onOpenLightbox}
      >
        {"![chart](/tmp/chart%20large.png)"}
      </MarkdownBody>,
    );

    fireEvent.click(await screen.findByRole("img", { name: "chart" }));

    expect(onOpenLightbox).toHaveBeenCalledWith("/tmp/chart large.png");
  });

  it.each([
    ["![x](</Users/a/my pic.png>)", "/Users/a/my pic.png"],
    ["![x](/Users/a/pic%20x.png)", "/Users/a/pic x.png"],
    ["![](</path/with space.png>)", "/path/with space.png"],
  ])(
    "decodes a local image path before reading it: %s",
    async (markdown, path) => {
      invokeMock.mockResolvedValueOnce({
        kind: "image",
        imageBase64: "iVBORw0KGgo=",
        mediaType: "image/png",
      });

      render(<MarkdownBody streaming={false}>{markdown}</MarkdownBody>);

      await waitFor(() => {
        expect(invokeMock).toHaveBeenCalledWith("read_attachment", {
          path,
          sessionId: null,
        });
      });
    },
  );

  it("loads a file:// image path through read_attachment (scheme stripped)", async () => {
    invokeMock.mockResolvedValueOnce({
      kind: "image",
      imageBase64: "ZmlsZQ==",
      mediaType: "image/png",
    });

    render(
      <MarkdownBody streaming={false} sessionId="session-file">
        {"![chart](file:///Users/a/chart.png)"}
      </MarkdownBody>,
    );

    expect(await screen.findByRole("img", { name: "chart" })).toHaveAttribute(
      "src",
      "data:image/png;base64,ZmlsZQ==",
    );
    expect(invokeMock).toHaveBeenCalledWith("read_attachment", {
      path: "/Users/a/chart.png",
      sessionId: "session-file",
    });
  });

  it.each([
    ["![x](file:///Users/a/my%20pic.png)", "/Users/a/my pic.png"],
    ["![x](file:///Users/a/%E4%B8%AD%E6%96%87.png)", "/Users/a/中文.png"],
  ])(
    "decodes percent-encoding in a file:// image path before reading it: %s",
    async (markdown, path) => {
      invokeMock.mockResolvedValueOnce({
        kind: "image",
        imageBase64: "iVBORw0KGgo=",
        mediaType: "image/png",
      });

      render(<MarkdownBody streaming={false}>{markdown}</MarkdownBody>);

      await waitFor(() => {
        expect(invokeMock).toHaveBeenCalledWith("read_attachment", {
          path,
          sessionId: null,
        });
      });
    },
  );

  it("falls back to a clickable preview path when local loading fails", async () => {
    invokeMock.mockRejectedValueOnce(new Error("boom"));
    const onOpenPreview = vi.fn();
    const onOpenLightbox = vi.fn();

    render(
      <MarkdownBody
        streaming={false}
        sessionId="session-2"
        onOpenPreview={onOpenPreview}
        onOpenLightbox={onOpenLightbox}
      >
        {"![chart](~/chart.png)"}
      </MarkdownBody>,
    );

    const fallback = await screen.findByRole("button", {
      name: "~/chart.png",
    });
    fireEvent.click(fallback);

    expect(onOpenPreview).toHaveBeenCalledWith("~/chart.png");
    expect(onOpenLightbox).not.toHaveBeenCalled();
  });

  it("does not create an executable image src", () => {
    render(
      <MarkdownBody streaming={false}>
        {"![x](javascript:alert(1))"}
      </MarkdownBody>,
    );

    expect(screen.getByRole("img", { name: "x" })).not.toHaveAttribute("src");
    expect(invokeMock).not.toHaveBeenCalled();
  });
});

describe("MarkdownBody bare image paths", () => {
  it("renders a standalone absolute image path through read_attachment", async () => {
    invokeMock.mockResolvedValueOnce({
      kind: "image",
      imageBase64: "YmFyZS1hYnNvbHV0ZQ==",
      mediaType: "image/png",
    });

    const { container } = render(
      <MarkdownBody
        streaming={false}
        autoInlineImagePaths={true}
        sessionId="session-bare-absolute"
      >
        {"\n\n/abs/path/chart.png\n\n"}
      </MarkdownBody>,
    );

    await waitFor(() => {
      expect(container.querySelector("img")).toHaveAttribute(
        "src",
        "data:image/png;base64,YmFyZS1hYnNvbHV0ZQ==",
      );
    });
    expect(invokeMock).toHaveBeenCalledWith("read_attachment", {
      path: "/abs/path/chart.png",
      sessionId: "session-bare-absolute",
    });
  });

  it("strips file:// from a standalone image path before reading it", async () => {
    invokeMock.mockResolvedValueOnce({
      kind: "image",
      imageBase64: "YmFyZS1maWxl",
      mediaType: "image/svg+xml",
    });

    const { container } = render(
      <MarkdownBody
        streaming={false}
        autoInlineImagePaths={true}
        sessionId="session-bare-file"
      >
        {"\n\nfile:///abs/x.svg\n\n"}
      </MarkdownBody>,
    );

    await waitFor(() => {
      expect(container.querySelector("img")).toHaveAttribute(
        "src",
        "data:image/svg+xml;base64,YmFyZS1maWxl",
      );
    });
    expect(invokeMock).toHaveBeenCalledWith("read_attachment", {
      path: "/abs/x.svg",
      sessionId: "session-bare-file",
    });
  });

  it("inlines a mid-sentence bare path below the unchanged paragraph text（规则 B②）", async () => {
    invokeMock.mockResolvedValueOnce({
      kind: "image",
      imageBase64: "bWlkc2VudGVuY2U=",
      mediaType: "image/png",
    });

    const { container } = render(
      <MarkdownBody
        streaming={false}
        autoInlineImagePaths={true}
        sessionId="session-mid-sentence"
      >
        {"\n\n结果在 /a/b.png 里\n\n"}
      </MarkdownBody>,
    );

    expect(screen.getByText(/结果在 \/a\/b\.png 里/)).toBeInTheDocument();
    await waitFor(() => {
      expect(container.querySelector("img")).toHaveAttribute(
        "src",
        "data:image/png;base64,bWlkc2VudGVuY2U=",
      );
    });
    expect(invokeMock).toHaveBeenCalledWith("read_attachment", {
      path: "/a/b.png",
      sessionId: "session-mid-sentence",
    });
  });

  it("appends an inlined image below a backtick-wrapped image path (规则 B①)", async () => {
    invokeMock.mockResolvedValueOnce({
      kind: "image",
      imageBase64: "YmFja3RpY2s=",
      mediaType: "image/png",
    });

    const { container } = render(
      <MarkdownBody
        streaming={false}
        autoInlineImagePaths={true}
        sessionId="session-backtick"
      >
        {"\n\n`/a/b.png`\n\n"}
      </MarkdownBody>,
    );

    // 原文不改——反引号内的路径仍原样渲成可点 <code>。
    const code = container.querySelector("code");
    expect(code?.textContent).toBe("/a/b.png");
    // 规则 B：段落下方追加一块内联图。
    await waitFor(() => {
      expect(container.querySelector("img")).toHaveAttribute(
        "src",
        "data:image/png;base64,YmFja3RpY2s=",
      );
    });
    expect(invokeMock).toHaveBeenCalledWith("read_attachment", {
      path: "/a/b.png",
      sessionId: "session-backtick",
    });
  });

  it("keeps a standalone non-image path as text", () => {
    render(
      <MarkdownBody streaming={false} autoInlineImagePaths={true}>
        {"\n\n/a/b.txt\n\n"}
      </MarkdownBody>,
    );

    expect(screen.queryByRole("img")).not.toBeInTheDocument();
    expect(screen.getByText("/a/b.txt")).toBeInTheDocument();
  });

  it("keeps a standalone relative image path as text", () => {
    render(
      <MarkdownBody streaming={false} autoInlineImagePaths={true}>
        {"\n\n./a.png\n\n"}
      </MarkdownBody>,
    );

    expect(screen.queryByRole("img")).not.toBeInTheDocument();
    expect(screen.getByText("./a.png")).toBeInTheDocument();
  });

  it("falls back to the path text when a bare image fails to load", async () => {
    invokeMock.mockRejectedValueOnce(new Error("boom"));

    render(
      <MarkdownBody streaming={false} autoInlineImagePaths={true}>
        {"\n\n/a/fail.png\n\n"}
      </MarkdownBody>,
    );

    await waitFor(() => {
      expect(screen.queryByRole("status")).not.toBeInTheDocument();
    });
    // 原段落文本 + 追加块失败后的 fallback 各渲出一份同样的路径文本。
    expect(screen.getAllByText("/a/fail.png").length).toBeGreaterThanOrEqual(2);
    expect(screen.queryByRole("img")).not.toBeInTheDocument();
  });

  it("does not inline a protocol-relative bare path (//host/a.png)", () => {
    render(
      <MarkdownBody streaming={false} autoInlineImagePaths={true}>
        {"\n\n//host/a.png\n\n"}
      </MarkdownBody>,
    );

    expect(screen.queryByRole("img")).not.toBeInTheDocument();
    expect(screen.getByText("//host/a.png")).toBeInTheDocument();
  });

  it("inlines a bare path after trimming trailing punctuation (/a/b.png。规则 B 规范化)", async () => {
    invokeMock.mockResolvedValueOnce({
      kind: "image",
      imageBase64: "dHJhaWxpbmc=",
      mediaType: "image/png",
    });

    const { container } = render(
      <MarkdownBody
        streaming={false}
        autoInlineImagePaths={true}
        sessionId="session-trailing-punct"
      >
        {"\n\n/a/b.png。\n\n"}
      </MarkdownBody>,
    );

    // 原文不改——句末全角句号仍在段落文本里。
    expect(screen.getByText("/a/b.png。")).toBeInTheDocument();
    await waitFor(() => {
      expect(container.querySelector("img")).toHaveAttribute(
        "src",
        "data:image/png;base64,dHJhaWxpbmc=",
      );
    });
    // 规范化：出图/read_attachment 用的是裁掉尾随句号后的路径。
    expect(invokeMock).toHaveBeenCalledWith("read_attachment", {
      path: "/a/b.png",
      sessionId: "session-trailing-punct",
    });
  });

  it("does not inline a bare path with a query string (/a/b.png?x=1)", () => {
    render(
      <MarkdownBody streaming={false} autoInlineImagePaths={true}>
        {"\n\n/a/b.png?x=1\n\n"}
      </MarkdownBody>,
    );

    expect(screen.queryByRole("img")).not.toBeInTheDocument();
    expect(screen.getByText("/a/b.png?x=1")).toBeInTheDocument();
  });

  it("inlines a bare path with an uppercase .SVG extension", async () => {
    invokeMock.mockResolvedValueOnce({
      kind: "image",
      imageBase64: "dXBwZXJjYXNl",
      mediaType: "image/svg+xml",
    });

    const { container } = render(
      <MarkdownBody
        streaming={false}
        autoInlineImagePaths={true}
        sessionId="session-svg-upper"
      >
        {"\n\n/a/b.SVG\n\n"}
      </MarkdownBody>,
    );

    await waitFor(() => {
      expect(container.querySelector("img")).toHaveAttribute(
        "src",
        "data:image/svg+xml;base64,dXBwZXJjYXNl",
      );
    });
    expect(invokeMock).toHaveBeenCalledWith("read_attachment", {
      path: "/a/b.SVG",
      sessionId: "session-svg-upper",
    });
  });

  it("inlines a bare path mixed with bold text below the paragraph (**粗体** /a/b.png，规则 B②)", async () => {
    invokeMock.mockResolvedValueOnce({
      kind: "image",
      imageBase64: "Ym9sZA==",
      mediaType: "image/png",
    });

    const { container } = render(
      <MarkdownBody
        streaming={false}
        autoInlineImagePaths={true}
        sessionId="session-bold-mixed"
      >
        {"\n\n**粗体** /a/b.png\n\n"}
      </MarkdownBody>,
    );

    // 原文不改——加粗文字与路径文字都还在同一个 <p> 里。
    const paragraph = container.querySelector("p");
    expect(paragraph?.querySelector("strong")?.textContent).toBe("粗体");
    expect(paragraph?.textContent).toContain("/a/b.png");
    await waitFor(() => {
      expect(container.querySelector("img")).toHaveAttribute(
        "src",
        "data:image/png;base64,Ym9sZA==",
      );
    });
    expect(invokeMock).toHaveBeenCalledWith("read_attachment", {
      path: "/a/b.png",
      sessionId: "session-bold-mixed",
    });
  });

  it("does not inline a file:// URL with a non-empty host", () => {
    render(
      <MarkdownBody streaming={false} autoInlineImagePaths={true}>
        {"\n\nfile://host/a.png\n\n"}
      </MarkdownBody>,
    );

    expect(screen.queryByRole("img")).not.toBeInTheDocument();
    expect(screen.getByText("file://host/a.png")).toBeInTheDocument();
  });

  it("splits one text line and one bare-path line into text + inlined image (按行切分)", async () => {
    invokeMock.mockResolvedValueOnce({
      kind: "image",
      imageBase64: "bGluZTE=",
      mediaType: "image/png",
    });

    const { container } = render(
      <MarkdownBody
        streaming={false}
        autoInlineImagePaths={true}
        sessionId="session-line-split"
      >
        {"\n\n一句话\n/a/b.png\n\n"}
      </MarkdownBody>,
    );

    // 原文不改——两行仍是同一个 <p> 里的原样文本（含内部换行）。
    expect(container.querySelector("p")?.textContent).toContain("一句话");
    await waitFor(() => {
      expect(container.querySelectorAll("img")).toHaveLength(1);
    });
    expect(invokeMock).toHaveBeenCalledWith("read_attachment", {
      path: "/a/b.png",
      sessionId: "session-line-split",
    });
  });

  it("inlines two bare-path lines with no separating text into two images (按行切分)", async () => {
    invokeMock.mockResolvedValue({
      kind: "image",
      imageBase64: "dHdvbGluZXM=",
      mediaType: "image/png",
    });

    const { container } = render(
      <MarkdownBody
        streaming={false}
        autoInlineImagePaths={true}
        sessionId="session-two-lines"
      >
        {"\n\n/a/one.png\n/a/two.png\n\n"}
      </MarkdownBody>,
    );

    await waitFor(() => {
      expect(container.querySelectorAll("img")).toHaveLength(2);
    });
    expect(invokeMock).toHaveBeenCalledWith("read_attachment", {
      path: "/a/one.png",
      sessionId: "session-two-lines",
    });
    expect(invokeMock).toHaveBeenCalledWith("read_attachment", {
      path: "/a/two.png",
      sessionId: "session-two-lines",
    });
  });

  it("disables bare-path auto-inlining while streaming but keeps ![]() working", () => {
    render(
      <MarkdownBody
        streaming={true}
        autoInlineImagePaths={true}
        sessionId="session-streaming"
      >
        {"\n\n/a/b.png\n\n![x](https://example.com/x.png)"}
      </MarkdownBody>,
    );

    // 裸路径不自动内联：streaming 中途仍原样是文本，不闪图。
    expect(screen.getByText("/a/b.png")).toBeInTheDocument();
    // ![]() 语法照常渲图，不受这个门控影响。
    expect(screen.getByRole("img", { name: "x" })).toHaveAttribute(
      "src",
      "https://example.com/x.png",
    );
  });

  it("inlines a bare path once streaming ends after starting mid-stream", async () => {
    invokeMock.mockResolvedValueOnce({
      kind: "image",
      imageBase64: "c3RyZWFtZW5k",
      mediaType: "image/png",
    });

    const { container, rerender } = render(
      <MarkdownBody
        streaming={true}
        autoInlineImagePaths={true}
        sessionId="session-stream-end"
      >
        {"\n\n/a/streaming.png"}
      </MarkdownBody>,
    );
    expect(container.querySelector("img")).toBeNull();

    rerender(
      <MarkdownBody
        streaming={false}
        autoInlineImagePaths={true}
        sessionId="session-stream-end"
      >
        {"\n\n/a/streaming.png\n\n"}
      </MarkdownBody>,
    );

    await waitFor(() => {
      expect(container.querySelector("img")).toHaveAttribute(
        "src",
        "data:image/png;base64,c3RyZWFtZW5k",
      );
    });
  });

  it("抽取 <...> 包裹且内部含空格的路径（规则 B④）", async () => {
    invokeMock.mockResolvedValueOnce({
      kind: "image",
      imageBase64: "YW5nbGVzcGFjZQ==",
      mediaType: "image/png",
    });

    const { container } = render(
      <MarkdownBody
        streaming={false}
        autoInlineImagePaths={true}
        sessionId="session-angle-space"
      >
        {"\n\n详见 </Users/alice/my pics/a b.png> 这张图\n\n"}
      </MarkdownBody>,
    );

    await waitFor(() => {
      expect(container.querySelector("img")).toHaveAttribute(
        "src",
        "data:image/png;base64,YW5nbGVzcGFjZQ==",
      );
    });
    expect(invokeMock).toHaveBeenCalledWith("read_attachment", {
      path: "/Users/alice/my pics/a b.png",
      sessionId: "session-angle-space",
    });
  });

  it("抽取 ~ 开头的路径（规则 B②）", async () => {
    invokeMock.mockResolvedValueOnce({
      kind: "image",
      imageBase64: "aG9tZWRpcg==",
      mediaType: "image/png",
    });

    const { container } = render(
      <MarkdownBody
        streaming={false}
        autoInlineImagePaths={true}
        sessionId="session-tilde"
      >
        {"\n\n看 ~/Pictures/cat.webp 这张\n\n"}
      </MarkdownBody>,
    );

    await waitFor(() => {
      expect(container.querySelector("img")).toHaveAttribute(
        "src",
        "data:image/png;base64,aG9tZWRpcg==",
      );
    });
    expect(invokeMock).toHaveBeenCalledWith("read_attachment", {
      path: "~/Pictures/cat.webp",
      sessionId: "session-tilde",
    });
  });

  it("列表项下方追加图片块", async () => {
    invokeMock.mockResolvedValueOnce({
      kind: "image",
      imageBase64: "bGlzdGl0ZW0=",
      mediaType: "image/png",
    });

    const { container } = render(
      <MarkdownBody
        streaming={false}
        autoInlineImagePaths={true}
        sessionId="session-list-item"
      >
        {"- 见 /a/list-item.png 这项\n"}
      </MarkdownBody>,
    );

    expect(screen.getByText(/见 \/a\/list-item\.png 这项/)).toBeInTheDocument();
    await waitFor(() => {
      expect(container.querySelector("img")).toHaveAttribute(
        "src",
        "data:image/png;base64,bGlzdGl0ZW0=",
      );
    });
  });

  it("同一路径在消息里出现两次只出一图（消息级去重）", async () => {
    invokeMock.mockResolvedValue({
      kind: "image",
      imageBase64: "ZGVkdXA=",
      mediaType: "image/png",
    });

    const { container } = render(
      <MarkdownBody
        streaming={false}
        autoInlineImagePaths={true}
        sessionId="session-dedup"
      >
        {"\n\n先看 `/a/dup.png`\n\n再看一次 /a/dup.png 确认\n\n"}
      </MarkdownBody>,
    );

    await waitFor(() => {
      expect(container.querySelectorAll("img")).toHaveLength(1);
    });
    expect(invokeMock).toHaveBeenCalledTimes(1);
  });

  it("已是 ![]() 语法的路径不会被规则 B 重复追加", async () => {
    invokeMock.mockResolvedValueOnce({
      kind: "image",
      imageBase64: "bm9kdXA=",
      mediaType: "image/png",
    });

    const { container } = render(
      <MarkdownBody
        streaming={false}
        autoInlineImagePaths={true}
        sessionId="session-no-dup"
      >
        {"\n\n![chart](/a/already.png) 见上图\n\n"}
      </MarkdownBody>,
    );

    await waitFor(() => {
      expect(container.querySelectorAll("img")).toHaveLength(1);
    });
    expect(invokeMock).toHaveBeenCalledTimes(1);
  });
});

describe("MarkdownBody autoInlineImagePaths gating（默认关闭·P1）", () => {
  it("默认（不传 autoInlineImagePaths）不自动出图、不调 read_attachment", () => {
    render(
      <MarkdownBody streaming={false} sessionId="session-default-off">
        {"\n\n结果在 /Users/victim/secret.png 里\n\n"}
      </MarkdownBody>,
    );

    expect(
      screen.getByText(/结果在 \/Users\/victim\/secret\.png 里/),
    ).toBeInTheDocument();
    expect(screen.queryByRole("img")).not.toBeInTheDocument();
    expect(invokeMock).not.toHaveBeenCalled();
  });

  it("显式 autoInlineImagePaths={false} 同样不出图", () => {
    render(
      <MarkdownBody
        streaming={false}
        sessionId="session-explicit-off"
        autoInlineImagePaths={false}
      >
        {"\n\n/a/b.png\n\n"}
      </MarkdownBody>,
    );

    expect(screen.queryByRole("img")).not.toBeInTheDocument();
    expect(invokeMock).not.toHaveBeenCalled();
  });

  it("autoInlineImagePaths={true} 打开后照常出图（与规则 B 行为一致）", async () => {
    invokeMock.mockResolvedValueOnce({
      kind: "image",
      imageBase64: "b3B0aW4=",
      mediaType: "image/png",
    });

    const { container } = render(
      <MarkdownBody
        streaming={false}
        sessionId="session-explicit-on"
        autoInlineImagePaths={true}
      >
        {"\n\n/a/b.png\n\n"}
      </MarkdownBody>,
    );

    await waitFor(() => {
      expect(container.querySelector("img")).toHaveAttribute(
        "src",
        "data:image/png;base64,b3B0aW4=",
      );
    });
    expect(invokeMock).toHaveBeenCalledWith("read_attachment", {
      path: "/a/b.png",
      sessionId: "session-explicit-on",
    });
  });

  it("![]() 语法渲图不受 autoInlineImagePaths 影响（关闭时依旧出图）", async () => {
    invokeMock.mockResolvedValueOnce({
      kind: "image",
      imageBase64: "c3ludGF4",
      mediaType: "image/png",
    });

    const { container } = render(
      <MarkdownBody streaming={false} sessionId="session-syntax-unaffected">
        {"![chart](/a/chart.png)"}
      </MarkdownBody>,
    );

    await waitFor(() => {
      expect(container.querySelector("img")).toHaveAttribute(
        "src",
        "data:image/png;base64,c3ludGF4",
      );
    });
  });
});

describe("MarkdownBody bare image re-render 回归（P2·去重不误判）", () => {
  it("内容不变、无关 prop（onOpenLightbox 函数身份）变化触发 re-render 后图片仍在", async () => {
    invokeMock.mockResolvedValue({
      kind: "image",
      imageBase64: "cmVyZW5kZXI=",
      mediaType: "image/png",
    });

    const content = "\n\n看 /a/rerender.png 这张\n\n";
    const { container, rerender } = render(
      <MarkdownBody
        streaming={false}
        sessionId="session-rerender"
        autoInlineImagePaths={true}
        onOpenLightbox={() => {}}
      >
        {content}
      </MarkdownBody>,
    );

    await waitFor(() => {
      expect(container.querySelectorAll("img")).toHaveLength(1);
    });
    expect(invokeMock).toHaveBeenCalledTimes(1);

    // 内容原样不变，仅 onOpenLightbox 换一个新的函数引用触发 re-render。
    rerender(
      <MarkdownBody
        streaming={false}
        sessionId="session-rerender"
        autoInlineImagePaths={true}
        onOpenLightbox={() => {}}
      >
        {content}
      </MarkdownBody>,
    );

    // re-render 后图片既没消失也没重复出现第二张；去重集合每次渲染整体
    // 重建，不会把「早前渲染出过的图」错判成「这次消息里的重复路径」。
    expect(container.querySelectorAll("img")).toHaveLength(1);
    expect(invokeMock).toHaveBeenCalledTimes(1);
  });
});

describe("MarkdownBody urlTransform scope (img src only)", () => {
  it("sanitizes non-image URLs while preserving local image src exemptions", async () => {
    invokeMock.mockResolvedValue({
      kind: "image",
      imageBase64: "d2luZG93cw==",
      mediaType: "image/png",
    });

    render(
      <MarkdownBody streaming={false}>
        {String.raw`[win](C:\\tmp\\note.md)

[custom](j:%5Cfoo)

[remote link](https://example.com/docs)

![chart](C:\\tmp\\x.png)

![encoded](C:%5Ctmp%5Cy.png)`}
      </MarkdownBody>,
    );

    // 盘符形态的链接 href 回归 defaultUrlTransform 默认消毒：清空。
    expect(screen.getByText("win").closest("a")).toHaveAttribute("href", "");
    // `j:%5C` 这类伪装盘符（percent-encoded 反斜杠）同样不放行。
    expect(screen.getByText("custom").closest("a")).toHaveAttribute("href", "");
    // 正常 http/https 链接不受影响。
    expect(screen.getByRole("link", { name: "remote link" })).toHaveAttribute(
      "href",
      "https://example.com/docs",
    );

    // 未编码的盘符路径 img src 照常豁免、渲出图。
    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("read_attachment", {
        path: "C:\\tmp\\x.png",
        sessionId: null,
      }),
    );
    // 含 %5C 编码反斜杠的盘符路径 img src 解码后同样豁免。
    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("read_attachment", {
        path: "C:\\tmp\\y.png",
        sessionId: null,
      }),
    );
  });
});

describe("MarkdownBody — 注入 AttachmentPort", () => {
  it("外链走注入的 stub.openUrl 而不是 Tauri plugin-opener", () => {
    const openUrlStub = vi.fn().mockResolvedValue(undefined);

    render(
      <AttachmentPortContext.Provider
        value={stubAttachmentPort({ openUrl: openUrlStub })}
      >
        <MarkdownBody streaming={false}>
          {"[external](https://example.com/report)"}
        </MarkdownBody>
      </AttachmentPortContext.Provider>,
    );

    fireEvent.click(screen.getByRole("link", { name: "external" }));

    expect(openUrlStub).toHaveBeenCalledWith("https://example.com/report");
    expect(openUrl).not.toHaveBeenCalled();
    expect(invokeMock).not.toHaveBeenCalled();
  });

  it("本地 html 链接走注入的 stub.openExternal 而不是 invoke", async () => {
    const openExternalStub = vi.fn().mockResolvedValue(undefined);

    render(
      <AttachmentPortContext.Provider
        value={stubAttachmentPort({ openExternal: openExternalStub })}
      >
        <MarkdownBody streaming={false} sessionId="session-stub">
          {"[report](report.html)"}
        </MarkdownBody>
      </AttachmentPortContext.Provider>,
    );

    fireEvent.click(screen.getByRole("link", { name: "report" }));

    await waitFor(() =>
      expect(openExternalStub).toHaveBeenCalledWith(
        "report.html",
        "session-stub",
      ),
    );
    expect(invokeMock).not.toHaveBeenCalled();
  });

  it("图片走注入的 stub.resolveImageSrc；返回 null 时降级不显示图", async () => {
    const resolveImageSrc = vi.fn().mockResolvedValue(null);

    render(
      <AttachmentPortContext.Provider
        value={stubAttachmentPort({ resolveImageSrc })}
      >
        <MarkdownBody streaming={false} sessionId="session-stub-img">
          {"![chart](assets/x.png)"}
        </MarkdownBody>
      </AttachmentPortContext.Provider>,
    );

    await waitFor(() =>
      expect(resolveImageSrc).toHaveBeenCalledWith(
        "assets/x.png",
        "session-stub-img",
      ),
    );
    // 加载态与失败态渲染同一段路径文本（分别在 loading span / 失败态 code 里）——
    // 先等 loading 态（role="status"）消失，再断言最终降级态，避免误配到转瞬即逝的
    // loading 节点。
    await waitFor(() => {
      expect(screen.queryByRole("status")).not.toBeInTheDocument();
    });
    expect(screen.getByText("assets/x.png")).toBeInTheDocument();
    expect(screen.queryByRole("img")).not.toBeInTheDocument();
    expect(invokeMock).not.toHaveBeenCalled();
  });
});
