// C ABI over Microsoft DirectXTex, shaped to the simulator's TextureProvider seam.
//
// Two peer paths, because "what DirectXTex costs at runtime" has two honest
// answers and a benchmark that picks only one is picking a side:
//
//   DXT_PEER_LOADER  — GetMetadataFromDDSMemory + ComputePitch, pointing into
//                      the caller's buffer. This mirrors DDSTextureLoader, which
//                      is what shipping engines actually use to feed the GPU.
//   DXT_PEER_SCRATCH — LoadFromDDSMemory into a ScratchImage. DirectXTex's own
//                      container API; it copies.
//
// The board names which path each row used.

#ifndef DXTEX_PROVIDER_H
#define DXTEX_PROVIDER_H

#include <stdint.h>
#include <stddef.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef struct DxtTexture DxtTexture;

#define DXT_PEER_LOADER 0
#define DXT_PEER_SCRATCH 1

#define DXT_OK 0
#define DXT_ERR_PARSE 1
#define DXT_ERR_RANGE 2
#define DXT_ERR_UNSUPPORTED 3
#define DXT_ERR_ALLOC 4

typedef struct {
    uint32_t width;
    uint32_t height;
    uint32_t depth;
    uint32_t mips;
    uint32_t layers;
    uint32_t dxgi_format;
    uint32_t block_bytes;
    uint32_t compressed;
} DxtDesc;

typedef struct {
    const uint8_t* data;
    size_t len;
    uint32_t width;
    uint32_t height;
    uint32_t row_pitch;
    uint32_t rows;
} DxtSub;

// `bytes` must outlive the returned handle: the loader path points into it
// rather than copying, which is the whole point of that path.
int dxt_open(const uint8_t* bytes, size_t len, int peer, DxtTexture** out);
int dxt_desc(const DxtTexture* tex, DxtDesc* out);
int dxt_subresource(const DxtTexture* tex, uint32_t mip, uint32_t layer, uint32_t face, DxtSub* out);
// Decompress/Convert to tightly packed RGBA8. Caller frees with dxt_free.
int dxt_decode_rgba8(const DxtTexture* tex, uint32_t mip, uint32_t layer, uint32_t face,
                     uint8_t** out, size_t* out_len);
void dxt_free(uint8_t* p);
void dxt_close(DxtTexture* tex);
// Bytes the handle itself holds resident (0 for the loader path, which owns none).
uint64_t dxt_resident_bytes(const DxtTexture* tex);

#ifdef __cplusplus
}
#endif

#endif
