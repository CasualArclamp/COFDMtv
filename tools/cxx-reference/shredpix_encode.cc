/*
Reference COFDMTV image encoder: drives the original Shredpix encoder (aicodix/shredpix,
app/src/main/cpp) to write a WAV file, for checking COFDMtv against it.

usage: shredpix_encode OUT.wav RATE MODE CALLSIGN CARRIER NOISE FANCY CHANNEL [PAYLOAD]

MODE 0 sends a ping (no payload), 6..13 an image payload (a file of at most 5380 bytes,
zero padded). CHANNEL: 0 mono, 1 first, 2 second, 4 analytic (I/Q); 1, 2 and 4 write
stereo. One second of silence is added before and after the signal.
*/

#include <algorithm>
#include <cstdint>
#include <cstdlib>
#include <cstring>
#include <iostream>
#define assert(expr)
#include "encoder.hh"
#include "wav_io.hh"

template <int RATE>
static int run(const char *out, int mode, const char *call, int carrier, int noise, bool fancy, int channel, const std::vector<uint8_t> &payload) {
	auto *enc = new Encoder<RATE>();
	enc->configure(payload.data(), reinterpret_cast<const int8_t *>(call), mode, carrier, noise, fancy);
	const int symbol_length = (1280 * RATE) / 8000;
	const int extended_length = symbol_length + symbol_length / 8;
	const int channels = channel ? 2 : 1;
	std::vector<int16_t> samples(RATE * channels, 0);
	std::vector<int16_t> block(extended_length * channels);
	int symbols = 0;
	while (enc->produce(block.data(), channel)) {
		samples.insert(samples.end(), block.begin(), block.end());
		++symbols;
	}
	samples.insert(samples.end(), RATE * channels, 0);
	delete enc;
	if (!write_wav(out, RATE, channels, samples)) {
		std::cerr << "cannot write " << out << std::endl;
		return 1;
	}
	std::cerr << "wrote " << symbols << " symbols to " << out << std::endl;
	return 0;
}

int main(int argc, char **argv) {
	if (argc < 9) {
		std::cerr << "usage: " << argv[0] << " OUT.wav RATE MODE CALLSIGN CARRIER NOISE FANCY CHANNEL [PAYLOAD]" << std::endl;
		return 1;
	}
	const char *out = argv[1];
	int rate = std::atoi(argv[2]);
	int mode = std::atoi(argv[3]);
	const char *call = argv[4];
	int carrier = std::atoi(argv[5]);
	int noise = std::atoi(argv[6]);
	bool fancy = std::atoi(argv[7]);
	int channel = std::atoi(argv[8]);
	std::vector<uint8_t> payload(5380, 0);
	if (mode) {
		if (argc < 10) {
			std::cerr << "MODE " << mode << " needs a PAYLOAD file" << std::endl;
			return 1;
		}
		std::vector<uint8_t> data = read_file(argv[9]);
		if (data.empty() || data.size() > payload.size()) {
			std::cerr << "payload must be 1.." << payload.size() << " bytes" << std::endl;
			return 1;
		}
		std::copy(data.begin(), data.end(), payload.begin());
	}
	switch (rate) {
	case 8000: return run<8000>(out, mode, call, carrier, noise, fancy, channel, payload);
	case 16000: return run<16000>(out, mode, call, carrier, noise, fancy, channel, payload);
	case 32000: return run<32000>(out, mode, call, carrier, noise, fancy, channel, payload);
	case 44100: return run<44100>(out, mode, call, carrier, noise, fancy, channel, payload);
	case 48000: return run<48000>(out, mode, call, carrier, noise, fancy, channel, payload);
	}
	std::cerr << "unsupported rate " << rate << std::endl;
	return 1;
}
