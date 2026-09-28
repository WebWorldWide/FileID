# Post Store feature roadmap

These are the owner's requested features for FileID after the current Windows Store release. They are shared requirements for macOS, Windows, and Linux work. Keep the first Store submission focused on the current release; do not make these features release gates for version 1.0.0.0.

## Best takes and natural language media search

Let a person ask for moments in their own photos, audio, and video, such as “show me videos only where he gets a hit” in baseball footage. Return matching files and, for time based media, the relevant time ranges so the person can review the evidence and sort results out of a larger collection.

Acceptance criteria:

- Search and analysis run on the device. Queries, media, derived captions, and embeddings do not leave it.
- Results distinguish a whole-file match from a moment inside a video or audio file. Show a preview, timestamp where applicable, and enough context for the person to confirm or reject a match.
- Indexing can be paused, resumed, and cancelled without corrupting the library. It stays bounded on large collections and degrades to CPU when acceleration is unavailable.
- False matches and uncertain results remain reviewable. No automatic move or delete follows from a model judgment.
- Define the shared IPC and test corpus first, including baseball action examples, negative examples, audio-only examples, and photos. Record the selected model and its commercial-use terms in `MODELS.md` before implementation.

## Batch conversion, compression, and upscaling

Support converting and optimizing supported video, audio, image, and document formats through a batch workflow. “All files” is the product goal, while each release must publish an exact input/output format matrix and say when a format is unsupported. Include conversion, compression, and upscaling where the format and available local tools permit them.

Acceptance criteria:

- Show output format, quality/size preset, destination, estimated size where reliable, and a sample preview before a batch starts.
- Preserve originals by default. Use a separate destination or explicit overwrite choice, collision handling, temporary output plus atomic completion, cancellation, and per-file results. A failed batch never silently removes an original.
- Preserve metadata when the format supports it, and clearly disclose metadata that cannot survive a conversion.
- Keep processing local. Vet codecs, binaries, model weights, and redistribution terms before adding dependencies or download sources. Downloads require an explicit user action and reviewed pinned sources.
- Add shared corpus cases for quality, orientation, audio/video sync, variable frame rate, large files, unsupported formats, cancellation, and disk-full recovery.

## Better folder organization

Improve the existing Restructure workflow described in `RESTRUCTURE.md`: make its proposals fit the owner's current folder conventions, surface clear reasons and confidence, and let the owner quickly correct a large plan before anything moves.

Acceptance criteria:

- Preview the before/after tree and every proposed move, including conflicts and low-confidence items. Let the person keep a file in place, edit a destination, or apply only part of a plan.
- Never move outside approved roots, overwrite an unrelated file, follow an unsafe link, or erase originals as a side effect. Journal each confirmed move and verify undo across restarts.
- Measure proposal quality and speed on the shared corpus and an on-hardware large library. Include mixed media, existing naming conventions, duplicate names, empty folders, and interrupted apply/undo.
- Keep macOS as the visual and behavioral reference while each platform uses native UI. Add shared IPC fields before platform DTOs change.

## Coordination

Platform agents should use this file and `RESTRUCTURE.md` as the common brief. Land shared IPC and corpus changes together with the first platform implementation, then mirror the contract on the other platforms. Keep post release feature work separate from Store certification fixes so the current release can be validated independently.
