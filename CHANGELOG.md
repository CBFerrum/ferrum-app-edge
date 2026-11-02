# Changelog

## 0.1.0

IoT Edge in a repository of its own, with the gateway it runs on: the app, `ferrum-site` and what the
two share moved here from Ferrum with their history. Workspaces keep the app as `edge`, and nothing
about their data changes. Run against simulators and stand-ins only.

- Definitions: devices described in a few lines of YAML over TCP, UDP, serial, HTTP, WebSocket, MQTT,
  Modbus TCP and RTU, OPC UA, listeners and webhooks, reading JSON, XML and Sparkplug B, with twenty
  templates, and anything else through an external driver.
- Flows: logic drawn on a canvas, simulated, reviewed by someone else, and run at the gateway with or
  without the link. A flow can ask for a command and cannot give one.
- Gateways: `ferrum-site` keeps a journal through outages, crashes and restores, and runs on what it
  was last told.
- Server to server: boxes in cabinets, a relay out of a closed network, exports held until delivered,
  webhooks, and signed updates with rollback.
- Alarms, reported state, a robot map and a console on the gateway.
