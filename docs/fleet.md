# Looking after a fleet

What IoT Edge does for many devices at once, beyond reading them. Everything here that reaches a
device goes through the ledger, one device at a time, so what stops a person asking one device
stops this for that device, and says so.

## Finding devices

The Devices page takes a search and a tag. `GET /devices` also takes `site`, `definition` and `asset`,
and everything given has to hold. A tag is one of a comma-separated list on the device, in any case.
An **Edge Asset** is a thing that matters here and is not a device, a building or a cold room. Assets
sit inside each other, a device belongs to one, and an asset takes in everything under it.

Each device shows when it was last heard and whether it is connected now. *How much of the time*
comes from `GET /devices/{id}/availability`, by the minutes it was heard in, and the Home page says
how each definition stands, and which firmware versions are running.

## Adding them in numbers

*Import* takes a CSV whose first line names its columns: `code` and `name` at least, and `kind`,
`site`, `gateway`, `definition`, `tags` and `address` if wanted, by the code or name people know them
by. Each row stands alone, and every row that failed says why. With *give each an access token*, each
new device gets one, shown once as a file: only a hash is stored.

## Asking them

Choosing devices on that page, or `POST /fleet/command` and `POST /fleet/desired`, asks each for the
same thing, up to 500 at a time. Each is the person's own request under their authority. Nothing
chosen is not everything: an empty selection is refused.

## By the clock

An **Edge Schedule** asks a site's, a definition's or a tag's devices for something every so many
minutes, or at a time of day on given days by the site's clock. A time missed by more than half an
hour has passed. It goes through the ledger as automation, so it runs under an envelope someone
approved, or waits for a person.

## Firmware

An **Edge Firmware** says what a file is, where a gateway can fetch it, and its SHA-256. An **Edge
Rollout** takes it to devices a few at a time, and only:

- devices of a definition that says `firmware: true`, which also takes `api: device`;
- that are commissioned for the `firmware.update` operation;
- of kind Sensor, Meter, Scanner or Station. **Never a robot, a controller, a machine or a fleet**,
  whatever a definition says: their firmware belongs to their makers, and to safety assessments that
  nothing here is part of.

`percent` is the share of those devices it will ever reach, which is how a change is tried. `batch` is
how many can be updating at once. It stops by itself at `stop_after` failures, and says so.

A gateway fetches a file once for its whole site, offers it only if the hash matches, and keeps it by
hash. A device hears `fw_title`, `fw_version`, `fw_size`, `fw_checksum` and `fw_checksum_algorithm` as
attributes, as ThingsBoard's clients expect, and fetches it from the gateway:

| Way in | How |
|---|---|
| MQTT | Publish the size you want to `v2/fw/request/{id}/chunk/{n}`, hear `v2/fw/response/{id}/chunk/{n}` |
| HTTP | `GET /api/v1/{token}/firmware?title=&version=&size=&chunk=` |
| CoAP | `GET coap://gateway/api/v1/{token}/firmware?size=&chunk=` |

Past the end there is nothing, which is how a device knows. It reports `fw_state`: `UPDATED` finishes
the command and `FAILED` fails it, and nothing else does, because only a device knows.

## Solutions

A solution is definitions, rules, flows and dashboards that go together for one kind of job. *Cold
rooms* and *Energy meters* come with this app, and *Download what is here* makes one to take
somewhere else, with no exports, secrets or tokens in it, because those belong to a site and a file
gets passed around. Installing one adds them as a person would, as drafts where there is such a
thing, and leaves alone any name already taken. So a flow from a file still goes through simulation
and review, and nothing runs by itself.

## Claiming

A device that says `{"secretKey": "…", "durationMs": 60000}` on `v1/devices/me/claim` can be claimed by
whoever holds it: `POST /devices/claim` with its code and that secret. Only a hash leaves the gateway.
It says who holds the device and nothing more: it still has to be commissioned before it takes work.

## Reports

Device Availability, Alarm History and Readings by the Hour are in the Reports page with the rest of
the workspace's reports, and can be exported from there.

## Customers

An **Edge Customer** names who owns what, and the people who may look. Set **Owned By** on a device and
**Customer** on a dashboard, and add the people as portal people of the workspace. They sign in to the
workspace's portal and find **Your devices**: how each is doing, its readings and alarms, and the
dashboards given to them, without their controls. They can acknowledge an alarm, and it says who did.
They cannot send a command, and a device of anyone else's is simply not there.

## Relations

An **Edge Relation** says any way two devices belong together that assets do not: this pump feeds that
tank, this meter measures that line. It shows on both devices, and `GET /devices?related=` finds them
from either end, which is enough to ask them all something at once. It moves nothing by itself, and
flows do not follow relations.

## Reports by the clock

An **Edge Report Schedule** runs a report at a time of day and sends it to the people it names. A mailer
that can attach sends the whole report as CSV, or PDF if this server has a renderer, and every mail shows
the first rows. The mail goes through the platform's outbox in one transaction with the note that it ran,
so a crash neither loses a report nor sends it twice. A report that cannot be made says why on the
schedule and is not tried again until its next day.

## Allowances

The operator of a server can give a workspace limits, in `tenant.quotas`: `measurements_per_day` and
`devices`. Past the first, gateways keep their readings, within their own budget, until the next day or
until it is raised, and commands, leases and reports go on: an allowance is never what stops a site. The
home page says when this is happening. Gateways before 0.3 do not know to keep them and lose those
readings, so upgrade them first.
