#!/usr/bin/env python3
"""A cargo registry for the integration tests: one crate, served from this
machine over the sparse protocol.

    registry.py make <dir> <port>     write the index and the crate file
    registry.py serve <dir> <port>    serve them over HTTP until killed

The crate file is written byte by byte, so that it is the same on every
machine and its checksum can stand in the fixture's Cargo.lock.
"""

import hashlib
import http.server
import json
import os
import struct
import sys
import zlib

NAME = "rostnix-fixture-dep"
VERSION = "0.1.0"
FILES = {
    "Cargo.toml": (
        "[package]\n"
        f'name = "{NAME}"\n'
        f'version = "{VERSION}"\n'
        'edition = "2021"\n'
        'description = "A crate of the registry the rostnix tests serve"\n'
        'license = "MIT"\n'
    ),
    "src/lib.rs": "pub fn answer() -> u32 {\n    42\n}\n",
}


def tar_entry(path, data):
    """One file of a ustar archive: its header and its padded contents."""

    def field(text, size):
        return text.encode().ljust(size, b"\0")

    def header(checksum):
        return b"".join(
            [
                field(path, 100),
                field("0000644", 8),
                field("0000000", 8),
                field("0000000", 8),
                field("%011o" % len(data), 12),
                field("%011o" % 0, 12),
                checksum,
                b"0",
                field("", 100),
                b"ustar\0",
                b"00",
                field("", 32),
                field("", 32),
                field("", 8),
                field("", 8),
                field("", 155),
                field("", 12),
            ]
        )

    total = sum(header(b" " * 8))
    padding = b"\0" * (-len(data) % 512)
    return header(b"%06o\0 " % total) + data + padding


def gzip_stored(data):
    """A gzip file that holds the data uncompressed: no compressor has a say
    in its bytes."""
    assert len(data) < 65536
    block = b"\x01" + struct.pack("<HH", len(data), len(data) ^ 0xFFFF) + data
    trailer = struct.pack("<II", zlib.crc32(data), len(data))
    return b"\x1f\x8b\x08\x00\x00\x00\x00\x00\x00\xff" + block + trailer


def crate():
    archive = b"".join(
        tar_entry(f"{NAME}-{VERSION}/{path}", text.encode())
        for path, text in sorted(FILES.items())
    )
    return gzip_stored(archive + b"\0" * 1024)


def make(root, port):
    data = crate()
    checksum = hashlib.sha256(data).hexdigest()
    base = f"http://127.0.0.1:{port}"

    index = os.path.join(root, "index")
    entry = os.path.join(index, NAME[0:2], NAME[2:4])
    crates = os.path.join(root, "crates", NAME)
    os.makedirs(entry, exist_ok=True)
    os.makedirs(crates, exist_ok=True)
    with open(os.path.join(index, "config.json"), "w") as out:
        json.dump({"dl": base + "/crates/{crate}/{crate}-{version}.crate", "api": base}, out)
    with open(os.path.join(entry, NAME), "w") as out:
        line = {
            "name": NAME,
            "vers": VERSION,
            "deps": [],
            "cksum": checksum,
            "features": {},
            "yanked": False,
        }
        out.write(json.dumps(line) + "\n")
    with open(os.path.join(crates, f"{NAME}-{VERSION}.crate"), "wb") as out:
        out.write(data)
    print(checksum)


class Quiet(http.server.SimpleHTTPRequestHandler):
    def log_message(self, *args):
        pass


def serve(root, port):
    os.chdir(root)
    http.server.ThreadingHTTPServer(("127.0.0.1", port), Quiet).serve_forever()


if __name__ == "__main__":
    command, root, port = sys.argv[1], sys.argv[2], int(sys.argv[3])
    {"make": make, "serve": serve}[command](root, port)
