# Runs INSIDE the Monty sandbox. Framing *declarations* only: plain dicts the
# host reads from the registry. The host does the actual framing (SPEC §6.2).


def _check_bytes(name, value):
    if not isinstance(value, bytes) or len(value) == 0:
        raise ValueError(f"Framing: {name} must be non-empty bytes")


class _Framing:
    def delimiter(self, delim=b"\n", keep_empty=False, max_len=4096):
        _check_bytes("delimiter", delim)
        return {"mode": "delimiter", "delimiter": delim, "keep_empty": keep_empty, "max_len": max_len}

    def cobs(self, max_len=4096):
        return {"mode": "cobs", "max_len": max_len}

    def slip(self, max_len=4096):
        return {"mode": "slip", "max_len": max_len}

    def hdlc(self, max_len=4096):
        return {"mode": "hdlc", "max_len": max_len}

    def length_prefixed(self, header=b"", length_offset=0, length_size=1, trailer_len=0,
                        endian="little", max_len=4096):
        if length_size not in (1, 2, 4):
            raise ValueError("Framing.length_prefixed: length_size must be 1, 2 or 4")
        if endian not in ("little", "big"):
            raise ValueError("Framing.length_prefixed: endian must be 'little' or 'big'")
        if length_offset < len(header):
            raise ValueError("Framing.length_prefixed: length_offset overlaps the header")
        return {"mode": "length_prefixed", "header": header, "length_offset": length_offset,
                "length_size": length_size, "trailer_len": trailer_len, "endian": endian,
                "max_len": max_len}

    def fixed(self, size):
        if size <= 0:
            raise ValueError("Framing.fixed: size must be > 0")
        return {"mode": "fixed", "size": size}

    def raw(self):
        return {"mode": "raw"}


Framing = _Framing()
