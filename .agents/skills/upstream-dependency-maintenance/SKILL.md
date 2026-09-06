---
name: upstream-dependency-maintenance
description: 维护 Moli 的 Stravia-AI 上游依赖 forks（http2、ratchet、btls）。在检查上游稳定发布、升级传输依赖、迁移定制补丁、更新 Cargo Git SHA 或验证原生符号隔离时使用；不用于无关依赖的批量升级。
---

# 上游传输依赖维护

在完整的 Stravia-AI fork 中维护最小功能差异，Moli 只通过 Cargo manifests
和 `Cargo.lock` 锁定已发布的精确 Git SHA。补丁源码、历史和发布基线归 fork
所有；不在主仓库重新保存补丁副本、平行版本清单或专用同步脚本。

## 维护边界

| 依赖 | 上游 / fork | 主仓库更新位置 | 必须保留的行为 |
| --- | --- | --- | --- |
| http2 | [0x676e67/http2](https://github.com/0x676e67/http2) / [Stravia-AI/http2](https://github.com/Stravia-AI/http2) | 根 `Cargo.toml` 的 `[patch.crates-io].http2`；`moli-stealth-net/Cargo.toml` 的精确版本约束 | 同一复用连接上，每个请求的 HEADERS priority 覆盖连接默认值。 |
| ratchet | [graphform/ratchet](https://github.com/graphform/ratchet) / [Stravia-AI/ratchet](https://github.com/Stravia-AI/ratchet) | 根 `[patch.crates-io]` 的 `ratchet_deflate`、`ratchet_ext`；`moli-websocket/Cargo.toml` 的 `ratchet_rs` 精确版本约束 | 解压后大小上限、逐消息压缩状态、末片段 flush；发送后的 bufferedAmount 消费不等待对端回包。 |
| btls | [0x676e67/btls](https://github.com/0x676e67/btls) / [Stravia-AI/btls](https://github.com/Stravia-AI/btls) | `moli-stealth-net/Cargo.toml` 的 `boring2`、`boring-sys2`、`tokio-boring2` | BoringSSL C/C++ 符号隔离、Chrome signature GREASE、trust-anchor APIs、ML-DSA signature algorithms。 |

- `ratchet_deflate` 与 `ratchet_ext` 必须来自同一 fork SHA，避免 extension trait
  身份分裂。`ratchet_rs` 保持 registry 来源，并验证所选版本与这两个 crate 兼容。
- `boring2`、`boring-sys2`、`tokio-boring2` 分别是 `btls`、`btls-sys`、
  `tokio-btls` 的依赖别名。三个包必须使用同一 fork SHA，保留 `prefix-symbols`。
- bufferedAmount 是 Moli 的 WebSocket 集成契约，不要把主仓库的连接调度代码搬入
  Ratchet。其他依赖、V8 vendor、浏览器身份策略和协议语义不属于此次升级范围。
- 不用浮动分支替代 `rev`，不把第三方 workspace 裁剪后塞入 Moli，不静默关闭
  压缩、符号隔离或 TLS 能力以绕过构建和测试。

## 来源线索

下表仅记录首次迁移的上游基线，不表示当前最新版本或当前 fork SHA。
每次操作先读取 Cargo manifests 和 lockfile，再从锁定的 fork 提交追溯基线。

| 依赖 | 首次发布基线 | 上游源码 SHA |
| --- | --- | --- |
| http2 | GitHub `v0.5.20` | `5a9a1fe28154461318310e6044959c6dc8a60d17` |
| ratchet | crates.io `ratchet_deflate 1.2.1` | `ef05a54eeec533f8fdf308053f65e5a1f5bd34ff` |
| btls | GitHub `v0.5.6` | `4edbf5d716ba014384569ac5c631cea83827abfc` |

btls 的该基线固定 BoringSSL 子模块
`91a66a59b6c1435120ff83e245d7719411294386`。ML-DSA 的 libssl 支持来自
BoringSSL 上游提交 `4a3cda40b965bbda7cebf86e35c1ed6890ebcc34` 的三个生产文件；
不能仅凭 EVP 已支持 ML-DSA 就删除这部分补丁。

## 执行流程

### 1. 确认范围与权限

读取相关 manifests、`Cargo.lock`、fork 提交历史，以及
[传输验证说明](../../../moli-cdp-smoke/README.md#transport-fingerprint-evidence)。
记录操作开始时这些文件的内容，保留用户尚未提交的改动。

检查上游、拉取源码和本地验证不需要推送权限。提交、推送 fork、创建或合并 PR、
推送主仓库都必须有用户当前请求的明确授权；“维护依赖”本身不是发布授权。
没有发布授权时完成本地候选与验证，报告待发布的准确提交，不将未发布 SHA
写入主仓库最终依赖。

### 2. 确认真正的稳定发布

http2 和 btls 查询各自上游的 GitHub Release，不使用 fork 的 latest 或上游 HEAD：

```bash
gh api repos/0x676e67/http2/releases/latest
gh api repos/0x676e67/btls/releases/latest
```

排除 draft/prerelease，解析 release tag 指向的实际 commit，核对源码中的包名、
版本及 workspace 继承。不能用提交日期或看起来更新的开发分支替代稳定发布。

Ratchet 以 [crates.io index](https://index.crates.io/ra/tc/ratchet_deflate)
中的最高非 yanked 稳定 SemVer 为准；历史 GitHub Release 曾停留在 1.2.0，
但 crates.io 已发布 1.2.1。不要把这个历史版本硬编码成未来的“最新版本”。

1. 从 index 获取版本和 `cksum`，下载
   `https://static.crates.io/crates/ratchet_deflate/ratchet_deflate-<version>.crate`。
2. 校验下载字节的 SHA-256 与 `cksum` 一致；只读取归档中该版本的
   `.cargo_vcs_info.json`，取得 `git.sha1` 和 `path_in_vcs`。
3. 若 checksum 不符、VCS 信息缺失或 `git.dirty` 为真，停止自动迁移并报告来源缺口，
   不猜测 tag 或采用仓库 HEAD。
4. 在完整 Git checkout 中验证该 SHA、包路径和版本；分别确认 `ratchet_rs`、
   `ratchet_deflate`、`ratchet_ext` 的已发布版本及依赖要求，不假定它们永远同版本。

### 3. 在独立 fork 中迁移最小差异

在 Moli 仓库外新建 checkout，保留上游完整历史、workspace、许可证和子模块。
下列尖括号参数须替换为本次确认的值；不要覆盖已有目录。

```bash
git clone -c core.autocrlf=false -c core.longpaths=true https://github.com/Stravia-AI/<dependency>.git <fork-dir>
git -C <fork-dir> remote add upstream https://github.com/<upstream-owner>/<dependency>.git
git -C <fork-dir> fetch upstream --tags
git -C <fork-dir> fetch origin <current-pinned-sha>
git -C <fork-dir> diff <old-upstream-sha> <current-pinned-sha>
git -C <fork-dir> switch -c moli/<release>-patch.<revision> <new-upstream-sha>
```

从旧基线到已锁定 fork 的差异中逐项确认仍需维护的行为，再选择性移植对应提交。
上游已合入的修复应删除；冲突须按新上游实现重新适配，不能整片覆盖旧文件、
直接合并开发主线、只让补丁勉强应用，或静默跳过仍需保留的行为。

存在 `.gitmodules` 时运行 `git submodule update --init --recursive`，检查实际
gitlink 与发布基线一致。原生构建补丁使用 LF，包括 Windows checkout。
新增补丁提交的说明记录 `Upstream-Release`、完整 `Upstream-Base` SHA、每项差异的
必要性、backport 来源及验证结果，使下一次维护能只靠 fork 历史恢复来源。
不要改写已经发布的补丁分支；新版本或补丁修订使用新分支。

### 4. 先验证候选，再发布与固定

先执行受影响 fork 自带的相关检查，再做 Moli 集成验证。未发布候选可通过仓库外
临时 Cargo 配置使用 `path` patch：http2 / Ratchet 覆写 `[patch.crates-io]`，
btls 覆写 `[patch."https://github.com/Stravia-AI/btls.git"]` 的三个实际包名。
Ratchet 的两个包或 btls 的三个包必须一起替换，且路径指向完整 fork 的成员目录。

版本升级时同步调整准确的版本约束，不改为通配符。将临时配置通过 Cargo
`--config <file>` 传入；它及临时路径产生的 lockfile 状态都不能进入最终改动。
任何解析失败都恢复本次临时改动到操作前状态，不用 `git reset --hard` 或
`git checkout --` 覆盖用户工作。

```bash
cargo nextest run --config <temporary-cargo-config> -p moli-stealth-net -p moli-websocket --no-fail-fast
```

获准发布后，审阅并提交 fork，推送新分支到 `Stravia-AI`，确认远程完整 commit SHA
和本地提交一致。随后按维护边界表更新主仓库所有相关 `rev`、精确版本和 `Cargo.lock`。
不要执行无范围的 `cargo update`。移除临时 path 覆写后解析依赖并检查：

```bash
cargo metadata --format-version 1
cargo metadata --locked --format-version 1
cargo build --locked -p moli
cargo nextest run --locked -p moli-stealth-net -p moli-websocket --no-fail-fast
```

从 metadata 的 `packages` 核对每个被替换包只有一个版本和来源，且 `source` 为
预期 fork URL 与完整 SHA，不是 path、registry 或另一份 Git 来源；再审阅 lockfile，
排除无关依赖升级。最终构建和测试必须使用发布后的 Git 源，不能用本地覆写结果代替。

### 5. 验证行为和原生边界

- http2：复用连接中不同请求仍保留各自的 HEADERS priority。
- WebSocket：压缩消息后跟未压缩分片、末片段 flush、解压后大小上限，以及对端
  不回包时发送完成后 bufferedAmount 仍归零。
- btls：构建需要 Git、CMake、C/C++ 工具链、libclang 和 Go；版本跟随项目 CI。
  Windows 从匹配架构的 MSVC 开发环境运行，Unix 在 fork 目录运行原生 smoke，
  避免意外继承 Moli 的 Cargo 配置。
- BoringSSL 先构建并枚举 exports，再以 `BORINGSSL_PREFIX=btls_sys` 重建；
  只能链接第二轮产物。检查 C exports 全部有前缀、C++ `bssl` namespace 和
  公开 opaque 类型均隔离；实际链接完整 Moli，捕获与 AWS-LC/OpenSSL 的碰撞。
  `prefix-symbols` 下不能接受无法证明命名空间的 `BORING_BSSL_PATH` 预编译替换。
- 用真实原生 API 检查 SHA-256、GREASE、trust-anchor 和
  `mldsa44:mldsa65:mldsa87:rsa_pss_rsae_sha256` 配置；仅 Rust 编译成功不足以证明能力。
- 按传输验证说明运行真实 Moli 与固定 Chrome baseline 的 direct 和 CONNECT
  对比，每种三次、各用独立进程，保留差异和原始证据。只允许本地 fixture 使用
  跳过证书验证的参数，不改生产 TLS 校验。

全部候选整合完成后，在主仓库执行一次格式、clippy 和全量测试：

```bash
cargo fmt --all
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo nextest run --no-fail-fast
```

失败须区分实际错误和本次改动的关系，不能用重跑成功覆盖原始失败记录。
明确报告验证过的平台；Windows/Linux 成功不能推断 macOS/ARM 已通过。

## 交付与收尾

报告旧/新发布基线、fork 完整 SHA、保留或删除的补丁、变更文件、实际验证结果、
未验证平台及阻塞项。行为或构建要求改变时更新现有传输文档，不另建版本账本。
验证完成后移除自己创建的 checkout、临时 Cargo 配置和探针脚本，保留必要证据；
未获准发布的候选留存并报告路径，不清掉待审阅成果。主仓库不提交、不推送，
除非用户已单独授权。
