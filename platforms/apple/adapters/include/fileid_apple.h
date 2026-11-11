/*
 * FileIDAppleKit: the C interface between the shared Rust engine (fileidd)
 * and the Swift adapters in FileID.app/Contents/Frameworks.
 *
 * Hand-written. The Rust mirror is platforms/windows/src/engine/src/apple/ffi.rs
 * (a reviewed surface in shared/scripts/check_runtime_egress.py); change both
 * together and bump FILEID_APPLE_ABI_VERSION. No adapter opens a URL, socket
 * or HTTP client or launches a process.
 *
 * Conventions: plain C types; strings are NUL-terminated UTF-8; memory the
 * adapter returns is released with fileid_apple_free; handles are nonzero and
 * 0 means failure. Adapters own Apple-framework mechanics only: no catalog
 * writes, job policy or ranking.
 */
#ifndef FILEID_APPLE_H
#define FILEID_APPLE_H

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

#define FILEID_APPLE_ABI_VERSION 1

#ifdef __cplusplus
extern "C" {
#endif

/* Interface version the library implements. */
uint32_t fileid_apple_abi_version(void);

/* Release adapter-owned state (sleep assertions, bookmark leases) before the
 * engine's hard exit. Idempotent. */
void fileid_apple_shutdown(void);

/* Free memory the adapter allocated for the caller. NULL is ignored. */
void fileid_apple_free(void *pointer);

/* Take an IOPM PreventSystemSleep assertion (the display may still sleep).
 * Returns its id, 0 on failure. */
uint32_t fileid_apple_sleep_begin(const char *reason);

/* Release an assertion from fileid_apple_sleep_begin. Unknown ids are ignored. */
void fileid_apple_sleep_end(uint32_t assertion);

/* Resolve a bookmark (security-scoped when it carries a scope) and start
 * accessing it. Returns a lease id, 0 on failure. On success *out_path holds
 * the resolved path (free with fileid_apple_free) and *out_stale whether the
 * app should recreate the bookmark. */
uint64_t fileid_apple_bookmark_open(const uint8_t *data, size_t length,
                                    char **out_path, bool *out_stale);

/* Stop accessing a leased bookmark. Unknown ids are ignored. */
void fileid_apple_bookmark_close(uint64_t lease);

/* Move the item at path to the Trash (FileManager.trashItem). Returns 0 on
 * success, with *out_landed its path in the Trash when Foundation reports it
 * (else NULL); otherwise nonzero with *out_error a message. Free both with
 * fileid_apple_free. The caller checks the item's identity before and after. */
int32_t fileid_apple_trash_item(const char *path, char **out_landed, char **out_error);

/* The item's Finder tags as a JSON array of strings (free with
 * fileid_apple_free), or NULL with *out_error set. */
char *fileid_apple_tags_read(const char *path, char **out_error);

/* Replace the item's Finder tags with tags[0..count] (count 0 clears them).
 * Returns 0 on success, otherwise nonzero with *out_error set. */
int32_t fileid_apple_tags_write(const char *path, const char *const *tags, size_t count,
                                char **out_error);

/* --- Stage 8: timeline, speech and video tools --- */
/* `cancel` arguments point at a caller-owned flag (a Rust AtomicBool) and may be
 * NULL; set it to stop long work. Replies are JSON objects freed with
 * fileid_apple_free; a failure is {"error": text}. */

/* Inspect a media file with AVFoundation. `strict` applies the media tools'
 * refusals (unplayable or protected input, duration and dimension limits);
 * otherwise only what timeline analysis reads. `mime_type` (may be NULL) names
 * the type of a staged file without a media extension. Reply: duration,
 * videoCount, audioCount, otherCount, width, height (displayed), rotation
 * (clockwise degrees), videoCodec, audioCodec (ffprobe names), videoStart,
 * audioStart, audioDuration, hdr, alpha, frameRate. */
char *fileid_apple_media_probe(const char *path, const char *mime_type, bool strict);

/* One export from {"recipe": ToolRecipe, "source", "output"}.
 * trim/remux/audio/proxy/rotate run AVAssetExportSession and reply {"probe":
 * source probe}; "video" fits or pads to a frame, stabilizes or follows people
 * and replies {"probe": output probe, "summary"}, or, without "output", only
 * plans and replies the source probe and summary. The output must not exist. */
char *fileid_apple_media_export(const char *request_json, const bool *cancel);

/* One upright RGB frame (width*height*3 bytes, freed with fileid_apple_free)
 * at `seconds`, drawn at exactly width x height; NULL on failure.
 * *out_seconds receives the decoded frame's time. */
uint8_t *fileid_apple_video_frame(const char *path, double seconds, uint32_t width,
                                  uint32_t height, double *out_seconds);

/* Frame pool: one sequential decode writing a JPEG no larger than max_side for
 * the first frame at or after every `interval`, plus manifest.json ({duration,
 * size, modifiedAt, frames: [{seconds, file, signature (32x18 gray)}]}), into
 * the existing empty `directory`. Returns 0, 2 (invalid request), 3 (failed) or
 * 4 (the source changed while decoding). */
int32_t fileid_apple_video_pool(const char *path, double interval, uint32_t max_side,
                                const char *directory, const bool *cancel);

/* On-device speech recognition: 1 ready, 0 unavailable or denied, -1 the host
 * bundle has no speech usage description (never asks). With `ask`, a user who
 * has not decided sees the system permission prompt. */
int32_t fileid_apple_speech_status(bool ask);

/* "apple-speech-ondevice-v1/<locale>/macOS-<version>", freed with fileid_apple_free. */
char *fileid_apple_speech_model_version(void);

/* Transcribe [start, end) seconds of the first audio track on device. Reply
 * {"segments": [{text, timestamp (relative to start), duration, confidence}]}. */
char *fileid_apple_speech_transcribe(const char *path, double start, double end,
                                     const bool *cancel);
/* --- end Stage 8 --- */

#ifdef __cplusplus
}
#endif

#endif
