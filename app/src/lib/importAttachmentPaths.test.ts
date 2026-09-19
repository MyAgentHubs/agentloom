import { describe, expect, it, vi, beforeEach } from "vitest";

const invokeMock = vi.fn();
vi.mock("@tauri-apps/api/core", () => ({
  invoke: (...args: unknown[]) => invokeMock(...args),
}));

import { importAttachmentPaths } from "./importAttachmentPaths";

beforeEach(() => {
  invokeMock.mockReset();
});

describe("importAttachmentPaths", () => {
  it("sessionId 为 null 时原样返回，不调用后端命令", async () => {
    const paths = ["/Users/me/a.png", "/Users/me/b.txt"];

    const result = await importAttachmentPaths(paths, null);

    expect(result).toEqual(paths);
    expect(invokeMock).not.toHaveBeenCalled();
  });

  it("sessionId 有值时逐个调用 import_attachment_into_workspace_cmd 并用返回值替换路径", async () => {
    invokeMock.mockImplementation(async (_cmd: string, args: unknown) => {
      const { path } = args as { path: string };
      return `/repo/proj/.agentloom/attachments/${path.split("/").pop()}`;
    });

    const result = await importAttachmentPaths(
      ["/Users/me/a.png", "/Users/me/b.txt"],
      "sess-1",
    );

    expect(result).toEqual([
      "/repo/proj/.agentloom/attachments/a.png",
      "/repo/proj/.agentloom/attachments/b.txt",
    ]);
    expect(invokeMock).toHaveBeenCalledWith(
      "import_attachment_into_workspace_cmd",
      { sessionId: "sess-1", path: "/Users/me/a.png" },
    );
    expect(invokeMock).toHaveBeenCalledWith(
      "import_attachment_into_workspace_cmd",
      { sessionId: "sess-1", path: "/Users/me/b.txt" },
    );
  });

  it("单个文件拷贝失败时退回原路径，不阻断其它文件", async () => {
    const errorSpy = vi.spyOn(console, "error").mockImplementation(() => {});
    invokeMock.mockImplementation(async (_cmd: string, args: unknown) => {
      const { path } = args as { path: string };
      if (path.endsWith("bad.png")) throw new Error("copy failed");
      return "/repo/proj/.agentloom/attachments/good.png";
    });

    const result = await importAttachmentPaths(
      ["/Users/me/good.png", "/Users/me/bad.png"],
      "sess-1",
    );

    expect(result).toEqual([
      "/repo/proj/.agentloom/attachments/good.png",
      "/Users/me/bad.png",
    ]);
    errorSpy.mockRestore();
  });
});
