#!/usr/bin/env python3
"""Reference implementation of docs/cattail.md, used to generate
docs/cattail-vectors.json. It is written apart from the Rust code of the
endpoint, so the vectors check two independent implementations.

Usage: python3 -I scripts/cattail_vectors.py > docs/cattail-vectors.json
Needs the "cryptography" package (Debian: python3-cryptography).
"""
import base64
import json
import random
import sys

from cryptography.hazmat.primitives import hashes
from cryptography.hazmat.primitives.ciphers.aead import AESGCM
from cryptography.hazmat.primitives.kdf.hkdf import HKDF

SEED = 1595
HEADER_LEN = 32
TAG_LEN = 16
OPEN = 0x01
DATA = 0x02
VERSION = 0x01
C2S = 0x01
S2C = 0x02
PRINTABLE = bytes(range(0x21, 0x7F))


def hkdf(salt, ikm, info):
    return HKDF(algorithm=hashes.SHA256(), length=16, salt=salt, info=info).derive(ikm)


def aead_nonce(direction, n):
    return bytes([direction, 0, 0, 0]) + n.to_bytes(8, "big")


class Sealer:
    def __init__(self, key, direction):
        self.aead = AESGCM(key)
        self.direction = direction
        self.n = 0

    def seal(self, plain):
        out = self.aead.encrypt(aead_nonce(self.direction, self.n), plain, None)
        self.n += 1
        return out


def header(typ, body_len, mid_len, end_len, ts=0, identity=bytes(8), counter=0):
    return (
        bytes([typ, VERSION])
        + ts.to_bytes(8, "big")
        + identity
        + counter.to_bytes(6, "big")
        + body_len.to_bytes(2, "big")
        + mid_len.to_bytes(2, "big")
        + end_len.to_bytes(2, "big")
        + bytes(2)
    )


def make_nonce(rng, printable):
    nonce = bytearray(rng.randbytes(24))
    for i in range(printable):
        nonce[i] = PRINTABLE[rng.randrange(len(PRINTABLE))]
    return bytes(nonce)


def frame(sealer, rng, typ, body, mid_len, end_len, **open_fields):
    plain = header(typ, len(body), mid_len, end_len, **open_fields)
    mid = rng.randbytes(mid_len)
    end = rng.randbytes(end_len)
    wire = sealer.seal(plain) + mid
    if body:
        wire += sealer.seal(body)
    wire += end
    return {
        "type": typ,
        "body": body.hex(),
        "mid_padding": mid.hex(),
        "end_padding": end.hex(),
        "header_plain": plain.hex(),
        "wire": wire.hex(),
    }, wire


def session(name, rng, key, now, client_printable, c2s_plan, s2c_plan):
    n_c = make_nonce(rng, client_printable)
    n_s = make_nonce(rng, 0)
    identity = rng.randbytes(8)
    counter = rng.randrange(1 << 20)
    k_c2s = hkdf(n_c, key, b"cattail-v1 c2s")
    k_s2c = hkdf(n_c + n_s, key, b"cattail-v1 s2c")
    c2s = Sealer(k_c2s, C2S)
    s2c = Sealer(k_s2c, S2C)

    c2s_frames, c2s_wire = [], n_c
    for i, (body, mid_len, end_len) in enumerate(c2s_plan):
        if i == 0:
            f, w = frame(c2s, rng, OPEN, body, mid_len, end_len,
                         ts=now, identity=identity, counter=counter)
        else:
            f, w = frame(c2s, rng, DATA, body, mid_len, end_len)
        c2s_frames.append(f)
        c2s_wire += w

    s2c_frames, s2c_wire = [], n_s
    for body, mid_len, end_len in s2c_plan:
        f, w = frame(s2c, rng, DATA, body, mid_len, end_len)
        s2c_frames.append(f)
        s2c_wire += w

    return {
        "name": name,
        "key": key.hex(),
        "key_base64": base64.b64encode(key).decode(),
        "client_nonce": n_c.hex(),
        "server_nonce": n_s.hex(),
        "key_c2s": k_c2s.hex(),
        "key_s2c": k_s2c.hex(),
        "timestamp": now,
        "identity": identity.hex(),
        "counter": counter,
        "c2s_frames": c2s_frames,
        "c2s_wire": c2s_wire.hex(),
        "c2s_plain": b"".join(bytes.fromhex(f["body"]) for f in c2s_frames).hex(),
        "s2c_frames": s2c_frames,
        "s2c_wire": s2c_wire.hex(),
        "s2c_plain": b"".join(bytes.fromhex(f["body"]) for f in s2c_frames).hex(),
    }


class ReplayWindow:
    SIZE = 1024

    def __init__(self):
        self.ids = {}

    def accept(self, identity, c):
        state = self.ids.get(identity)
        if state is None:
            self.ids[identity] = [c, {c}]
            return True
        top, seen = state
        if c > top:
            state[0] = c
            state[1] = {x for x in seen if c - x < self.SIZE} | {c}
            return True
        if top - c >= self.SIZE or c in seen:
            return False
        seen.add(c)
        return True


def timestamp_ok(now, start, ts):
    return abs(now - ts) <= 120 and ts >= start


def main():
    rng = random.Random(SEED)
    now = 1795219200  # 2026-11-20 00:00:00 UTC
    key = rng.randbytes(32)
    fake_client_hello = bytes([0x16, 0x03, 0x01, 0x07, 0x00]) + rng.randbytes(259)
    fake_server_hello = bytes([0x16, 0x03, 0x03, 0x00, 0x7a]) + rng.randbytes(295)

    sessions = [
        session("basic", rng, key, now, 0,
                [(fake_client_hello, 13, 29), (rng.randbytes(50), 0, 0), (b"", 5, 7)],
                [(fake_server_hello, 7, 0), (b"\x17", 0, 40)]),
        session("printable_nonce", rng, key, now, 10,
                [(b"\x16", 0, 0)],
                [(b"\x16\x03", 3, 3)]),
        session("padding_only_open", rng, key, now - 120, 0,
                [(b"", 0, 100), (fake_client_hello[:64], 1, 2)],
                [(b"", 20, 0), (b"\x16", 0, 0)]),
    ]

    basic = sessions[0]
    wire = bytearray.fromhex(basic["c2s_wire"])
    wire[24 + 47] ^= 0x01  # the last byte of the tag of the OPEN header
    other_key = rng.randbytes(32)
    rejects = [
        {"name": "header_tag_flipped", "key": basic["key"], "c2s_wire": wire.hex()},
        {"name": "other_key", "key": other_key.hex(), "c2s_wire": basic["c2s_wire"]},
    ]

    start = now - 60
    timestamps = [
        {"now": now, "start": start, "timestamp": ts, "accept": timestamp_ok(now, start, ts)}
        for ts in [now, now + 120, now - 60, now + 121, now - 61, now - 121]
    ]
    timestamps += [
        {"now": now, "start": now - 600, "timestamp": ts,
         "accept": timestamp_ok(now, now - 600, ts)}
        for ts in [now - 120, now - 121]
    ]

    window = ReplayWindow()
    replay = []
    for identity, c in [("aa", 5), ("aa", 5), ("aa", 3), ("bb", 5), ("aa", 1029),
                        ("aa", 4), ("aa", 6), ("aa", 6), ("aa", 5), ("aa", 1028),
                        ("aa", 3000), ("aa", 1029)]:
        ident = bytes.fromhex(identity * 8)
        replay.append({"identity": ident.hex(), "counter": c, "accept": window.accept(ident, c)})

    patterns = [
        {"pattern": "stop=3\n0=400-1400,c,200-900\n1=300-1200,c,100-700\n2=100-600,c\nmid=0-32\nend=0-96",
         "valid": True,
         "parsed": {"stop": 3, "lines": {"0": [[400, 1400], "c", [200, 900]],
                                         "1": [[300, 1200], "c", [100, 700]],
                                         "2": [[100, 600], "c"]},
                    "mid": [0, 32], "end": [0, 96], "nonce": [0, 0]}},
        {"pattern": "stop=1;0=64;nonce=8-12;future_key=whatever",
         "valid": True,
         "parsed": {"stop": 1, "lines": {"0": [[64, 64]]},
                    "mid": [0, 0], "end": [0, 0], "nonce": [8, 12]}},
        {"pattern": "", "valid": True,
         "parsed": {"stop": 0, "lines": {}, "mid": [0, 0], "end": [0, 0], "nonce": [0, 0]}},
        {"pattern": "stop=17", "valid": False},
        {"pattern": "stop=1\n0=47", "valid": False},
        {"pattern": "stop=1\n0=100-50", "valid": False},
        {"pattern": "stop=1\n0=16449", "valid": False},
        {"pattern": "mid=0-4097", "valid": False},
        {"pattern": "nonce=0-25", "valid": False},
        {"pattern": "stop=1\n0=x", "valid": False},
        {"pattern": "stop=1\n0=" + ",".join(["c"] * 17), "valid": False},
    ]

    out = {
        "spec": "docs/cattail.md, version 1",
        "generator": "scripts/cattail_vectors.py (Python cryptography), seed %d" % SEED,
        "note": "padding bytes, nonces and identities come from random.Random(seed); "
                "every frame lists its own fields, so an encoder can be checked "
                "without reproducing that generator",
        "sessions": sessions,
        "rejects": rejects,
        "timestamps": timestamps,
        "replay": replay,
        "patterns": patterns,
    }
    json.dump(out, sys.stdout, indent=1)
    sys.stdout.write("\n")


if __name__ == "__main__":
    main()
