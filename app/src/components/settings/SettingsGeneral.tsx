import { useI18n } from "../../i18n";
import {
  MIN_SESSION_LIFECYCLE_DAYS,
  setSessionLifecyclePolicy,
  useSessionLifecyclePolicy,
} from "../../lib/sessionLifecycle";
import { getSessionLifecycleCopy } from "../../lib/sessionLifecycleCopy";

export function SettingsGeneral() {
  const { locale } = useI18n();
  const copy = getSessionLifecycleCopy(locale);
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

  return (
    <div className="st-lang">
      <div className="st-lang__head">
        <h2>{copy.title}</h2>
        <p>{copy.subtitle}</p>
      </div>
      <div className="st-lang__field">
        <div className="st-lang__label">{copy.groupLabel}</div>
        <div className="st-chat__group">
          <label className="st-chat__option st-chat__option--active">
            <span className="st-chat__text">
              <span className="st-chat__title">{copy.archiveLabel}</span>
              <span className="st-chat__desc">{copy.archiveDesc}</span>
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
                aria-label={copy.archiveLabel}
                style={{ width: 72 }}
              />{" "}
              {copy.days}
            </span>
          </label>
          <label className="st-chat__option st-chat__option--active">
            <span className="st-chat__text">
              <span className="st-chat__title">{copy.purgeLabel}</span>
              <span className="st-chat__desc">{copy.purgeDesc}</span>
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
                aria-label={copy.purgeLabel}
                style={{ width: 72 }}
              />{" "}
              {copy.days}
            </span>
          </label>
          <div className="st-chat__desc">{copy.minHint}</div>
        </div>
      </div>
    </div>
  );
}
