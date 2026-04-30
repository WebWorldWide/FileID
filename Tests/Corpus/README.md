# Tests/Corpus/

Auto-generated test corpus. **Not committed** to the repo (see `.gitignore`).
Run `bash scripts/build_corpus.sh` to populate.

## Why

Exercises every track in FileID's pipeline against a known-good fixture so
`scripts/iterate.sh` can detect regressions automatically.

## Structure

The layout is intentionally messy to validate Restructure's three-tier
assistant logic. After scan + clustering:

| Folder | Tier | What it tests |
|--------|------|---------------|
| `Albert Einstein/`            | Anchor (named person) | Person clustering; folder name preservation |
| `Marie Curie/`                | Anchor                | Multi-person discrimination |
| `Nikola Tesla/`               | Anchor                | Anchor stays put across re-runs |
| `2019/`                       | Anchor (time)         | Year-folder detection |
| `Marie Curie's Laboratory/`   | Mixed                 | Meaningful name + outlier — should KEEP folder, MOVE outlier |
| `Untitled folder/`            | Junk                  | Cleanup duplicates; Restructure dissolves it |
| `Camera Roll/`                | Junk                  | Dissolve + re-bucket via heuristic |

## Sources & attribution

All images are public-domain photos sourced from Wikimedia Commons
(historical figures whose works are now PD because the photographers
died > 70 years ago) and NASA (PD by US-government policy).

Image attribution is recorded in `scripts/build_corpus.sh` next to each
download URL. The script verifies SHA-256 checksums on download to fail
fast if Wikipedia rotates a file.
