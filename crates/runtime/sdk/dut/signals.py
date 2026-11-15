# Runs INSIDE the Monty sandbox. Signals reported to the engine via emit().
from dut.core import emit, _rt


def heartbeat(ok=True):
    emit("heartbeat", {"ok": ok == True})


def identity(data):
    if not isinstance(data, dict):
        raise TypeError("identity() takes a dict")
    emit("identity", data)


def record(data):
    if not isinstance(data, dict):
        raise TypeError("record() takes a dict")
    emit("record", data)


def event(name, **data):
    """Report a declared event. Name and fields must be declared in Device(events=...)."""
    dev = _rt["device"]
    if dev is None or name not in dev.events:
        raise ValueError(f"event '{name}' is not declared in Device(events=...)")
    fields = dev.events[name]["fields"]
    for key in data:
        if key not in fields:
            raise ValueError(f"event '{name}' has no field '{key}'")
    payload = dict(data)
    payload["name"] = name
    emit("event", payload)


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
