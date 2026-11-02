# IoT Edge against the reference products

What each product does, read from the copies in `referance/` for what they do and not how, and where
IoT Edge stands, capability by capability. **Nothing here has run on physical equipment**, and no
entry is a vendor qualification or a safety certification. [compatibility.md](compatibility.md) has
the detail of what each thing was tried against.

| Status | Means |
|---|---|
| **Verified** | Built, and it met software we did not write: a broker, a library, a stack, in CI or by hand |
| **Built** | Built and tested here, against a stand-in we wrote from the specification |
| **Partly** | Built in part, and the row says what is missing |
| **By design** | Left out on purpose, and the row says why |
| **External** | Done by an external driver or another product, which ships as a starting point |
| **No** | A gap |

## Reaching devices

| Capability | Who has it | Status | Notes |
|---|---|---|---|
| Device described as data | ThingsBoard profiles, EdgeX, OpenRemote | **Verified** | Definitions, tried on a sample, reviewed, published to gateways |
| Device API over MQTT 3.1, 3.1.1, 5 | ThingsBoard | **Verified** | ThingsBoard's own topics, against Paho and rumqttc |
| Device API over HTTP | ThingsBoard | **Verified** | ThingsBoard's paths, against curl |
| Device API over CoAP, with Observe and blocks | ThingsBoard | **Verified** | Against aiocoap. **No DTLS** |
| Gateway API for devices behind a device | ThingsBoard | **Built** | `v1/gateway/*`, against rumqttc |
| Access tokens, basic MQTT credentials, X.509 | ThingsBoard, Hono | **Verified** | Only hashes reach a gateway |
| Device provisioning by a shared key | ThingsBoard | **Verified** | It adds a device that reports, and takes no command until commissioned |
| Claiming by a device's secret | ThingsBoard | **Built** | |
| Bulk import from CSV, with tokens | ThingsBoard PE, Hono | **Built** | |
| LwM2M server | ThingsBoard, Hono | **Verified** | Against Eclipse Leshan's client, by plain text values. **No DTLS, no bootstrap, no TLV or SenML writes** |
| SNMP v1, v2c and v3, traps and informs | ThingsBoard, EdgeX | **Verified** | Against PySNMP: v3 with SHA-1 and AES, and SHA-256. Traps are v1 and v2c |
| OCPP 1.6-J and 2.0.1 | ThingsBoard Gateway, OpenRemote | **Verified** | 1.6 against the Python `ocpp` library; 2.0.1 against frames written here. Security profiles 0 to 2. **No certificates (profile 3), no smart charging** |
| Modbus TCP and RTU | Everyone | **Built** | Against a stand-in and a pseudo-terminal |
| OPC UA client: watch, read, write, call, encryption, users, browse | Ignition, Kura, Azure IoT Operations | **Verified** | Against async-opcua's server |
| OPC UA events and history | Ignition | **Verified** | Against asyncua: events with their ids, and a node's history from where this last got to. Acknowledging a condition is a `call` to its `i=9111`, which no server has answered yet |
| OPC UA server | Ignition, Kura | **Verified** | As an export, read only, against asyncua's client; with encryption, a login and certificates a person trusts, against async-opcua's. **No history**, and a variable appears with its first reading |
| Siemens S7 | PLC4X, Ignition, ThingsBoard Gateway | **Verified** | Against python-snap7's server. Its emulation of bit writes differs, so a bit write has only met the standard's description |
| EtherNet/IP, Logix tags | PLC4X, Ignition | **Verified** | Against cpppo, direct and routed. **No I/O connections, by design** |
| TwinCAT ADS | PLC4X, Ignition | **Verified** | Against pyads's test server, by symbol |
| BACnet/IP | ThingsBoard Gateway, OpenRemote | **Verified** | Against BACpypes3, read and write. **No Who-Is, no COV** |
| KNX over IP | ThingsBoard Gateway, OpenRemote | **Built** | Tunnelling, against a stand-in |
| IEC 60870-5-104 | Ignition modules | **Verified** | Against lib60870: interrogation, spontaneous data, commands sent |
| DLMS/COSEM | Metering head ends, Ignition modules | **Built** | Over the wrapper, by OBIS code, with blocks and low-level security, read only. The requests compare equal to Gurux's client. **No HDLC, no high-level security or ciphering** |
| DNP3 | Ignition modules | **Verified** | As a master over TCP against opendnp3: integrity, events, unsolicited, select-before-operate. **No Secure Authentication, no TLS, no serial** |
| M-Bus | OpenRemote, metering | **Verified** | Frames read as pyMeterBus reads them. No meter |
| CANopen and raw CAN | ThingsBoard Gateway, Kura | **External** | `drivers/can_bridge.py`, on python-can's virtual bus |
| Bluetooth Low Energy | ThingsBoard Gateway | **External** | `drivers/ble_bridge.py`. Has met no device |
| LoRaWAN, Zigbee | ThingsBoard, OpenRemote | **External** | ChirpStack and Zigbee2MQTT over MQTT, with templates |
| Sigfox, NB-IoT platforms, other clouds as sources | ThingsBoard PE integrations | **Partly** | By webhook or MQTT, as any other source. No ready-made decoders |
| CoAP client, with Observe and blocks | Hono | **Verified** | Against aiocoap |
| Sparkplug B primary host | Ignition, HiveMQ | **Built** | STATE, sequence checks, rebirth, births and deaths, over rumqttd |
| Sparkplug B edge node | Ignition Edge, Kura | **Built** | As an export, over rumqttd. No other Sparkplug software has met either yet. **No datasets or templates** |
| MTConnect | Ignition modules | **Built** | Current and the sample stream, against a stand-in |
| SQL as a source | ThingsBoard Gateway (ODBC) | **Verified** | PostgreSQL and SQLite, read only. Others, by an external driver |
| FTP and a watched folder | ThingsBoard Gateway | **Verified** | FTP against pyftpdlib |
| NATS as a source | Magistrala | **Verified** | Against nats-server |
| Kafka and AMQP as sources | ThingsBoard PE | **Verified** | Against Apache Kafka and RabbitMQ, in CI. A Kafka reader keeps its place in a file and has no consumer group |
| HTTP polling, WebSocket, TCP, UDP, serial, webhooks | Everyone | **Built** | Against real sockets |
| Any other protocol | EdgeX, Kura | **Verified** | External drivers, as JSON lines in any language |
| Discovery of devices | EdgeX | **Partly** | Strangers on a connection are noticed and a person claims them. No network scanning, on purpose |

## Robots

| Capability | Who has it | Status | Notes |
|---|---|---|---|
| VDA 5050 2.0, 2.1, 3.0.0 | Master controls | **Built** | Against a stand-in |
| Open-RMF, openTCS, rosbridge, MiR, Universal Robots dashboard | Each | **Built** | Templates written from their documents. None has met a running system |
| ROS 2 over DDS directly | rclcpp, zenoh | **By design** | Through rosbridge: a DDS stack belongs in a vendor-qualified process |
| Traffic, path planning, collision avoidance, motion | Nav2, RMF, MoveIt, fleet managers | **By design** | Never. They own execution |

## Logic

| Capability | Who has it | Status | Notes |
|---|---|---|---|
| Flow builder on a canvas | Node-RED, ThingsBoard, OpenRemote, StreamPipes, EMQX | **Verified** | 44 kinds of step, on real binaries |
| Filter, enrich, transform, delta, geofence, dedupe, split, schedule | ThingsBoard rule nodes | **Built** | |
| Answer a device | ThingsBoard | **Verified** | A flow answered Paho on real binaries |
| Rule engine on the platform | ThingsBoard, OpenRemote | **Verified** | Flows that run on the platform, a closed minute at a time |
| Rules at the edge, offline | ThingsBoard Edge, Greengrass | **Verified** | Rules and flows at the gateway, on real binaries |
| Simulate before it runs | ThingsBoard's script test, in part | **Verified** | By the code a gateway runs, and required before review |
| Review by someone else, one site first | Rarely anywhere | **Verified** | |
| Live debugging of a flow | Node-RED, ThingsBoard | **Verified** | Real messages at each step, on real binaries |
| Version history, export and import | ThingsBoard, Node-RED | **Built** | |
| Sub-flows | Node-RED, ThingsBoard | **Built** | A reviewed flow is copied in, so what is simulated and run is the whole thing, and a change to what it uses means seeing it again |
| Scripts in a flow | Node-RED function, ThingsBoard TBEL | **By design** | No code in a flow, so that it can be reviewed and cannot hang a gateway |
| Loops | Node-RED | **By design** | Refused when it is checked |
| Flows acting directly on equipment | Node-RED, ThingsBoard | **By design** | A flow asks the ledger |
| Calculated fields | ThingsBoard | **Built** | `derive` in a definition, at the gateway, and flows for anything across devices |
| Statistical anomaly detection | StreamPipes | **Built** | Per device. It is a statistic, not a diagnosis |
| Machine learning at the edge | Greengrass, Azure, StreamPipes | **External** | An external driver can run a model and report events. No ONNX runtime is built in |

## Alarms and notifications

| Capability | Who has it | Status | Notes |
|---|---|---|---|
| Rules with hold and clear, severities | ThingsBoard, Ignition | **Verified** | |
| Acknowledge, clear, assign, comment, shelve | ThingsBoard, Ignition | **Built** | |
| Escalation and notification rules | ThingsBoard, Ignition pipelines | **Built** | In-app, email and webhooks, durable, at least once |
| SMS and voice | ThingsBoard PE, Ignition | **External** | By a webhook to a provider |
| Mobile push | ThingsBoard | **Partly** | Through the workspace's PWA and its notifications |

## Dashboards and data

| Capability | Who has it | Status | Notes |
|---|---|---|---|
| Dashboards: grid, widgets, variables | ThingsBoard, Ignition, Node-RED | **Verified** | In a browser, at 1440 and 375 pixels |
| SCADA symbols | ThingsBoard SCADA | **Built** | A small set. No drawing tool for new ones |
| Maps | ThingsBoard, OpenRemote | **Built** | Where devices say they are |
| Controls that send commands | ThingsBoard, Ignition | **Verified** | Through the ledger |
| Public read-only links | ThingsBoard | **Verified** | A link can read only what was laid out |
| Drill-down between dashboards | ThingsBoard states | **Built** | Links and table rows open another dashboard with its device variable set |
| Custom widgets in JavaScript | ThingsBoard | **By design** | No code a reviewer cannot read |
| History: long ranges by rollup, short by sample | ThingsBoard, Ignition | **Verified** | |
| Time-series at scale | ThingsBoard with Cassandra, TimescaleDB | **Partly** | PostgreSQL carries this to what one database can take. Past that, export to ClickHouse or InfluxDB |
| Live updates over a socket | ThingsBoard | **Built** | Pages hear on the platform's socket that a site's readings landed and ask again at once, paced to one ask in three seconds, so a reading shows as soon as its gateway has sent it. The socket says that something changed and not what: a page still asks, with its reader's rights. Shared dashboards poll |
| Reports | ThingsBoard PE | **Built** | Availability, alarms and readings, to read and export, and by schedule to anyone by email as CSV or PDF, with the mail in one transaction with the note that it ran. The PDF path needs the server's renderer and has not been run |
| Assets and hierarchy | ThingsBoard, OpenRemote | **Built** | Assets inside assets, devices on them, selection by what is under an asset |
| Arbitrary entity relations | ThingsBoard | **Built** | Between devices, by any name, and devices can be chosen by them. Assets contain. Flows do not follow them |
| Customers owning devices | ThingsBoard | **Built** | A customer's people sign in to the workspace's portal and see their devices, alarms and given dashboards, and nothing else. They can acknowledge an alarm, and they cannot command |

## Fleet

| Capability | Who has it | Status | Notes |
|---|---|---|---|
| Firmware for devices, in rollouts | ThingsBoard, Hono | **Verified** | Paho fetched 50 KB in chunks on real binaries, and a rollout finished |
| Software update of the gateway itself | Kura, Greengrass | **Built** | Signed releases, staged |
| Bulk commands and settings | ThingsBoard PE | **Verified** | Each through the ledger |
| Scheduler | ThingsBoard PE | **Built** | As automation, under an envelope |
| Solution bundles | ThingsBoard PE | **Verified** | They arrive as drafts |
| Tenants, roles, audit, SSO, API tokens | ThingsBoard, Hono | **Verified** | The platform's own |
| Usage limits per tenant | ThingsBoard | **Built** | The operator's, per workspace: measurements a day and devices. Past the first, gateways hold readings until there is room, and commands and leases go on. Other limits are budgets at the gateway |
| Container workloads at the edge | Greengrass, Kura | **By design** | One binary, and external drivers |
| Local dashboards at a gateway | Ignition Edge | **Partly** | The console shows work, links and problems. It draws no charts |
| Federation between platforms | Ignition GAN | **Partly** | A gateway can export to another platform's ingest, and peers work through each other. Platforms do not share a view |

## Exports

| Capability | Who has it | Status | Notes |
|---|---|---|---|
| Webhooks, MQTT | Everyone | **Verified** | Signed batches |
| NATS | Magistrala | **Verified** | nats-server, in CI |
| Kafka | ThingsBoard | **Verified** | Apache Kafka, in CI |
| RabbitMQ and other AMQP 0-9-1 | ThingsBoard | **Verified** | RabbitMQ, in CI, with publisher confirms |
| InfluxDB, ClickHouse | StreamPipes, Azure | **Verified** | The real servers, in CI |
| S3, SQS, SNS | ThingsBoard, Greengrass | **Built** | Against moto, with signatures that match Amazon's published examples. Not sent to AWS |
| Google Pub/Sub, Azure IoT Hub, Event Hubs | ThingsBoard | **Built** | Against the documented shape of the calls only. Not sent to Google or Azure |

## Twins and standards

| Capability | Who has it | Status | Notes |
|---|---|---|---|
| Reported and desired state | Ditto, ThingsBoard | **Verified** | |
| W3C Thing Description | Ditto | **Built** | Written from the records. Not checked against a validator |
| Asset Administration Shell | BaSyx | **Verified** | The output validates against the official JSON schema. **Export only**: no AAS server |
| EPCIS | GS1 | **No** | Not a concern of this app today |

## Brokers

| Capability | Who has it | Status | Notes |
|---|---|---|---|
| Devices connecting straight to a gateway | ThingsBoard, Mosquitto | **Verified** | The device API only |
| A general MQTT broker: bridges, ACLs, shared subscriptions, clusters | EMQX, Mosquitto | **By design** | Run Mosquitto beside it. `deploy/mosquitto.conf` is a start |

## Above equipment, on purpose

These are the boundaries, and no feature above crosses them. Ferrum does not do motion control, safety
functions or LLM control of plant, and does not offer a way to run a script on a PLC. Whatever can
change equipment is a command through the ledger, and anything that protects a person stays with the
equipment and its safety system.
