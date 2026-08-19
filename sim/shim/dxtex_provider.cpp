// C ABI over Microsoft DirectXTex. See dxtex_provider.h for the two peer paths.
//
// Everything here uses DirectXTex's own API for the work that matters —
// metadata parse, pitch computation, decompression. The only hand-written part
// is locating the start of the pixel data, which DDSTextureLoader also does by
// hand, and which cannot be got from DirectXTex's public surface.

#include "dxtex_provider.h"

#include <DirectXTex.h>

#include <cstring>
#include <new>
#include <vector>

using namespace DirectX;

namespace {

constexpr uint32_t kDdsMagic = 0x20534444; // "DDS "
constexpr size_t kDdsHeaderSize = 124;
constexpr size_t kDdsDx10HeaderSize = 20;
constexpr uint32_t kFourCcDx10 = 0x30315844; // "DX10"

// Start of the surface data: magic + DDS_HEADER, plus the DX10 extension when
// the pixel-format FourCC says so. Mirrors DDSTextureLoader.
bool pixel_data_offset(const uint8_t* bytes, size_t len, size_t* out) {
    if (len < 4 + kDdsHeaderSize) {
        return false;
    }
    uint32_t magic = 0;
    std::memcpy(&magic, bytes, 4);
    if (magic != kDdsMagic) {
        return false;
    }
    // DDS_PIXELFORMAT sits at offset 72 within DDS_HEADER; its dwFourCC is +8.
    uint32_t four_cc = 0;
    std::memcpy(&four_cc, bytes + 4 + 72 + 8, 4);
    size_t off = 4 + kDdsHeaderSize;
    if (four_cc == kFourCcDx10) {
        off += kDdsDx10HeaderSize;
    }
    if (off > len) {
        return false;
    }
    *out = off;
    return true;
}

uint32_t block_bytes_for(DXGI_FORMAT fmt) {
    if (IsCompressed(fmt)) {
        switch (fmt) {
            case DXGI_FORMAT_BC1_TYPELESS:
            case DXGI_FORMAT_BC1_UNORM:
            case DXGI_FORMAT_BC1_UNORM_SRGB:
            case DXGI_FORMAT_BC4_TYPELESS:
            case DXGI_FORMAT_BC4_UNORM:
            case DXGI_FORMAT_BC4_SNORM:
                return 8;
            default:
                return 16;
        }
    }
    return static_cast<uint32_t>(BitsPerPixel(fmt) / 8);
}

} // namespace

struct DxtTexture {
    const uint8_t* base = nullptr; // borrowed; caller guarantees lifetime
    size_t len = 0;
    size_t data_offset = 0;
    TexMetadata meta{};
    ScratchImage scratch; // peer == DXT_PEER_SCRATCH only
    int peer = DXT_PEER_LOADER;
};

extern "C" {

int dxt_open(const uint8_t* bytes, size_t len, int peer, DxtTexture** out) {
    if (!bytes || !out) {
        return DXT_ERR_RANGE;
    }
    auto* tex = new (std::nothrow) DxtTexture();
    if (!tex) {
        return DXT_ERR_ALLOC;
    }
    tex->base = bytes;
    tex->len = len;
    tex->peer = peer;

    HRESULT hr;
    if (peer == DXT_PEER_SCRATCH) {
        hr = LoadFromDDSMemory(bytes, len, DDS_FLAGS_NONE, &tex->meta, tex->scratch);
    } else {
        hr = GetMetadataFromDDSMemory(bytes, len, DDS_FLAGS_NONE, tex->meta);
        if (SUCCEEDED(hr) && !pixel_data_offset(bytes, len, &tex->data_offset)) {
            hr = E_FAIL;
        }
    }
    if (FAILED(hr)) {
        delete tex;
        return DXT_ERR_PARSE;
    }
    *out = tex;
    return DXT_OK;
}

int dxt_desc(const DxtTexture* tex, DxtDesc* out) {
    if (!tex || !out) {
        return DXT_ERR_RANGE;
    }
    out->width = static_cast<uint32_t>(tex->meta.width);
    out->height = static_cast<uint32_t>(tex->meta.height);
    out->depth = static_cast<uint32_t>(tex->meta.depth);
    out->mips = static_cast<uint32_t>(tex->meta.mipLevels);
    out->layers = static_cast<uint32_t>(tex->meta.arraySize);
    out->dxgi_format = static_cast<uint32_t>(tex->meta.format);
    out->block_bytes = block_bytes_for(tex->meta.format);
    out->compressed = IsCompressed(tex->meta.format) ? 1u : 0u;
    return DXT_OK;
}

int dxt_subresource(const DxtTexture* tex, uint32_t mip, uint32_t layer, uint32_t face,
                    DxtSub* out) {
    if (!tex || !out) {
        return DXT_ERR_RANGE;
    }
    const size_t item = static_cast<size_t>(layer) + static_cast<size_t>(face);
    if (mip >= tex->meta.mipLevels || item >= tex->meta.arraySize) {
        return DXT_ERR_RANGE;
    }

    if (tex->peer == DXT_PEER_SCRATCH) {
        const Image* img = tex->scratch.GetImage(mip, item, 0);
        if (!img) {
            return DXT_ERR_RANGE;
        }
        const size_t rows = img->rowPitch ? img->slicePitch / img->rowPitch : 0;
        out->data = img->pixels;
        out->len = img->slicePitch;
        out->width = static_cast<uint32_t>(img->width);
        out->height = static_cast<uint32_t>(img->height);
        out->row_pitch = static_cast<uint32_t>(img->rowPitch);
        out->rows = static_cast<uint32_t>(rows);
        return DXT_OK;
    }

    // Loader path: walk the mip chain the way DDSTextureLoader's FillInitData
    // does, using DirectXTex's ComputePitch for every level.
    size_t offset = tex->data_offset;
    for (size_t it = 0; it < tex->meta.arraySize; ++it) {
        size_t w = tex->meta.width;
        size_t h = tex->meta.height;
        size_t d = tex->meta.depth;
        for (size_t m = 0; m < tex->meta.mipLevels; ++m) {
            size_t row_pitch = 0;
            size_t slice_pitch = 0;
            HRESULT hr = ComputePitch(tex->meta.format, w, h, row_pitch, slice_pitch,
                                      CP_FLAGS_NONE);
            if (FAILED(hr)) {
                return DXT_ERR_UNSUPPORTED;
            }
            const size_t total = slice_pitch * d;
            if (it == item && m == mip) {
                if (offset + total > tex->len) {
                    return DXT_ERR_RANGE;
                }
                out->data = tex->base + offset;
                out->len = total;
                out->width = static_cast<uint32_t>(w);
                out->height = static_cast<uint32_t>(h);
                out->row_pitch = static_cast<uint32_t>(row_pitch);
                out->rows = static_cast<uint32_t>(row_pitch ? total / row_pitch : 0);
                return DXT_OK;
            }
            offset += total;
            w = w > 1 ? w >> 1 : 1;
            h = h > 1 ? h >> 1 : 1;
            d = d > 1 ? d >> 1 : 1;
        }
    }
    return DXT_ERR_RANGE;
}

int dxt_decode_rgba8(const DxtTexture* tex, uint32_t mip, uint32_t layer, uint32_t face,
                     uint8_t** out, size_t* out_len) {
    if (!tex || !out || !out_len) {
        return DXT_ERR_RANGE;
    }
    DxtSub sub{};
    int rc = dxt_subresource(tex, mip, layer, face, &sub);
    if (rc != DXT_OK) {
        return rc;
    }

    Image src{};
    src.width = sub.width;
    src.height = sub.height;
    src.format = tex->meta.format;
    src.rowPitch = sub.row_pitch;
    src.slicePitch = sub.len;
    src.pixels = const_cast<uint8_t*>(sub.data);

    ScratchImage dst;
    HRESULT hr;
    if (IsCompressed(tex->meta.format)) {
        hr = Decompress(src, DXGI_FORMAT_R8G8B8A8_UNORM, dst);
    } else if (tex->meta.format == DXGI_FORMAT_R8G8B8A8_UNORM) {
        hr = dst.InitializeFromImage(src);
    } else {
        hr = Convert(src, DXGI_FORMAT_R8G8B8A8_UNORM, TEX_FILTER_DEFAULT,
                     TEX_THRESHOLD_DEFAULT, dst);
    }
    if (FAILED(hr)) {
        return DXT_ERR_UNSUPPORTED;
    }

    const Image* img = dst.GetImage(0, 0, 0);
    if (!img) {
        return DXT_ERR_UNSUPPORTED;
    }
    const size_t tight = static_cast<size_t>(img->width) * img->height * 4;
    auto* buf = static_cast<uint8_t*>(::operator new(tight, std::nothrow));
    if (!buf) {
        return DXT_ERR_ALLOC;
    }
    // Tighten: the seam's contract is packed RGBA8, whatever the row pitch was.
    for (size_t y = 0; y < img->height; ++y) {
        std::memcpy(buf + y * img->width * 4, img->pixels + y * img->rowPitch,
                    img->width * 4);
    }
    *out = buf;
    *out_len = tight;
    return DXT_OK;
}

int dxt_decode_rgba_f32(const DxtTexture* tex, uint32_t mip, uint32_t layer, uint32_t face,
                        float** out, size_t* out_floats) {
    if (!tex || !out || !out_floats) {
        return DXT_ERR_RANGE;
    }
    DxtSub sub{};
    int rc = dxt_subresource(tex, mip, layer, face, &sub);
    if (rc != DXT_OK) {
        return rc;
    }

    Image src{};
    src.width = sub.width;
    src.height = sub.height;
    src.format = tex->meta.format;
    src.rowPitch = sub.row_pitch;
    src.slicePitch = sub.len;
    src.pixels = const_cast<uint8_t*>(sub.data);

    ScratchImage dst;
    HRESULT hr;
    if (IsCompressed(tex->meta.format)) {
        hr = Decompress(src, DXGI_FORMAT_R32G32B32A32_FLOAT, dst);
    } else if (tex->meta.format == DXGI_FORMAT_R32G32B32A32_FLOAT) {
        hr = dst.InitializeFromImage(src);
    } else {
        hr = Convert(src, DXGI_FORMAT_R32G32B32A32_FLOAT, TEX_FILTER_DEFAULT,
                     TEX_THRESHOLD_DEFAULT, dst);
    }
    if (FAILED(hr)) {
        return DXT_ERR_UNSUPPORTED;
    }

    const Image* img = dst.GetImage(0, 0, 0);
    if (!img) {
        return DXT_ERR_UNSUPPORTED;
    }
    const size_t row_floats = static_cast<size_t>(img->width) * 4;
    const size_t tight = row_floats * img->height;
    auto* buf = static_cast<float*>(::operator new(tight * sizeof(float), std::nothrow));
    if (!buf) {
        return DXT_ERR_ALLOC;
    }
    for (size_t y = 0; y < img->height; ++y) {
        std::memcpy(buf + y * row_floats, img->pixels + y * img->rowPitch,
                    row_floats * sizeof(float));
    }
    *out = buf;
    *out_floats = tight;
    return DXT_OK;
}

void dxt_free(uint8_t* p) {
    ::operator delete(p);
}

void dxt_close(DxtTexture* tex) {
    delete tex;
}

uint64_t dxt_resident_bytes(const DxtTexture* tex) {
    if (!tex) {
        return 0;
    }
    if (tex->peer == DXT_PEER_SCRATCH) {
        return static_cast<uint64_t>(tex->scratch.GetPixelsSize());
    }
    return 0;
}

} // extern "C"
