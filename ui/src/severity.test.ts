import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";
import { SEVERITY_TONE, severityTone } from "./severity";

describe("alarm severity tone", () => {
  it("has an entry for every severity the packages define", () => {
    const yaml = readFileSync(new URL("../../packages/edge/entities/edge.yaml", import.meta.url), "utf8");
    const defined = new Set<string>();
    for (const line of yaml.split("\n")) {
      const m = /name: (?:min_)?severity,.*?options: \[([^\]]*)\]/.exec(line);
      if (m) m[1]!.split(",").forEach((o) => defined.add(o.trim()));
    }
    expect(defined.size).toBeGreaterThan(0);
    for (const id of defined) expect(SEVERITY_TONE, id).toHaveProperty(id);
  });
  it("is red for Critical and Major, amber below, blue for Info", () => {
    expect(["Critical", "Major"].map(severityTone)).toEqual(["red", "red"]);
    expect(["Minor", "Warning"].map(severityTone)).toEqual(["amber", "amber"]);
    expect(severityTone("Info")).toBe("blue");
  });
  it("makes an unknown id amber", () => {
    expect(severityTone("Catastrophic")).toBe("amber");
    expect(severityTone("toString")).toBe("amber");
  });
});
