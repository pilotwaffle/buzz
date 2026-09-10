/**
 * Strict NIP-AO closed-object builders (Slice 2 structured controls).
 *
 * Every function returns a plain JS object whose JSON serialization matches the
 * exact field order and shape of the corresponding Rust struct in
 * `crates/buzz-core/src/agent_control.rs`. The `deny_unknown_fields` serde
 * attribute means any extra key, misspelled field, or wrong type is rejected by
 * the validator — these builders therefore construct the object one property at
 * a time in declaration order and never use spread.
 *
 * All timestamps are Unix seconds (u64). `issued_at` must equal the outer
 * kind-24200 event's `created_at`; the caller passes it as `issuedAt` here and
 * feeds the same value to `sendAgentObserverControl`'s optional `createdAt`
 * parameter.
 */

const FORMAT_COMMAND = "buzz-agent-control-command" as const;
const FORMAT_PAUSE_LEASE = "buzz-agent-pause-lease" as const;
const VERSION = 1 as const;

const DEFAULT_CANCEL_TTL_SECS = 30;
const DEFAULT_STEER_TTL_SECS = 60;
const DEFAULT_TRANSITION_TTL_SECS = 60;
const DEFAULT_PAUSE_LEASE_SECS = 300;

export interface ControlTarget {
  computer_id: string;
  agent_pubkey: string;
  channel_id: string;
  run_id: string;
}

export interface BuildOneShotInput {
  operatorPubkey: string;
  target: ControlTarget;
  seq: number;
  issuedAt: number;
  expiresAt?: number;
  steerMessageEventId?: string;
  commandId?: string;
}

export interface OneShotControlCommand {
  format: typeof FORMAT_COMMAND;
  version: typeof VERSION;
  command_id: string;
  control: "cancel" | "steer";
  operator_pubkey: string;
  target: ControlTarget;
  seq: number;
  issued_at: number;
  expires_at: number;
  steer_message_event_id?: string;
}

export interface BuildLeaseInput {
  operatorPubkey: string;
  target: ControlTarget;
  seq: number;
  issuedAt: number;
  transitionExpiresAt?: number;
  leaseExpiresAt?: number;
  leaseId?: string;
  generation?: number;
  transitionId?: string;
}

export interface PauseLeaseTransition {
  format: typeof FORMAT_PAUSE_LEASE;
  version: typeof VERSION;
  transition_id: string;
  lease_id: string;
  generation: number;
  transition: "pause" | "renew" | "resume";
  operator_pubkey: string;
  target: ControlTarget;
  seq: number;
  issued_at: number;
  transition_expires_at: number;
  lease_expires_at?: number;
}

// ── Helpers ──────────────────────────────────────────────────────────────────

function uuid(): string {
  return crypto.randomUUID();
}

function clampSeq(seq: number): number {
  const n = Math.trunc(seq);
  if (n < 1) return 1;
  if (n > Number.MAX_SAFE_INTEGER) return Number.MAX_SAFE_INTEGER;
  return n;
}

// ── One-shot builders ────────────────────────────────────────────────────────

/** Cancel the in-flight turn for the given target. */
export function buildCancelCommand(input: BuildOneShotInput): OneShotControlCommand {
  const issuedAt = Math.trunc(input.issuedAt);
  const cmd: OneShotControlCommand = {
    format: FORMAT_COMMAND,
    version: VERSION,
    command_id: input.commandId ?? uuid(),
    control: "cancel",
    operator_pubkey: input.operatorPubkey,
    target: input.target,
    seq: clampSeq(input.seq),
    issued_at: issuedAt,
    expires_at: input.expiresAt ?? issuedAt + DEFAULT_CANCEL_TTL_SECS,
  };
  return cmd;
}

/**
 * Steer a running agent with a durable operator message.
 *
 * `steerMessageEventId` is the Nostr event id of the already-published durable
 * operator message that the agent must resolve before claiming the command.
 */
export function buildSteerCommand(input: BuildOneShotInput): OneShotControlCommand {
  if (!input.steerMessageEventId) {
    throw new Error("steerMessageEventId is required for steer commands");
  }
  const issuedAt = Math.trunc(input.issuedAt);
  const cmd: OneShotControlCommand = {
    format: FORMAT_COMMAND,
    version: VERSION,
    command_id: input.commandId ?? uuid(),
    control: "steer",
    operator_pubkey: input.operatorPubkey,
    target: input.target,
    seq: clampSeq(input.seq),
    issued_at: issuedAt,
    expires_at: input.expiresAt ?? issuedAt + DEFAULT_STEER_TTL_SECS,
    steer_message_event_id: input.steerMessageEventId,
  };
  return cmd;
}

// ── Pause-lease builders ─────────────────────────────────────────────────────

/** Pause new queue dispatch for the target agent (generation 1). */
export function buildPauseTransition(input: BuildLeaseInput): PauseLeaseTransition {
  const issuedAt = Math.trunc(input.issuedAt);
  const leaseId = input.leaseId ?? uuid();
  const transition: PauseLeaseTransition = {
    format: FORMAT_PAUSE_LEASE,
    version: VERSION,
    transition_id: input.transitionId ?? uuid(),
    lease_id: leaseId,
    generation: 1,
    transition: "pause",
    operator_pubkey: input.operatorPubkey,
    target: input.target,
    seq: clampSeq(input.seq),
    issued_at: issuedAt,
    transition_expires_at:
      input.transitionExpiresAt ?? issuedAt + DEFAULT_TRANSITION_TTL_SECS,
    lease_expires_at:
      input.leaseExpiresAt ?? issuedAt + DEFAULT_PAUSE_LEASE_SECS,
  };
  return transition;
}

/** Extend an active pause lease by 300 s (generation > 1). */
export function buildRenewTransition(input: BuildLeaseInput): PauseLeaseTransition {
  if (!input.leaseId) {
    throw new Error("leaseId is required for renew transitions");
  }
  if (!input.generation || input.generation < 2) {
    throw new Error("generation must be >= 2 for renew transitions");
  }
  const issuedAt = Math.trunc(input.issuedAt);
  if (!input.leaseExpiresAt) {
    throw new Error("leaseExpiresAt is required for renew transitions");
  }
  const transition: PauseLeaseTransition = {
    format: FORMAT_PAUSE_LEASE,
    version: VERSION,
    transition_id: input.transitionId ?? uuid(),
    lease_id: input.leaseId,
    generation: input.generation,
    transition: "renew",
    operator_pubkey: input.operatorPubkey,
    target: input.target,
    seq: clampSeq(input.seq),
    issued_at: issuedAt,
    transition_expires_at:
      input.transitionExpiresAt ?? issuedAt + DEFAULT_TRANSITION_TTL_SECS,
    lease_expires_at: input.leaseExpiresAt,
  };
  return transition;
}

/** Release an active pause lease (generation > 1, no `lease_expires_at`). */
export function buildResumeTransition(input: BuildLeaseInput): PauseLeaseTransition {
  if (!input.leaseId) {
    throw new Error("leaseId is required for resume transitions");
  }
  if (!input.generation || input.generation < 2) {
    throw new Error("generation must be >= 2 for resume transitions");
  }
  const issuedAt = Math.trunc(input.issuedAt);
  const transition: PauseLeaseTransition = {
    format: FORMAT_PAUSE_LEASE,
    version: VERSION,
    transition_id: input.transitionId ?? uuid(),
    lease_id: input.leaseId,
    generation: input.generation,
    transition: "resume",
    operator_pubkey: input.operatorPubkey,
    target: input.target,
    seq: clampSeq(input.seq),
    issued_at: issuedAt,
    transition_expires_at:
      input.transitionExpiresAt ?? issuedAt + DEFAULT_TRANSITION_TTL_SECS,
  };
  // lease_expires_at is deliberately absent for resume (the validator rejects it)
  return transition;
}