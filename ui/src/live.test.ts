import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { pacer } from "./live";

describe("pacing what a socket says", () => {
  beforeEach(() => {
    vi.useFakeTimers();
  });
  afterEach(() => {
    vi.useRealTimers();
  });

  it("goes at once, then keeps the last of a burst for later", () => {
    const run = vi.fn();
    const p = pacer(3000, run);
    p.hit();
    expect(run).toHaveBeenCalledTimes(1);
    p.hit();
    p.hit();
    p.hit();
    expect(run).toHaveBeenCalledTimes(1);
    vi.advanceTimersByTime(3000);
    expect(run).toHaveBeenCalledTimes(2);
    vi.advanceTimersByTime(30000);
    expect(run).toHaveBeenCalledTimes(2);
  });

  it("goes at once again after a quiet spell, and says nothing once stopped", () => {
    const run = vi.fn();
    const p = pacer(3000, run);
    p.hit();
    vi.advanceTimersByTime(5000);
    p.hit();
    expect(run).toHaveBeenCalledTimes(2);
    p.hit();
    p.stop();
    vi.advanceTimersByTime(30000);
    expect(run).toHaveBeenCalledTimes(2);
  });
});
