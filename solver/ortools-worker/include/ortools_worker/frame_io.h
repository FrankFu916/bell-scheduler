#pragma once

#include <cstddef>
#include <iosfwd>
#include <string>

#include "google/protobuf/message_lite.h"

namespace class_schedule::solver {

inline constexpr std::size_t kMaximumFrameLength = 64U * 1024U * 1024U;

// Reads exactly one four-byte big-endian length-delimited protobuf payload.
// The caller separately checks that stdin reaches EOF after the frame.
bool ReadFrame(std::istream& input, google::protobuf::MessageLite* message,
               std::string* error);

// Writes exactly one four-byte big-endian length-delimited protobuf payload.
bool WriteFrame(std::ostream& output,
                const google::protobuf::MessageLite& message,
                std::string* error);

}  // namespace class_schedule::solver

