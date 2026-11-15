# Runs INSIDE the Monty sandbox. Protocol helpers for framed binary (SPEC §4, layer 3).
#
# Only the generic "body + CRC trailer" envelope. Headers, opcodes and field
# layouts are device-specific and stay in the device script.
from dut.core import pack, unpack, crc

_SIZES = {"B": 1, "H": 2, "I": 4}


def _trailer_fmt(kind, fmt):
    if fmt is not None:
        if len(fmt) != 2 or fmt[0] not in "<>" or fmt[1] not in _SIZES:
            raise ValueError("CRC trailer fmt must be one of <B, >B, <H, >H, <I, >I")
        return fmt
    if kind.startswith("crc8"):
        return "<B"
    if kind.startswith("crc16"):
        return "<H"
    if kind.startswith("crc32"):
        return "<I"
    raise ValueError(f"cannot infer the CRC trailer size of {kind!r}; pass fmt=")


def with_crc(body, kind="crc16-ccitt", fmt=None):
    """Return body + CRC(body) as a trailer. fmt defaults to the CRC width, little-endian."""
    return body + pack(_trailer_fmt(kind, fmt), [crc(kind, body)])


def check_crc(frame, kind="crc16-ccitt", fmt=None):
    """Return the body if the trailing CRC matches, else None (also for frames too short)."""
    fmt = _trailer_fmt(kind, fmt)
    n = _SIZES[fmt[1]]
    if len(frame) <= n:
        return None
    body = frame[:-n]
    if crc(kind, body) != unpack(fmt, frame[-n:])[0]:
        return None
    return body
