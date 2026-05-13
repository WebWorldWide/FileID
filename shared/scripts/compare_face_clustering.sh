#!/usr/bin/env bash
# compare_face_clustering.sh — verify mac/Windows engines produce
# topologically equivalent face clusters on the same library.
#
# Usage:
#   shared/scripts/compare_face_clustering.sh <mac.sqlite> <windows.sqlite>
#
# Reads `face_verifications` from each SQLite file and reports:
#   - cluster_count per platform
#   - per-person face counts (top 20)
#   - Jaccard similarity of the face_id -> cluster_id mapping
#     (after stable cluster-id remapping by largest-overlap-first
#      bipartite matching).
#
# Exits non-zero if cluster_count diverges by >10% OR Jaccard < 0.85.
#
# Requirements: sqlite3 + jq + awk (all standard on macOS and Git Bash).

set -euo pipefail

if [[ $# -ne 2 ]]; then
    echo "usage: $0 <mac.sqlite> <windows.sqlite>" >&2
    exit 2
fi

mac_db="$1"
win_db="$2"

for db in "$mac_db" "$win_db"; do
    [[ -f "$db" ]] || { echo "ERROR: $db not found" >&2; exit 2; }
done

for tool in sqlite3 jq awk; do
    command -v "$tool" >/dev/null || { echo "ERROR: $tool required" >&2; exit 2; }
done

dump() {
    sqlite3 "$1" \
        "SELECT face_id, cluster_id FROM face_verifications WHERE cluster_id IS NOT NULL ORDER BY face_id;"
}

mac_pairs="$(dump "$mac_db")"
win_pairs="$(dump "$win_db")"

mac_cluster_count="$(echo "$mac_pairs" | awk -F'|' '{print $2}' | sort -u | wc -l | tr -d ' ')"
win_cluster_count="$(echo "$win_pairs" | awk -F'|' '{print $2}' | sort -u | wc -l | tr -d ' ')"

cluster_count_drift_pct=0
if [[ "$mac_cluster_count" -gt 0 ]]; then
    cluster_count_drift_pct=$(awk -v a="$mac_cluster_count" -v b="$win_cluster_count" \
        'BEGIN{d=(b-a)/a*100; if (d<0) d=-d; printf "%.1f", d}')
fi

# Build face_id -> cluster_id maps; Jaccard over the equivalence
# relation "same cluster on this platform" so cluster-id ordering
# doesn't matter.
jaccard="$(python3 - "$mac_db" "$win_db" <<'PY'
import sqlite3, sys
mac_db, win_db = sys.argv[1], sys.argv[2]
def load(db):
    c = sqlite3.connect(db).cursor()
    c.execute("SELECT face_id, cluster_id FROM face_verifications WHERE cluster_id IS NOT NULL")
    return {fid: cid for fid, cid in c.fetchall()}
m = load(mac_db); w = load(win_db)
common = set(m) & set(w)
if len(common) < 2:
    print("NA"); sys.exit()
same_mac  = {frozenset((a,b)) for a in common for b in common if a < b and m[a] == m[b]}
same_win  = {frozenset((a,b)) for a in common for b in common if a < b and w[a] == w[b]}
inter = same_mac & same_win
union = same_mac | same_win
print(f"{len(inter)/len(union):.3f}" if union else "1.000")
PY
)"

echo "Mac   cluster_count: $mac_cluster_count"
echo "Win   cluster_count: $win_cluster_count"
echo "Drift              : ${cluster_count_drift_pct}%"
echo "Jaccard (same-cluster pairs over face_id intersection): $jaccard"

# Top-20 per-person face counts side-by-side.
echo
echo "Top-20 cluster sizes (mac | win):"
paste \
    <(echo "$mac_pairs" | awk -F'|' '{print $2}' | sort | uniq -c | sort -rn | head -20) \
    <(echo "$win_pairs" | awk -F'|' '{print $2}' | sort | uniq -c | sort -rn | head -20) \
    | awk '{printf "  %5s %-6s | %5s %-6s\n", $1, $2, $3, $4}'

fail=0
awk -v d="$cluster_count_drift_pct" 'BEGIN{exit (d > 10)}' || {
    echo "FAIL: cluster_count drift > 10%"; fail=1
}
if [[ "$jaccard" != "NA" ]]; then
    awk -v j="$jaccard" 'BEGIN{exit (j < 0.85)}' || {
        echo "FAIL: Jaccard < 0.85"; fail=1
    }
fi
exit $fail
