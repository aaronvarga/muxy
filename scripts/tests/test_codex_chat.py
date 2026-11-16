"""Trust-boundary and reconciliation checks for the isolated Codex gate."""

import importlib.util
import json
from pathlib import Path
import socket
import struct
import time
import unittest
from unittest.mock import patch


SPEC = importlib.util.spec_from_file_location(
    "codex_chat_gate", Path(__file__).resolve().parents[1] / "check-codex-chat.py"
)
gate = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(gate)


class TransportTests(unittest.TestCase):
    def setUp(self):
        self.client = gate.Client.__new__(gate.Client)
        self.client.socket, self.server = socket.socketpair()
        self.client.pending = []
        self.client.pending_bytes = 0
        self.addCleanup(self.client.close)
        self.addCleanup(self.server.close)

    def receive(self):
        return self.client.receive(time.monotonic() + 1)

    def test_fragmented_message_with_interleaved_ping(self):
        self.server.sendall(b'\x01\x06{"id":\x89\x01x\x80\x02\x37}')
        self.assertEqual(self.receive(), {"id": 7})
        pong = self.server.recv(7)
        self.assertEqual(pong[:2], b"\x8a\x81")
        self.assertEqual(pong[-1] ^ pong[2], ord("x"))

    def test_rejects_oversized_frame_before_reading_payload(self):
        self.server.sendall(b"\x81\x7f" + struct.pack("!Q", gate.MAX_MESSAGE + 1))
        with self.assertRaisesRegex(gate.GateError, "exceeds limit"):
            self.receive()

    def test_rejects_fragment_continuation_without_message(self):
        self.server.sendall(b"\x80\x02{}")
        with self.assertRaisesRegex(gate.GateError, "unexpected WebSocket"):
            self.receive()

    def test_reply_wait_keeps_early_approval_and_typed_id(self):
        approval = {"id": 7, "method": "item/commandExecution/requestApproval"}
        reply = {"id": "7", "result": {}}
        for message in (approval, reply):
            payload = json.dumps(message).encode()
            self.server.sendall(bytes([0x81, len(payload)]) + payload)
        self.assertEqual(self.client.wait(lambda msg: msg.get("id") == "7"), reply)
        self.assertEqual(self.client.wait(lambda msg: msg.get("method") is not None), approval)

    def test_rejects_unbounded_unmatched_notifications(self):
        payload = b'{"method":"noise"}'
        self.server.sendall((bytes([0x81, len(payload)]) + payload) * (gate.MAX_QUEUED + 1))
        with self.assertRaisesRegex(gate.GateError, "buffer overflow"):
            self.client.wait(lambda _: False)

    def test_rejects_byte_overflow_before_count_limit(self):
        self.client.pending_bytes = gate.MAX_QUEUED_BYTES
        self.server.sendall(b'\x81\x02{}')
        with self.assertRaisesRegex(gate.GateError, "byte buffer overflow"):
            self.client.wait(lambda _: False)

    def test_partial_frame_obeys_absolute_deadline(self):
        self.server.sendall(b'\x81\x7e\x00')
        with self.assertRaises(TimeoutError):
            self.client.receive(time.monotonic() + 0.01)


class PaginationTests(unittest.TestCase):
    def test_repeated_cursor_stops_resynchronization_loop(self):
        class Client:
            def request(self, *_):
                return {"data": [], "nextCursor": "unchanged"}

        with self.assertRaisesRegex(gate.GateError, "repeated its cursor"):
            gate.pages(Client(), "thread/turns/list", {"threadId": "thread"})

    def test_page_budget_stops_unbounded_distinct_cursors(self):
        class Client:
            def __init__(self):
                self.count = 0

            def request(self, *_):
                self.count += 1
                return {"data": [], "nextCursor": str(self.count)}

        with self.assertRaisesRegex(gate.GateError, "page budget"):
            gate.pages(Client(), "thread/turns/list", {"threadId": "thread"})


class IdentityTests(unittest.TestCase):
    def test_rejects_reused_pid_before_terminal_handoff(self):
        original = gate.ProcessIdentity(42, (1, 2), Path("/codex"))
        reused = gate.ProcessIdentity(42, (3, 4), Path("/codex"))
        with patch.object(gate.ProcessIdentity, "read", return_value=reused):
            with self.assertRaisesRegex(gate.GateError, "identity changed"):
                original.revalidate()


if __name__ == "__main__":
    unittest.main()
