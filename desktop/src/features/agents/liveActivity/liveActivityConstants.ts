/**
 * Live Activity timeline constants.
 *
 * Kept in a separate file so the mapping module, timeline component, and
 * latency instrumentation all reference the same named values without
 * importing React.
 */

/** Milliseconds without a frame before the timeline shows "stale." */
export const LIVE_ACTIVITY_STALE_THRESHOLD_MS = 10_000;

/** Default observer publish tick in milliseconds (R1 default). */
export const OBSERVER_PUBLISH_TICK_DEFAULT_MS = 500;

/** Ceiling for the observer publish tick (R1). */
export const OBSERVER_PUBLISH_TICK_CEILING_MS = 1000;

/** Maximum byte length of an excerpt before the "show more" affordance. */
export const EXCERPT_BYTE_CAP = 300;