# Shared test corpus

Small, deterministic, checked-in regression fixtures consumed by both platforms'
harnesses (`platforms/apple/scripts/iterate.sh`, `platforms/windows/build/iterate.ps1`)
and unit tests (e.g. the Windows `shell/ocr.rs` known-text test).

- `ocr/known-text.png` — 400×120 rendered "FileID OCR 12345" (generated via
  CoreGraphics; assert on the tokens in `assertions.json`, not the full string).
- `collisions/{a,b}/IMG_0001.jpg` — two distinct payloads sharing a basename;
  feeds the C2 no-clobber assertions.
- `unicode-names/manifest.json` — filename edge cases (NFC/NFD, emoji, RTL,
  bidi-override spoof, trailing space, long names). Harnesses create the files
  at runtime — git can't reliably carry these names across platforms.
- `assertions.json` — platform-neutral expected outcomes.

Keep fixtures tiny and rights-clear. No real personal photos.
