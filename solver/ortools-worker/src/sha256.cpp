#include "ortools_worker/sha256.h"

#include <array>
#include <bit>
#include <cstddef>
#include <cstdint>
#include <span>
#include <string_view>
#include <vector>

namespace class_schedule::solver {
namespace {

constexpr std::array<std::uint32_t, 64> kRoundConstants{
    0x428a2f98U, 0x71374491U, 0xb5c0fbcfU, 0xe9b5dba5U, 0x3956c25bU,
    0x59f111f1U, 0x923f82a4U, 0xab1c5ed5U, 0xd807aa98U, 0x12835b01U,
    0x243185beU, 0x550c7dc3U, 0x72be5d74U, 0x80deb1feU, 0x9bdc06a7U,
    0xc19bf174U, 0xe49b69c1U, 0xefbe4786U, 0x0fc19dc6U, 0x240ca1ccU,
    0x2de92c6fU, 0x4a7484aaU, 0x5cb0a9dcU, 0x76f988daU, 0x983e5152U,
    0xa831c66dU, 0xb00327c8U, 0xbf597fc7U, 0xc6e00bf3U, 0xd5a79147U,
    0x06ca6351U, 0x14292967U, 0x27b70a85U, 0x2e1b2138U, 0x4d2c6dfcU,
    0x53380d13U, 0x650a7354U, 0x766a0abbU, 0x81c2c92eU, 0x92722c85U,
    0xa2bfe8a1U, 0xa81a664bU, 0xc24b8b70U, 0xc76c51a3U, 0xd192e819U,
    0xd6990624U, 0xf40e3585U, 0x106aa070U, 0x19a4c116U, 0x1e376c08U,
    0x2748774cU, 0x34b0bcb5U, 0x391c0cb3U, 0x4ed8aa4aU, 0x5b9cca4fU,
    0x682e6ff3U, 0x748f82eeU, 0x78a5636fU, 0x84c87814U, 0x8cc70208U,
    0x90befffaU, 0xa4506cebU, 0xbef9a3f7U, 0xc67178f2U,
};

std::uint32_t ReadBigEndianWord(const std::uint8_t* bytes) {
  return (static_cast<std::uint32_t>(bytes[0]) << 24U) |
         (static_cast<std::uint32_t>(bytes[1]) << 16U) |
         (static_cast<std::uint32_t>(bytes[2]) << 8U) |
         static_cast<std::uint32_t>(bytes[3]);
}

void WriteBigEndianWord(std::uint32_t value, std::uint8_t* output) {
  output[0] = static_cast<std::uint8_t>((value >> 24U) & 0xffU);
  output[1] = static_cast<std::uint8_t>((value >> 16U) & 0xffU);
  output[2] = static_cast<std::uint8_t>((value >> 8U) & 0xffU);
  output[3] = static_cast<std::uint8_t>(value & 0xffU);
}

}  // namespace

Sha256Digest Sha256(std::span<const std::uint8_t> input) {
  const std::uint64_t bit_length =
      static_cast<std::uint64_t>(input.size()) * 8ULL;
  std::vector<std::uint8_t> padded(input.begin(), input.end());
  padded.push_back(0x80U);
  while ((padded.size() % 64U) != 56U) {
    padded.push_back(0U);
  }
  for (int shift = 56; shift >= 0; shift -= 8) {
    padded.push_back(
        static_cast<std::uint8_t>((bit_length >> shift) & 0xffULL));
  }

  std::array<std::uint32_t, 8> hash{
      0x6a09e667U, 0xbb67ae85U, 0x3c6ef372U, 0xa54ff53aU,
      0x510e527fU, 0x9b05688cU, 0x1f83d9abU, 0x5be0cd19U,
  };

  std::array<std::uint32_t, 64> schedule{};
  for (std::size_t offset = 0; offset < padded.size(); offset += 64U) {
    for (std::size_t index = 0; index < 16U; ++index) {
      schedule[index] = ReadBigEndianWord(&padded[offset + index * 4U]);
    }
    for (std::size_t index = 16U; index < schedule.size(); ++index) {
      const auto s0 = std::rotr(schedule[index - 15U], 7) ^
                      std::rotr(schedule[index - 15U], 18) ^
                      (schedule[index - 15U] >> 3U);
      const auto s1 = std::rotr(schedule[index - 2U], 17) ^
                      std::rotr(schedule[index - 2U], 19) ^
                      (schedule[index - 2U] >> 10U);
      schedule[index] = schedule[index - 16U] + s0 +
                        schedule[index - 7U] + s1;
    }

    auto a = hash[0];
    auto b = hash[1];
    auto c = hash[2];
    auto d = hash[3];
    auto e = hash[4];
    auto f = hash[5];
    auto g = hash[6];
    auto h = hash[7];

    for (std::size_t round = 0; round < schedule.size(); ++round) {
      const auto sum1 = std::rotr(e, 6) ^ std::rotr(e, 11) ^ std::rotr(e, 25);
      const auto choose = (e & f) ^ ((~e) & g);
      const auto temporary1 =
          h + sum1 + choose + kRoundConstants[round] + schedule[round];
      const auto sum0 = std::rotr(a, 2) ^ std::rotr(a, 13) ^ std::rotr(a, 22);
      const auto majority = (a & b) ^ (a & c) ^ (b & c);
      const auto temporary2 = sum0 + majority;

      h = g;
      g = f;
      f = e;
      e = d + temporary1;
      d = c;
      c = b;
      b = a;
      a = temporary1 + temporary2;
    }

    hash[0] += a;
    hash[1] += b;
    hash[2] += c;
    hash[3] += d;
    hash[4] += e;
    hash[5] += f;
    hash[6] += g;
    hash[7] += h;
  }

  Sha256Digest digest{};
  for (std::size_t index = 0; index < hash.size(); ++index) {
    WriteBigEndianWord(hash[index], digest.data() + index * 4U);
  }
  return digest;
}

Sha256Digest Sha256(std::string_view input) {
  const auto* bytes = reinterpret_cast<const std::uint8_t*>(input.data());
  return Sha256(std::span<const std::uint8_t>(bytes, input.size()));
}

}  // namespace class_schedule::solver

