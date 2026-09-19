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

import { invoke } from "@tauri-apps/api/core";
import { openUrl } from "@tauri-apps/plugin-opener";
import { clearAttachmentCache } from "../lib/attachmentCache";
import { MessageContent } from "./MessageContent";

beforeEach(() => {
  vi.mocked(invoke).mockReset();
  vi.mocked(openUrl).mockClear();
  clearAttachmentCache();
});

describe("MessageContent", () => {
  it("command 卡 output 含图片路径 → 内联渲染有界缩略图并可点击", async () => {
    vi.mocked(invoke).mockResolvedValueOnce({
      kind: "image",
      imageBase64: "dGh1bWI=",
      mediaType: "image/png",
    });
    const onOpenPreview = vi.fn();
    const onOpenLightbox = vi.fn();
    render(
      <MessageContent
        blocks={[
          {
            type: "tool",
            id: "t-image",
            tool: "Bash",
            summary: "render image artifact",
            card: "command",
            status: "ok",
            exit_code: 0,
            output: "saved to /abs/path/moon.png",
          },
        ]}
        sessionId="session-artifact"
        onOpenPreview={onOpenPreview}
        onOpenLightbox={onOpenLightbox}
      />,
    );

    const image = await screen.findByAltText("moon.png");
    expect(image.tagName).toBe("IMG");
    expect(image).toHaveAttribute("src", "data:image/png;base64,dGh1bWI=");
    expect(image).toHaveStyle({
      maxHeight: "240px",
      maxWidth: "100%",
      objectFit: "contain",
      height: "auto",
    });
    expect(invoke).toHaveBeenCalledWith("read_attachment", {
      path: "/abs/path/moon.png",
      sessionId: "session-artifact",
    });

    fireEvent.click(image);

    expect(onOpenLightbox).toHaveBeenCalledWith("/abs/path/moon.png");
    expect(onOpenPreview).not.toHaveBeenCalled();
  });

  it.each([
    ["invoke 失败", () => Promise.reject(new Error("read failed"))],
    ["缺少 imageBase64", () => Promise.resolve({ kind: "image" })],
  ])("工具图片%s → 回退成可点击文字 chip", async (_label, load) => {
    vi.mocked(invoke).mockReturnValueOnce(load() as ReturnType<typeof invoke>);
    const onOpenPreview = vi.fn();
    const onOpenLightbox = vi.fn();
    render(
      <MessageContent
        blocks={[
          {
            type: "tool",
            id: "t-image-fallback",
            tool: "Bash",
            summary: "render image artifact",
            card: "command",
            status: "ok",
            exit_code: 0,
            output: "saved to /abs/path/fallback.png",
          },
        ]}
        onOpenPreview={onOpenPreview}
        onOpenLightbox={onOpenLightbox}
      />,
    );

    const chip = await screen.findByRole("button", {
      name: "/abs/path/fallback.png",
    });
    expect(screen.queryByAltText("fallback.png")).toBeNull();
    expect(chip.parentElement).toHaveStyle({ alignItems: "flex-start" });

    fireEvent.click(chip);
    expect(onOpenPreview).toHaveBeenCalledWith("/abs/path/fallback.png");
    expect(onOpenLightbox).not.toHaveBeenCalled();
  });

  it("tool 卡没有图片路径 → 不渲染图片 chip", () => {
    render(
      <MessageContent
        blocks={[
          {
            type: "tool",
            id: "t-no-image",
            tool: "Bash",
            summary: "npm test",
            card: "command",
            status: "ok",
            exit_code: 0,
            output: "all tests passed",
          },
        ]}
        onOpenPreview={vi.fn()}
        onOpenLightbox={vi.fn()}
      />,
    );

    expect(screen.queryByRole("button", { name: /预览图片/ })).toBeNull();
  });

  it("cp 命令中的绝对与相对图片路径 → 捕获完整 token，不生成伪绝对路径", async () => {
    vi.mocked(invoke).mockResolvedValue({
      kind: "image",
      imageBase64: "aW1hZ2U=",
      mediaType: "image/png",
    });
    render(
      <MessageContent
        blocks={[
          {
            type: "tool",
            id: "t-cp-image-paths",
            tool: "Bash",
            summary: "copy generated image",
            card: "command",
            status: "ok",
            exit_code: 0,
            output:
              "cp /Users/dev/.codex/tmp/call_X.png output/imagegen/kitten-and-puppy.png",
          },
        ]}
        onOpenPreview={vi.fn()}
        onOpenLightbox={vi.fn()}
      />,
    );

    await waitFor(() => expect(invoke).toHaveBeenCalledTimes(2));
    const paths = vi
      .mocked(invoke)
      .mock.calls.map(([, args]) => (args as { path: string }).path);
    expect(paths).toEqual([
      "/Users/dev/.codex/tmp/call_X.png",
      "output/imagegen/kitten-and-puppy.png",
    ]);
    expect(paths).not.toContain("/imagegen/kitten-and-puppy.png");
  });

  it("同组绝对与相对路径内容相同 → 只保留相对路径缩略图", async () => {
    const onOpenLightbox = vi.fn();
    vi.mocked(invoke).mockResolvedValue({
      kind: "image",
      imageBase64: "c2FtZS1pbWFnZQ==",
      mediaType: "image/png",
    });
    render(
      <MessageContent
        blocks={[
          {
            type: "tool",
            id: "t-content-deduplicate-relative",
            tool: "Bash",
            summary: "copy generated image",
            card: "command",
            status: "ok",
            exit_code: 0,
            output:
              "cp /Users/dev/.codex/tmp/call_X.png output/imagegen/deep-space.png",
          },
        ]}
        onOpenPreview={vi.fn()}
        onOpenLightbox={onOpenLightbox}
      />,
    );

    await waitFor(() => expect(screen.getAllByRole("img")).toHaveLength(1));
    expect(screen.getByAltText("deep-space.png")).toBeInTheDocument();
    expect(screen.queryByAltText("call_X.png")).toBeNull();

    fireEvent.click(screen.getByAltText("deep-space.png"));
    expect(onOpenLightbox).toHaveBeenCalledWith(
      "output/imagegen/deep-space.png",
    );
  });

  it.each([
    ["相对路径", "output/first.png", "output/second.png"],
    ["绝对路径", "/tmp/first.png", "/tmp/second.png"],
  ])("同组同为%s且内容相同 → 保留靠后路径", async (_label, first, second) => {
    vi.mocked(invoke).mockResolvedValue({
      kind: "image",
      imageBase64: "c2FtZS1pbWFnZQ==",
      mediaType: "image/png",
    });
    render(
      <MessageContent
        blocks={[
          {
            type: "tool",
            id: `t-content-deduplicate-later-${_label}`,
            tool: "Bash",
            summary: "copy generated image",
            card: "command",
            status: "ok",
            exit_code: 0,
            output: `${first} ${second}`,
          },
        ]}
        onOpenPreview={vi.fn()}
        onOpenLightbox={vi.fn()}
      />,
    );

    await waitFor(() => expect(screen.getAllByRole("img")).toHaveLength(1));
    expect(screen.getByAltText("second.png")).toBeInTheDocument();
    expect(screen.queryByAltText("first.png")).toBeNull();
  });

  it("同组路径内容不同 → 两张缩略图都保留", async () => {
    vi.mocked(invoke).mockImplementation((_command, args) => {
      const path = (args as { path: string }).path;
      return Promise.resolve({
        kind: "image",
        imageBase64: path.includes("moon") ? "bW9vbg==" : "c3Rhcg==",
        mediaType: "image/png",
      });
    });
    render(
      <MessageContent
        blocks={[
          {
            type: "tool",
            id: "t-content-deduplicate-distinct",
            tool: "Bash",
            summary: "generate distinct images",
            card: "command",
            status: "ok",
            exit_code: 0,
            output: "/tmp/moon.png output/imagegen/star.png",
          },
        ]}
        onOpenPreview={vi.fn()}
        onOpenLightbox={vi.fn()}
      />,
    );

    expect(await screen.findAllByRole("img")).toHaveLength(2);
    expect(screen.getByAltText("moon.png")).toBeInTheDocument();
    expect(screen.getByAltText("star.png")).toBeInTheDocument();
  });

  it("同组一张加载成功一张失败 → 成功缩略图与失败降级 chip 都保留", async () => {
    vi.mocked(invoke).mockImplementation((_command, args) => {
      const path = (args as { path: string }).path;
      if (path.includes("missing")) return Promise.reject(new Error("missing"));
      return Promise.resolve({
        kind: "image",
        imageBase64: "bW9vbg==",
        mediaType: "image/png",
      });
    });
    render(
      <MessageContent
        blocks={[
          {
            type: "tool",
            id: "t-content-deduplicate-failure",
            tool: "Bash",
            summary: "load image artifacts",
            card: "command",
            status: "ok",
            exit_code: 0,
            output: "/tmp/moon.png output/imagegen/missing.png",
          },
        ]}
        onOpenPreview={vi.fn()}
        onOpenLightbox={vi.fn()}
      />,
    );

    expect(await screen.findByAltText("moon.png")).toBeInTheDocument();
    expect(
      await screen.findByRole("button", {
        name: "output/imagegen/missing.png",
      }),
    ).toBeInTheDocument();
  });

  it("同组三路径两同一异 → 保留相对路径同图赢家与异图", async () => {
    vi.mocked(invoke).mockImplementation((_command, args) => {
      const path = (args as { path: string }).path;
      return Promise.resolve({
        kind: "image",
        imageBase64: path.includes("nebula") ? "bmVidWxh" : "ZGVlcC1zcGFjZQ==",
        mediaType: "image/png",
      });
    });
    render(
      <MessageContent
        blocks={[
          {
            type: "tool",
            id: "t-content-deduplicate-three",
            tool: "Bash",
            summary: "copy and generate images",
            card: "command",
            status: "ok",
            exit_code: 0,
            output:
              "/tmp/deep-space-temp.png output/imagegen/nebula.png output/imagegen/deep-space.png",
          },
        ]}
        onOpenPreview={vi.fn()}
        onOpenLightbox={vi.fn()}
      />,
    );

    await waitFor(() => expect(screen.getAllByRole("img")).toHaveLength(2));
    expect(screen.getByAltText("deep-space.png")).toBeInTheDocument();
    expect(screen.getByAltText("nebula.png")).toBeInTheDocument();
    expect(screen.queryByAltText("deep-space-temp.png")).toBeNull();
  });

  it.each([
    ["URL", "https://example.com/a/b/pic.png"],
    ["协议相对 URL", "//cdn.example.com/a/b/pic.png"],
    ["单斜杠 scheme URL", "file:/tmp/pic.png"],
    ["grep 行号", "docs/img/diagram.png:12: some match"],
    ["裸文件名", "photo.png"],
  ])("%s 中的图片字样 → 不捕获为本地图片路径", async (_label, output) => {
    render(
      <MessageContent
        blocks={[
          {
            type: "tool",
            id: `t-reject-${_label}`,
            tool: "Bash",
            summary: "inspect output",
            card: "command",
            status: "ok",
            exit_code: 0,
            output,
          },
        ]}
        onOpenPreview={vi.fn()}
        onOpenLightbox={vi.fn()}
      />,
    );

    await waitFor(() => expect(invoke).not.toHaveBeenCalled());
  });

  it.each([
    ["点前缀相对路径", "./assets/logo.svg", "./assets/logo.svg"],
    ["中文句号收尾", "已保存到 output/pics/cat.png。", "output/pics/cat.png"],
    ["ASCII 括号包裹", "(output/pics/cat.png)", "output/pics/cat.png"],
    ["参数粘连绝对路径", "--out=/tmp/a.png", "/tmp/a.png"],
    ["Windows 正斜杠绝对路径", "C:/tmp/logo.png", "C:/tmp/logo.png"],
    ["Windows 反斜杠绝对路径", "C:\\tmp\\logo.png", "C:\\tmp\\logo.png"],
  ])("%s → 捕获清理后的完整路径", async (_label, output, expectedPath) => {
    vi.mocked(invoke).mockResolvedValue({
      kind: "image",
      imageBase64: "aW1hZ2U=",
      mediaType: "image/png",
    });
    render(
      <MessageContent
        blocks={[
          {
            type: "tool",
            id: `t-accept-${_label}`,
            tool: "Bash",
            summary: "generate image",
            card: "command",
            status: "ok",
            exit_code: 0,
            output,
          },
        ]}
        onOpenPreview={vi.fn()}
        onOpenLightbox={vi.fn()}
      />,
    );

    await waitFor(() =>
      expect(invoke).toHaveBeenCalledWith("read_attachment", {
        path: expectedPath,
        sessionId: null,
      }),
    );
    expect(invoke).toHaveBeenCalledTimes(1);
  });

  it("单个工具块含 9 个合格图片路径时只读取前 8 个", async () => {
    vi.mocked(invoke).mockResolvedValue({
      kind: "image",
      imageBase64: "aW1hZ2U=",
      mediaType: "image/png",
    });
    const paths = Array.from(
      { length: 9 },
      (_, index) => `/tmp/generated-${index + 1}.png`,
    );
    render(
      <MessageContent
        blocks={[
          {
            type: "tool",
            id: "t-image-path-limit",
            tool: "Bash",
            summary: "generate image batch",
            card: "command",
            status: "ok",
            exit_code: 0,
            output: paths.join("\n"),
          },
        ]}
        onOpenPreview={vi.fn()}
        onOpenLightbox={vi.fn()}
      />,
    );

    await waitFor(() => expect(invoke).toHaveBeenCalledTimes(8));
    const readPaths = vi
      .mocked(invoke)
      .mock.calls.map(([, args]) => (args as { path: string }).path);
    expect(readPaths).toEqual(paths.slice(0, 8));
    expect(readPaths).not.toContain(paths[8]);
  });

  it("绝对路径以相对路径结尾 → 丢短留长，只渲染绝对路径", async () => {
    vi.mocked(invoke).mockResolvedValue({
      kind: "image",
      imageBase64: "aW1hZ2U=",
      mediaType: "image/png",
    });
    render(
      <MessageContent
        blocks={[
          {
            type: "tool",
            id: "t-absolute-relative-duplicate",
            tool: "Bash",
            summary: "generate image",
            card: "command",
            status: "ok",
            exit_code: 0,
            output: "/abs/root/output/x.png output/x.png",
          },
        ]}
        onOpenPreview={vi.fn()}
        onOpenLightbox={vi.fn()}
      />,
    );

    await waitFor(() => expect(invoke).toHaveBeenCalledTimes(1));
    expect(invoke).toHaveBeenCalledWith("read_attachment", {
      path: "/abs/root/output/x.png",
      sessionId: null,
    });
  });

  it.each([
    ["相对在前", "output/x.png", "/abs/root/output/x.png"],
    ["绝对在前", "/abs/root/output/x.png", "output/x.png"],
  ])(
    "跨 item 去重（%s）→ 与顺序无关地丢短留长",
    async (_label, first, second) => {
      vi.mocked(invoke).mockResolvedValue({
        kind: "image",
        imageBase64: "aW1hZ2U=",
        mediaType: "image/png",
      });
      render(
        <MessageContent
          blocks={[
            {
              type: "tool",
              id: "t-cross-item-first",
              tool: "Bash",
              summary: "generate first reference",
              card: "command",
              status: "ok",
              exit_code: 0,
              output: first,
            },
            { type: "text", text: "分隔两个工具块" },
            {
              type: "tool",
              id: "t-cross-item-second",
              tool: "Bash",
              summary: "generate second reference",
              card: "command",
              status: "ok",
              exit_code: 0,
              output: second,
            },
          ]}
          onOpenPreview={vi.fn()}
          onOpenLightbox={vi.fn()}
        />,
      );

      await waitFor(() => expect(invoke).toHaveBeenCalledTimes(1));
      expect(invoke).toHaveBeenCalledWith("read_attachment", {
        path: "/abs/root/output/x.png",
        sessionId: null,
      });
    },
  );

  it.each([
    "Write",
    "write",
    "Edit",
    "edit",
    "fs_write",
    "fs_edit",
    "apply_patch",
  ])(
    "%s 工具块 output 里的图片路径 → 不扫描（output 是回执/引用，不是产物）",
    async (tool) => {
      render(
        <MessageContent
          blocks={[
            {
              type: "tool",
              id: `t-content-tool-${tool}`,
              tool,
              summary: "update image reference",
              card: "command",
              status: "ok",
              exit_code: 0,
              output: "wrote /abs/path/ignored.png",
            },
          ]}
          onOpenPreview={vi.fn()}
          onOpenLightbox={vi.fn()}
        />,
      );

      await waitFor(() => expect(invoke).not.toHaveBeenCalled());
    },
  );

  // T24b 规则 A：Write/Edit/fs_write/fs_edit 的 summary 恒等于目标文件路径（可信产物
  // 信号）——是图片就该出图，不同于上面 output 的「引用字符串不算产物」规则。
  it.each(["Write", "Edit", "MultiEdit", "fs_write", "fs_edit"])(
    "%s 工具块 summary 是图片路径（Write/Edit 产物）→ 出图",
    async (tool) => {
      vi.mocked(invoke).mockResolvedValueOnce({
        kind: "image",
        imageBase64: "bW9vbg==",
        mediaType: "image/svg+xml",
      });
      render(
        <MessageContent
          blocks={[
            {
              type: "tool",
              id: `t-produced-${tool}`,
              tool,
              summary: "/abs/out/chart.svg",
              card: "compact",
              status: "ok",
              exit_code: 0,
              output: null,
            },
          ]}
          onOpenPreview={vi.fn()}
          onOpenLightbox={vi.fn()}
        />,
      );

      await waitFor(() => expect(invoke).toHaveBeenCalledTimes(1));
      expect(invoke).toHaveBeenCalledWith("read_attachment", {
        path: "/abs/out/chart.svg",
        sessionId: null,
      });
    },
  );

  // T24b 规则 A：codex file_change 的 tool 名是 "file"，summary 只留 basename
  // （"add logo.svg"）没有目录分隔符过不了形状检查；真实全路径由后端（agent_event.rs
  // codex_file_change_image_output）塞进 output，换行分隔——走通用扫描能捞到。
  it("file 工具块（codex file_change）：summary 只有 basename，output 里的全路径 → 出图", async () => {
    vi.mocked(invoke).mockResolvedValueOnce({
      kind: "image",
      imageBase64: "bW9vbg==",
      mediaType: "image/svg+xml",
    });
    render(
      <MessageContent
        blocks={[
          {
            type: "tool",
            id: "t-file-change",
            tool: "file",
            summary: "add logo.svg",
            card: "compact",
            status: "ok",
            exit_code: 0,
            output: "/repo/assets/logo.svg",
          },
        ]}
        onOpenPreview={vi.fn()}
        onOpenLightbox={vi.fn()}
      />,
    );

    await waitFor(() => expect(invoke).toHaveBeenCalledTimes(1));
    expect(invoke).toHaveBeenCalledWith("read_attachment", {
      path: "/repo/assets/logo.svg",
      sessionId: null,
    });
  });

  // T24b ③：工具产物缩略图套上统一样式类 al-chat-image（t24a 分支的
  // app/src/styles/chatImage.css 认这个类名——本分支不新起一套 CSS，只挂类名）。
  it("工具产物缩略图 <img> 带 al-chat-image 类名", async () => {
    vi.mocked(invoke).mockResolvedValueOnce({
      kind: "image",
      imageBase64: "bW9vbg==",
      mediaType: "image/png",
    });
    render(
      <MessageContent
        blocks={[
          {
            type: "tool",
            id: "t-al-chat-image",
            tool: "Bash",
            summary: "render chart",
            card: "command",
            status: "ok",
            exit_code: 0,
            output: "saved to /abs/out/chart.png",
          },
        ]}
        onOpenPreview={vi.fn()}
        onOpenLightbox={vi.fn()}
      />,
    );

    const image = await screen.findByRole("img");
    expect(image).toHaveClass("al-chat-image");
  });

  it("file 工具块（codex file_change）：非图片改动 output 为空 → 不出图", async () => {
    render(
      <MessageContent
        blocks={[
          {
            type: "tool",
            id: "t-file-change-non-image",
            tool: "file",
            summary: "add notes.txt",
            card: "compact",
            status: "ok",
            exit_code: 0,
            output: null,
          },
        ]}
        onOpenPreview={vi.fn()}
        onOpenLightbox={vi.fn()}
      />,
    );

    await waitFor(() => expect(invoke).not.toHaveBeenCalled());
  });

  it("tool 卡多图去重且缩略图列表保持换行布局", async () => {
    vi.mocked(invoke)
      .mockResolvedValueOnce({
        kind: "image",
        imageBase64: "bW9vbg==",
        mediaType: "image/png",
      })
      .mockResolvedValueOnce({
        kind: "image",
        imageBase64: "c3Rhcg==",
        mediaType: "image/png",
      });
    render(
      <MessageContent
        blocks={[
          {
            type: "tool",
            id: "t-deduplicate-image",
            tool: "Bash",
            summary: "created /abs/path/moon.png",
            card: "command",
            status: "ok",
            exit_code: 0,
            output:
              "preview /abs/path/moon.png and /abs/path/star.png then /abs/path/moon.png",
          },
        ]}
        onOpenPreview={vi.fn()}
        onOpenLightbox={vi.fn()}
      />,
    );

    const images = await screen.findAllByRole("img");
    expect(images).toHaveLength(2);
    expect(screen.getAllByAltText("moon.png")).toHaveLength(1);
    expect(images[0].closest("button")?.parentElement).toHaveStyle({
      display: "flex",
      flexWrap: "wrap",
      alignItems: "flex-start",
    });
  });

  it("同一折叠组多个 tool 引用同一路径 → 组下只渲染一个缩略图", async () => {
    vi.mocked(invoke).mockResolvedValueOnce({
      kind: "image",
      imageBase64: "bW9vbg==",
      mediaType: "image/png",
    });
    render(
      <MessageContent
        blocks={[
          {
            type: "tool",
            id: "t-first-shared-image",
            tool: "Bash",
            summary: "generate shared image",
            card: "command",
            status: "ok",
            exit_code: 0,
            output: "created /abs/path/moon.png",
          },
          {
            type: "tool",
            id: "t-second-shared-image",
            tool: "Bash",
            summary: "confirm shared image",
            card: "command",
            status: "ok",
            exit_code: 0,
            output: "confirmed /abs/path/moon.png",
          },
        ]}
        onOpenPreview={vi.fn()}
        onOpenLightbox={vi.fn()}
      />,
    );

    const previews = await screen.findAllByRole("button", {
      name: /moon\.png/,
    });
    const fold = screen.getByText("执行了 2 步").closest(".toolfold");

    expect(previews).toHaveLength(1);
    expect(fold?.nextElementSibling).toContainElement(previews[0]);
  });

  it("同一折叠组后续 tool 引用新路径 → 两张缩略图都在组下渲染", async () => {
    vi.mocked(invoke)
      .mockResolvedValueOnce({
        kind: "image",
        imageBase64: "bW9vbg==",
        mediaType: "image/png",
      })
      .mockResolvedValueOnce({
        kind: "image",
        imageBase64: "c3Rhcg==",
        mediaType: "image/png",
      });
    render(
      <MessageContent
        blocks={[
          {
            type: "tool",
            id: "t-first-image",
            tool: "Bash",
            summary: "generate first image",
            card: "command",
            status: "ok",
            exit_code: 0,
            output: "created /abs/path/moon.png",
          },
          {
            type: "tool",
            id: "t-second-new-image",
            tool: "Bash",
            summary: "copy and create second image",
            card: "command",
            status: "ok",
            exit_code: 0,
            output: "copied /abs/path/moon.png to /abs/path/star.png",
          },
        ]}
        onOpenPreview={vi.fn()}
        onOpenLightbox={vi.fn()}
      />,
    );

    const moonPreview = await screen.findByRole("button", {
      name: /moon\.png/,
    });
    const starPreview = await screen.findByRole("button", {
      name: /star\.png/,
    });
    const fold = screen.getByText("执行了 2 步").closest(".toolfold");

    expect(screen.getAllByRole("button", { name: /moon\.png/ })).toHaveLength(
      1,
    );
    expect(fold?.nextElementSibling).toContainElement(moonPreview);
    expect(fold?.nextElementSibling).toContainElement(starPreview);
  });

  it("未提供 onOpenPreview → 不渲染图片 chip", () => {
    render(
      <MessageContent
        blocks={[
          {
            type: "tool",
            id: "t-without-preview",
            tool: "Bash",
            summary: "render image artifact",
            card: "command",
            status: "ok",
            exit_code: 0,
            output: "saved to /abs/path/moon.png",
          },
        ]}
      />,
    );

    expect(screen.queryByRole("button", { name: /moon\.png/ })).toBeNull();
  });
});

describe("MessageContent 搜索类工具输出不误渲图片卡", () => {
  it("Grep 命中一堆 .png 路径 → 不渲染图片附件卡", () => {
    render(
      <MessageContent
        blocks={[
          {
            type: "tool",
            id: "t-grep-png",
            tool: "Grep",
            summary: "rg -l .png$",
            card: "command",
            status: "ok",
            exit_code: 0,
            output: "/abs/path/moon.png\n/abs/path/sun.png\n/abs/path/icon.svg",
          },
        ]}
        onOpenPreview={vi.fn()}
        onOpenLightbox={vi.fn()}
      />,
    );

    expect(screen.queryByAltText("moon.png")).toBeNull();
    expect(screen.queryByAltText("sun.png")).toBeNull();
    expect(screen.queryByAltText("icon.svg")).toBeNull();
    expect(invoke).not.toHaveBeenCalledWith(
      "read_attachment",
      expect.anything(),
    );
  });

  it("Glob 命中一堆 .svg 路径 → 不渲染图片附件卡", () => {
    render(
      <MessageContent
        blocks={[
          {
            type: "tool",
            id: "t-glob-svg",
            tool: "Glob",
            summary: "**/*.svg",
            card: "command",
            status: "ok",
            exit_code: 0,
            output: "/abs/path/logo.svg\n/abs/path/badge.svg",
          },
        ]}
        onOpenPreview={vi.fn()}
        onOpenLightbox={vi.fn()}
      />,
    );

    expect(screen.queryByAltText("logo.svg")).toBeNull();
    expect(screen.queryByAltText("badge.svg")).toBeNull();
    expect(invoke).not.toHaveBeenCalledWith(
      "read_attachment",
      expect.anything(),
    );
  });

  it("连续成功 Grep/Glob 折叠成组，output 含图片路径 → 折叠组下方不出图片卡", () => {
    const searchTool = (
      id: string,
      tool: "Grep" | "Glob",
      output: string,
    ): Block => ({
      type: "tool",
      id,
      tool,
      summary: `${tool} search`,
      card: "command",
      status: "ok",
      exit_code: 0,
      output,
    });

    render(
      <MessageContent
        blocks={[
          searchTool("s1", "Grep", "/abs/path/a.png"),
          searchTool("s2", "Glob", "/abs/path/b.png"),
          searchTool("s3", "Grep", "/abs/path/c.svg"),
        ]}
        onOpenPreview={vi.fn()}
        onOpenLightbox={vi.fn()}
      />,
    );

    expect(screen.getByText("执行了 3 步")).toBeInTheDocument();
    expect(screen.queryByAltText("a.png")).toBeNull();
    expect(screen.queryByAltText("b.png")).toBeNull();
    expect(screen.queryByAltText("c.svg")).toBeNull();
    expect(invoke).not.toHaveBeenCalledWith(
      "read_attachment",
      expect.anything(),
    );
  });
});

describe("MessageContent read 类与 verifier 工具输出不误渲图片卡", () => {
  it("Read 工具读到含 svg import 的文件内容 → 不渲染图片附件卡", () => {
    render(
      <MessageContent
        blocks={[
          {
            type: "tool",
            id: "t-read-svg-import",
            tool: "Read",
            summary: "AboutDialog.tsx",
            card: "command",
            status: "ok",
            exit_code: 0,
            output: 'import agentloomIcon from "../assets/agentloom-icon.svg";',
          },
        ]}
        onOpenPreview={vi.fn()}
        onOpenLightbox={vi.fn()}
      />,
    );

    expect(screen.queryByAltText("agentloom-icon.svg")).toBeNull();
    expect(invoke).not.toHaveBeenCalledWith(
      "read_attachment",
      expect.anything(),
    );
  });

  it("fs_read（myagent 名）读到含 svg import 的文件内容 → 不渲染图片附件卡", () => {
    render(
      <MessageContent
        blocks={[
          {
            type: "tool",
            id: "t-fs-read-svg-import",
            tool: "fs_read",
            summary: "AboutDialog.tsx",
            card: "command",
            status: "ok",
            exit_code: 0,
            output: 'import agentloomIcon from "../assets/agentloom-icon.svg";',
          },
        ]}
        onOpenPreview={vi.fn()}
        onOpenLightbox={vi.fn()}
      />,
    );

    expect(screen.queryByAltText("agentloom-icon.svg")).toBeNull();
    expect(invoke).not.toHaveBeenCalledWith(
      "read_attachment",
      expect.anything(),
    );
  });

  it("verifier 工具的测试日志含 .png 路径 → 不渲染图片附件卡", () => {
    render(
      <MessageContent
        blocks={[
          {
            type: "tool",
            id: "t-verifier-log",
            tool: "verifier",
            summary: "npm test",
            card: "command",
            status: "ok",
            exit_code: 0,
            output:
              "FAIL src/components/Snapshot.test.tsx\n  screenshot saved to /tmp/diff/mismatch.png",
          },
        ]}
        onOpenPreview={vi.fn()}
        onOpenLightbox={vi.fn()}
      />,
    );

    expect(screen.queryByAltText("mismatch.png")).toBeNull();
    expect(invoke).not.toHaveBeenCalledWith(
      "read_attachment",
      expect.anything(),
    );
  });
});
