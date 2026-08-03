import { describe, it, expect, vi } from "vitest";
import { render, screen, fireEvent, within } from "@testing-library/react";
import { ModelDropdown } from "./ModelDropdown";
import { CUSTOM_MODEL_SENTINEL } from "./agentFormHelpers";
import { I18nProvider } from "../../i18n";

describe("ModelDropdown", () => {
  const open = () =>
    fireEvent.click(screen.getByRole("button", { name: /m1/ }));
  it("渲染选项 + 选择回调", () => {
    const onChange = vi.fn();
    render(
      <ModelDropdown
        value="m1"
        options={["m1", "m2", CUSTOM_MODEL_SENTINEL]}
        liveModels={["m2"]}
        onChange={onChange}
        onSelectCustom={vi.fn()}
      />,
    );
    open();
    fireEvent.click(screen.getByRole("menuitemradio", { name: /m2/ }));
    expect(onChange).toHaveBeenCalledWith("m2");
    expect(screen.queryByRole("menu")).not.toBeInTheDocument();
  });
  it("实时模型标「实时」badge", () => {
    render(
      <ModelDropdown
        value="m1"
        options={["m1", "m2", CUSTOM_MODEL_SENTINEL]}
        liveModels={["m2"]}
        onChange={vi.fn()}
        onSelectCustom={vi.fn()}
      />,
    );
    open();
    const menu = screen.getByRole("menu");
    const m2item = within(menu).getByRole("menuitemradio", { name: /m2/ });
    expect(within(m2item).getByText("实时")).toBeInTheDocument();
    const m1item = within(menu).getByRole("menuitemradio", { name: /^m1$/ });
    expect(within(m1item).queryByText("实时")).toBeNull();
  });
  it("选自定义哨兵 → onSelectCustom", () => {
    const onChange = vi.fn();
    const onSelectCustom = vi.fn();
    render(
      <ModelDropdown
        value="m1"
        options={["m1", CUSTOM_MODEL_SENTINEL]}
        liveModels={[]}
        onChange={onChange}
        onSelectCustom={onSelectCustom}
      />,
    );
    open();
    fireEvent.click(screen.getByRole("menuitemradio", { name: /自定义/ }));
    expect(onSelectCustom).toHaveBeenCalled();
    expect(onChange).not.toHaveBeenCalled();
  });
  it("空 value 显示「选择模型」且未注入默认项", () => {
    render(
      <ModelDropdown
        value=""
        options={["m1", CUSTOM_MODEL_SENTINEL]}
        liveModels={[]}
        onChange={vi.fn()}
        onSelectCustom={vi.fn()}
      />,
    );

    expect(
      screen.getByRole("button", { name: /选择模型/ }),
    ).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: /选择模型/ }));
    expect(
      screen.queryByRole("menuitemradio", { name: /myagent 默认/ }),
    ).toBeNull();
  });
  it("渲染 defaultOption 并点击回调空字符串", () => {
    const onChange = vi.fn();
    render(
      <ModelDropdown
        value=""
        options={["m1", CUSTOM_MODEL_SENTINEL]}
        liveModels={[]}
        onChange={onChange}
        onSelectCustom={vi.fn()}
        defaultOption={{ value: "", label: "myagent 默认" }}
      />,
    );

    fireEvent.click(screen.getByRole("button", { name: /myagent 默认/ }));
    const defaultItem = screen.getByRole("menuitemradio", {
      name: /myagent 默认/,
    });
    expect(defaultItem).toHaveAttribute("aria-checked", "true");

    fireEvent.click(defaultItem);

    expect(onChange).toHaveBeenCalledWith("");
    expect(screen.queryByRole("menu")).not.toBeInTheDocument();
  });
  it("aria-checked 标出当前项", () => {
    render(
      <ModelDropdown
        value="m1"
        options={["m1", "m2", CUSTOM_MODEL_SENTINEL]}
        liveModels={[]}
        onChange={vi.fn()}
        onSelectCustom={vi.fn()}
      />,
    );
    open();

    const menu = screen.getByRole("menu");
    const m1item = within(menu).getByRole("menuitemradio", { name: /^m1$/ });
    const m2item = within(menu).getByRole("menuitemradio", { name: /^m2$/ });
    const customItem = within(menu).getByRole("menuitemradio", {
      name: /自定义/,
    });
    expect(m1item).toHaveAttribute("aria-checked", "true");
    expect(m1item).toHaveClass("dd__item--on");
    expect(m2item).toHaveAttribute("aria-checked", "false");
    expect(m2item).not.toHaveClass("dd__item--on");
    expect(customItem).toHaveAttribute("aria-checked", "false");
    expect(customItem).not.toHaveClass("dd__item--on");
  });
  it("disabled 透传到 trigger", () => {
    render(
      <ModelDropdown
        value="unique-disabled-model"
        options={["unique-disabled-model", CUSTOM_MODEL_SENTINEL]}
        liveModels={[]}
        onChange={vi.fn()}
        onSelectCustom={vi.fn()}
        disabled
      />,
    );

    expect(screen.getByRole("button")).toBeDisabled();
  });
  it("English locale 渲染英文文案且无残留中文", () => {
    render(
      <I18nProvider initialLocale="en">
        <ModelDropdown
          value=""
          options={["m1", "m2", CUSTOM_MODEL_SENTINEL]}
          liveModels={["m2"]}
          onChange={vi.fn()}
          onSelectCustom={vi.fn()}
        />
      </I18nProvider>,
    );

    // placeholder 走英文文案「Select a model」
    expect(
      screen.getByRole("button", { name: /Select a model/ }),
    ).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: /Select a model/ }));

    const menu = screen.getByRole("menu");
    const m2item = within(menu).getByRole("menuitemradio", { name: /m2/ });
    // 「实时」→「Live」
    expect(within(m2item).getByText("Live")).toBeInTheDocument();
    expect(within(m2item).queryByText("实时")).toBeNull();
    // 「自定义…」→「Custom…」
    within(menu).getByRole("menuitemradio", { name: /Custom/ });
    // 全菜单无任何残留中文
    expect(within(menu).queryByText(/选择模型/)).toBeNull();
    expect(within(menu).queryByText(/自定义/)).toBeNull();
  });
});
