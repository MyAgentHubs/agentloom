import type { CSSProperties } from "react";
import { useI18n } from "../../i18n";
import {
  MIN_SESSION_LIFECYCLE_DAYS,
  setSessionLifecyclePolicy,
  useSessionLifecyclePolicy,
} from "../../lib/sessionLifecycle";

const styles = {
  switchGroup: {
    alignItems: "center",
    display: "flex",
    flexShrink: 0,
    gap: 8,
  },
  switchStatus: {
    color: "var(--ink-2)",
    fontSize: 11,
    whiteSpace: "nowrap",
  },
  switch: {
    border: 0,
    borderRadius: 999,
    cursor: "pointer",
    height: 24,
    padding: 2,
    transition: "background 120ms ease",
    width: 42,
  },
  switchKnob: {
    background: "white",
    borderRadius: "50%",
    boxShadow: "0 1px 3px rgba(0, 0, 0, 0.3)",
    display: "block",
    height: 20,
    transition: "transform 120ms ease",
    width: 20,
  },
} satisfies Record<string, CSSProperties>;

export function SettingsGeneral() {
  const { t } = useI18n();
  const [policy] = useSessionLifecyclePolicy();

  const update = (
    key: "archiveAfterDays" | "deleteArchivedAfterDays",
    value: string,
  ) => {
    const parsed = Number(value);
    if (!Number.isFinite(parsed) || parsed < MIN_SESSION_LIFECYCLE_DAYS) return;
    setSessionLifecyclePolicy({
      ...policy,
      [key]: Math.floor(parsed),
    });
  };

  const toggleEnabled = () => {
    setSessionLifecyclePolicy({
      ...policy,
      enabled: !policy.enabled,
    });
  };

  return (
    <div className="st-lang">
      <div className="st-lang__head">
        <h2>{t("settings.general.title")}</h2>
        <p>{t("settings.general.subtitle")}</p>
      </div>
      <div className="st-lang__field">
        <div className="st-lang__label">{t("settings.general.groupLabel")}</div>
        <div className="st-chat__group">
          <div className="st-chat__option st-chat__option--active">
            <span className="st-chat__text">
              <span className="st-chat__title">
                {t("settings.general.enableLabel")}
              </span>
              <span className="st-chat__desc">
                {t("settings.general.enableDesc")}
              </span>
            </span>
            <span style={styles.switchGroup}>
              <span style={styles.switchStatus}>
                {t(
                  policy.enabled
                    ? "settings.general.enabled"
                    : "settings.general.disabled",
                )}
              </span>
              <button
                type="button"
                role="switch"
                aria-checked={policy.enabled}
                aria-label={t("settings.general.enableLabel")}
                style={{
                  ...styles.switch,
                  background: policy.enabled ? "var(--green)" : "var(--ink-4)",
                }}
                onClick={toggleEnabled}
              >
                <span
                  aria-hidden="true"
                  style={{
                    ...styles.switchKnob,
                    transform: policy.enabled ? "translateX(18px)" : "translateX(0)",
                  }}
                />
              </button>
            </span>
          </div>

          <label className="st-chat__option st-chat__option--active">
            <span className="st-chat__text">
              <span className="st-chat__title">
                {t("settings.general.archiveLabel")}
              </span>
              <span className="st-chat__desc">
                {t("settings.general.archiveDesc")}
              </span>
            </span>
            <span>
              <input
                type="number"
                min={MIN_SESSION_LIFECYCLE_DAYS}
                step={1}
                value={policy.archiveAfterDays}
                onChange={(event) =>
                  update("archiveAfterDays", event.target.value)
                }
                aria-label={t("settings.general.archiveLabel")}
                style={{ width: 72 }}
              />{" "}
              {t("settings.general.days")}
            </span>
          </label>

          <label className="st-chat__option st-chat__option--active">
            <span className="st-chat__text">
              <span className="st-chat__title">
                {t("settings.general.purgeLabel")}
              </span>
              <span className="st-chat__desc">
                {t("settings.general.purgeDesc")}
              </span>
            </span>
            <span>
              <input
                type="number"
                min={MIN_SESSION_LIFECYCLE_DAYS}
                step={1}
                value={policy.deleteArchivedAfterDays}
                onChange={(event) =>
                  update("deleteArchivedAfterDays", event.target.value)
                }
                aria-label={t("settings.general.purgeLabel")}
                style={{ width: 72 }}
              />{" "}
              {t("settings.general.days")}
            </span>
          </label>

          <div className="st-chat__desc">{t("settings.general.minHint")}</div>
          <div className="st-chat__desc">
            {t("settings.general.protectionHint")}
          </div>
        </div>
      </div>
    </div>
  );
}
