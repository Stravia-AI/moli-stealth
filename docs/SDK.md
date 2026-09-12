# 预编译 Rust SDK

`moli-sdk` 是宿主使用的 Rust Interface；`moli-sdk-ffi` 是单独构建的静态实现。宿主不得同时依赖后者或把 Moli 源码 crate 加入自己的依赖图。内部 C ABI 不是独立公共 C API；`moli-sdk/abi.txt` 的身份同时在构建时和进入实现时校验。

## 当前分发状态

仓库初始 `moli-sdk/artifacts.json` 的 `targets` 为空，**不是已发布 SDK**。它没有虚构的 Release、占位 SHA-256 或源码回退。默认构建会报告缺少绑定；本地联调须使用下面的显式覆盖。只有从实际六平台归档生成绑定、六个平台真实消费通过，并在获得单独授权后上传同一批字节至固定 Release，才具备默认网络分发条件。

`.github/workflows/sdk.yml` 可手动触发，也响应经授权推送到 `sdk-validation/**` 的验证分支；不创建 Release、不推送分支、不发布 crate。分支触发使用 `sdk-validation-<实际实现SHA>` 作为计划标签，不表示该 Release 已存在。它先构建六套真实产物，再产生包含最终摘要的本地 SDK commit/Git bundle，然后在六个原生环境中使用该 revision 和预填充的校验缓存运行外部消费项目。预填缓存验收不能替代一次真正从最终 Release 首次下载的验收。运行证据以实际 Actions 结果、`runtime.json`、消费者日志和计时文件为准；配置存在本身不证明六平台已通过。

## 宿主依赖

最终绑定 revision 产生并可获取后，在独立 Cargo workspace 中引用它：

```toml
[dependencies]
moli-sdk = { git = "https://github.com/Stravia-AI/moli-stealth", rev = "这里替换为实际已绑定的40位SDK提交" }
```

上面的 revision 是填写说明，不是可用提交。必须使用完整真实 revision，不能使用分支、`latest` 或用实现提交替代尚未产生的最终绑定提交。Cargo 仍需检出 Git 仓库、编译轻量 SDK 并进行最终链接；本方案不消除这些成本，也不承诺固定加速倍数。正常和构建依赖都不编译 Moli 实现。debug/release 使用相同的优化归档，不各下载一份实现。

Windows 宿主自己的 `.cargo/config.toml` 必须包含对应 target 的静态 CRT：

```toml
[target.x86_64-pc-windows-msvc]
rustflags = ["-C", "target-feature=+crt-static"]

[target.aarch64-pc-windows-msvc]
rustflags = ["-C", "target-feature=+crt-static"]
```

依赖仓库的 Cargo 配置不会自动配置宿主。也可在宿主构建环境设置 `RUSTFLAGS="-C target-feature=+crt-static"`。SDK 构建脚本拒绝 Windows 未启用 `crt-static`，不会修改宿主其他依赖的 CRT。Windows 需要匹配架构的 MSVC C++ 工具和 Windows SDK；GNU/musl 宿主仍需目标 Rust 标准库和目标链接器。SDK 不提供 macOS 产物；既有 CLI 的 macOS workflow 保留不变。

实现构建固定 Rust 1.96.1；跨版本验收另用原生 Rust 1.98.1 宿主，以空 target 目录离线编译并实际运行同一实现。验收同时核对归档 manifest 中的生产端 release/host 和消费端 `rustc -vV`，不会把同版本消费误记为跨版本证明。Windows x64 本地实现已通过两个宿主的完整工作负载和 11 项消费测试；其他目标仍需各自的原生验收，不承诺任意 Rust 版本兼容。Git 依赖仓库中的 `rust-toolchain` 不会替外部宿主固定工具链。

公共异步调用、浏览器/页面、HTTP 流、Cookie 及关闭方式见 `moli-sdk/src/lib.rs` 的 Rust 文档和 `sdk-consumer/src/main.rs` 的实际消费代码。实现拥有运行时与所有者线程，宿主不创建 Moli runtime。丢弃 Future 请求取消；显式异步关闭等待回收，Drop 只提交非阻塞清理。仅 HTTP 调用不启动浏览器，但仍获取完整包；不承诺链接器一定裁掉全部 V8。

## 下载、缓存和离线

构建脚本按 Cargo 的 `TARGET`，而非构建机 `HOST`，选择 `artifacts.json` 中的固定 GitHub Release asset。下载通过验证证书的 HTTPS；归档 SHA-256 和 manifest SHA-256 均取自 SDK 内容里的绑定，不从同一个远程 Release 临时下载摘要来充当信任根。

缓存位置依次为：

1. `MOLI_SDK_CACHE_DIR`；
2. `$CARGO_HOME/moli-sdk`；
3. 用户主目录的 `.cargo/moli-sdk`。

每项路径是 `<cache>/<target>/<archive-sha256>/`，内含 `artifact.tar.gz`、校验解包后的 `package/` 和 OS 文件锁 `artifact.lock`。进程崩溃会释放 OS 锁；未完成的 `download.partial`、`unpack.partial/` 不会成为已接受包。下次取得同一项锁后才能处理遗留暂存。校验失败明确报错，不自动镜像发现、重试、换平台或源码构建。多个并发 Cargo 构建共享同一项时串行完成下载/解包；完整项验证后直接使用，不访问网络。

外部数据按不可信输入处理：拒绝路径穿越、绝对路径、Windows 前缀/分隔符逃逸、重复项、链接、设备和非普通文件；有下载和解包大小上限。除 manifest 自身外，整个包中的文件都必须在 manifest 清单中并验证摘要；额外文件也会被拒绝，避免原生搜索目录中的同名库遮蔽系统库。Cargo 监视完整包目录，因此新增文件也触发重新校验。默认 manifest 本身必须匹配 SDK 的固定摘要。

离线命令应同时明确通知 Cargo 和 SDK 构建脚本：

```sh
CARGO_NET_OFFLINE=true cargo build --offline
```

也可设置 `MOLI_SDK_OFFLINE=true` 只禁止 SDK 下载，同时允许 Cargo 获取宿主其他依赖。**Cargo 的裸 `--offline` 参数不会可靠地传递给任意 build script**；仅该参数或 Cargo 配置文件中的 offline 不能作为 SDK 网络开关。请显式设置上述环境变量。有效缓存永远不需要网络；离线缺失时报告准确缓存路径。正常 Cargo 包和 Git 源仍应事先由 Cargo 获取。

有绑定但不能联网时，可以预填真实归档：

```sh
python scripts/sdk-bind.py seed --binding moli-sdk/artifacts.json \
  --artifacts /path/to/downloaded-actions-or-release-assets \
  --target x86_64-unknown-linux-gnu --cache /path/to/sdk-cache
export MOLI_SDK_CACHE_DIR=/path/to/sdk-cache
CARGO_NET_OFFLINE=true cargo build --offline
```

`seed` 校验固定归档摘要、manifest 摘要和实现身份，不重写绑定。不要在正在消费的缓存项上手动覆盖文件。损坏缓存会明确失败；停止使用者后删除诊断中指出的**单个**缓存项，联网构建才重新下载。

## 未发布实现、本地覆盖和调试

本地覆盖目录必须是**解包后的包根目录**，包含 `manifest.json`、`lib/` 和清单中的全部 notices 文件，不是 `.tar.gz` 文件：

```sh
python scripts/sdk-package.py --target x86_64-unknown-linux-gnu \
  --local --output /tmp/sdk-local --native-notices /usr/share/doc
export MOLI_SDK_ARTIFACT_DIR=/tmp/sdk-local/work-x86_64-unknown-linux-gnu/package
cargo run --manifest-path /path/to/external-host/Cargo.toml
```

Windows 同样使用 `--target x86_64-pc-windows-msvc` 或 `aarch64-pc-windows-msvc`，在原生 MSVC Developer Shell 中执行。`--local` 允许未提交的真实实现，记录实际 HEAD、`dirty` 和 `local`；**不跳过**完整性、ABI、target 和 CRT 校验。`sdk-bind.py bind` 拒绝把本地产物或脏源码产物混入 Release 绑定。

深入源码调试使用 `--local --profile dev`；默认仍是 `--profile release`。Windows 调试实现固定单个 codegen unit，减少重复调试信息并避开 COFF 归档的 4 GiB 边界，仍保留完整调试信息和未优化代码。输出路径应为新的目录，脚本拒绝覆盖上一次构建工作区。测试先于提交的联调应使用本地模式，不必为满足打包检查而提交用户未完成的其他修改。

设置 `MOLI_SDK_ARTIFACT_DIR` 表示开发者明确信任该目录及其 manifest，允许它不是当前绑定 Release 的实现；manifest 自带摘要证明文件一致性，**不证明来源可信**。ABI 身份必须仍与 SDK 源码完全一致。ABI 改变时更新两侧并重新构建，不提供跳过 ABI 开关。manifest 至少记录 schema、target、CRT、ABI、实现 revision、构建 profile、逐文件 SHA-256、静态库顺序和系统库列表。

每个实现包有独立 `-symbols.tar.gz`，其绑定摘要与 `manifest.json` 中的实现包 SHA-256 将它关联到准确实现。宿主默认构建不下载 symbols。符号包保存未剥离的静态归档和本次实现依赖构建输出中的 Windows PDB，不扫描共享 target 中无关的 CLI、测试或示例 PDB；两类分发包均保留许可证清单。正常包移除可安全剥离的 debug sections，不弱化或忽略未定义符号。Windows 的 short import object 原样保留；带 `.voltbl` 或 `.chks64` 索引元数据的 MSVC 成员也原样保留，包括其自带调试记录，因为 `llvm-strip` 不会随节和符号重排更新这些元数据。正常包不包含配套 PDB。需要重链接调试时，把符号包中对应的未剥离归档作为显式本地包的库，并重新生成完整 manifest，或直接运行 `--local --profile dev`；不可把库换入正常缓存后绕过摘要校验。优化实现的局部变量和单步行为受优化影响，符号不等于无优化构建。

打包前将实现的 Rust 符号闭包私有化，覆盖运行时入口、mangled 符号、COFF 数据别名和异常 personality 弱引用；保留公共 C ABI 与系统 ABI。仅重命名 `rust_eh_personality` 不够：相同 Rust 版本的宿主可能因此同时抽取两个同名 std 对象。正常包与符号包从同一份私有化归档分离；符号包中的 `rust-private-symbols.json` 保存原名映射。COFF 保持符号索引、重定位和导入成员身份，ELF 更新 COMDAT 签名。不得用弱化符号或允许重复定义替代该隔离。

Windows x64 本地 debug 实现已通过真实 LLDB 消费调试：命中 `moli_sdk_ffi::moli_sdk_open` 的闭包源码断点、单步进入下一行，并继续完成全部工作负载，以退出码 0 结束。这验证了该未优化实现的源码调试路径，不代表其他目标或优化实现已经通过同样检查。

## 六个平台与运行数据

| target | 原生构建/运行契约 | 宿主条件 |
| --- | --- | --- |
| x86_64-pc-windows-msvc | Windows x64 runner | 静态 MSVC CRT、匹配 Windows SDK |
| aarch64-pc-windows-msvc | Windows ARM64 runner | 静态 MSVC CRT、原生 ARM64 编译器与宿主 |
| x86_64-unknown-linux-gnu | Debian 12 x64 容器 | glibc 2.36 基线、目标系统运行库 |
| aarch64-unknown-linux-gnu | Debian 12 ARM64 容器 | 同上，非模拟架构 |
| x86_64-unknown-linux-musl | Alpine 3.23 x64 容器 | musl，无 glibc 兼容层 |
| aarch64-unknown-linux-musl | Alpine 3.23 ARM64 容器 | 同上，非模拟架构 |

表格是验证配置，不是未经执行的兼容声明。`sdk-audit.py` 对真实消费可执行文件记录 NEEDED/imports 与 GNU symbol versions，拒绝非系统动态依赖和超过 Debian 12 的 GLIBC 版本。GNU 允许 libc、libm、pthread/dl 等 glibc 系统组件及 libgcc_s；musl 允许 musl loader/libc 及 libgcc_s。实际最终列表由每次验证的 `runtime.json` 给出。musl 不额外要求宿主 `+crt-static`，也不承诺所有业务宿主都是完全静态 ELF。

V8、带符号前缀的 BoringSSL、Rust 实现以及 Fontconfig/字体处理链的非系统原生库静态提供。`PKG_CONFIG_ALL_STATIC=1` 使锁定的 `yeslogic-fontconfig-sys 6.0.1` 走静态 pkg-config 元数据；禁止 `RUST_FONTCONFIG_DLOPEN`。打包器读取 rustc 的 `native-static-libs` 和 Cargo 原生搜索目录，打包未被 staticlib 纳入的非系统库（包括需要的 C++ runtime），缺少静态归档时失败。每个归档检查真实 ELF/COFF machine，Windows 同时检查动态 CRT 指令。

当前锁定 rusty_v8 是 `277195ed3ad13df707e01f1914999cca223bf908`（152.2.0）；Deno/serde_v8 锁定 `6e218631e42bf065869703d3b1cef10acbe5c769`，其适配层引用同一 V8 revision，避免两份 V8 带来的 Rust 类型与原生链接冲突。普通构建仍保留默认 `use_custom_libcxx`；仅 Linux SDK 生产显式选择 `RUSTY_V8_MOLI_LIBSTDCXX=1` 变体，并静态提供 stdc++、gcc_eh、gcc 和 atomic。ARM JIT 的指令缓存刷新需要 `libgcc.a` 中的 `__clear_cache`，不能只链接异常处理运行库。

btls 锁定 `c0aeb7fe5c1281611bde29868becdb7d4393ee6f`，修复 BoringSSL AR 长名称解析，避免含目录的 COFF 成员覆盖导致 ARM 前缀清单缺项。Go 最低要求由 BoringSSL 的 `go.mod`（1.24）决定，工作流使用 Go 1.27.0。Alpine 不复用参考 StraviaPlatform 的 Zig sysroot，也不复制其 stdexcept 符号弱化补丁；不得忽略符号或添加 glibc 兼容包。

`sdk-linux.sh` 将依赖 Release 地址固定到 `Stravia-AI/rusty_v8` 的 `v152.2.0-moli-sdk-libstdcxx.1`，不跟随 latest，也不把普通 libc++ 归档混入 SDK。每个目标使用 `librusty_v8_moli_libstdcxx_release_<target>.a.gz`、匹配的 `src_binding_moli_libstdcxx_release_<target>.rs` 和 `moli-v8-native-notices-<target>.tar.gz`。摘要作为脚本中的固定构建输入维护，不在构建时动态相信远程元数据。原生 notices 随 SDK 主包和符号包一同提供，包含 V8/第三方、Rust/编译器及 GCC 运行库许可文本。

锁定的 [V8 构建脚本](https://github.com/Stravia-AI/rusty_v8/blob/277195ed3ad13df707e01f1914999cca223bf908/build.rs) 校验 `RUSTY_V8_ARCHIVE_SHA256`；SDK 生产脚本还校验下载的 bindings 和 notices，再编译本地扩展桥接。完整 V8 子模块树用于原生生产；预编译消费即使检出了这棵树，也只使用公共头接口。内部 Wasm 桥接及生成头仅由显式源码构建模式启用，不由文件是否存在决定。源码或版本变化后必须同步审计并重新验证固定输入；V8 依赖的原生消费通过不等于完整 SDK 六平台验收通过。

### 字体、Fontconfig 和证书

静态库不包含字体包、字体配置或操作系统证书数据。部署在 Debian 12 的已知字体环境：

```sh
apt-get update
apt-get install --yes --no-install-recommends ca-certificates fontconfig fonts-dejavu-core fonts-noto-cjk
```

Alpine 3.23 的对应环境（启用官方 main/community 仓库）：

```sh
apk add --no-cache ca-certificates fontconfig font-dejavu font-noto-cjk libgcc
```

`fontconfig` 命令/配置包可能带系统动态 Fontconfig，但 SDK **不能依赖它的动态库**；最终依赖审计会拒绝这种情况。运行数据使用 `/etc/fonts/fonts.conf` 和 `/etc/fonts/conf.d/`，字体在运行环境安装。可显式设置：

```sh
export FONTCONFIG_FILE=/etc/fonts/fonts.conf
export FONTCONFIG_PATH=/etc/fonts
fc-cache -f
fc-match 'DejaVu Sans'
fc-match 'Noto Sans CJK SC'
```

不要把变量设置为 CI sysroot、构建机 home 或暂存路径。构建/消费在不同原生容器中进行，只有消费容器安装测试字体，证明发现的是部署环境数据。容器需要可读字体及配置；非 root 用户应有可写用户字体缓存目录或使用预生成的可读缓存。`fc-match` 只是诊断，不是渲染验收：消费测试还应检查中文/拉丁文本度量、布局、真实绘制和缺失字体回退。空白截图、API 返回 true 或没有异常不能单独证明字体正确。

Windows 使用已安装的系统字体；Windows 消费任务不额外安装字体或修改字体注册表。固定 DejaVu/Noto 的中文、拉丁及回退渲染断言在 GNU 和 Alpine 消费环境运行。不能据此声称任意 Windows 环境都完整覆盖所有语言。字体许可证不随 SDK 静态库消失，重分发字体需遵守字体自己的许可。

## 源码工作区验证

工作区包含 SDK 加载器，因此尚未发布的实现变更也需要有效静态产物，不能让 `cargo clippy --workspace` 或 `cargo nextest` 隐式绕过加载器。源码 CI 显式调用 `sdk-package.py --local --profile dev`，把生成目录设置为 `MOLI_SDK_ARTIFACT_DIR`，再运行完整工作区检查；同时固定 `CARGO_BUILD_TARGET` 以复用同一目标的编译结果。该步骤验证当前源码与当前内部 ABI，不是下游构建时自动回退源码。

Alpine 构建阶段通过 `sdk-host-rustc.py` 仅让 host 构建工具动态链接 musl，使 bindgen 可以加载系统 `libclang`。显式 `--target` 的实现编译不受该设置影响，仍按静态产物契约打包；这不是引入 glibc 或放宽目标运行依赖。

Windows 实现构建使用 Visual Studio 2022 工具链；x64 CI 固定 `windows-2022`，不继承已切换到 Visual Studio 2026 的滚动镜像，并安装经固定 SHA-256 校验的 NASM 2.16.03。ARM64 使用 Ninja 和 `clang-cl` 编译 BoringSSL 汇编；AWS-LC 保留其自带 ARM 汇编集成的 Visual Studio 2022 生成器。两者均链接静态 MSVC CRT。生产端 Cargo/libgit2 检出完整 V8 子模块需要 `core.longpaths=true`，工作流在一次性 Windows runner 上设置；这不是 SDK 宿主的安装要求。新工具链须重新完成符号隔离、链接和运行验收后再升级。

本地维护者采用前述本地覆盖流程，随后执行仓库要求的 `cargo fmt --all`、`cargo clippy --workspace --all-targets --all-features -- -D warnings` 和 `cargo nextest run --no-fail-fast`。独立 `sdk-consumer` 不属于源码工作区，须另外格式化并通过仓库外真实消费入口验证。

`rejections.py` 同时调用 `download_fixture.py`：临时复制相同 SDK 和私有 `build_loader.rs`，只替换临时构建入口的网络 transport，将固定 GitHub URL 映射到 loopback HTTP。消费者仍链接真实静态实现，同一加载器实际处理下载流、摘要、解包和并发缓存。未发布本地包的 fixture 仅为测试构造明确标记的 synthetic manifest，并记录原始 manifest；不据此声称通过 Release 来源或生产 TLS 验收。生产入口没有网络覆盖变量、镜像或关闭 TLS 的选项。

GNU/Alpine 消费任务还运行 `font-check.py`：在两个独立 Fontconfig 目录分别选择固定 Noto Sans CJK SC 2.004 和完整的 Noto Serif CJK SC 2.003。正例必须匹配从固定 Sans 字体离线提取的 SVG 字形；负例须通过原有宽度、缺字和回退检查，再由独立轮廓比较拒绝。判断依据包含实际退出状态和 `font-outline-comparison.json`，不是匹配 panic 文案。来源与许可见 `sdk-consumer/font-reference/`，测试字形不进入 SDK 实现包。

### 已执行的 Windows x64 构建计量

2026-09-11 在同一台 Ryzen 9 8940HX 主机、Rust 1.96.1、MSVC 14.44.35207 下运行 `benchmark.py`。源码和 SDK 均取 Git revision `9799762ac149c0cba1d2e912acc0fa4e912f8348`，各自使用空 target 目录，按源码、SDK 的顺序串行构建，共享已有依赖下载缓存。SDK 显式使用本地优化静态包；数字不包含真实 Release 首次下载，也不代表六平台验证完成。

| 阶段 | 源码集成 | SDK 集成 |
| --- | ---: | ---: |
| 首次 debug 构建 | 401.018 s | 36.048 s |
| 缓存 debug 构建 | 1.062 s | 0.312 s |
| 仅宿主代码修改 | 24.362 s | 2.976 s |
| 切换 release | 453.504 s | 27.298 s |
| 首次构建 compiler-artifact 事件 | 639 | 147 |

两种集成都实际运行了二进制 HTTP、重复 Cookie 写入与下一请求携带、真实布局导航、隔离世界 JavaScript、渲染 HTML 和关闭工作负载。仅宿主修改都只重建一个宿主编译单元。表中事件数不是 crate 数；原始证据由 `comparison.json`、两侧的 `measurement-environment.json`、`measurements.jsonl`、Cargo 日志及 timings HTML 记录。这是一组构建成本观测，不是固定加速承诺或运行性能基准。

## 从实现提交到最终绑定

1. 完成实现与锁文件验证并提交实现，记为 `I`。Release 打包只接受干净源码；本地联调则明确使用 `--local`。
2. 在六个原生环境运行 `sdk-package.py --target ...`。Linux 环境准备见 `sdk-linux.sh`，Windows 见 `sdk-windows.ps1`。产物名包含 `I` 与 target；包内保留 Rust/native notices 与构建身份。宿主 debug/release 共用优化实现，符号另包。
3. 汇集六套真实 archive 和 symbols，执行：

   ```sh
   python scripts/sdk-bind.py bind --artifacts /path/to/six-target-assets \
     --repository Stravia-AI/moli-stealth --release sdk-明确版本标签
   ```

   该命令重新计算真实文件摘要，验证包内完整清单、配对符号、共同 ABI/实现身份和六 target 完备性，然后写入 `moli-sdk/artifacts.json`。不接受缺失平台或假摘要。
4. 将绑定作为后续 SDK 提交 `S`。`I` 与 `S` 可不同，避免“归档摘要必须包含未来自身 commit”的循环。工作流产出一个本地 `sdk-bound` commit 和增量 Git bundle，未推送它；恢复 bundle 的仓库需已含 `I`。
5. 独立复制 `sdk-consumer` 到仓库外，通过 `verify.py --sdk-revision S --repository <精确Git仓库> --destination <新外部目录> --target ... --cache-dir ...` 运行。设置 `RUSTUP_TOOLCHAIN=1.96.1-<target>`，并预先安装该原生目标的 `1.98.1-<target>` Rust 工具链；脚本不会把跨版本检查降为 `cargo check`。完整入口要求优化的 release 实现，即使显式使用本地覆盖，也不能传入 `--profile dev` 包替代下载夹具所需的真实优化实现。它记录首轮、缓存、显式离线、仅宿主修改、profile 切换、依赖编译单元以及真实公共行为，并独立保存跨版本 target、编译器身份和运行证据。使用 `benchmark.py` 对同机器同工具链下源码集成和 SDK 集成进行匹配工作负载比较；不能以删减业务行为证明收益。
6. 六平台任一失败时不得宣告完整 SDK。Actions 缓存消费通过后，Release 上传和最终 Git revision 的推送仍需相应授权。真正发布须上传**相同字节与名称**，并完成默认首次网络下载、缓存/离线和消费验收；本 workflow 不执行这一步。

### 常见失败

- **unsupported target**：目前只支持表格中的六个 target；不会选宿主架构来凑数。
- **no bound artifact**：当前 revision 未绑定真实六平台产物。使用有效最终 revision，或显式本地覆盖。
- **Windows requires static MSVC CRT**：配置宿主，而非依赖仓库的 Cargo flags；不要链接 `/MD` 原生库。
- **offline cache miss**：检查 target、绑定摘要和缓存路径；预填已核对的 archive。Cargo 依赖缓存与 SDK 产物缓存是两件事。
- **download failed / SHA-256 mismatch**：检查固定 Release 与资产是否存在及网络权限；不要修改 TLS 校验或临时相信远程摘要。
- **manifest / ABI / target / CRT mismatch**：SDK 与实现不匹配或被错误拷贝；重建同 ABI/target 的实际包，不跳过校验。
- **non-system dependency has no static archive**：实现构建遗漏非系统静态链；修复构建环境，不让宿主安装其动态开发库来掩盖。
- **font fallback/Chinese rendering failure**：先检查运行环境字体和 `/etc/fonts`，再查看消费字体断言；系统启动成功不是完成渲染验收。

初始未绑定状态、尚未执行的 Actions、未授权的真实 Release 和未取得的网络分发证据必须在交付时明确报告，不能写成已经发布或六平台全部验证成功。
