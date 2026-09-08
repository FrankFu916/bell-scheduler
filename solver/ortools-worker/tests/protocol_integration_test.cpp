#include <iostream>
#include <sstream>
#include <string>

#include "ortools_worker/frame_io.h"
#include "ortools_worker/solver_worker.h"
#include "test_fixture.h"

int main() {
  const auto request = class_schedule::solver::test::ValidRequest();
  std::stringstream input(std::ios::in | std::ios::out | std::ios::binary);
  std::string frame_error;
  if (!class_schedule::solver::WriteFrame(input, request, &frame_error)) {
    std::cerr << frame_error << '\n';
    return 1;
  }
  input.seekg(0);
  std::stringstream output(std::ios::in | std::ios::out | std::ios::binary);
  std::stringstream diagnostics;
  const int exit_code =
      class_schedule::solver::RunOneShot(input, output, diagnostics);
  if (exit_code != 0 || !diagnostics.str().empty()) {
    std::cerr << "one-shot failed: " << exit_code << ' ' << diagnostics.str()
              << '\n';
    return 1;
  }
  output.seekg(0);
  scheduler::v1::SolverEnvelope response;
  if (!class_schedule::solver::ReadFrame(output, &response, &frame_error)) {
    std::cerr << frame_error << '\n';
    return 1;
  }
  if (response.request_id() != request.request_id() ||
      !response.has_solve_response() ||
      response.solve_response().status() !=
          scheduler::v1::SOLVER_STATUS_OPTIMAL) {
    std::cerr << "unexpected response envelope\n";
    return 1;
  }
  if (output.peek() != std::char_traits<char>::eof()) {
    std::cerr << "stdout contained bytes after the single response frame\n";
    return 1;
  }
  return 0;
}
