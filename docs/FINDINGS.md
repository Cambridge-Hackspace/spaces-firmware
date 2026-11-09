# What FIRMWARE.md did not say

> **Topic names below are history (0.4, CS:S #182).** This firmware now speaks
> the resource-shaped wire -- `resource/request/scan`, `resource/response/scan`,
> `resource/lease` and so on, every payload naming the `resource` -- and the
> `toolguard/*` names quoted in these notes are the edge's deprecated aliases
> for them. The findings themselves still hold: they are about the shapes, not
> the names.

This firmware was written from the platform's `FIRMWARE.md` alone, on purpose,
without reading the platform's source. The point was a fresh read: everything
a firmware author would have to guess, and everything they would get wrong by
following the document faithfully, is written down here for the platform's
maintainers. Where the document was silent or ambiguous, the questions went to
someone who knows the source (the platform's own agent, by way of a person),
and the answers are recorded with them.

The copy read was dated 2026-10-03, SHA-256 `6ad81a59b7e8e33a…`. Section names
below are that copy's.

The document is good. It is clear about what is safety-critical, it names each
common mistake where it happens, and the fail-safe rules it insists on (a lease
timed from receipt, `tool_on` exactly `true`, `relay_on` absent rather than
false) are all ones this firmware enforces and tests. What follows is the
short list of places it let a careful reader down.

## Wrong: following the document builds a broken module

These were confirmed against the source or against the running edge. A module
built faithfully from the document gets each of them wrong.

### 1. The edge does not answer tool-on with the server's envelope

*The local broker: edge ↔ module* says "the response topics carry the server's
envelope unchanged", and step 4 says to energize only on `tool_on: true`. The
reference edge's reply on `toolguard/response/tool-on` is
`{"authorized": true|false, "reason": "…"}`, with no `tool_on` at all.

A module following the document never starts a tool. That fails safe, but it
also means nobody could have shipped a working module from the document alone.

*What we did:* a yes is an explicit `authorized: true` or `tool_on: true`, and
an explicit `false` in either field wins. Everything else, including a missing
reply, is a no. Tested in `spaces-device/tests/module.rs`. The document's
envelope has since left the wire (CS:S #192), and the fault injector in
`tools/fake-edge` no longer speaks it; its `partial` command sends a reply with
no `authorized` at all, which is the case that still matters.

### 2. A module must not call `boot-reset`

*Step 3 — boot in the right order* tells every device to call
`POST /api/toolguard/boot-reset` first, then `module-state` and `power-state`.
`boot-reset` is **global**: it returns every tool the server believes is in use
to idle, not just this device's. A module that follows step 3 ends, and bills,
every running session in the building each time it restarts.

For a module behind an edge, those three calls are the edge's job. A module
makes exactly one HTTP request in its life: registration.

*What we did:* register, persist, then speak only to the local broker.

### 3. A lease's `tool_id` is a UUID

*Authorization is a lease* shows `"tool_id": "laser-01"` in a lease, and step 5
`"dev-tool-01"`: an external id, the same kind a module sends in `tool-on`. The
reference edge sends the tool's **UUID**. A module that matches leases on
`tool_id` against its configured external id never matches one, and never
stays on. (It fails safe, again, but silently.)

*What we did:* match leases on `device_id` only, as the document also says to,
and ignore `tool_id` in a lease.

### 4. "Periodically" means at least every few seconds

Step 6 says a power module should publish `toolguard/request/power`
"periodically", and *MQTT* describes a 15-second heartbeat. The edge counts a
module **offline after 5 seconds** without a power report (`module_offline_ms`)
and stops leasing it. A module reporting on the heartbeat's 15-second cadence
loses its lease between reports, every time.

*What we did:* a power report every second. It doubles as the module's
heartbeat, carries `device_id`, and reports `relay_on` truthfully.

### Also confirmed

- **Only a `power` binding gets leases.** *Step 2* says a metered tool needs a
  `power` binding. In fact any module that holds a relay must be bound in the
  `power` role, metered or not, or it is never leased: a `reader` binding can
  start a session, but nothing keeps it on.
- **The local broker is not the site broker,** and must not be. Sharing one
  would let modules read the site's topics; the site broker's ACL rightly
  refuses the local ones.

### 5. A power report's `draw_now` is a number on the local broker

*`POST /api/toolguard/power-report`* says "Decimal fields are **strings**, not
floats", with `"draw_now": "4.2"` in its example, and *The local broker* lists
`toolguard/request/power` with the same fields and no word on their types. A
module that reasonably carries the rule across sends `"draw_now": "0.0"`, and
the reference edge (the published `:dev` image, 2026-10-14) rejects it:

    WARN Failed to parse local power request: invalid type: string "0.0", expected f64

It drops the whole report, and the power report is the module's heartbeat, so
the module never counts as alive and is never leased. Nothing tells the
module: the edge still answers nothing, and the tool simply never stays on.
Found only on the first run against a real edge; the fake edge, written from
the document, accepted the string.

*What we did:* `draw_now` is a JSON number on the local broker. Tested in
`spaces-device/tests/module.rs`. The fake edge no longer judges reports at all:
normal bench runs use the real edge, and the fake one keeps only fault
injection (#10), so it cannot hide a difference like this again. `voltage_now`
is untried: this module does not send it. The document should give the local topic's types, or the
edge should accept both.

## Silent: what a module author has to guess

### 6. Who runs the local broker

The document says modules speak to "the local broker" and the edge is their
counterparty, but not whether the edge *is* the broker or connects to one, so
an author cannot tell where to point a module. It connects to one: the edge
expects a broker beside it (a mosquitto on the edge's own machine, here).
*MQTT* should say so, with the address an edge uses by default.

### 7. The local broker's security is the whole of it

Nothing on the local wire is signed. `command_key` covers the site broker's
commands only, and *The local broker* says its topics are "inside one
building". But any client that can publish `toolguard/lease` can keep any
relay on, and any client that can publish `toolguard/response/tool-on` can say
yes to anyone. The broker's logins and ACLs are the only thing in the way.

The document should say that plainly, and say what the ACL must be. The one
on this project's edge VM: the edge's login may read and write everything; a
module's may publish only `toolguard/request/#` and read only
`toolguard/response/#` and `toolguard/lease`. Checked: a module's login cannot
forge a lease or a reply, nor read another module's requests.

### 8. Replies carry nothing to say whose they are

`toolguard/response/tool-on` names no card, tool, device or request. With more
than one module on a broker, each sees every reply, and cannot tell its own
from a neighbour's: Alice's yes at one machine can switch on another where Bob
is waiting for his no. Confirmed as a real gap.

*What we did:* the module accepts a reply only while it has exactly one
request outstanding, for at most five seconds. That narrows the window; it
does not close it. A fix needs the protocol: echo the `card` and `tool_id` in
the reply, or better a request id the module chooses.

**FIXED UPSTREAM, 2026-10-23 — CS:S #166.** The edge now echoes `tool_id` in
the tool-on reply, which is what the paragraph above asked for, and
`ToolOnReply::is_for` drops a reply naming another tool before the verdict is
read. The bound is worth stating exactly: the protocol gap is closed **against
an edge carrying #166**, and an older edge sends no `tool_id` at all, where the
absent field falls back to the five-second single-request window above. So the
weaker guard is still load-bearing and stays. The door half of the same model
had always echoed `door_id`; the tool half simply never did.

Related: `toolguard/request/tool-on` carries no `device_id` either, so the edge
cannot tell which module asked, only which tool.

### 9. What happens when a lease lapses mid-session

The document is clear that silence means stop. It does not say whether the
session is then over, or whether a renewal arriving later may bring the tool
back. For a laser cutter, coming back on by itself after a network blip is
exactly what latching exists to prevent.

*What we did:* a lapse ends the session. The outputs go off, `tool-log` and
`tool-off` are sent with the card that started it, and a fresh swipe is
needed. A later renewal is ignored. Tested.

### 10. What `seconds` in `tool-log` counts

`seconds (float)`, and nothing else. For a metered tool it matters whether that
is the session's length or the time the machine actually worked.

*What we did:* the time the tool was energized *and* running (the demo's
running switch, standing in for a beam firing), counted on a monotonic clock
and stopped at the instant the outputs cut, not when the cut is noticed.

### 11. Device invites are emoji

*Registration* says `"device_code": "<invite code from an administrator>"`,
and nothing more. They are eight emoji: 24 to 56 bytes, 8 to 16 codepoints,
some with an invisible U+FE0F that matters. A firmware author has no reason to
expect that, and every natural first attempt is wrong: a fixed-size buffer,
a length check, a URL decoder that drops bytes, a page without a declared
character set, a password field that hides a bad paste.

*What we did:* no length checks, no normalising, UTF-8 declared in the page,
the form and the HTTP header, and a plain text field meant for pasting. Tested
with a real emoji invite, byte for byte.

### 12. A lost registration reply burns the invite

The document rightly says to persist the token before anything else. But if
the server accepts the invite and the reply is lost on the way back, the invite
is used and the device never learns its token; a retry is refused. No firmware
can fix that. The server could, by answering a repeat of the same invite from
the same MAC address with the same credentials.

### 13. Class 2 has no lease source

Leases come from an edge. A class 2 device has no edge, and the document does
not say what keeps its relay safe if the server goes away mid-session. The
lifecycle diagram mixes the two classes ("renew the lease" beside a direct
`POST /tool-on`). Not a problem for this module, which is class 1, but a trap
for the next author.

### 14. Whether the server ever sees a module

The server watches `last_seen`, updated by heartbeats on the site broker,
which a module should not use. So a module's liveness reaches the edge (by its
power reports) but, as far as the document says, never the server. If the
edge forwards it, the document should say so.

## Inconsistent: small things

15. **Two boot orders.** *Step 3* says `boot-reset` → `module-state` →
    `power-state`; the lifecycle diagram says `boot-reset` → `sync` →
    `power-state` → `module-state`. (Moot for a module after finding 2, but an
    edge author has to pick one.)
16. **`tool_id` means two things.** An external id in requests and in the
    lease example, a UUID in `module-state` and in real leases. The document
    says to match both, which every author then has to do everywhere; after
    finding 3, it is worth saying which payload carries which.
17. **"Carry no `device_id`".** *The local broker* says its topics carry no
    `device_id`, and then lists it in `toolguard/request/power` and
    `toolguard/lease`, where it is essential.
18. **A stale phrase.** `tool-off` "answers `status: "error"` … for … a bad
    key", but keys were retired; the document's own checker covers literal
    denial strings, not prose.
19. **A stray 422.** `power-report` says an unauthenticated request gets
    "401, not 422", but nothing else in the document returns 422, so it reads
    as a warning about a bug that is not described.
