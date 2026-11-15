# Runs INSIDE the Monty sandbox. Sugar over next_event (SPEC §6.3).
#
# Rule: every incoming frame is dispatched to all on_frame handlers before any
# predicate sees it. request/expect only *watch* the stream.
from dut.core import write, next_event, now, emit, _rt


def _is_data(ev):
    return ev["kind"] == "frame" or ev["kind"] == "packet"


def _error_text(e):
    return f"{type(e).__name__}: {e}"


def _run_handler(fn, ev):
    _rt["fail_raised"] = False
    try:
        fn(ev["data"])
    except Exception as e:
        if _rt["fail_raised"]:
            _rt["fail_raised"] = False   # fail() already reported it
        else:
            emit("parse_error", {"error": _error_text(e), "channel": ev["channel"],
                                 "frame": ev["data"].hex()})


def _run_timer(timer_id):
    timers = _rt["device"].timers
    if timer_id < 0 or timer_id >= len(timers):
        return
    _rt["fail_raised"] = False
    try:
        timers[timer_id]["fn"]()
    except Exception as e:
        if _rt["fail_raised"]:
            _rt["fail_raised"] = False
        else:
            emit("timer_error", {"error": _error_text(e), "timer": timer_id})


def _dispatch(ev):
    dev = _rt["device"]
    if dev is None:
        return
    if _is_data(ev):
        _rt["dispatching"] = True
        try:
            for fn in dev.handlers:
                _run_handler(fn, ev)
        finally:
            _rt["dispatching"] = False
    elif ev["kind"] == "tick":
        timer_id = ev["timer"]
        if _rt["depth"] > 0:
            # A verb/timer is waiting: defer the tick (coalesced) until idle.
            if timer_id not in _rt["pending"]:
                _rt["pending"].append(timer_id)
        else:
            _run_timer(timer_id)


def _run_pending():
    while len(_rt["pending"]) > 0 and _rt["depth"] == 0:
        timer_id = _rt["pending"].pop(0)
        _run_timer(timer_id)


def _no_wait_in_handler(name):
    if _rt["dispatching"]:
        raise RuntimeError(f"{name}() cannot be used inside an on_frame handler")


def wait(pred, timeout):
    """Process events until pred(event) is true. Returns the event, or None on timeout.

    Every frame is dispatched to the on_frame handlers before pred sees it.
    """
    _no_wait_in_handler("wait")
    deadline = now() + timeout
    _rt["depth"] += 1
    try:
        while True:
            remaining = deadline - now()
            if remaining <= 0:
                return None
            ev = next_event(remaining)
            if ev is None:
                return None
            _dispatch(ev)
            if pred(ev):
                return ev
    finally:
        _rt["depth"] -= 1


def _matcher(match):
    # bytes -> prefix match; otherwise a function(frame) -> bool.
    # A match function that raises on a junk frame counts as "no match".
    if isinstance(match, bytes):
        return lambda f: f.startswith(match)

    def safe(f):
        try:
            return match(f) == True
        except Exception:
            return False
    return safe


def _frame_on(channel, match):
    m = _matcher(match)
    return lambda ev: _is_data(ev) and ev["channel"] == channel and m(ev["data"])


def request(data, match, timeout=1.0, channel="main"):
    """write(data) then wait for a frame matching `match`. Raises TimeoutError."""
    _no_wait_in_handler("request")
    if _rt["in_request"]:
        raise RuntimeError("request() already in flight")
    _rt["in_request"] = True
    try:
        write(data, channel)
        ev = wait(_frame_on(channel, match), timeout)
    finally:
        _rt["in_request"] = False
    if ev is None:
        raise TimeoutError(f"request: no matching reply within {timeout}s")
    return ev["data"]


def expect(prefix, timeout=1.0, channel="main"):
    """Wait for a frame starting with `prefix`. Returns it. Raises TimeoutError."""
    _no_wait_in_handler("expect")
    if isinstance(prefix, str):
        prefix = prefix.encode()
    ev = wait(_frame_on(channel, prefix), timeout)
    if ev is None:
        raise TimeoutError(f"expect: no frame starting with {prefix} within {timeout}s")
    return ev["data"]


def wait_until(cond, timeout=1.0):
    """Wait until cond() is true (usually on parser-maintained `state`). Raises TimeoutError."""
    _no_wait_in_handler("wait_until")
    if cond():
        return True
    if wait(lambda ev: cond(), timeout) is None:
        raise TimeoutError(f"wait_until: condition not met within {timeout}s")
    return True
