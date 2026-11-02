import { describe, expect, it } from "vitest";
import { added, type Widget, isOn, kindOf, KINDS, moved, nextSpot, overlap, parse, resized, settle, share, tone } from "./layout";

const w = (id: string, x: number, y: number, width = 2, h = 2): Widget => ({ id, type: "value", x, y, w: width, h });
const clashes = (list: Widget[]) => list.flatMap((a) => list.filter((b) => a.id < b.id && overlap(a, b)));

describe("laying a dashboard out", () => {
  it("moves down what a widget lands on, and what that lands on", () => {
    const out = moved([w("a", 0, 0), w("b", 0, 2), w("c", 0, 4), w("d", 6, 0)], "c", 0, 0);
    expect(clashes(out)).toEqual([]);
    expect(out.find((x) => x.id === "c")).toMatchObject({ x: 0, y: 0 });
    expect(out.find((x) => x.id === "a")!.y).toBe(2);
    expect(out.find((x) => x.id === "b")!.y).toBe(4);
    expect(out.find((x) => x.id === "d")!.y).toBe(0);
  });

  it("does not stack two pushed widgets on each other", () => {
    expect(clashes(resized([w("big", 0, 0, 4, 1), w("a", 0, 1), w("b", 0, 3)], "big", 4, 6))).toEqual([]);
  });

  it("stays on the grid however far it is dragged", () => {
    expect(moved([w("a", 0, 0, 4)], "a", 50, -3)[0]).toMatchObject({ x: 8, y: 0 });
    expect(resized([w("a", 0, 0)], "a", 99, 0)[0]).toMatchObject({ w: 12, h: 1 });
    expect(settle([w("a", 0, 0)], "nothing")).toHaveLength(1);
  });

  it("finds room for a new one", () => {
    expect(nextSpot([], 3, 3)).toEqual({ x: 0, y: 0 });
    expect(nextSpot([w("a", 0, 0, 12, 2)], 3, 3)).toEqual({ x: 0, y: 2 });
    expect(nextSpot([w("a", 0, 0, 6, 2)], 6, 2)).toEqual({ x: 6, y: 0 });
  });

  it("gives every new widget a name of its own", () => {
    const one = added([w("w2", 0, 0)], "gauge");
    const two = added(one.widgets.slice(1), "gauge");
    expect(new Set([one.id, "w2"]).size).toBe(2);
    expect(two.widgets.at(-1)).toMatchObject({ type: "gauge", title: "Gauge" });
    expect(clashes(one.widgets)).toEqual([]);
  });

  it("has something to say about every kind", () => {
    for (const k of KINDS) expect(kindOf(k.type)).toBe(k);
  });

  it("reads what cannot be read as empty", () => {
    expect(parse("{")).toEqual({ widgets: [] });
    expect(parse(null)).toEqual({ widgets: [] });
    expect(parse('{"widgets": 3}')).toEqual({ widgets: [] });
  });
});

describe("what a value means", () => {
  it("is worse the higher it goes, by default", () => {
    expect(tone(10, { warn: 70, bad: 90 })).toBe("ok");
    expect(tone(70, { warn: 70, bad: 90 })).toBe("warn");
    expect(tone(95, { warn: 70, bad: 90 })).toBe("bad");
    expect(tone(95, {})).toBe("ok");
  });

  it("is worse the lower it goes when red is below amber", () => {
    expect(tone(50, { warn: 30, bad: 10 })).toBe("ok");
    expect(tone(25, { warn: 30, bad: 10 })).toBe("warn");
    expect(tone(5, { warn: 30, bad: 10 })).toBe("bad");
  });

  it("stays on the dial", () => {
    expect(share(50)).toBe(0.5);
    expect(share(-5)).toBe(0);
    expect(share(500)).toBe(1);
    expect(share(5, 10, 10)).toBe(0);
  });

  it("knows on from off", () => {
    expect([isOn(1), isOn(0), isOn(true), isOn(false), isOn("running"), isOn("stopped"), isOn(null)]).toEqual([true, false, true, false, true, false, false]);
    expect([isOn("open", "open"), isOn("closed", "open"), isOn(2, "2")]).toEqual([true, false, true]);
  });
});
