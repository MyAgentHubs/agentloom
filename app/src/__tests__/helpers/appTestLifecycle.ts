import { vi, beforeEach, afterEach } from "vitest";
import { clearTeamConfigCache } from "../../lib/useTeamConfig";
import { setChatVerbosity } from "../../lib/chatVerbosity";
import { createAppTestFixtures } from "./appTestFixtures";
import { createAppTestMocks } from "./appTestMocks";
import { createAppTestInteractions } from "./appTestInteractions";
import { createAppTestReviewScenarios } from "./appTestReviewScenarios";

// Each suite supplies its own hoisted mocks and registers the original hooks.
export function setupAppTests({
  invokeMock,
  listenMock,
  openMock,
  sessionMainProps,
}: {
  invokeMock: ReturnType<typeof vi.fn>;
  listenMock: ReturnType<typeof vi.fn>;
  openMock: ReturnType<typeof vi.fn>;
  sessionMainProps: unknown[];
}) {
  beforeEach(() => {
    localStorage.clear();
    // V3b：这份文件里的既有用例都写在「过程细节全量可见」的心智模型下（早于
    // verbosity 概念）——默认档实际是「摘要」（V2 决策点 1），会把工具/思考块折算成
    // chip 改变既有断言。这里重置回 full，让既有断言继续验证原本要验证的东西。
    setChatVerbosity("full");
    invokeMock.mockReset();
    listenMock.mockReset();
    openMock.mockReset();
    sessionMainProps.length = 0;
    clearTeamConfigCache();
    listenMock.mockResolvedValue(() => {});
  });

  afterEach(() => {
    vi.restoreAllMocks();
    vi.unstubAllGlobals();
    vi.useRealTimers();
  });

  const fixtures = createAppTestFixtures();
  const mocks = createAppTestMocks(invokeMock, fixtures);
  const interactions = createAppTestInteractions(listenMock, fixtures);
  const reviewScenarios = createAppTestReviewScenarios(
    invokeMock,
    fixtures,
    mocks,
    interactions,
  );
  const {
    localNamespace,
    localRepo,
    githubNamespace,
    githubRepo,
    emptyReview,
    agentProfile,
    agentProfiles,
    LAST_AGENT_ID_KEY,
    runCard,
    appMember,
    reviewWithChanges,
    escapeRegExp,
    dEnv,
    decisionCardBlock,
    decisionCardMessage,
    deferred,
    askCardPayload,
    orchestratedTeamMessages,
    workerTerminalEvent,
  } = fixtures;
  const {
    mockAppWithReview,
    keepPreviewLoading,
    mockBasicApp,
    mockRemovableProjectApp,
    sessionReviewCallCount,
    setupRunningS1,
    mockAppWith,
  } = mocks;
  const {
    openReviewPanel,
    configureTeamLead,
    agentEventCb,
    agentEventBatchCb,
    emitAgentEventBatch,
    leadDecisionCardCb,
    leadMessageAppendedCb,
    decisionCardResolvedCb,
    inlineDecisionCard,
    findInlineDecisionButton,
    clickDecisionOption,
  } = interactions;
  const {
    startRunCloseoutLiveUi,
    startRunCloseoutReviewRace,
    startSameSessionReviewRace,
    startStaleOpenReviewRace,
  } = reviewScenarios;

  return {
    localNamespace,
    localRepo,
    githubNamespace,
    githubRepo,
    emptyReview,
    agentProfile,
    agentProfiles,
    LAST_AGENT_ID_KEY,
    runCard,
    appMember,
    reviewWithChanges,
    mockAppWithReview,
    openReviewPanel,
    keepPreviewLoading,
    mockBasicApp,
    mockRemovableProjectApp,
    escapeRegExp,
    configureTeamLead,
    agentEventCb,
    agentEventBatchCb,
    emitAgentEventBatch,
    sessionReviewCallCount,
    startRunCloseoutLiveUi,
    startRunCloseoutReviewRace,
    startSameSessionReviewRace,
    startStaleOpenReviewRace,
    leadDecisionCardCb,
    leadMessageAppendedCb,
    decisionCardResolvedCb,
    dEnv,
    decisionCardBlock,
    decisionCardMessage,
    inlineDecisionCard,
    findInlineDecisionButton,
    clickDecisionOption,
    deferred,
    askCardPayload,
    setupRunningS1,
    mockAppWith,
    orchestratedTeamMessages,
    workerTerminalEvent,
  };
}
