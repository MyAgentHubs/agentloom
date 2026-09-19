import { invoke } from "@tauri-apps/api/core";

/**
 * 附件对话框选中文件后的落点决策：有会话工作区就把每个路径拷进
 * `<工作区>/.agentloom/attachments/`（`import_attachment_into_workspace` 命令要求
 * `session_id`，没有会话时不调用、原样返回——保持旧行为，读取时走 read_attachment
 * 的 A/B 规则）。单个文件拷贝失败不阻断其它文件，退回原路径并打日志。
 */
export async function importAttachmentPaths(
  paths: string[],
  sessionId: string | null,
): Promise<string[]> {
  if (!sessionId) return paths;
  return Promise.all(
    paths.map(async (path) => {
      try {
        return await invoke<string>("import_attachment_into_workspace_cmd", {
          sessionId,
          path,
        });
      } catch (error) {
        console.error("Failed to import attachment into workspace", error);
        return path;
      }
    }),
  );
}
