import { describe, it, expect, vi } from "vitest";
import { runClones } from "./cloneOrchestrator";

describe("runClones", () => {
  it("每仓独立：成功置 done、失败置 fail，互不影响", async () => {
    const updates: Array<[string, string]> = [];
    const onUpdate = (k: string, st: any) => updates.push([k, st.phase]);

    await runClones(
      ["a", "b"],
      async (k) => {
        if (k === "b") throw "远端 403";
        return { repoId: "r-" + k };
      },
      onUpdate,
    );

    expect(updates).toContainEqual(["a", "cloning"]);
    expect(updates).toContainEqual(["a", "done"]);
    expect(updates).toContainEqual(["b", "fail"]);
  });

  it("重试=单 key 重发", async () => {
    const onUpdate = vi.fn();

    await runClones(["a"], async () => ({ repoId: "r1" }), onUpdate);

    expect(onUpdate).toHaveBeenCalledWith("a", {
      phase: "done",
      repoId: "r1",
    });
  });

  it("位置被占用错误置 occupied，避免并入可重试失败", async () => {
    const onUpdate = vi.fn();

    await runClones(
      ["github.com/octo/occupied"],
      async () => {
        throw new Error("PATH_OCCUPIED");
      },
      onUpdate,
    );

    expect(onUpdate).toHaveBeenCalledWith("github.com/octo/occupied", {
      phase: "occupied",
      message: "Error: PATH_OCCUPIED",
    });
  });

  it("最多只让 concurrency 个 clone 同时在飞", async () => {
    let inFlight = 0;
    let maxInFlight = 0;
    const started: string[] = [];

    await runClones(
      ["a", "b", "c", "d", "e"],
      async (key) => {
        started.push(key);
        inFlight += 1;
        maxInFlight = Math.max(maxInFlight, inFlight);
        await new Promise((resolve) => setTimeout(resolve, 10));
        inFlight -= 1;
        return { repoId: `r-${key}` };
      },
      vi.fn(),
      2,
    );

    expect(started).toEqual(["a", "b", "c", "d", "e"]);
    expect(maxInFlight).toBeLessThanOrEqual(2);
  });
});
