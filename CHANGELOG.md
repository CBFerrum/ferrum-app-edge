# Changelog

## Unreleased

- A gateway as an OPC UA server, by an export to `listen+opcua://`: each device a folder, each metric a
  variable, read only, with a login over encryption if a secret is named.
- OPC UA history with `history:`, and events carrying `EventId` and `ConditionId`, met against asyncua.
- DNP3 as a master over TCP: integrity polls, events and unsolicited responses, and commands selected
  and then operated. It has met opendnp3's outstation.
- DLMS/COSEM meters by OBIS code, over the wrapper, read only.
- Kafka topics and AMQP queues as sources. A Kafka reader keeps its place across restarts in the
  gateway's state folder.

## 0.2.0

The gaps with ThingsBoard and the other reference products, closed or named: `docs/parity.md` goes
through each capability, and says what met someone else's software, what only met a stand-in, what is
left out on purpose, and what is still missing. **Nothing has run on physical equipment.**

- Devices that call in: MQTT 3.1 to 5, HTTP and CoAP on ThingsBoard's own topics and paths, with
  tokens, passwords and certificates, provisioning, claiming, LwM2M, and OCPP 1.6 and 2.0.1.
- Protocols by name, with `tags:` for one line each: S7, EtherNet/IP, ADS, BACnet, SNMP v1 to 3 with traps,
  KNX, IEC 60870-5-104, M-Bus, and SQL, folders, FTP, NATS and CoAP as sources. Sparkplug as a primary
  host and as an edge node, MTConnect's sample stream, and browsing and events on OPC UA. CAN and
  Bluetooth by external drivers that ship here.
- Numbers worked out at the gateway, with `derive`.
- Flows: fifteen more kinds of step, answers to devices, live watching of real messages, flows on the
  platform that see across sites, one flow used inside another, every version kept, and flows in files.
- Dashboards and history, opening one another, and shared by a link that can read only what was laid out.
- Alarms that are assigned, shelved and commented on, and notices that escalate in steps.
- A fleet: find by tag, site, definition and asset, import from CSV, ask many at once, schedules,
  reports, solutions in one file, and firmware rollouts that stop by themselves, for sensors and
  meters only, and never a robot, a controller, a machine or a fleet.
- Exports to NATS, Kafka, RabbitMQ, InfluxDB, ClickHouse, S3, SQS, SNS, Pub/Sub, Azure IoT Hub, Event
  Hubs and Sparkplug. An address with a password in it is refused.
- A device as a twin, a W3C Thing Description and an Asset Administration Shell.
- Tighter limits: every listener needs the site's `listen` list, a definition reads files only under
  `files_dir`, and a poll that writes is refused, because writing is a command.

Upgrading needs a gateway and the app together: `ConfigRequest` and `FlowSpec` grew, in ways an older
one ignores. A site that wants `file://` or `sqlite://` must name `files_dir`.

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
