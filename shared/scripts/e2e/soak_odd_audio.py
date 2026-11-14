#!/usr/bin/env python3
"""Add malformed audio files to a soak_library.py library and refresh its manifest.

usage: python3 soak_odd_audio.py <library-dir>
The files cover headers that lie: zero sample rate, zero channels, zero bits, a data chunk larger than the file,
an fmt chunk cut short, a block align of zero, odd formats, a bare RIFF tag, truncated MP3/M4A/FLAC/OGG heads."""
import hashlib, json, os, struct, sys


def wav(rate=44100, channels=1, bits=16, data=b"\0\0" * 200, claim=None, fmt_size=16, block=None, tag=1, riff_size=None):
    block = channels * bits // 8 if block is None else block
    fmt = struct.pack("<HHIIHH", tag, channels, rate, rate * block, block, bits)[:fmt_size]
    d = b"data" + struct.pack("<I", len(data) if claim is None else claim) + data
    body = b"WAVE" + b"fmt " + struct.pack("<I", fmt_size) + fmt + d
    return b"RIFF" + struct.pack("<I", len(body) if riff_size is None else riff_size) + body


FILES = {
    "zero-rate.wav": wav(rate=0), "zero-channels.wav": wav(channels=0, block=0), "zero-bits.wav": wav(bits=0, block=2),
    "zero-block.wav": wav(block=0), "huge-claim.wav": wav(claim=0xFFFFFFF0), "huge-riff.wav": wav(riff_size=0xFFFFFFFF),
    "short-fmt.wav": wav(fmt_size=8), "no-data.wav": wav(data=b""), "one-byte.wav": wav(data=b"\1"),
    "float-tag.wav": wav(tag=3, bits=32, block=4, data=b"\0" * 400), "bad-tag.wav": wav(tag=0xFFFE),
    "rate-one.wav": wav(rate=1), "rate-max.wav": wav(rate=0xFFFFFFFF), "bits-odd.wav": wav(bits=7, block=1),
    "riff-only.wav": b"RIFF", "riff-wave-only.wav": b"RIFF\x04\0\0\0WAVE", "empty-ext.wav": b"",
    "garbage.wav": bytes(range(256)) * 8, "id3-only.mp3": b"ID3\x04\0\0\0\0\0\x10" + b"\0" * 16,
    "bad-frame.mp3": b"\xff\xfb\x00\x00" + b"\0" * 64, "trunc.flac": b"fLaC\0\0\0\x22" + b"\0" * 10,
    "trunc.ogg": b"OggS\0\x02" + b"\0" * 20, "trunc.m4a": b"\0\0\0\x20ftypM4A \0\0\0\0" + b"\0" * 8,
    "aiff-empty.aiff": b"FORM\0\0\0\x04AIFF", "caf-bad.caf": b"caff\0\x01\0\0" + b"\xff" * 16,
}

lib = os.path.abspath(sys.argv[1])
d = os.path.join(lib, "OddAudio")
os.makedirs(d, exist_ok=True)
for name, data in FILES.items():
    with open(os.path.join(d, name), "wb") as f:
        f.write(data)
manifest_path = lib.rstrip("/") + ".manifest.json"
m = json.load(open(manifest_path))
for name, data in FILES.items():
    m[f"OddAudio/{name}"] = hashlib.sha256(data).hexdigest()
json.dump(m, open(manifest_path, "w"), indent=0, sort_keys=True)
print(len(FILES), "odd audio files added;", len(m), "in manifest")
