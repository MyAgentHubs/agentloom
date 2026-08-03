import { act, render, renderHook, waitFor } from "@testing-library/react";
import { useEffect, type ReactNode } from "react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { GenerationEvent } from "../types/repoDocument";

const { invokeMock, listenMock } = vi.hoisted(() => ({
  invokeMock: vi.fn(),
  listenMock: vi.fn(),
}));

vi.mock("@tauri-apps/api/core", () => ({ invoke: invokeMock }));
vi.mock("@tauri-apps/api/event", () => ({ listen: listenMock }));

import { useRepoDocument } from "./useRepoDocument";
import { RepoDocumentProvider } from "../contexts/RepoDocumentProvider";

function wrapper({ children }: { children: ReactNode }) {
  return <RepoDocumentProvider>{children}</RepoDocumentProvider>;
}

describe("useRepoDocument", () => {
  let eventHandler: ((event: { payload: GenerationEvent }) => void) | null;

  beforeEach(() => {
    invokeMock.mockReset();
    listenMock.mockReset();
    eventHandler = null;
    listenMock.mockImplementation(
      async (_channel: string, handler: typeof eventHandler) => {
        eventHandler = handler;
        return vi.fn();
      },
    );
  });

  function emit(payload: GenerationEvent) {
    act(() => eventHandler?.({ payload }));
  }

  it("loads a stored stale document", async () => {
    const stored = {
      repo_id: "repo-1",
      content: "stored",
      generated_at: 100,
      head_sha: "old-sha",
      stale: true,
    };
    invokeMock.mockResolvedValueOnce(stored);
    const { result } = renderHook(() => useRepoDocument("repo-1", "intro"), {
      wrapper,
    });

    await waitFor(() => expect(result.current.loading).toBe(false));
    expect(invokeMock).toHaveBeenCalledWith("get_project_intro", {
      repoId: "repo-1",
    });
    expect(result.current.doc).toEqual(stored);
    expect(result.current.error).toBeNull();
  });

  it("keeps doc null when no stored document exists", async () => {
    invokeMock.mockResolvedValueOnce(null);
    const { result } = renderHook(() => useRepoDocument("repo-1", "daily"), {
      wrapper,
    });

    await waitFor(() => expect(result.current.loading).toBe(false));
    expect(result.current.doc).toBeNull();
    expect(result.current.error).toBeNull();
  });

  it("accumulates deltas and installs the completed document", async () => {
    invokeMock
      .mockResolvedValueOnce(null)
      .mockResolvedValueOnce({ run_id: "run-1" });
    const { result } = renderHook(() => useRepoDocument("repo-1", "intro"), {
      wrapper,
    });
    await waitFor(() => expect(result.current.loading).toBe(false));

    act(() => result.current.generate("agent-1"));
    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("generate_project_intro", {
        repoId: "repo-1",
        agentId: "agent-1",
      }),
    );
    emit({
      feature: "project_intro",
      phase: "started",
      repo_id: "repo-1",
      run_id: "run-1",
    });
    emit({
      feature: "project_intro",
      phase: "delta",
      repo_id: "repo-1",
      run_id: "run-1",
      delta: "hello ",
    });
    emit({
      feature: "project_intro",
      phase: "delta",
      repo_id: "repo-1",
      run_id: "run-1",
      delta: "world",
    });
    expect(result.current.liveText).toBe("hello world");

    emit({
      feature: "project_intro",
      phase: "completed",
      repo_id: "repo-1",
      run_id: "run-1",
      document: {
        repo_id: "repo-1",
        content: "final document",
        generated_at: 200,
        head_sha: "new-sha",
      },
    });
    expect(result.current.doc?.content).toBe("final document");
    expect(result.current.doc?.stale).toBe(false);
    expect(result.current.generating).toBe(false);
    expect(result.current.liveText).toBe("");
  });

  it("ignores events for another feature or run", async () => {
    invokeMock
      .mockResolvedValueOnce(null)
      .mockResolvedValueOnce({ run_id: "run-1" });
    const { result } = renderHook(() => useRepoDocument("repo-1", "daily"), {
      wrapper,
    });
    await waitFor(() => expect(result.current.loading).toBe(false));
    act(() => result.current.generate("agent-1"));
    await waitFor(() => expect(invokeMock).toHaveBeenCalledTimes(2));

    emit({
      feature: "project_intro",
      phase: "delta",
      repo_id: "repo-1",
      run_id: "run-1",
      delta: "wrong feature",
    });
    emit({
      feature: "daily",
      phase: "delta",
      repo_id: "repo-1",
      run_id: "run-2",
      delta: "wrong run",
    });
    expect(result.current.liveText).toBe("");
    expect(result.current.doc).toBeNull();
  });

  it("reports generation errors and leaves generating state", async () => {
    invokeMock
      .mockResolvedValueOnce(null)
      .mockResolvedValueOnce({ run_id: "run-1" });
    const { result } = renderHook(() => useRepoDocument("repo-1", "daily"), {
      wrapper,
    });
    await waitFor(() => expect(result.current.loading).toBe(false));
    act(() => result.current.generate("agent-1"));
    await waitFor(() => expect(invokeMock).toHaveBeenCalledTimes(2));

    emit({
      feature: "daily",
      phase: "error",
      repo_id: "repo-1",
      run_id: "run-1",
      message: "agent unavailable",
    });
    expect(result.current.error).toBe("agent unavailable");
    expect(result.current.generating).toBe(false);
  });

  it("keeps generation result when the consumer leaves and returns", async () => {
    invokeMock
      .mockResolvedValueOnce(null)
      .mockResolvedValueOnce({ run_id: "run-1" });
    const observed: {
      current: ReturnType<typeof useRepoDocument> | null;
    } = { current: null };

    function Probe() {
      const value = useRepoDocument("repo-1", "intro");
      useEffect(() => {
        observed.current = value;
      }, [value]);
      return null;
    }

    function Harness({ visible }: { visible: boolean }) {
      return (
        <RepoDocumentProvider>
          {visible ? <Probe /> : null}
        </RepoDocumentProvider>
      );
    }

    const view = render(<Harness visible />);
    await waitFor(() => expect(observed.current?.loading).toBe(false));
    act(() => observed.current?.generate("agent-1"));
    await waitFor(() => expect(observed.current?.generating).toBe(true));

    view.rerender(<Harness visible={false} />);
    emit({
      feature: "project_intro",
      phase: "completed",
      repo_id: "repo-1",
      run_id: "run-1",
      document: {
        repo_id: "repo-1",
        content: "completed while away",
        generated_at: 300,
        head_sha: "new-sha",
      },
    });
    view.rerender(<Harness visible />);

    await waitFor(() =>
      expect(observed.current?.doc?.content).toBe("completed while away"),
    );
    expect(observed.current?.generating).toBe(false);
    expect(invokeMock).toHaveBeenCalledTimes(2);
  });
});
