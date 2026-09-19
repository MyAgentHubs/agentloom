import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { Block } from "../types/agent";

vi.mock("./CodeBlock", () => ({
  CodeBlock: ({ code, lang }: { code: string; lang?: string }) => (
    <div data-lang={lang} data-testid="codeblock">
      {code}
    </div>
  ),
}));

vi.mock("./MermaidBlock", () => ({
  MermaidBlock: ({ code, complete }: { code: string; complete: boolean }) => (
    <div data-testid="mermaidblock" data-complete={String(complete)}>
      {code}
    </div>
  ),
}));

vi.mock("@tauri-apps/plugin-opener", () => ({
  openUrl: vi.fn().mockResolvedValue(undefined),
}));

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(),
  isTauri: vi.fn().mockReturnValue(false),
}));

const writeImageMock = vi.fn().mockResolvedValue(undefined);
vi.mock("@tauri-apps/plugin-clipboard-manager", () => ({
  writeImage: (...args: unknown[]) => writeImageMock(...args),
}));

vi.mock("@tauri-apps/api/image", () => ({
  Image: { fromBytes: vi.fn().mockResolvedValue({ __fakeImage: true }) },
}));

import { invoke, isTauri } from "@tauri-apps/api/core";
import { openUrl } from "@tauri-apps/plugin-opener";
import { clearAttachmentCache } from "../lib/attachmentCache";
import { computeImageMenuPosition, MessageContent } from "./MessageContent";

beforeEach(() => {
  vi.mocked(invoke).mockReset();
  vi.mocked(isTauri).mockReturnValue(false);
  writeImageMock.mockClear();
  writeImageMock.mockResolvedValue(undefined);
  vi.mocked(openUrl).mockClear();
  clearAttachmentCache();
});

describe("computeImageMenuPosition", () => {
  it("右侧空间足够时将菜单放在图片右侧外", () => {
    expect(
      computeImageMenuPosition(
        { left: 100, right: 220, top: 80, bottom: 200 },
        { x: 160, y: 140 },
        { width: 800, height: 600 },
      ),
    ).toEqual({ left: 228, top: 140 });
  });

  it("右侧空间不足但左侧足够时将菜单放在图片左侧外", () => {
    expect(
      computeImageMenuPosition(
        { left: 400, right: 760, top: 80, bottom: 200 },
        { x: 700, y: 140 },
        { width: 800, height: 600 },
      ),
    ).toEqual({ left: 212, top: 140 });
  });

  it("图片两侧都放不下时回退到经过 clamp 的光标位置", () => {
    expect(
      computeImageMenuPosition(
        { left: 100, right: 700, top: 80, bottom: 200 },
        { x: 790, y: 590 },
        { width: 800, height: 600 },
      ),
    ).toEqual({ left: 612, top: 496 });
  });

  it("将菜单的竖直位置 clamp 在视口边距内", () => {
    const rect = { left: 100, right: 220, top: 80, bottom: 200 };
    const viewport = { width: 800, height: 600 };

    expect(
      computeImageMenuPosition(rect, { x: 160, y: -20 }, viewport),
    ).toEqual({ left: 228, top: 8 });
    expect(
      computeImageMenuPosition(rect, { x: 160, y: 590 }, viewport),
    ).toEqual({ left: 228, top: 496 });
  });

  it("无光标位置时使用图片顶部偏移作为键盘菜单的纵向锚点", () => {
    expect(
      computeImageMenuPosition(
        { left: 100, right: 220, top: 80, bottom: 200 },
        undefined,
        { width: 800, height: 600 },
      ),
    ).toEqual({ left: 228, top: 92 });
  });

  it("无光标且左右都放不下时将菜单放在图片下方外", () => {
    const imageRect = { left: 50, right: 210, top: 80, bottom: 160 };
    const position = computeImageMenuPosition(imageRect, undefined, {
      width: 300,
      height: 400,
    });
    const menuRect = {
      left: position.left,
      right: position.left + 180,
      top: position.top,
      bottom: position.top + 96,
    };

    expect(position).toEqual({ left: 50, top: 168 });
    expect(
      menuRect.left < imageRect.right &&
        menuRect.right > imageRect.left &&
        menuRect.top < imageRect.bottom &&
        menuRect.bottom > imageRect.top,
    ).toBe(false);
  });

  it("无光标且左右下方都放不下时将菜单放在图片上方外", () => {
    const imageRect = { left: 50, right: 210, top: 140, bottom: 230 };
    const position = computeImageMenuPosition(imageRect, undefined, {
      width: 300,
      height: 260,
    });
    const menuRect = {
      left: position.left,
      right: position.left + 180,
      top: position.top,
      bottom: position.top + 96,
    };

    expect(position).toEqual({ left: 50, top: 36 });
    expect(
      menuRect.left < imageRect.right &&
        menuRect.right > imageRect.left &&
        menuRect.top < imageRect.bottom &&
        menuRect.bottom > imageRect.top,
    ).toBe(false);
  });
});

describe("MessageContent", () => {
  it("工具图片右键菜单可复制完整路径，且不影响左键预览", async () => {
    vi.mocked(invoke).mockResolvedValueOnce({
      kind: "image",
      imageBase64: "dGh1bWI=",
      mediaType: "image/png",
    });
    const writeText = vi.fn().mockResolvedValue(undefined);
    Object.defineProperty(navigator, "clipboard", {
      configurable: true,
      value: { writeText },
    });
    const onOpenPreview = vi.fn();
    const onOpenLightbox = vi.fn();
    render(
      <MessageContent
        blocks={[
          {
            type: "tool",
            id: "t-image-menu-path",
            tool: "Bash",
            summary: "render image artifact",
            card: "command",
            status: "ok",
            exit_code: 0,
            output: "saved to /abs/path/moon.png",
          },
        ]}
        onOpenPreview={onOpenPreview}
        onOpenLightbox={onOpenLightbox}
      />,
    );

    const image = await screen.findByAltText("moon.png");
    fireEvent.contextMenu(image);

    const menu = screen.getByRole("menu");
    expect(menu).toBeInTheDocument();
    expect(menu.parentElement).toBe(document.body);
    expect(
      screen.getByRole("menuitem", { name: "复制图片" }),
    ).toBeInTheDocument();
    fireEvent.click(screen.getByRole("menuitem", { name: "复制全路径" }));

    await waitFor(() =>
      expect(writeText).toHaveBeenCalledWith("/abs/path/moon.png"),
    );
    expect(screen.queryByRole("menu")).toBeNull();
    const feedback = await screen.findByText("路径已复制", {
      selector: '[role="status"]',
    });
    expect(feedback.parentElement).toBe(document.body);

    fireEvent.click(image);
    expect(onOpenLightbox).toHaveBeenCalledWith("/abs/path/moon.png");
    expect(onOpenPreview).not.toHaveBeenCalled();
  });

  it("右键另一张工具图片时只保留新图片的菜单", async () => {
    vi.mocked(invoke)
      .mockResolvedValueOnce({
        kind: "image",
        imageBase64: "Zmlyc3Q=",
        mediaType: "image/png",
      })
      .mockResolvedValueOnce({
        kind: "image",
        imageBase64: "c2Vjb25k",
        mediaType: "image/png",
      });
    render(
      <MessageContent
        blocks={[
          {
            type: "tool",
            id: "t-image-menu-single-open",
            tool: "Bash",
            summary: "render image artifacts",
            card: "command",
            status: "ok",
            exit_code: 0,
            output: "saved /abs/path/first.png and /abs/path/second.png",
          },
        ]}
        onOpenPreview={vi.fn()}
        onOpenLightbox={vi.fn()}
      />,
    );

    const firstImage = await screen.findByAltText("first.png");
    const secondImage = await screen.findByAltText("second.png");
    fireEvent.contextMenu(firstImage);
    const firstMenu = screen.getByRole("menu");

    fireEvent.contextMenu(secondImage);

    const menus = screen.getAllByRole("menu");
    expect(menus).toHaveLength(1);
    expect(firstMenu).not.toBeInTheDocument();
    expect(menus[0]).toBeInTheDocument();
  });

  it("图片菜单支持键盘打开、Esc 和外部点击关闭", async () => {
    vi.mocked(invoke).mockResolvedValueOnce({
      kind: "image",
      imageBase64: "dGh1bWI=",
      mediaType: "image/png",
    });
    render(
      <MessageContent
        blocks={[
          {
            type: "tool",
            id: "t-image-menu-keyboard",
            tool: "Bash",
            summary: "render image artifact",
            card: "command",
            status: "ok",
            exit_code: 0,
            output: "saved to /abs/path/moon.png",
          },
        ]}
        onOpenPreview={vi.fn()}
        onOpenLightbox={vi.fn()}
      />,
    );

    const preview = await screen.findByRole("button", {
      name: "预览图片 moon.png",
    });
    fireEvent.keyDown(preview, { key: "F10", shiftKey: true });
    expect(screen.getByRole("menu")).toBeInTheDocument();

    fireEvent.keyDown(document, { key: "Escape" });
    expect(screen.queryByRole("menu")).toBeNull();

    fireEvent.contextMenu(preview);
    fireEvent.pointerDown(document.body);
    expect(screen.queryByRole("menu")).toBeNull();
  });

  it("工具图片右键可写入图片；clipboard.write 不可用时禁用该项但仍可复制路径", async () => {
    vi.mocked(invoke)
      .mockResolvedValueOnce({
        kind: "image",
        imageBase64: "dGh1bWI=",
        mediaType: "image/png",
      })
      .mockResolvedValueOnce({
        kind: "image",
        imageBase64: "dGh1bWI=",
        mediaType: "image/png",
      });
    const write = vi.fn().mockResolvedValue(undefined);
    const writeText = vi.fn().mockResolvedValue(undefined);
    class MockClipboardItem {
      constructor(public items: Record<string, Blob>) {}
    }
    vi.stubGlobal("ClipboardItem", MockClipboardItem);
    Object.defineProperty(navigator, "clipboard", {
      configurable: true,
      value: { write, writeText },
    });
    const imageBlock: Block[] = [
      {
        type: "tool",
        id: "t-image-menu-copy",
        tool: "Bash",
        summary: "render image artifact",
        card: "command",
        status: "ok",
        exit_code: 0,
        output: "saved to /abs/path/moon.png",
      },
    ];
    const first = render(
      <MessageContent
        blocks={imageBlock}
        onOpenPreview={vi.fn()}
        onOpenLightbox={vi.fn()}
      />,
    );

    fireEvent.contextMenu(await screen.findByAltText("moon.png"));
    fireEvent.click(screen.getByRole("menuitem", { name: "复制图片" }));

    await waitFor(() => expect(write).toHaveBeenCalledOnce());
    expect(write.mock.calls[0][0][0]).toBeInstanceOf(MockClipboardItem);
    first.unmount();

    Object.defineProperty(navigator, "clipboard", {
      configurable: true,
      value: { writeText },
    });
    render(
      <MessageContent
        blocks={imageBlock}
        onOpenPreview={vi.fn()}
        onOpenLightbox={vi.fn()}
      />,
    );

    fireEvent.contextMenu(await screen.findByAltText("moon.png"));
    expect(screen.getByRole("menuitem", { name: "复制图片" })).toBeDisabled();
    fireEvent.click(screen.getByRole("menuitem", { name: "复制全路径" }));

    await waitFor(() =>
      expect(writeText).toHaveBeenCalledWith("/abs/path/moon.png"),
    );
  });

  it("svg 图片右键复制会先栅格化成 PNG 再写剪贴板（规则 C）", async () => {
    vi.mocked(invoke).mockResolvedValueOnce({
      kind: "image",
      imageBase64: "PHN2Zy8+",
      mediaType: "image/svg+xml",
    });
    const write = vi.fn().mockResolvedValue(undefined);
    class MockClipboardItem {
      constructor(public items: Record<string, Blob>) {}
    }
    vi.stubGlobal("ClipboardItem", MockClipboardItem);
    Object.defineProperty(navigator, "clipboard", {
      configurable: true,
      value: { write, writeText: vi.fn().mockResolvedValue(undefined) },
    });

    const drawImageMock = vi.fn();
    vi.spyOn(HTMLCanvasElement.prototype, "getContext").mockReturnValue({
      drawImage: drawImageMock,
    } as unknown as CanvasRenderingContext2D);
    vi.spyOn(HTMLCanvasElement.prototype, "toBlob").mockImplementation(
      (cb: BlobCallback) => cb(new Blob(["png"], { type: "image/png" })),
    );
    const originalImage = globalThis.Image;
    class MockImage {
      onload: (() => void) | null = null;
      onerror: (() => void) | null = null;
      naturalWidth = 64;
      naturalHeight = 64;
      private _src = "";
      set src(value: string) {
        this._src = value;
        queueMicrotask(() => this.onload?.());
      }
      get src() {
        return this._src;
      }
    }
    // @ts-expect-error 测试替身，不需要实现完整 Image 接口
    globalThis.Image = MockImage;

    render(
      <MessageContent
        blocks={[
          {
            attachment_id: "/abs/path/icon.svg",
            media_type: "image/svg+xml",
            type: "image",
          },
        ]}
        sessionId="session-svg-copy"
        onOpenLightbox={vi.fn()}
      />,
    );

    fireEvent.contextMenu(await screen.findByRole("img"));
    fireEvent.click(screen.getByRole("menuitem", { name: "复制图片" }));

    await waitFor(() => expect(write).toHaveBeenCalledOnce());
    expect(drawImageMock).toHaveBeenCalled();
    expect(write.mock.calls[0][0][0]).toBeInstanceOf(MockClipboardItem);
    expect(Object.keys(write.mock.calls[0][0][0].items)).toEqual(["image/png"]);

    globalThis.Image = originalImage;
    vi.restoreAllMocks();
  });

  it("svg 栅格化失败时降级为复制路径并给出对应提示（规则 C）", async () => {
    vi.mocked(invoke).mockResolvedValueOnce({
      kind: "image",
      imageBase64: "PHN2Zy8+",
      mediaType: "image/svg+xml",
    });
    const write = vi.fn();
    const writeText = vi.fn().mockResolvedValue(undefined);
    class MockClipboardItem {
      constructor(public items: Record<string, Blob>) {}
    }
    vi.stubGlobal("ClipboardItem", MockClipboardItem);
    Object.defineProperty(navigator, "clipboard", {
      configurable: true,
      value: { write, writeText },
    });
    // 2d context 拿不到 → 栅格化必失败。
    vi.spyOn(HTMLCanvasElement.prototype, "getContext").mockReturnValue(null);
    const originalImage = globalThis.Image;
    class MockImage {
      onload: (() => void) | null = null;
      onerror: (() => void) | null = null;
      naturalWidth = 64;
      naturalHeight = 64;
      private _src = "";
      set src(value: string) {
        this._src = value;
        queueMicrotask(() => this.onload?.());
      }
      get src() {
        return this._src;
      }
    }
    // @ts-expect-error 测试替身
    globalThis.Image = MockImage;

    render(
      <MessageContent
        blocks={[
          {
            attachment_id: "/abs/path/broken.svg",
            media_type: "image/svg+xml",
            type: "image",
          },
        ]}
        sessionId="session-svg-fallback"
        onOpenLightbox={vi.fn()}
      />,
    );

    fireEvent.contextMenu(await screen.findByRole("img"));
    fireEvent.click(screen.getByRole("menuitem", { name: "复制图片" }));

    await waitFor(() =>
      expect(writeText).toHaveBeenCalledWith("/abs/path/broken.svg"),
    );
    expect(write).not.toHaveBeenCalled();
    expect(
      await screen.findByText("图片复制失败，已改为复制路径"),
    ).toBeInTheDocument();

    globalThis.Image = originalImage;
    vi.restoreAllMocks();
  });

  it("① Tauri 环境下复制图片经 writeImage 写系统剪贴板，不调用 navigator.clipboard.write", async () => {
    vi.mocked(isTauri).mockReturnValue(true);
    vi.mocked(invoke).mockResolvedValueOnce({
      kind: "image",
      imageBase64: "dGh1bWI=",
      mediaType: "image/png",
    });
    const write = vi.fn().mockResolvedValue(undefined);
    Object.defineProperty(navigator, "clipboard", {
      configurable: true,
      value: { write, writeText: vi.fn().mockResolvedValue(undefined) },
    });
    const imageBlock: Block[] = [
      {
        type: "tool",
        id: "t-image-menu-tauri",
        tool: "Bash",
        summary: "render image artifact",
        card: "command",
        status: "ok",
        exit_code: 0,
        output: "saved to /abs/path/moon.png",
      },
    ];

    render(
      <MessageContent
        blocks={imageBlock}
        onOpenPreview={vi.fn()}
        onOpenLightbox={vi.fn()}
      />,
    );

    fireEvent.contextMenu(await screen.findByAltText("moon.png"));
    fireEvent.click(screen.getByRole("menuitem", { name: "复制图片" }));

    await waitFor(() => expect(writeImageMock).toHaveBeenCalledOnce());
    expect(write).not.toHaveBeenCalled();
    expect(await screen.findByText("图片已复制")).toBeInTheDocument();
  });

  it("Tauri 环境下即使 navigator.clipboard 不存在，「复制图片」菜单项仍可用", async () => {
    vi.mocked(isTauri).mockReturnValue(true);
    vi.mocked(invoke).mockResolvedValueOnce({
      kind: "image",
      imageBase64: "dGh1bWI=",
      mediaType: "image/png",
    });
    Object.defineProperty(navigator, "clipboard", {
      configurable: true,
      value: undefined,
    });
    const imageBlock: Block[] = [
      {
        type: "tool",
        id: "t-image-menu-tauri-no-clipboard",
        tool: "Bash",
        summary: "render image artifact",
        card: "command",
        status: "ok",
        exit_code: 0,
        output: "saved to /abs/path/moon.png",
      },
    ];

    render(
      <MessageContent
        blocks={imageBlock}
        onOpenPreview={vi.fn()}
        onOpenLightbox={vi.fn()}
      />,
    );

    fireEvent.contextMenu(await screen.findByAltText("moon.png"));
    expect(
      screen.getByRole("menuitem", { name: "复制图片" }),
    ).not.toBeDisabled();
  });

  it("④ Tauri writeImage 抛错时 toast 仍是复制失败，且 console.warn 收到 err.name", async () => {
    vi.mocked(isTauri).mockReturnValue(true);
    writeImageMock.mockRejectedValue(
      Object.assign(new Error("denied"), { name: "NotAllowedError" }),
    );
    const warnSpy = vi.spyOn(console, "warn").mockImplementation(() => {});
    vi.mocked(invoke).mockResolvedValueOnce({
      kind: "image",
      imageBase64: "dGh1bWI=",
      mediaType: "image/png",
    });
    Object.defineProperty(navigator, "clipboard", {
      configurable: true,
      value: {
        write: vi.fn(),
        writeText: vi.fn().mockResolvedValue(undefined),
      },
    });
    const imageBlock: Block[] = [
      {
        type: "tool",
        id: "t-image-menu-tauri-fail",
        tool: "Bash",
        summary: "render image artifact",
        card: "command",
        status: "ok",
        exit_code: 0,
        output: "saved to /abs/path/moon.png",
      },
    ];

    render(
      <MessageContent
        blocks={imageBlock}
        onOpenPreview={vi.fn()}
        onOpenLightbox={vi.fn()}
      />,
    );

    fireEvent.contextMenu(await screen.findByAltText("moon.png"));
    fireEvent.click(screen.getByRole("menuitem", { name: "复制图片" }));

    expect(await screen.findByText("复制失败")).toBeInTheDocument();
    expect(warnSpy).toHaveBeenCalledWith(
      expect.stringContaining("copyImage"),
      "NotAllowedError",
    );
    warnSpy.mockRestore();
  });

  it("image block 经 read_attachment 加载后渲染图片", async () => {
    vi.mocked(invoke).mockResolvedValueOnce({
      kind: "image",
      imageBase64: "aW1hZ2U=",
      mediaType: "image/png",
    });

    const onOpenLightbox = vi.fn();
    render(
      <MessageContent
        blocks={[
          {
            attachment_id: "/abs/path/moon.png",
            media_type: "image/png",
            type: "image",
          },
        ]}
        sessionId="session-1"
        onOpenLightbox={onOpenLightbox}
      />,
    );

    const image = await screen.findByRole("img");
    expect(image).toHaveAttribute("src", "data:image/png;base64,aW1hZ2U=");
    // 规则 C：粘贴附件回显走统一样式类，不再各处内联 maxHeight/maxWidth。
    expect(image).toHaveClass("al-chat-image");
    expect(invoke).toHaveBeenCalledWith("read_attachment", {
      path: "/abs/path/moon.png",
      sessionId: "session-1",
    });

    fireEvent.click(image);
    expect(onOpenLightbox).toHaveBeenCalledWith("/abs/path/moon.png");
  });

  it("image block remount 后立即复用缓存且不重复读取", async () => {
    vi.mocked(invoke).mockResolvedValueOnce({
      kind: "image",
      imageBase64: "Y2FjaGVk",
      mediaType: "image/png",
    });
    const blocks: Block[] = [
      {
        attachment_id: "/abs/path/cached.png",
        media_type: "image/png",
        type: "image",
      },
    ];

    const first = render(
      <MessageContent blocks={blocks} sessionId="session-cache" />,
    );
    await screen.findByRole("img");
    first.unmount();

    render(<MessageContent blocks={blocks} sessionId="session-cache" />);

    expect(screen.getByRole("img")).toHaveAttribute(
      "src",
      "data:image/png;base64,Y2FjaGVk",
    );
    expect(invoke).toHaveBeenCalledTimes(1);
  });

  it("image block 加载失败 → 回退为可点击路径", async () => {
    vi.mocked(invoke).mockRejectedValueOnce(new Error("read failed"));
    const onOpenPreview = vi.fn();

    render(
      <MessageContent
        blocks={[
          {
            attachment_id: "/abs/path/moon.png",
            media_type: "image/png",
            type: "image",
          },
        ]}
        onOpenPreview={onOpenPreview}
      />,
    );

    await waitFor(() =>
      expect(
        screen.getByRole("button", { name: "/abs/path/moon.png" }),
      ).toBeInTheDocument(),
    );
    fireEvent.click(screen.getByRole("button", { name: "/abs/path/moon.png" }));

    expect(onOpenPreview).toHaveBeenCalledWith("/abs/path/moon.png");
  });
});
