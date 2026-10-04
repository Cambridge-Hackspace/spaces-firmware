#!/usr/bin/env python3
"""A stand-in for the Spaces edge coordinator, for testing module firmware on a
bench.

It speaks the local-broker side of the protocol the way the reference edge
does: it answers tool-on requests with {"authorized", "reason"}, acknowledges
tool-off, tool-log and power reports with {"ok": true}, and while a session is
open it leases the module bound to that tool every second for three seconds.
Like the real edge, it only leases a module it has heard a power report from in
the last five seconds.

What it does not do is talk to a server. Cards are allowed or refused from the
command line, so it can be run with nothing but a broker.

Its real use is breaking things on cue. Type a command while it runs:

    pause       stop sending leases (as if the edge died or the network went)
    resume      start sending them again
    revoke      send grant: false for the open session, and end it
    ok          answer the next request with {"status": "ok"} and no yes
    ignore      do not answer the next request at all
    late        answer the next request with a yes, 8 s late
    doc         answer in the format FIRMWARE.md describes, not the edge's
    edge        answer in the edge's format again (the default)
    status      show what it thinks is going on
    quit

Usage:

    fake_edge.py --broker 192.168.1.10 --allow ALICE-CARD

If the broker wants a login, give the username with --username and the
password in a file with --password-file (so it never appears in `ps` or in
your shell history):

    fake_edge.py --broker 192.168.1.10 --username edge \
        --password-file ~/.fake-edge-password --allow ALICE-CARD

Needs paho-mqtt (pip install paho-mqtt).
"""

import argparse
import json
import sys
import threading
import time

import paho.mqtt.client as mqtt

TOOL_ON_REQUEST = "toolguard/request/tool-on"
TOOL_OFF_REQUEST = "toolguard/request/tool-off"
TOOL_LOG_REQUEST = "toolguard/request/tool-log"
POWER_REPORT = "toolguard/request/power"
TOOL_ON_RESPONSE = "toolguard/response/tool-on"
TOOL_OFF_RESPONSE = "toolguard/response/tool-off"
TOOL_LOG_RESPONSE = "toolguard/response/tool-log"
POWER_RESPONSE = "toolguard/response/power"
LEASE = "toolguard/lease"


def log(direction, text):
    stamp = time.strftime("%H:%M:%S") + f".{int(time.time() * 1000) % 1000:03d}"
    print(f"{stamp} {direction} {text}", flush=True)


class FakeEdge:
    def __init__(self, client, allow, ttl_ms, interval_ms, offline_ms, tool_uuid):
        self.client = client
        self.allow = set(allow)
        self.ttl_ms = ttl_ms
        self.interval_ms = interval_ms
        self.offline_ms = offline_ms
        # The real edge sends the tool's UUID in a lease, not its external id.
        self.tool_uuid = tool_uuid
        self.lock = threading.Lock()
        # tool_id -> card, for the open session on each tool
        self.sessions = {}
        # device_id -> (tool_id, monotonic seconds of its last power report)
        self.modules = {}
        self.leasing = True
        self.next_reply = None  # None, "ok", "ignore", "late"
        self.reply_format = "edge"

    # -- outgoing ---------------------------------------------------------

    def publish(self, topic, payload):
        body = json.dumps(payload)
        self.client.publish(topic, body)
        log(">>", f"{topic} {body}")

    def yes(self):
        if self.reply_format == "doc":
            return {"status": "ok", "tool_on": True}
        return {"authorized": True, "reason": "authorized"}

    def no(self, reason):
        if self.reply_format == "doc":
            return {"status": "error", "message": reason, "tool_on": False}
        return {"authorized": False, "reason": reason}

    # -- incoming ---------------------------------------------------------

    def on_message(self, topic, raw):
        log("<<", f"{topic} {raw.decode(errors='replace')}")
        try:
            msg = json.loads(raw)
        except ValueError:
            log("!!", "that did not parse; ignored")
            return
        with self.lock:
            if topic == TOOL_ON_REQUEST:
                self.tool_on(msg)
            elif topic == TOOL_OFF_REQUEST:
                self.sessions.pop(msg.get("tool_id"), None)
                self.publish(TOOL_OFF_RESPONSE, {"ok": True})
            elif topic == TOOL_LOG_REQUEST:
                self.publish(TOOL_LOG_RESPONSE, {"ok": True})
            elif topic == POWER_REPORT:
                device = msg.get("device_id")
                if device:
                    self.modules[device] = (msg.get("tool_id"), time.monotonic())
                self.publish(POWER_RESPONSE, {"ok": True})

    def tool_on(self, msg):
        card, tool = msg.get("card"), msg.get("tool_id")
        override, self.next_reply = self.next_reply, None
        if override == "ignore":
            log("..", f"not answering the request for {card}, as asked")
            return
        if override == "ok":
            self.publish(TOOL_ON_RESPONSE, {"status": "ok"})
            return
        if override == "late":
            log("..", f"will answer {card} with a yes in 8 s")
            threading.Timer(8.0, self.late_yes, args=(card, tool)).start()
            return
        if tool in self.sessions:
            self.publish(TOOL_ON_RESPONSE, self.no("Tool is already in use"))
        elif card not in self.allow:
            self.publish(TOOL_ON_RESPONSE, self.no("Unknown card"))
        else:
            self.sessions[tool] = card
            self.publish(TOOL_ON_RESPONSE, self.yes())

    def late_yes(self, card, tool):
        with self.lock:
            self.sessions[tool] = card
            self.publish(TOOL_ON_RESPONSE, self.yes())

    # -- the lease loop ---------------------------------------------------

    def lease_loop(self):
        while True:
            time.sleep(self.interval_ms / 1000)
            with self.lock:
                if not self.leasing:
                    continue
                now = time.monotonic()
                for device, (tool, seen) in self.modules.items():
                    if tool not in self.sessions:
                        continue
                    if (now - seen) * 1000 > self.offline_ms:
                        # A module the edge has not heard from is not leased.
                        continue
                    self.publish(LEASE, {
                        "tool_id": self.tool_uuid,
                        "device_id": device,
                        "grant": True,
                        "ttl_ms": self.ttl_ms,
                        "reason": None,
                    })

    # -- commands ---------------------------------------------------------

    def command(self, word):
        with self.lock:
            if word == "pause":
                self.leasing = False
                log("..", "leases paused; modules should switch off within the TTL")
            elif word == "resume":
                self.leasing = True
                log("..", "leases resumed")
            elif word == "revoke":
                for device, (tool, _) in self.modules.items():
                    if tool in self.sessions:
                        self.publish(LEASE, {
                            "tool_id": self.tool_uuid, "device_id": device,
                            "grant": False, "ttl_ms": self.ttl_ms, "reason": "revoked",
                        })
                        self.sessions.pop(tool, None)
            elif word in ("ok", "ignore", "late"):
                self.next_reply = word
                log("..", f"the next request will get: {word}")
            elif word in ("doc", "edge"):
                self.reply_format = word
                log("..", f"answering in the {word} format")
            elif word == "status":
                now = time.monotonic()
                log("..", f"leasing={self.leasing} format={self.reply_format} "
                    f"sessions={self.sessions} modules="
                    f"{ {d: (t, round(now - s, 1)) for d, (t, s) in self.modules.items()} }")
            elif word:
                log("..", f"unknown command {word!r}; see the top of this file")


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--broker", default="127.0.0.1")
    parser.add_argument("--port", type=int, default=1883)
    parser.add_argument("--allow", action="append", default=[],
                        help="a card to authorize; repeat for more")
    parser.add_argument("--ttl-ms", type=int, default=3000)
    parser.add_argument("--interval-ms", type=int, default=1000)
    parser.add_argument("--offline-ms", type=int, default=5000)
    parser.add_argument("--tool-uuid", default="7c9e6679-7425-40de-944b-e07fc1f90ae7")
    parser.add_argument("--username")
    parser.add_argument("--password-file",
                        help="a file whose first line is the broker password")
    args = parser.parse_args()

    client = mqtt.Client(mqtt.CallbackAPIVersion.VERSION2, client_id="fake-edge")
    if args.username:
        password = None
        if args.password_file:
            with open(args.password_file) as f:
                password = f.readline().rstrip("\n")
        client.username_pw_set(args.username, password)
    edge = FakeEdge(client, args.allow, args.ttl_ms, args.interval_ms,
                    args.offline_ms, args.tool_uuid)

    def on_connect(client, _userdata, _flags, reason, _props):
        log("..", f"connected to {args.broker}:{args.port} ({reason})")
        if reason.is_failure:
            return
        # Subscribe on every connect: a clean session forgets subscriptions.
        for topic in (TOOL_ON_REQUEST, TOOL_OFF_REQUEST, TOOL_LOG_REQUEST, POWER_REPORT):
            client.subscribe(topic)

    client.on_connect = on_connect
    client.on_message = lambda _c, _u, m: edge.on_message(m.topic, m.payload)
    client.connect(args.broker, args.port)
    client.loop_start()
    threading.Thread(target=edge.lease_loop, daemon=True).start()

    log("..", f"allowing {sorted(edge.allow) or 'no cards'}; type 'status' or see the top of this file")
    for line in sys.stdin:
        word = line.strip()
        if word == "quit":
            break
        edge.command(word)
    client.loop_stop()


if __name__ == "__main__":
    main()
