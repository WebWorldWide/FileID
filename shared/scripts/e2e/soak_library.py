#!/usr/bin/env python3
"""Generate a deterministic test library (about 3,000 real files) for soak.py.

usage: python3 soak_library.py <empty-dir> [--files N] [--seed S]

No private data and no third-party packages: PNGs are encoded with zlib, JPEGs are converted from PNGs with `sips`
(macOS; without it the JPEG slots are written as PNG-named JPEG stubs skipped), audio is written with the wave module,
PDF/DOCX/zip are built by hand. It plants byte-identical copies, near-duplicates, zero-byte, truncated and corrupt
images, unusual names, a dotfolder, a symlink loop and a read-only file, then writes <dir>/../<dir-name>.manifest.json
with every regular file's SHA-256 (the manifest lives outside the library so scanning never sees it)."""
import hashlib, json, os, random, shutil, struct, subprocess, sys, wave, zipfile, zlib, math

HERE = os.path.dirname(os.path.abspath(__file__))
MEDIA = os.path.join(HERE, "../../test-corpus/conformance-media")
WORDS = ("harbor lantern orchard compass velvet granite meadow copper falcon ribbon quartz thistle maple saffron "
         "glacier ember violet anchor prairie cobalt juniper willow summit harvest meridian pebble cinder lagoon "
         "tundra marble sparrow canyon lattice fennel basalt dune ivory kestrel nectar opal plume rivet sorrel "
         "talon umber vesper wren yarrow zephyr budget invoice recipe itinerary receipt contract lecture notes").split()


def chunk(kind, data):
    return struct.pack(">I", len(data)) + kind + data + struct.pack(">I", zlib.crc32(kind + data) & 0xFFFFFFFF)


def png(width, height, rng, tweak=0):
    """A banded picture: random vertical bands, each row sheared, with a noisy strip. `tweak` changes one pixel."""
    bands, x = [], 0
    while x < width:
        w = rng.randint(4, max(5, width // 3))
        bands.append((min(w, width - x), bytes(rng.randrange(256) for _ in range(3))))
        x += w
    base = b"".join(color * w for w, color in bands)
    slope = rng.choice((0, 1, 2, 3, -1, -2))
    rows = []
    for y in range(height):
        shift = (y * slope) % width * 3
        row = base[shift:] + base[:shift]
        if y % 23 < 3:
            row = rng.randbytes(width * 3)
        rows.append(b"\x00" + row)
    if tweak:
        row = bytearray(rows[height // 2])
        row[1 + (tweak % width) * 3] ^= 0x40
        rows[height // 2] = bytes(row)
    return (b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", width, height, 8, 2, 0, 0, 0))
            + chunk(b"IDAT", zlib.compress(b"".join(rows), 3)) + chunk(b"IEND", b""))


def pdf(text):
    stream = f"BT /F1 12 Tf 20 50 Td ({text}) Tj ET".encode()
    objects = [b"<< /Type /Catalog /Pages 2 0 R >>", b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
               b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 100] /Contents 4 0 R /Resources << /Font << /F1 5 0 R >> >> >>",
               b"<< /Length %d >>\nstream\n" % len(stream) + stream + b"\nendstream",
               b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>"]
    out, offsets = bytearray(b"%PDF-1.4\n"), []
    for n, body in enumerate(objects, 1):
        offsets.append(len(out))
        out += b"%d 0 obj\n" % n + body + b"\nendobj\n"
    xref = len(out)
    out += b"xref\n0 %d\n0000000000 65535 f \n" % (len(objects) + 1) + b"".join(b"%010d 00000 n \n" % o for o in offsets)
    out += b"trailer\n<< /Size %d /Root 1 0 R >>\nstartxref\n%d\n%%%%EOF\n" % (len(objects) + 1, xref)
    return bytes(out)


def docx(path, paragraphs):
    body = "".join(f"<w:p><w:r><w:t>{p}</w:t></w:r></w:p>" for p in paragraphs)
    with zipfile.ZipFile(path, "w", zipfile.ZIP_DEFLATED) as z:
        z.writestr("[Content_Types].xml", '<?xml version="1.0"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">'
                   '<Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>'
                   '<Default Extension="xml" ContentType="application/xml"/>'
                   '<Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>')
        z.writestr("_rels/.rels", '<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">'
                   '<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>')
        z.writestr("word/document.xml", '<?xml version="1.0"?><w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>'
                   + body + "</w:body></w:document>")


def sentence(rng, n=12):
    return " ".join(rng.choice(WORDS) for _ in range(n))


def main():
    root = os.path.abspath(sys.argv[1])
    total = int(sys.argv[sys.argv.index("--files") + 1]) if "--files" in sys.argv else 3000
    rng = random.Random(int(sys.argv[sys.argv.index("--seed") + 1]) if "--seed" in sys.argv else 20261114)
    assert not os.path.exists(root) or not os.listdir(root), "use an empty directory"
    os.makedirs(root, exist_ok=True)
    scale = total / 3000
    made = []

    def folder():
        top = rng.choice(["Photos", "Documents", "Projects", "Archive", "Downloads", "Media", "Work", "Family Trips"])
        sub = rng.choice(["2021", "2022", "2023", "2024", "misc", "Summer Trip", "taxes", "drafts", "Q1", "Q2", "shared", "old stuff"])
        deep = rng.choice(["", "", "", "a/b", "x/y/z", "inbox"])
        return os.path.join(root, top, sub, *([d for d in deep.split("/") if d]))

    def put(rel_dir, name, data):
        d = rel_dir if os.path.isabs(rel_dir) else os.path.join(root, rel_dir)
        os.makedirs(d, exist_ok=True)
        p = os.path.join(d, name)
        i = 1
        while os.path.exists(p):
            stem, ext = os.path.splitext(name)
            p = os.path.join(d, f"{stem}-{i}{ext}")
            i += 1
        with open(p, "wb") as f:
            f.write(data)
        made.append(p)
        return p

    n = lambda k: max(1, int(k * scale))
    pngs = []
    for i in range(n(900)):
        w, h = rng.choice((64, 96, 128, 160, 200)), rng.choice((64, 96, 128, 160, 200))
        pngs.append(put(folder(), f"IMG_{rng.randint(1000, 9999)}.png", png(w, h, rng)))
    # near-duplicates: same picture with one pixel changed
    for i in range(n(80)):
        w, h = 128, 128
        seed = rng.randrange(1 << 30)
        for t in (0, 1, 2):
            put(folder(), f"near-{seed % 1000}-{t}.png", png(w, h, random.Random(seed), tweak=t))
    # JPEGs through sips
    jpg_src = os.path.join(os.path.dirname(root), "soak-jpeg-src-" + os.path.basename(root))
    os.makedirs(jpg_src, exist_ok=True)
    for i in range(n(400)):
        w, h = rng.choice((96, 128, 192, 256)), rng.choice((96, 128, 192, 256))
        with open(os.path.join(jpg_src, f"{i}.png"), "wb") as f:
            f.write(png(w, h, rng))
    out = os.path.join(jpg_src, "out")
    os.makedirs(out, exist_ok=True)
    jpgs = []
    try:
        names = sorted(f for f in os.listdir(jpg_src) if f.endswith(".png"))
        for k in range(0, len(names), 100):
            subprocess.run(["sips", "-s", "format", "jpeg", "-s", "formatOptions", str(rng.choice((40, 70, 90)))]
                           + [os.path.join(jpg_src, x) for x in names[k:k + 100]] + ["--out", out],
                           capture_output=True, check=True)
        for x in sorted(os.listdir(out)):
            with open(os.path.join(out, x), "rb") as f:
                jpgs.append(put(folder(), f"DSC{rng.randint(10000, 99999)}.jpg", f.read()))
    finally:
        shutil.rmtree(jpg_src, ignore_errors=True)
    # exact duplicate groups
    pool = pngs + jpgs
    for g in range(n(60)):
        src = rng.choice(pool)
        data = open(src, "rb").read()
        for c in range(rng.randint(1, 3)):
            put(folder(), "copy-of-" + os.path.basename(src), data)
    # documents
    for i in range(n(300)):
        put(folder(), f"{rng.choice(WORDS)}-{i}.txt", (sentence(rng, 60) + "\n").encode())
    for i in range(n(150)):
        put(folder(), f"{rng.choice(WORDS)}-{i}.md", f"# {sentence(rng, 3)}\n\n{sentence(rng, 40)}\n\n- {sentence(rng, 5)}\n".encode())
    for i in range(n(100)):
        rows = "\n".join(",".join(str(rng.randint(0, 999)) for _ in range(5)) for _ in range(20))
        put(folder(), f"table-{i}.csv", ("a,b,c,d,e\n" + rows + "\n").encode())
    for i in range(n(100)):
        put(folder(), f"page-{i}.html", f"<html><head><title>{sentence(rng, 3)}</title></head><body><p>{sentence(rng, 30)}</p></body></html>".encode())
    for i in range(n(150)):
        put(folder(), f"{rng.choice(WORDS)}-report-{i}.pdf", pdf(sentence(rng, 6)))
    for i in range(n(100)):
        d = os.path.join(folder())
        os.makedirs(d, exist_ok=True)
        p = os.path.join(d, f"letter-{i}.docx")
        docx(p, [sentence(rng, 20) for _ in range(3)])
        made.append(p)
    for i in range(n(80)):
        d = folder()
        os.makedirs(d, exist_ok=True)
        p = os.path.join(d, f"bundle-{i}.zip")
        with zipfile.ZipFile(p, "w", zipfile.ZIP_DEFLATED) as z:
            for j in range(3):
                z.writestr(f"inner-{j}.txt", sentence(rng, 30))
        made.append(p)
    for i in range(n(40)):
        d = folder()
        os.makedirs(d, exist_ok=True)
        p = os.path.join(d, f"tone-{i}.wav")
        hz = rng.randint(200, 1200)
        with wave.open(p, "wb") as w:
            w.setnchannels(1); w.setsampwidth(2); w.setframerate(8000)
            w.writeframes(b"".join(struct.pack("<h", int(8000 * math.sin(2 * math.pi * hz * t / 8000))) for t in range(4000)))
        made.append(p)
    for name in sorted(os.listdir(MEDIA)):
        for k in range(2):
            put(os.path.join("Media", "clips"), f"{k}-{name}", open(os.path.join(MEDIA, name), "rb").read())
    # edge cases
    for i in range(n(20)):
        put("Edge/zero", f"empty-{i}.dat" if i % 2 else f"empty-{i}.jpg", b"")
    for i in range(n(20)):
        data = open(rng.choice(jpgs or pngs), "rb").read()
        put("Edge/truncated", f"cut-{i}.jpg" if jpgs else f"cut-{i}.png", data[:max(8, len(data) // 3)])
    for i in range(n(20)):
        put("Edge/corrupt", f"bad-{i}.jpg" if i % 2 else f"bad-{i}.png", rng.randbytes(rng.randint(100, 4000)))
    odd = ["with space and  double.txt", "Ünïcödé-naïve-café.txt", "日本語のファイル.txt", "emoji 🎉🙂 party.txt", "Ω≈ç√∫.md",
           "tab\tinname.txt", "trailing dot.txt.", "UPPER.TXT", "mixedCase.Md", "-leading-dash.txt", "a" * 200 + ".txt", "b" * 240 + ".md"]
    for name in odd:
        put("Edge/names", name, (sentence(rng, 20) + "\n").encode())
    for i in range(n(20)):
        put("Edge/names/Folder With Spaces 🎈", f"{i} file.txt", (sentence(rng, 10) + "\n").encode())
    for i in range(10):
        put(os.path.join(root, ".hidden-folder"), f"secret-{i}.txt", (sentence(rng, 10) + "\n").encode())
    ro = put("Edge/readonly", "locked.txt", b"read only file\n")
    os.chmod(ro, 0o444)
    os.makedirs(os.path.join(root, "Edge/loop/a"), exist_ok=True)
    put("Edge/loop/a", "inside.txt", b"inside loop\n")
    os.symlink("..", os.path.join(root, "Edge/loop/a/up"))
    os.symlink(os.path.join(root, "Edge/loop"), os.path.join(root, "Edge/loop/self"))
    os.symlink("does-not-exist", os.path.join(root, "Edge/loop/dangling"))

    # Page-shaped pictures (ISO A4 ratio, no camera data): the engine reads their text with the fast reader and owes
    # the careful read until the accurate reader has warmed up, so the soak's catch-up checks have something to settle.
    # A separate generator keeps every other file of the library as it was.
    paper = random.Random(4242)
    for i in range(n(40)):
        w = paper.choice((297, 420, 594))
        h = round(w * 2 ** 0.5)
        put("Paper", f"scan-{i}.png", png(w, h, paper) if i % 2 else png(h, w, paper))

    manifest = {}
    for dp, dns, fns in os.walk(root, followlinks=False):
        for fn in fns:
            p = os.path.join(dp, fn)
            if os.path.islink(p):
                continue
            with open(p, "rb") as f:
                manifest[os.path.relpath(p, root)] = hashlib.sha256(f.read()).hexdigest()
    mpath = os.path.join(os.path.dirname(root), os.path.basename(root) + ".manifest.json")
    with open(mpath, "w") as f:
        json.dump(manifest, f, indent=0, sort_keys=True)
    print(f"{len(manifest)} files, manifest {mpath}")


if __name__ == "__main__":
    main()
