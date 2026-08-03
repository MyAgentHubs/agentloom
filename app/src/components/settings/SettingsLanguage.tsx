import { localeOptions, useI18n, type Locale } from "../../i18n";

export function SettingsLanguage() {
  const { locale, setLocale, t } = useI18n();

  return (
    <div className="st-lang">
      <div className="st-lang__head">
        <h2>{t("settings.language.title")}</h2>
        <p>{t("settings.language.subtitle")}</p>
      </div>
      <div className="st-lang__field">
        <div className="st-lang__label">{t("settings.language.current")}</div>
        <div
          className="st-lang__seg"
          role="radiogroup"
          aria-label={t("settings.language.title")}
        >
          {localeOptions.map((option) => (
            <button
              key={option.locale}
              type="button"
              role="radio"
              aria-checked={locale === option.locale}
              className={`st-lang__option${locale === option.locale ? " active" : ""}`}
              onClick={() => setLocale(option.locale as Locale)}
            >
              <span>{option.native}</span>
              <small>{option.label}</small>
            </button>
          ))}
        </div>
      </div>
    </div>
  );
}
