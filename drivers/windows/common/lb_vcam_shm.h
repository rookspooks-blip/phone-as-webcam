/*
 * Lensbridge virtual camera shared memory layout (version 1).
 *
 * Writer: the Lensbridge desktop app (desktop/apps/lensbridge/src/vcam/windows.rs).
 * Readers: the DirectShow filter loaded into Zoom/Discord/OBS/... processes.
 *
 * Layout (all little-endian, offsets in bytes):
 *   0                      LbVcamHeader (64 bytes)
 *   64                     slot 0: LbVcamSlot (64 bytes) + I420 data (max_width*max_height*3/2)
 *   64 + slot_size         slot 1: same
 *
 * Synchronization: per-slot seqlock. The writer fills the slot that is NOT
 * `latest_slot`: seq becomes odd, data is written, seq becomes even, then
 * `latest_slot` and `heartbeat_ms` are updated. A reader copies the latest
 * slot and retries if seq was odd or changed during the copy.
 *
 * Keep in sync with the Rust definition (same field order and sizes).
 */
#ifndef LB_VCAM_SHM_H
#define LB_VCAM_SHM_H

#include <stdint.h>

#define LB_VCAM_SHM_NAME L"Local\\LensbridgeVirtualCamera1"
#define LB_VCAM_MAGIC 0x4356424Cu /* "LBVC" */
#define LB_VCAM_VERSION 1u
#define LB_VCAM_MAX_WIDTH 1920u
#define LB_VCAM_MAX_HEIGHT 1080u
#define LB_VCAM_FORMAT_I420 1u
#define LB_VCAM_HEADER_SIZE 64u
#define LB_VCAM_SLOT_HEADER_SIZE 64u
#define LB_VCAM_SLOT_SIZE (LB_VCAM_SLOT_HEADER_SIZE + LB_VCAM_MAX_WIDTH * LB_VCAM_MAX_HEIGHT * 3u / 2u)
#define LB_VCAM_TOTAL_SIZE (LB_VCAM_HEADER_SIZE + 2u * LB_VCAM_SLOT_SIZE)
/* Frames older than this are treated as "no signal". */
#define LB_VCAM_STALE_MS 1500u

#pragma pack(push, 8)
typedef struct LbVcamHeader {
    uint32_t magic;
    uint32_t version;
    uint32_t max_width;
    uint32_t max_height;
    uint32_t slot_size;
    uint32_t format;
    volatile uint32_t latest_slot; /* 0 or 1 */
    uint32_t reserved0;
    volatile uint64_t heartbeat_ms; /* writer's GetTickCount64() at last publish */
    uint64_t reserved[3];
} LbVcamHeader;

typedef struct LbVcamSlot {
    volatile uint64_t seq; /* even = stable, odd = being written */
    uint32_t width;
    uint32_t height;
    uint64_t frame_id;
    uint64_t pts_us;
    uint64_t reserved[4];
} LbVcamSlot;
#pragma pack(pop)

#ifdef __cplusplus
static_assert(sizeof(LbVcamHeader) == LB_VCAM_HEADER_SIZE, "header size");
static_assert(sizeof(LbVcamSlot) == LB_VCAM_SLOT_HEADER_SIZE, "slot header size");
#endif

#endif /* LB_VCAM_SHM_H */
