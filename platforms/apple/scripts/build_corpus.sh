#!/bin/bash
# Build the FileID test corpus.
# Idempotent: re-running skips files already present + verified.
#
# Sources:
#  - Wikimedia Commons portraits (PD-old: photographers died > 70y ago)
#  - NASA Image Library (PD-NASA)
#  - Project Gutenberg (PD)
#
# Layout (see Tests/Corpus/README.md):
#  Albert Einstein/   — anchor folder, multiple PD photos of one person
#  Marie Curie/       — anchor folder
#  Nikola Tesla/      — anchor folder
#  2019/              — time anchor (NASA Apollo 11 anniversary photos)
#  Untitled folder/   — junk (mixed contents incl. a near-duplicate)
#  Camera Roll/       — junk (with another near-duplicate)

set -uo pipefail
# Don't `set -e` — individual download failures shouldn't abort the whole
# script. Wikipedia file names change occasionally; we tolerate misses
# and report at the end.

PROJECT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
CORPUS="$PROJECT_DIR/Tests/Corpus"

# Wikimedia's Special:FilePath endpoint redirects to the highest-resolution
# upload, with optional ?width= for a thumbnail. We pin sizes to keep
# downloads modest (~50-100 KB per face image).
WM="https://commons.wikimedia.org/wiki/Special:FilePath"

mkdir -p "$CORPUS"

# fetch <url> <dest> — idempotent download with retry. Skips if dest exists.
fetch() {
    local url="$1"
    local dest="$2"
    if [ -s "$dest" ]; then
        return 0   # already present
    fi
    mkdir -p "$(dirname "$dest")"
    if ! curl --silent --show-error --location --fail \
              --max-time 60 \
              --user-agent "FileID-Corpus-Builder/1.0 (https://github.com/AdamNolle/FileID)" \
              --output "$dest" \
              "$url"; then
        echo "  ✗ failed: $url" >&2
        rm -f "$dest"
        return 1
    fi
    if [ ! -s "$dest" ]; then
        echo "  ✗ empty: $url" >&2
        rm -f "$dest"
        return 1
    fi
    echo "  ✓ $(basename "$dest")  ($(du -h "$dest" | cut -f1))"
}

echo "Building test corpus at $CORPUS"
echo

# ─── Albert Einstein (PD-old, 4 distinct photos) ─────────────────────
echo "[1/6] Albert Einstein/"
fetch "$WM/Albert%20Einstein%20Head.jpg?width=600"                  "$CORPUS/Albert Einstein/einstein_1.jpg"
fetch "$WM/Einstein%201921%20by%20F%20Schmutzer%20-%20restoration.jpg?width=600"  "$CORPUS/Albert Einstein/einstein_2.jpg"
fetch "$WM/Albert%20Einstein%20%28Nobel%29.png?width=600"           "$CORPUS/Albert Einstein/einstein_3.jpg"
fetch "$WM/Albert%20Einstein%20photo%201921.jpg?width=600"           "$CORPUS/Albert Einstein/einstein_4.jpg"

# ─── Marie Curie (PD-old, 3 photos) ──────────────────────────────────
echo
echo "[2/6] Marie Curie/"
fetch "$WM/Marie%20Curie%20c1920.jpg?width=600"                     "$CORPUS/Marie Curie/curie_1.jpg"
fetch "$WM/Marie%20Curie%201903.jpg?width=600"                       "$CORPUS/Marie Curie/curie_2.jpg"
fetch "$WM/Marie%20Curie%201920.png?width=600"                       "$CORPUS/Marie Curie/curie_3.jpg"

# ─── Nikola Tesla (PD-old, 3 photos) ─────────────────────────────────
echo
echo "[3/6] Nikola Tesla/"
fetch "$WM/N.Tesla.JPG?width=600"                                    "$CORPUS/Nikola Tesla/tesla_1.jpg"
fetch "$WM/Tesla%20circa%201890.jpeg?width=600"                      "$CORPUS/Nikola Tesla/tesla_2.jpg"
fetch "$WM/Tesla3.jpg?width=600"                                     "$CORPUS/Nikola Tesla/tesla_3.jpg"

# ─── Time anchor: 2019 ────────────────────────────────────────────────
echo
echo "[4/6] 2019/"
fetch "$WM/Apollo%2011%20Crew.jpg?width=800"                         "$CORPUS/2019/apollo_crew.jpg"
fetch "$WM/Saturn%20V%20on%20launch%20pad%2C%20Apollo%2011%20mission.jpg?width=800"   "$CORPUS/2019/saturn_v.jpg"

# ─── Junk folder #1: Untitled folder/ (with near-duplicates) ─────────
echo
echo "[5/6] Untitled folder/  (junk + near-dup)"
# Different file in this junk folder so we get a different perceptual hash
fetch "$WM/Albert%20Einstein%201947a.jpg?width=600"                  "$CORPUS/Untitled folder/IMG_0001.jpg"
fetch "$WM/Earthrise.jpg?width=800"                                  "$CORPUS/Untitled folder/IMG_0002.jpg"
# Near-duplicate: same source as einstein_1 but different size → tests phash
fetch "$WM/Albert%20Einstein%20Head.jpg?width=400"                   "$CORPUS/Untitled folder/IMG_0003.jpg"

# ─── Junk folder #2: Camera Roll/ (more files, another near-dup) ─────
echo
echo "[6/7] Camera Roll/  (junk + near-dup)"
fetch "$WM/Pillars%20of%20creation%202014%20HST%20WFC3-UVIS%20full-res%20denoised.jpg?width=800"  "$CORPUS/Camera Roll/IMG_0010.jpg"
# Near-dup of curie_1 at smaller size
fetch "$WM/Marie%20Curie%20c1920.jpg?width=400"                      "$CORPUS/Camera Roll/IMG_0011.jpg"

# ─── Mixed-tier folder: meaningful name + 1 outlier ──────────────────
# A folder named for a topic that mostly-matches its contents but has a
# stray outlier file. The Restructure assistant should KEEP the folder
# (name is meaningful, ≥ 60% of content shares the dominant identity)
# and DISSOLVE just the outlier into its proper anchor.
echo
echo "[7/7] Marie Curie's Laboratory/  (Mixed tier — meaningful name + 1 outlier)"
MIXED="$CORPUS/Marie Curie's Laboratory"
mkdir -p "$MIXED"
# Copy 2 Curie photos + 1 Einstein outlier from the anchor downloads.
# `cp -n` skips if dest already exists (idempotent re-run).
if [ -f "$CORPUS/Marie Curie/curie_1.jpg" ]; then
    cp -n "$CORPUS/Marie Curie/curie_1.jpg"   "$MIXED/lab_photo_1.jpg" 2>/dev/null || true
    echo "  ✓ lab_photo_1.jpg  (Curie)"
fi
if [ -f "$CORPUS/Marie Curie/curie_2.jpg" ]; then
    cp -n "$CORPUS/Marie Curie/curie_2.jpg"   "$MIXED/lab_photo_2.jpg" 2>/dev/null || true
    echo "  ✓ lab_photo_2.jpg  (Curie)"
fi
if [ -f "$CORPUS/Albert Einstein/einstein_1.jpg" ]; then
    cp -n "$CORPUS/Albert Einstein/einstein_1.jpg"  "$MIXED/visitor.jpg" 2>/dev/null || true
    echo "  ✓ visitor.jpg     (Einstein — outlier)"
fi

# ─── Summary ─────────────────────────────────────────────────────────
echo
TOTAL=$(find "$CORPUS" -type f ! -name 'README.md' ! -name '.DS_Store' | wc -l | tr -d ' ')
SIZE=$(du -sh "$CORPUS" | cut -f1)
echo "Done — $TOTAL files, $SIZE total."
echo "Folders:"
find "$CORPUS" -mindepth 1 -maxdepth 1 -type d | sed 's|.*/|  |'
