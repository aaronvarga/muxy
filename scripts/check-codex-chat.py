#!/usr/bin/env python3
"""Probe shared-daemon Codex compatibility without touching real sessions.

Uses an owned Unix app-server, two WebSocket clients and a local Responses API
fixture. No credentials, user configuration, real model requests or dependencies.
An unsuccessful probe exits nonzero. This is evidence for part of the gate,
not verification of Muxy's target resolver, standalone recovery or signed-in UI.
"""

import argparse
import base64
from contextlib import ExitStack, contextmanager
import ctypes
from dataclasses import dataclass
import fcntl
import hashlib
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import os
from pathlib import Path
import pty
import re
import selectors
import shlex
import shutil
import socket
import struct
import subprocess
import sys
import tempfile
import termios
import threading
import time


MAX_MESSAGE = 16 << 20
MAX_QUEUED = 256
MAX_QUEUED_BYTES = 4 << 20
TIMEOUT = 20


class GateError(Exception):
    pass


def require(condition, detail):
    if not condition:
        raise GateError(detail)


@dataclass(frozen=True)
class ProcessIdentity:
    pid: int
    started: tuple
    executable: Path

    @classmethod
    def read(cls, pid):
        require(sys.platform == "darwin", "native process identity probe requires macOS")
        library = ctypes.CDLL("/usr/lib/libproc.dylib", use_errno=True)
        info = ctypes.create_string_buffer(136)
        library.proc_pidinfo.argtypes = [ctypes.c_int, ctypes.c_int, ctypes.c_uint64,
                                        ctypes.c_void_p, ctypes.c_int]
        library.proc_pidpath.argtypes = [ctypes.c_int, ctypes.c_void_p, ctypes.c_uint32]
        require(library.proc_pidinfo(pid, 3, 0, info, len(info)) == len(info),
                "cannot establish native process start time")
        started = struct.unpack_from("=QQ", info.raw, 120)
        executable = ctypes.create_string_buffer(4096)
        require(library.proc_pidpath(pid, executable, len(executable)) > 0,
                "cannot establish native executable")
        return cls(pid, started, Path(os.fsdecode(executable.value)).resolve(strict=True))

    def revalidate(self):
        require(self == self.read(self.pid), "process identity changed before handoff")


class Client:
    """Bounded, synchronous test-only WebSocket transport over a Unix socket."""

    def __init__(self, endpoint):
        self.socket = socket.socket(socket.AF_UNIX)
        self.socket.settimeout(TIMEOUT)
        self.pending = []
        self.pending_bytes = 0
        self.next_id = 0
        self.deadline = time.monotonic() + TIMEOUT
        try:
            self.socket.connect(str(endpoint))
            # Darwin's LOCAL_PEERPID ties this socket to the captured engine process.
            self.peer_pid = struct.unpack("=i", self.socket.getsockopt(0, 2, 4))[0]
            key = base64.b64encode(os.urandom(16)).decode()
            self.socket.sendall((
                "GET / HTTP/1.1\r\nHost: localhost\r\nUpgrade: websocket\r\n"
                f"Connection: Upgrade\r\nSec-WebSocket-Key: {key}\r\n"
                "Sec-WebSocket-Version: 13\r\n\r\n"
            ).encode())
            headers = bytearray()
            while not headers.endswith(b"\r\n\r\n"):
                require(len(headers) < 8192, "WebSocket headers exceed limit")
                headers.extend(self.exact(1))
            lines = headers.decode("ascii").split("\r\n")
            expected = base64.b64encode(hashlib.sha1(
                (key + "258EAFA5-E914-47DA-95CA-C5AB0DC85B11").encode()
            ).digest()).decode()
            fields = dict(line.split(":", 1) for line in lines[1:] if ":" in line)
            fields = {k.lower(): v.strip() for k, v in fields.items()}
            require(lines[0].startswith("HTTP/1.1 101 "), "WebSocket upgrade rejected")
            require(fields.get("sec-websocket-accept") == expected,
                    "WebSocket endpoint returned an invalid challenge")
        except BaseException:
            self.close()
            raise

    def close(self):
        self.socket.close()

    def exact(self, count):
        result = bytearray()
        while len(result) < count:
            remaining = self.deadline - time.monotonic()
            require(remaining > 0, "native frame read timed out")
            self.socket.settimeout(remaining)
            chunk = self.socket.recv(count - len(result))
            require(chunk, "app-server disconnected")
            result.extend(chunk)
        return bytes(result)

    def frame(self, payload, opcode=1):
        require(len(payload) <= MAX_MESSAGE, "outbound message exceeds limit")
        header = bytes([0x80 | opcode])
        size = len(payload)
        if size < 126:
            header += bytes([0x80 | size])
        elif size <= 65535:
            header += bytes([0x80 | 126]) + struct.pack("!H", size)
        else:
            header += bytes([0x80 | 127]) + struct.pack("!Q", size)
        mask = os.urandom(4)
        self.socket.settimeout(TIMEOUT)
        self.socket.sendall(header + mask + bytes(
            byte ^ mask[index % 4] for index, byte in enumerate(payload)
        ))

    def send(self, message):
        self.frame(json.dumps(message).encode())

    def receive(self, deadline):
        self.deadline = deadline
        parts = bytearray()
        started = False
        while True:
            remaining = deadline - time.monotonic()
            require(remaining > 0, "timed out waiting for native event")
            self.socket.settimeout(remaining)
            first, second = self.exact(2)
            opcode, final = first & 15, bool(first & 0x80)
            require(not first & 0x70 and not second & 0x80,
                    "unsupported WebSocket flags")
            size = second & 127
            if size == 126:
                size = struct.unpack("!H", self.exact(2))[0]
            elif size == 127:
                size = struct.unpack("!Q", self.exact(8))[0]
            require(size <= MAX_MESSAGE - len(parts), "native message exceeds limit")
            if opcode >= 8:
                require(final and size <= 125, "invalid WebSocket control frame")
            payload = self.exact(size)
            if opcode == 9:
                self.frame(payload, 10)
                continue
            if opcode == 10:
                continue
            require(opcode != 8, "app-server closed WebSocket")
            require((opcode == 1 and not started) or (opcode == 0 and started),
                    "unexpected WebSocket frame")
            started = True
            parts.extend(payload)
            if final:
                message = json.loads(parts)
                require(isinstance(message, dict), "invalid native envelope")
                return message

    def wait(self, predicate, timeout=TIMEOUT):
        for index, message in enumerate(self.pending):
            if predicate(message):
                self.pending_bytes -= len(json.dumps(message).encode())
                return self.pending.pop(index)
        deadline = time.monotonic() + timeout
        while True:
            message = self.receive(deadline)
            if predicate(message):
                return message
            require(len(self.pending) < MAX_QUEUED, "native event buffer overflow")
            size = len(json.dumps(message).encode())
            require(self.pending_bytes + size <= MAX_QUEUED_BYTES, "native event byte buffer overflow")
            self.pending.append(message)
            self.pending_bytes += size

    def request(self, method, params):
        self.next_id += 1
        request_id = f"muxy-gate-{self.next_id}"
        self.send({"id": request_id, "method": method, "params": params})
        reply = self.wait(lambda msg: msg.get("id") == request_id and "method" not in msg)
        # Native errors can include context; never print arbitrary provider content.
        require("error" not in reply, f"{method} rejected (code {reply.get('error', {}).get('code')})")
        require("result" in reply, f"{method} has no result")
        return reply["result"]

    def initialize(self, home):
        result = self.request("initialize", {
            "clientInfo": {"name": "muxy_compatibility_gate", "version": "1"},
            "capabilities": {"experimentalApi": True},
        })
        require(Path(result["codexHome"]).resolve() == home.resolve(),
                "endpoint belongs to another CODEX_HOME")
        self.send({"method": "initialized"})
        return result

    def event(self, method, thread_id):
        return self.wait(lambda msg: msg.get("method") == method
                         and msg.get("params", {}).get("threadId") == thread_id)


class StdioClient(Client):
    def __init__(self, executable, env, project):
        self.pending, self.pending_bytes, self.next_id = [], 0, 0
        self.buffer = bytearray()
        self.process = subprocess.Popen([executable, "app-server", "--stdio"], env=env, cwd=project,
            stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, bufsize=0, start_new_session=True)

    def send(self, message):
        self.process.stdin.write(json.dumps(message).encode() + b"\n")

    def receive(self, deadline):
        with selectors.DefaultSelector() as selector:
            selector.register(self.process.stdout, selectors.EVENT_READ)
            while True:
                if b"\n" in self.buffer:
                    line, _, tail = self.buffer.partition(b"\n")
                    self.buffer = bytearray(tail)
                    return json.loads(line)
                require(len(self.buffer) <= MAX_MESSAGE, "stdio native message exceeds limit")
                remaining = deadline - time.monotonic()
                require(remaining > 0 and selector.select(remaining), "stdio native request timed out")
                chunk = os.read(self.process.stdout.fileno(), 65536)
                require(chunk, "stdio app-server disconnected")
                self.buffer.extend(chunk)

    def close(self):
        if self.process.poll() is None:
            self.process.stdin.close()
            try:
                self.process.wait(timeout=3)
            except subprocess.TimeoutExpired:
                self.process.terminate()
                try:
                    self.process.wait(timeout=3)
                except subprocess.TimeoutExpired:
                    self.process.kill()
                    self.process.wait(timeout=3)
        self.process.stdout.close()


class Fixture(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, *_):
        pass

    def do_GET(self):
        # Explicitly request HTTP streaming instead of a model WebSocket upgrade.
        self.send_error(426)

    def do_POST(self):
        size = int(self.headers.get("Content-Length", "0"))
        if not 0 < size <= MAX_MESSAGE:
            self.send_error(413)
            return
        body = json.loads(self.rfile.read(size))
        with self.server.lock:
            self.server.requests += 1
            number = self.server.requests
            approval = not self.server.approval_sent and "Synthetic approval gate." in json.dumps(body)
            if approval:
                self.server.approval_sent = True
        if approval:
            tools = {tool.get("name") for tool in body.get("tools", [])}
            name = next((name for name in ("exec_command", "shell_command", "shell")
                         if name in tools), None)
            if name is None:
                self.send_error(422)
                return
            arguments = {"sandbox_permissions": "require_escalated",
                         "justification": "Synthetic Muxy compatibility approval"}
            command = f"/usr/bin/touch {shlex.quote(str(self.server.marker))}"
            if name == "exec_command":
                arguments["cmd"] = command
            elif name == "shell_command":
                arguments["command"] = command
            else:
                arguments["command"] = ["/usr/bin/touch", str(self.server.marker)]
            item = {"type": "function_call", "id": "fc_muxy_gate",
                    "call_id": "call_muxy_gate", "name": name,
                    "arguments": json.dumps(arguments)}
        else:
            item = {"type": "message", "id": f"msg_muxy_gate_{number}",
                    "role": "assistant", "status": "completed",
                    "content": [{"type": "output_text", "text": "Gate complete."}]}
        response = {"id": f"resp_muxy_gate_{number}", "object": "response",
                    "status": "completed", "output": [item],
                    "usage": {"input_tokens": 1, "output_tokens": 1, "total_tokens": 2}}
        events = [
            {"type": "response.created", "response": {**response, "status": "in_progress", "output": []}},
            {"type": "response.output_item.done", "output_index": 0, "item": item},
            {"type": "response.completed", "response": response},
        ]
        payload = "".join(f"event: {event['type']}\ndata: {json.dumps(event)}\n\n"
                          for event in events).encode()
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.send_header("Content-Length", str(len(payload)))
        self.end_headers()
        self.wfile.write(payload)


@contextmanager
def engine(executable, root):
    home = root / "home"
    home.mkdir()
    codex_home = home / ".codex"
    codex_home.mkdir()
    project = root / "project"
    project.mkdir()
    fixture = ThreadingHTTPServer(("127.0.0.1", 0), Fixture)
    fixture.daemon_threads = True
    fixture.requests = 0
    fixture.approval_sent = False
    fixture.marker = project / "declined-command-ran"
    fixture.lock = threading.Lock()
    worker = threading.Thread(target=fixture.serve_forever, daemon=True)
    worker.start()
    (codex_home / "config.toml").write_text(
        'model = "gpt-5.1-codex-max"\n'
        f'openai_base_url = "http://127.0.0.1:{fixture.server_port}/v1"\n'
        '[features]\nplugins = false\nplugin_discovery_prefetch = false\n'
        'apps = false\nhooks = false\nshell_snapshot = false\n'
    )
    (codex_home / "auth.json").write_text(json.dumps({
        "auth_mode": "apikey", "OPENAI_API_KEY": "muxy-local-fixture",
    }))
    (codex_home / "auth.json").chmod(0o600)
    endpoint = root / "server.sock"
    # Do not inherit auth, backend, hooks, proxy or configuration environment.
    env = {key: os.environ[key] for key in ("PATH", "TMPDIR", "LANG") if key in os.environ}
    env.update(HOME=str(home), CODEX_HOME=str(codex_home))
    process = None
    try:
        with (root / "server.stderr").open("wb") as stderr:
            process = subprocess.Popen(
                [executable, "app-server", "--listen", f"unix://{endpoint}"],
                cwd=project, env=env, stdin=subprocess.DEVNULL,
                stdout=subprocess.DEVNULL, stderr=stderr, start_new_session=True,
            )
            deadline = time.monotonic() + TIMEOUT
            while not endpoint.exists():
                require(process.poll() is None, "isolated app-server exited during startup")
                require(time.monotonic() < deadline, "isolated app-server startup timed out")
                time.sleep(0.02)
            yield endpoint, codex_home, project, process, env
    finally:
        if process is not None and process.poll() is None:
            process.terminate()
            try:
                process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait(timeout=5)
        fixture.shutdown()
        fixture.server_close()
        worker.join(timeout=5)


def pages(client, method, params):
    result, seen, cursor = [], set(), None
    for _ in range(16):
        page = client.request(method, {**params, "limit": 1, "cursor": cursor})
        require(len(page["data"]) <= 1, f"{method} ignored page size")
        result.extend(page["data"])
        cursor = page.get("nextCursor")
        if cursor is None:
            return result
        require(cursor not in seen, f"{method} repeated its cursor")
        seen.add(cursor)
    raise GateError(f"{method} exceeded page budget")


@contextmanager
def console(executable, endpoint, project, env, thread_id, marker):
    """Start only an isolated TUI; stopping it must not stop its shared engine."""
    master, slave = pty.openpty()
    fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 40, 120, 0, 0))
    process = None
    try:
        process = subprocess.Popen(
            [executable, *( ["--remote", f"unix://{endpoint}"] if endpoint else ["--no-daemon"]), "resume", thread_id],
            cwd=project, env={**env, "TERM": "xterm-256color"},
            stdin=slave, stdout=slave, stderr=slave, start_new_session=True,
            preexec_fn=lambda: fcntl.ioctl(0, termios.TIOCSCTTY, 0),
        )
        os.close(slave)
        slave = None
        output = bytearray()
        ready, stop = threading.Event(), threading.Event()
        errors = []

        def pump():
            try:
                with selectors.DefaultSelector() as selector:
                    selector.register(master, selectors.EVENT_READ)
                    while not stop.is_set():
                        if not selector.select(timeout=0.05):
                            continue
                        chunk = os.read(master, 65536)
                        require(chunk, "isolated Console closed its terminal")
                        require(len(output) + len(chunk) <= MAX_MESSAGE, "Console output exceeds limit")
                        output.extend(chunk)
                        if b"\x1b[6n" in chunk:
                            os.write(master, b"\x1b[1;1R")
                        if b"\x1b]11;?" in chunk:
                            os.write(master, b"\x1b]11;rgb:0000/0000/0000\x1b\\")
                        if not ready.is_set():
                            plain = re.sub(rb"\x1b\[[0-?]*[ -/]*[@-~]", b"", bytes(output))
                            if marker.encode() in re.sub(rb"\s+", b"", plain) and b"gpt-5.1-codex-max" in plain:
                                ready.set()
            except (GateError, OSError) as error:
                if not stop.is_set():
                    errors.append(error)
                    ready.set()

        reader = threading.Thread(target=pump, daemon=True)
        reader.start()
        require(ready.wait(timeout=TIMEOUT), "isolated Console did not load native history")
        require(not errors, "isolated Console terminal reader failed")
        require(process.poll() is None, "isolated Console exited before loading history")
        identity = ProcessIdentity.read(process.pid)
        yield process, master, identity
    finally:
        if process is not None and process.poll() is None:
            process.terminate()
            try:
                process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait(timeout=5)
        if "stop" in locals():
            stop.set()
        if "reader" in locals():
            reader.join(timeout=5)
        os.close(master)
        if slave is not None:
            os.close(slave)


def run(executable, report, rust_native_test=False):
    with tempfile.TemporaryDirectory(prefix="mxgate-", dir="/tmp") as temporary:
        root = Path(temporary).resolve()
        with engine(executable, root) as (endpoint, home, project, process, env), ExitStack() as stack:
            def connect():
                client = Client(endpoint)
                stack.callback(client.close)
                client.initialize(home)
                require(client.peer_pid == process.pid, "endpoint belongs to another engine PID")
                return client

            first, second = connect(), connect()
            engine_identity = ProcessIdentity.read(process.pid)
            endpoint_identity = (endpoint.stat().st_dev, endpoint.stat().st_ino)
            wrong = Client(endpoint)
            try:
                try:
                    wrong.initialize(root / "another-home")
                except GateError as error:
                    require(str(error) == "endpoint belongs to another CODEX_HOME",
                            "wrong-home check failed for another reason")
                else:
                    raise GateError("wrong home was accepted")
            finally:
                wrong.close()
            report["endpoint_home"] = "passed"

            params = {"cwd": str(project), "modelProvider": "openai",
                      "sandbox": "read-only", "approvalPolicy": "on-request",
                      "approvalsReviewer": "user"}
            unrelated = first.request("thread/start", params)["thread"]
            first.request("turn/start", {"threadId": unrelated["id"],
                "input": [{"type": "text", "text": "Unrelated fixture.", "text_elements": []}]})
            first.event("turn/completed", unrelated["id"])
            unrelated_before = first.request("thread/read", {
                "threadId": unrelated["id"], "includeTurns": True})["thread"]
            for mode in ("legacy", "paginated"):
                if mode == "legacy":
                    target = first.request("thread/fork", {"threadId": unrelated["id"]})["thread"]
                else:
                    target = first.request("thread/start", {**params, "historyMode": mode})["thread"]
                thread_id = target["id"]
                require(thread_id != unrelated["id"], "native threads unexpectedly alias")
                marker = f"MUXY_TARGET_{mode}"
                client_id="00000000-0000-4000-8000-00000000000"+("1" if mode=="legacy" else "2")
                seed = first.request("turn/start", {"threadId": thread_id,"clientUserMessageId":client_id,
                    "input": [{"type": "text", "text": marker, "text_elements": []}]})
                first.event("turn/completed", thread_id)
                saved=first.request("thread/turns/list", {"threadId":thread_id,"limit":20,"itemsView":"full"})
                require(any(item.get("type")=="userMessage" and item.get("clientId")==client_id
                            for turn in saved["data"] for item in turn["items"]),
                        "native history did not preserve the submission correlation ID")
                report["native_operation_reconciliation"]="passed"
                expected_turns = [turn["id"] for turn in target["turns"]] + [seed["turn"]["id"]]
                if mode == "legacy":
                    report["distinct_thread_session_ids"] = (
                        "passed" if thread_id != target["sessionId"] else "not exercised"
                    )
                if mode == "legacy":
                    with console(executable, endpoint, project, env, thread_id, marker) as (_, terminal, terminal_identity):
                        require(terminal_identity.executable == engine_identity.executable,
                                "Console and engine use different native executables")
                        terminal_identity.revalidate()
                        engine_identity.revalidate()
                        require((endpoint.stat().st_dev, endpoint.stat().st_ino) == endpoint_identity,
                                "engine socket was replaced before handoff")
                        report["process_identity"] = "passed"
                        if rust_native_test:
                            subprocess.run(["cargo", "test", "-p", "muxy-server", "--lib",
                                "session::chat::codex::tests::native_cli_transport_and_terminal_binding", "--", "--ignored", "--exact"],
                                cwd=Path(__file__).resolve().parent.parent,
                                env={**os.environ, "MUXY_CODEX_PROBE_HOME": str(home),
                                     "MUXY_CODEX_PROBE_ENDPOINT": str(endpoint), "MUXY_CODEX_PROBE_PROJECT": str(project),
                                     "MUXY_CODEX_PROBE_ENGINE_PID":str(engine_identity.pid),"MUXY_CODEX_PROBE_TUI_PID": str(terminal_identity.pid), "MUXY_CODEX_PROBE_THREAD": thread_id},
                                check=True, timeout=90, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
                            report["rust_native_transport_and_binding"] = "passed"
                            subprocess.run(["cargo","test","-p","muxy-server","--lib",
                                "session::chat::tests::native_owner_resume_console_and_terminal_takeover","--","--ignored","--exact","--nocapture"],
                                cwd=Path(__file__).resolve().parent.parent,
                                env={**os.environ,"MUXY_CODEX_PROBE_ENGINE_PID":str(engine_identity.pid),
                                     "MUXY_CODEX_PROBE_HOME":str(home),"MUXY_CODEX_PROBE_PROJECT":str(project),
                                     "MUXY_CODEX_PROBE_ENDPOINT":str(endpoint),"MUXY_CODEX_PROBE_THREAD":thread_id},
                                check=True,timeout=90)
                            report["muxy_owner_console_takeover"]="passed"
                        os.write(terminal, b"\x1b[200~Synthetic approval gate.\x1b[201~")
                        # Keep the paste separate from Enter, as a real terminal does.
                        time.sleep(0.2)
                        os.write(terminal, b"\r")
                        turn = first.wait(lambda msg: msg.get("method") == "turn/started"
                            and msg.get("params", {}).get("threadId") == thread_id
                            and msg["params"]["turn"]["id"] not in expected_turns)["params"]["turn"]
                        require(turn["id"], "turn/start has no acknowledged native turn ID")
                        report["turn_acknowledgment"] = "passed"
                        expected_turns.append(turn["id"])
                        method = "item/commandExecution/requestApproval"
                        prompt_a = first.event(method, thread_id)
                        resumed = second.request("thread/resume", {"threadId": thread_id})
                        require(resumed["thread"]["id"] == thread_id, "resumed another thread")
                        require(resumed["thread"]["sessionId"] == target["sessionId"],
                                "resume changed session tree")
                        prompt_b = second.event(method, thread_id)
                        require(type(prompt_a["id"]) is type(prompt_b["id"])
                                and prompt_a["id"] == prompt_b["id"], "approval IDs differ across clients")
                    active = second.request("thread/read", {"threadId": thread_id, "includeTurns": True})["thread"]
                    require(active["turns"][-1]["status"] == "inProgress",
                            "detaching Console interrupted its active shared turn")
                    first.request("thread/unsubscribe", {"threadId": thread_id})
                    first.close()
                    first = connect()
                    first.request("thread/resume", {"threadId": thread_id})
                    replay = first.event(method, thread_id)
                    require(type(replay["id"]) is type(prompt_b["id"])
                            and replay["id"] == prompt_b["id"], "pending approval not replayed")
                    second.send({"id": prompt_b["id"], "result": {"decision": "decline"}})
                    for client in (first, second):
                        resolved = client.event("serverRequest/resolved", thread_id)["params"]
                        require(type(resolved["requestId"]) is type(prompt_b["id"])
                                and resolved["requestId"] == prompt_b["id"], "resolved another approval")
                    # A stale competing answer must not revive an already resolved request.
                    first.send({"id": replay["id"], "result": {"decision": "accept"}})
                    first.event("turn/completed", thread_id)
                    first.request("thread/read", {"threadId": thread_id})
                    require(not (project / "declined-command-ran").exists(),
                            "declined command ran after a competing stale approval")
                    report["two_client_approvals"] = "passed"
                    report["active_console_detach"] = "passed"
                else:
                    extra = first.request("turn/start", {"threadId": thread_id,
                        "input": [{"type": "text", "text": "Paginated fixture.", "text_elements": []}]})
                    expected_turns.append(extra["turn"]["id"])
                    first.event("turn/completed", thread_id)
                read = first.request("thread/read", {"threadId": thread_id, "includeTurns": mode == "legacy"})
                require(read["thread"]["id"] == thread_id, "history returned another thread")
                if mode == "paginated":
                    turns = pages(first, "thread/turns/list", {"threadId": thread_id, "sortDirection": "asc"})
                    require([turn["id"] for turn in turns] == expected_turns,
                            "paginated history lost or duplicated seeded turns")
                    items = pages(first, "thread/items/list", {"threadId": thread_id})
                    require(marker in json.dumps(items) and "Paginated fixture." in json.dumps(items),
                            "paginated items lost seeded prompt text")
                    require(len(items) >= 4, "paginated history lost seeded items")
                    native = [entry["item"] for entry in items]
                    require(len({item["id"] for item in native}) == len(native),
                            "paginated history duplicated native item IDs")
                    require(sum(item["type"] == "userMessage" for item in native) == 2
                            and sum(item["type"] == "agentMessage" and item["text"] == "Gate complete."
                                    for item in native) == 2,
                            "paginated history lost user or assistant items")
                else:
                    require([turn["id"] for turn in read["thread"]["turns"]] == expected_turns,
                            "legacy history lost or duplicated seeded turns")
                    require(marker in json.dumps(read) and "Synthetic approval gate." in json.dumps(read),
                            "legacy history lost seeded prompt text")
                report[f"{mode}_history"] = "passed"
                first.request("thread/unsubscribe", {"threadId": thread_id})
                require(process.poll() is None, "detach stopped the shared engine")
                handoff = second.request("thread/resume", {"threadId": thread_id})
                require(handoff["thread"]["id"] == thread_id, "handoff resumed another thread")
                report["api_detach"] = "passed"
                with console(executable, endpoint, project, env, thread_id, marker):
                    joined = second.request("thread/resume", {"threadId": thread_id})
                    require(joined["thread"]["id"] == thread_id, "chat joined another Console thread")
                    require(all(joined.get(key) == handoff.get(key) for key in (
                        "model", "reasoningEffort", "approvalPolicy", "approvalsReviewer", "sandbox", "runtimeWorkspaceRoots"
                    )), "Console recovery changed the daemon-owned thread configuration")
                require(process.poll() is None, "stopping Console stopped shared engine")
                restored = second.request("thread/read", {"threadId": thread_id})
                require(restored["thread"]["id"] == thread_id, "Console detach lost thread")
                require(restored["thread"]["modelProvider"] == "openai", "Console changed provider")
                if mode == "paginated":
                    require(pages(second, "thread/items/list", {"threadId": thread_id}) == items,
                            "Console recovery changed paginated history")
                else:
                    after = second.request("thread/read", {"threadId": thread_id, "includeTurns": True})
                    require(after["thread"]["turns"] == read["thread"]["turns"],
                            "Console recovery changed legacy history")
                report[f"{mode}_console_handoff"] = "passed"
            listed = pages(second, "thread/list", {"cwd": str(project), "modelProviders": ["openai"]})
            require(all(thread["modelProvider"] == "openai" for thread in listed), "listing ignored provider")
            read = second.request("thread/read", {"threadId": unrelated["id"], "includeTurns": True})["thread"]
            require(read["id"] == unrelated["id"] and read["turns"] == unrelated_before["turns"],
                    "unrelated thread changed")
            report["unrelated_thread"] = "passed"
            first.close()
            second.close()
            process.terminate()
            process.wait(timeout=5)
            stdio = StdioClient(executable, env, project)
            try:
                stdio.initialize(home)
                target = stdio.request("thread/start", {**params, "historyMode": "paginated"})["thread"]
                thread_id = target["id"]
                marker = "MUXY_STANDALONE_ROOT"
                stdio.request("turn/start", {"threadId": thread_id,
                    "input": [{"type": "text", "text": marker, "text_elements": []}]})
                stdio.event("turn/completed", thread_id)
                require(stdio.request("thread/read", {"threadId": thread_id})["thread"]["status"]["type"] == "idle",
                        "owned stdio engine did not confirm completion")
            finally:
                stdio.close()
            require(stdio.process.poll() is not None, "owned stdio engine did not stop")
            report["owned_stdio_shutdown"] = "passed"
            with console(executable, None, project, env, thread_id, marker):
                report["standalone_console_handoff"] = "passed"


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--codex", default="codex", help="CLI executable to verify")
    parser.add_argument("--rust-native-test", action="store_true", help="also verify Muxy's Rust transport against the isolated engine")
    args = parser.parse_args()
    report = {
        "probe": "failed",
        "scope": "isolated shared-daemon native API and TUI",
        "implementation_gate": "not established",
        "unverified": ["Muxy end-to-end terminal takeover",
                       "signed-in provider context", "Muxy desktop integration"],
    }
    try:
        executable = shutil.which(args.codex)
        require(executable is not None, "Codex executable not found")
        report["cli"] = subprocess.check_output([executable, "--version"], text=True, timeout=5).strip()
        run(executable, report, args.rust_native_test)
        if args.rust_native_test:
            report["implementation_gate"]="passed for the tested local native engine and launch context"
            report["unverified"].remove("Muxy end-to-end terminal takeover")
        report["probe"] = "passed"
        print(json.dumps(report, indent=2))
        return 0
    except (GateError, OSError, ValueError, KeyError, subprocess.SubprocessError) as error:
        # Report only controlled diagnostics, never native message bodies or stderr.
        report["reason"] = str(error) if isinstance(error, GateError) else type(error).__name__
        print(json.dumps(report, indent=2))
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
