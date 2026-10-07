import { cloneElement, type ReactNode } from "react";
import { useAttachmentPort } from "../lib/attachmentPortContext";
import {
  decodeFilePath,
  isLocalFileReference,
  isPreviewablePath,
} from "../lib/chatFilePath";
import { PathContextTarget } from "./PathContextTarget";

export function MarkdownFileLink({
  children,
  href,
  sessionId,
  onOpenPreview,
  onError,
}: {
  children?: ReactNode;
  href?: string;
  sessionId?: string | null;
  onOpenPreview?: (path: string) => void;
  onError: (error: unknown) => void;
}) {
  const attachmentPort = useAttachmentPort();
  const external = !!href && /^https?:\/\//i.test(href);
  const local = !!href && isLocalFileReference(href);
  const path = href ? decodeFilePath(href) : "";
  const link = (
    <a
      href={href}
      onClick={(event) => {
        event.preventDefault();
        if (external) {
          void attachmentPort.openUrl(href!).catch(() => {});
        } else if (local && isPreviewablePath(href!)) {
          if (/\.html?$/i.test(path)) {
            void attachmentPort
              .openExternal(path, sessionId ?? null)
              .catch(onError);
          } else {
            onOpenPreview?.(path);
          }
        }
      }}
    >
      {children}
    </a>
  );
  return local ? (
    <PathContextTarget path={path}>
      {(props) => cloneElement(link, props)}
    </PathContextTarget>
  ) : (
    link
  );
}
