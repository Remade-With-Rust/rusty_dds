// One-shot DirectXTex encode+decode round-trip (demo + quality harvest).
//
// Usage (depth optional, default 1):
//   dxtex_roundtrip <in.rgba> <w> <h> <dxgi> <out.rgba> <out.json>
//   dxtex_roundtrip <in.rgba> <w> <h> <d> <dxgi> <out.rgba> <out.json>
//
// Reads tightly packed RGBA8 (depth slices contiguous), Compress|Convert,
// Decompress|Convert back to R8G8B8A8_UNORM, writes out.rgba + timings JSON.
// BC7 uses TEX_COMPRESS_BC7_QUICK (mode-6 class).

#include <DirectXTex.h>

#include <algorithm>
#include <chrono>
#include <cstdint>
#include <cstdio>
#include <cstring>
#include <fstream>
#include <stdexcept>
#include <string>
#include <vector>

using namespace DirectX;

static std::vector<uint8_t> ReadFileBytes(const char* path) {
    std::ifstream in(path, std::ios::binary);
    if (!in) {
        throw std::runtime_error(std::string("open failed: ") + path);
    }
    in.seekg(0, std::ios::end);
    const auto sz = static_cast<size_t>(in.tellg());
    in.seekg(0, std::ios::beg);
    std::vector<uint8_t> buf(sz);
    if (sz && !in.read(reinterpret_cast<char*>(buf.data()), static_cast<std::streamsize>(sz))) {
        throw std::runtime_error(std::string("read failed: ") + path);
    }
    return buf;
}

static DXGI_FORMAT ParseDxgi(const char* name) {
    const std::string s(name);
    if (s == "BC1_UNORM") return DXGI_FORMAT_BC1_UNORM;
    if (s == "BC2_UNORM") return DXGI_FORMAT_BC2_UNORM;
    if (s == "BC3_UNORM") return DXGI_FORMAT_BC3_UNORM;
    if (s == "BC4_UNORM") return DXGI_FORMAT_BC4_UNORM;
    if (s == "BC4_SNORM") return DXGI_FORMAT_BC4_SNORM;
    if (s == "BC5_UNORM") return DXGI_FORMAT_BC5_UNORM;
    if (s == "BC5_SNORM") return DXGI_FORMAT_BC5_SNORM;
    if (s == "BC7_UNORM") return DXGI_FORMAT_BC7_UNORM;
    if (s == "R8G8B8A8_UNORM") return DXGI_FORMAT_R8G8B8A8_UNORM;
    if (s == "B8G8R8A8_UNORM") return DXGI_FORMAT_B8G8R8A8_UNORM;
    throw std::runtime_error(std::string("unknown dxgi: ") + name);
}

static bool IsBlockCompressed(DXGI_FORMAT fmt) {
    switch (fmt) {
    case DXGI_FORMAT_BC1_UNORM:
    case DXGI_FORMAT_BC2_UNORM:
    case DXGI_FORMAT_BC3_UNORM:
    case DXGI_FORMAT_BC4_UNORM:
    case DXGI_FORMAT_BC4_SNORM:
    case DXGI_FORMAT_BC5_UNORM:
    case DXGI_FORMAT_BC5_SNORM:
    case DXGI_FORMAT_BC7_UNORM:
        return true;
    default:
        return false;
    }
}

static HRESULT SwizzleRgbaToBgra(const ScratchImage& src, ScratchImage& dst) {
    TexMetadata outMeta = src.GetMetadata();
    outMeta.format = DXGI_FORMAT_B8G8R8A8_UNORM;
    HRESULT hr = dst.Initialize(outMeta);
    if (FAILED(hr)) {
        return hr;
    }
    for (size_t idx = 0; idx < src.GetImageCount(); ++idx) {
        const Image* s = &src.GetImages()[idx];
        const Image* d = &dst.GetImages()[idx];
        for (size_t y = 0; y < s->height; ++y) {
            const uint8_t* sp = s->pixels + y * s->rowPitch;
            uint8_t* dp = d->pixels + y * d->rowPitch;
            for (size_t x = 0; x < s->width; ++x) {
                const uint8_t* p = sp + x * 4;
                uint8_t* q = dp + x * 4;
                q[0] = p[2];
                q[1] = p[1];
                q[2] = p[0];
                q[3] = p[3];
            }
        }
    }
    return S_OK;
}

static HRESULT BuildRgba(const uint8_t* rgba, size_t rgba_len, size_t w, size_t h, size_t d,
                         ScratchImage& out) {
    TexMetadata meta{};
    meta.width = w;
    meta.height = h;
    meta.depth = d;
    meta.arraySize = 1;
    meta.mipLevels = 1;
    meta.format = DXGI_FORMAT_R8G8B8A8_UNORM;
    meta.dimension = (d > 1) ? TEX_DIMENSION_TEXTURE3D : TEX_DIMENSION_TEXTURE2D;
    HRESULT hr = out.Initialize(meta);
    if (FAILED(hr)) {
        return hr;
    }
    const size_t slice = w * h * 4;
    if (rgba_len < slice * d) {
        return E_FAIL;
    }
    for (size_t z = 0; z < d; ++z) {
        const Image* img = out.GetImage(0, 0, z);
        const uint8_t* src = rgba + z * slice;
        for (size_t y = 0; y < h; ++y) {
            memcpy(img->pixels + y * img->rowPitch, src + y * w * 4, w * 4);
        }
    }
    return S_OK;
}

static HRESULT Encode(const ScratchImage& src, DXGI_FORMAT target, ScratchImage& out) {
    if (IsBlockCompressed(target)) {
        TEX_COMPRESS_FLAGS flags = TEX_COMPRESS_DEFAULT;
        if (target == DXGI_FORMAT_BC7_UNORM) {
            flags = TEX_COMPRESS_BC7_QUICK;
        }
        return Compress(src.GetImages(), src.GetImageCount(), src.GetMetadata(), target, flags,
                        TEX_THRESHOLD_DEFAULT, out);
    }
    if (target == src.GetMetadata().format) {
        HRESULT hr = out.Initialize(src.GetMetadata());
        if (FAILED(hr)) {
            return hr;
        }
        for (size_t i = 0; i < src.GetImageCount(); ++i) {
            const Image* s = &src.GetImages()[i];
            const Image* d = &out.GetImages()[i];
            for (size_t y = 0; y < s->height; ++y) {
                memcpy(d->pixels + y * d->rowPitch, s->pixels + y * s->rowPitch,
                       (std::min)(s->rowPitch, d->rowPitch));
            }
        }
        return S_OK;
    }
    if (target == DXGI_FORMAT_B8G8R8A8_UNORM) {
        HRESULT hr = Convert(src.GetImages(), src.GetImageCount(), src.GetMetadata(), target,
                             TEX_FILTER_DEFAULT, TEX_THRESHOLD_DEFAULT, out);
        if (FAILED(hr)) {
            hr = SwizzleRgbaToBgra(src, out);
        }
        return hr;
    }
    return Convert(src.GetImages(), src.GetImageCount(), src.GetMetadata(), target,
                   TEX_FILTER_DEFAULT, TEX_THRESHOLD_DEFAULT, out);
}

static HRESULT DecodeToRgba(const ScratchImage& src, ScratchImage& out) {
    const auto fmt = src.GetMetadata().format;
    if (IsBlockCompressed(fmt)) {
        return Decompress(src.GetImages(), src.GetImageCount(), src.GetMetadata(),
                          DXGI_FORMAT_R8G8B8A8_UNORM, out);
    }
    if (fmt == DXGI_FORMAT_R8G8B8A8_UNORM) {
        HRESULT hr = out.Initialize(src.GetMetadata());
        if (FAILED(hr)) {
            return hr;
        }
        for (size_t i = 0; i < src.GetImageCount(); ++i) {
            const Image* s = &src.GetImages()[i];
            const Image* d = &out.GetImages()[i];
            for (size_t y = 0; y < s->height; ++y) {
                memcpy(d->pixels + y * d->rowPitch, s->pixels + y * s->rowPitch,
                       (std::min)(s->rowPitch, d->rowPitch));
            }
        }
        return S_OK;
    }
    // BGRA → RGBA
    HRESULT hr = Convert(src.GetImages(), src.GetImageCount(), src.GetMetadata(),
                         DXGI_FORMAT_R8G8B8A8_UNORM, TEX_FILTER_DEFAULT, TEX_THRESHOLD_DEFAULT,
                         out);
    if (SUCCEEDED(hr)) {
        return hr;
    }
    // Software BGRA→RGBA swizzle
    TexMetadata outMeta = src.GetMetadata();
    outMeta.format = DXGI_FORMAT_R8G8B8A8_UNORM;
    hr = out.Initialize(outMeta);
    if (FAILED(hr)) {
        return hr;
    }
    for (size_t idx = 0; idx < src.GetImageCount(); ++idx) {
        const Image* s = &src.GetImages()[idx];
        const Image* d = &out.GetImages()[idx];
        for (size_t y = 0; y < s->height; ++y) {
            const uint8_t* sp = s->pixels + y * s->rowPitch;
            uint8_t* dp = d->pixels + y * d->rowPitch;
            for (size_t x = 0; x < s->width; ++x) {
                const uint8_t* p = sp + x * 4;
                uint8_t* q = dp + x * 4;
                q[0] = p[2];
                q[1] = p[1];
                q[2] = p[0];
                q[3] = p[3];
            }
        }
    }
    return S_OK;
}

static void WriteTightRgba(const ScratchImage& rgba, const char* path) {
    const auto& meta = rgba.GetMetadata();
    std::ofstream out(path, std::ios::binary);
    for (size_t z = 0; z < meta.depth; ++z) {
        const Image* img = rgba.GetImage(0, 0, z);
        for (size_t y = 0; y < img->height; ++y) {
            out.write(reinterpret_cast<const char*>(img->pixels + y * img->rowPitch),
                      static_cast<std::streamsize>(img->width * 4));
        }
    }
}

int main(int argc, char** argv) {
    // Legacy: in w h dxgi out json  (6 trailing after exe = argc 7)
    // New:     in w h d dxgi out json (argc 8)
    if (argc != 7 && argc != 8) {
        std::fprintf(stderr,
                     "Usage: dxtex_roundtrip <in.rgba> <w> <h> [d] <dxgi> <out.rgba> <out.json>\n");
        return 2;
    }
    try {
        const char* inPath = argv[1];
        const size_t w = static_cast<size_t>(std::atoi(argv[2]));
        const size_t h = static_cast<size_t>(std::atoi(argv[3]));
        size_t d = 1;
        const char* dxgiName = nullptr;
        const char* outRgba = nullptr;
        const char* outJson = nullptr;
        if (argc == 7) {
            dxgiName = argv[4];
            outRgba = argv[5];
            outJson = argv[6];
        } else {
            d = static_cast<size_t>(std::atoi(argv[4]));
            dxgiName = argv[5];
            outRgba = argv[6];
            outJson = argv[7];
        }
        if (d < 1) {
            d = 1;
        }
        const DXGI_FORMAT target = ParseDxgi(dxgiName);

        auto bytes = ReadFileBytes(inPath);
        ScratchImage src;
        HRESULT hr = BuildRgba(bytes.data(), bytes.size(), w, h, d, src);
        if (FAILED(hr)) {
            throw std::runtime_error("BuildRgba failed");
        }

        using clock = std::chrono::steady_clock;

        ScratchImage encoded;
        const auto t0 = clock::now();
        hr = Encode(src, target, encoded);
        const auto t1 = clock::now();
        if (FAILED(hr)) {
            std::fprintf(stderr, "encode hr=0x%08lx\n", static_cast<unsigned long>(hr));
            return 1;
        }

        ScratchImage decoded;
        const auto t2 = clock::now();
        hr = DecodeToRgba(encoded, decoded);
        const auto t3 = clock::now();
        if (FAILED(hr)) {
            std::fprintf(stderr, "decode hr=0x%08lx\n", static_cast<unsigned long>(hr));
            return 1;
        }

        const double encode_ns = std::chrono::duration<double, std::nano>(t1 - t0).count();
        const double decode_ns = std::chrono::duration<double, std::nano>(t3 - t2).count();

        WriteTightRgba(decoded, outRgba);

        std::ofstream js(outJson, std::ios::binary);
        js << "{\n"
           << "  \"ok\": true,\n"
           << "  \"peer\": \"Microsoft DirectXTex\",\n"
           << "  \"encode_ns\": " << encode_ns << ",\n"
           << "  \"decode_ns\": " << decode_ns << ",\n"
           << "  \"roundtrip_ns\": " << (encode_ns + decode_ns) << ",\n"
           << "  \"encoded_bytes\": " << encoded.GetPixelsSize() << ",\n"
           << "  \"width\": " << w << ",\n"
           << "  \"height\": " << h << ",\n"
           << "  \"depth\": " << d << "\n"
           << "}\n";
        return 0;
    } catch (const std::exception& e) {
        std::fprintf(stderr, "error: %s\n", e.what());
        return 1;
    }
}
