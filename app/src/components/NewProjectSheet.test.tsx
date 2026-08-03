import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { I18nProvider } from "../i18n";
import { NewProjectSheet } from "./NewProjectSheet";

const { openMock } = vi.hoisted(() => ({ openMock: vi.fn() }));

vi.mock("@tauri-apps/plugin-dialog", () => ({ open: openMock }));

function renderSheet(
  overrides: {
    onClose?: () => void;
    onCreate?: ReturnType<typeof vi.fn>;
    mode?: "create" | "edit";
    initial?: { name: string; icon: string | null };
    onSave?: ReturnType<typeof vi.fn>;
    onRemove?: ReturnType<typeof vi.fn>;
  } = {},
) {
  const onClose = overrides.onClose ?? vi.fn();
  const onCreate = overrides.onCreate ?? vi.fn();
  render(
    <I18nProvider initialLocale="zh">
      <NewProjectSheet
        open
        mode={overrides.mode}
        initial={overrides.initial}
        onClose={onClose}
        onCreate={onCreate}
        onSave={overrides.onSave}
        onRemove={overrides.onRemove}
      />
    </I18nProvider>,
  );
  return { onClose, onCreate };
}

describe("NewProjectSheet", () => {
  beforeEach(() => {
    openMock.mockReset();
  });

  it("按原型顺序渲染名称、位置、八个 emoji 和 CTA", () => {
    renderSheet();

    expect(
      screen.getByRole("dialog", { name: "新建项目" }),
    ).toBeInTheDocument();
    expect(screen.getByLabelText("名称")).toHaveValue("");
    expect(screen.getByRole("radio", { name: /新建文件夹/ })).toHaveAttribute(
      "aria-checked",
      "true",
    );
    expect(
      screen.getByRole("radio", { name: /选择已有文件夹/ }),
    ).toBeInTheDocument();
    expect(screen.getAllByRole("radio", { name: /项目标识/ })).toHaveLength(8);
    expect(screen.getByRole("button", { name: "取消" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "创建项目" })).toBeDisabled();
  });

  it("名称输入受控并更新默认创建路径预览", async () => {
    const user = userEvent.setup();
    renderSheet();

    const input = screen.getByLabelText("名称");
    await user.type(input, "我的小说");

    expect(input).toHaveValue("我的小说");
    expect(screen.getByText("将创建 ~/AgentLoom/我的小说")).toBeInTheDocument();
  });

  it("默认位置创建时提交默认目录、空路径和首个 emoji", async () => {
    const user = userEvent.setup();
    const onCreate = vi.fn().mockResolvedValue(undefined);
    const { onClose } = renderSheet({ onCreate });

    await user.type(screen.getByLabelText("名称"), "  新项目  ");
    await user.click(screen.getByRole("button", { name: "创建项目" }));

    await waitFor(() =>
      expect(onCreate).toHaveBeenCalledWith({
        name: "新项目",
        newUnderDefault: true,
        existingPath: null,
        icon: "📕",
      }),
    );
    expect(onClose).toHaveBeenCalledOnce();
  });

  it("选择已有文件夹和 emoji 后提交对应组合", async () => {
    const user = userEvent.setup();
    const onCreate = vi.fn().mockResolvedValue(undefined);
    openMock.mockResolvedValue("/Users/me/existing-project");
    renderSheet({ onCreate });

    await user.type(screen.getByLabelText("名称"), "已有项目");
    await user.click(screen.getByRole("radio", { name: /选择已有文件夹/ }));
    expect(openMock).toHaveBeenCalledWith({ directory: true, multiple: false });
    expect(screen.getByText("/Users/me/existing-project")).toBeInTheDocument();
    await user.click(screen.getByRole("radio", { name: "项目标识 📊" }));
    await user.click(screen.getByRole("button", { name: "创建项目" }));

    await waitFor(() =>
      expect(onCreate).toHaveBeenCalledWith({
        name: "已有项目",
        newUnderDefault: false,
        existingPath: "/Users/me/existing-project",
        icon: "📊",
      }),
    );
  });

  it("名称仅空白时阻止创建，取消和 Esc 均关闭", async () => {
    const user = userEvent.setup();
    const { onClose, onCreate } = renderSheet();

    await user.type(screen.getByLabelText("名称"), "   ");
    expect(screen.getByRole("button", { name: "创建项目" })).toBeDisabled();
    expect(onCreate).not.toHaveBeenCalled();

    await user.click(screen.getByRole("button", { name: "取消" }));
    expect(onClose).toHaveBeenCalledOnce();
    fireEvent.keyDown(screen.getByRole("dialog"), { key: "Escape" });
    expect(onClose).toHaveBeenCalledTimes(2);
  });

  it("编辑模式预填名称和 emoji，隐藏位置并保存", async () => {
    const user = userEvent.setup();
    const onSave = vi.fn().mockResolvedValue(undefined);
    renderSheet({
      mode: "edit",
      initial: { name: "我的小说", icon: "🎨" },
      onSave,
    });

    expect(
      screen.getByRole("dialog", { name: "编辑项目" }),
    ).toBeInTheDocument();
    expect(screen.getByLabelText("名称")).toHaveValue("我的小说");
    expect(screen.getByRole("radio", { name: "项目标识 🎨" })).toBeChecked();
    expect(screen.queryByText("位置")).not.toBeInTheDocument();

    await user.clear(screen.getByLabelText("名称"));
    await user.type(screen.getByLabelText("名称"), "新名字");
    await user.click(screen.getByRole("radio", { name: "项目标识 🚀" }));
    await user.click(screen.getByRole("button", { name: "保存" }));

    await waitFor(() =>
      expect(onSave).toHaveBeenCalledWith({ name: "新名字", icon: "🚀" }),
    );
  });

  it("编辑模式点击移除项目触发 onRemove", async () => {
    const user = userEvent.setup();
    const onRemove = vi.fn();
    renderSheet({
      mode: "edit",
      initial: { name: "旧项目", icon: null },
      onSave: vi.fn(),
      onRemove,
    });

    const removeButton = screen.getByRole("button", { name: "移除项目" });
    expect(removeButton).toBeInTheDocument();
    await user.click(removeButton);
    expect(onRemove).toHaveBeenCalledOnce();
  });

  it("编辑模式未传 onRemove 时不显示移除项目按钮", () => {
    renderSheet({
      mode: "edit",
      initial: { name: "我的项目", icon: "📕" },
      onSave: vi.fn(),
    });

    expect(
      screen.queryByRole("button", { name: "移除项目" }),
    ).not.toBeInTheDocument();
  });
});
