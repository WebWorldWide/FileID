# Mac App Store submission notes (draft for the owner)

Draft text for App Store Connect. Nothing here has been submitted; the owner pastes, edits and
answers each item. Facts below were checked against the code on 2026-11-04 (sandboxed App Store
flavor, `FILEID_APP_STORE`).

## App Review notes (paste into "Notes" under App Review Information)

FileID organizes photos, videos and documents entirely on this Mac. It reads only the folders
the user chooses in the standard open panel (security-scoped bookmarks keep that access across
launches). It never uploads files, and has no accounts, analytics or crash reporting.

The only network use is downloading optional on-device AI models when the user asks for them
(Settings, or the Describe Files page). Every download comes from huggingface.co and is checked
against a pinned SHA-256 before use. All default models are Apache-2.0 or MIT licensed; Gemma
models show Google's terms and download only after the user accepts them.

To review without downloading anything: choose any folder of photos from the welcome screen or
File ▸ Choose Folder… (⌘O). Scanning, tagging, duplicate detection and keyword search work with
the built-in Apple Vision models. Optional downloads add features: People (face grouping) needs
the 37 MB SFace model, search by description needs the CLIP model (about 600 MB), and "Describe
Files" needs a vision-language model (about 3.5 GB for Qwen3-VL 4B, the recommendation for
8 GB Macs).

Moving files: "Move to Trash" uses the system Trash (Finder's Put Back works), and Organize
Folders shows every move and can undo it. Nothing is changed without the user pressing a
button that says what will happen.

Speech: if the user analyzes a video, its audio may be transcribed with Apple's on-device
speech recognizer (the system asks for Speech Recognition permission first).

## App Privacy questionnaire

"Data Not Collected". Reasons: no data leaves the device; model downloads send only a standard
HTTPS request for a public file. The privacy manifest (`Resources/PrivacyInfo.xcprivacy`)
declares no tracking and no collected data types, plus the required-reason APIs actually used
(UserDefaults CA92.1, file timestamps 3B52.1 and C617.1, disk space E174.1 and 85F4.1, system
uptime 35F9.1).

## Owner decisions still open

- **Export compliance.** `ITSAppUsesNonExemptEncryption` is not set. The app uses only Apple's
  HTTPS (URLSession) and CryptoKit hashing, which usually qualifies for the exemption, so
  `NO`/"None of the algorithms mentioned" is the likely answer — but it is the owner's legal
  declaration.
- **Age rating.** No user-generated content is shared, no web access beyond model downloads:
  likely 4+.
- **Category.** The bundle declares `public.app-category.productivity`.
- **Support and privacy URLs.** Needed in App Store Connect; `shared/docs/PRIVACY.md` is the
  source text for the privacy page.
- **Screenshots.** Take them from a library without personal paths or names. Settings and
  Locations now show home paths as `~/…`.
- **Minimum macOS.** 15.0 (`LSMinimumSystemVersion`).

## Still to verify before submitting

- Build, sign and validate a release package on a quiet machine:
  `bash platforms/apple/scripts/testflight.sh --package-only`, then launch the signed sandboxed
  bundle, choose a folder, scan, trash and restore a duplicate, and undo an Organize Folders move.
- In the sandboxed bundle, check that "scan my Downloads" in Chat opens the folder picker on
  Downloads, and that both "Open Trash" buttons open the user's Trash.
