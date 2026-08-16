import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { invoke } from "@tauri-apps/api/core";
import { I18nProvider } from "../i18n";
import { clearAttachmentCache } from "../lib/attachmentCache";
import { AttachmentPortContext } from "../lib/attachmentPortContext";
import type { AttachmentPort } from "../lib/remoteSessionPort";
import { Lightbox } from "./Lightbox";

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

// 扫描 invoke mock 的每一通调用的首参数——只要有一通命令名是 "read_attachment" 就算
// 命中，不管第二参数的形状是什么。比 `not.toHaveBeenCalledWith("read_attachment",
// expect.anything())` 更严：后者要求整通调用恰好是两个参数且第二参数非 null/undefined
// 才算匹配，一旦实现改成单参数调用（或参数形状变了）就会静默漏判「同命令不同参」。
function invokedWithCommand(command: string): boolean {
  return vi.mocked(invoke).mock.calls.some((call) => call[0] === command);
}

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(),
}));

beforeEach(() => {
  vi.mocked(invoke).mockReset();
  clearAttachmentCache();
});

function renderLightbox(onClose = vi.fn()) {
  vi.mocked(invoke).mockResolvedValueOnce({
    kind: "image",
    imageBase64: "bGlnaHRib3g=",
    mediaType: "image/png",
  });
  render(
    <I18nProvider>
      <Lightbox
        path="/tmp/lightbox.png"
        sessionId="session-1"
        onClose={onClose}
      />
    </I18nProvider>,
  );
  return onClose;
}

describe("Lightbox", () => {
  it("loads and displays the attachment in a full-screen dialog", async () => {
    renderLightbox();

    const image = await screen.findByRole("img", { name: "放大的图片" });
    expect(screen.getByRole("dialog", { name: "图片放大预览" })).toBeVisible();
    expect(image).toHaveAttribute("src", "data:image/png;base64,bGlnaHRib3g=");
    expect(image).toHaveStyle({
      maxWidth: "90vw",
      maxHeight: "90vh",
      objectFit: "contain",
    });
    // 不注入 port 时的默认路径：精确核 read_attachment 的调用参数形状（path/sessionId）。
    expect(invoke).toHaveBeenCalledWith("read_attachment", {
      path: "/tmp/lightbox.png",
      sessionId: "session-1",
    });
  });

  it("closes on Escape and backdrop click, but not image click", async () => {
    const onClose = renderLightbox();
    const image = await screen.findByRole("img", { name: "放大的图片" });

    fireEvent.click(image);
    expect(onClose).not.toHaveBeenCalled();

    fireEvent.keyDown(document, { key: "Escape" });
    expect(onClose).toHaveBeenCalledTimes(1);

    onClose.mockClear();
    fireEvent.click(screen.getByTestId("lightbox-backdrop"));
    expect(onClose).toHaveBeenCalledTimes(1);
  });

  it("offers a localized close button", async () => {
    const onClose = renderLightbox();

    await waitFor(() =>
      expect(
        screen.getByRole("button", { name: "关闭图片预览" }),
      ).toBeVisible(),
    );
    fireEvent.click(screen.getByRole("button", { name: "关闭图片预览" }));

    expect(onClose).toHaveBeenCalledOnce();
  });
});

describe("Lightbox — 注入 AttachmentPort", () => {
  it("走注入的 stub 而不是 Tauri invoke", async () => {
    const resolveImageSrc = vi
      .fn()
      .mockResolvedValue("data:image/png;base64,c3R1Yg==");

    render(
      <I18nProvider>
        <AttachmentPortContext.Provider
          value={stubAttachmentPort({ resolveImageSrc })}
        >
          <Lightbox
            path="/tmp/stub.png"
            sessionId="session-stub"
            onClose={vi.fn()}
          />
        </AttachmentPortContext.Provider>
      </I18nProvider>,
    );

    const image = await screen.findByRole("img", { name: "放大的图片" });
    expect(image).toHaveAttribute("src", "data:image/png;base64,c3R1Yg==");
    expect(resolveImageSrc).toHaveBeenCalledWith(
      "/tmp/stub.png",
      "session-stub",
    );
    // I18nProvider 挂载时会调一次 set_ui_locale（与本组件无关）；断言的是「不再直接
    // 调 read_attachment」，不是「invoke 一次没被叫」。
    expect(invokedWithCommand("read_attachment")).toBe(false);
  });

  it("stub resolve 返回 null 时降级为加载失败态（不显示图）", async () => {
    render(
      <I18nProvider>
        <AttachmentPortContext.Provider value={stubAttachmentPort()}>
          <Lightbox
            path="/tmp/missing.png"
            sessionId="session-missing"
            onClose={vi.fn()}
          />
        </AttachmentPortContext.Provider>
      </I18nProvider>,
    );

    // Lightbox 的 loading/failed 两态复用同一个 role="status" 节点（只换文字，不换
    // 元素）——findByRole 的首次同步检查可能抓到「还在 loading」那一刻的引用，紧接着
    // 就断言文案会有时序竞态。这里改用 waitFor 反复轮询，直到文案本身收敛到失败终态
    // 再往下断言，跟其它文件里「先等 loading 态消失、再断言终态」是同一族修法（Lightbox
    // 结构不换元素，所以这里等的是文案收敛而不是节点消失）。
    await waitFor(() => {
      expect(screen.getByRole("status")).toHaveTextContent("图片加载失败");
    });
    expect(screen.queryByRole("img")).not.toBeInTheDocument();
    expect(invokedWithCommand("read_attachment")).toBe(false);
  });
});
