import {
  createContext,
  type ReactNode,
  useCallback,
  useContext,
  useEffect,
  useMemo,
  useState,
} from "react";
import { invoke } from "@tauri-apps/api/core";
import type { RepoMeta } from "./types/agent";
import { messages } from "./i18nMessages";

export { messages } from "./i18nMessages";

export type Locale = "zh" | "en";

const STORAGE_KEY = "agentloom.locale.v2";

export const localeOptions: {
  locale: Locale;
  label: string;
  native: string;
  short: string;
}[] = [
  { locale: "zh", label: "Chinese", native: "中文", short: "中" },
  { locale: "en", label: "English", native: "English", short: "EN" },
];

export type I18nKey = keyof typeof messages.zh;
export type TranslationKey = I18nKey;
export type TFn = (
  key: I18nKey,
  values?: Record<string, string | number>,
) => string;

type I18nContextValue = {
  locale: Locale;
  setLocale: (locale: Locale) => void;
  t: TFn;
};

function normalizeLocale(value: string | null | undefined): Locale | null {
  if (!value) return null;
  const lower = value.toLowerCase();
  if (lower.startsWith("zh")) return "zh";
  if (lower.startsWith("en")) return "en";
  return null;
}

function detectLocale(): Locale {
  try {
    const stored = normalizeLocale(window.localStorage?.getItem(STORAGE_KEY));
    if (stored) return stored;
  } catch {
    // localStorage can be unavailable in tests or hardened webviews.
  }

  try {
    const systemLanguage =
      typeof navigator === "undefined"
        ? undefined
        : navigator.language || navigator.languages?.[0];
    if (normalizeLocale(systemLanguage) === "zh") return "zh";
  } catch {
    // Navigator language APIs can be unavailable in hardened webviews.
  }

  // First launch follows the system language, with English as the safe fallback.
  return "en";
}

function translate(
  locale: Locale,
  key: I18nKey,
  values?: Record<string, string | number>,
): string {
  let template: string = messages[locale][key] ?? messages.zh[key] ?? key;
  if (!values) return template;
  for (const [name, value] of Object.entries(values)) {
    template = template.split(`{${name}}`).join(String(value));
  }
  return template;
}

const defaultValue: I18nContextValue = {
  locale: "zh",
  setLocale: () => {},
  t: (key, values) => translate("zh", key, values),
};

const I18nContext = createContext<I18nContextValue>(defaultValue);

export function I18nProvider({
  children,
  initialLocale,
}: {
  children: ReactNode;
  initialLocale?: Locale;
}) {
  const [locale, setLocaleState] = useState<Locale>(
    () => initialLocale ?? detectLocale(),
  );

  useEffect(() => {
    document.documentElement.lang = locale === "zh" ? "zh-CN" : "en";
    try {
      void invoke("set_ui_locale", { locale }).catch(() => {});
    } catch {
      // Keep initialization and switching functional outside the Tauri runtime.
    }
  }, [locale]);

  const setLocale = useCallback((next: Locale) => {
    setLocaleState(next);
    try {
      window.localStorage?.setItem(STORAGE_KEY, next);
    } catch {
      // Keep language switching functional even when persistence is unavailable.
    }
  }, []);

  const value = useMemo<I18nContextValue>(
    () => ({
      locale,
      setLocale,
      t: (key, values) => translate(locale, key, values),
    }),
    [locale, setLocale],
  );

  return <I18nContext.Provider value={value}>{children}</I18nContext.Provider>;
}

export function useI18n(): I18nContextValue {
  return useContext(I18nContext);
}

export const DEFAULT_LOCAL_PROJECT_ID = "local-default";
export const DEFAULT_LOCAL_PROJECT_NAME_SENTINEL = "我的项目";

export function localProjectDisplayName(
  repo: Pick<RepoMeta, "id" | "name">,
  t: I18nContextValue["t"],
): string {
  return repo.id === DEFAULT_LOCAL_PROJECT_ID &&
    repo.name === DEFAULT_LOCAL_PROJECT_NAME_SENTINEL
    ? t("projectSwitcher.myProject")
    : repo.name;
}
