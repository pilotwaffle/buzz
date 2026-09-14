import { stringify as yamlStringify } from "yaml";

import { useUpdateWorkflowMutation } from "@/features/workflows/hooks";
import type { RoutineState, Workflow } from "@/shared/api/types";
import { Button } from "@/shared/ui/button";
import { Skeleton } from "@/shared/ui/skeleton";
import { getWorkflowSteps, withWorkflowEnabled } from "./workflowDefinition";

const PAUSED_REASON_LABELS: Record<string, string> = {
  strikes: "strikes",
  daily_budget: "daily budget",
};

/**
 * Routine state (status, strikes, last fire/outcome, budgets) plus a
 * re-enable affordance when auto-paused. Shared by WorkflowDetailPanel's
 * Routine section and WorkflowCard's routine popover (S3-6, S3-8) so the two
 * surfaces can never disagree about what an auto-paused routine looks like.
 */
export function RoutineStatePanel({
  isLoading,
  state,
  workflow,
}: {
  isLoading: boolean;
  state: RoutineState | undefined;
  workflow: Workflow;
}) {
  const updateMutation = useUpdateWorkflowMutation(
    workflow.id,
    workflow.revision,
  );
  const routineStep = getWorkflowSteps(workflow.definition).find(
    (step) => step.action === "invoke_agent",
  );
  const isAutoPaused = state?.status === "disabled" && Boolean(state.pausedReason);

  function handleReenable() {
    const nextYaml = yamlStringify(
      withWorkflowEnabled(workflow.definition, true),
    );
    updateMutation.mutate(nextYaml);
  }

  if (isLoading || !state) {
    return <Skeleton className="h-24 w-full" data-testid="routine-state-panel-loading" />;
  }

  return (
    <div data-testid="routine-state-panel">
      <dl className="grid grid-cols-2 gap-x-4 gap-y-1 text-xs">
        <dt className="text-muted-foreground">Status</dt>
        <dd>
          {isAutoPaused
            ? `Auto-paused: ${PAUSED_REASON_LABELS[state.pausedReason as string] ?? state.pausedReason}`
            : state.status}
        </dd>
        <dt className="text-muted-foreground">Strikes</dt>
        <dd>{state.consecutiveFailures}</dd>
        <dt className="text-muted-foreground">Last fire</dt>
        <dd>{state.lastFiredAt ?? "Never"}</dd>
        <dt className="text-muted-foreground">Last outcome</dt>
        <dd>{state.lastOutcome ?? "—"}</dd>
        {routineStep ? (
          <>
            <dt className="text-muted-foreground">Tokens / run</dt>
            <dd>{String(routineStep.token_budget_per_run)}</dd>
            <dt className="text-muted-foreground">Tokens / day</dt>
            <dd>{String(routineStep.token_budget_per_day)}</dd>
          </>
        ) : null}
      </dl>
      {isAutoPaused ? (
        <Button
          className="mt-3"
          disabled={updateMutation.isPending}
          onClick={handleReenable}
          size="sm"
          type="button"
          variant="outline"
        >
          {updateMutation.isPending ? "Re-enabling…" : "Re-enable"}
        </Button>
      ) : null}
      {updateMutation.isError ? (
        <p className="mt-2 text-xs text-destructive" role="alert">
          Couldn't re-enable the routine. Try again.
        </p>
      ) : null}
    </div>
  );
}
