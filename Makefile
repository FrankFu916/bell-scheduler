.PHONY: toolchain check test lint fmt rust-gate frontend-install frontend-check desktop-build desktop-bundle staging-test verify-ortools worker-configure worker-build worker-test gate
.NOTPARALLEL:

RUST_TOOLCHAIN ?= 1.88.0
RUST_TARGET_DIR ?= $(CURDIR)/target/rust-$(RUST_TOOLCHAIN)
BUILD_JOBS ?= 1
ifneq ($(BUILD_JOBS),1)
ifneq ($(BUILD_JOBS),2)
$(error BUILD_JOBS must be 1 or 2)
endif
endif
BUILD_NICE ?= 10
LOW_PRIORITY = nice -n "$(BUILD_NICE)"
RUST_TOOLCHAIN_BIN := $(shell rustup which --toolchain $(RUST_TOOLCHAIN) cargo 2>/dev/null | sed 's,/cargo$$,,')
RUST_ENV = $(LOW_PRIORITY) env PATH="$(RUST_TOOLCHAIN_BIN):$(PATH)" RUSTUP_TOOLCHAIN="$(RUST_TOOLCHAIN)" CARGO_TARGET_DIR="$(RUST_TARGET_DIR)" CARGO_BUILD_JOBS="$(BUILD_JOBS)" RUST_TEST_THREADS="$(BUILD_JOBS)"

toolchain:
	@test -x "$(RUST_TOOLCHAIN_BIN)/cargo" || { echo "Rust toolchain $(RUST_TOOLCHAIN) is not installed" >&2; exit 1; }
	@$(RUST_ENV) rustc --version | grep -q '^rustc 1\.88\.0 '
	@$(RUST_ENV) cargo --version | grep -q '^cargo 1\.88\.0 '

check: toolchain
	$(RUST_ENV) cargo check --workspace --all-targets

test: toolchain
	$(RUST_ENV) cargo test --workspace --all-targets

lint: toolchain
	$(RUST_ENV) cargo clippy --workspace --all-targets --all-features -- -D warnings

fmt: toolchain
	$(RUST_ENV) cargo fmt --all -- --check

rust-gate: fmt lint test

frontend-install:
	$(LOW_PRIORITY) npm --prefix frontend ci

frontend-check:
	$(LOW_PRIORITY) npm --prefix frontend test
	$(LOW_PRIORITY) npm --prefix frontend run build

desktop-build: frontend-check toolchain
	$(RUST_ENV) cargo build -p class-schedule-desktop

desktop-bundle: toolchain
	$(LOW_PRIORITY) python3 scripts/build_macos_dev_app.py --jobs "$(BUILD_JOBS)"

staging-test:
	$(LOW_PRIORITY) python3 -m unittest discover -s scripts -p test_stage_macos_worker.py -v

verify-ortools:
	@actual=$$(shasum -a 256 .cache/ortools/or-tools_arm64_macOS-26.2_cpp_v9.15.6755.tar.gz | awk '{print $$1}'); \
	expected=de0400a45939a66ee13cd8360c230e830fc5e03a6ed5a8a8b60f58a39e4a67bc; \
	test "$$actual" = "$$expected" || { echo "OR-Tools archive checksum mismatch" >&2; exit 1; }

worker-configure: verify-ortools
	$(LOW_PRIORITY) cmake -S solver/ortools-worker -B solver/ortools-worker/build -DORTOOLS_WORKER_BUILD_TESTS=ON -DCMAKE_BUILD_TYPE=Release

worker-build: worker-configure
	$(LOW_PRIORITY) cmake --build solver/ortools-worker/build --parallel "$(BUILD_JOBS)"

worker-test: worker-build
	$(LOW_PRIORITY) ctest --test-dir solver/ortools-worker/build --parallel "$(BUILD_JOBS)" --output-on-failure

gate: rust-gate frontend-check worker-test staging-test
