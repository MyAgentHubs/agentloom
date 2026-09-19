/// 把消息里图片的 data URI 转成写剪贴板 / 下载用的 Blob（规则 C）。

export function dataUriToBlob(dataUri: string): Blob {
  const commaIndex = dataUri.indexOf(",");
  if (commaIndex < 0) throw new Error("Invalid image data URI");

  const metadata = dataUri.slice(0, commaIndex);
  const mediaType = metadata.match(/^data:([^;,]+)/)?.[1] || "image/png";
  const encoded = dataUri.slice(commaIndex + 1);
  const decoded = metadata.includes(";base64")
    ? atob(encoded)
    : decodeURIComponent(encoded);
  const bytes = Uint8Array.from(decoded, (character) =>
    character.charCodeAt(0),
  );
  return new Blob([bytes], { type: mediaType });
}

export const MAX_RASTER_DIMENSION = 4096;

export function isSvgDataUri(dataUri: string): boolean {
  return /^data:image\/svg\+xml/i.test(dataUri);
}

/// Renders any `<img>`-loadable data URI onto a canvas at its original size
/// (capped at maxDimension to avoid oversized canvases) and exports it as a
/// PNG blob. Started as an svg-only helper (system clipboard rejects
/// `image/svg+xml`); the desktop clipboard path also uses it for jpeg/webp/gif
/// since `tauri::image::Image::fromBytes` only decodes png. Image load
/// failure / missing 2d context / toBlob failure all throw; callers decide
/// their own fallback.
export async function rasterizeDataUriToPngBlob(
  dataUri: string,
  maxDimension: number,
): Promise<Blob> {
  const image = new Image();
  const loaded = new Promise<void>((resolve, reject) => {
    image.onload = () => resolve();
    image.onerror = () => reject(new Error("svg image failed to load"));
  });
  image.src = dataUri;
  await loaded;

  const width = Math.max(1, Math.min(image.naturalWidth || 1, maxDimension));
  const height = Math.max(1, Math.min(image.naturalHeight || 1, maxDimension));

  const canvas = document.createElement("canvas");
  canvas.width = width;
  canvas.height = height;
  const ctx = canvas.getContext("2d");
  if (!ctx) throw new Error("2d canvas context unavailable");
  ctx.drawImage(image, 0, 0, width, height);

  return await new Promise<Blob>((resolve, reject) => {
    canvas.toBlob((blob) => {
      if (blob) resolve(blob);
      else reject(new Error("svg canvas rasterization failed"));
    }, "image/png");
  });
}

/// 剪贴板写图的统一入口：svg 先栅格化成 PNG，其余位图直接转 blob。
export async function blobForClipboardCopy(
  dataUri: string,
  maxDimension: number = MAX_RASTER_DIMENSION,
): Promise<Blob> {
  if (isSvgDataUri(dataUri)) {
    return rasterizeDataUriToPngBlob(dataUri, maxDimension);
  }
  return dataUriToBlob(dataUri);
}
