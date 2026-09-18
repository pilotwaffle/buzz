import * as React from "react";
import { ChevronDown, ChevronRight } from "lucide-react";

import type { DelegationSummaryData } from "@/features/delegations/lib/groupDelegationMessages";
import type { DelegationDisplayState } from "@/shared/api/delegationTypes";
import { Badge, type BadgeProps } from "@/shared/ui/badge";
import { Button } from "@/shared/ui/button";

const STATE_LABEL: Record<DelegationDisplayState, string> = {
  approved: "Approved",
  running: "Running",
  delivered: "Delivered",
  failed: "Failed",
  expired: "Expired",
};

const STATE_VARIANT: Record<DelegationDisplayState, BadgeProps["variant"]> = {
  approved: "info",
  running: "info",
  delivered: "success",
  failed: "destructive",
  expired: "warning",
};

export type DelegationSummaryCardProps = {
  summary: DelegationSummaryData;
};

export function DelegationSummaryCard({ summary }: DelegationSummaryCardProps) {
  const [expanded, setExpanded] = React.useState(false);

  return (
    <div
      className="rounded-lg border border-border/70 bg-muted/40 p-3"
      data-delegation-summary=""
      data-delegation-id={summary.delegationId}
    >
      <div className="flex items-center justify-between gap-2">
        <div className="flex items-center gap-2">
          <span className="text-sm font-medium">Delegation</span>
          <Badge variant={STATE_VARIANT[summary.state]}>
            {STATE_LABEL[summary.state]}
          </Badge>
          {summary.tokensUsed != null && (
            <span className="text-xs text-muted-foreground">
              {summary.tokensUsed.toLocaleString()} tokens used
            </span>
          )}
        </div>
        <Button
          aria-expanded={expanded}
          onClick={() => setExpanded((value) => !value)}
          size="sm"
          type="button"
          variant="ghost"
        >
          {expanded ? (
            <ChevronDown className="h-3.5 w-3.5" />
          ) : (
            <ChevronRight className="h-3.5 w-3.5" />
          )}
          {expanded ? "Hide notices" : "Show notices"}
        </Button>
      </div>
      {expanded && (
        <ul className="mt-2 space-y-1 border-t border-border/50 pt-2 text-xs text-muted-foreground">
          {summary.rawMessages.map((message) => (
            <li key={message.id}>
              <span className="font-medium">{message.time}</span> —{" "}
              {message.body || "(no content)"}
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}
