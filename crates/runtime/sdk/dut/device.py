# Runs INSIDE the Monty sandbox. Device declaration and registry.
#
# Monty has no __name__ on functions, so the decorator decides the name.
from dut.core import _rt, SDK_VERSION
from dut.framing import Framing

VERBS = ["set_channel", "tx_on", "rx_start", "rx_count", "status"]
HOOKS = ["prepare", "enter", "exit", "recover", "finish"]


def _event_types(events):
    # {"name": "description"} or {"name": {"description": ..., "fields": {"f": "description"}}}
    out = {}
    if events is None:
        return out
    if not isinstance(events, dict):
        raise TypeError("events must be a dict of name -> description")
    for name in events:
        spec = events[name]
        if isinstance(spec, str):
            spec = {"description": spec}
        if not isinstance(spec, dict) or not isinstance(spec.get("description"), str):
            raise ValueError(f"event '{name}' needs a description")
        fields = spec.get("fields", {})
        if not isinstance(fields, dict):
            raise ValueError(f"event '{name}': fields must be a dict of name -> description")
        out[name] = {"description": spec["description"], "fields": dict(fields)}
    return out


class Device:
    def __init__(self, name, framing=None, heartbeat_within=None, channels=None, sdk=None,
                 events=None):
        if _rt["device"] is not None:
            raise RuntimeError("only one Device per script")
        if not isinstance(name, str) or name == "":
            raise ValueError("Device name must be a non-empty string")
        if heartbeat_within is not None and heartbeat_within <= 0:
            raise ValueError("heartbeat_within must be > 0 seconds")
        self.name = name
        self.sdk = sdk
        self.framing = framing if framing is not None else Framing.delimiter(b"\n")
        self.heartbeat_within = heartbeat_within
        self.channels = channels if channels is not None else {}
        self.events = _event_types(events)
        self.verbs = {}
        self.hooks = {}
        self.handlers = []
        self.timers = []
        self.readers = {}
        _rt["device"] = self

    # -- registration -----------------------------------------------------
    def _add_verb(self, name, fn, confirm):
        if name in self.verbs:
            raise ValueError(f"verb '{name}' defined twice")
        self.verbs[name] = {"fn": fn, "confirm": confirm}
        return fn

    def _verb(self, name, fn, confirm):
        # Supports both @dev.tx_on and @dev.tx_on(confirm="rf_power").
        if fn is None:
            return lambda f: self._add_verb(name, f, confirm)
        return self._add_verb(name, fn, confirm)

    def verb(self, name, confirm=None):
        if not isinstance(name, str) or name == "":
            raise ValueError("verb name must be a non-empty string")
        return lambda f: self._add_verb(name, f, confirm)

    def set_channel(self, fn=None, confirm=None):
        return self._verb("set_channel", fn, confirm)

    def tx_on(self, fn=None, confirm=None):
        return self._verb("tx_on", fn, confirm)

    def rx_start(self, fn=None, confirm=None):
        return self._verb("rx_start", fn, confirm)

    def rx_count(self, fn=None, confirm=None):
        return self._verb("rx_count", fn, confirm)

    def status(self, fn=None, confirm=None):
        return self._verb("status", fn, confirm)

    def hook(self, name):
        if name not in HOOKS:
            raise ValueError(f"unknown hook '{name}', expected one of {HOOKS}")

        def deco(fn):
            if name in self.hooks:
                raise ValueError(f"hook '{name}' defined twice")
            self.hooks[name] = fn
            return fn
        return deco

    def on_frame(self, fn):
        self.handlers.append(fn)
        return fn

    def every(self, seconds):
        if seconds <= 0:
            raise ValueError("every(seconds=...) must be > 0")

        def deco(fn):
            self.timers.append({"seconds": seconds, "fn": fn})
            return fn
        return deco

    def reader(self, arg=None):
        # @dev.reader  or  @dev.reader("name")
        if isinstance(arg, str):
            return lambda f: self._add_reader(arg, f)
        if arg is None:
            return lambda f: self._add_reader("default", f)
        return self._add_reader("default", arg)

    def _add_reader(self, name, fn):
        if name in self.readers:
            raise ValueError(f"reader '{name}' defined twice")
        self.readers[name] = fn
        return fn

    # -- data-only view for the host --------------------------------------
    def describe(self):
        verbs = {}
        for name in self.verbs:
            verbs[name] = {"confirm": self.verbs[name]["confirm"],
                           "standard": name in VERBS}
        timers = []
        i = 0
        for t in self.timers:
            timers.append({"id": i, "seconds": t["seconds"]})
            i += 1
        return {
            "name": self.name,
            "sdk": SDK_VERSION,
            "sdk_declared": self.sdk,
            "framing": self.framing,
            "heartbeat_within": self.heartbeat_within,
            "channels": self.channels,
            "verbs": verbs,
            "hooks": sorted(self.hooks.keys()),
            "handlers": len(self.handlers),
            "timers": timers,
            "readers": sorted(self.readers.keys()),
            "events": self.events,
        }
