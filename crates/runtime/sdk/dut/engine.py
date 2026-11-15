# Runs INSIDE the Monty sandbox. Entry points the host calls via feed_run.
# Not part of the customer API. Everything returned here is plain data.
from dut.core import now, next_event, _rt, device
from dut.flow import _dispatch, _run_pending
from dut.signals import record


def _start():
    _rt["fail_raised"] = False
    _rt["in_request"] = False
    _rt["depth"] = 0
    _rt["dispatching"] = False


def registry():
    return device().describe()


def call_verb(name, args):
    dev = device()
    if name not in dev.verbs:
        raise KeyError(f"verb '{name}' is not defined by this device")
    _start()
    return dev.verbs[name]["fn"](**args)


def call_hook(name, args):
    dev = device()
    if name not in dev.hooks:
        return None   # hooks are optional
    _start()
    return dev.hooks[name](**args)


def read(name):
    dev = device()
    if name not in dev.readers:
        raise KeyError(f"reader '{name}' is not defined by this device")
    _start()
    value = dev.readers[name]()
    if isinstance(value, dict):
        record(value)
    return value


def pump(seconds):
    """Idle listening: dispatch frames and ticks for `seconds` of host time."""
    device()
    _start()
    deadline = now() + seconds
    while True:
        _run_pending()
        remaining = deadline - now()
        if remaining <= 0:
            return None
        ev = next_event(remaining)
        if ev is None:
            return None
        _dispatch(ev)
