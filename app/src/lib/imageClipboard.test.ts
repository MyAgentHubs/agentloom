import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { blobForClipboardCopy, dataUriToBlob } from "./imageClipboard";

const PNG_DATA_URI = "data:image/png;base64,aGVsbG8=";
const SVG_DATA_URI =
  "data:image/svg+xml;base64," +
  btoa('<svg xmlns="http://www.w3.org/2000/svg" width="200" height="100"/>');

describe("dataUriToBlob", () => {
  it("解出正确的 mime type", () => {
    const blob = dataUriToBlob(PNG_DATA_URI);
    expect(blob.type).toBe("image/png");
  });

  it("没有逗号分隔符时抛错", () => {
    expect(() => dataUriToBlob("data:image/png;base64")).toThrow();
  });
});

describe("blobForClipboardCopy — 位图直接沿用", () => {
  it("PNG data URI 不走栅格化，直接转 blob", async () => {
    const blob = await blobForClipboardCopy(PNG_DATA_URI);
    expect(blob.type).toBe("image/png");
  });
});

describe("blobForClipboardCopy — SVG 先栅格化成 PNG", () => {
  let toBlobMock: ReturnType<typeof vi.fn>;
  let getContextMock: ReturnType<typeof vi.fn>;
  let drawImageMock: ReturnType<typeof vi.fn>;
  let originalImage: typeof Image;

  beforeEach(() => {
    drawImageMock = vi.fn();
    getContextMock = vi.fn().mockReturnValue({ drawImage: drawImageMock });
    toBlobMock = vi.fn((cb: (blob: Blob | null) => void) => {
      cb(new Blob(["png"], { type: "image/png" }));
    });
    vi.spyOn(HTMLCanvasElement.prototype, "getContext").mockImplementation(
      getContextMock as never,
    );
    vi.spyOn(HTMLCanvasElement.prototype, "toBlob").mockImplementation(
      toBlobMock as never,
    );

    originalImage = globalThis.Image;
    class MockImage {
      onload: (() => void) | null = null;
      onerror: (() => void) | null = null;
      naturalWidth = 200;
      naturalHeight = 100;
      private _src = "";
      set src(value: string) {
        this._src = value;
        queueMicrotask(() => this.onload?.());
      }
      get src() {
        return this._src;
      }
    }
    // @ts-expect-error 测试替身，不需要实现完整 Image 接口
    globalThis.Image = MockImage;
  });

  afterEach(() => {
    vi.restoreAllMocks();
    globalThis.Image = originalImage;
  });

  it("按原始尺寸把 svg 画到 canvas，再用 toBlob 导出 image/png", async () => {
    const blob = await blobForClipboardCopy(SVG_DATA_URI);

    expect(getContextMock).toHaveBeenCalledWith("2d");
    expect(drawImageMock).toHaveBeenCalledWith(
      expect.anything(),
      0,
      0,
      200,
      100,
    );
    expect(toBlobMock).toHaveBeenCalledWith(expect.any(Function), "image/png");
    expect(blob.type).toBe("image/png");
  });

  it("超过上限的尺寸按上限裁切（不超过 4096）", async () => {
    class HugeImage {
      onload: (() => void) | null = null;
      onerror: (() => void) | null = null;
      naturalWidth = 8000;
      naturalHeight = 6000;
      private _src = "";
      set src(value: string) {
        this._src = value;
        queueMicrotask(() => this.onload?.());
      }
      get src() {
        return this._src;
      }
    }
    // @ts-expect-error 测试替身
    globalThis.Image = HugeImage;

    await blobForClipboardCopy(SVG_DATA_URI);

    expect(drawImageMock).toHaveBeenCalledWith(
      expect.anything(),
      0,
      0,
      4096,
      4096,
    );
  });

  it("svg 加载失败时抛错（调用方据此降级为复制路径）", async () => {
    class FailingImage {
      onload: (() => void) | null = null;
      onerror: (() => void) | null = null;
      private _src = "";
      set src(value: string) {
        this._src = value;
        queueMicrotask(() => this.onerror?.());
      }
      get src() {
        return this._src;
      }
    }
    // @ts-expect-error 测试替身
    globalThis.Image = FailingImage;

    await expect(blobForClipboardCopy(SVG_DATA_URI)).rejects.toThrow();
  });

  it("2d context 不可用时抛错", async () => {
    getContextMock.mockReturnValue(null);

    await expect(blobForClipboardCopy(SVG_DATA_URI)).rejects.toThrow();
  });
});
