# A "compromised" PDF child: speaks the protocol, then floods the parent.
# argv: fake_child.py <mode> --pdf-worker <budget> <path>
import sys, struct
mode = sys.argv[1]
inp = sys.stdin.buffer
out = sys.stdout.buffer
def read(n):
    b = b""
    while len(b) < n:
        c = inp.read(n - len(b))
        if not c: sys.exit(0)
        b += c
    return b
assert read(1) == b"K"
(n,) = struct.unpack("<I", read(4)); read(n); read(48)
out.write(b"O" + struct.pack("<I", 1) + struct.pack("<ff", 1000.0, 1000.0) + b"\0"); out.flush()
if mode == "failed":
    chunk = bytearray()
    i = 0
    while True:
        chunk = b"".join(b"X" + struct.pack("<I", (i + k) & 0xffffffff) for k in range(100000))
        i += 100000
        out.write(chunk); out.flush()
elif mode == "pages":
    # The parent may first send an empty list; wait for one with a page.
    entries = []
    while not entries:
        assert read(1) == b"W"
        (count,) = struct.unpack("<I", read(4))
        entries = [struct.unpack("<If", read(8)) for _ in range(count)]
    page, scale = entries[0]
    w = h = int(1000 * scale)
    frame = b"P" + struct.pack("<IfII", page, scale, w, h) + b"\x7f" * (w * h * 4)
    sys.stderr.write("frame %d bytes\n" % len(frame))
    while True:
        out.write(frame); out.flush()
