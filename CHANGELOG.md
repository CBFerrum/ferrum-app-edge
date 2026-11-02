# Changelog

## 0.4.0

From a review of the connected system under retries, crashes, configuration changes, clock changes
and physical exceptions. **Nothing here has met physical equipment.**

- **Recovery asks the device a command went to, and no other.** What a command went out on is written
  with its intent. Looking for it after a crash, following it and stopping it go back to exactly that,
  or nowhere: a device that merely takes the same operation is never asked, because its "never heard
  of it" would look like leave to send the command again. If what reaches a device has changed since,
  the outcome is unknown, for a person. **Finish running work before upgrading:** work a journal holds
  from before this has nothing to go back to, and becomes unknown.
- **Stopping one vehicle's order stops one vehicle.** `cancelOrder` went to every vehicle on an adapter,
  and it stops whatever a vehicle is doing. It now goes to the vehicle that has the order, once that
  vehicle says so. Over MQTT a cancel goes to the device's own topic, not to `all`.
- **Time is the platform's, carried forward by a clock nothing here can set.** Setting the wall clock
  either way changes nothing, a suspended machine has been away, and a process restart loses nothing.
  After a reboot nobody can say how long it was off, so no new work starts until the platform answers
  or a person vouches at the console. A clock that ran ahead no longer locks a gateway out of signing.
  `clock_drift_ppm` keeps a slow crystal from stretching a lease.
- **Every command is checked the moment before it goes**, not only when it was planned: its deadline,
  the lease, the authority, a cancel, the device and what it may do. Fencing is checked in the same
  statement that writes the intent. A command out of time is held back, where it used to go with a
  second to live.
- **A dependency needs a recorded completion.** Absent, failed, started and unreadable are not done.
  How a command ended outlives its row, and what only the platform knows is asked of it.
- **A redirect cannot leave the allow list**, for firmware, updates, exports and HTTP devices, and
  encrypted never becomes plain. `allow` now means the host it names, or a range when it ends in a dot
  or a dash: `plant.example.com` used to match `plant.example.com.elsewhere.org`, and `192.168.1` used to
  match `192.168.100.5`. **Check your `allow` list.** A release's download is held to it too, so a site
  with one lists where its releases come from, redirects included. The link to the platform follows no
  redirect at all.
- A box behind a peer takes its deadline from the site that holds authority, refuses what is out of time
  or not what was authorized, and will not speak for what went out on something else.
- The console asks for the load as well as the place, when a task names one, and takes a person's word
  for the time when there is no platform to ask.
- Definitions can send `{expires_at}` and `{epoch}`, so equipment that can check them, can.

## 0.3.1

- The gateway's releases are static, so they run on any 64-bit Linux. Those of 0.3.0 needed glibc 2.39 and
  would not start on Ubuntu 22.04, Debian 12 or Raspberry Pi OS Bookworm. Names now come from `/etc/hosts`
  and DNS alone, so a `.local` name found by mDNS needs an address, or a build of your own. On ARM the
  program sits at a fixed address, as Rust's static ARM target builds it, so it gets no address
  randomization of its own; a build of your own keeps that too.
- A static build uses mimalloc, because on musl's own allocator a gateway stored about 15% fewer readings
  at full load. On one 4-core ARM machine it carries what 0.3.0 did, about 90,000 readings a second into
  the journal. Five minutes at 60,000 a second settled at about 40 MB of memory, as 0.3.0 does, after a
  peak of 78 MB rather than 52.
- A release is started on Ubuntu 22.04, Debian 12, CentOS 7 and Alpine, and passes the gateway's tests on
  the C library it ships with, before anything is published. Both run on a pull request that changes
  the release, so a tag is not the first time they are tried.

## 0.3.0

- Customers who own devices: their people see those devices, their alarms and the dashboards given to
  them in the workspace's portal, and nothing else.
- A device can show its customer only some of its readings, and alarms can be chosen for push.
- Relations between devices, by any name, shown on both and usable to choose them.
- Reports by the clock, by email, as CSV or PDF.
- Pages hear on the platform's socket that readings landed, and ask again at once.
- A workspace's allowance of measurements a day: past it, gateways hold readings until there is room.
- CoAP and LwM2M over DTLS, by pre-shared key, so they can leave a private network: `listen+coaps` and
  `listen+lwm2ms`. Met Eclipse Leshan's client.
- SNMP v3 traps, with `?user=` on `listen+snmp`, and BACnet's `discover`, by Who-Is.
- LwM2M writes go as TLV, the one format every device has to take.
- OCPP security profile 3: a charger known by its certificate. Charging profiles go as commands.
- Templates for The Things Stack, Sigfox, LORIOT, ThingPark, AWS IoT Core and Azure Event Hubs. `bytes:`
  decodes a payload delivered inside JSON, and `device:` can come from the message.
- An export to a table of your own in PostgreSQL.
- A backlog drains as fast as the platform takes it, rather than 5,000 readings each `sync_secs`, and a
  link piling up wakes the reader, rather than dropping them until its next `read_secs`. One gateway went
  from about 1,000 stored a second to 28,500 with nothing lost. `scripts/soak.py` measures it.
- AAS Part 2's repository, reading only, and what scanners saw as a GS1 EPCIS 2.0 document.
- `ferrum-site release-key` and `sign`, and a workflow that builds the gateway on a tag.
- ThingsBoard's own Python client works against a gateway unchanged, in CI, for devices and gateways.
  It found a real fault: its firmware subscription did not match the chunks, so a download hung.
- Modbus TCP against pymodbus, and Sparkplug payloads against Eclipse Tahu's definitions, in CI.
- A gateway as an OPC UA server, by an export to `listen+opcua://`: each device a folder, each metric a
  variable, read only, with a login over encryption if a secret is named.
- OPC UA history with `history:`, and events carrying `EventId` and `ConditionId`, met against asyncua.
- DNP3 as a master over TCP: integrity polls, events and unsolicited responses, and commands selected
  and then operated. It has met opendnp3's outstation.
- DLMS/COSEM meters by OBIS code, over the wrapper, read only.
- Kafka topics and AMQP queues as sources. A Kafka reader keeps its place across restarts in the
  gateway's state folder.

Upgrading: gateways first if a workspace will have an allowance of measurements, since an older one treats
a held exchange as delivered. An export to `postgres` or `listen+opcua` needs a gateway of this version. A
site that wants `listen+opcua`, `listen+coaps` or `listen+lwm2ms` names the address under `listen`.

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
