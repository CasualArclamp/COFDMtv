/*
Reference multi-frame (CRS) chunk encoder: the logic of aicodix/crs encode.cc, with
std::vector instead of std::aligned_alloc (which MinGW lacks), for checking COFDMtv's
chunks against it. Each chunk file is CHUNKBYTES long (zero padded), ready to be sent as
one COFDMTV image payload with CHUNKBYTES = 5380.

usage: crs_chunks INPUT CHUNKBYTES CHUNK...
*/

#include <algorithm>
#include <cstdint>
#include <cstdlib>
#include <cstring>
#include <iostream>
#include <vector>
#include "crc.hh"
#include "galois_field.hh"
#include "cauchy_reed_solomon_erasure_coding.hh"
#include "wav_io.hh"

int main(int argc, char **argv) {
	if (argc < 4) {
		std::cerr << "usage: " << argv[0] << " INPUT CHUNKBYTES CHUNK..." << std::endl;
		return 1;
	}
	std::vector<uint8_t> input = read_file(argv[1]);
	int input_bytes = input.size();
	if (input_bytes < 1 || input_bytes > 16777216) {
		std::cerr << "bad input size" << std::endl;
		return 1;
	}
	int chunk_bytes = std::atoi(argv[2]);
	int chunk_count = argc - 3;
	int crs_overhead = 3 + 2 + 2 + 3 + 4;
	int avail_bytes = (chunk_bytes - crs_overhead) & ~1;
	int block_count = (input_bytes + avail_bytes - 1) / avail_bytes;
	if (avail_bytes < 1 || avail_bytes > 65536 || block_count > 1024 || chunk_count < block_count) {
		std::cerr << "need at least " << block_count << " chunks of a sensible size" << std::endl;
		return 1;
	}
	std::cerr << "CRS(" << chunk_count << ", " << block_count << ")" << std::endl;
	const int SIMD2 = 64;
	int dirty_bytes = (input_bytes + block_count - 1) / block_count;
	int block_bytes = dirty_bytes;
	if (block_bytes % SIMD2)
		block_bytes += SIMD2 - (block_bytes % SIMD2);
	std::vector<uint8_t> data(block_count * block_bytes, 0);
	CODE::CRC<uint32_t> crc(0x8F6E37A0);
	for (int i = 0, j = 0; i < block_count; ++i) {
		j += dirty_bytes;
		int copy_bytes = dirty_bytes;
		if (j > input_bytes)
			copy_bytes -= j - input_bytes;
		for (int k = 0; k < copy_bytes; ++k) {
			data[block_bytes * i + k] = input[i * dirty_bytes + k];
			crc(data[block_bytes * i + k]);
		}
	}
	typedef CODE::GaloisField<16, 0b10001000000001011, uint16_t> GF;
	static GF instance;
	CODE::CauchyReedSolomonErasureCoding<GF> crs;
	std::vector<uint8_t> block(block_bytes);
	for (int i = 0; i < chunk_count; ++i) {
		int ident = block_count + i;
		crs.encode(data.data(), block.data(), ident, block_bytes, block_count);
		std::vector<uint8_t> chunk(chunk_bytes, 0);
		chunk[0] = 'C';
		chunk[1] = 'R';
		chunk[2] = 'S';
		uint16_t splits = block_count - 1;
		chunk[3] = splits;
		chunk[4] = splits >> 8;
		chunk[5] = ident;
		chunk[6] = ident >> 8;
		int32_t size = input_bytes - 1;
		chunk[7] = size;
		chunk[8] = size >> 8;
		chunk[9] = size >> 16;
		uint32_t crc32 = crc();
		for (int k = 0; k < 4; ++k)
			chunk[10 + k] = crc32 >> (8 * k);
		int copy = dirty_bytes + (dirty_bytes & 1);
		for (int k = 0; k < copy; ++k)
			chunk[14 + k] = block[k];
		if (!write_file(argv[3 + i], chunk.data(), chunk.size())) {
			std::cerr << "cannot write " << argv[3 + i] << std::endl;
			return 1;
		}
	}
	return 0;
}
