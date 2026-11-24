# Runs INSIDE the Monty sandbox. Shared runtime state for the SDK.
#
# Host primitives come from the `_host` external module. Everything else in
# the SDK is built on top of these (SPEC §4, layer 1).
from _host import write, next_event, now, emit
from _host import pack, unpack, crc, cobs_encode, cobs_decode, slip_encode, slip_decode, bytes_from_list

SDK_VERSION = "0.1"

# Customer-visible scratch dict. Persists for the session (SPEC §6.1).
state = {}

# SDK-internal runtime. Not part of the public API.
_rt = {
    "device": None,       # the single Device instance of this script
    "depth": 0,           # nesting of active wait() calls
    "dispatching": False, # True while an on_frame handler runs
    "in_request": False,  # one request in flight at a time (SPEC §6.3)
    "pending": [],        # timer ids whose ticks arrived while busy
    "fail_raised": False, # set by fail() so dispatch doesn't double-report
    "metrics": {},        # name -> Counter/Gauge, filled as they are declared
}


def device():
    dev = _rt["device"]
    if dev is None:
        raise RuntimeError("no Device declared in this script")
    return dev
