# FileID Quick Look Preview Extension

When the user presses Space on a JPEG/PNG/HEIC in Finder, this extension takes over Quick Look and renders FileID's enriched preview: the photo + smart name + AI caption + detected people + tag chips.

## Status

**Source-complete; build target not wired into SwiftPM.**

SwiftPM cannot build app extensions (`.appex` bundles). Apple requires extensions to be built via an Xcode project with a separate target. The source in this folder is ready to drop into an Xcode target as soon as you've got an Apple Developer account and have generated `FileID.xcodeproj`.

## Files

- `QLPreviewProvider.swift` — the `QLPreviewingController` implementation. Reads FileID's read-only SQLite at `~/Library/Application Support/FileID/fileid.sqlite`, looks up the file by `path_text`, renders an HTML preview overlaying smart name + caption + people + tag chips on the photo.
- `Info.plist` — the extension's manifest. Declares the principal class, supported content types (image variants), and `QLIsDataBasedPreview = true` so we can return HTML.

## How to add the target in Xcode

1. Open `FileID.xcodeproj` in Xcode.
2. **File → New → Target → Quick Look Preview Extension** (under macOS).
3. Name it `FileIDQuickLookPreview`. Bundle identifier: `com.fileid.app.QuickLookPreview`.
4. Replace the auto-generated `PreviewProvider.swift` with the contents of `QLPreviewProvider.swift` here.
5. Replace the auto-generated `Info.plist` with the one in this folder.
6. Add **GRDB** as a Swift Package dependency on the new target (it's already a dep of the main app — just check the box for the new target in the package's "Frameworks and Libraries" tab).
7. Add the entitlement `com.apple.security.app-sandbox = YES` and `com.apple.security.files.user-selected.read-only = YES` to the extension target. The extension also needs to access `~/Library/Application Support/FileID/fileid.sqlite` — by default this is allowed for sandboxed apps reading their own group container. If you sandbox the main app with an app group, add `com.apple.security.application-groups = group.com.fileid` to **both** targets and move the DB to the group container.
8. Sign the extension with the same team as the main app. Notarize the host app + extension together.

## Behavior preview (what the user sees)

```
┌──────────────────────────────────┐
│  [photo at top]                  │
│                                  │
│  Mia at Beach.heic               │  ← gold (smart name)
│  was IMG_5512.HEIC               │  ← muted (original)
│                                  │
│  Mia laughing at the water,      │  ← caption
│  golden hour light.              │
│                                  │
│  👤 Mia, Adam                    │  ← lavender (people)
│                                  │
│  [Sunset] [Beach] [2024]         │  ← gold chips (tags)
└──────────────────────────────────┘
```

## Why HTML?

`QLPreviewReply(dataOfContentType: .html, …)` is the simplest path to a styled preview without bundling an entire SwiftUI scene into the extension. HTML keeps the extension binary tiny and lets the rendering be theme-flexible (the styling matches the FileID dark + gold aesthetic).

A future iteration could switch to `QLPreviewReply(contextSize:reply:)` returning a SwiftUI scene rendered to PDF or PNG for sharper macOS-native typography.
