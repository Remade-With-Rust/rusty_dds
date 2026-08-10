// Times Microsoft DirectXTex CPU Compress (or Convert for uncompressed)
// on RGBA8 case files written by rusty_dds bench_baselines.
//
// Case dir layout (per id):
//   <id>.rgba   — tightly packed RGBA8 slices (depth major)
//   <id>.meta   — key=value: width, height, depth, dxgi
//
// Usage:
//   dxtex_encode_bench.exe <cases_dir> <out.json> [iters]
//
// BC7 uses TEX_COMPRESS_BC7_QUICK (mode-6 class) to match rusty_dds's
// mode-6-only encoder; other BCn use TEX_COMPRESS_DEFAULT.

#include <DirectXTex.h>

#include <algorithm>
#include <chrono>
#include <cstdint>
#include <cstdio>
#include <cstring>
#include <filesystem>
#include <fstream>
#include <sstream>
#include <stdexcept>
#include <string>
#include <unordered_map>
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

static std::unordered_map<std::string, std::string> ReadMeta(const fs::path& path) {
    std::ifstream in(path);
    if (!in) {
        throw std::runtime_error("failed to open meta " + path.string());
    }
    std::unordered_map<std::string, std::string> m;
    std::string line;
    while (std::getline(in, line)) {
        if (line.empty() || line[0] == '#') {
            continue;
        }
        const auto eq = line.find('=');
        if (eq == std::string::npos) {
            continue;
        }
        m[line.substr(0, eq)] = line.substr(eq + 1);
    }
    return m;
}

static DXGI_FORMAT ParseDxgi(const std::string& name) {
    if (name == "BC1_UNORM") return DXGI_FORMAT_BC1_UNORM;
    if (name == "BC2_UNORM") return DXGI_FORMAT_BC2_UNORM;
    if (name == "BC3_UNORM") return DXGI_FORMAT_BC3_UNORM;
    if (name == "BC4_UNORM") return DXGI_FORMAT_BC4_UNORM;
    if (name == "BC4_SNORM") return DXGI_FORMAT_BC4_SNORM;
    if (name == "BC5_UNORM") return DXGI_FORMAT_BC5_UNORM;
    if (name == "BC5_SNORM") return DXGI_FORMAT_BC5_SNORM;
    if (name == "BC7_UNORM") return DXGI_FORMAT_BC7_UNORM;
    if (name == "R8G8B8A8_UNORM") return DXGI_FORMAT_R8G8B8A8_UNORM;
    if (name == "B8G8R8A8_UNORM") return DXGI_FORMAT_B8G8R8A8_UNORM;
    throw std::runtime_error("unknown dxgi " + name);
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

static HRESULT BuildRgbaSource(const uint8_t* rgba, size_t rgba_len, size_t w, size_t h,
                               size_t d, ScratchImage& out) {
    TexMetadata meta{};
    meta.width = w;
    meta.height = h;
    meta.depth = d;
    meta.arraySize = 1;
    meta.mipLevels = 1;
    meta.miscFlags = 0;
    meta.miscFlags2 = 0;
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
        if (!img || !img->pixels) {
            return E_FAIL;
        }
        // DirectXTex rowPitch may include padding; copy row-by-row.
        const uint8_t* src = rgba + z * slice;
        for (size_t y = 0; y < h; ++y) {
            memcpy(img->pixels + y * img->rowPitch, src + y * w * 4, w * 4);
        }
    }
    return S_OK;
}

static HRESULT SwizzleRgbaToBgra(const ScratchImage& src, ScratchImage& dst) {
    const TexMetadata& meta = src.GetMetadata();
    TexMetadata outMeta = meta;
    outMeta.format = DXGI_FORMAT_B8G8R8A8_UNORM;
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

static HRESULT EncodeOnce(const ScratchImage& src, DXGI_FORMAT target, ScratchImage& out) {
    if (IsBlockCompressed(target)) {
        TEX_COMPRESS_FLAGS flags = TEX_COMPRESS_DEFAULT;
        if (target == DXGI_FORMAT_BC7_UNORM) {
            // Mode-6-class path — closer to rusty_dds's BC7 encoder.
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
        std::fwprintf(stderr, L"Usage: dxtex_encode_bench <cases_dir> <out.json> [iters]\n");
        return 2;
    }
    const fs::path casesDir = argv[1];
    const fs::path outPath = argv[2];
    const int iters = (argc >= 4) ? _wtoi(argv[3]) : 40;
    if (iters < 1) {
        return 2;
    }

    std::vector<fs::path> metas;
    for (auto& ent : fs::directory_iterator(casesDir)) {
        if (ent.is_regular_file() && ent.path().extension() == L".meta") {
            metas.push_back(ent.path());
        }
    }
    std::sort(metas.begin(), metas.end());

    std::ostringstream json;
    json << "{\n";
    json << "  \"peer\": \"Microsoft DirectXTex\",\n";
    json << "  \"repo\": \"https://github.com/microsoft/DirectXTex\",\n";
    json << "  \"protocol\": \"RGBA8 ScratchImage + Compress|Convert (BC7=TEX_COMPRESS_BC7_QUICK)\",\n";
    json << "  \"iters\": " << iters << ",\n";
    json << "  \"cases\": [\n";

    bool first = true;
    for (const auto& metaPath : metas) {
        const std::string id = metaPath.stem().string();
        const fs::path rgbaPath = metaPath.parent_path() / (id + ".rgba");
        try {
            auto kv = ReadMeta(metaPath);
            const size_t w = static_cast<size_t>(std::stoul(kv.at("width")));
            const size_t h = static_cast<size_t>(std::stoul(kv.at("height")));
            const size_t d = static_cast<size_t>(std::stoul(kv.at("depth")));
            const DXGI_FORMAT target = ParseDxgi(kv.at("dxgi"));
            auto rgba = ReadFileBytes(rgbaPath);

            ScratchImage src;
            HRESULT hr = BuildRgbaSource(rgba.data(), rgba.size(), w, h, d, src);
            if (FAILED(hr)) {
                throw std::runtime_error("BuildRgbaSource failed");
            }

            ScratchImage probe;
            hr = EncodeOnce(src, target, probe);
            if (FAILED(hr)) {
                if (!first) json << ",\n";
                first = false;
                json << "    {\"id\": \"" << JsonEscape(id) << "\", \"ok\": false, \"hr\": "
                     << static_cast<unsigned long>(hr) << "}";
                std::fprintf(stderr, "skip %s: hr=0x%08lx\n", id.c_str(),
                             static_cast<unsigned long>(hr));
                continue;
            }

            for (int i = 0; i < 2; ++i) {
                ScratchImage tmp;
                hr = EncodeOnce(src, target, tmp);
            }

            using clock = std::chrono::steady_clock;
            const auto t0 = clock::now();
            size_t sink = 0;
            for (int i = 0; i < iters; ++i) {
                ScratchImage tmp;
                hr = EncodeOnce(src, target, tmp);
                if (FAILED(hr)) {
                    break;
                }
                sink += tmp.GetPixelsSize();
            }
            const auto t1 = clock::now();
            if (FAILED(hr)) {
                continue;
            }

            const double ns_per_iter =
                std::chrono::duration<double, std::nano>(t1 - t0).count() /
                static_cast<double>(iters);

            if (!first) json << ",\n";
            first = false;
            json << "    {\"id\": \"" << JsonEscape(id) << "\", \"ok\": true, "
                 << "\"ns_per_iter\": " << ns_per_iter << ", "
                 << "\"width\": " << w << ", \"height\": " << h << ", \"depth\": " << d << ", "
                 << "\"out_bytes\": " << probe.GetPixelsSize() << ", "
                 << "\"sink\": " << sink << "}";
            std::fprintf(stderr, "%s: %.1f ns/iter\n", id.c_str(), ns_per_iter);
        } catch (const std::exception& e) {
            std::fprintf(stderr, "skip %s: %s\n", id.c_str(), e.what());
            if (!first) json << ",\n";
            first = false;
            json << "    {\"id\": \"" << JsonEscape(id)
                 << "\", \"ok\": false, \"error\": \"" << JsonEscape(e.what()) << "\"}";
        }
    }

    json << "\n  ]\n}\n";
    std::ofstream out(outPath, std::ios::binary);
    if (!out) {
        return 1;
    }
    const auto s = json.str();
    out.write(s.data(), static_cast<std::streamsize>(s.size()));
    return 0;
}
