import { describe, expect, it, vi, beforeEach } from "vitest";

const fromBytesMock = vi.fn();
const writeImageMock = vi.fn();
const isTauriMock = vi.fn();

vi.mock("@tauri-apps/api/core", () => ({
  isTauri: (...args: unknown[]) => isTauriMock(...args),
}));

vi.mock("@tauri-apps/api/image", () => ({
  Image: { fromBytes: (...args: unknown[]) => fromBytesMock(...args) },
}));

vi.mock("@tauri-apps/plugin-clipboard-manager", () => ({
  writeImage: (...args: unknown[]) => writeImageMock(...args),
}));

import {
  canCopyImageInEnv,
  copyImageToClipboard,
  writeImageTauri,
} from "./imageClipboardTauri";

const PNG_DATA_URI = "data:image/png;base64,cG5nLWJ5dGVz"; // "png-bytes"
const SVG_DATA_URI = "data:image/svg+xml;base64," + btoa("<svg></svg>");
const JPEG_DATA_URI = "data:image/jpeg;base64,anBlZy1ieXRlcw=="; // "jpeg-bytes"

function stubCanvasRasterization(drawImageMock: ReturnType<typeof vi.fn>) {
  vi.spyOn(HTMLCanvasElement.prototype, "getContext").mockReturnValue({
    drawImage: drawImageMock,
  } as unknown as CanvasRenderingContext2D);
  vi.spyOn(HTMLCanvasElement.prototype, "toBlob").mockImplementation(
    (cb: BlobCallback) => cb(new Blob(["png"], { type: "image/png" })),
  );
  const originalImage = globalThis.Image;
  class MockImage {
    onload: (() => void) | null = null;
    onerror: (() => void) | null = null;
    naturalWidth = 32;
    naturalHeight = 32;
    private _src = "";
    set src(value: string) {
      this._src = value;
      queueMicrotask(() => this.onload?.());
    }
    get src() {
      return this._src;
    }
  }
  // @ts-expect-error test double, no need to implement the full Image interface
  globalThis.Image = MockImage;
  return () => {
    globalThis.Image = originalImage;
    vi.restoreAllMocks();
  };
}

beforeEach(() => {
  fromBytesMock.mockReset();
  writeImageMock.mockReset();
  isTauriMock.mockReset();
  fromBytesMock.mockResolvedValue({ __fakeImage: true });
  writeImageMock.mockResolvedValue(undefined);
  isTauriMock.mockReturnValue(false);
});

describe("writeImageTauri", () => {
  it("png：把字节交给 Image.fromBytes 再 writeImage，不走浏览器 clipboard", async () => {
    await writeImageTauri(PNG_DATA_URI);

    expect(fromBytesMock).toHaveBeenCalledTimes(1);
    const bytes = fromBytesMock.mock.calls[0][0] as Uint8Array;
    expect(bytes).toBeInstanceOf(Uint8Array);
    expect(writeImageMock).toHaveBeenCalledTimes(1);
    expect(writeImageMock).toHaveBeenCalledWith({ __fakeImage: true });
  });

  it("svg：先栅格化成 png 再交给 writeImage", async () => {
    const drawImageMock = vi.fn();
    vi.spyOn(HTMLCanvasElement.prototype, "getContext").mockReturnValue({
      drawImage: drawImageMock,
    } as unknown as CanvasRenderingContext2D);
    vi.spyOn(HTMLCanvasElement.prototype, "toBlob").mockImplementation(
      (cb: BlobCallback) => cb(new Blob(["png"], { type: "image/png" })),
    );
    const originalImage = globalThis.Image;
    class MockImage {
      onload: (() => void) | null = null;
      onerror: (() => void) | null = null;
      naturalWidth = 32;
      naturalHeight = 32;
      private _src = "";
      set src(value: string) {
        this._src = value;
        queueMicrotask(() => this.onload?.());
      }
      get src() {
        return this._src;
      }
    }
    // @ts-expect-error test double, no need to implement the full Image interface
    globalThis.Image = MockImage;

    await writeImageTauri(SVG_DATA_URI);

    expect(drawImageMock).toHaveBeenCalled();
    expect(fromBytesMock).toHaveBeenCalledTimes(1);
    expect(writeImageMock).toHaveBeenCalledTimes(1);

    globalThis.Image = originalImage;
    vi.restoreAllMocks();
  });

  it("writeImage 抛错时把错误原样抛给调用方（不吞异常）", async () => {
    writeImageMock.mockRejectedValue(new Error("system clipboard denied"));

    await expect(writeImageTauri(PNG_DATA_URI)).rejects.toThrow(
      "system clipboard denied",
    );
  });

  it("jpeg：非 png 格式在桌面路径下也先栅格化成 png 再交给 writeImage（Image.fromBytes 只解码 png）", async () => {
    const drawImageMock = vi.fn();
    const restore = stubCanvasRasterization(drawImageMock);

    await writeImageTauri(JPEG_DATA_URI);

    expect(drawImageMock).toHaveBeenCalled();
    expect(fromBytesMock).toHaveBeenCalledTimes(1);
    expect(writeImageMock).toHaveBeenCalledTimes(1);

    restore();
  });
});

describe("canCopyImageInEnv", () => {
  it("Tauri 环境下恒可用，不要求 navigator.clipboard 存在", () => {
    isTauriMock.mockReturnValue(true);
    expect(canCopyImageInEnv(undefined)).toBe(true);
  });

  it("非 Tauri 环境下要求 clipboard.write 与 ClipboardItem 都存在", () => {
    isTauriMock.mockReturnValue(false);
    // @ts-expect-error test double, no need to implement the full ClipboardItem interface
    globalThis.ClipboardItem = class {};
    expect(canCopyImageInEnv({ write: vi.fn() })).toBe(true);
    expect(canCopyImageInEnv(undefined)).toBe(false);
  });
});

describe("copyImageToClipboard", () => {
  it("① Tauri 环境下经 writeImage 写系统剪贴板，不调用 navigator.clipboard.write", async () => {
    isTauriMock.mockReturnValue(true);
    const write = vi.fn().mockResolvedValue(undefined);

    await copyImageToClipboard(PNG_DATA_URI, { write }, true);

    expect(writeImageMock).toHaveBeenCalledTimes(1);
    expect(write).not.toHaveBeenCalled();
  });

  it("② 非 Tauri 环境走浏览器 navigator.clipboard.write", async () => {
    isTauriMock.mockReturnValue(false);
    const write = vi.fn().mockResolvedValue(undefined);
    // @ts-expect-error test double, no need to implement the full ClipboardItem interface
    globalThis.ClipboardItem = class {
      constructor(public items: Record<string, Blob>) {}
    };

    await copyImageToClipboard(PNG_DATA_URI, { write }, true);

    expect(write).toHaveBeenCalledTimes(1);
    expect(writeImageMock).not.toHaveBeenCalled();
  });

  it("非 Tauri 且 canCopyImage 为 false 时直接抛错，不调用任何写入", async () => {
    isTauriMock.mockReturnValue(false);
    const write = vi.fn();

    await expect(
      copyImageToClipboard(PNG_DATA_URI, { write }, false),
    ).rejects.toThrow("Clipboard image API unavailable");
    expect(write).not.toHaveBeenCalled();
    expect(writeImageMock).not.toHaveBeenCalled();
  });

  it("③ svg 在 Tauri 路径下先栅格化再 writeImage", async () => {
    isTauriMock.mockReturnValue(true);
    const drawImageMock = vi.fn();
    vi.spyOn(HTMLCanvasElement.prototype, "getContext").mockReturnValue({
      drawImage: drawImageMock,
    } as unknown as CanvasRenderingContext2D);
    vi.spyOn(HTMLCanvasElement.prototype, "toBlob").mockImplementation(
      (cb: BlobCallback) => cb(new Blob(["png"], { type: "image/png" })),
    );
    const originalImage = globalThis.Image;
    class MockImage {
      onload: (() => void) | null = null;
      onerror: (() => void) | null = null;
      naturalWidth = 16;
      naturalHeight = 16;
      private _src = "";
      set src(value: string) {
        this._src = value;
        queueMicrotask(() => this.onload?.());
      }
      get src() {
        return this._src;
      }
    }
    // @ts-expect-error test double, no need to implement the full Image interface
    globalThis.Image = MockImage;

    await copyImageToClipboard(SVG_DATA_URI, undefined, true);

    expect(drawImageMock).toHaveBeenCalled();
    expect(writeImageMock).toHaveBeenCalledTimes(1);

    globalThis.Image = originalImage;
    vi.restoreAllMocks();
  });
});
