# Security Policy

## Reporting a vulnerability

**Please do not open a public issue for a security problem.**

Report it privately through GitHub:
[**Report a vulnerability**](https://github.com/WebWorldWide/FileID/security/advisories/new)
(Security → Advisories → Report a vulnerability). That opens a private thread
visible only to you and the maintainers.

Helpful things to include, when you have them:

- Which front-end and version (`fileid --version`, or Settings → Engine info)
- OS and architecture
- A minimal reproduction — a crafted file, a command, or a sequence of steps
- What an attacker gains, and whether it needs local access or a user action

Reports are read as time allows; this is a small project, so please don't expect
same-day turnaround. You'll get an acknowledgement, and a heads-up before any
advisory naming you is published. If you'd rather not be credited, say so.

## Supported versions

FileID is pre-1.0 and ships as **unsigned prereleases**. Only the latest release
and `main` receive fixes — there are no backports to older tags.

| Version | Supported |
| :-- | :-- |
| `main` | ✅ |
| Latest release | ✅ |
| Anything older | ❌ |

## Scope

FileID runs entirely on-device. There is no FileID server, account, or hosted
API, so there is no remote attack surface to report against. The interesting
boundaries are local, and are documented in detail — including the threat model,
what's enforced today, and what's still open — in
[`shared/docs/SECURITY.md`](shared/docs/SECURITY.md).

Things that are **in scope**:

- Malicious file content that exploits the scan pipeline (image/PDF/archive
  decoders, OCR, face detection, the VLM, hashing)
- Escaping the Restructure/rename path containment, or otherwise getting FileID
  to write, move, or delete outside the user-selected library root
- Defeating the engine-binary integrity check the app performs before spawn
- Tampering with a model or runtime download (transport, digest, or redirect
  handling)
- Any network egress beyond a user-initiated model download — the project ships
  **no telemetry** and treats a violation as a release blocker
  (see [`shared/docs/PRIVACY.md`](shared/docs/PRIVACY.md))

Things that are **out of scope**:

- The absence of Authenticode/notarization signatures on prerelease builds —
  this is known, documented, and tracked in
  [`shared/docs/SHIP.md`](shared/docs/SHIP.md)
- Vulnerabilities in third-party model weights or in tools you supply on `PATH`
  (`llama-mtmd-cli`, `tesseract`, `ffmpeg`, HEIC converters). These sit inside
  your local trust boundary — report those upstream.
- An attacker who already has code execution as your user account, or
  administrator/root on the machine
