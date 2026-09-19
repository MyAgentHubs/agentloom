/// Desktop (Tauri) image clipboard write: bypasses the browser Clipboard API's
/// user-activation window (in WKWebView, awaiting a blob / canvas toBlob and
/// then calling `navigator.clipboard.write` is frequently denied) by writing
/// to the system clipboard through the Rust backend instead.
import { isTauri } from "@tauri-apps/api/core";
import { Image } from "@tauri-apps/api/image";
import { writeImage } from "@tauri-apps/plugin-clipboard-manager";
import {
  blobForClipboardCopy,
  dataUriToBlob,
  MAX_RASTER_DIMENSION,
  rasterizeDataUriToPngBlob,
} from "./imageClipboard";

/// `Blob.arrayBuffer` is missing in some test environments (jsdom);
/// `FileReader` is the portable way to read bytes that works consistently
/// across real browsers, WKWebView, and jsdom.
function blobToBytes(blob: Blob): Promise<Uint8Array> {
  return new Promise((resolve, reject) => {
    const reader = new FileReader();
    reader.onload = () => resolve(new Uint8Array(reader.result as ArrayBuffer));
    reader.onerror = () =>
      reject(reader.error ?? new Error("blob read failed"));
    reader.readAsArrayBuffer(blob);
  });
}

function isPngDataUri(dataUri: string): boolean {
  return /^data:image\/png/i.test(dataUri);
}

/// `tauri::image::Image::fromBytes` only decodes png (the `image-png`
/// feature), so any non-png source — jpeg/webp/gif/svg alike — is rasterized
/// to png first; only a png source skips straight to bytes.
export async function writeImageTauri(
  dataUri: string,
  maxDimension: number = MAX_RASTER_DIMENSION,
): Promise<void> {
  const blob = isPngDataUri(dataUri)
    ? dataUriToBlob(dataUri)
    : await rasterizeDataUriToPngBlob(dataUri, maxDimension);
  const bytes = await blobToBytes(blob);
  const image = await Image.fromBytes(bytes);
  await writeImage(image);
}

/// Whether the "copy image" menu item should be enabled: desktop always can
/// (writes through the Tauri clipboard plugin, no `ClipboardItem` needed),
/// other environments need the browser Clipboard API to actually exist.
export function canCopyImageInEnv(
  clipboard: Pick<Clipboard, "write"> | undefined,
): boolean {
  if (isTauri()) return true;
  return (
    typeof clipboard?.write === "function" &&
    typeof ClipboardItem !== "undefined"
  );
}

/// Single entry point for copying an image: desktop uses the Tauri clipboard
/// plugin, other environments keep the original browser Clipboard API — the
/// caller (`MessageContent`) doesn't need its own branching.
export async function copyImageToClipboard(
  dataUri: string,
  clipboard: Pick<Clipboard, "write"> | undefined,
  canCopyImage: boolean,
): Promise<void> {
  if (isTauri()) {
    await writeImageTauri(dataUri);
    return;
  }
  if (!canCopyImage || !clipboard) {
    throw new Error("Clipboard image API unavailable");
  }
  const blob = await blobForClipboardCopy(dataUri);
  await clipboard.write([new ClipboardItem({ [blob.type]: blob })]);
}
