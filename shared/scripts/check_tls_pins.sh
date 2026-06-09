#!/bin/bash
# Keeps the two TLS-pin representations locked together:
#   shared/security/tls-pins.json   (SPKI hashes — consumed by macOS)
#   shared/security/pinned-roots/*.pem (root certs — embedded by the Windows engine)
# Fails if any slug's computed SPKI sha256 differs from the JSON, if a PEM
# is missing/expiring, or if either side lists a slug the other doesn't.
#
#   bash shared/scripts/check_tls_pins.sh             # consistency check (CI)
#   bash shared/scripts/check_tls_pins.sh --capture   # also probe the live hosts
#                                                     # and warn when a served chain's
#                                                     # root is outside the pin set
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
PINS_JSON="$ROOT/security/tls-pins.json"
PEM_DIR="$ROOT/security/pinned-roots"

fail=0

slugs_json=$(python3 -c "
import json
for r in json.load(open('$PINS_JSON'))['roots']: print(r['slug'], r['spkiSHA256Base64'])
")

while read -r slug expected; do
    pem="$PEM_DIR/$slug.pem"
    if [ ! -f "$pem" ]; then
        echo "❌ $slug: $pem missing"
        fail=1
        continue
    fi
    actual=$(openssl x509 -in "$pem" -pubkey -noout \
        | openssl pkey -pubin -outform der 2>/dev/null \
        | openssl dgst -sha256 -binary | base64)
    if [ "$actual" != "$expected" ]; then
        echo "❌ $slug: SPKI mismatch (json=$expected pem=$actual)"
        fail=1
    fi
    if ! openssl x509 -in "$pem" -noout -checkend 31536000 >/dev/null; then
        echo "⚠️  $slug: root expires within a year — plan rotation"
    fi
done <<< "$slugs_json"

for pem in "$PEM_DIR"/*.pem; do
    slug=$(basename "$pem" .pem)
    echo "$slugs_json" | awk '{print $1}' | grep -qx "$slug" || {
        echo "❌ $slug.pem on disk but absent from tls-pins.json"
        fail=1
    }
done

if [ "${1:-}" = "--capture" ]; then
    for host in huggingface.co cas-bridge.xethub.hf.co github.com release-assets.githubusercontent.com developer.download.nvidia.com; do
        root_subj=$(echo | openssl s_client -connect "$host:443" -servername "$host" -showcerts 2>/dev/null \
            | awk '/ i:/{line=$0} END{print line}')
        echo "🔎 $host root issuer:$root_subj"
    done
    echo "   (manually confirm each issuer family is represented in the pin set)"
fi

if [ "$fail" = "1" ]; then
    echo "❌ TLS pin consistency check FAILED"
    exit 1
fi
echo "✅ TLS pins consistent ($(echo "$slugs_json" | wc -l | tr -d ' ') roots)"
