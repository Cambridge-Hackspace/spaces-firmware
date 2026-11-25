"""The faults the stand-in edge exists for, each checked without a broker.

Run with `python3 -m unittest discover tools/fake-edge` from the repository
root. A fault that quietly stopped being one -- a `partial` reply that grew an
`authorized` field -- would leave the bench test it serves passing for the
wrong reason.
"""

import json
import unittest

import fake_edge as fe


class Recorder:
    """Stands in for the MQTT client: keeps what was published."""

    def __init__(self):
        self.sent = []

    def publish(self, topic, body):
        self.sent.append((topic, body))

    def on(self, topic):
        return [body for t, body in self.sent if t == topic]


def edge():
    client = Recorder()
    return fe.FakeEdge(client, ttl_ms=3000, tool_uuid="uuid-1"), client


def scan(e):
    e.on_message(fe.SCAN_REQUEST, json.dumps({"resource": "laser", "card": "C"}).encode())


def heard_from(e, device="dev-1"):
    e.on_message(fe.ALIVE_REQUEST, json.dumps({"device_id": device}).encode())


class Faults(unittest.TestCase):
    def test_a_request_is_answered_yes_by_default(self):
        e, c = edge()
        scan(e)
        reply = json.loads(c.on(fe.SCAN_RESPONSE)[0])
        self.assertIs(reply["authorized"], True)
        self.assertEqual(reply["resource"], "laser")

    def test_partial_carries_no_authorized_field(self):
        e, c = edge()
        e.command("partial")
        scan(e)
        reply = json.loads(c.on(fe.SCAN_RESPONSE)[0])
        self.assertNotIn("authorized", reply)
        self.assertNotIn("laser", e.sessions, "a partial reply opened a session")

    def test_garbage_does_not_parse(self):
        e, c = edge()
        e.command("garbage")
        scan(e)
        with self.assertRaises(ValueError):
            json.loads(c.on(fe.SCAN_RESPONSE)[0])

    def test_ignore_answers_nothing_and_only_once(self):
        e, c = edge()
        e.command("ignore")
        scan(e)
        self.assertEqual(c.on(fe.SCAN_RESPONSE), [])
        scan(e)
        self.assertEqual(len(c.on(fe.SCAN_RESPONSE)), 1, "the fault outlived its request")

    def test_pause_stops_the_leases_and_resume_restarts_them(self):
        e, c = edge()
        heard_from(e)
        scan(e)
        e.renew()
        self.assertEqual(len(c.on(fe.LEASE)), 1)
        e.command("pause")
        e.renew()
        self.assertEqual(len(c.on(fe.LEASE)), 1, "leased while paused")
        e.command("resume")
        e.renew()
        self.assertEqual(len(c.on(fe.LEASE)), 2)

    def test_revoke_withdraws_the_grant_and_ends_the_session(self):
        e, c = edge()
        heard_from(e)
        scan(e)
        e.command("revoke")
        lease = json.loads(c.on(fe.LEASE)[-1])
        self.assertIs(lease["grant"], False)
        self.assertEqual(lease["device_id"], "dev-1")
        e.renew()
        self.assertEqual(len(c.on(fe.LEASE)), 1, "leased after a revoke")

    def test_no_lease_without_a_session(self):
        e, c = edge()
        heard_from(e)
        e.renew()
        self.assertEqual(c.on(fe.LEASE), [])



class Seals(unittest.TestCase):
    """What the fake edge seals (CS:S #249), so a module that signs believes
    it. The vectors are pinned against css_wire_sign too, in
    spaces-device/tests/signing.rs: two implementations, checked each against
    the other. The lease is FIRMWARE.md's own worked example."""

    KEY = "0a0b0c0d"

    def test_a_lease_signs_the_documented_bytes(self):
        lease = {"resource": "res-1", "device_id": "dev-1", "grant": True, "ttl_ms": 3000,
                 "reason": None, "member": {"id": "u-1", "name": "Cal"}}
        self.assertEqual(
            fe.canonical(fe.LEASE, lease, "n-edge", "n-mod", 7),
            b"11:css-local/114:resource/lease5:dev-16:n-edge5:n-mod1:75:res-14:true4:30003:u-13:Cal")
        self.assertEqual(
            fe.sealed(fe.LEASE, lease, self.KEY, "n-edge", "n-mod", 7)["seal"]["sig"],
            "7ae3d8164ad6034d6e8e3950a07adb684b52433806630c547da9d9ae8c56647f")

    def test_a_scan_answer_and_a_hello_sign_as_css_wire_does(self):
        reply = {"resource": "laser", "authorized": True, "reason": "authorized",
                 "strike": {"pulse_ms": 4200}, "device_id": "dev-1"}
        self.assertEqual(
            fe.sealed(fe.SCAN_RESPONSE, reply, self.KEY, "n-edge", "n-mod", 3)["seal"]["sig"],
            "62d02add2e706ed477cbe1029395c766cdc9f970f3da1baec75b8b36b62a65bb")
        self.assertEqual(
            fe.sealed(fe.HELLO, {"device_id": "dev-1"}, self.KEY, "n-edge", "n-mod", 1)["seal"]["sig"],
            "123b8574e1df9cbd6df884449596683324ad0a0c5094989d605252a89aad7920")

    def test_a_reason_is_not_signed_and_the_grant_is(self):
        lease = {"resource": "r", "device_id": "d", "grant": True, "ttl_ms": 1, "reason": None}
        sig = lambda m: fe.sealed(fe.LEASE, m, self.KEY, "a", "b", 1)["seal"]["sig"]
        self.assertEqual(sig(lease), sig({**lease, "reason": "anything"}))
        self.assertNotEqual(sig(lease), sig({**lease, "grant": False}))


def keyed_edge():
    client = Recorder()
    e = fe.FakeEdge(client, ttl_ms=3000, tool_uuid="uuid-1", keys={"dev-1": "k1"})
    e.sealers["dev-1"].mine = "edge-n"
    return e, client


class Signing(unittest.TestCase):
    def test_a_sealed_message_naming_no_edge_nonce_is_answered_with_a_hello(self):
        e, c = keyed_edge()
        e.on_message(fe.ALIVE_REQUEST, json.dumps(
            {"device_id": "dev-1", "seal": {"from": "mod-n", "to": "", "seq": 1, "sig": "x"}}).encode())
        hello = json.loads(c.on(fe.HELLO)[0])
        self.assertEqual(hello["seal"]["to"], "mod-n")
        self.assertEqual(hello["seal"]["from"], "edge-n")

    def test_a_message_naming_the_edges_nonce_gets_no_hello(self):
        e, c = keyed_edge()
        e.on_message(fe.ALIVE_REQUEST, json.dumps(
            {"device_id": "dev-1", "seal": {"from": "mod-n", "to": "edge-n", "seq": 2, "sig": "x"}}).encode())
        self.assertEqual(c.on(fe.HELLO), [])

    def test_leases_and_answers_are_sealed_for_a_keyed_device(self):
        e, c = keyed_edge()
        e.on_message(fe.ALIVE_REQUEST, json.dumps(
            {"device_id": "dev-1", "seal": {"from": "mod-n", "to": "edge-n", "seq": 1, "sig": "x"}}).encode())
        scan(e)
        e.renew()
        replies = [json.loads(b) for b in c.on(fe.SCAN_RESPONSE)]
        self.assertEqual([r.get("device_id") for r in replies], ["dev-1", None],
                         "one sealed copy for the keyed device, and the unsigned one")
        self.assertEqual(replies[0]["seal"]["to"], "mod-n")
        lease = json.loads(c.on(fe.LEASE)[0])
        self.assertEqual(lease["seal"]["to"], "mod-n")
        self.assertEqual(lease["seal"]["sig"], fe.sealed(
            fe.LEASE, {k: v for k, v in lease.items() if k != "seal"}, "k1",
            "edge-n", "mod-n", lease["seal"]["seq"])["seal"]["sig"])

    def test_unsigned_answers_with_no_seal_at_all(self):
        e, c = keyed_edge()
        e.command("unsigned")
        scan(e)
        replies = [json.loads(b) for b in c.on(fe.SCAN_RESPONSE)]
        self.assertEqual(len(replies), 1)
        self.assertNotIn("seal", replies[0])
        self.assertIs(replies[0]["authorized"], True)

    def test_no_keys_seals_nothing(self):
        e, c = edge()
        heard_from(e)
        scan(e)
        e.renew()
        self.assertNotIn("seal", json.loads(c.on(fe.LEASE)[0]))
        self.assertEqual(len(c.on(fe.SCAN_RESPONSE)), 1)
        self.assertEqual(c.on(fe.HELLO), [])


if __name__ == "__main__":
    unittest.main()
