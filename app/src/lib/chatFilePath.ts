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
