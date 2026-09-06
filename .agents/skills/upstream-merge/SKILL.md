---
name: upstream-merge
description: 将 lexmount/moli 上游的指定 Release、tag 或提交合入 moli-stealth。在同步上游、升级 Moli 基线、处理合并冲突或创建上游合并提交时使用；重点保留 fork 契约、消除语义冲突并验证真实行为。不用于 http2、ratchet、btls 等依赖 fork 的独立升级。
---

# 合并 Moli 上游

保留上游历史和本仓库仍然需要的行为差异，而不是保留每一份旧实现。
Git 自动合并成功只证明没有文本冲突，不证明能编译、行为一致或定制没有退化。

## 边界与入口

- 主仓库上游为 [lexmount/moli](https://github.com/lexmount/moli)，实际远端配置以本地为准。
  用户指定 Release/tag 时，只合入该版本指向的提交，不顺带同步最新 `main`。
- 传输依赖 fork 的升级使用 [upstream-dependency-maintenance](../upstream-dependency-maintenance/SKILL.md)。
  不借主仓库同步更换依赖来源、更新无关 Git SHA 或重新生成整份 lockfile。
- 读取适用的 agent 指令、[领域术语](../../../CONTEXT.md)、
  [传输替换 ADR](../../../docs/adr/0001-replace-curl-with-chrome-transport.md)、
  当前 Cargo manifests、`.cargo/config.toml` 和 `.config/nextest.toml`。
  浏览器身份、传输指纹和 Stealth 按领域文档区分；GPU/Canvas 等已有差异按当前源码核对，
  不借同步扩展伪装范围。
- “同步上游”不自动授权提交、推送或发布。未明确授权提交时，完成代码和验证，
  保留待提交合并；授权合并提交也不等于授权推送。此前一次提交授权不是后续任务的永久授权。

下列 Git/Cargo 命令从仓库根目录运行；尖括号参数须替换为本次确认的值。

## 1. 确认起点与目标

先检查工作区、当前分支、远端和最近历史：

```bash
git status --short --branch
git remote -v
git log -6 --oneline
```

记录合并前 HEAD 的完整 SHA、工作区/暂存区状态及已有合并或 rebase 状态。
存在用户改动时先判断能否安全隔离；不自动 stash、不用 `reset --hard` 或整文件检出覆盖，
也不接管归属不明的未完成合并。无法安全继续时说明具体冲突范围再询问。

读取指定 Release 说明，核对远端确实指向用户指定仓库，再拉取目标：

```bash
git fetch --no-tags upstream tag <tag>
git rev-parse "refs/tags/<tag>^{commit}"
git merge-base HEAD "<target-sha>"
git merge-base --is-ancestor "<target-sha>" HEAD
```

- `^{commit}` 将 annotated tag 解析为实际提交；记录完整 target SHA 和共同基点。
- tag 同名但指向不同时，不强制覆盖本地 tag；先核对双方来源。
- `--is-ancestor` 返回 0 表示目标已包含在当前历史中，不再制造合并，也不倒退工作区。
  返回 1 表示未包含；其他错误必须处理，不能当作“需要合并”。
- 用户指定分支或 SHA 时按该来源 fetch，再解析并固定为完整提交；不在后续步骤继续追浮动分支。
  不把历史案例里的版本号或 SHA 当作未来的目标。

## 2. 分清双边差异

在合并前分别列出上游新增提交、上游改动和 fork 改动：

```bash
git log --oneline "HEAD..<target-sha>"
git diff --stat "HEAD...<target-sha>"
git diff --name-only "HEAD...<target-sha>"
git diff --name-only "<target-sha>...HEAD"
```

三点 diff 从共同基点比较；不要用两个分支尖端的普通 diff 代替双边改动分析。
记录三类路径：仅上游修改、仅 fork 修改、双方修改。重点读取交集，但不能只看交集：
上游新增模块可能与未修改的 fork 模块实现同一接口，产生重复导入、重复安装或行为覆盖。

按实际改动检查以下边界，而非固定遍历整个仓库：

| 边界 | 重点检查 |
| --- | --- |
| Cargo / 原生构建 | 精确版本、`[patch.crates-io]`、依赖 fork 的 `rev`、V8 vendor、MSVC CRT、平台配置是否保留；版本号与 lockfile 是否一致。 |
| 网络 / CLI / CDP | stealth transport 消费接口、raw body 无损输出、流式与终态、网络记录共享/回收、会话游标和取消语义是否匹配。 |
| Renderer / 浏览器身份 | 同名实现、安装顺序、窗口/子 frame/worker 的身份来源、V8 定制及现有平台测试补丁是否仍有效。 |
| 测试 / 文档 | 上游新增测试是否假定未定制的默认值；原有契约测试和使用说明是否与合并后实现一致。 |

读集较大且边界独立时，可并行核对网络/CLI 与 renderer/身份；明确唯一集成写入者，
子代理跳过格式化、构建和测试。其结论须用实际 diff、定义和调用点核对，尤其不能仅凭报告
判断某份实现来自上游还是 fork。

## 3. 合入，并消除语义冲突

```bash
git merge --no-commit --no-ff "<target-sha>"
```

`--no-ff` 防止快进绕过 `--no-commit`，保持提交权限边界。默认使用保留双边历史的合并，
不擅自改为 squash、rebase 或挑选部分提交。

出现文本冲突时使用可用的 `resolving-merge-conflicts` skill。无论 Git 是否报告冲突，都要：

1. 按双方意图适配，不整片选择 ours/theirs，也不以删除仍需保留的行为换取通过。
2. 修改或删除导出符号前查询 LSP references；可用时通过 LSP 核对定义和调用点。
   迁移全部消费者，删除已被取代的实现、导入、调用与注释，不留别名或双路径。
3. 上游提供更完整的实现且覆盖现有需求时，统一使用该实现并验证差异；
   不为“保留定制”同时安装两个版本，不靠重命名重复符号掩盖同一职责的重复实现。
4. 新测试与 fork 不一致时，先用实际失败和源码区分回归与上游默认值假设。
   保留用户可见契约；用跨环境一致性、边界或状态转换替换无效的默认值断言，
   不把空字符串机械改成当前显卡型号，也不弱化为“没有抛错”。
5. 手工修正可能合法触及仅 fork 修改的文件，但必须有具体合并原因。
   不能把“交集外文件绝不可改”当作规则，也不能扩大为无关重构。

### 已验证的 v1.1.3 案例

这些是排查线索，不是每次必须套用的补丁：

- fork 旧 `context_bootstrap/window_runtime.rs` 与上游新增 `chrome_runtime.rs`
  都定义 `install_chrome_runtime_state`。自动合并在 `runtime_state.rs` 留下双导入和双调用，
  Clippy 实际报 `E0252`。最终删除旧实现，保留上游实现，在 performance seed 安装之后调用一次；
  上游 Chrome 表面、计时和异步回调测试通过。
- 上游 `worker_offscreen_canvas_exposes_webgl_identity_consistently` 断言 vendor/renderer 为空，
  但 fork 的 WebGL 实现已有非空身份。实际运行验证了该断言冲突；最终从真实窗口 VM 读取身份，
  与 worker 的实际输出比较，同时保留接口暴露和实例品牌断言。生产 GPU 行为未改动。

## 4. 分层验证真实行为

先运行受影响的最小测试集合；已存在的失败记录作为红灯，修正后运行同一入口确认消失。
沿用上游和仓库现有测试，不为合并另造大套测试或固定源码文本的断言。

全部修正整合后，如果涉及 Rust 源码或 Rust 构建元数据，按仓库要求运行：

```bash
cargo fmt --all
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo nextest run --no-fail-fast
cargo build --locked -p moli
```

- 共享同一 `target` 的 Cargo 检查由集成者串行执行；并发启动通常只会争抢构建锁和资源。
  长构建不等于挂起，不用缩短超时、反复启动或盲目重试掩盖问题。
- 前一命令失败导致后续命令未执行时，明确记录未执行项；修正后补齐，不能只报局部通过。
- 验证依赖当前代码、lockfile、配置和平台。源码变化后重跑受影响检查；
  用户随后只授权提交、且已审阅内容未变时，可以沿用本次成功结果，不必无依据重复长构建。
- 报告实际通过数、失败和跳过项；遵守当前 nextest 平台配置，不为全绿取消测试。
  Windows 通过不能推断 Linux/macOS/ARM 已通过。纯文档变更无需 Rust 检查。

编译和单测不能代替真实入口。运行本次构建的二进制检查版本，再按改动选择本地 CLI/CDP 场景。
不要运行旧 release 二进制并把结果归给新代码；执行 smoke 前用 `MOLI_BIN` 指定准确产物路径，
Windows 通常为 `target/debug/moli.exe`，其他平台通常为 `target/debug/moli`。

CLI 样例应验证消费者可观察的结果，例如 raw JSON 解码后与原始字节完全一致、
`URLSearchParams` 子类可正常使用、表单克隆保留编辑状态，而不是仅断言退出成功。
CDP 组选取以[现有冒烟说明](../../../moli-cdp-smoke/README.md#running)为准。
例如在 `moli-cdp-smoke` 目录运行：

```bash
uv run moli-cdp-smoke --group media-error --group network-body-cache
```

只访问受控本地 fixture；不要用生产网站是否放行代替兼容性证据。临时服务需确认就绪，
结束后停止，删除自己创建的样例和脚本，保留必要的日志与 smoke 证据路径。

## 5. 审阅、提交与交付

在整合边界审阅最终 diff，避免每个小编辑都重复 Git 审计：

- 仅上游修改且无需适配的文件应与目标提交一致；无关 fork 定制应保持不变。
- 解释每个额外适配文件，确认没有漏合入、冲突标记、无关格式化或临时依赖路径。
- 核对 manifests、lockfile、测试和相关现有文档；只暂存明确属于本次合并的修正，
  不用无范围的 `git add .` 收入用户文件。
- 使用 `git diff --cached --check`、`git diff --check` 和状态检查确认提交内容。
  Rust 提交前必须已有上述三项仓库检查的成功结果；无法通过则报告阻塞，不声称完成。

未获提交授权时，明确交付为“上游代码已合入工作区、验证完成、合并待提交”，
说明暂存/未暂存状态，不声称上游已进入 HEAD 历史。获准提交时先核对待提交内容未变、
`MERGE_HEAD` 为目标 SHA，再创建中文合并提交：

```bash
git rev-parse HEAD MERGE_HEAD
git commit -m "合并上游 <tag> 并保留隐身定制"
git log -1 --format="%h%n%P%n%s"
git status --short --branch
```

确认提交双亲分别为合并前 HEAD 与目标提交，工作区状态符合预期；未另获授权不推送。
最终报告目标 tag/SHA、必要的适配文件与取舍、实际验证结果、未验证范围、证据路径，
以及准确的合并/提交/推送状态。不在 skill 中维护每次同步的版本账本或测试通过数。
