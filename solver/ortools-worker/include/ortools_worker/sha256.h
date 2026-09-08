#pragma once

#include <array>
#include <cstdint>
#include <span>
#include <string_view>

namespace class_schedule::solver {

using Sha256Digest = std::array<std::uint8_t, 32>;

Sha256Digest Sha256(std::span<const std::uint8_t> input);
Sha256Digest Sha256(std::string_view input);

}  // namespace class_schedule::solver

