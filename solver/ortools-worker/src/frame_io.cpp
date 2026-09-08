#include "ortools_worker/frame_io.h"

#include <array>
#include <cstdint>
#include <istream>
#include <limits>
#include <ostream>
#include <string>

namespace class_schedule::solver {
namespace {

bool ReadFully(std::istream& input, char* destination, std::size_t length,
               std::size_t* actual) {
  *actual = 0;
  while (*actual < length) {
    const auto remaining = length - *actual;
    const auto chunk = static_cast<std::streamsize>(remaining);
    input.read(destination + *actual, chunk);
    const auto read = input.gcount();
    if (read > 0) {
      *actual += static_cast<std::size_t>(read);
    }
    if (*actual == length) {
      return true;
    }
    if (input.eof() || input.bad()) {
      return false;
    }
    if (input.fail()) {
      input.clear(input.rdstate() & ~std::ios::failbit);
    }
  }
  return true;
}

}  // namespace

bool ReadFrame(std::istream& input, google::protobuf::MessageLite* message,
               std::string* error) {
  if (message == nullptr || error == nullptr) {
    return false;
  }

  std::array<char, 4> header{};
  std::size_t header_bytes = 0;
  if (!ReadFully(input, header.data(), header.size(), &header_bytes)) {
    *error = "FRAME_TRUNCATED_HEADER:" + std::to_string(header_bytes);
    return false;
  }

  const auto byte = [&header](std::size_t index) {
    return static_cast<std::uint32_t>(
        static_cast<unsigned char>(header[index]));
  };
  const std::uint32_t declared = (byte(0) << 24U) | (byte(1) << 16U) |
                                 (byte(2) << 8U) | byte(3);
  if (declared > kMaximumFrameLength) {
    *error = "FRAME_TOO_LARGE:" + std::to_string(declared);
    return false;
  }

  std::string payload(static_cast<std::size_t>(declared), '\0');
  std::size_t payload_bytes = 0;
  if (!ReadFully(input, payload.data(), payload.size(), &payload_bytes)) {
    *error = "FRAME_TRUNCATED_PAYLOAD:" + std::to_string(payload_bytes) +
             "/" + std::to_string(declared);
    return false;
  }
  if (!message->ParseFromArray(payload.data(),
                               static_cast<int>(payload.size()))) {
    *error = "PROTOBUF_DECODE_FAILED";
    return false;
  }
  return true;
}

bool WriteFrame(std::ostream& output,
                const google::protobuf::MessageLite& message,
                std::string* error) {
  if (error == nullptr) {
    return false;
  }
  std::string payload;
  if (!message.SerializeToString(&payload)) {
    *error = "PROTOBUF_ENCODE_FAILED";
    return false;
  }
  if (payload.size() > kMaximumFrameLength ||
      payload.size() > std::numeric_limits<std::uint32_t>::max()) {
    *error = "RESPONSE_FRAME_TOO_LARGE:" + std::to_string(payload.size());
    return false;
  }

  const auto length = static_cast<std::uint32_t>(payload.size());
  const std::array<char, 4> header{
      static_cast<char>((length >> 24U) & 0xffU),
      static_cast<char>((length >> 16U) & 0xffU),
      static_cast<char>((length >> 8U) & 0xffU),
      static_cast<char>(length & 0xffU),
  };
  output.write(header.data(), static_cast<std::streamsize>(header.size()));
  output.write(payload.data(), static_cast<std::streamsize>(payload.size()));
  output.flush();
  if (!output.good()) {
    *error = "FRAME_WRITE_FAILED";
    return false;
  }
  return true;
}

}  // namespace class_schedule::solver

