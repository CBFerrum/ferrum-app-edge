import { describe, expect, it } from "vitest";
import { tries } from "./connecting";

describe("trying a device", () => {
  it("has a command for each way in", () => {
    const got = tries(["listen+mqtt://0.0.0.0:1883", "listen+http://0.0.0.0:8080", "listen+coap://0.0.0.0:5683"], "TOK");
    expect(got.map((t) => t.how)).toEqual(["MQTT", "HTTP", "CoAP"]);
    expect(got[0]!.command).toContain("-p 1883 -u TOK");
    expect(got[1]!.command).toContain("http://GATEWAY:8080/api/v1/TOK/telemetry");
    expect(got[2]!.command).toContain("coap://GATEWAY:5683/api/v1/TOK/telemetry");
  });

  it("uses TLS where the port does", () => {
    const got = tries(["listen+mqtts://0.0.0.0:8883", "listen+https://0.0.0.0:8443"], "TOK");
    expect(got[0]!.command).toContain("--cafile");
    expect(got[1]!.command).toContain("https://GATEWAY:8443");
  });

  it("has nothing for what a device does not call", () => {
    expect(tries(["tcp://10.0.0.7:9100", "nonsense"], "TOK")).toEqual([]);
  });
});
