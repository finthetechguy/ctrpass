"""Client for the privileged radio helper, `ctrpass serve`, which speaks JSON lines on stdio."""

import json
import os
import shlex
import shutil
import subprocess
import threading
from collections import deque


def helper_command(interface, force=False):
    """Returns the argv that starts the helper, going through pkexec unless already root."""
    tail = ["serve", "-i", interface] + (["--force"] if force else [])
    override = os.environ.get("CTRPASS_HELPER")
    if override:
        return shlex.split(override) + tail
    binary = os.environ.get("CTRPASS_BIN") or shutil.which("ctrpass")
    if not binary:
        raise FileNotFoundError("'ctrpass' is not installed")
    argv = [os.path.abspath(binary)] + tail
    return argv if os.geteuid() == 0 else ["pkexec"] + argv


class Helper:
    """Runs the helper and passes each event dict to `on_event` from a background thread, ending with an "exit" event."""

    def __init__(self, on_event):
        self._on_event = on_event
        self._proc = None
        self._stderr = deque(maxlen=20)
        self._lock = threading.Lock()

    @property
    def running(self):
        return self._proc is not None and self._proc.poll() is None

    def start(self, interface, force=False):
        if self.running:
            raise RuntimeError("helper already running")
        self._proc = subprocess.Popen(
            helper_command(interface, force),
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            bufsize=1,
        )
        threading.Thread(target=self._read_stderr, args=(self._proc,), daemon=True).start()
        threading.Thread(target=self._read_stdout, args=(self._proc,), daemon=True).start()

    def _read_stdout(self, proc):
        for line in proc.stdout:
            line = line.strip()
            if not line:
                continue
            try:
                event = json.loads(line)
            except ValueError:
                continue
            if isinstance(event, dict) and "event" in event:
                self._on_event(event)
        proc.stdout.close()
        code = proc.wait()
        self._on_event({"event": "exit", "code": code, "stderr": "\n".join(self._stderr)})

    def _read_stderr(self, proc):
        for line in proc.stderr:
            self._stderr.append(line.rstrip())
        proc.stderr.close()

    def _send(self, **command):
        with self._lock:
            if not self.running:
                raise RuntimeError("helper is not running")
            self._proc.stdin.write(json.dumps(command) + "\n")
            self._proc.stdin.flush()

    def load_keys(self, path):
        # The key file is read here, as the user, so the helper never opens user paths.
        with open(path, encoding="utf-8") as f:
            self.send_keys(f.read())

    def send_keys(self, text):
        self._send(cmd="keys", text=text)

    def load_store(self, snapshot):
        """Hands over a store's contents, as the JSON text `Store.snapshot()` returns."""
        self._send(cmd="load", store=json.loads(snapshot))

    def scan(self, channels=None):
        command = {"cmd": "scan"}
        if channels:
            command["channels"] = list(channels)
        self._send(**command)

    def exchange(self, role="lead", passes=None, window=60.0, radio="auto", channels=None, rotate_every=None):
        """Runs passes until `passes` are done or stop is sent; each "pass" event carries what arrived."""
        command = {"cmd": "exchange", "role": role, "window": window, "radio": radio}
        if rotate_every is not None:
            command["rotate_every"] = rotate_every
        if passes is not None:
            command["passes"] = passes
        if channels:
            command["channels"] = list(channels)
        self._send(**command)

    def rotate(self):
        """Changes the MAC and clears the filter of passed consoles, ending with a "rotated" event."""
        self._send(cmd="rotate")

    def stop(self):
        self._send(cmd="stop")

    def close(self, timeout=5):
        if not self.running:
            return
        try:
            self._send(cmd="quit")
            self._proc.stdin.close()
            self._proc.wait(timeout)
        except (RuntimeError, OSError, subprocess.TimeoutExpired):
            self._proc.terminate()


def file_pass(store, event):
    """Files a "pass" event's arrivals and peer book into `store`, returning what `Store.file_received` reports."""
    messages = [bytes.fromhex(m) for m in event.get("messages", [])]
    result = {"filed": [], "refused": []}
    if messages and event.get("peer"):
        result = store.file_received(messages, event["peer"])
    if event.get("peers") is not None:
        store.set_peers(json.dumps(event["peers"]))
    return result
