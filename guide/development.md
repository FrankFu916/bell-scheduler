# 开发与构建

以下命令从仓库根目录执行。完整的桌面求解构建使用 Apple Silicon、macOS 26.0 或更新版本；当前锁定的 OR-Tools 归档只适用于 macOS arm64。Rust 和前端检查也可在 Linux 上运行，其桌面编译依赖见 [CI 配置](../.github/workflows/correctness.yml)。

## 准备工具

- Rust **1.88.0**，包括 `rustfmt` 和 `clippy`；版本见 [rust-toolchain.toml](../rust-toolchain.toml)。
- Node.js **24.20.0** 和 npm；版本见 [frontend/.node-version](../frontend/.node-version)。使用 `npm ci` 安装锁定依赖。
- CMake **3.24 或更新版本**、支持 C++20 的编译器和 Make。
- macOS 构建需要 Xcode Command Line Tools，以及支持 macOS 26 的 SDK。构建脚本还使用 `otool`、`lipo`、`install_name_tool` 和 `codesign`。
- Python **3.9 或更新版本**，供构建和安装检查脚本使用；求解器本身是 C++ 可执行文件。
- `protoc`：Rust 的 Protobuf 构建需要它位于 `PATH`，或通过 `PROTOC` 指定。macOS 下可使用下文锁定归档自带的编译器；C++ worker 始终使用该归档中的 `protoc-33.1.0`。
- 下载和解包使用 `curl`、`tar`；归档校验使用 `shasum`。

安装 Rust 工具链并检查工具版本：

```sh
rustup toolchain install 1.88.0 --profile minimal --component rustfmt --component clippy
export PATH="${CARGO_HOME:-$HOME/.cargo}/bin:$PATH"
rustc +1.88.0 --version
cargo +1.88.0 --version
node --version
cmake --version
python3 --version
```

上面的 PATH 设置使 `cargo +1.88.0` 使用 rustup 启动器；同时安装了 Homebrew Rust 等工具链时，也应核对实际版本。如果只运行 Rust 和前端检查，可使用系统安装的 `protoc`，先确认 `protoc --version` 能成功执行。Linux 所需的 `protobuf-compiler` 和 Tauri 系统开发包列在 CI 的安装步骤中。

## 下载和构建 OR-Tools worker

依赖版本、下载地址和摘要以 [ortools.lock.json](../solver/ortools-worker/third_party/ortools.lock.json) 为准。当前锁定引擎为 **9.15.6755**，归档为：

```text
or-tools_arm64_macOS-26.2_cpp_v9.15.6755.tar.gz
SHA-256: de0400a45939a66ee13cd8360c230e830fc5e03a6ed5a8a8b60f58a39e4a67bc
```

下载到构建脚本要求的位置：

```sh
mkdir -p .cache/ortools
curl --fail --location \
  --output .cache/ortools/or-tools_arm64_macOS-26.2_cpp_v9.15.6755.tar.gz \
  https://github.com/google/or-tools/releases/download/v9.15/or-tools_arm64_macOS-26.2_cpp_v9.15.6755.tar.gz
make verify-ortools
```

校验通过后解压。CMake 不会自动下载或解压归档；它要求下面的目录已经存在：

```sh
tar -xzf .cache/ortools/or-tools_arm64_macOS-26.2_cpp_v9.15.6755.tar.gz \
  -C .cache/ortools
test -f .cache/ortools/or-tools_arm64_macOS-26.2_cpp_v9.15.6755/lib/cmake/ortools/ortoolsConfig.cmake
export PROTOC="$PWD/.cache/ortools/or-tools_arm64_macOS-26.2_cpp_v9.15.6755/bin/protoc-33.1.0"
"$PROTOC" --version
```

在同一终端中配置、编译并测试 worker：

```sh
make worker-configure
make worker-build
make worker-test
```

`worker-build` 会先执行配置和归档校验，`worker-test` 会先构建，再运行 CTest。生成的可执行文件位于：

```text
solver/ortools-worker/build/ortools-scheduler-worker
```

这个开发可执行文件通过 RPATH 加载 `.cache/ortools` 中的动态库。保留归档及其解压目录，直到完成需要 worker 的构建和测试。完整桌面构建会在复制品上设置应用内相对加载路径。

## Rust、前端和桌面应用

运行 Rust 检查和锁定的前端安装：

```sh
make check
make rust-gate
make frontend-install frontend-check
```

`rust-gate` 包括格式检查、Clippy 和 workspace 测试；Clippy 将警告视为错误。`frontend-check` 运行前端测试、TypeScript strict 类型检查和 Vite 构建。

浏览器开发服务器：

```sh
npm --prefix frontend run dev
```

地址为 `http://127.0.0.1:1420`。浏览器可以预览界面；访问本机项目数据库、运行求解器等命令需要 Tauri 桌面环境。

编译桌面程序：

```sh
make desktop-build
```

该命令编译前端和 Rust 桌面程序，不组装随应用携带的 worker。要运行包含 worker 和动态库的 macOS 开发应用，先完成归档下载、解压和前端安装，再执行：

```sh
make desktop-bundle
open "target/rust-1.88.0/debug/bundle/macos/排课助手 · Bell.app"
```

`desktop-bundle` 会构建并测试 worker、组装依赖与许可文件、构建启用 `managed-worker` 的应用，并执行安装树校验和隔离运行检查。默认产物为 debug `.app`。它使用开发用 ad-hoc 签名，不是经过 Developer ID 签名或公证的安装包。第三方许可与二进制分发限制见 [THIRD_PARTY_LICENSES.md](../THIRD_PARTY_LICENSES.md)。

## CLI 冒烟检查

先完成 worker 构建，然后编译 CLI。这里显式使用与 Makefile 一致的输出目录：

```sh
CARGO_TARGET_DIR=target/rust-1.88.0 cargo +1.88.0 build --locked -p class-schedule-cli
target/rust-1.88.0/debug/class-schedule-cli --help
```

用仓库内的 [小型合成样本](../fixtures/small/README.md) 排课。每次使用新的输出目录，命令不会覆盖已有结果：

```sh
bell_smoke_dir=$(mktemp -d target/bell-smoke.XXXXXX)
target/rust-1.88.0/debug/class-schedule-cli solve \
  --input-dir fixtures/small \
  --input-mode existing-sections \
  --worker solver/ortools-worker/build/ortools-scheduler-worker \
  --output-dir "$bell_smoke_dir" \
  --execution reproducible \
  --workers 1 \
  --time-limit-seconds 30 \
  --seed 1
```

检查输出目录内的 `summary.json`。可用课表要求 `result.publishable` 和 `result.hard_valid` 均为 `true`，且 `provenance.validation_result` 为 `passed`；通过独立校验后才会输出 `timetable.csv`。对于此 `solve` 命令，退出码 `0` 表示成功提供课表，`3` 表示没有可发布课表，`2` 表示命令或处理错误。达到时限、未知、已证明不可行和取消各有独立状态，不能只凭缺少 CSV 判断原因。

该 `solve` 命令从 CSV 直接求解和导出。SQLite 项目流程使用 `import`、`solve-project`、`list-runs` 和 `export-run`；采用及复制使用 `adopt-run`、`clone-scenario` 和 `show-scenario`。通过对应子命令的 `--help` 查看必需参数。

`scenario-timetable` 可读取已采用方案的七种课表视图。提供方案 ID、预期方案版本、预期课表版本和 `--view` 后，会列出可选择的对象；再添加返回的 `--entity-id` 即可读取该对象的课次和完整周格。支持分页，单页上限为 100。此命令使用只读数据库连接，不启动求解器，也不会迁移或改写数据库；旧数据库需先通过正常项目打开流程升级。

## 提交前检查与代码边界

完整 macOS 检查需要先准备上述工具和归档：

```sh
make check
make gate
```

可以先运行受改动影响的 crate 测试，再执行完整检查，例如：

```sh
cargo +1.88.0 test --locked -p class-schedule-application
cargo +1.88.0 test --locked -p class-schedule-persistence
make staging-test
```

- 业务实体和不变量属于 `crates/domain`；导入、求解、采用、复制等流程属于 `crates/application`。CLI 和 Tauri 只解析请求、调用用例并映射响应。
- React 负责交互状态和展示。不要让浏览器提交任意 assignment、数据库路径或 worker 路径，也不要在前端复制业务校验。
- 学生冲突按真实 enrollment 计算。Hard 约束不能放宽；所有 solver assignment 必须经过独立 Rust validator，品质由 Rust 重新计算。
- C++ OR-Tools worker 是 one-shot sidecar，通过[版本化 framed Protobuf](../crates/solver-contract/proto/scheduler/v1/solver.proto) 通信；stdout 只承载协议。修改协议时同时维护两端解码和错误状态测试。
- SQLite 变更通过新增 migration、外键和事务完成。源项目、方案、课表各自保有修订；修改操作必须检查预期修订，失败不得留下部分记录。
- 为修复和新行为添加覆盖真实边界的测试。使用合成数据，勿提交学校的真实学生名单、项目数据库、构建输出或密钥。依赖调整需同步对应锁文件和第三方声明。
