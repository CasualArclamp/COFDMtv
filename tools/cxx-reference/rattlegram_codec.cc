/*
Reference COFDMTV text encoder and decoder: drives the original Rattlegram codec
(aicodix/rattlegram, app/src/main/cpp) for checking COFDMtv against it.

usage: rattlegram_codec encode OUT.wav RATE CALLSIGN CARRIER NOISE FANCY CHANNEL [TEXT]
       rattlegram_codec decode IN.wav [CHANNEL]

An empty TEXT sends a ping; @FILE reads the text from FILE (UTF-8 survives that, unlike a
Windows command line). CHANNEL: 0 mono, 1 first, 2 second, 3 sum (decode only),
4 analytic (I/Q). Encoding adds one second of silence before and after the signal.
Decoding feeds 20 ms blocks as the app does and prints events to stdout, one per line.
*/

#include <algorithm>
#include <cstdint>
#include <cstdlib>
#include <cstring>
#include <cstring>
#include <iostream>
#define assert(expr) do {} while (0)
#include "encoder.hh"
#include "decoder.hh"
#include "wav_io.hh"

template <int RATE>
static int encode(const char *out, const char *call, int carrier, int noise, bool fancy, int channel, const char *text) {
	auto *enc = new Encoder<RATE>();
	uint8_t payload[170] = {0};
	std::strncpy(reinterpret_cast<char *>(payload), text, sizeof(payload));
	enc->configure(payload, reinterpret_cast<const int8_t *>(call), carrier, noise, fancy);
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

template <int RATE>
static int decode(const Wav &wav, int channel) {
	auto *dec = new Decoder<RATE>();
	const int count = RATE / 50;
	const int channels = wav.channels;
	std::vector<int16_t> block(count * (channel ? 2 : 1));
	uint8_t payload[171] = {0};
	size_t frames = wav.samples.size() / channels;
	for (size_t start = 0; start + count <= frames; start += count) {
		for (int i = 0; i < count; ++i) {
			const int16_t *frame = wav.samples.data() + (start + i) * channels;
			if (channel) {
				block[2 * i] = frame[0];
				block[2 * i + 1] = channels > 1 ? frame[1] : frame[0];
			} else {
				block[i] = frame[0];
			}
		}
		if (!dec->feed(block.data(), count, channel))
			continue;
		int status = dec->process();
		float cfo;
		int32_t mode;
		uint8_t call[10] = {0};
		switch (status) {
		case STATUS_FAIL:
			std::cout << "FAIL at=" << start << std::endl;
			break;
		case STATUS_NOPE:
			dec->staged(&cfo, &mode, call);
			std::cout << "NOPE cfo=" << cfo << " mode=" << mode << " call=" << call << std::endl;
			break;
		case STATUS_PING:
			dec->staged(&cfo, &mode, call);
			std::cout << "PING cfo=" << cfo << " call=" << call << std::endl;
			break;
		case STATUS_SYNC:
			dec->staged(&cfo, &mode, call);
			std::cout << "SYNC at=" << start << " cfo=" << cfo << " mode=" << mode << " call=" << call << std::endl;
			break;
		case STATUS_DONE: {
			int flips = dec->fetch(payload);
			if (flips < 0)
				std::cout << "DONE failed" << std::endl;
			else
				std::cout << "DONE flips=" << flips << " text=" << reinterpret_cast<char *>(payload) << std::endl;
			break;
		}
		}
	}
	delete dec;
	return 0;
}

int main(int argc, char **argv) {
	if (argc >= 9 && !std::strcmp(argv[1], "encode")) {
		const char *out = argv[2];
		int rate = std::atoi(argv[3]);
		const char *call = argv[4];
		int carrier = std::atoi(argv[5]);
		int noise = std::atoi(argv[6]);
		bool fancy = std::atoi(argv[7]);
		int channel = std::atoi(argv[8]);
		std::string text = argc > 9 ? argv[9] : "";
		if (!text.empty() && text[0] == '@') {
			std::vector<uint8_t> data = read_file(text.c_str() + 1);
			text.assign(data.begin(), data.end());
		}
		switch (rate) {
		case 8000: return encode<8000>(out, call, carrier, noise, fancy, channel, text.c_str());
		case 16000: return encode<16000>(out, call, carrier, noise, fancy, channel, text.c_str());
		case 32000: return encode<32000>(out, call, carrier, noise, fancy, channel, text.c_str());
		case 44100: return encode<44100>(out, call, carrier, noise, fancy, channel, text.c_str());
		case 48000: return encode<48000>(out, call, carrier, noise, fancy, channel, text.c_str());
		}
		std::cerr << "unsupported rate " << rate << std::endl;
		return 1;
	}
	if (argc >= 3 && !std::strcmp(argv[1], "decode")) {
		Wav wav;
		if (!read_wav(argv[2], wav)) {
			std::cerr << "cannot read " << argv[2] << " (16-bit PCM or 32-bit float WAV)" << std::endl;
			return 1;
		}
		int channel = argc > 3 ? std::atoi(argv[3]) : (wav.channels > 1 ? 1 : 0);
		switch (wav.rate) {
		case 8000: return decode<8000>(wav, channel);
		case 16000: return decode<16000>(wav, channel);
		case 32000: return decode<32000>(wav, channel);
		case 44100: return decode<44100>(wav, channel);
		case 48000: return decode<48000>(wav, channel);
		}
		std::cerr << "unsupported rate " << wav.rate << std::endl;
		return 1;
	}
	std::cerr << "usage: " << argv[0] << " encode OUT.wav RATE CALLSIGN CARRIER NOISE FANCY CHANNEL [TEXT]" << std::endl;
	std::cerr << "       " << argv[0] << " decode IN.wav [CHANNEL]" << std::endl;
	return 1;
}
