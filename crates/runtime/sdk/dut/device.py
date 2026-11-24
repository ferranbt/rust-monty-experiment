# Runs INSIDE the Monty sandbox. Device declaration and registry.
#
# Monty has no __name__ on functions, so the decorator decides the name.
import re

from dut.core import _rt, SDK_VERSION
from dut.framing import Framing
from dut.formats import Raw
from dut.metrics import _describe as _describe_metrics

VERBS = ["set_channel", "tx_on", "rx_start", "rx_count", "status"]
HOOKS = ["prepare", "enter", "exit", "recover", "finish"]


class Device:
    def __init__(self, name, framing=None, heartbeat_within=None, channels=None, sdk=None,
                 format=None):
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
        # Any object with decode(frame) and encode(message); see dut.formats.
        self.format = format if format is not None else Raw()
        if not hasattr(self.format, "decode") or not hasattr(self.format, "encode"):
            raise TypeError("format must have decode(frame) and encode(message) methods")
        self.verbs = {}
        self.hooks = {}
        self.handlers = []
        self.line_handlers = []
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

    def on_line(self, pattern):
        """Call the function for every line matching `pattern`, a regex anchored at the
        start of the line. Named groups arrive as keyword arguments; a pattern without
        named groups passes its groups positionally.

            @dev.on_line(r"T=(?P<temp>-?[0-9.]+)")
            def temp(temp): ...
        """
        if not isinstance(pattern, str) or pattern == "":
            raise ValueError("on_line(pattern): pattern must be a non-empty str")
        compiled = re.compile(pattern)

        def deco(fn):
            self.line_handlers.append({"pattern": compiled, "fn": fn})
            return fn
        return deco

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
            "handlers": len(self.handlers) + len(self.line_handlers),
            "timers": timers,
            "readers": sorted(self.readers.keys()),
            "metrics": _describe_metrics(),
        }
