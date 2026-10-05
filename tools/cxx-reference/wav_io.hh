/*
Minimal WAV reading and writing for the reference harness.

Reads 16-bit PCM or 32-bit float WAV files (any channel count) and writes 16-bit PCM.
Only what the harness needs: no extensible-format quirks beyond skipping unknown chunks.
*/

#pragma once

#include <algorithm>
#include <cmath>
#include <cstdint>
#include <cstdio>
#include <cstring>
#include <string>
#include <vector>

struct Wav {
	int rate = 0;
	int channels = 0;
	// Interleaved samples scaled to the int16 range, as the Android apps deliver them.
	std::vector<int16_t> samples;
};

static uint32_t le32(const uint8_t *p) { return p[0] | p[1] << 8 | p[2] << 16 | uint32_t(p[3]) << 24; }
static uint16_t le16(const uint8_t *p) { return p[0] | p[1] << 8; }

static bool read_wav(const char *path, Wav &wav) {
	FILE *f = std::fopen(path, "rb");
	if (!f)
		return false;
	std::vector<uint8_t> data;
	uint8_t buf[65536];
	size_t n;
	while ((n = std::fread(buf, 1, sizeof(buf), f)) > 0)
		data.insert(data.end(), buf, buf + n);
	std::fclose(f);
	if (data.size() < 12 || std::memcmp(data.data(), "RIFF", 4) || std::memcmp(data.data() + 8, "WAVE", 4))
		return false;
	int format = 0, bits = 0;
	size_t pos = 12;
	while (pos + 8 <= data.size()) {
		const uint8_t *ck = data.data() + pos;
		uint32_t size = le32(ck + 4);
		const uint8_t *body = ck + 8;
		if (!std::memcmp(ck, "fmt ", 4)) {
			format = le16(body);
			wav.channels = le16(body + 2);
			wav.rate = le32(body + 4);
			bits = le16(body + 14);
			if (format == 0xFFFE && size >= 26)
				format = le16(body + 24);
		} else if (!std::memcmp(ck, "data", 4)) {
			size_t avail = std::min<size_t>(size, data.size() - pos - 8);
			if (format == 1 && bits == 16) {
				size_t count = avail / 2;
				wav.samples.resize(count);
				for (size_t i = 0; i < count; ++i)
					wav.samples[i] = int16_t(le16(body + 2 * i));
			} else if (format == 3 && bits == 32) {
				size_t count = avail / 4;
				wav.samples.resize(count);
				for (size_t i = 0; i < count; ++i) {
					uint32_t u = le32(body + 4 * i);
					float v;
					std::memcpy(&v, &u, 4);
					float s = std::nearbyint(v * 32767.f);
					wav.samples[i] = int16_t(s < -32768 ? -32768 : s > 32767 ? 32767 : s);
				}
			} else {
				return false;
			}
			return wav.rate > 0 && wav.channels > 0;
		}
		pos += 8 + size + (size & 1);
	}
	return false;
}

static void put32(std::vector<uint8_t> &v, uint32_t x) {
	for (int i = 0; i < 4; ++i)
		v.push_back(x >> (8 * i));
}

static void put16(std::vector<uint8_t> &v, uint16_t x) {
	v.push_back(x);
	v.push_back(x >> 8);
}

static bool write_wav(const char *path, int rate, int channels, const std::vector<int16_t> &samples) {
	std::vector<uint8_t> v;
	uint32_t bytes = samples.size() * 2;
	v.insert(v.end(), {'R', 'I', 'F', 'F'});
	put32(v, 36 + bytes);
	v.insert(v.end(), {'W', 'A', 'V', 'E', 'f', 'm', 't', ' '});
	put32(v, 16);
	put16(v, 1);
	put16(v, channels);
	put32(v, rate);
	put32(v, rate * channels * 2);
	put16(v, channels * 2);
	put16(v, 16);
	v.insert(v.end(), {'d', 'a', 't', 'a'});
	put32(v, bytes);
	for (int16_t s : samples)
		put16(v, uint16_t(s));
	FILE *f = std::fopen(path, "wb");
	if (!f)
		return false;
	bool ok = std::fwrite(v.data(), 1, v.size(), f) == v.size();
	return std::fclose(f) == 0 && ok;
}

static std::vector<uint8_t> read_file(const char *path) {
	std::vector<uint8_t> data;
	FILE *f = std::fopen(path, "rb");
	if (!f)
		return data;
	uint8_t buf[65536];
	size_t n;
	while ((n = std::fread(buf, 1, sizeof(buf), f)) > 0)
		data.insert(data.end(), buf, buf + n);
	std::fclose(f);
	return data;
}

static bool write_file(const std::string &path, const uint8_t *data, size_t len) {
	FILE *f = std::fopen(path.c_str(), "wb");
	if (!f)
		return false;
	bool ok = std::fwrite(data, 1, len, f) == len;
	return std::fclose(f) == 0 && ok;
}
