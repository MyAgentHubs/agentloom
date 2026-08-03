import { useDropdown } from "../hooks/useDropdown";
import { useI18n } from "../i18n";

export type Mode = "normal" | "team" | "round";

const SUMMON = [
  {
    id: "team",
    label: "Agent Team",
    descKey: "modeDropdown.team.description",
  },
  {
    id: "round",
    label: "Round Table",
    descKey: "modeDropdown.round.description",
  },
] as const;

type Props = {
  mode: Mode;
  onModeChange: (m: Mode) => void;
  disabled?: boolean;
};

function ModeIcon({ mode }: { mode: Mode }) {
  if (mode === "team") {
    return (
      <svg viewBox="0 0 24 24" aria-hidden="true">
        <circle cx="12" cy="6" r="3" />
        <circle cx="6" cy="18" r="3" />
        <circle cx="18" cy="18" r="3" />
        <path d="M10.4 8.6 7.7 15.2M13.6 8.6l2.7 6.6M9 18h6" />
      </svg>
    );
  }
  if (mode === "round") {
    return (
      <svg viewBox="0 0 24 24" aria-hidden="true">
        <circle cx="12" cy="12" r="8" />
        <path d="M12 4v16M4 12h16" />
      </svg>
    );
  }
  return (
    <svg viewBox="0 0 24 24" aria-hidden="true">
      <circle cx="12" cy="12" r="5" />
      <path d="M12 3v3M12 18v3M3 12h3M18 12h3" />
    </svg>
  );
}

export function ModeDropdown({ mode, onModeChange, disabled }: Props) {
  const { t } = useI18n();
  const dd = useDropdown();
  const label =
    mode === "normal"
      ? "Normal"
      : mode === "team"
        ? "Agent Team"
        : "Round Table";

  return (
    <div className="dd" ref={dd.containerRef}>
      <button
        type="button"
        className={`composer__mode composer__mode--${mode}`}
        aria-label={t("modeDropdown.select", { label })}
        title={t("modeDropdown.select", { label })}
        {...dd.triggerProps}
        disabled={disabled}
        onClick={dd.toggle}
      >
        <ModeIcon mode={mode} />
        <span className="composer__mode-dot" aria-hidden="true" />
      </button>
      {dd.open && (
        <div className="dd__menu" role="menu">
          <div className="dd__h">{t("modeDropdown.current")}</div>
          <button
            type="button"
            role="menuitemradio"
            aria-checked={mode === "normal"}
            className={`dd__item${mode === "normal" ? " dd__item--on" : ""}`}
            onClick={() => {
              onModeChange("normal");
              dd.close();
            }}
          >
            <span className="dd__item-label">
              {t("modeDropdown.normal.label")}
            </span>
            <span className="dd__item-desc">
              {t("modeDropdown.normal.description")}
            </span>
          </button>
          <div className="dd__div" />
          <div className="dd__h">{t("modeDropdown.collaboration")}</div>
          {SUMMON.map((m) =>
            m.id === "team" ? (
              <button
                key={m.id}
                type="button"
                role="menuitemradio"
                aria-checked={mode === "team"}
                className={`dd__item${mode === "team" ? " dd__item--on" : ""}`}
                onClick={() => {
                  onModeChange("team");
                  dd.close();
                }}
              >
                <span className="dd__item-label">{m.label}</span>
                <span className="dd__item-desc">{t(m.descKey)}</span>
              </button>
            ) : (
              <div
                key={m.id}
                role="menuitem"
                aria-disabled="true"
                className="dd__item dd__item--soon"
                title={t("modeDropdown.soonTitle", { label: m.label })}
              >
                <span className="dd__item-label">
                  {m.label}
                  <span className="dd__soon">{t("modeDropdown.soon")}</span>
                </span>
                <span className="dd__item-desc">{t(m.descKey)}</span>
              </div>
            ),
          )}
        </div>
      )}
    </div>
  );
}
