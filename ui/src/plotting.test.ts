import { describe, expect, it } from "vitest";
import { setTimeZone } from "@ferrum/client";
import { csv, extent, linePath, nearest, summary, timeTicks, valueTicks } from "./plotting";

const p = (at: number, value: number | null) => ({ at, value });

describe("a chart's axes", () => {
  it("uses round numbers that take in everything", () => {
    expect(valueTicks(3, 97)).toEqual([0, 20, 40, 60, 80, 100]);
    expect(valueTicks(0.12, 0.38, 2)).toEqual([0.1, 0.2, 0.3, 0.4]);
  });

  it("still has an axis for one value", () => {
    const ticks = valueTicks(5, 5);
    expect(ticks[0]).toBeLessThan(5);
    expect(ticks[ticks.length - 1]).toBeGreaterThan(5);
    expect(valueTicks(0, 0).length).toBeGreaterThan(1);
  });

  it("has none for nothing", () => {
    expect(valueTicks(Infinity, -Infinity)).toEqual([]);
    expect(timeTicks(10, 10)).toEqual([]);
    // On the workspace's clock: India's hours begin at half past, UTC's.
    setTimeZone("Asia/Kolkata");
    expect(timeTicks(Date.UTC(2026, 10, 6, 0, 10), Date.UTC(2026, 10, 6, 5, 10))[0]).toBe(Date.UTC(2026, 10, 6, 0, 30));
    setTimeZone(undefined);
  });

  it("keeps times on whole steps", () => {
    const hour = 3_600_000;
    const ticks = timeTicks(hour * 100 + 17, hour * 106);
    expect(ticks.length).toBeGreaterThanOrEqual(2);
    expect(ticks.length).toBeLessThanOrEqual(7);
    expect(ticks.every((t) => (t - ticks[0]!) % hour === 0)).toBe(true);
  });
});

describe("a line", () => {
  const x = (t: number) => t;
  const y = (v: number) => v;

  it("breaks where nothing was reported", () => {
    expect(linePath([p(0, 1), p(1, 2), p(2, null), p(3, 4)], x, y, 10)).toBe("M0.0 1.0L1.0 2.0M3.0 4.0");
  });

  it("does not join readings too far apart to mean anything", () => {
    expect(linePath([p(0, 1), p(100, 2)], x, y, 10)).toBe("M0.0 1.0M100.0 2.0");
  });

  it("takes in the bands drawn around it", () => {
    expect(extent([{ label: "a", points: [{ at: 0, value: 5, min: 1, max: 9 }, p(1, null)] }])).toEqual([1, 9]);
    expect(extent([{ label: "a", points: [p(0, null)] }])).toBeNull();
  });

  it("finds the nearest reading, never a gap", () => {
    expect(nearest([p(0, 1), p(10, null), p(20, 3)], 11)?.at).toBe(20);
    expect(nearest([p(0, null)], 0)).toBeNull();
  });

  it("sums up what was reported", () => {
    expect(summary([{ at: 0, value: 2, min: 1, max: 3 }, p(1, null), p(2, 4)])).toEqual({ min: 1, max: 4, avg: 3, last: 4 });
    expect(summary([p(0, null)])).toBeNull();
  });
});

describe("a chart as a file", () => {
  it("has a column for each line and a row for each time", () => {
    const out = csv([
      { label: "temp", unit: "C", points: [p(0, 1), p(1000, 2)] },
      { label: "a, b", points: [p(1000, 3)] },
    ]);
    expect(out).toBe(`time,temp (C),"a, b"\n1970-01-01T00:00:00.000Z,1,\n1970-01-01T00:00:01.000Z,2,3\n`);
  });
});
