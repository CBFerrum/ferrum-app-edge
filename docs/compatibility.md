# Compatibility

What the site runtime has been tried against, in the fields a qualification needs. **Nothing here has
run on physical equipment.** A row says what was exercised, on what, and what remains: support for a
protocol is not support for a device, and no entry is a functional-safety or vendor qualification.

Levels, in the assessment's words: *Known supported*, *Known unsupported*, *Conditionally supported*,
*Vendor-dependent*, *Customer validation required*, *Unresolved safety or compliance question*.
Certification labels (Community, Security-reviewed, Operationally verified, Vendor-qualified) are
earned per device model and firmware. None is held yet.

## How far each has been taken

There is no one word for this, so none is used. Five separate things can be true of a protocol, and
being true of one says nothing about the next.

| Label | Means |
|---|---|
| **Implemented transport** | The code exists and speaks it |
| **Independent software tested** | It met software we did not write, such as a reference library or server |
| **Vendor simulator tested** | It met a simulator its equipment's maker ships |
| **Physical equipment qualified** | It ran on named equipment and firmware, through recovery and failure, and the record says which |
| **Production profile supported** | Someone stands behind that equipment and firmware at a site, with procedures |

| Protocol or path | Implemented | Independent software | Vendor simulator | Physical equipment | Production profile |
|---|---|---|---|---|---|
| MQTT 3.1.1 and 5 | Yes | Yes: Mosquitto, Paho | No | No | No |
| OPC UA, both ways | Yes | Yes: async-opcua, asyncua, open62541 | No | No | No |
| Sparkplug B | Yes | Yes: Eclipse Tahu's definitions | No | No | No |
| Modbus TCP | Yes | Yes: pymodbus | No | No | No |
| Modbus RTU | Yes | No: a pseudo-terminal | No | No | No |
| Siemens S7 | Yes | Yes: python-snap7 | No | No | No |
| EtherNet/IP | Yes | Yes: cpppo | No | No | No |
| TwinCAT ADS | Yes | Yes: pyads | No | No | No |
| BACnet/IP | Yes | Yes: BACpypes3 | No | No | No |
| SNMP | Yes | Yes: PySNMP | No | No | No |
| IEC 60870-5-104 | Yes | Yes: lib60870 | No | No | No |
| DNP3 | Yes | Yes: opendnp3 | No | No | No |
| OCPP 1.6 | Yes | Yes: the Python `ocpp` library | No | No | No |
| OCPP 2.0.1 | Yes | No: frames written here | No | No | No |
| LwM2M, CoAP | Yes | Yes: Leshan, aiocoap | No | No | No |
| KNX, M-Bus, DLMS, MTConnect | Yes | No: stand-ins or frames compared | No | No | No |
| VDA 5050 2.x | Yes | No: a stand-in broker | No | No | No |
| VDA 5050 3.0.0, Open-RMF, openTCS, rosbridge, MiR, Universal Robots | A template only | No | No | No | No |
| A robotic warehouse mission, end to end | Yes | Against a simulated fleet only | No | No | No |
| A machine start, end to end | Yes | Against a simulated press only | No | No | No |

Nothing has reached the last two columns. A template in `templates` is a start for someone with the
equipment, and no row here is a claim about any device.

## Adapters

| | Simulator | Operator tasks | MQTT 5 | VDA 5050 | Modbus TCP |
|---|---|---|---|---|---|
| Level | Known supported, for rehearsal only | Known supported | Conditionally supported | Customer validation required | Customer validation required |
| Equipment | None. A fleet in a file | A person at the site console | Any device with MQTT 5 and a status topic | Vehicles and master controls speaking VDA 5050 | Controllers exposing registers |
| Protocol version | None | None | MQTT 5 through `rumqttc` | 2.0 and 2.1. Others are refused. For 3.0.0 see the template under Definitions | Modbus TCP through `tokio-modbus` |
| Reads | Battery, state | None | Topics named in the site's configuration | `state` topic | Registers named in the configuration |
| Writes | Missions | Tasks | Commands on the device's topic | Orders | Only registers named per operation |
| Authentication | None needed | PIN from the platform's list | Username and password; TLS by the broker's listener | As MQTT | None: Modbus has none. It belongs on a segmented network |
| Licences | Ferrum's | Ferrum's | MIT / Apache | As MQTT; the VDA specification is public | BSD-style |
| Maximum load | Not measured | Not measured | Not measured | Not measured | Not measured |
| Offline from the platform | Keeps running within the lease | Keeps running, at the console | Keeps running within the lease | Keeps running within the lease | Keeps running within the lease |
| Cancellation | Immediate | The person is told | A request on its topic, confirmed only by a status | `cancelOrder`, confirmed only by state | Refused. Stop it at the machine: a stop over Modbus would look like one without being one |
| Retry | Never automatic | Never automatic | Never automatic | Never automatic | Never automatic |
| After a crash | Asks the file, or with `cannot_find` it cannot ask | The task is still there | Cannot ask. The outcome is unknown until a status arrives | Cannot ask. Unknown until `state` names the order | Cannot ask. Unknown until the status register says |
| Tested by | `gateway/tests/recovery_it.rs`, killing the real binary | The same | `gateway/tests/adapters_it.rs`, real sockets against a stand-in broker | The same, against a stand-in broker | The same, against a stand-in controller |
| Not tested | | | Any real broker under load, TLS, reconnect storms | Any vehicle, any master control, any vendor simulator | Any controller, any gateway, word order on real devices |
| Excluded | Nothing physical happens | | Sparkplug B, which a definition reads | Interaction with a vehicle's own safety; instant actions beyond cancel | Generating controller logic. Coils and registers not named |
| Maintainer | Ferrum | Ferrum | Ferrum | Ferrum | Ferrum |
| Support ends | With the release line | With the release line | With the release line | With the release line | With the release line |

## Definitions

A definition is an adapter written as data ([guide](definitions.md)), so its promises are one
engine's, and what varies is the transport underneath. All of these are *Customer validation
required* per device model, and the templates in `templates` are starts, not support.

| Transport | Tried against | Not tried |
|---|---|---|
| TCP, UDP, serial, HTTP, WebSocket | Real sockets and a pseudo-terminal, with stand-ins (`gateway/tests/adapters_it.rs`) | Real serial hardware, RS-485 timing, any vendor's device |
| MQTT 5 | rumqttd 0.20 and a stand-in. TLS is not exercised | Mosquitto, EMQX, HiveMQ, reconnect storms |
| Device API | ThingsBoard's own Python client 1.13, unchanged, as a device and as a gateway, with firmware, in CI. Eclipse Paho 2.1 and 1.6 (MQTT 3.1.1), rumqttc v4 and v5, curl, aiocoap 0.4.17 | Any device's firmware, ThingsBoard's other SDKs |
| Modbus TCP and RTU | pymodbus 3.16 over TCP, in CI, and a stand-in on a pseudo-terminal with two units | Any PLC, a real RS-485 bus, CRC errors on the line |
| OPC UA | async-opcua 0.19's own server: None, and Basic256Sha256 with SignAndEncrypt, users, server trust by certificate, and browsing (`gateway/tests/protocols_it.rs`) | Any commercial server. Events have not been raised by anything |
| OPC UA, the other way | asyncua 2.1, in CI: as a client of a gateway's server, browsing, reading, subscribing and refused a write; as a server, its events and its history. A server's login, encryption and certificates against async-opcua's client. Two faults it found here are fixed: a select clause it could not read, and a device clock running ahead holding a value back. open62541's alarm server by hand: a condition enabled, raised, and acknowledged by the ids in its event | Any SCADA or historian |
| Siemens S7 | python-snap7 3.3's server: reads of every type, writes, strings, a missing block, and on real binaries through a gateway. Its server treats a bit write as a byte, so bits have not met a faithful peer | Any PLC |
| EtherNet/IP | cpppo 5.2: read, write, arrays, direct and through a slot | Any Logix controller |
| TwinCAT ADS | pyads 3.6's test server: symbols, read, write | Any TwinCAT runtime, routes |
| BACnet/IP | BACpypes3: analog and binary values, names, a commandable write, a refused read | Any controller, foreign device registration |
| SNMP v1, v2c, v3 | PySNMP 7.1: get, set, a missing OID, a trap and an inform, and v3 with SHA-1 and AES, SHA-256, and a wrong password. The key derivation matches RFC 3414's example | Any device's MIB, v3 traps |
| KNX | A stand-in written from the specification (`gateway/tests/knx_it.rs`). Nothing independent could run as an interface | Any interface, any bus |
| IEC 60870-5-104 | lib60870's server: interrogation and spontaneous data on real sockets. Commands were sent, and it did not hang up | Any RTU, redundancy groups |
| M-Bus | Frames built here and read the same by pyMeterBus 0.8 | Any meter, any level converter |
| DNP3 | opendnp3's outstation, by hand: an integrity poll, events and unsolicited responses confirmed, one point on request, and binary and analog commands selected and operated. A stand-in does the same in CI | Any RTU. No Secure Authentication, no TLS, no serial, no file transfer |
| DLMS/COSEM | The requests compare equal to those of Gurux's client, apart from which optional services are offered, and read a stand-in written from the Green Book | Any meter. Nothing above low-level security, and no HDLC on a serial or optical port |
| OCPP 1.6 and 2.0.1 | The Python `ocpp` 2.1 library for 1.6, in CI and through a real gateway. 2.0.1 by frames written here (`gateway/tests/ocpp_it.rs`) | Any charger |
| LwM2M | Eclipse Leshan's demo client 2.0.0-M16, in CI | Any device |
| CoAP and LwM2M over DTLS | Eclipse Leshan's client, with its own DTLS stack, by pre-shared key, in CI, and refused when it asked to be a device its key was not for. The library here on both ends for CoAP. Key is the SHA-256 of the access token | Any constrained device. Certificates and raw public keys over DTLS |
| CoAP client | aiocoap 0.4.17, with 3 KB in blocks | Observe from a real device |
| NATS, FTP, folders | nats-server, pyftpdlib, the file system | |
| SQL | PostgreSQL 17 and SQLite | Other databases |
| Sparkplug B | rumqttd, as a primary host and an edge node, and its payloads read both ways by Eclipse Tahu's definitions (`gateway/tests/sparkplug_it.rs`) | Ignition, any real edge node, any real host |
| MTConnect | A stand-in agent (`gateway/tests/mtconnect_it.rs`) | A real agent or machine tool |
| Listeners and webhooks | Real sockets | Load, and devices that reconnect abruptly |
| External drivers | A shell script, and python-can's virtual bus | Any vendor's library, a real CAN bus, any Bluetooth device |

| Export | Tried against |
|---|---|
| NATS, Kafka, RabbitMQ, InfluxDB 2, ClickHouse | The real services, in CI (`gateway/tests/exports_it.rs`) |
| S3, SQS, SNS | moto, in CI. Signatures match Amazon's published example |
| Google Pub/Sub, Azure IoT Hub, Event Hubs | The documented shape of the calls. An assertion for Google is checked by `openssl`. **Nothing has been sent to their real services** |

| Template | Written from | State |
|---|---|---|
| VDA 5050 3.0.0 | The repository's JSON schemas, decoding a state shaped like them | No vehicle, no master control, no vendor simulator |
| Open-RMF | rmf-web's API server models | No running RMF |
| openTCS | Web API v8's OpenAPI document | No running kernel |
| rosbridge | The protocol's documentation, from memory of its operations | No running rosbridge. Check the action operations against your version |
| MiR | Public knowledge of its REST API | No robot. No copy of the vendor's documentation was read |
| Universal Robots dashboard | The driver's documentation of the dashboard client | No arm |

Measured on an Apple M1 Pro, in release, with `contract/tests/load.rs`: decoding alone,
369,000 JSON messages a second with three values each, 1,040,000 Modbus reads, 12,000 Sparkplug
messages of twenty metrics, and 269,000 readings a second through a five-step flow. That is what those
two steps cost.

End to end, with `scripts/soak.py`: one device publishing over MQTT at QoS 0, through one gateway's journal
and the platform, to rows in PostgreSQL with its default settings. An Apple M1 Pro with everything on
it, publisher included, built in release without link-time optimization:

| Offered | Happened |
|---|---|
| 28,500 readings a second for 20 seconds | All of them stored, 22 seconds after starting. Nothing dropped or shed |
| 100,000 a second for 20 seconds | About 74,000 a second taken at the gateway, and 37,000 a second stored, sustained. The rest counted on `/metrics` as dropped, and none lost silently |
| 7,500 a second with the platform killed for 30 seconds | About 225,000 kept at the gateway. All of them stored 32 seconds after the platform came back |

**This is one gateway on one machine for under a minute.** Many gateways on one platform, hours of it, a
real network, a gateway's own disk, and what happens as `op_measurement` grows are not measured.

## Known unsupported, for now

| What | Why, and what it would take |
|---|---|
| PROFINET, EtherCAT, CANopen as such | Not built. An external driver is the way, and PROFINET and EtherCAT belong to a controller anyway |
| ROS 2 by DDS directly | Through rosbridge. A vendor-qualified adapter as a process of its own is the rule |
| ThingsBoard, Siemens, Rockwell, AVEVA, Ignition, Dematic, Blue Yonder, Manhattan | Coexist and integrate per contract. There is no universal adapter and none is claimed |

[parity.md](parity.md) has the whole picture, reference by reference.

## Unresolved safety and compliance questions

Ferrum sits above equipment, so these belong to the integrator and the equipment's maker:

- whether a given operation may be started remotely at all (IEC 62443, ISO 10218, ISO 3691-4, ISO 13849);
- what an automation envelope may allow at a given site;
- what a site must do when a mission's outcome is unknown and a person is near the equipment.

A safety function never depends on Ferrum.
