import { act, render, waitFor } from "@testing-library/react";
import { describe, it, expect, vi } from "vitest";
import App from "./App";
import * as draftStore from "./lib/composerDraftStore";
import { setupAppTests } from "./__tests__/helpers/appTestSetup";
import { makeSession } from "./test/factories";

const { invokeMock, listenMock, openMock } = vi.hoisted(() => ({
  invokeMock: vi.fn(),
  listenMock: vi.fn(),
  openMock: vi.fn(),
}));

vi.mock("@tauri-apps/api/core", () => ({
  invoke: (...args: unknown[]) =>
    (invokeMock as (...a: unknown[]) => unknown)(...args),
}));
vi.mock("@tauri-apps/api/event", () => ({ listen: listenMock }));
vi.mock("@tauri-apps/plugin-dialog", () => ({ open: openMock }));
vi.mock("./lib/composerDraftStore", async (importOriginal) => {
  const actual =
    await importOriginal<typeof import("./lib/composerDraftStore")>();
  return {
    ...actual,
    pruneDraftsForSessions: vi.fn(actual.pruneDraftsForSessions),
  };
});

const KEY = (id: string) => `agentloom.draft.${id}`;
const settle = () =>
  act(async () => {
    await new Promise((r) => setTimeout(r, 30));
  });

describe("App startup draft pruning", () => {
  const { mockAppWith } = setupAppTests({
    invokeMock,
    listenMock,
    openMock,
    sessionMainProps: [],
  });

  function seedDrafts() {
    for (const id of ["live", "archived", "orphan", "__new__"]) {
      draftStore.saveDraft(id === "__new__" ? null : id, {
        text: `draft ${id}`,
        attachments: [],
      });
    }
  }

  it("prunes orphans once after the first successful list, keeping archived and new-session drafts", async () => {
    seedDrafts();
    mockAppWith([
      makeSession({
        id: "live",
        repo_id: "local-default",
        namespace_id: "local",
      }),
      makeSession({
        id: "archived",
        repo_id: "local-default",
        namespace_id: "local",
        archived: true,
      }),
    ]);
    render(<App />);
    await waitFor(() => expect(localStorage.getItem(KEY("orphan"))).toBeNull());
    await settle();
    expect(localStorage.getItem(KEY("live"))).not.toBeNull();
    expect(localStorage.getItem(KEY("archived"))).not.toBeNull();
    expect(localStorage.getItem(KEY("__new__"))).not.toBeNull();
    const spy = vi.mocked(draftStore.pruneDraftsForSessions);
    expect(spy).toHaveBeenCalledTimes(1);
    expect(spy.mock.calls[0][0].map((s) => s.id).sort()).toEqual([
      "archived",
      "live",
    ]);
  });

  it("keeps every draft when the session list fails to load", async () => {
    seedDrafts();
    vi.mocked(draftStore.pruneDraftsForSessions).mockClear();
    mockAppWith([], { list_sessions: () => Promise.reject("boom") });
    render(<App />);
    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("list_sessions"),
    );
    await settle();
    expect(draftStore.pruneDraftsForSessions).not.toHaveBeenCalled();
    for (const id of ["live", "archived", "orphan", "__new__"]) {
      expect(localStorage.getItem(KEY(id))).not.toBeNull();
    }
  });

  it("keeps every draft while the session list is still loading", async () => {
    seedDrafts();
    vi.mocked(draftStore.pruneDraftsForSessions).mockClear();
    mockAppWith([], { list_sessions: () => new Promise(() => {}) });
    render(<App />);
    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("list_sessions"),
    );
    await settle();
    expect(draftStore.pruneDraftsForSessions).not.toHaveBeenCalled();
    for (const id of ["live", "archived", "orphan", "__new__"]) {
      expect(localStorage.getItem(KEY(id))).not.toBeNull();
    }
  });
});
