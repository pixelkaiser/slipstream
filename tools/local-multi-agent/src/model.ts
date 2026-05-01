export const defaultModel = "Qwen/Qwen3.6-27B-FP8";

export const defaultModelAliases: Record<string, string> = {
  auto: defaultModel,
  "auto-efficient": defaultModel,
  "auto-coding": defaultModel,
  "auto-reasoning": defaultModel,
};

function nonEmpty(value: string | undefined): string | undefined {
  const trimmed = value?.trim();
  return trimmed ? trimmed : undefined;
}

export function configuredModelAliases(rawAliases: string | undefined): Record<string, string> {
  const aliasesJson = nonEmpty(rawAliases);
  if (!aliasesJson) {
    return {};
  }

  const aliases = JSON.parse(aliasesJson) as unknown;
  if (!aliases || typeof aliases !== "object" || Array.isArray(aliases)) {
    throw new Error("LOCAL_MODEL_ALIASES must be a JSON object mapping Warp model IDs to provider model IDs.");
  }

  return Object.fromEntries(
    Object.entries(aliases)
      .map(([key, value]) => [key, typeof value === "string" ? nonEmpty(value) : undefined] as const)
      .filter((entry): entry is [string, string] => entry[1] != null),
  );
}

export function resolveProviderModel(params: {
  openAiModel?: string;
  warpModel?: string;
  localModelAliases?: string;
}): string {
  const requestedModel = nonEmpty(params.warpModel);
  const modelAliases = {
    ...defaultModelAliases,
    ...configuredModelAliases(params.localModelAliases),
  };

  if (requestedModel && modelAliases[requestedModel]) {
    return modelAliases[requestedModel];
  }

  if (requestedModel && !requestedModel.startsWith("auto")) {
    return requestedModel;
  }

  const explicitModel = nonEmpty(params.openAiModel);
  if (explicitModel) {
    return explicitModel;
  }

  return defaultModel;
}
