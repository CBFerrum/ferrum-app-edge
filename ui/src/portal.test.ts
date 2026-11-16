import { describe, expect, it } from "vitest";
import { setTimeZone } from "@ferrum/client";
import { when } from "./Portal";

describe("a customer's devices", () => {
  it("says when a device was last heard from on the workspace's clock, not the browser's", () => {
    // 11:00 UTC on 5 Nov is already 6 Nov in Kiritimati (UTC+14), and still 5 Nov in any browser west of UTC+13.
    setTimeZone("Pacific/Kiritimati");
    try {
      const day = new Intl.DateTimeFormat(undefined, { dateStyle: "medium", timeZone: "Pacific/Kiritimati" }).format(new Date("2026-11-05T11:00:00Z"));
      expect(when("2026-11-05T11:00:00Z")).toContain(day);
      expect(when(null)).toBe("Not heard from");
    } finally {
      setTimeZone(undefined);
    }
  });
});
