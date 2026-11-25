# A "compromised" PDF child: speaks the protocol, then floods the parent or
# sends what it should not. A three-page document of 1000 x 1000 points.
# argv: fake_child.py <mode>[=<argument>] --pdf-worker <budget> <path>
#
# The wire format is the one in crates/aster-pdf/src/child.rs.
import sys, struct, time, os, signal

# Like the real child: nothing inherited stays open; and never idle forever.
os.closerange(3, 4096)
signal.alarm(30)

mode = sys.argv[1]
argument = None
if "=" in mode:
    mode, argument = mode.split("=", 1)
inp = sys.stdin.buffer
out = sys.stdout.buffer


def read(n):
    b = b""
    while len(b) < n:
        c = inp.read(n - len(b))
        if not c:
            sys.exit(0)
        b += c
    return b


def request():
    """The next request: (tag, payload)."""
    tag = read(1)
    if tag == b"W":
        (count,) = struct.unpack("<I", read(4))
        return tag, [struct.unpack("<If", read(8)) for _ in range(count)]
    if tag == b"T":
        (count,) = struct.unpack("<I", read(4))
        return tag, [struct.unpack("<I", read(4))[0] for _ in range(count)]
    if tag == b"L" or tag == b"C":
        return tag, None
    if tag == b"Q":
        search, case, start, found, n = struct.unpack("<QBIII", read(21))
        return tag, (search, case, start, found, read(n))
    raise SystemExit("unknown request %r" % tag)


def wait_for(tag, nonempty=True):
    while True:
        t, payload = request()
        if t == tag and (payload or not nonempty):
            return payload


def working(kind, search, page):
    out.write(b"w" + bytes([kind]) + struct.pack("<QI", search, page))


def text(page, body, chars, boxes=None):
    out.write(b"T" + struct.pack("<III", page, len(body), chars) + body)
    out.write(boxes if boxes is not None else b"\0" * chars)


def hits(search, page, entries):
    # entries: list of (start, len, [boxes])
    out.write(b"H" + struct.pack("<QII", search, page, len(entries)))
    for start, length, boxes in entries:
        out.write(struct.pack("<III", start, length, len(boxes)))
        for b in boxes:
            out.write(struct.pack("<ffff", *b))


def searched(search, page):
    out.write(b"N" + struct.pack("<QI", search, page))


def done(search, total, truncated):
    out.write(b"D" + struct.pack("<QIB", search, total, truncated))


assert read(1) == b"K"
(n,) = struct.unpack("<I", read(4)); read(n); read(96)
out.write(b"O" + struct.pack("<I", 3) + struct.pack("<ff", 1000.0, 1000.0) * 3 + b"\0"); out.flush()
BOX = (1.0, 2.0, 3.0, 4.0)

if mode == "failed":
    chunk = bytearray()
    i = 0
    while True:
        chunk = b"".join(b"X" + struct.pack("<I", (i + k) & 0xffffffff) for k in range(100000))
        i += 100000
        out.write(chunk); out.flush()
elif mode == "pages":
    # The parent may first send an empty list; wait for one with a page.
    entries = wait_for(b"W")
    page, scale = entries[0]
    w = h = int(1000 * scale)
    frame = b"P" + struct.pack("<IfII", page, scale, w, h) + b"\x7f" * (w * h * 4)
    sys.stderr.write("frame %d bytes\n" % len(frame))
    while True:
        out.write(frame); out.flush()

# --- Text: page 0 is what the test asks for.
elif mode == "text_oversized":
    wait_for(b"T")
    working(0, 0, 0)
    # One byte more than 200 000 chars of four bytes, and the bytes to go
    # with it: the header is enough to refuse it.
    body = b"a" * 800_001
    out.write(b"T" + struct.pack("<III", 0, len(body), 200_000) + body)
elif mode == "text_chars_over":
    wait_for(b"T")
    working(0, 0, 0)
    out.write(b"T" + struct.pack("<III", 0, 200_001, 200_001) + b"a" * 200_001)
elif mode == "text_mismatch":
    wait_for(b"T")
    working(0, 0, 0)
    # "é" is one char; the header says two, and two boxes follow.
    text(0, "é".encode(), 2)
elif mode == "text_nan":
    wait_for(b"T")
    working(0, 0, 0)
    text(0, b"a", 1, b"\1" + struct.pack("<ffff", 0.0, float("nan"), 1.0, 1.0))
elif mode == "text_utf8":
    wait_for(b"T")
    working(0, 0, 0)
    text(0, b"\xc3\x28", 2)
elif mode == "text_wrong_page":
    wait_for(b"T")
    working(0, 0, 2)
elif mode == "text_out_of_range":
    wait_for(b"T")
    working(0, 0, 0)
    text(77, b"a", 1)
elif mode == "text_unasked":
    working(0, 0, 0)
    text(0, b"a", 1)
elif mode == "text_without_start":
    wait_for(b"T")
    text(0, b"a", 1)
elif mode == "text_flood":
    wait_for(b"T")
    frame = b"w" + bytes([0]) + struct.pack("<QI", 0, 0)
    while True:
        out.write(frame * 10000); out.flush()
elif mode == "text_twice":
    wait_for(b"T")
    while True:
        working(0, 0, 0); text(0, b"a", 1); out.flush()
elif mode == "text_hang":
    wait_for(b"T")
    working(0, 0, 0); out.flush()
    time.sleep(30)

# --- Outline
elif mode == "outline_huge":
    wait_for(b"L", False)
    working(1, 0, 0)
    out.write(b"L" + struct.pack("<I", 0xFFFFFFFF))
elif mode == "outline_many":
    wait_for(b"L", False)
    working(1, 0, 0)
    out.write(b"L" + struct.pack("<I", 10_001))
elif mode == "outline_deep":
    wait_for(b"L", False)
    working(1, 0, 0)
    out.write(b"L" + struct.pack("<I", 1) + struct.pack("<HBI", 32, 0, 1) + b"x")
elif mode == "outline_title":
    wait_for(b"L", False)
    working(1, 0, 0)
    out.write(b"L" + struct.pack("<I", 1) + struct.pack("<HBI", 0, 0, 2049))
elif mode == "outline_target":
    wait_for(b"L", False)
    working(1, 0, 0)
    out.write(b"L" + struct.pack("<I", 1) + struct.pack("<HBI", 0, 2, 1) + b"x" + struct.pack("<I", 3))
elif mode == "outline_unasked":
    working(1, 0, 0)
    out.write(b"L" + struct.pack("<I", 0))
elif mode == "outline_twice":
    wait_for(b"L", False)
    while True:
        working(1, 0, 0); out.write(b"L" + struct.pack("<I", 0)); out.flush()
elif mode == "outline_hang":
    wait_for(b"L", False)
    working(1, 0, 0); out.flush()
    time.sleep(30)

# --- Search
elif mode == "find_wrong_search":
    search = wait_for(b"Q", False)[0]
    working(2, search + 1, 0)
elif mode == "find_hits_wrong_search":
    search = wait_for(b"Q", False)[0]
    working(2, search, 0)
    hits(search + 1, 0, [(0, 1, [BOX])])
elif mode == "find_unknown_search":
    wait_for(b"Q", False)
    working(2, 987654321, 0)
elif mode == "find_hits_over_total":
    # The test sets the limit to 5 matches: 3 and 3.
    search = wait_for(b"Q", False)[0]
    working(2, search, 0); hits(search, 0, [(0, 1, [BOX])] * 3)
    working(2, search, 1); hits(search, 1, [(0, 1, [BOX])] * 3)
elif mode == "find_hits_in_a_frame":
    search = wait_for(b"Q", False)[0]
    working(2, search, 0); hits(search, 0, [(0, 1, [BOX])] * 6)
elif mode == "find_boxes_in_a_hit":
    search = wait_for(b"Q", False)[0]
    working(2, search, 0); hits(search, 0, [(0, 1, [BOX] * 257)])
elif mode == "find_nan":
    search = wait_for(b"Q", False)[0]
    working(2, search, 0); hits(search, 0, [(0, 1, [(0.0, float("inf"), 1.0, 1.0)])])
elif mode == "find_descending":
    search = wait_for(b"Q", False)[0]
    working(2, search, 1); searched(search, 1)
    working(2, search, 0)
elif mode == "find_page_range":
    search = wait_for(b"Q", False)[0]
    working(2, search, 3)
elif mode == "find_done_early":
    search = wait_for(b"Q", False)[0]
    working(2, search, 0); searched(search, 0)
    done(search, 0, 0)
elif mode == "find_done_total":
    search = wait_for(b"Q", False)[0]
    for p in range(3):
        working(2, search, p); searched(search, p)
    done(search, 7, 0)
elif mode == "find_done_twice":
    search = wait_for(b"Q", False)[0]
    for p in range(3):
        working(2, search, p); searched(search, p)
    done(search, 0, 0); done(search, 0, 0)
elif mode == "find_flood":
    search = wait_for(b"Q", False)[0]
    frame = b"w" + bytes([2]) + struct.pack("<QI", search, 0)
    while True:
        out.write(frame * 10000); out.flush()
elif mode == "find_hang":
    # The first child hangs on page 0; the next is asked to go on at page 1
    # and does.
    search, case, start, found, query = wait_for(b"Q", False)
    if start == 0:
        working(2, search, 0); out.flush()
        time.sleep(30)
    for p in range(start, 3):
        working(2, search, p); searched(search, p)
    done(search, found, 0)
elif mode == "find_replaced":
    # An honest child that is slow: it answers search 1's page only after
    # it has been told to stop.
    search = wait_for(b"Q", False)[0]
    working(2, search, 0); out.flush()
    request()
    hits(search, 0, [(0, 1, [BOX])])
    out.flush()
    # (and then nothing: it has been cancelled)

out.flush()
time.sleep(30)
