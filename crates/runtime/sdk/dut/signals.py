# Runs INSIDE the Monty sandbox. Signals reported to the engine via emit().
from dut.core import emit, _rt


def heartbeat(ok=True):
    emit("heartbeat", {"ok": ok == True})


def identity(data):
    if not isinstance(data, dict):
        raise TypeError("identity() takes a dict")
    emit("identity", data)


def log(text, **fields):
    """A log line, reported to the host like any other signal (kind "log")."""
    payload = dict(fields)
    payload["text"] = str(text)
    emit("log", payload)


def fail(msg):
    # Report and abort the current verb/hook. The engine decides what it means.
    emit("fail", {"msg": str(msg)})
    _rt["fail_raised"] = True
    raise RuntimeError(f"fail: {msg}")
