export type ModelMapping = {
  primary?: string;
  opus?: string;
  sonnet?: string;
  haiku?: string;
  subagent?: string;
};

type ParsedModelId = {
  id: string;
  version: number | null;
  hasLightSuffix: boolean;
  hasExcludedSuffix: boolean;
};

const LIGHT_SUFFIXES = ["air", "flash", "lite", "mini"];
const EXCLUDED_SUFFIXES = ["preview", "beta"];

function parseModelId(id: string): ParsedModelId {
  const normalized = id.toLowerCase();
  const versionMatches = Array.from(
    // Supports ids like glm-5, glm-4.7, kimi-k2, and kimi-k2.5.
    normalized.matchAll(/(?:^|[-_a-z])(\d+(?:\.\d+)*)(?=$|[-_a-z])/g),
  );
  const versionToken = versionMatches[versionMatches.length - 1]?.[1];
  const version = versionToken === undefined ? null : Number(versionToken);

  return {
    id,
    version: Number.isFinite(version) ? version : null,
    hasLightSuffix: LIGHT_SUFFIXES.some((suffix) =>
      normalized.includes(suffix),
    ),
    hasExcludedSuffix: EXCLUDED_SUFFIXES.some((suffix) =>
      normalized.includes(suffix),
    ),
  };
}

function pickHighestVersion(models: ParsedModelId[]): ParsedModelId {
  return models.reduce((best, candidate) => {
    if (candidate.version! > best.version!) return candidate;
    if (
      candidate.version === best.version &&
      candidate.id.length < best.id.length
    ) {
      return candidate;
    }
    return best;
  });
}

export function deriveModelMapping(modelIds: string[]): ModelMapping | null {
  if (modelIds.length === 0) return null;

  const versioned = modelIds
    .map(parseModelId)
    .filter((model): model is ParsedModelId & { version: number } => {
      return model.version !== null;
    });
  if (versioned.length === 0) return null;

  const mainlineCandidates = versioned.filter(
    (model) => !model.hasLightSuffix && !model.hasExcludedSuffix,
  );
  const flagshipCandidates =
    mainlineCandidates.length > 0
      ? mainlineCandidates
      : versioned.filter((model) => !model.hasExcludedSuffix);
  const flagship = pickHighestVersion(
    flagshipCandidates.length > 0 ? flagshipCandidates : versioned,
  );

  const sameVersionLight = versioned.filter(
    (model) => model.hasLightSuffix && model.version === flagship.version,
  );
  const lightCandidates =
    sameVersionLight.length > 0
      ? sameVersionLight
      : versioned.filter((model) => model.hasLightSuffix);
  const haiku =
    lightCandidates.length > 0
      ? pickHighestVersion(lightCandidates).id
      : flagship.id;

  return {
    primary: flagship.id,
    opus: flagship.id,
    sonnet: flagship.id,
    haiku,
    subagent: flagship.id,
  };
}
