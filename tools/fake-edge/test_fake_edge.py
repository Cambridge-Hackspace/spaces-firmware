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


if __name__ == "__main__":
    unittest.main()
