import { fireEvent, render, screen } from "@testing-library/react";
import { beforeEach, describe, expect, it } from "vitest";
import { I18nProvider } from "../../i18n";
import {
  __resetSessionLifecycleForTests,
  getSessionLifecyclePolicy,
} from "../../lib/sessionLifecycle";
import { SettingsGeneral } from "./SettingsGeneral";

function renderGeneral(locale: "zh" | "en" = "zh") {
  return render(
    <I18nProvider initialLocale={locale}>
      <SettingsGeneral />
    </I18nProvider>,
  );
}

describe("SettingsGeneral", () => {
  beforeEach(() => {
    localStorage.clear();
    __resetSessionLifecycleForTests();
  });

  it("defaults lifecycle automation off with the 3/60 day policy", () => {
    renderGeneral();

    expect(
      screen.getByRole("switch", { name: "启用自动会话生命周期" }),
    ).not.toBeChecked();

    const archive = screen.getByRole("spinbutton", {
      name: "自动归档未活动会话",
    });
    const purge = screen.getByRole("spinbutton", {
      name: "永久删除已归档会话",
    });

    expect(archive).toHaveValue(3);
    expect(purge).toHaveValue(60);
    expect(archive).toHaveAttribute("min", "1");
    expect(purge).toHaveAttribute("min", "1");
  });

  it("persists the master switch and edited thresholds", () => {
    renderGeneral();

    fireEvent.click(
      screen.getByRole("switch", { name: "启用自动会话生命周期" }),
    );
    fireEvent.change(
      screen.getByRole("spinbutton", { name: "自动归档未活动会话" }),
      { target: { value: "7" } },
    );
    fireEvent.change(
      screen.getByRole("spinbutton", { name: "永久删除已归档会话" }),
      { target: { value: "90" } },
    );

    expect(getSessionLifecyclePolicy()).toEqual({
      enabled: true,
      archiveAfterDays: 7,
      deleteArchivedAfterDays: 90,
    });
  });

  it("renders English copy when English is active", () => {
    renderGeneral("en");

    expect(
      screen.getByRole("heading", { name: "General" }),
    ).toBeInTheDocument();
    expect(
      screen.getByRole("switch", {
        name: "Enable automatic session lifecycle",
      }),
    ).not.toBeChecked();
    expect(
      screen.getByRole("spinbutton", {
        name: "Auto-archive inactive sessions",
      }),
    ).toHaveValue(3);
    expect(
      screen.getByRole("spinbutton", {
        name: "Permanently delete archived sessions",
      }),
    ).toHaveValue(60);
  });
});
