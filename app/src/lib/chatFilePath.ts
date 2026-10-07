const PREVIEWABLE_PATH =
  /^[^\s`()]+\.(md|markdown|mdx|txt|log|svg|png|jpe?g|gif|webp|bmp|ico|html?|json|ya?ml|toml|ini|cfg|conf|xml|csv|tsx?|jsx?|mjs|cjs|py|rs|go|java|kt|rb|php|c|cc|cpp|h|hpp|cs|swift|sh|bash|zsh|sql|css|scss|less|vue|svelte)$/i;

export function isPreviewablePath(path: string): boolean {
  return path.length <= 512 && PREVIEWABLE_PATH.test(path);
}

export function isLocalFileReference(path: string): boolean {
  if (!path || /^(?:\/\/|#|\?)/.test(path)) return false;
  if (/^[a-z][a-z\d+.-]*:/i.test(path) && !/^[a-z]:[\\/]/i.test(path))
    return false;
  return (
    /^(?:\.{0,2}\/|~\/|[a-z]:[\\/])/i.test(path) || /\.[a-z\d]+$/i.test(path)
  );
}

export function decodeFilePath(path: string): string {
  try {
    return decodeURIComponent(path);
  } catch {
    return path;
  }
}

// Receives the Markdown URL before the navigation/image transforms touch it.
// Code spans are literal paths and must never pass through URL decoding.
export function copyPathFromUrl(url: string): string | undefined {
  if (/^file:/i.test(url)) {
    const match = /^file:(\/{1,3})(.*)$/i.exec(url);
    if (!match || match[1].length === 2 || match[2].startsWith("/")) return;
    return decodeFilePath("/" + match[2]);
  }
  return isLocalFileReference(url) ? decodeFilePath(url) : undefined;
}
