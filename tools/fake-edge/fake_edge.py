#!/usr/bin/env python3
"""A fault injector that stands in for the Spaces edge, for breaking module
firmware on cue.

Normal bench runs use the real edge: its decisions -- which card is allowed,
which tool is in use, which module is alive -- are the platform's, and a copy of
them here would drift from the real thing, as this file once did when it
accepted a power report the real edge rejected (#10). So this keeps only the
minimum it takes to open a session, and the faults the real edge will not
produce on cue:

    pause       stop sending leases (as if the edge died or the network went)
    resume      send them again
    revoke      send grant: false for the open session, and end it
    ignore      do not answer the next request at all
    late        answer the next request with a yes, 8 s late
    partial     answer the next request with no `authorized` field at all
    garbage     answer the next request with something that is not JSON
    status      show what it thinks is going on
    quit

Every request is answered yes unless a fault says otherwise; a refusal is the
real edge's to give. While a session is open it leases every device it has
heard from, every second for three seconds.

Usage:

    fake_edge.py --broker 192.168.1.10

If the broker wants a login, give the username with --username and the
password in a file with --password-file (so it never appears in `ps` or in
your shell history):

    fake_edge.py --broker 192.168.1.10 --username edge \
        --password-file ~/.fake-edge-password

Needs paho-mqtt (pip install paho-mqtt). The tests beside it do not.
"""

import argparse
import json
import sys
import threading
import time

# The resource-shaped local wire (CS:S #182; names in CS:S's css_wire::local).
SCAN_REQUEST = "resource/request/scan"
STOP_REQUEST = "resource/request/stop"
USAGE_REQUEST = "resource/request/usage"
POWER_REPORT = "resource/request/power"
ALIVE_REQUEST = "resource/request/alive"
SCAN_RESPONSE = "resource/response/scan"
STOP_RESPONSE = "resource/response/stop"
USAGE_RESPONSE = "resource/response/usage"
POWER_RESPONSE = "resource/response/power"
LEASE = "resource/lease"

REQUESTS = (SCAN_REQUEST, STOP_REQUEST, USAGE_REQUEST, POWER_REPORT, ALIVE_REQUEST)
FAULTS = ("ignore", "late", "partial", "garbage")


def log(direction, text):
    stamp = time.strftime("%H:%M:%S") + f".{int(time.time() * 1000) % 1000:03d}"
    print(f"{stamp} {direction} {text}", flush=True)


class FakeEdge:
    def __init__(self, client, ttl_ms, tool_uuid, strike_ms=0):
        self.client = client
        self.ttl_ms = ttl_ms
        # The real edge sends the resource's UUID in a lease, not its external id.
        self.tool_uuid = tool_uuid
        self.strike_ms = strike_ms
        self.lock = threading.Lock()
        # Resources with an open session.
        self.sessions = set()
        # Devices heard from, by power report or alive pulse.
        self.devices = set()
        self.leasing = True
        self.next_fault = None

    # -- outgoing ---------------------------------------------------------

    def publish(self, topic, payload):
        body = payload if isinstance(payload, str) else json.dumps(payload)
        self.client.publish(topic, body)
        log(">>", f"{topic} {body}")

    def yes(self, resource):
        reply = {"resource": resource, "authorized": True, "reason": "authorized"}
        # A resource with a latch gets its pulse on a yes (CS:S #182).
        if self.strike_ms > 0:
            reply["strike"] = {"pulse_ms": self.strike_ms}
        return reply

    def lease(self, device, grant, reason=None):
        return {
            "resource": self.tool_uuid,
            "device_id": device,
            "grant": grant,
            "ttl_ms": self.ttl_ms,
            "reason": reason,
        }

    # -- incoming ---------------------------------------------------------

    def on_message(self, topic, raw):
        log("<<", f"{topic} {raw.decode(errors='replace')}")
        try:
            msg = json.loads(raw)
        except ValueError:
            log("!!", "that did not parse; ignored")
            return
        with self.lock:
            if topic == SCAN_REQUEST:
                self.scan(msg.get("resource"))
            elif topic == STOP_REQUEST:
                self.sessions.discard(msg.get("resource"))
                self.publish(STOP_RESPONSE, {"ok": True})
            elif topic == USAGE_REQUEST:
                self.publish(USAGE_RESPONSE, {"ok": True})
            elif topic in (POWER_REPORT, ALIVE_REQUEST):
                if msg.get("device_id"):
                    self.devices.add(msg["device_id"])
                if topic == POWER_REPORT:
                    self.publish(POWER_RESPONSE, {"ok": True})

    def scan(self, resource):
        fault, self.next_fault = self.next_fault, None
        if fault == "ignore":
            log("..", "not answering, as asked")
        elif fault == "late":
            log("..", "will answer with a yes in 8 s")
            threading.Timer(8.0, self.late_yes, args=(resource,)).start()
        elif fault == "partial":
            # The rule this breaks: only `authorized: true` is a yes.
            self.publish(SCAN_RESPONSE, {"resource": resource, "reason": "ok"})
        elif fault == "garbage":
            self.publish(SCAN_RESPONSE, "this is not JSON")
        else:
            self.sessions.add(resource)
            self.publish(SCAN_RESPONSE, self.yes(resource))

    def late_yes(self, resource):
        with self.lock:
            self.sessions.add(resource)
            self.publish(SCAN_RESPONSE, self.yes(resource))

    # -- the lease loop ---------------------------------------------------

    def renew(self):
        """One round of leases: every device heard from, while a session is
        open and leasing is not paused."""
        if not self.leasing or not self.sessions:
            return
        for device in sorted(self.devices):
            self.publish(LEASE, self.lease(device, True))

    def lease_loop(self, interval_ms):
        while True:
            time.sleep(interval_ms / 1000)
            with self.lock:
                self.renew()

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
                for device in sorted(self.devices):
                    self.publish(LEASE, self.lease(device, False, "revoked"))
                self.sessions.clear()
            elif word in FAULTS:
                self.next_fault = word
                log("..", f"the next request will get: {word}")
            elif word == "status":
                log("..", f"leasing={self.leasing} sessions={sorted(self.sessions)} "
                    f"devices={sorted(self.devices)} next={self.next_fault}")
            elif word:
                log("..", f"unknown command {word!r}; see the top of this file")


def main():
    import paho.mqtt.client as mqtt

    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--broker", default="127.0.0.1")
    parser.add_argument("--port", type=int, default=1883)
    parser.add_argument("--ttl-ms", type=int, default=3000)
    parser.add_argument("--interval-ms", type=int, default=1000)
    parser.add_argument("--tool-uuid", default="7c9e6679-7425-40de-944b-e07fc1f90ae7")
    parser.add_argument("--strike-ms", type=int, default=0,
                        help="answer a yes with a latch pulse of this many ms, as the "
                             "real edge does for a resource with a strike binding")
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
    edge = FakeEdge(client, args.ttl_ms, args.tool_uuid, args.strike_ms)

    def on_connect(client, _userdata, _flags, reason, _props):
        log("..", f"connected to {args.broker}:{args.port} ({reason})")
        if reason.is_failure:
            return
        # Subscribe on every connect: a clean session forgets subscriptions.
        for topic in REQUESTS:
            client.subscribe(topic)

    client.on_connect = on_connect
    client.on_message = lambda _c, _u, m: edge.on_message(m.topic, m.payload)
    client.connect(args.broker, args.port)
    client.loop_start()
    threading.Thread(target=edge.lease_loop, args=(args.interval_ms,), daemon=True).start()

    log("..", "answering yes to every request; type a fault, 'status', or see the top of this file")
    for line in sys.stdin:
        word = line.strip()
        if word == "quit":
            break
        edge.command(word)
    client.loop_stop()


if __name__ == "__main__":
    main()
