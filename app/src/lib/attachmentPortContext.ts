import { createContext, useContext } from "react";
import { invoke } from "@tauri-apps/api/core";
import { openUrl } from "@tauri-apps/plugin-opener";
import type { AttachmentPort } from "./remoteSessionPort";

// AttachmentPort 的桌面默认实现：逐字节照抄改造前各组件里的 invoke 调用，只是把
// 「invoke 参数拼装 + data URI 拼接」搬进这一个模块，行为不变——不注入 provider 时，
// 消费方经 useAttachmentPort() 拿到的就是这份实现，走同一条 Tauri 命令。

type AttachmentContent = {
  kind: "text" | "image" | "binary";
  imageBase64?: string;
  mediaType?: string;
};

async function resolveImageSrc(
  path: string,
  sessionId?: string | null,
): Promise<string | null> {
  try {
    const attachment = await invoke<AttachmentContent>("read_attachment", {
      path,
      sessionId: sessionId ?? null,
    });
    if (attachment.imageBase64 && attachment.mediaType) {
      return `data:${attachment.mediaType};base64,${attachment.imageBase64}`;
    }
    return null;
  } catch {
    return null;
  }
}

function openExternal(path: string, sessionId?: string | null): Promise<void> {
  return invoke("open_attachment_external", {
    sessionId: sessionId ?? null,
    path,
  });
}

export const defaultAttachmentPort: AttachmentPort = {
  resolveImageSrc,
  openExternal,
  openUrl: (url: string) => openUrl(url),
};

export const AttachmentPortContext = createContext<AttachmentPort>(
  defaultAttachmentPort,
);

export function useAttachmentPort(): AttachmentPort {
  return useContext(AttachmentPortContext);
}
