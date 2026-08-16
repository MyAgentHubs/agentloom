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

function stubAttachmentPort(overrides: Partial<AttachmentPort> = {}): AttachmentPort {
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
    expect(screen.getByRole("img", { name: "remote" })).toHaveStyle({
      maxWidth: "100%",
    });
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
