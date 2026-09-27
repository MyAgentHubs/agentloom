/** Keep the backend command sequence for saving an edited project outside App.tsx
 * to control the main App component's file size.
 * Only call update_project_path when the path changes, avoiding needless backend validation and last_used_at refreshes.
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
