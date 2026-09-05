/**
 * Slice-0 rollout identifiers for the Agent Computer capabilities.
 *
 * These values intentionally match the PRD wire/configuration names exactly.
 * They are present in the preview manifest with `defaultEnabled: false` and no
 * active platform, so declaring them cannot expose UI or emit events. A later
 * implementation slice must both wire its gate and opt it into a platform.
 */
export const AGENT_COMPUTER_FEATURE_FLAGS = [
  "BUZZ_LIVE_ACTIVITY",
  "BUZZ_AGENT_CONTROLS",
  "BUZZ_ROUTINES",
  "BUZZ_DELEGATION",
] as const;

/** A valid Agent Computer rollout identifier. */
export type AgentComputerFeatureFlag =
  (typeof AGENT_COMPUTER_FEATURE_FLAGS)[number];
