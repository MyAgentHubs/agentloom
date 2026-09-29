// SettingsScreen.tsx — msgfix2 U3 · 手机端设置屏（首个设置项：显示详细活动 / verbose 开关）。
// msgfix2 U4 在同屏第二行扩展了缓存开关（「本设备缓存已加载的长消息全文」/ body cache）。
//
// **纯 props 组件**（同 `SessionListScreen.tsx`/`SessionStreamScreen.tsx` 的既有取向）——
// localStorage 读写、偏好持久化都在调用方（`app/AppRuntime.tsx` + `verbosePreference.ts`/
// `cachePreference.ts`），本文件不碰 `window.localStorage`。
//
// **结构复用 U3 留的扩展点**（brief 原文："结构上让 U4 可在同屏扩展缓存开关"）：设置项是一个简单的
// `<ul>` 列表 + 每项一行 `SettingRow`（label + toggle），本单只在这个列表里再加一个
// `<SettingRow>` 元素，不改动既有结构。

import { useI18n } from "../i18n.ts";
import "./SettingsScreen.css";

export interface SettingsScreenProps {
  verboseEnabled: boolean;
  onToggleVerbose: (next: boolean) => void;
  /** msgfix2 U4：缓存开关当前值——关闭 = body cache 不再写入 + 立即清空已缓存内容
   *  （`app/AppRuntime.tsx::toggleCacheEnabled`）。 */
  cacheEnabled: boolean;
  onToggleCache: (next: boolean) => void;
  /**
   * msgfix2 U4 修单 H2：规格四触发点之②"显式解除配对"——修单前这个入口在整个前端完全不存在
   * （`store/cacheManager.ts` 头注早就写了"同 repair_failed 页面'重试'走的是同一条
   * `attemptRepairClear` 路径"，但从未真的有 UI 挂到这条路径上）。省略时不渲染这一行（同
   * `onBack` 的既有降级取向，供尚未接好这条依赖的调用点/测试沿用旧行为）。
   */
  onUnpair?: () => void;
  /** 省略时不渲染返回键（同 `SessionStreamScreen.tsx::onBack` 的既有降级取向）。 */
  onBack?: () => void;
}

export function SettingsScreen({ verboseEnabled, onToggleVerbose, cacheEnabled, onToggleCache, onUnpair, onBack }: SettingsScreenProps) {
  const { t } = useI18n();
  return (
    <div className="settings-screen" data-testid="settings-screen">
      <header className="settings-screen__head">
        {onBack && (
          <button
            type="button"
            className="app-runtime-back"
            data-testid="settings-back"
            aria-label={t("settings.back")}
            onClick={onBack}
          >
            {"‹"}
          </button>
        )}
        <span className="settings-screen__title">{t("settings.title")}</span>
      </header>
      <ul className="settings-list" data-testid="settings-list">
        <SettingRow
          testId="settings-verbose-toggle"
          label={t("settings.verbose.label")}
          hint={t("settings.verbose.hint")}
          checked={verboseEnabled}
          onChange={onToggleVerbose}
        />
        <SettingRow
          testId="settings-cache-toggle"
          label={t("settings.cache.label")}
          hint={t("settings.cache.hint")}
          checked={cacheEnabled}
          onChange={onToggleCache}
        />
        {onUnpair && (
          <li className="settings-row">
            <button type="button" className="settings-action settings-action--destructive" data-testid="settings-unpair-button" onClick={onUnpair}>
              <span className="settings-row__body">
                <span className="settings-row__label">{t("settings.unpair.label")}</span>
                <span className="settings-row__hint">{t("settings.unpair.hint")}</span>
              </span>
            </button>
          </li>
        )}
      </ul>
    </div>
  );
}

/** 一个"标签 + 开关"设置行——`role="switch"`/`aria-checked` 走原生可及性语义,不依赖真正的
 *  `<input type="checkbox">`（本仓其它交互控件也是这个取向,同 `SessionStreamScreen.tsx` 的
 *  按钮惯例,不额外引入表单控件的默认样式需要覆盖）。 */
function SettingRow({
  testId,
  label,
  hint,
  checked,
  onChange,
}: {
  testId: string;
  label: string;
  hint?: string;
  checked: boolean;
  onChange: (next: boolean) => void;
}) {
  return (
    <li className="settings-row">
      <button
        type="button"
        className={`settings-toggle${checked ? " settings-toggle--on" : ""}`}
        data-testid={testId}
        role="switch"
        aria-checked={checked}
        onClick={() => onChange(!checked)}
      >
        <span className="settings-row__body">
          <span className="settings-row__label">{label}</span>
          {hint && <span className="settings-row__hint">{hint}</span>}
        </span>
        <span className="settings-toggle__track" aria-hidden="true">
          <span className="settings-toggle__thumb" />
        </span>
      </button>
    </li>
  );
}
