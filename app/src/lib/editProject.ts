/** T22：编辑项目保存时依次调用的后端命令编排——从 App.tsx 抽出以省 App.tsx 行数
 * （App.tsx 行数已顶到 check_file_size.py 门禁基线，任何净增都可能变红）。
 * 路径没变时不调用 update_project_path（避免无意义地触发后端校验 + last_used_at 刷新）。
 */
export type InvokeFn = <T>(
  cmd: string,
  args?: Record<string, unknown>,
) => Promise<T>;

export async function saveProjectEdits(
  invoke: InvokeFn,
  repo: { id: string; name: string },
  args: { name: string; icon: string | null; path: string | null },
): Promise<void> {
  if (args.path) {
    await invoke("update_project_path", { id: repo.id, newPath: args.path });
  }
  if (args.name !== repo.name) {
    await invoke("rename_repo", { id: repo.id, name: args.name });
  }
  await invoke("set_repo_icon", { id: repo.id, icon: args.icon });
}
