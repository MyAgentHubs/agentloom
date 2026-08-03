const attachmentDataUris = new Map<string, string>();

export function attachmentCacheKey(
  path: string,
  sessionId?: string | null,
): string {
  return `${sessionId ?? ""}::${path}`;
}

export function getAttachmentDataUri(
  path: string,
  sessionId?: string | null,
): string | null {
  return attachmentDataUris.get(attachmentCacheKey(path, sessionId)) ?? null;
}

export function setAttachmentDataUri(
  path: string,
  sessionId: string | null | undefined,
  dataUri: string,
): void {
  attachmentDataUris.set(attachmentCacheKey(path, sessionId), dataUri);
}

export function clearAttachmentCache(): void {
  attachmentDataUris.clear();
}
