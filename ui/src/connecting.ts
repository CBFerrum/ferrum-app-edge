// What a person runs to try a device, with their token in it.

export interface Try {
  how: string;
  command: string;
}

const SAMPLE = `{"temperature": 21.5}`;

/** A command for each way in. The gateway's own address is the one thing only they know. */
export function tries(addresses: string[], token: string): Try[] {
  const out: Try[] = [];
  for (const address of addresses) {
    const [scheme, rest] = address.split("://");
    const port = rest?.split(":")[1]?.split("?")[0] ?? "";
    switch (scheme) {
      case "listen+mqtt":
        out.push({ how: "MQTT", command: `mosquitto_pub -h GATEWAY -p ${port} -u ${token} -t v1/devices/me/telemetry -m '${SAMPLE}'` });
        break;
      case "listen+mqtts":
        out.push({ how: "MQTT over TLS", command: `mosquitto_pub -h GATEWAY -p ${port} --cafile ca.pem -u ${token} -t v1/devices/me/telemetry -m '${SAMPLE}'` });
        break;
      case "listen+http":
      case "listen+https":
        out.push({
          how: scheme === "listen+http" ? "HTTP" : "HTTPS",
          command: `curl -X POST ${scheme === "listen+http" ? "http" : "https"}://GATEWAY:${port}/api/v1/${token}/telemetry -H 'content-type: application/json' -d '${SAMPLE}'`,
        });
        break;
      case "listen+coap":
        out.push({ how: "CoAP", command: `coap-client -m post -t json -e '${SAMPLE}' coap://GATEWAY:${port}/api/v1/${token}/telemetry` });
        break;
      case "listen+coaps":
        out.push({ how: "CoAP over DTLS, whose key is the SHA-256 of the token", command: `coap-client -m post -t json -u DEVICE_CODE -k KEY -e '${SAMPLE}' coaps://GATEWAY:${port}/api/v1/${token}/telemetry` });
        break;
    }
  }
  return out;
}
