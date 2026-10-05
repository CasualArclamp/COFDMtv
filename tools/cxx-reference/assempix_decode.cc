/*
Reference COFDMTV image decoder: drives the original Assempix decoder (aicodix/assempix,
app/src/main/cpp) over a WAV file, with the app's multi-frame (CRS) bookkeeping from
MainActivity.java, for checking COFDMtv against it.

usage: assempix_decode IN.wav OUTPREFIX [CHANNEL]

CHANNEL: 0 mono, 1 first, 2 second, 3 sum, 4 analytic (I/Q); default 0 for mono files,
1 for stereo. Each decoded payload is written to OUTPREFIX_N.bin (5380 bytes), a
reassembled multi-frame image to OUTPREFIX_crs.bin. Events go to stdout, one per line.
*/

#include <algorithm>
#include <cstdint>
#include <cstdlib>
#include <cstring>
#include <iostream>
#include <set>
#define assert(expr)
#include "crsec.hh"
#include "decoder.hh"
#include "wav_io.hh"

static void crs_payload(CauchyReedSolomonErasureCoding &crsec, const uint8_t *payload, const std::string &prefix) {
	static int current_count = 0, current_bytes = 0;
	static long current_crc = 0;
	static std::set<int> idents;
	const int overhead = 14, chunks_max = 12, avail = 5380;
	const int bytes_max = (avail - overhead) * chunks_max;
	int count = (payload[4] << 8) + payload[3] + 1;
	int ident = (payload[6] << 8) + payload[5];
	int bytes = (payload[9] << 16) + (payload[8] << 8) + payload[7] + 1;
	long crc = (long(payload[13]) << 24) + (payload[12] << 16) + (payload[11] << 8) + payload[10];
	if (count > chunks_max || ident < count || bytes > bytes_max) {
		std::cout << "CRS unsupported" << std::endl;
		return;
	}
	if (current_count != count || current_bytes != bytes || current_crc != crc) {
		idents.clear();
		current_count = count;
		current_bytes = bytes;
		current_crc = crc;
	}
	if (idents.count(ident)) {
		std::cout << "CRS duplicate ident=" << ident << std::endl;
		return;
	}
	if (int(idents.size()) == count) {
		std::cout << "CRS redundant ident=" << ident << std::endl;
		return;
	}
	crsec.chunk(payload, idents.size(), ident);
	idents.insert(ident);
	std::cout << "CRS chunk ident=" << ident << " have=" << idents.size() << "/" << count << std::endl;
	if (int(idents.size()) < count)
		return;
	std::vector<uint8_t> data(bytes);
	long got = crsec.recover(data.data(), bytes, idents.size());
	if (got != current_crc) {
		std::cout << "CRS corrupted" << std::endl;
	} else {
		std::string name = prefix + "_crs.bin";
		write_file(name, data.data(), data.size());
		std::cout << "CRS complete bytes=" << bytes << " -> " << name << std::endl;
	}
	current_count = current_bytes = 0;
	current_crc = 0;
}

template <int RATE>
static int run(const Wav &wav, const std::string &prefix, int channel) {
	auto *dec = new Decoder<RATE>();
	auto *crsec = new CauchyReedSolomonErasureCoding();
	const int symbol_length = (1280 * RATE) / 8000;
	const int extended_length = symbol_length + symbol_length / 8;
	const int channels = wav.channels;
	std::vector<uint32_t> spectrum(640 * 64), spectrogram(640 * 64), constellation(64 * 64), peak(16);
	std::vector<uint8_t> payload(5380);
	int decoded = 0;
	size_t frames = wav.samples.size() / channels;
	// The app hands the decoder whole blocks of extended_length frames.
	std::vector<int16_t> block(extended_length * (channel ? 2 : 1));
	for (size_t start = 0; start + extended_length <= frames; start += extended_length) {
		for (int i = 0; i < extended_length; ++i) {
			const int16_t *frame = wav.samples.data() + (start + i) * channels;
			if (channel) {
				block[2 * i] = frame[0];
				block[2 * i + 1] = channels > 1 ? frame[1] : frame[0];
			} else {
				block[i] = frame[0];
			}
		}
		int status = dec->process(spectrum.data(), spectrogram.data(), constellation.data(), peak.data(), block.data(), channel, 0x00ffffff);
		float cfo;
		int32_t mode;
		int8_t call[10] = {0};
		switch (status) {
		case STATUS_FAIL:
			std::cout << "FAIL at=" << start << std::endl;
			break;
		case STATUS_NOPE:
			dec->cached(&cfo, &mode, call);
			std::cout << "NOPE cfo=" << cfo << " mode=" << mode << " call=" << reinterpret_cast<char *>(call) << std::endl;
			break;
		case STATUS_SYNC:
			dec->cached(&cfo, &mode, call);
			std::cout << "SYNC at=" << start << " cfo=" << cfo << " mode=" << mode << " call=" << reinterpret_cast<char *>(call) << std::endl;
			break;
		case STATUS_DONE: {
			int flips = dec->fetch(payload.data());
			if (flips < 0) {
				std::cout << "DONE failed" << std::endl;
				break;
			}
			std::string name = prefix + "_" + std::to_string(++decoded) + ".bin";
			write_file(name, payload.data(), payload.size());
			std::cout << "DONE flips=" << flips << " -> " << name << std::endl;
			if (payload[0] == 'C' && payload[1] == 'R' && payload[2] == 'S')
				crs_payload(*crsec, payload.data(), prefix);
			break;
		}
		}
	}
	delete crsec;
	delete dec;
	return 0;
}

int main(int argc, char **argv) {
	if (argc < 3) {
		std::cerr << "usage: " << argv[0] << " IN.wav OUTPREFIX [CHANNEL]" << std::endl;
		return 1;
	}
	Wav wav;
	if (!read_wav(argv[1], wav)) {
		std::cerr << "cannot read " << argv[1] << " (16-bit PCM or 32-bit float WAV)" << std::endl;
		return 1;
	}
	int channel = argc > 3 ? std::atoi(argv[3]) : (wav.channels > 1 ? 1 : 0);
	std::string prefix = argv[2];
	switch (wav.rate) {
	case 8000: return run<8000>(wav, prefix, channel);
	case 16000: return run<16000>(wav, prefix, channel);
	case 32000: return run<32000>(wav, prefix, channel);
	case 44100: return run<44100>(wav, prefix, channel);
	case 48000: return run<48000>(wav, prefix, channel);
	}
	std::cerr << "unsupported rate " << wav.rate << std::endl;
	return 1;
}
