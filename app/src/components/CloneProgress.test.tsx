import { render, screen, fireEvent } from "@testing-library/react";
import { describe, it, expect, vi } from "vitest";
import { CloneProgress } from "./CloneProgress";
import type { CloneRowState } from "../types/repoManage";

const progress: Record<string, CloneRowState> = {
  "github.com/acme/a": { phase: "done", repoId: "r1" },
  "github.com/acme/b": { phase: "cloning" },
  "github.com/acme/c": { phase: "fail", message: "远端 403 · 凭据可能过期" },
  "github.com/acme/d": { phase: "occupied", message: "Error: PATH_OCCUPIED" },
};

describe("CloneProgress", () => {
  it("dest 头行 + nonblock 提示行渲染", () => {
    render(
      <CloneProgress
        destLabel="~/code/github.com/acme/"
        rows={progress}
        onRetry={vi.fn()}
        onOpenSession={vi.fn()}
      />,
    );
    expect(screen.getByText(/~\/code\/github.com\/acme\//)).toBeTruthy();
    expect(screen.getByText(/非阻塞/)).toBeTruthy();
  });
  it("done 行可打开会话，fail 行可重试", () => {
    const onOpenSession = vi.fn();
    const onRetry = vi.fn();
    render(
      <CloneProgress
        destLabel="~/code/"
        rows={progress}
        onRetry={onRetry}
        onOpenSession={onOpenSession}
      />,
    );
    fireEvent.click(screen.getByText("打开会话"));
    expect(onOpenSession).toHaveBeenCalledWith("r1");
    fireEvent.click(screen.getByText("重试"));
    expect(onRetry).toHaveBeenCalledWith("github.com/acme/c");
  });
  it("fail 行显示错误文案", () => {
    render(
      <CloneProgress
        destLabel="~/code/"
        rows={progress}
        onRetry={vi.fn()}
        onOpenSession={vi.fn()}
      />,
    );
    expect(screen.getByText(/远端 403/)).toBeTruthy();
  });
  it("occupied 行按结构化 phase 显示本地化文案", () => {
    render(
      <CloneProgress
        destLabel="~/code/"
        rows={progress}
        onRetry={vi.fn()}
        onOpenSession={vi.fn()}
      />,
    );
    expect(screen.getByText("位置被占用")).toBeTruthy();
    expect(screen.queryByText("Error: PATH_OCCUPIED")).toBeNull();
  });
});
