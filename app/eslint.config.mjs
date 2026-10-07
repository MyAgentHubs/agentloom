// ESLint flat config for app/src.
// Scope: this gate only enforces max-lines-per-function (150 lines). It does
// not pull in any recommended rule set on purpose.
// react-hooks plugin is intentionally not installed; the two former
// exhaustive-deps sites are ReviewPanel / UndoReviewPanel effects,
// re-evaluate them if the plugin is ever adopted.
import tseslint from "typescript-eslint";
import globals from "globals";

// Legacy files that already exceed the 150-line function limit. This list is
// a ratchet: it may only shrink (a file drops off once its long functions are
// split), never grow with newly-written violations. See
// src/eslintLegacyRatchet.test.ts for the enforcement of that invariant.
export const LEGACY_LONG_FUNCTION_FILES = [
  "src/App.tsx",
  "src/__tests__/helpers/appTestFixtures.ts",
  "src/__tests__/helpers/appTestMocks.ts",
  "src/__tests__/helpers/appTestReviewScenarios.tsx",
  "src/components/AboutDialog.tsx",
  "src/components/ComposerAgentSelector.tsx",
  "src/components/ContinuationBriefPanel.tsx",
  "src/components/FilesPanel.tsx",
  "src/components/GateCard.tsx",
  "src/components/GlobalSearch.tsx",
  "src/components/InputArea.tsx",
  "src/components/MemberDrillIn.tsx",
  "src/components/MessageStream.tsx",
  "src/components/NewProjectSheet.tsx",
  "src/components/OverviewHome.tsx",
  "src/components/RepoList.tsx",
  "src/components/RepoManagePanel.tsx",
  "src/components/RepoSwitcherDropdown.tsx",
  "src/components/RightPanel.tsx",
  "src/components/RunCard.tsx",
  "src/components/SessionMain.tsx",
  "src/components/SessionMenu.tsx",
  "src/components/SessionRow.tsx",
  "src/components/Sidebar.tsx",
  "src/components/SurfaceHeader.tsx",
  "src/components/UndoReviewPanel.tsx",
  "src/components/UpdateButton.tsx",
  "src/components/settings/AgentForm.tsx",
  "src/components/settings/SettingsAgents.tsx",
  "src/components/settings/SettingsSearch.tsx",
  "src/components/settings/UpdateSection.tsx",
  "src/lib/useTeamConfig.ts",
];

export default tseslint.config(
  {
    ignores: ["dist", "node_modules", "src-tauri"],
  },
  {
    files: ["src/**/*.{ts,tsx,js,jsx,mts}"],
    ignores: [
      "src/**/*.test.{ts,tsx,js,jsx,mts}",
      "src/**/*.spec.{ts,tsx,js,jsx,mts}",
    ],
    languageOptions: {
      parser: tseslint.parser,
      globals: { ...globals.browser, ...globals.node },
    },
    plugins: {
      "@typescript-eslint": tseslint.plugin,
    },
    // Inline `/* eslint ... */` and `eslint-disable` comments could otherwise
    // turn max-lines-per-function off (or silence it) from inside a source
    // file, bypassing the legacy ratchet entirely. Ignoring all inline
    // config, and reporting every now-inert disable comment as a warning
    // (which --max-warnings 0 turns red), closes that hole.
    linterOptions: {
      noInlineConfig: true,
      reportUnusedDisableDirectives: "error",
    },
    rules: {
      "max-lines-per-function": [
        "error",
        { max: 150, skipBlankLines: true, skipComments: true, IIFEs: true },
      ],
    },
  },
  ...(LEGACY_LONG_FUNCTION_FILES.length > 0
    ? [
        {
          files: LEGACY_LONG_FUNCTION_FILES,
          rules: {
            "max-lines-per-function": "off",
          },
        },
      ]
    : []),
);
