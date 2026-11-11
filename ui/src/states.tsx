// A failed request, told apart by what the platform answered: refused (403), no connection (status 0),
// anything else. Denied never says what exists.

import { ApiError } from "@ferrum/client";
import { StatePanel } from "@ferrum/kit";

export function FailedState({ error, onRetry }: { error: unknown; onRetry?: () => void }) {
  const status = error instanceof ApiError ? error.status : null;
  if (status === 403) return <StatePanel kind="denied" />;
  if (status === 0) return <StatePanel kind="offline" onRetry={onRetry} />;
  return <StatePanel kind="failed" error={error} onRetry={onRetry} />;
}
