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

/* --- Stage 6: faces --- */

/* Open a photo for face extraction (FaceClustering.extractOneFile): files
 * under 256 B are refused, the EXIF orientation must be 1-8, the image is
 * decoded upright with its long edge at most max_pixel_size (HEIF through the
 * hardware decoder). With landmarks true, Vision's face landmarks run once on
 * it. Returns an image handle (0 on failure) and *out_json (free with
 * fileid_apple_free): {"width","height","orientation","faces":[{"bounds",
 * "points"}]}; bounds are the normalized bottom-left box as four Double bit
 * patterns, points five top-left pixel points [hi-x eye, lo-x eye, nose, hi-x
 * mouth corner, lo-x mouth corner] as ten Float bit patterns. */
uint64_t fileid_apple_faces_open(const char *path, uint32_t max_pixel_size, bool landmarks,
                                 char **out_json);

/* The open image drawn into tightly packed RGBA8 (device RGB, top-left origin,
 * FaceAlign.renderRGBA8). Free *out_rgba with fileid_apple_free. */
bool fileid_apple_faces_pixels(uint64_t image, uint8_t **out_rgba, size_t *out_length,
                               uint32_t *out_width, uint32_t *out_height);

/* The integral pixel rect (top-left origin) of the open image drawn at 112x112
 * into out_rgba112 (112*112*4 bytes, the SFace input ArcFaceService draws),
 * and *out_jpeg the crop at JPEG quality 0.85 (may stay NULL). */
bool fileid_apple_faces_crop(uint64_t image, int64_t x, int64_t y, int64_t width, int64_t height,
                             uint8_t *out_rgba112, uint8_t **out_jpeg, size_t *out_jpeg_length);

/* JPEG (quality 0.85) of tightly packed RGBA8, as face_crops/<id>.jpg are
 * written. Free with fileid_apple_free; NULL on failure. */
uint8_t *fileid_apple_faces_jpeg(const uint8_t *rgba, uint32_t width, uint32_t height,
                                 size_t *out_length);

/* Release an image handle. Unknown handles are ignored. */
void fileid_apple_faces_close(uint64_t image);

/* Best Takes' photo as packed sRGB RGB8 (TakeMediaAnalysis.signals): kind 0
 * decodeBoundedImage fitted to long_side, 1 decodeScanImage drawn unscaled
 * (Face Mesh's detail image), 2 decodeBoundedImage drawn unscaled (tiles).
 * *out_source_long_side is the file's long side. Free *out_rgb. */
bool fileid_apple_take_decode(const char *path, uint32_t kind, uint32_t long_side,
                              uint8_t **out_rgb, size_t *out_length, uint32_t *out_width,
                              uint32_t *out_height, uint32_t *out_source_long_side);

/* VisionTakeFaceAnalyzer over packed sRGB RGB8: face landmarks and capture
 * quality, human rectangles when people is true, with Face Mesh inputs for this
 * image. JSON (free with fileid_apple_free): {"complete","faces":[{"box",
 * "minSide","quality","eyes","mesh":{"bits","boxOnly"}}],"people":[{"box"}]},
 * numbers as IEEE-754 bit patterns. When faces were found, *out_observations
 * keeps them for fileid_apple_take_mesh until fileid_apple_take_release. */
char *fileid_apple_take_faces(const uint8_t *rgb, uint32_t width, uint32_t height, bool people,
                              uint64_t *out_observations);

/* Face Mesh inputs of kept faces for a width x height decode of the same
 * photo, as a JSON array of {"bits","boxOnly"} (free with fileid_apple_free). */
char *fileid_apple_take_mesh(uint64_t observations, uint32_t width, uint32_t height);

/* Release kept faces. Unknown ids are ignored. */
void fileid_apple_take_release(uint64_t observations);

/* --- end Stage 6 --- */

#ifdef __cplusplus
}
#endif

#endif
