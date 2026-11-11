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

/* Release adapter-owned state (sleep assertions, bookmark leases) and cancel
 * VLM requests before the engine's hard exit. Idempotent. */
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

/* --- Stage 7: MLX vision-language models --- */

/* Events a VLM request reports through its callback. */
#define FILEID_APPLE_VLM_TOKEN 1    /* payload: UTF-8 text chunk */
#define FILEID_APPLE_VLM_PROGRESS 2 /* payload: JSON object */
#define FILEID_APPLE_VLM_DONE 3     /* payload: JSON reply; terminal */
#define FILEID_APPLE_VLM_FAILED 4   /* payload: {"message","cancelled"}; terminal */

/* Called from any thread. The payload is valid only during the call. Exactly
 * one terminal event (DONE or FAILED) ends every accepted request; the
 * callback is never called for it again. */
typedef void (*fileid_apple_vlm_callback)(void *context, uint32_t event,
                                          const uint8_t *payload, size_t length);

/* Start a VLM request: a UTF-8 JSON object with an "op" (status, load, unload,
 * prefetch, analyze, generate, chat, transcribe, sequence, benchmark) and its
 * fields. Models load only from the local folder the request names; nothing
 * is downloaded. Returns a handle, 0 when the request was refused (then the
 * callback is never called). */
uint64_t fileid_apple_vlm_submit(const uint8_t *request, size_t length,
                                 fileid_apple_vlm_callback callback, void *context);

/* Cancel a running request; it still reports its terminal event. Unknown
 * handles are ignored. */
void fileid_apple_vlm_cancel(uint64_t handle);

/* --- end Stage 7 --- */

#ifdef __cplusplus
}
#endif

#endif
