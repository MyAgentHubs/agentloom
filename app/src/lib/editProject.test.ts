import { describe, expect, it, vi } from "vitest";
import { saveProjectEdits } from "./editProject";

describe("saveProjectEdits", () => {
  it("路径变化时调用 update_project_path，且用新路径", async () => {
    const invoke = vi.fn().mockResolvedValue(undefined);
    await saveProjectEdits(
      invoke,
      { id: "r1", name: "旧名" },
      { name: "旧名", icon: null, path: "/new/path" },
    );

    expect(invoke).toHaveBeenCalledWith("update_project_path", {
      id: "r1",
      newPath: "/new/path",
    });
  });

  it("路径为 null 时不调用 update_project_path", async () => {
    const invoke = vi.fn().mockResolvedValue(undefined);
    await saveProjectEdits(
      invoke,
      { id: "r1", name: "旧名" },
      { name: "旧名", icon: null, path: null },
    );

    expect(invoke).not.toHaveBeenCalledWith(
      "update_project_path",
      expect.anything(),
    );
  });

  it("名称未变时不调用 rename_repo", async () => {
    const invoke = vi.fn().mockResolvedValue(undefined);
    await saveProjectEdits(
      invoke,
      { id: "r1", name: "旧名" },
      { name: "旧名", icon: null, path: null },
    );

    expect(invoke).not.toHaveBeenCalledWith("rename_repo", expect.anything());
  });

  it("名称变化时调用 rename_repo；总是调用 set_repo_icon", async () => {
    const invoke = vi.fn().mockResolvedValue(undefined);
    await saveProjectEdits(
      invoke,
      { id: "r1", name: "旧名" },
      { name: "新名", icon: "🚀", path: null },
    );

    expect(invoke).toHaveBeenCalledWith("rename_repo", {
      id: "r1",
      name: "新名",
    });
    expect(invoke).toHaveBeenCalledWith("set_repo_icon", {
      id: "r1",
      icon: "🚀",
    });
  });

  it("update_project_path 失败时不再调用后续命令", async () => {
    const invoke = vi
      .fn()
      .mockRejectedValueOnce(new Error("AL_ERR:project.pathNotWritable"));
    await expect(
      saveProjectEdits(
        invoke,
        { id: "r1", name: "旧名" },
        { name: "新名", icon: null, path: "/bad/path" },
      ),
    ).rejects.toThrow();

    expect(invoke).toHaveBeenCalledTimes(1);
  });
});
