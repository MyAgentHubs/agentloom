import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { openUrl } from "@tauri-apps/plugin-opener";
import { clearAttachmentCache } from "../lib/attachmentCache";
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
