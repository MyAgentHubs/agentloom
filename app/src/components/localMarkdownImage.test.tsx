import { render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { invoke } from "@tauri-apps/api/core";
import { clearAttachmentCache } from "../lib/attachmentCache";
import { AttachmentPortContext } from "../lib/attachmentPortContext";
import type { AttachmentPort } from "../lib/remoteSessionPort";
import { LocalMarkdownImage } from "./localMarkdownImage";

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(),
}));

beforeEach(() => {
  vi.mocked(invoke).mockReset();
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

describe("LocalMarkdownImage — 不注入 AttachmentPort（现行为）", () => {
  it("经默认 port 走 Tauri read_attachment 加载图片", async () => {
    vi.mocked(invoke).mockResolvedValueOnce({
      kind: "image",
      imageBase64: "ZGVmYXVsdA==",
      mediaType: "image/png",
    });

    render(
      <LocalMarkdownImage path="assets/x.png" alt="chart" sessionId="s1" />,
    );

    expect(await screen.findByRole("img", { name: "chart" })).toHaveAttribute(
      "src",
      "data:image/png;base64,ZGVmYXVsdA==",
    );
    expect(invoke).toHaveBeenCalledWith("read_attachment", {
      path: "assets/x.png",
      sessionId: "s1",
    });
  });
});

describe("LocalMarkdownImage — 注入 AttachmentPort", () => {
  it("走注入的 stub 而不是 Tauri invoke", async () => {
    const resolveImageSrc = vi
      .fn()
      .mockResolvedValue("data:image/png;base64,c3R1Yg==");

    render(
      <AttachmentPortContext.Provider
        value={stubAttachmentPort({ resolveImageSrc })}
      >
        <LocalMarkdownImage path="assets/y.png" alt="stubbed" sessionId="s2" />
      </AttachmentPortContext.Provider>,
    );

    expect(
      await screen.findByRole("img", { name: "stubbed" }),
    ).toHaveAttribute("src", "data:image/png;base64,c3R1Yg==");
    expect(resolveImageSrc).toHaveBeenCalledWith("assets/y.png", "s2");
    expect(invoke).not.toHaveBeenCalled();
  });

  it("stub resolve 返回 null 时降级显示路径文本（不显示图）", async () => {
    render(
      <AttachmentPortContext.Provider value={stubAttachmentPort()}>
        <LocalMarkdownImage
          path="assets/missing.png"
          alt="missing"
          sessionId="s3"
        />
      </AttachmentPortContext.Provider>,
    );

    // 加载态与失败态渲染同一段路径文本（分别在 loading span / 失败态 code 里）——
    // 先等 loading 态（role="status"）消失，再断言最终降级态，避免误配到转瞬即逝的
    // loading 节点（findByText 会抓首个匹配，可能抓到马上被替换掉的 loading span）。
    await waitFor(() => {
      expect(screen.queryByRole("status")).not.toBeInTheDocument();
    });
    expect(screen.getByText("assets/missing.png")).toBeInTheDocument();
    expect(screen.queryByRole("img")).not.toBeInTheDocument();
    expect(invoke).not.toHaveBeenCalled();
  });
});
