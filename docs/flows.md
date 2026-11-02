# Flows

A flow is logic drawn as steps and wires: when a number stays high, raise an alarm; when a device goes
quiet, tell someone; when a battery is low, ask for a charge. It runs at the gateway, so it goes on
working with the link down, and what it remembers survives a restart.

## What a flow cannot do

Node-RED and ThingsBoard's rule chains do this and more, and a plant floor is why this does less.

- **There is no code.** Every step is one of the kinds below. A flow can be read and reviewed, and
  nothing in one can do what its steps do not say.
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
known, at these times, unlike the rest, make it another's.

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
to a message; a gateway knows what it has heard since it started. *Make it another's* lets what
follows act for another device, so a sensor's reading can raise an alarm on the line it belongs to.
*Choose by value* can go by a part of the value (`$.method`) and pass on the whole of it. *Average over
time* takes `across: true` for a total over every device a flow hears, instead of one for each.

**Do**: raise an alarm, tell someone, ask for a command, record an event, record a number, set an
attribute, send elsewhere, answer the device, write to the log. An alarm must have something wired to
`clear`. *Tell someone* and *ask for
a command* happen at most once per `every`, a minute unless you say, however often they are reached,
so a chatty sensor cannot flood people or the ledger.

A device that asks a gateway something (`v1/devices/me/rpc/request/{id}`, or `POST .../rpc`) raises an
event called `rpc.request`, holding its `id`, `method` and `params`. A flow can choose by `$.method`
and *answer the device*, so a device that wants the time, a limit or a setting needs no platform.

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

What flows remember is written down every 15 seconds. After a crash a hold or window may have lost up
to that much, and an effect from those seconds may happen again: an alarm is idempotent, and a
message may repeat. A new revision starts with nothing remembered, because its steps may mean
something else now.

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
