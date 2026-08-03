import { render, screen, waitFor } from "@testing-library/react";
import mermaid from "mermaid";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { MermaidBlock } from "./MermaidBlock";

vi.mock("mermaid", () => ({
  default: {
    initialize: vi.fn(),
    render: vi.fn(async (_id: string, _code: string) => ({
      svg: '<svg data-testid="diagram"><g/></svg>',
    })),
  },
}));

describe("MermaidBlock", () => {
  beforeEach(() => {
    vi.mocked(mermaid.render).mockReset();
    vi.mocked(mermaid.render).mockImplementation(
      async () =>
        ({
          svg: '<svg data-testid="diagram"><g/></svg>',
        }) as Awaited<ReturnType<typeof mermaid.render>>,
    );
    vi.mocked(mermaid.initialize).mockReset();
  });

  it("renders the diagram svg when complete", async () => {
    render(<MermaidBlock code="graph TD; A-->B" complete={true} />);

    await waitFor(() =>
      expect(screen.getByTestId("diagram")).toBeInTheDocument(),
    );
  });

  it("shows raw source without rendering while incomplete", () => {
    render(<MermaidBlock code="graph TD; A-->B" complete={false} />);

    expect(screen.getByText(/graph TD/).tagName).toBe("PRE");
    expect(mermaid.render).not.toHaveBeenCalled();
  });

  it("falls back to raw source when rendering rejects", async () => {
    vi.mocked(mermaid.render).mockRejectedValueOnce(new Error("bad"));

    render(<MermaidBlock code="graph TD; A-->B" complete={true} />);

    await waitFor(() =>
      expect(screen.getByText(/graph TD/)).toBeInTheDocument(),
    );
    expect(screen.queryByTestId("diagram")).toBeNull();
  });
});
