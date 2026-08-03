const IMAGE_ATTACHMENT_MARKDOWN =
  /!\[[^\]\r\n]*\]\(\s*(?:<[^>\r\n]*>|[^)\r\n]*)\s*\)/g;
const ATTACHMENT_PREFIX_LINE =
  /^[ \t]*Attached (?:image|file):[^\r\n]*(?:\r?\n|$)/gim;

export function deriveSessionTitle(text: string): string {
  return text
    .replace(ATTACHMENT_PREFIX_LINE, "")
    .replace(IMAGE_ATTACHMENT_MARKDOWN, "")
    .trim()
    .slice(0, 30);
}
