#include <iostream>

#ifdef _WIN32
#include <fcntl.h>
#include <io.h>
#endif

#include "google/protobuf/stubs/common.h"
#include "ortools_worker/solver_worker.h"

int main() {
#ifdef _WIN32
  _setmode(_fileno(stdin), _O_BINARY);
  _setmode(_fileno(stdout), _O_BINARY);
#endif
  GOOGLE_PROTOBUF_VERIFY_VERSION;
  const int result = class_schedule::solver::RunOneShot(
      std::cin, std::cout, std::cerr);
  google::protobuf::ShutdownProtobufLibrary();
  return result;
}
