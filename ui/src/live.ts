// Pages that show readings ask again as soon as a gateway's batch lands, rather than at their next
// poll. With many gateways that is many batches, so asks are paced: at most one in a few seconds,
// with the last always kept, so that the page ends up with what arrived. Polling stays underneath,
// slower, for a socket that is down.

import { useQueryClient } from "@tanstack/react-query";
import { useCallback, useEffect, useRef } from "react";
import { useLiveEntity, type LiveEvent } from "@ferrum/kit";

const GAP_MS = 3_000;
/** What moves when a reading lands. */
const SHOWS_READINGS = ["board-data", "device", "devices", "overview", "history", "fleet"];
export const SLOW_POLL_MS = 30_000;

/** Calls `run` at once, and after that at most every `gap` milliseconds, last call kept. */
export function pacer(gap: number, run: () => void, now: () => number = Date.now) {
  let last = Number.NEGATIVE_INFINITY;
  let waiting: ReturnType<typeof setTimeout> | undefined;
  const go = () => {
    waiting = undefined;
    last = now();
    run();
  };
  return {
    hit() {
      if (waiting !== undefined) return;
      const left = last + gap - now();
      if (left <= 0) go();
      else waiting = setTimeout(go, left);
    },
    stop() {
      clearTimeout(waiting);
      waiting = undefined;
    },
  };
}

export function useFreshReadings() {
  const qc = useQueryClient();
  const pace = useRef<ReturnType<typeof pacer>>(null);
  pace.current ??= pacer(GAP_MS, () => {
    for (const key of SHOWS_READINGS) void qc.invalidateQueries({ queryKey: ["edge", key] });
  });
  useEffect(() => () => pace.current?.stop(), []);
  const heard = useCallback((e: LiveEvent) => {
    if (e.event === "ops.measurements") pace.current?.hit();
  }, []);
  useLiveEntity("Site", heard);
}
