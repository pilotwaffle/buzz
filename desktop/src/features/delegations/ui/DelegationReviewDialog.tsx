import * as React from "react";
import { toast } from "sonner";

import { s1GateEnabled, s1GateLog } from "@/features/agents/liveActivity/s1Log";
import { useManagedAgentsQuery } from "@/features/agents/hooks";
import { approveDelegation } from "@/shared/api/tauriDelegations";
import type { DelegationRequestV2 } from "@/shared/api/delegationTypes";
import { Button } from "@/shared/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/shared/ui/dialog";

/**
 * The exact warning text spec 6.3 requires when a delegation request carries
 * a cost cap — this release cannot enforce a cost cap, so any request naming
 * one is refused at claim time (`cost_unknown`) rather than run unmetered.
 */
export const DELEGATION_COST_CAP_WARNING =
  "cost cap set: this delegation cannot run in this release";

export type DelegationReviewDialogProps = {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  /** The raw `buzz-delegation` fenced-block JSON, unparsed. */
  requestJson: string;
  /** The id of the message containing the fenced block (D-4). */
  originEventId?: string;
  /**
   * Injectable seam for `approveDelegation` — defaults to the real Tauri
   * call. Tests supply a stub here instead of mocking the IPC layer.
   */
  approveDelegationFn?: typeof approveDelegation;
};

function agentLabel(
  pubkey: string,
  agents: Array<{ pubkey: string; name: string }>,
): string {
  const match = agents.find((agent) => agent.pubkey === pubkey);
  return match?.name ?? pubkey;
}

export function DelegationReviewDialog({
  open,
  onOpenChange,
  requestJson,
  originEventId,
  approveDelegationFn = approveDelegation,
}: DelegationReviewDialogProps) {
  const [isApproving, setIsApproving] = React.useState(false);
  const managedAgentsQuery = useManagedAgentsQuery({ enabled: open });
  const managedAgents = managedAgentsQuery.data ?? [];

  const parsed = React.useMemo<
    { ok: true; request: DelegationRequestV2 } | { ok: false; error: string }
  >(() => {
    try {
      return { ok: true, request: JSON.parse(requestJson) };
    } catch (error) {
      return {
        ok: false,
        error: error instanceof Error ? error.message : String(error),
      };
    }
  }, [requestJson]);

  React.useEffect(() => {
    if (open && s1GateEnabled()) s1GateLog("[delegation] review-open");
  }, [open]);

  const handleApprove = React.useCallback(async () => {
    if (!originEventId) {
      toast.error("Cannot approve: missing originating message id");
      return;
    }
    setIsApproving(true);
    try {
      await approveDelegationFn(originEventId, requestJson);
      if (s1GateEnabled()) s1GateLog("[delegation] approved");
      toast.success("Delegation approved");
      onOpenChange(false);
    } catch (error) {
      toast.error(
        error instanceof Error ? error.message : "Failed to approve delegation",
      );
    } finally {
      setIsApproving(false);
    }
  }, [originEventId, requestJson, onOpenChange, approveDelegationFn]);

  return (
    <Dialog onOpenChange={onOpenChange} open={open}>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>Review delegation</DialogTitle>
          <DialogDescription>
            Approving this publishes a signed approval to the relay. The
            target agent begins work only after you approve.
          </DialogDescription>
        </DialogHeader>

        {!parsed.ok ? (
          <p className="text-sm text-destructive">
            This block failed to parse and cannot be approved: {parsed.error}
          </p>
        ) : (
          <div className="space-y-2 text-sm">
            <Row
              label="Source"
              value={agentLabel(parsed.request.source_agent, managedAgents)}
            />
            <Row
              label="Target"
              value={agentLabel(parsed.request.target_agent, managedAgents)}
            />
            <Row label="Turns" value={String(parsed.request.max_turns)} />
            <Row
              label="Token budget"
              value={parsed.request.token_budget.toLocaleString()}
            />
            <Row
              label="Expires"
              value={new Date(
                parsed.request.expires_at * 1000,
              ).toLocaleString()}
            />
            <Row label="Hop budget" value={String(parsed.request.hop_budget)} />
            {parsed.request.cost_cap_microusd != null && (
              <p
                className="rounded-md border border-amber-500/30 bg-amber-500/10 px-2 py-1 text-xs text-amber-700"
                role="alert"
              >
                {DELEGATION_COST_CAP_WARNING}
              </p>
            )}
          </div>
        )}

        <DialogFooter>
          <Button
            disabled={isApproving}
            onClick={() => onOpenChange(false)}
            type="button"
            variant="secondary"
          >
            Cancel
          </Button>
          <Button
            disabled={!parsed.ok || isApproving || !originEventId}
            onClick={handleApprove}
            type="button"
          >
            {isApproving ? "Approving…" : "Approve"}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}

function Row({ label, value }: { label: string; value: string }) {
  return (
    <div className="flex justify-between gap-4">
      <span className="text-muted-foreground">{label}</span>
      <span className="font-medium">{value}</span>
    </div>
  );
}
