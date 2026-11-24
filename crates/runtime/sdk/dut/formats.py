# Runs INSIDE the Monty sandbox. Formats: what a frame means as data.
#
# A format is any object with two methods (Monty has no inheritance, so there is
# no base class to subclass):
#
#   decode(frame: bytes) -> what handlers, request and expect receive
#   encode(message) -> bytes to write
#
# Pass one to Device(format=...). A decode that raises drops the frame and
# reports a `format_error` signal.


class Raw:
    """Frames stay bytes (the default)."""

    def decode(self, frame):
        return frame

    def encode(self, message):
        return message


class Text:
    """Frames are UTF-8 text; junk bytes become U+FFFD. Every line is logged."""

    def decode(self, frame):
        return frame.decode("utf-8", "replace")

    def encode(self, message):
        return message.encode() if isinstance(message, str) else message
