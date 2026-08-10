// Times Microsoft DirectXTex LoadFromDDSMemory + Decompress/Convert-to-RGBA8
// on a directory of .dds cases. Emits JSON for the rusty_dds side-by-side artifact.
//
// Usage:
//   dxtex_decode_bench.exe <cases_dir> <out.json> [iters]

#include <DirectXTex.h>

#include <algorithm>
#include <chrono>
#include <cstdint>
#include <cstdio>
#include <filesystem>
#include <fstream>
#include <sstream>
#include <stdexcept>
#include <string>
#include <vector>

namespace fs = std::filesystem;
using namespace DirectX;

static std::vector<uint8_t> ReadFileBytes(const fs::path& path) {
    std::ifstream in(path, std::ios::binary);
    if (!in) {
        throw std::runtime_error("failed to open " + path.string());
    }
    in.seekg(0, std::ios::end);
    const auto sz = static_cast<size_t>(in.tellg());
    in.seekg(0, std::ios::beg);
    std::vector<uint8_t> buf(sz);
    if (sz && !in.read(reinterpret_cast<char*>(buf.data()), static_cast<std::streamsize>(sz))) {
        throw std::runtime_error("failed to read " + path.string());
    }
    return buf;
}

static bool IsBlockCompressed(DXGI_FORMAT fmt);

static HRESULT SwizzleBgraToRgba(const ScratchImage& src, ScratchImage& dst) {
    const TexMetadata& meta = src.GetMetadata();
    TexMetadata outMeta = meta;
    outMeta.format = DXGI_FORMAT_R8G8B8A8_UNORM;
    HRESULT hr = dst.Initialize(outMeta);
    if (FAILED(hr)) {
        return hr;
    }
    for (size_t idx = 0; idx < src.GetImageCount(); ++idx) {
        const Image* s = &src.GetImages()[idx];
        const Image* d = &dst.GetImages()[idx];
        if (!s->pixels || !d->pixels) {
            return E_FAIL;
        }
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

static bool IsBlockCompressed(DXGI_FORMAT fmt) {
    switch (fmt) {
    case DXGI_FORMAT_BC1_TYPELESS:
    case DXGI_FORMAT_BC1_UNORM:
    case DXGI_FORMAT_BC1_UNORM_SRGB:
    case DXGI_FORMAT_BC2_TYPELESS:
    case DXGI_FORMAT_BC2_UNORM:
    case DXGI_FORMAT_BC2_UNORM_SRGB:
    case DXGI_FORMAT_BC3_TYPELESS:
    case DXGI_FORMAT_BC3_UNORM:
    case DXGI_FORMAT_BC3_UNORM_SRGB:
    case DXGI_FORMAT_BC4_TYPELESS:
    case DXGI_FORMAT_BC4_UNORM:
    case DXGI_FORMAT_BC4_SNORM:
    case DXGI_FORMAT_BC5_TYPELESS:
    case DXGI_FORMAT_BC5_UNORM:
    case DXGI_FORMAT_BC5_SNORM:
    case DXGI_FORMAT_BC6H_TYPELESS:
    case DXGI_FORMAT_BC6H_UF16:
    case DXGI_FORMAT_BC6H_SF16:
    case DXGI_FORMAT_BC7_TYPELESS:
    case DXGI_FORMAT_BC7_UNORM:
    case DXGI_FORMAT_BC7_UNORM_SRGB:
        return true;
    default:
        return false;
    }
}

// Full official path: parse DDS bytes + produce RGBA8 (or fail).
static HRESULT DecodeToRgba8(const uint8_t* bytes, size_t len, ScratchImage& outRgba) {
    ScratchImage loaded;
    TexMetadata meta{};
    HRESULT hr = LoadFromDDSMemory(bytes, len, DDS_FLAGS_NONE, &meta, loaded);
    if (FAILED(hr)) {
        return hr;
    }

    ScratchImage rgba;
    if (IsBlockCompressed(meta.format)) {
        hr = Decompress(loaded.GetImages(), loaded.GetImageCount(), loaded.GetMetadata(),
                        DXGI_FORMAT_R8G8B8A8_UNORM, rgba);
        if (FAILED(hr)) {
            return hr;
        }
    } else if (meta.format == DXGI_FORMAT_R8G8B8A8_UNORM ||
               meta.format == DXGI_FORMAT_R8G8B8A8_UNORM_SRGB) {
        rgba = std::move(loaded);
    } else if (meta.format == DXGI_FORMAT_B8G8R8A8_UNORM ||
               meta.format == DXGI_FORMAT_B8G8R8A8_UNORM_SRGB) {
        // Software swizzle when Convert is unavailable (minimal DirectXTex build).
        hr = Convert(loaded.GetImages(), loaded.GetImageCount(), loaded.GetMetadata(),
                     DXGI_FORMAT_R8G8B8A8_UNORM, TEX_FILTER_DEFAULT, TEX_THRESHOLD_DEFAULT,
                     rgba);
        if (FAILED(hr)) {
            hr = SwizzleBgraToRgba(loaded, rgba);
            if (FAILED(hr)) {
                return hr;
            }
        }
    } else {
        hr = Convert(loaded.GetImages(), loaded.GetImageCount(), loaded.GetMetadata(),
                     DXGI_FORMAT_R8G8B8A8_UNORM, TEX_FILTER_DEFAULT, TEX_THRESHOLD_DEFAULT,
                     rgba);
        if (FAILED(hr)) {
            return hr;
        }
    }
    outRgba = std::move(rgba);
    return S_OK;
}

static std::string JsonEscape(const std::string& s) {
    std::string o;
    o.reserve(s.size());
    for (char c : s) {
        if (c == '\\' || c == '"') {
            o.push_back('\\');
        }
        o.push_back(c);
    }
    return o;
}

int wmain(int argc, wchar_t** argv) {
    if (argc < 3) {
        std::fwprintf(stderr, L"Usage: dxtex_decode_bench <cases_dir> <out.json> [iters]\n");
        return 2;
    }
    const fs::path casesDir = argv[1];
    const fs::path outPath = argv[2];
    const int iters = (argc >= 4) ? _wtoi(argv[3]) : 50;
    if (iters < 1) {
        std::fwprintf(stderr, L"iters must be >= 1\n");
        return 2;
    }

    std::vector<fs::path> files;
    for (auto& ent : fs::directory_iterator(casesDir)) {
        if (ent.is_regular_file() && ent.path().extension() == L".dds") {
            files.push_back(ent.path());
        }
    }
    std::sort(files.begin(), files.end());

    std::ostringstream json;
    json << "{\n";
    json << "  \"peer\": \"Microsoft DirectXTex\",\n";
    json << "  \"repo\": \"https://github.com/microsoft/DirectXTex\",\n";
    json << "  \"protocol\": \"LoadFromDDSMemory + Decompress|Convert -> R8G8B8A8_UNORM\",\n";
    json << "  \"iters\": " << iters << ",\n";
    json << "  \"cases\": [\n";

    bool first = true;
    for (const auto& path : files) {
        const std::string name = path.stem().string();
        std::vector<uint8_t> bytes;
        try {
            bytes = ReadFileBytes(path);
        } catch (const std::exception& e) {
            std::fprintf(stderr, "skip %s: %s\n", name.c_str(), e.what());
            continue;
        }

        ScratchImage probe;
        HRESULT hr = DecodeToRgba8(bytes.data(), bytes.size(), probe);
        if (FAILED(hr)) {
            std::fprintf(stderr, "skip %s: DirectXTex hr=0x%08lx\n", name.c_str(),
                         static_cast<unsigned long>(hr));
            if (!first) {
                json << ",\n";
            }
            first = false;
            json << "    {\"id\": \"" << JsonEscape(name) << "\", \"ok\": false, \"hr\": "
                 << static_cast<unsigned long>(hr) << "}";
            continue;
        }

        // Warmup
        for (int i = 0; i < 3; ++i) {
            ScratchImage tmp;
            hr = DecodeToRgba8(bytes.data(), bytes.size(), tmp);
            if (FAILED(hr)) {
                break;
            }
        }

        using clock = std::chrono::steady_clock;
        const auto t0 = clock::now();
        size_t sink = 0;
        for (int i = 0; i < iters; ++i) {
            ScratchImage tmp;
            hr = DecodeToRgba8(bytes.data(), bytes.size(), tmp);
            if (FAILED(hr)) {
                break;
            }
            sink += tmp.GetPixelsSize();
        }
        const auto t1 = clock::now();
        if (FAILED(hr)) {
            std::fprintf(stderr, "bench failed %s hr=0x%08lx\n", name.c_str(),
                         static_cast<unsigned long>(hr));
            continue;
        }

        const double total_ns =
            std::chrono::duration<double, std::nano>(t1 - t0).count();
        const double ns_per_iter = total_ns / static_cast<double>(iters);

        const auto& meta = probe.GetMetadata();
        if (!first) {
            json << ",\n";
        }
        first = false;
        json << "    {\"id\": \"" << JsonEscape(name) << "\", \"ok\": true, "
             << "\"ns_per_iter\": " << ns_per_iter << ", "
             << "\"width\": " << meta.width << ", "
             << "\"height\": " << meta.height << ", "
             << "\"depth\": " << meta.depth << ", "
             << "\"pixels_bytes\": " << probe.GetPixelsSize() << ", "
             << "\"sink\": " << sink << "}";
        std::fprintf(stderr, "%s: %.1f ns/iter\n", name.c_str(), ns_per_iter);
    }

    json << "\n  ]\n}\n";

    std::ofstream out(outPath, std::ios::binary);
    if (!out) {
        std::fwprintf(stderr, L"failed to write output json\n");
        return 1;
    }
    const auto s = json.str();
    out.write(s.data(), static_cast<std::streamsize>(s.size()));
    return 0;
}
