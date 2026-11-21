// astc-oracle: a batch front end over ARM astcenc 5.7.1, used as a black-box
// reference for the rusty_dds ASTC decoder and encoder.
//
// This file is ours (MIT, like the rest of rusty_dds). It links the astcenc
// static library (Apache-2.0) and includes its headers at build time, in WSL
// only. It is never compiled by cargo, never part of the published package and
// never part of the crate's dependency graph. See README.md.
//
// Every command reads fixed-size input records on stdin and writes fixed-size
// binary records on stdout. Single-threaded and deterministic.
//
//   oracle decode <profile> <u8|f16> <bx> <by> <bz>
//       in : 16-byte blocks
//       out: per block, texels in (z, y, x) order, RGBA; u8 = 4 bytes/texel,
//            f16 = 4 little-endian u16 half bit patterns/texel.
//       Uses the public astcenc_decompress_image on a strip of blocks, so it is
//       exactly what the astcenc CLI does for that output type.
//
//   oracle info <profile> <bx> <by> <bz>
//       in : 16-byte blocks
//       out: one INFO record per block (layout below), from physical_to_symbolic,
//            unpack_color_endpoints and unpack_weights.
//
//   oracle ise <quant 0..20> <count 1..64>
//       in : 72-byte bit strings (LSB-first)
//       out: <count> bytes per input (the raw ISE values, decode_ise)
//
//   oracle modes <bx> <by> <bz>
//       out: 2048 MODE records (no stdin)
//
//   oracle partitions <bx> <by> <bz>
//       out: for count 2..4, seed 0..1023: texel_count bytes, partition_of_texel
//            (no stdin)
//
// profile: ldr-srgb | ldr | hdr-ldr-a | hdr

#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <cstdint>
#include <vector>
#include <string>

#include "astcenc.h"
#include "astcenc_internal.h"
#include "astcenc_internal_entry.h"

static void die(const char* msg) {
	fprintf(stderr, "astc-oracle: %s\n", msg);
	exit(2);
}

static astcenc_profile parse_profile(const char* s) {
	std::string p(s);
	if (p == "ldr-srgb") return ASTCENC_PRF_LDR_SRGB;
	if (p == "ldr") return ASTCENC_PRF_LDR;
	if (p == "hdr-ldr-a") return ASTCENC_PRF_HDR_RGB_LDR_A;
	if (p == "hdr") return ASTCENC_PRF_HDR;
	die("bad profile");
	return ASTCENC_PRF_LDR;
}

static astcenc_context* make_ctx(astcenc_profile prof, unsigned bx, unsigned by, unsigned bz) {
	astcenc_config cfg;
	// A full context (no self-decompress flag) keeps every block mode and every
	// partitioning (can_omit_* false), which is what decoding arbitrary blocks needs.
	astcenc_error e = astcenc_config_init(prof, bx, by, bz, ASTCENC_PRE_MEDIUM, 0, &cfg);
	if (e != ASTCENC_SUCCESS) die(astcenc_get_error_string(e));
	astcenc_context* ctx = nullptr;
	e = astcenc_context_alloc(&cfg, 1, &ctx, nullptr);
	if (e != ASTCENC_SUCCESS) die(astcenc_get_error_string(e));
	return ctx;
}

static bool read_exact(void* buf, size_t n) {
	return fread(buf, 1, n, stdin) == n;
}

static void put(const void* p, size_t n) {
	if (fwrite(p, 1, n, stdout) != n) die("write failed");
}

static int cmd_decode(int argc, char** argv) {
	if (argc != 7) die("usage: decode <profile> <u8|f16> <bx> <by> <bz>");
	astcenc_profile prof = parse_profile(argv[2]);
	bool u8 = std::string(argv[3]) == "u8";
	if (!u8 && std::string(argv[3]) != "f16") die("type must be u8 or f16");
	unsigned bx = atoi(argv[4]), by = atoi(argv[5]), bz = atoi(argv[6]);
	astcenc_context* ctx = make_ctx(prof, bx, by, bz);
	const size_t chunk = 4096;
	std::vector<uint8_t> in(chunk * 16);
	astcenc_swizzle swz { ASTCENC_SWZ_R, ASTCENC_SWZ_G, ASTCENC_SWZ_B, ASTCENC_SWZ_A };
	std::vector<uint8_t> o8;
	std::vector<uint16_t> o16;
	for (;;) {
		size_t got = fread(in.data(), 16, chunk, stdin);
		if (got == 0) break;
		unsigned w = (unsigned)got * bx, h = by, d = bz;
		size_t px = (size_t)w * h * d;
		std::vector<void*> slices(d);
		if (u8) {
			o8.assign(px * 4, 0);
			for (unsigned z = 0; z < d; z++) slices[z] = o8.data() + (size_t)z * w * h * 4;
		} else {
			o16.assign(px * 4, 0);
			for (unsigned z = 0; z < d; z++) slices[z] = o16.data() + (size_t)z * w * h * 4;
		}
		astcenc_image img;
		img.dim_x = w; img.dim_y = h; img.dim_z = d;
		img.data_type = u8 ? ASTCENC_TYPE_U8 : ASTCENC_TYPE_F16;
		img.data = slices.data();
		astcenc_error e = astcenc_decompress_image(ctx, in.data(), got * 16, &img, &swz, 0);
		if (e != ASTCENC_SUCCESS) die(astcenc_get_error_string(e));
		astcenc_decompress_reset(ctx);
		// Reorder the strip into per-block texel order.
		for (size_t b = 0; b < got; b++) {
			for (unsigned z = 0; z < bz; z++)
			for (unsigned y = 0; y < by; y++)
			for (unsigned x = 0; x < bx; x++) {
				size_t src = (((size_t)z * h + y) * w + b * bx + x) * 4;
				if (u8) put(&o8[src], 4);
				else put(&o16[src], 8);
			}
		}
	}
	astcenc_context_free(ctx);
	return 0;
}

// INFO record, little-endian, fixed size:
//   u8  block_type (0 error, 1 const f16, 2 const u16, 3 normal)
//   u8  partition_count
//   u16 partition_index
//   u16 block_mode
//   u8  color_formats[4]
//   u8  color_quant (quant_method 0..20)
//   u8  weight_quant (quant_method 0..20)
//   i8  plane2_component (-1 single plane)
//   u8  grid_x, grid_y, grid_z
//   u8  color_formats_matched
//   u8  weight_count (stored weights, both planes)
//   u16 constant_color[4]           (void extent; 0 otherwise)
//   u8  color_values[4][8]          (unquantised 0..255)
//   u8  weights[64]                 (unquantised 0..64, astcenc layout: plane 2 at +32)
//   i32 ep[4][2][4]                 (unpack_color_endpoints output)
//   u8  rgb_hdr[4], a_hdr[4]
//   u8  w1[216], w2[216]            (per-texel infilled weights 0..64)
//   u8  part[216]                   (partition_of_texel)
static const size_t INFO_SIZE = 1 + 1 + 2 + 2 + 4 + 1 + 1 + 1 + 3 + 1 + 1 + 8 + 32 + 64 + 128 + 8 + 216 * 3;

static int cmd_info(int argc, char** argv) {
	if (argc != 6) die("usage: info <profile> <bx> <by> <bz>");
	astcenc_profile prof = parse_profile(argv[2]);
	unsigned bx = atoi(argv[3]), by = atoi(argv[4]), bz = atoi(argv[5]);
	astcenc_context* ctxo = make_ctx(prof, bx, by, bz);
	const block_size_descriptor& bsd = *ctxo->context.bsd;
	uint8_t blk[16];
	std::vector<uint8_t> rec(INFO_SIZE);
	while (read_exact(blk, 16)) {
		std::fill(rec.begin(), rec.end(), 0);
		symbolic_compressed_block scb;
		memset(&scb, 0, sizeof(scb));
		physical_to_symbolic(bsd, blk, scb);
		size_t o = 0;
		auto w8 = [&](int v) { rec[o++] = (uint8_t)v; };
		auto w16 = [&](int v) { rec[o++] = (uint8_t)v; rec[o++] = (uint8_t)(v >> 8); };
		auto w32 = [&](int v) { for (int i = 0; i < 4; i++) rec[o++] = (uint8_t)((uint32_t)v >> (8 * i)); };
		w8(scb.block_type);
		bool normal = scb.block_type == SYM_BTYPE_NONCONST;
		bool cnst = scb.block_type == SYM_BTYPE_CONST_F16 || scb.block_type == SYM_BTYPE_CONST_U16;
		w8(normal ? scb.partition_count : 0);
		w16(normal ? scb.partition_index : 0);
		w16(normal ? scb.block_mode : 0);
		for (int i = 0; i < 4; i++) w8(normal && i < scb.partition_count ? scb.color_formats[i] : 0);
		w8(normal ? (int)scb.quant_mode : 0);
		const block_mode* bm = nullptr;
		const decimation_info* di = nullptr;
		if (normal) {
			bm = &bsd.get_block_mode(scb.block_mode);
			di = &bsd.get_decimation_info(bm->decimation_mode);
		}
		w8(normal ? bm->quant_mode : 0);
		w8(normal ? (bm->is_dual_plane ? scb.plane2_component : -1) : -1);
		w8(normal ? di->weight_x : 0);
		w8(normal ? di->weight_y : 0);
		w8(normal ? di->weight_z : 0);
		w8(normal ? scb.color_formats_matched : 0);
		w8(normal ? di->weight_count * (bm->is_dual_plane ? 2 : 1) : 0);
		for (int i = 0; i < 4; i++) w16(cnst ? scb.constant_color[i] : 0);
		for (int p = 0; p < 4; p++)
			for (int i = 0; i < 8; i++)
				w8(normal && p < scb.partition_count ? scb.color_values[p][i] : 0);
		for (int i = 0; i < 64; i++) w8(normal ? scb.weights[i] : 0);
		int eps[4][2][4] = {};
		uint8_t rh[4] = {}, ah[4] = {};
		if (normal) {
			for (int p = 0; p < scb.partition_count; p++) {
				bool rgb_hdr, a_hdr;
				vint4 e0, e1;
				unpack_color_endpoints(prof, scb.color_formats[p], scb.color_values[p], rgb_hdr, a_hdr, e0, e1);
				alignas(16) int t0[4], t1[4];
				store(e0, t0);
				store(e1, t1);
				for (int c = 0; c < 4; c++) { eps[p][0][c] = t0[c]; eps[p][1][c] = t1[c]; }
				rh[p] = rgb_hdr; ah[p] = a_hdr;
			}
		}
		for (int p = 0; p < 4; p++) for (int e = 0; e < 2; e++) for (int c = 0; c < 4; c++) w32(eps[p][e][c]);
		for (int p = 0; p < 4; p++) w8(rh[p]);
		for (int p = 0; p < 4; p++) w8(ah[p]);
		if (normal) {
			alignas(32) int wp1[BLOCK_MAX_TEXELS + 16] = {};
			alignas(32) int wp2[BLOCK_MAX_TEXELS + 16] = {};
			unpack_weights(bsd, scb, *di, bm->is_dual_plane, wp1, wp2);
			for (int i = 0; i < 216; i++) w8(i < bsd.texel_count ? wp1[i] : 0);
			for (int i = 0; i < 216; i++) w8(i < bsd.texel_count && bm->is_dual_plane ? wp2[i] : 0);
			const partition_info& pi = bsd.get_partition_info(scb.partition_count, scb.partition_index);
			for (int i = 0; i < 216; i++) w8(i < bsd.texel_count ? pi.partition_of_texel[i] : 0);
		} else {
			o += 216 * 3;
		}
		if (o != INFO_SIZE) die("info record size mismatch");
		put(rec.data(), INFO_SIZE);
	}
	astcenc_context_free(ctxo);
	return 0;
}

static int cmd_ise(int argc, char** argv) {
	if (argc != 4) die("usage: ise <quant> <count>");
	int q = atoi(argv[2]);
	int n = atoi(argv[3]);
	if (q < 0 || q > 20 || n < 1 || n > 64) die("bad ise args");
	uint8_t in[72 + 16];
	uint8_t out[64];
	while (read_exact(in, 72)) {
		memset(in + 72, 0, 16);
		decode_ise((quant_method)q, (unsigned)n, in, out, 0);
		put(out, (size_t)n);
	}
	return 0;
}

// MODE record: u8 valid, u8 grid_x, grid_y, grid_z, u8 weight_quant, u8 dual,
// u8 weight_count (both planes), u8 weight_bits.
static int cmd_modes(int argc, char** argv) {
	if (argc != 5) die("usage: modes <bx> <by> <bz>");
	unsigned bx = atoi(argv[2]), by = atoi(argv[3]), bz = atoi(argv[4]);
	astcenc_context* ctxo = make_ctx(ASTCENC_PRF_LDR, bx, by, bz);
	const block_size_descriptor& bsd = *ctxo->context.bsd;
	for (unsigned m = 0; m < 2048; m++) {
		uint8_t r[8] = {};
		unsigned packed = bsd.block_mode_packed_index[m];
		if (packed != BLOCK_BAD_BLOCK_MODE) {
			const block_mode& bm = bsd.block_modes[packed];
			const decimation_info& di = bsd.get_decimation_info(bm.decimation_mode);
			r[0] = 1;
			r[1] = di.weight_x; r[2] = di.weight_y; r[3] = di.weight_z;
			r[4] = bm.quant_mode;
			r[5] = bm.is_dual_plane;
			r[6] = (uint8_t)(di.weight_count * (bm.is_dual_plane ? 2 : 1));
			r[7] = bm.weight_bits;
		}
		put(r, 8);
	}
	astcenc_context_free(ctxo);
	return 0;
}

static int cmd_partitions(int argc, char** argv) {
	if (argc != 5) die("usage: partitions <bx> <by> <bz>");
	unsigned bx = atoi(argv[2]), by = atoi(argv[3]), bz = atoi(argv[4]);
	astcenc_context* ctxo = make_ctx(ASTCENC_PRF_LDR, bx, by, bz);
	const block_size_descriptor& bsd = *ctxo->context.bsd;
	for (unsigned c = 2; c <= 4; c++)
		for (unsigned s = 0; s < 1024; s++) {
			const partition_info& pi = bsd.get_partition_info(c, s);
			put(pi.partition_of_texel, bsd.texel_count);
		}
	astcenc_context_free(ctxo);
	return 0;
}

int main(int argc, char** argv) {
	if (argc < 2) die("usage: oracle <decode|info|ise|modes|partitions> ...");
	std::string c(argv[1]);
	if (c == "decode") return cmd_decode(argc, argv);
	if (c == "info") return cmd_info(argc, argv);
	if (c == "ise") return cmd_ise(argc, argv);
	if (c == "modes") return cmd_modes(argc, argv);
	if (c == "partitions") return cmd_partitions(argc, argv);
	die("unknown command");
	return 2;
}
