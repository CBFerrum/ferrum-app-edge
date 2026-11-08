# Flows

A flow is logic drawn as steps and wires: when a number stays high, raise an alarm; when a device goes
quiet, tell someone; when a battery is low, ask for a charge. It runs at the gateway, so it goes on
working with the link down, and what it remembers survives a restart.

## What a flow cannot do

Node-RED and ThingsBoard's rule chains do this and more, and a plant floor is why this does less.

- **There is no code.** Every step is one of the kinds below. A flow can be read and reviewed, and
  nothing in one can do what its steps do not say: a step of another kind, or a field its kind does not
  take, is refused when the flow is read. Formulas are exact arithmetic with `round`, `abs`, `min`, `max`
  and `coalesce`, Ferrum's own formula engine, the server's too; there is no clock or random number in one.
- **It cannot loop.** A flow with a cycle is refused, and one message's work is bounded, so a bad flow
  costs a little time and cannot hang a gateway.
- **It cannot move anything.** The strongest thing a flow can do is ask for a command, which goes
  through the same ledger as a person's: the device must be commissioned for it, the site must hold
  authority, and it runs only under an automation envelope someone approved, or when a person says.
  With the link down such a request waits, and if it has waited longer than its `expires`, or its flow
  has since been stopped, it is not issued, and the device's history says why.
- **It is not a safety function.** A flow runs on a general-purpose computer in a process that can
  stop. Nothing that protects a person may depend on one. Interlocks, emergency stops and collision
  avoidance stay with the equipment and its safety system.

## Steps

**When**: a number, a state change, an event, an attribute, a timer. A device can be a code, the start
of one and a star (`PRESS-*`), or empty for any. Each device is watched on its own: a hold on `PRESS-1`
says nothing about `PRESS-2`.

**Decide**: compare, in range, must last, gone quiet, only changes, not too often, wait, count, average
over time, calculate, combine, all true, remember, translate, choose by value, replace, take a part,
has these parts, how far it went, in a place, only the first, one for each, rename parts, add what is
known, at these times, unlike the rest, make it another's, bring to units, by quality, average over the
time seen, an episode (the last four are for business-critical flows, below).

*Must last* has two inputs, and something must be wired to `reset`, or it holds for ever. *Combine* and
*all true* wait on inputs named by their wires, and can ignore values older than `fresh`, because a
stale reading is not a reading. Formulas use `+ - * /`, `round`, `abs`, `min`, `max` and `coalesce`,
with inputs in brackets: `[value] * 1.8 + 32`, in exact decimals. No answer is no message.

*How far it went* is a counter's rise since last time, and says nothing when one goes backwards,
because that is a reset and not a fall. Give it `per` to get a rate. *In a place* takes a circle or a
polygon, and says when a device enters and leaves as well as where it is; the first position is not
an arrival. *Unlike the rest* learns each device's normal from its own last readings, and an odd
reading does not teach it that faults are normal. It needs ten readings before it says anything, and
it is a statistic, not a diagnosis. *Add what is known* adds a device's newest numbers and attributes
to a message; a gateway knows what it has heard since it started. With `fresh` (`5m`) it adds only what the
device saw within that long, by the time the device gave; older, or of no known time, it is left out, so what a
site last heard before it lost its link is not taken for what is so now. *Make it another's* lets what
follows act for another device, so a sensor's reading can raise an alarm on the line it belongs to.
*Choose by value* can go by a part of the value (`$.method`) and pass on the whole of it. *Average over
time* takes `across: true` for a total over every device a flow hears, instead of one for each.

**Do**: raise an alarm, tell someone, ask for a command, record an event, record a number, set an
attribute, send elsewhere, answer the device, write to the log, open a case. An alarm must have something wired to
`clear`. *Tell someone* and *ask for
a command* happen at most once per `every`, a minute unless you say, however often they are reached,
so a chatty sensor cannot flood people or the ledger.

A device that asks a gateway something (`v1/devices/me/rpc/request/{id}`, or `POST .../rpc`) raises an
event called `rpc.request`, holding its `id`, `method` and `params`. A flow can choose by `$.method`
and *answer the device*, so a device that wants the time, a limit or a setting needs no platform.

## Business-critical flows

A flow marked *business-critical* (`"profile": "critical"`) is one whose decisions must survive a
power cut without being lost or made twice. Four things change for it.

- **Each reading knows when it was seen and how far it can be trusted.** A device that sends its own time
  is believed about it; one that does not is taken to have seen a reading when it arrived. A reading its
  device's limits call bad (a sensor fault, a value it cannot have, one stuck too long) reaches this flow,
  marked bad, where any other flow never hears it. *By quality* sends it on by how far it can be trusted,
  or as *stale* when it was seen too long ago.
- **A reading had already changes nothing.** The same reading delivered again, after a reconnect or a
  restart, is known by when it was seen and is left alone.
- **Where it stands and what it did are written together**, after every reading that leads to
  something: a power cut leaves both or neither. Before the cut nothing of that step was written and the
  reading is decided again, once; after it, the flow knows the reading. Other flows still write what they
  remember every 15 seconds.
- **Episodes become cases.** *An episode* is a condition that holds: it opens once the value has stayed
  past its line for *must hold for*, and closes once it has stayed back past a second line for *must stay
  clear for*; between the two lines nothing changes, so a value hovering at a limit is one episode, not
  fifty. Holding again soon after closing is the same episode, *reopened*. Only a good reading moves it:
  a bad one is evidence, and silence is said once as *stale* and is never a recovery. Its identity comes
  from the flow, the step, the device and when it began to hold, so the gateway names it the same however
  often it says so. *Open a case* hands it to the platform, which keeps one *Edge Condition* for each
  episode, each reading it was sent once, and says once that the condition opened, cleared, reopened or was
  interrupted. A process the workspace runs (Studio, Solutions) can start on *Edge condition opened* with
  the episode as its key: one case for each episode, whatever was said twice.

*Bring to units* turns raw counts into engineering units exactly (`value × scale + offset`), keeps the raw
value as its provenance, and sends what is not a number, or lands outside what it can be, to *quarantine*
instead of letting it pass as a reading. *Average over the time seen* is a statistic for each stretch of
the time readings were seen, not when they arrived: it waits *wait for stragglers* past a window's end
before saying anything, takes a late reading for *take corrections for* after that and says the corrected
result on *revised*, and says one later still on *late*. Wire *revised* to a record, not to a command: a
correction revises the evidence and does not repeat what the first result led to. A device quiet for *a
quiet device closes after* closes what it has open, marked partial.

What a flow keeps track of is bounded: a step remembers at most 5,000 devices and a flow 50,000 things in
all, and each one left out is said, so untrusted device codes cannot fill a gateway's memory.

Below its disk reserve a gateway keeps only alarms of its measurements and starts no new work, and says so
in its health; what flows decided, cases and requests among them, is always written. Given room again, it takes
up the work that waited.

## One flow inside another

A flow with *way into this flow* and *way out of this flow* steps can be used by others, with *use a
flow*, once it has been approved. Its steps are copied in when the flow is checked, so what a reviewer
simulates and a gateway runs is the whole of it, and a gateway has nothing to look up. If the flow
it uses changes after it was seen, the flow that uses it will not run until it has been drafted,
simulated and reviewed again, because what was seen would no longer be what runs.

## From a drawing to running

| Stage | What happens | Who |
|---|---|---|
| Draft | Can have faults, and reaches nothing | Edge Engineer |
| Simulated | A script is walked through it, by the code a gateway runs. It must do something | Edge Engineer |
| In review | It cannot change while someone is reading it | |
| Approved | By someone else, whatever roles the author also holds | Edge Reviewer |
| Running | At one site, which is how a change is tried, or at all of them | Edge Reviewer |

A new version needs simulating again: a simulation of something else proves nothing. Choosing *Change
it* makes a draft and leaves what is running alone, until the new one has been through the same.
Stopping is never a question of review: anyone who may work on flows may stop one. Each step is in the
audit trail.

Every version that ran is kept. *Draw from this* puts an old one on the canvas, which then goes
through the same review, because going back is a change like any other. A flow can be downloaded and
started from a file somewhere else, where it begins as a draft.

## Watching one run

A simulation shows what a flow would do. *Watch it run* shows what it is doing: every step a real
message reaches, with its device, its value and the way it left. It stops by itself after ten
minutes, an hour at most, because what passes through costs something and a person forgets. Nothing
is reported until someone asks, an Edge Engineer turns it on, and whoever can see the flow can read
what arrives. It is kept for an hour. A gateway reports it with each exchange, so it arrives in
seconds and not at once, a value over 2 KB is only said to have been long, and it is lost if the link
is down: a trace is for a person watching and is not worth slowing anything for.

## At the gateway

Flows arrive with the rest of a gateway's configuration, which takes `managed: true` in `site.yaml`.
A flow is fed everything a device reports that passed its quality limits, and the clock runs timers,
holds, silences and delays. What a flow decides is written to the journal before it counts, and
reaches the platform when the link allows.

What flows remember is written down every 15 seconds, and a business-critical flow's with each step
that does something. After a crash any other flow's hold or window may have lost up to 15 seconds, and
an effect from those seconds may happen again: an alarm is idempotent, and a message may repeat.

Whoever sets a new revision running chooses what it does with what the running one remembered. *Start
empty*: it remembers nothing, because its steps may mean something else now. *Drain first*: what is open
in the running one says what it has (a window its partial result, an episode that it was interrupted,
which is not that it cleared), then the new one starts empty. *Keep unchanged*: a step whose settings did
not change keeps what it remembered, and any other starts empty; the flow says which, before it runs and in
the gateway's notices.

## On the platform

A flow set to run on the platform sees across sites: the power of a whole fleet, or one line against
another a long way off. It goes through the same review and runs the same code, and it hears a device
with no gateway as well. Three things differ.

- It hears each closed minute, and a device's last good reading of that minute, and not each
  reading. Timers, holds and silences tick by the minute.
- It stops when the platform does, and catches up afterwards at an hour of its time each minute. It
  starts from the moment it is first run, and so does each new revision. What a gateway sends later
  than the minute has been through that gateway's flows and is not heard here; events and attributes
  go by when they arrived, so those are.
- *Answer the device* and *send elsewhere* need a gateway, and a flow with either is refused.

Anything that has to work with the link down belongs at a gateway. What it does is applied as the
fact a gateway would have sent, in one transaction with where it stands, so a crash repeats nothing
and loses nothing, and what it asks for goes to the same ledger with the same rules.
