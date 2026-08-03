import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { invoke } from "@tauri-apps/api/core";
import { I18nProvider } from "../i18n";
import { clearAttachmentCache } from "../lib/attachmentCache";
import { Lightbox } from "./Lightbox";

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
