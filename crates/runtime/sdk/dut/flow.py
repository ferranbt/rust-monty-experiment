# Runs INSIDE the Monty sandbox. Sugar over next_event (SPEC §6.3).
#
# Rule: every incoming frame is dispatched to all handlers before any predicate
# sees it. request/expect only *watch* the stream.
from dut.core import next_event, now, emit, _rt
from dut.core import write as _host_write

# Frames shown in a timeout error: the last few seen while waiting.
_SEEN = 3


def _is_data(ev):
    return ev["kind"] == "frame" or ev["kind"] == "packet"


def _error_text(e):
    return f"{type(e).__name__}: {e}"


def _decode(data):
    # Text-mode frames: junk bytes become U+FFFD instead of raising.
    if isinstance(data, bytes):
        return data.decode("utf-8", "replace")
    return data


def _to_bytes(data):
    if isinstance(data, str):
        return data.encode()
    return data


def write(data, channel="main"):
    """Send a message to the device, encoded by the Device's format. A str is sent UTF-8 encoded."""
    dev = _rt["device"]
    if dev is not None:
        data = dev.format.encode(data)
    _host_write(_to_bytes(data), channel)


def _next(timeout):
    # next_event, with frame data decoded by the Device's format. A frame the
    # format rejects is reported and skipped.
    deadline = now() + timeout
    while True:
        ev = next_event(max(0, deadline - now()))
        dev = _rt["device"]
        if ev is None or not _is_data(ev) or dev is None:
            return ev
        try:
            ev["data"] = dev.format.decode(ev["data"])
            return ev
        except Exception as e:
            frame = ev["data"]
            emit("format_error", {"error": _error_text(e), "channel": ev["channel"],
                                  "frame": frame.hex() if isinstance(frame, bytes) else frame})
            if now() >= deadline:
                return None


def _report(e, ev):
    if _rt["fail_raised"]:
        _rt["fail_raised"] = False   # fail() already reported it
        return
    data = ev["data"]
    frame = data.hex() if isinstance(data, bytes) else data
    emit("parse_error", {"error": _error_text(e), "channel": ev["channel"], "frame": frame})


def _run_handler(fn, arg, ev):
    _rt["fail_raised"] = False
    try:
        fn(arg)
    except Exception as e:
        _report(e, ev)


def _run_line_handler(fn, match, ev):
    _rt["fail_raised"] = False
    named = match.groupdict()
    try:
        if len(named) > 0:
            fn(**named)
        else:
            fn(*match.groups())
    except Exception as e:
        _report(e, ev)


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
        if isinstance(ev["data"], str):
            # Every text line is logged; scripts never have to.
            emit("log", {"text": ev["data"]})
        _rt["dispatching"] = True
        try:
            for fn in dev.handlers:
                _run_handler(fn, ev["data"], ev)
            if len(dev.line_handlers) > 0:
                line = _decode(ev["data"])
                for h in dev.line_handlers:
                    match = h["pattern"].match(line)
                    if match is not None:
                        _run_line_handler(h["fn"], match, ev)
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

    Every frame is dispatched to the handlers before pred sees it.
    """
    _no_wait_in_handler("wait")
    deadline = now() + timeout
    _rt["depth"] += 1
    try:
        while True:
            remaining = deadline - now()
            if remaining <= 0:
                return None
            ev = _next(remaining)
            if ev is None:
                return None
            _dispatch(ev)
            if pred(ev):
                return ev
    finally:
        _rt["depth"] -= 1


def _matcher(match):
    # bytes/str -> prefix match; otherwise a function(frame) -> bool.
    # A match function that raises on a junk frame counts as "no match".
    if isinstance(match, bytes) or isinstance(match, str):
        def prefix(f):
            if isinstance(f, str):
                return f.startswith(_decode(match))
            return f.startswith(_to_bytes(match))
        return prefix

    def safe(f):
        try:
            return match(f) == True
        except Exception:
            return False
    return safe


def _watch(channel, match, seen):
    # Frame predicate that also remembers the last frames on `channel`.
    m = _matcher(match)

    def pred(ev):
        if not _is_data(ev) or ev["channel"] != channel:
            return False
        seen.append(ev["data"])
        if len(seen) > _SEEN:
            seen.pop(0)
        return m(ev["data"])
    return pred


def _seen_text(seen):
    if len(seen) == 0:
        return "nothing received"
    return "last frames: " + ", ".join([repr(f) for f in seen])


def request(data, match, timeout=1.0, channel="main"):
    """write(data) then wait for a frame matching `match`. Raises TimeoutError."""
    _no_wait_in_handler("request")
    if _rt["in_request"]:
        raise RuntimeError("request() already in flight")
    seen = []
    _rt["in_request"] = True
    try:
        write(data, channel)
        ev = wait(_watch(channel, match, seen), timeout)
    finally:
        _rt["in_request"] = False
    if ev is None:
        raise TimeoutError(f"request: no matching reply within {timeout}s; "
                           f"sent {_to_bytes(data)!r}; {_seen_text(seen)}")
    return ev["data"]


def expect(prefix, timeout=1.0, channel="main"):
    """Wait for a frame starting with `prefix`. Returns it. Raises TimeoutError."""
    _no_wait_in_handler("expect")
    seen = []
    ev = wait(_watch(channel, prefix, seen), timeout)
    if ev is None:
        raise TimeoutError(f"expect: no frame starting with {prefix!r} within {timeout}s; "
                           f"{_seen_text(seen)}")
    return ev["data"]


def wait_until(cond, timeout=1.0):
    """Wait until cond() is true (usually on parser-maintained `state`). Raises TimeoutError."""
    _no_wait_in_handler("wait_until")
    if cond():
        return True
    seen = []
    watch = _watch("main", lambda f: False, seen)

    def pred(ev):
        watch(ev)
        return cond()
    if wait(pred, timeout) is None:
        raise TimeoutError(f"wait_until: condition not met within {timeout}s; {_seen_text(seen)}")
    return True
