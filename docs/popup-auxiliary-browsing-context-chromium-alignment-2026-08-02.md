# Popup / Auxiliary Browsing Context：现状、Chromium 对照与统一方案

日期：2026-08-02

状态：架构评估与分阶段迁移设计；`popup-refactor` 已完成 Phase 1 primitive 抽取、
Phase 2A script-agent identity / current-policy baseline，以及 Phase 2B 的 selective
shared-agent ownership、V8 foreground routing、核心可行性矩阵和两次 release 长序列
内存验收。Phase 3 第一纵切已经把 renderer-owned auxiliary browsing-context/Page
reservation 和 selective related-agent admission 接入 production initial `about:blank`
target build；第二纵切的首个基础提交又建立了 live Page replacement reservation 和
prepared-document environment reuse，第二个提交已经把 prepared replacement commit、原
stable Page slot 内的 `PageVm`/view publication 和同一个 core `Page` 的 adoption 边界接通；
第三个提交已经让 protocol `Page.navigate` / target navigation 在已有 Page 上使用这份
replacement path，并覆盖 active、background、inactive target 及 Fetch response-stage；
`noopener` 仍显式使用 fresh agent。第四个提交又把 opener 同步拿到的同一 V8
WindowProxy 交给 related auxiliary Page 的首个 realm，并保留 inherited `about:blank`
的 creator security token。第五个基础提交把 main default realm bootstrap 拆成 callback
内可用的 in-scope prebootstrap 与 callback 后 Inspector materialization，并验证两段之间
Window/Document identity 不变。第六个基础提交又把 related Page admission 从 isolate
holder 内部路由改成 live source Page membership capability，并允许在已经进入的 opener
scope 内重建 target realm 的独立 native bridge bindings、复用缓存的 Inspector backend
handle，全程不回借 holder。第三纵切 D 已经把这些基础接到 production：对保留 opener、
非命名 target 的 initial `about:blank`，`window.open()` 在同步 callback 内创建并暂存真实
auxiliary `PageVm`、独立 realm 和唯一 Document，protocol target 随后采纳同一份 residence，
不再创建或 replay 第二份 initial Document。该窄路径同时继承 creator origin、referrer、
base URL、policy/storage authority，并保留 Classic WebDriver 所需的不可伪造 target identity，
而不恢复 opener 侧 Document owner。Phase 4 第一纵切又把相同的真实 initial Page 扩展到保留
opener、非命名、非 `javascript:` 的 non-empty URL：destination 在 target admission 后只由
auxiliary Page owner 导航一次，opener host 不再启动 mirrored loader。Phase 4 第二纵切又把
destination 提升为携带 exact `TargetPageResidenceIdentity` 的 typed claim，并在 target-local
slot 中建立 `Held → Published → Consumed` authority；旧 admission 因 Page generation 变化而失效
后，不能导航 replacement Page，也不能被 `Page.enable` 等入口从 target URL 重建。Phase 3-5
仍未整体完成。Phase 4 第三纵切已经把 HTTP 204/205 收敛为不提交 Document 的独立 terminal，
并覆盖 initial realm/history 保留和后续 redirect replacement。第四纵切又把 replacement
Document 的 commit/attachment publication 与其精确 DCL continuation 分开：protocol 可以在
parser 仍阻塞时控制已经提交的 realm，renderer owner 则用独立 typed terminal 交付同一 turn 的
output fence 与最终 PageState，避免 popup、普通 navigation、Classic/BiDi 和 direct child lane
各自猜测异步完成。第五纵切进一步补齐 Fetch response-stage 的 effective-response terminal：
`fulfillRequest` / `continueResponse` 覆盖后的 204/205 与原始 204 都不提交 Document，而原始 204
被 fulfill 为 200 时仍可正常提交；buffered synthetic body 不再绕过公共 no-commit/download
分类器。第六纵切进一步把普通 main-document pre-response transport failure 收敛成 browser-owned
error Document：请求失败 URL 继续作为 Target/history URL，新 Document 使用
`chrome-error://chromewebdata/` 并通过 `unreachableUrl` 暴露原 URL；stable Page、popup
WindowProxy 和 opener graph 保持，Document/realm 按正常 replacement 边界替换。同步 initial
realm 会把 opener 的数值 viewport surface 安装到最终 target Context，而不是即将 detach 的临时
facade；Page script environment 也会跨 realm 保存实际 opener 值。named target、`noopener`、
`javascript:` URL、完整 cross-origin WindowProxy whitelist、target admission 前的早期任务以及
close transaction 仍需后续纵切收敛。

代码基线：

- Lightmount 原始评估：`2e351a545b04`，分支 `cdp-better-4784u`；实施状态以
  `popup-refactor` 当前分支为准。
- Chromium：`a03603fe9af6`，本地 checkout `/home/donoughliu/chromium/src`。

本文讨论 HTML `window.open()`、`target=_blank` 和命名 target 创建的 auxiliary
top-level browsing context。它不是 Blink 用于 `<select>`、权限气泡等 UI 的
`PagePopup` 机制。

## 结论

原始评估基线中的 popup 不是“缺几个 API”，而是同时存在两套相互独立的实现：

1. opener `PageVM` 内的 `LightweightPopupBrowsingContextRecord` 提供同步返回给 JS
   的 Window-like facade，并自行加载、解析和执行 popup 文档；
2. protocol 收到 renderer output 后再创建一个独立 popup target、独立 `PageVM`
   和独立 document isolate，并再次导航同一个 URL。

两条路径各自已有不少能力，但它们不是同一个 browsing context。当前测试甚至把
“popup URL 恰好有两个 load owner”写成了预期。这会导致网络副作用重复、opener
拿到的 Window 与 CDP target 看到不同 DOM、`close()` 与 target 生命周期分裂，继续
补 facade 只会扩大同步成本。

`popup-refactor` 当前已经为 opener-preserving、非命名、非 `javascript:` URL 建立一条
production 迁移路径：opener 与 target 看到同一 stable WindowProxy、initial realm、Document
和 Page residence；non-empty destination 在 target admission 后从该 Page 发起一次 replacement
navigation，opener host 不再保存对应 lightweight Document record 或启动 mirrored loader。
上述双实现判断仍适用于 named target、`noopener`、`javascript:` URL 和其余尚未迁移入口；
因此 lightweight 模型仍是 Phase 4-6 的主要删除对象，而不是可以继续扩展的长期架构。

建议采纳下面的方向：

- 复用 child-frame 已经成熟的 stable `WindowProxy`、可替换 `LocalWindow` /
  `Document` generation、独立 realm、security token、跨源 facade 和 typed realm
  materialization 基础；
- 复用方式是把这些能力抽成通用 browsing-context primitive，而不是把 popup
  伪装成 iframe；
- 每个 auxiliary top-level context 仍拥有独立 Page runtime、history、task queue 和
  CDP target；
- 需要同步脚本关系的 opener / popup 共享一个可承载多个 Page realm 的 script
  agent（第一版可以是共享 V8 isolate），而不是共享同一个 V8 `Context`；
- renderer 创建的那个真实 auxiliary Page 必须被 protocol target 直接绑定，protocol
  不再创建第二个 Page、第二次导航或镜像文档；
- `noopener`、COOP group switch 和未来需要进程/agent 隔离的路径通过 remote
  WindowProxy endpoint 或独立 script agent 表达。

在开始主迁移前，必须先完成 Phase 2B 小型可行性实验：证明当前“每个并发 Page
script environment 一个 isolate”可以选择性演化为“每 script agent 一个 isolate、
每 Page/Document 一个 V8 Context”，同时不破坏 CDP object id、inspector context、
page-local task/event routing、关闭语义和内存 containment。

## 术语与必须分开的层次

本文用以下术语，避免把不同层级合并成一个“popup object”：

| 概念 | 本文含义 | 典型生命周期 |
|---|---|---|
| browsing context | 一个可导航上下文；popup 是 auxiliary top-level context，iframe 是 nested context | 可跨多次 navigation |
| WindowProxy | 调用方持有的稳定外壳，按当前 origin 和当前 inner Window 转发或拒绝属性访问 | browsing context 生命周期 |
| LocalWindow | 某个已提交 Document 的 inner Window owner | 通常随 cross-document navigation 替换 |
| realm | 一个 V8 `Context` 及其 global lexical environment / intrinsics | 通常随 LocalWindow / Document generation 替换 |
| script agent | 能让 V8 object 同步互相引用的执行宿主；拟议实现可拥有一个 isolate | 可承载多个相关 Page/realm |
| Page runtime / `PageVM` | 一个 top-level Page 的任务、导航、文档和协议执行责任方 | top-level context 生命周期 |
| browsing-context group | related pages、命名 target 查找和 opener 关系的边界 | 可因 `noopener` / COOP 分裂 |
| CDP target | 对同一个 Page runtime 的协议身份和 session 路由 | Page 可观察生命周期 |

关键点是：

- “共享 isolate”不等于“共享 realm”。两个 same-origin Window 应有不同 V8
  `Context`、不同全局 lexical state 和不同 platform singleton identity；它们只是
  可以通过 stable WindowProxy 同步互访。
- “同一个 browser context”也不等于“同一个 browsing-context group”或“同一个
  isolate”。browser context 主要承载 profile/storage/permission 隔离。
- “独立 CDP target”不要求再创建一份 renderer 文档。target 是观察和控制 Page 的
  协议身份，不是第二个页面副本。

## 评估方法与证据边界

本次评估直接阅读了 Lightmount 和本地 Chromium 源码，并运行了聚焦 nextest。
关键入口如下。

Lightmount popup：

- `lightmount-renderer-v8/src/context_bootstrap/window_runtime/dialogs.rs`
  - `window_open_callback`
- `lightmount-renderer-v8/src/native_bridge/context_host/popups.rs`
  - `LightweightPopupBrowsingContextRecord`
  - `create_lightweight_popup_window`
  - `commit_lightweight_popup_document`
  - `execute_lightweight_popup_document_scripts`
- `lightmount-protocol/src/domains/page/popup.rs`
- `lightmount-protocol/src/domains/target/lifecycle.rs`
  - `PopupTargetCreation`
  - `ensure_popup_initial_document_page_async`
- `lightmount-protocol/src/domains/target/tests/tests_target_creation.rs`
  - `window_open_hands_off_session_storage_snapshot_and_initial_storage_key`

Lightmount child-frame / realm：

- `lightmount-renderer-v8/src/frame_owner_model.rs`
- `lightmount-renderer-v8/src/frame_owner_model/records.rs`
- `lightmount-renderer-v8/src/frame_owner_model/store.rs`
- `lightmount-renderer-v8/src/native_bridge/context_host/child_frame_runtime/window.rs`
- `lightmount-renderer-v8/src/native_bridge/context_host/child_frame_runtime/isolated_world.rs`
- `lightmount-renderer-v8/src/script_vm/child_frame_realm_materialization.rs`
- `lightmount-renderer-v8/src/script_vm/post_parse.rs`
- `lightmount-renderer-v8/src/native_bridge/context_host/window_execution_context/`
- `lightmount-renderer-v8/src/native_bridge/context_host/window_security_tokens.rs`

Chromium：

- `third_party/blink/renderer/core/frame/local_dom_window.cc`
  - `LocalDOMWindow::open`
- `third_party/blink/renderer/core/page/frame_tree.cc`
  - `FrameTree::FindOrCreateFrameForNavigation`
  - `FrameTree::FindFrameForNavigationInternal`
- `third_party/blink/renderer/core/page/create_window.cc`
  - `CreateNewWindow`
- `third_party/blink/renderer/bindings/core/v8/window_proxy.h`
- `third_party/blink/renderer/bindings/core/v8/local_window_proxy.*`
- `third_party/blink/renderer/bindings/core/v8/remote_window_proxy.*`
- `content/browser/renderer_host/render_frame_host_impl.cc`
  - `RenderFrameHostImpl::CreateNewWindow`
- `content/browser/web_contents/web_contents_impl.cc`
  - `WebContentsImpl::CreateNewWindow`
- `content/common/frame.mojom`
  - `CreateNewWindowParams` / `CreateNewWindowReply`

没有在本次文档工作中重新编译 Chromium，也没有重新跑 WPT。Chromium 结论是源码
对照；WPT 数字来自仓库当前已提交的 case list，只能作为风险信号，不能当作新鲜的
回归结果。

## Lightmount 当前实现

下面 1-4 节保留原始架构评估，便于说明迁移为什么必要；其后 Phase 3 实施记录是当前分支
状态的增量事实。尤其是第三纵切 D 已经替换了窄 initial `about:blank` 路径，不能再把该路径
计入“两个 Document / 两个 Page owner”。

### 1. `window.open()` 同步路径

`window_open_callback` 已经处理了不少正确的前置语义：

- 使用 entered Window / creator Document 解析 URL；
- 非法 URL 抛错；
- 解析 window features；
- 识别 `_self`、`_parent`、`_top` 和 `_blank`；
- 处理 `noopener` / `noreferrer` 和 anchor `_blank` 的 implicit noopener；
- 尝试复用命名 lightweight popup；
- 新 popup 返回一个稳定的 synthetic Window shell，或在 opener 被抑制时返回
  `null`；
- 生成 renderer output，供 protocol 后续创建/复用 target。

这些行为让常见页面不至于在 `const w = window.open(...)` 处立即失败，也为
`Page.windowOpen`、target auto-attach 和 session storage handoff 提供了输入。

### 2. opener 内的 lightweight popup

`create_lightweight_popup_window` 在 opener 当前 V8 context 中通过
`instantiate_window_shell` 创建 Window-like object，并把它存入
`LightweightPopupBrowsingContextRecord`。record 已包含很多浏览器状的状态：

- stable Window shell / popup id / name / opener endpoint；
- initial `about:blank` Document、LocalWindow id 和后续 Document generation；
- location、history、navigation projection；
- local/session storage、storage key 和 session snapshot；
- timer、message、fetch/XHR、worker、CSP 与部分 lifecycle 状态；
- 非 `about:blank` URL 的 renderer-local load、parser 和 script execution。

但这里的 LocalWindow id 是 Lightmount owner identity，不代表出现了新的 V8 realm。
源码已经明确记录：lightweight popup 仍共享 opener 的 concrete V8 context；它在
`WindowExecutionContextRealmRecords` 中只能作为 scoped alias，不能注册成另一个
concrete realm。`document.domain` 更新 security token 时也必须跳过 popup，否则会
错误修改 opener context 的 token。

popup 文档脚本还走一条专用执行路径：扫描 DOM 中的 script，并用包含
`with (__scope) { with (window) { ... } }` 的 wrapper 模拟 popup global。它可以覆盖
不少脚本，但无法等价表达独立 global lexical environment、intrinsics、模块图、
inspector context、跨 realm wrapper 和完整 WindowProxy security semantics。

### 3. protocol 创建的真实 target

renderer output 到达 `lightmount-protocol` 后，`PopupTargetCreation` 会：

- 分配 target/session 相关身份；
- 创建 auxiliary/background target；
- 确保 popup initial Document `Page` 存在；
- 建立独立 `PageVM` 和 document isolate；
- 对请求 URL 再执行一次真实 target navigation；
- 接入 `Target.setAutoAttach`、`waitForDebuggerOnStart`、Fetch/Network 和 Runtime
  evaluate 等 CDP 路由。

这个 target 对 Playwright/CDP 来说是“真的”，但它与 opener 返回的 lightweight
Window 不是同一个 Page。

### 4. 当前实际拓扑

```text
opener target / opener PageVM / opener isolate
    |
    | window.open()
    +--> LightweightPopupBrowsingContextRecord
    |       +-- synthetic Window shell（共享 opener V8 Context）
    |       +-- mirrored loader/parser/script/lifecycle
    |
    +--> frozen renderer popup output
            |
            v
       protocol target creation
            +-- popup target
            +-- popup PageVM
            +-- popup document isolate
            +-- second navigation/load
```

`window_open_hands_off_session_storage_snapshot_and_initial_storage_key` 的测试注释明确
把第一个请求称为 opener lightweight facade 的 mirrored load，把第二个请求称为
真实 auxiliary target navigation，并断言 popup URL “must have exactly two load
owners”。这不是原始评估时的推测，而是 Phase 4 第一纵切之前的入库合同；同一测试现已
反转为“exactly one authoritative navigation owner”，当前实现状态见 Phase 4 第一纵切 A。

### 5. 已经可用的能力

当前实现不应被概括成“完全没做”。已存在的有价值能力包括：

- 常见 `window.open` 参数、features、invalid URL 和 special target 处理；
- named popup 的部分复用和 `targetInfoChanged`；
- initial `about:blank`、history、204/205 不替换文档等局部语义；
- opener policy、`noopener` / `noreferrer`、implicit noopener；
- session storage snapshot、storage key、browser-context storage partition；
- renderer-local popup 的 timer、message、fetch/XHR、worker、CSP 和部分 child blocker；
- CDP target create/attach/auto-attach/wait-for-debugger；
- popup target 的 Fetch/Network interception、Runtime evaluate、browser context 和 dialog
  路由；
- 跨 root/child/lightweight source 的 frozen popup activation identity，避免 protocol
  消费时回查已经变化的 current source。

这些能力应迁移到统一 owner，而不是删除后重写。

### 6. 核心缺口

| 语义 | 当前状态 | 直接后果 |
|---|---|---|
| authoritative Page | lightweight Page 与 target Page 并存 | DOM、history、lifecycle 可分叉 |
| navigation owner | 同一个 URL 两个 load owner | 请求、cookie、服务端副作用和计时重复 |
| realm | facade 共享 opener `Context`；target 有另一个 isolate | opener handle 与 CDP execution context 无关 |
| synchronous access | facade 可模拟部分 `w.document` | 写入不会自然出现在 target DOM |
| cross-origin WindowProxy | 有局部 restriction/facade | 不是完整 outer/inner 或 local/remote proxy 模型 |
| `window.close()` | lightweight record 可关闭；target 另有生命周期 | `closed`、targetDestroyed、资源回收可能不一致 |
| focus/blur | top-level Window 上仍有 no-op surface | named-target focus 和事件不完整 |
| named target | renderer facade registry 与 protocol target registry 分开 | 复用/导航/关闭后查找可能不一致 |
| opener / COOP | 有 opener suppression 字段和局部 policy | 没有完整 browsing-context-group split / opener sever |
| popup blocker | userGesture 被观测，部分 policy 已冻结 | 没有统一的 transient activation 消耗和创建 gate |
| sandbox | 有部分 frame policy 输入 | `allow-popups` / escape-sandbox 创建边界不完整 |
| initial empty Document | 两条路径各有一份 | 同步 mutation 与 target attach 无法指向同一对象 |
| script loader | lightweight 有专用 parser/script wrapper | 与主/child loader、module、CSP、currentness 持续漂移 |

因此，继续为 lightweight 路径分别补 module、dynamic import、beforeunload、COOP、
cross-origin descriptor、CDP Runtime context 等功能，会让每个修复都复制到另一条路径。

### 7. 当前 WPT 风险快照

对 `lightmount-benchmark/wpt-cross-current/{passed,failed,timeout}-cases.txt` 使用下面的
粗粒度关键字切片：

```text
window-open|window_open|browsing-context-names|noopener|noreferrer|opener|auxiliary
```

当前记录为：

| 状态 | case 数 |
|---|---:|
| pass | 30 |
| fail | 11 |
| timeout | 29 |

代表性风险包括：

- `multiple-globals/context-for-window-open.html` timeout；
- `windows/auxiliary-browsing-contexts/opener*.html` 多项 timeout；
- `browsing-context-names/choose-existing-001.html` timeout；
- `initial-empty-document/window-open-204-pushState-replaceState.html` fail；
- `the-window-object/window-open-noreferrer.html` fail。

也已有明确通过项，例如 initial-empty 204 fragment、部分 feature tokenization、
`noreferrer-null-opener` 和 anchor implicit noopener。这个分布与“表面和协议能力已有，
多 global / opener / named-context 生命周期仍不完整”的代码结论一致。

## Chromium 的责任链

Chromium 的具体类很多，但对 Lightmount 最重要的不是复制多进程结构，而是保持同一
条语义责任链。

### 1. Blink 完成调用方语义和 target 选择

`LocalDOMWindow::open`：

- 从 entered Window 完成 URL；
- 解析 window features、referrer、user gesture 和 attribution；
- 通过 `FrameTree::FindOrCreateFrameForNavigation` 选择 special/named target 或创建
  新 auxiliary context；
- 对得到的那个 frame 发起导航；
- special target 保持返回现有 Window；普通 `noopener` 新窗口返回 `null`；
- existing named target 在适用时更新 opener 并返回该 context 的 `DOMWindow`。

`FrameTree::FindFrameForNavigationInternal` 的查找范围依次包含：

- `_self` / `_current`、`_top`、`_parent`、`_blank` 等关键字；
- 当前 frame subtree；
- 当前 Page 的完整 frame tree；
- `Page::RelatedPages()` 中其它 Page 的 frame tree；
- embedder fallback。

所以命名 target 不是某个 `Window` object 的局部 map，而是 related browsing contexts
上的查找和 `CanNavigate` policy。

### 2. Blink 与 browser process 共同创建一个真实 Page

`blink::CreateNewWindow` 设置 auxiliary frame type，检查 dismissal、URL/security、
sandbox popup flags，分配 session storage namespace，并调用 `ChromeClient::CreateWindow`。
保留 opener 的路径会 clone session storage；`noopener` 路径不沿用这份 clone。

`RenderFrameHostImpl::CreateNewWindow` 在 browser process 中统一处理：

- popup blocker / embedder policy；
- transient user activation 判断与消耗；
- storage namespace；
- credentialless、fenced frame、COOP 导致的 opener suppression；
- virtual browsing-context group；
- 是否建立新的 `BrowsingInstance`；
- initial empty Document policy/COOP reporter；
- DevTools `wait_for_debugger`；
- frame、widget、interface 和 document token。

`WebContentsImpl::CreateNewWindow` 创建实际的新 `WebContents` / `Page` / `FrameTree`。
保留脚本关系的 popup 与 source `SiteInstance`/`BrowsingInstance` 协作；opener suppressed
或禁止 JS access 的路径进入新的 BrowsingInstance，source renderer 不拿到可访问 handle。

Lightmount 不需要照搬 UI thread、widget、SiteInstance 或多进程 IPC，但需要保留：

- 创建 policy 只有一个最终裁决；
- initial empty context 先真实存在；
- 之后的 navigation 只作用于该 context；
- DevTools target 观察同一个 Page；
- opener suppression 改变返回 handle 和 group/agent 关系，而不是创建一份镜像。

### 3. stable outer WindowProxy + replaceable inner global

`window_proxy.h` 对 split Window model 的注释非常直接：

- outer global proxy 跨 navigation 复用；
- 每个 Document 通常对应新的 inner global object；
- initial empty Document 到 same-origin 首次 commit 是允许复用 inner global 的唯一特殊
  情况；
- same-origin access 转发到当前 inner global；
- cross-origin access 进入 outer proxy interceptors；
- local frame 使用 `LocalWindowProxy`，跨进程 frame 使用 `RemoteWindowProxy`。

这正是 Lightmount child-frame 已经开始实现、而 lightweight popup 绕开的基础。

### 4. Chromium 与当前 Lightmount 的对比

| 维度 | Chromium | 当前 Lightmount | 目标 |
|---|---|---|---|
| 新窗口实体 | 一个真实 Page/FrameTree | lightweight record + target Page 两份 | 一个 auxiliary Page runtime |
| JS 返回值 | 指向真实 context 的 WindowProxy，或 `null` | 指向 opener 内 facade | 指向真实 auxiliary context 的 stable proxy |
| initial empty Document | 新 Page 的真实初始文档 | facade 与 target 各一份 | 同一个真实初始文档 |
| navigation | 对选中的 frame/Page 导航一次 | mirror load + target navigation | 一个 navigation token、一个 loader owner |
| realm | Page/Frame 的 V8 context | facade 共享 opener context | popup 独立 V8 context |
| same-origin sync access | 真实 cross-context object access | facade 模拟 | 共享 script agent 上的真实 proxy 转发 |
| cross-origin | local/remote WindowProxy + access checks | 部分 facade restriction | 复用通用 proxy/access surface |
| named target | frame tree + related pages + policy | 两套局部 registry | group-level registry + single context identity |
| opener | frame relationship，可被 suppression/COOP 切断 | facade 与 target relationship 可漂移 | group graph 的唯一 opener edge |
| storage | 创建时确定 namespace/clone policy | snapshot handoff 后 target 另建 | 创建 transaction 只分配/clone 一次 |
| CDP | target 对应实际 Page | target 对应第二个 Page | target bind/adopt renderer-created Page |
| close | Window/Page/target 同一生命周期 | record 与 target 分开 | 同一 close transaction |
| popup gate | browser-side policy + activation consume | 局部 userGesture/policy | owner-level gate，结果冻结一次 |
| COOP | BrowsingInstance / virtual BCG / opener sever | boolean/policy projection 为主 | group switch + proxy endpoint 更新 |

## 为什么 child-frame 基础值得复用

child-frame 当前实现已经从早期 same-realm shell 演化成一条较完整的 realm ownership
链。关键能力包括：

### 1. stable proxy 与 realm promotion

`ChildWindowProxyRecords` 保持：

- stable child WindowProxy identity；
- live Window wrapper 与 facade context；
- stable `parent` / `top`；
- same-origin 和 cross-origin endpoint projection；
- caller-specific cross-origin access surface；
- default execution context id。

`take_child_window_proxy_shell_for_realm` 会把 pre-bootstrap facade context 的 global
detach，`post_parse` 创建真正的 child V8 `Context` 时复用这个 global object，随后由
`promote_child_window_proxy_shell_to_realm` 完成 promotion。这样 JS 早先拿到的
`iframe.contentWindow` identity 不会因 realm materialization 或 navigation 改变。

### 2. 明确的 LocalWindow / Document transition

`frame_owner_model` 已把 transition 写成 typed decision：

- `Installed` / `Preserved` / `Replaced` / `Retired`；
- `ReplaceLocalWindow`；
- `ReuseInitialEmptyLocalWindow`。

`store` 只在 initial empty、same accessible origin、policy/domain 条件匹配时复用
LocalWindow；其它 cross-document commit 替换 LocalWindow。旧 generation 不能因为
异步完成事件再写入新 Document。

### 3. 独立 V8 Context 与精确 owner

`ensure_prebootstrapped_child_default_context` 为 child 创建独立 V8 `Context`，注册
Window execution context binding/security token，并按需请求 materialization。

`child_frame_realm_materialization` 使用 typed task，携带 exact child handle、owner 和
generation；执行前重新验证 currentness，失败时回滚，成功后注册 inspector context。
这比 popup 专用 `with(window)` wrapper 更接近浏览器 realm 模型。

### 4. security 与跨源表面

child 路径已经具备：

- effective-origin security token；
- same-origin/cross-origin access decision；
- restricted WindowProxy surface；
- named/indexed property projection；
- `postMessage` 和 cross-origin `location` navigation；
- same-origin → cross-origin → same-origin round trip 时的 stable proxy identity。

它还不是 Chromium WindowProxy 的完整实现，但责任边界是正确的：security 决策位于
stable proxy / concrete realm 边界，而不是散落在每个 WebAPI callback。

### 5. initial empty 与旧 realm 退休

child initial empty Document 可以继承 creator origin/policy/resource authority，并带有
明确的 load-token suppression。commit 会 preflight exact owner，决定是否复用 initial
empty LocalWindow，安装新 Document loader，并退休旧 Document 的 callbacks、wrapper、
IndexedDB 等状态。

这些正是 popup 需要的机制。

## 不能直接“把 popup 当 child frame”

复用 child 基础不等于创建一个隐藏 iframe。两者有不可抹平的产品语义：

| child frame | auxiliary popup |
|---|---|
| 有 parent browsing context | 是 top-level context，没有 parent |
| `frameElement` 指向 owner element | `frameElement === null` |
| `top` 指向所属 top-level Page | `top === self` |
| 通常随 owner element detach | 可以在 opener 关闭后继续存在 |
| 参与 parent parser/load blocker | 不应成为 opener Document 的 child load blocker |
| 属于同一 Page 的 frame tree | 拥有独立 Page、history 和 CDP target |
| name lookup 首先是 frame-tree 语义 | name 还需跨 related Page 查找 |
| owner key 当前是 iframe `DomHandle` | 需要独立 `BrowsingContextId` / Page identity |

所以应该抽取 stable proxy、realm、generation 和 access-control primitive，再分别由
`NestedBrowsingContext` 与 `AuxiliaryTopLevelBrowsingContext` 组合。

## 架构选项

### 选项 A：保留双实现并增强同步

做法：继续维护 lightweight DOM，在每次 mutation、navigation、history、storage、close
时同步到 target Page。

结论：拒绝。

原因：

- 两个 loader owner 无法安全去重所有网络和服务端副作用；
- JS object identity、Promise、module namespace、DOM wrapper、Event 和 lexical binding
  不能通过状态复制等价同步；
- currentness/generation race 会成倍增加；
- CLI 与 CDP 的完成条件会继续不同。

### 选项 B：target 保持独立 isolate，opener 只持 remote facade

做法：总是先创建 target PageVM/isolate，opener 返回 RPC 风格 facade。

结论：只适合 `noopener`、COOP-separated 或真正 remote 的访问路径，不能作为普通
same-origin `window.open` 的唯一模型。

原因：same-origin opener 可以同步读取/写入 popup DOM、传递 JS object、调用函数并
观察立即结果。跨 isolate RPC 不能在不引入阻塞和对象代理系统的情况下满足这些语义。

### 选项 C：把 popup realm 永久放在 opener PageVM

做法：直接把 child realm machinery 用于 opener 内一个 top-level-shaped record，CDP
target 代理进 opener PageVM。

结论：可作为短期原型，不适合作为终态。

原因：popup 应有独立 Page task/lifecycle/target，并可在 opener 关闭后存活；把它永久
嵌在 opener PageVM 会让 close、调度、target session、memory accounting 和 ownership
持续特殊化。

### 选项 D：related-page group + shared script agent + 独立 Page runtime

做法：

- browsing-context group 管 related Page、name 和 opener graph；
- script agent 管可以同步互访的 V8 contexts；第一版可让保留 opener 的 popup 与
  opener 共享 isolate；
- opener Page 和 popup Page 各有独立 `PageVM`、main WindowProxy、Document realm、
  task queue、history 和 target binding；
- child 与 popup 共用抽取后的 WindowProxy/LocalWindow/realm primitive；
- protocol target 绑定已经存在的 popup Page residence，不再另建 Page。

结论：推荐。

它同时满足同步 JS identity、独立 Page/CDP 生命周期和单一 document owner。代价是要
放宽当前 per-page-isolate policy，因此必须先做可行性验证。

不建议把所有 browser-context Page 无条件放进一个全局 isolate。共享范围应由 script
agent / browsing relationship 决定，并保留未来按 origin/COOP 切分 agent 的能力。

## 目标架构

下面是逻辑结构，不要求按同名 Rust struct 落地：

```text
RendererBrowserContextRuntime
  |
  +-- BrowsingContextGroupRegistry
       |
       +-- RendererBrowsingContextGroup
            +-- related-page name registry
            +-- opener relationship graph
            +-- one or more RendererScriptAgent
            |     +-- V8 isolate / inspector backend
            |     +-- realm/context registry
            |
            +-- opener Auxiliary/Primary PageRuntime
            |     +-- PageVM
            |     +-- stable main WindowProxy
            |     +-- current LocalWindow/Document realm
            |     +-- CDP target binding
            |
            +-- popup Auxiliary PageRuntime
                  +-- PageVM
                  +-- stable main WindowProxy
                  +-- current LocalWindow/Document realm
                  +-- CDP target binding
```

### 通用 browsing-context record

拟议的通用 record 至少应表达：

```text
BrowsingContextIdentity
  id
  kind = PrimaryTopLevel | AuxiliaryTopLevel | Nested
  group_id
  script_agent_id
  name
  parent?                 // 只用于 nested
  opener?                 // 只是一条可切断的 related-context edge
  stable_window_proxy
  current_local_window_generation
  current_document_generation
  current_realm/context_token
  origin/policy/security_token
  history/session_storage_namespace/storage_key
  lifecycle = InitialEmpty | Active | Closing | Closed
  page_residence?         // top-level only
  target_binding?         // top-level only
```

不要把所有字段都放进一个巨型 struct；上面只是必须有唯一 owner 的状态清单。具体可
拆成 identity、relationship、document owner、proxy endpoint 和 Page residence。

### 责任归属

| 责任 | 建议 owner |
|---|---|
| context id、kind、name、parent/opener edge | browsing-context group |
| stable WindowProxy 和 local/remote endpoint | 通用 WindowProxy host |
| LocalWindow/Document generation | browsing-context document owner |
| V8 isolate、context registry、inspector backend | script agent |
| navigation、task queue、history、Page lifecycle | top-level PageVM / nested frame owner |
| browser-context storage/permission/network policy | renderer browser-context runtime |
| target id、session、auto-attach 和 CDP policy | protocol target controller |
| target 到 renderer Page 的绑定 | `RendererPageResidenceIdentity` 与 target residence bridge |

protocol 可以决定是否 auto-attach、是否 wait for debugger、如何发 CDP event，但不能
再拥有第二个 popup loader 或文档。

## 目标 `window.open()` transaction

### 新 auxiliary context

建议的顺序如下：

1. 在 entered realm 捕获 exact source Page/Document generation，完成 URL 和 features
   解析。
2. 在 browsing-context group 中解析 special target / named target，并执行
   `CanNavigate`、sandbox、popup blocker、transient activation、opener/COOP policy。
3. 如果选择已有 context，只对该 context 排队一次 navigation，并返回其 stable
   WindowProxy；不要创建 popup carrier 或 target。
4. 如果创建新 context，先同步分配 auxiliary context id、Page residence、stable main
   WindowProxy 和真实 initial empty Document realm。
5. initial empty Document 继承 creator origin/policy；按最终 opener policy 分配或 clone
   session storage namespace，整个 transaction 只做一次。
6. 把 non-empty URL 记录成该 Page 的一个 pending navigation token。此时
   `window.open()` 已可返回，调用方可以立即执行 `w.document.write(...)`。
7. renderer 发布 immutable `AuxiliaryContextCreated` output，其中携带 context/Page
   residence、source generation、target name、features、opener policy 和 pending
   navigation identity；它是“已创建 Page 的通知”，不是“请 protocol 再创建 Page”。
8. protocol 为这份 Page residence 分配并绑定 CDP target，应用 auto-attach、Fetch/
   Network、Runtime script 和 wait-for-debugger policy。
9. owner runtime 只在 target admission 完成后释放 popup 自身的 task/script/navigation；
   `waitForDebuggerOnStart` 只暂停这一个真实 target。
10. 对 pending URL 执行一次 navigation。redirect 可以产生多个网络 hop，但一个
    navigation token 只有一个 authoritative loader。

`Page.windowOpen` 应从同一 creation record 派生为观测事件，不应作为创建第二个 Page
的触发器。

### 同步创建与 protocol admission 的边界

`window.open()` 必须同步返回，但 CDP output 通常在当前 renderer turn 结束后才被
protocol 消费。不能用 sleep、drain 或轮询填这个时间差。建议显式建模：

- `PendingAuxiliaryPage` 已拥有 initial empty realm，因此 opener 的同步跨 Window 操作
  是真实操作；
- popup 自己的新 task、parser 和目标 URL navigation 在 `TargetAdmission` 前不可运行；
- protocol 不存在的 CLI 路径使用同一 admission API 的默认立即接受策略；
- auto-attach 配置应在 renderer browser-context runtime 中有可读取的冻结 snapshot，
  或由 owner loop 做明确 handshake；
- stale admission 必须带 Page residence/generation，不能释放已关闭或已替换的 popup。

### named target

命名查找必须统一为 group operation：

1. special keyword；
2. source frame subtree / current Page frame tree；
3. related Page；
4. policy fallback。

查找到现有 popup 后：

- 返回同一 stable WindowProxy；
- 只导航同一 Page；
- 必要时 focus 该 Page；
- 已关闭 context 不参与查找；
- `noopener` 对 special target 和 existing target 的具体 Chromium/WPT 行为要由专门
  compatibility test 固定，不能只靠 feature parser 推断。

### `noopener` / `noreferrer`

创建仍然发生，但：

- `window.open()` 返回 `null`；
- 新 context 没有可脚本访问的 opener edge；
- `noreferrer` 同时影响 referrer；
- 它可以进入新的 browsing-context group / script agent；
- source renderer 不需要持有 local proxy，protocol 仍可观察独立 target；
- storage namespace clone policy应与 Chromium/WPT 对齐，不能无条件复用保留 opener
  路径的 snapshot。

### cross-origin navigation 与 COOP

普通 related popup 从 same-origin initial empty Document 导航到 cross-origin 后：

- source 已持有的 WindowProxy identity 保持稳定；
- access surface 切到 cross-origin restriction；
- `postMessage` 和允许的 `location` write 仍走 endpoint；
- 回到 same-origin 后可重新转发到新的 LocalWindow realm，但不能复活旧 realm。

第一阶段可以让 related cross-origin Page 留在同一 isolate，并依赖 security token 和
access checks；这符合 Blink 中 cross-origin LocalFrame 也可能存在的事实。未来需要
agent/process separation 时再把 endpoint 切成 remote。

COOP group switch 不只是设置 `crossOriginIsolated` boolean。它要：

- 分配/切换 browsing-context group；
- sever opener relationship；
- 让旧 group 持有的 proxy endpoint 呈现断开/closed 语义；
- 防止旧 generation 的 message/navigation/async completion 进入新 group；
- 必要时切换 script agent。V8 context 不能跨 isolate 搬迁，因此 agent split 必须是
  新 realm commit，而不是移动已有 handle。

### close 与 target 生命周期

`window.close()`、`Target.closeTarget`、opener 观察到的 `popup.closed`、
`Target.targetDestroyed` 和资源回收必须落到同一个 close transaction：

1. context 从 `Active` 原子进入 `Closing`；
2. beforeunload/unload policy 由同一个 Page owner 决定；
3. 拒绝新 navigation/task，旧 async completion 因 generation 不匹配被丢弃；
4. 关闭 Document/realm/Page resources；
5. 从 name/opener registry 移除；
6. protocol 从同一状态变化派生 targetDestroyed；
7. stable proxy 继续可被旧 JS handle 观察为 `closed`，但不再暴露 live inner Window。

popup 不应因为 opener Page 关闭而自动销毁，除非产品 policy 明确如此。

## 迁移计划

迁移应按能够独立验证的不变量切片，不做一次性全仓库重写。

### Phase 0：冻结现状与目标 probe

目的：在改 ownership 前把关键可观察差异变成最小复现。

- 保留当前双 load 测试作为“已知债务”的 characterization，但移除任何把双 load 描述
  为长期正确语义的文档；
- 增加或整理本地 HTML/CDP probe：
  - `const w = open('about:blank'); w.document.write(...)` 后 CDP DOM 与 `w.document`
    必须相同；
  - non-empty URL 服务端只收到一个 top-level navigation request；
  - popup script mutation 可由 opener 和 popup target 同时观察；
  - `w.close()` 产生对应 targetDestroyed；
  - popup 在 opener close 后继续运行；
  - same-origin → cross-origin → same-origin 的 proxy identity；
  - named popup reuse；
  - 204/205 保留 initial Document/history；
- 用本地 Chromium 对同一 probe 录制 event/order/return-value 参考。

目标语义测试在相应 phase 完成前可以作为独立 probe 或明确 ignored debt，不能通过
放宽断言让错误路径变绿。

### Phase 1：从 child 抽取通用 primitive，行为不变

- 把 key 从裸 `DomHandle` 提升为可表达 nested/top-level 的 typed context identity；
- 抽取 stable proxy record、LocalWindow transition、realm materialization request、
  security/access surface 和 retirement hook；
- child adapter 继续提供 `parent`、`top`、`frameElement` 和 parent load blocker；
- 所有现有 child focused nextest 必须保持通过；
- 此阶段不改 popup production path，避免同时改基础和调用方。

完成标志：通用 primitive 不依赖 iframe owner element，child 行为无回退。

实施状态（`popup-refactor`，Phase 1 第一至三切片）：

- 已新增与 iframe owner、Page target、popup carrier 无关的
  `BrowsingContextId` / `BrowsingContextKind`；main 与 child frame owner record
  现在显式持有该 identity；
- owner-model 的 stable WindowProxy record、LocalWindow commit transition、Document
  creation/initial-empty transition 和 realm materialization request 已移到通用
  browsing-context model；frame 层通过 type alias 维持现有调用语义；
- child V8 WindowProxy registry 的 authoritative key 已从 `DomHandle` 改为
  `BrowsingContextId`；`DomHandle` 仅保留在 child adapter 中用于从 iframe owner
  查找 context，owner rebind 仍保留当前 stable proxy 行为；
- realm lifecycle 和 exact Document/LocalWindow/realm currentness 已抽成参数化的通用
  primitive；frame owner 通过 typed alias 保留原有状态机和 stale-generation 判定；
- Document owner retirement transaction 现在同时携带 `BrowsingContextId` 和精确的
  retired/current owner generation；initial empty install、navigation replacement、
  `document.open()` 和 detach 都从 owner store 发布同一形状的 transition，iframe
  `DomHandle` 仅由 frame adapter 额外携带；
- main 与 child Document replacement 现在都组合同一个通用 owner transaction；
  external-state retirement hook 携带 context id、retired owner 和 exact Document token，
  child adapter 只追加 iframe handle 并消费 child 专属清理；
- origin access comparison、realm access policy、default/isolated world 和
  `RealmHostProjection` 已移到通用 browsing-context model；child realm 初始化与
  isolated-world rebind 必须同时匹配 context id、exact owner、realm token 和 world，
  `parent` / `top` / `frameElement` 仍由 child adapter 安装；
- popup production path、loader、protocol target creation 和双 load characterization
  尚未改变。这正是 Phase 1 的行为不变边界，不代表 popup 已修复；后续 auxiliary
  Page 可以组合上述 primitive，而不需要继承 iframe owner element 或 parent load
  blocker。通用 primitive 已不依赖 iframe owner element，Phase 1 完成。

### Phase 2：shared script-agent 可行性实验

#### Phase 2A：显式 identity 与当前策略基线

Phase 2A 当时的源码基线已经引入 typed `ScriptAgentId`，由 document isolate holder 分配并通过
Lightmount runtime memory diagnostics 暴露。`RendererPageScriptEnvironment` 是当前
agent identity 的稳定宿主：

- 同时存活的两个顶层 Page script environment 必须报告不同 `ScriptAgentId`；
- 同一个稳定 Page 的 cross-document navigation 必须保留 `ScriptAgentId` 和 main
  WindowProxy，但替换 Document realm/context generation；
- child default world、isolated world 和同 Page navigation generation 复用所属 Page
  agent；
- fresh Page diagnostics 明确报告 `scriptAgentScope = page-script-environment`；Phase 2A
  当时尚未开放 related-page admission。

这一步只把现有策略变成可命名、可测试的边界，没有让两个 production Page 共享
isolate，也没有改变 popup 双实现或双 load。

仓库历史上已经有过 renderer-owner-wide shared document isolate：`40321d2894` 建立
基础，`310362ebe3` 将其设为默认。旧回归覆盖了多个 Page 的不同 V8 Context/global、
不同 Inspector context group、peer close 后存活、navigation context replacement、
timer/fetch/worker/IndexedDB 和 stale generation 路由。因此“一个 isolate 能承载多个
Page realm”本身不是未知项。

但该策略随后因跨页面 V8 heap 累积和回收边界过宽，在 `7b17efa965` 切回 per-Page
containment，并由 `b149639b6d` 接受为临时 workaround。历史功能回归不能覆盖这一
内存失败，也不能证明把所有 renderer-owner Page 再次放入同一 agent 是安全的。当前
设计因此只允许由 browsing relationship admission 的 related Page 共享 agent；禁止
恢复 owner-wide 默认共享。历史和当前 workaround 的边界见
[Per-Page Document Isolate 临时 Workaround](per-page-document-isolate-temporary-workaround-2026-07-10.md)。

#### Phase 2B：selective related-page admission

建立最小实验，让两个 Page residence 在同一 script agent/isolate 中拥有不同 V8
Context，并验证：

- realm 的 `globalThis`、intrinsics、lexical bindings 相互独立；
- same-origin WindowProxy 可同步传递 object/function/DOM wrapper；
- context embedder data 能精确路由到各自 Page/Document generation；
- CDP executionContextId、remote object id、object group 和 binding 按 target/session
  隔离；
- inspector context created/destroyed 顺序正确；
- microtask、timer、fetch、worker、IndexedDB 和 unhandled rejection 回到来源 Page；
- 关闭一个 Page 不销毁另一个 Page 的 isolate/resources；
- popup 在 opener PageVM drop 后仍可执行；
- 不引入裸指针、泄漏全局 cache、sleep/drain/retry。
- related Page 全部关闭后 agent/isolate 可确定销毁；非 related Page 不会被 admission；
- 多轮 related-page create/close 与 navigation churn 的 heap/RSS 不退回
  renderer-owner-wide 累积模式。

如果实验无法在当前 `RendererPageScriptEnvironment` / `PageVm` ownership 下保持这些
不变量，应先重构 script-agent owner，不能退回 mirror 同步。

实施状态（Phase 2B 完成时的 test-only 验证基线；Phase 3 第一纵切已提升窄入口）：

- document isolate 现在持有 `RendererScriptAgentV8ForegroundTaskRouter`，production
  fresh Page 默认只注册一个 Page member，因此现有 per-Page containment policy 不变；
- stable `RendererPageScriptEnvironment` 持有 RAII agent membership；same-Page
  replacement 复用 membership，Page slot retirement 先撤销 route，再清理 Page tasks；
- Phase 2B 中 `#[cfg(test)]` 的 related-page reservation 可以从同 renderer owner 的 live source
  Page 共享 isolate holder，同时创建独立 Page environment、main WindowProxy、V8
  Context、task sources、output journal 和 Inspector binding；Phase 3 第一纵切已把
  reservation/admission/router 提升为 production primitive，但只允许 renderer 明确新建的
  auxiliary context 使用，默认 Page 仍是 fresh；
- V8 foreground task 是 isolate/agent scoped，V8 不提供 originating `Context`。router
  只让一个 live member 执行 concrete task 一次，随后给其他 member 排入 typed
  checkpoint；如果执行 Page 正在退休，尚未执行的 concrete task 会转投 surviving
  member，checkpoint-only payload 不跨 Page 退休；
- 初版只把 task 路由给一个 member 时，peer Page 的 `awaitPromise` 稳定在 30 秒门禁
  超时；加入 task-once + peer-checkpoint 后同一用例通过。这说明 fan-out 是多 realm
  agent 的必要 checkpoint 语义，不应以 sleep、轮询或把 task 重复执行多次替代；
- 聚焦实验已经证明：两个 related Page 报告同一 `ScriptAgentId` / 一个 isolate，
  但拥有不同 global、intrinsics、main WindowProxy、Inspector context group；跨 target
  remote object fail closed；source Page close 后 peer、navigation replacement 和 async
  WebAssembly foreground continuation 仍可工作；
- 默认两个并发 Page 仍报告两个 agent/两个 isolate；同一 Page navigation 仍报告一个
  agent/一个 membership。三条 policy 回归共同防止 test admission 外溢到 production。
- 第二切片增加 `#[cfg(test)]` owner-thread probe，把 peer Page 已存在的 stable main
  WindowProxy 直接安装到 related Page realm；没有创建第二个 proxy、mirror global 或
  旁路 DOM。same-origin Page 已验证普通 object、function 和 DOM wrapper 可同步跨 realm
  传递；peer 同源 navigation 后，保存的 proxy 保持严格相等并投影到 replacement
  Document，新 realm 不继承旧 global property；
- timer、`fetch(data:)`、unhandled rejection 分别回到创建它们的 Page realm；A/B 同时
  有 pending work 时不会把 completion 或 rejection event 投到 peer；
- 同一个 Inspector isolate backend 中，同名 object group 仍由 Page inspector binding
  隔离，A 的 `Runtime.releaseObjectGroup` 不释放 B 的 remote object；同名 isolated world
  中 A 的 `Runtime.addBinding` 不注入 B，binding observation 只进入 A 的 output stream；
- dedicated worker 的 message route 和 Page-local IndexedDB manager route 在共享 isolate
  下保持精确；admission source close 后 peer 的 worker event 和 IndexedDB transaction
  仍可完成；
- 三个 member 按中间 Page → 原始 source 的非 LIFO 顺序关闭后，survivor 可再次 admission
  新 member；`scriptAgentPageCount` 按 3→2→1→2→1 变化，整个序列只创建并最终销毁一个
  document isolate。这里的 membership count 与只统计未 commit residence 的全局
  `reserved` diagnostics 已明确区分。
- shared isolate 让此前被 per-Page isolate disposal 掩盖的 Context 自引用变得可观察：
  rusty_v8 Context slot 由 Context annex 持有，但 `BridgeContextWindowWrapper` 和
  `IntrinsicInterfaceRegistry` 又分别通过强 `v8::Global` 指回该 realm 的 Window、
  constructor、prototype 和 public interface。只丢 Rust `Global<Context>` 不能打破这个
  跨 Rust/V8 的 ownership cycle；peer Page close 后 Context 和 global handles 会留在仍
  存活的 agent isolate 中；
- teardown 现在先清所属 wrapper cache，再只移除上述两个明确拥有 realm-local strong
  V8 handle 的 slot。host pointer、runtime-observable token、resource owner、Promise
  rejection 和其它 execution-owner metadata 保持到 V8 真正回收旧 realm，因此 retained
  child-realm function 仍能按原 owner fail closed，而不是退化成通用 `no access`；
- 新的普通回归连续创建三个 related peer；每个 peer 都执行一次 cross-document
  navigation，再关闭并由 anchor agent 触发 GC。active/native context 必须是 anchor+1，
  replacement 后不能增加，close 后必须回到 anchor baseline；detached context、Inspector
  registry、agent member count 和最终 isolate accounting 同时设为硬断言；
- 新的 ignored release acceptance 默认执行 120 轮，每轮分配 24,000 个 JS objects、
  4,000 个 DOM nodes 和 1,000 个 resolved Promises，每 12 轮导航一次 peer。它记录
  `/proc/self/status`、V8 heap/physical/external memory、global handles、native/detached
  contexts、Inspector registry、agent membership 和 isolate accounting；release 且至少
  60 轮时，后半段线性斜率硬门为 used heap `<= 0.02 MiB/轮`、RSS
  `<= 0.20 MiB/轮`。

本切片还暴露并修正了一条独立 fixture 生命周期：最初 full suite 中 standalone
`ScriptVm` 在 realm bootstrap 后立即丢弃 membership，导致异步 WebAssembly foreground
task 没有 live Page route，5 秒门禁超时。membership 现在显式穿过 page-realm/default-world
bootstrap 并保留到 `ScriptVm` 生命周期；owner-managed Page 仍由 stable Page environment
额外持有，并在 Page slot retirement 时主动撤销。第一切片合入前验证结果：

```bash
cargo nextest run -p lightmount-renderer-v8 \
  script_vm::tests::browser_api::misc::webassembly_compile_accepts_spec_valid_bounds_above_v8_instantiation_limits
# 1 passed

cargo nextest run -p lightmount-renderer-v8 \
  -E 'test(/runtime::tests::(related_page_script_agent_experiment_shares_isolate_and_survives_source_close|per_page_isolate_policy_uses_distinct_isolates_and_isolates_contexts|per_page_isolate_policy_reuses_navigation_isolate_and_replaces_contexts)$/)'
# 3 passed

cargo nextest run --no-fail-fast
# 15551 passed, 17 skipped

cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
# passed
```

第二切片最终验证：

```bash
cargo nextest run -p lightmount-renderer-v8 \
  -E 'test(/runtime::tests::related_page_script_agent_/)'
# 7 passed

# 上述 7-case filter 连续执行 20 轮
# 20/20 passed，合计 140 case executions

cargo nextest run --no-fail-fast
# 15557 passed, 17 skipped

cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
# passed
```

#### Phase 2B 第三切片：realm teardown 根因与 release 长序列

最初的零 payload 诊断只保留一个 anchor Page，重复创建/关闭 related peer，并每两轮
导航一次 peer。修复前四轮 post-close native context 数依次为 `2 / 4 / 5 / 7`，used
global handles 为 `10,208 / 15,552 / 18,080 / 23,328 bytes`，detached context 为
`0 / 1 / 1 / 2`。每个未导航 peer 本身留下一个 Context，每次 peer navigation 又留下
一个 retired Context；即使追加 V8 full GC 也不下降。与此同时：

- Inspector default-context registry 在 peer close 后已经回到 `1`；
- agent membership 已从 `2` 回到 `1`；
- `RendererPageScriptEnvironment` 的最后一个 `Rc` owner 确实析构；
- 整个 anchor 关闭后 isolate accounting 也回到 baseline。

因此问题不是 Inspector registry、Page testing handle、environment clone 或 GC 调度，
而是仍存活 isolate 内的 strong V8 root。进一步核对 Context annex 后确认了上节所述的
两个 self-cycle slot。

第一次修复尝试在 retirement 时调用 `Context::clear_all_slots()`。它能让内存 probe
立即转绿，但全量 nextest 准确地否决了这个边界：retained old-child XHR、fetch、Beacon
function 丢失 host pointer，预期的旧 realm shutdown/`false` 语义变成 `no access`；child
self-navigation load 和一次 `document.domain` navigation 也失败。最终实现只释放两个
拥有 realm-local strong V8 handles 的 slot，保留其它旧 realm metadata。对应 5 个
child-navigation 回归与新的 related realm 释放回归组成 6-case 交叉集合，最终全部通过。

exact release 源码快照：

- commit：`847448b8447f0d226567394d2e878265d3d0cafe`；
- Git tree：`58acc6161960baef5466636f81dca99ba1318b4f`；
- profile：Cargo `release`，独立
  `CARGO_TARGET_DIR=target/related-agent-memory-release-847448b844`；
- rustc：`1.96.1 (31fca3adb 2026-06-26)`，host
  `x86_64-unknown-linux-gnu`；
- host：Linux `6.12.73+deb13-amd64`，Intel Core i9-13900K，32 online logical CPUs；
- 测试 binary SHA-256：
  `10fa659fea2c262cc26260afb7f5af6bfdc9edc64beabf04c2840f259a6127d4`。

两次运行使用同一个 binary、相同 workload 和硬门：

| 指标 | run 1 | run 2 | 门禁/解释 |
|---|---:|---:|---|
| 120 轮 elapsed | `8.474 s` | `8.206 s` | 完整 payload 与 10 次 peer navigation |
| 后 60 轮 post-close used-heap slope | `0.000000000 MiB/轮` | `0.000000000 MiB/轮` | `<= 0.02` |
| 后 60 轮 post-close RSS slope | `0.025481 MiB/轮` | `0.005147 MiB/轮` | `<= 0.20` |
| 首/末 10 轮均值 used-heap delta | `0.006378 MiB` | `0.006378 MiB` | 非线性增长指标，不单独设门 |
| 首/末 10 轮均值 RSS delta | `4.605078 MiB` | `3.907813 MiB` | allocator/file-backed 平台变化，不单独设门 |
| peak active used heap | `16.373085 MiB` | `16.264664 MiB` | peer heavy payload 存活时 |
| peak active RSS | `125.832031 MiB` | `125.355469 MiB` | 同上 |
| final post-close used heap | `1.825363 MiB` | `1.825363 MiB` | anchor-only |
| final post-close RSS | `102.847656 MiB` | `101.660156 MiB` | anchor-only |
| max detached contexts（active/nav/close） | `0 / 0 / 0` | `0 / 0 / 0` | 硬断言 |
| native contexts（anchor/active/post-nav/post-close） | `1 / 2 / 2 / 1` | `1 / 2 / 2 / 1` | 每轮硬断言 |
| used global handles（first → last post-close） | `7,744 → 7,840 B` | `7,744 → 7,840 B` | 没有按 peer/导航累积 |
| isolate accounting（baseline → live → final） | `0 → 1 → 0` | `0 → 1 → 0` | created/destroyed 均为 `1` |

原始 JSON 位于 ignored `target/` 下，不作为源码提交：

- `target/related-agent-memory/847448b844-run1.json`，SHA-256
  `94d47df8d63d6b86a4772d5e3fe755659c976cb20eae6071840d4ca97421aa20`；
- `target/related-agent-memory/847448b844-run2.json`，SHA-256
  `39088d5bfbcce683530fb0af01fb1deb5c8c4f7eebc5952953bf35928c65521e`。

复跑命令；run 2 只替换 `OUTPUT` 文件名：

```bash
env \
  CARGO_TARGET_DIR=/home/donoughliu/code/lightmount3/target/related-agent-memory-release-847448b844 \
  LIGHTMOUNT_RELATED_AGENT_MEMORY_ITERATIONS=120 \
  LIGHTMOUNT_RELATED_AGENT_MEMORY_PEER_NAVIGATION_EVERY=12 \
  LIGHTMOUNT_RELATED_AGENT_MEMORY_PAYLOAD_OBJECTS=24000 \
  LIGHTMOUNT_RELATED_AGENT_MEMORY_DOM_NODES=4000 \
  LIGHTMOUNT_RELATED_AGENT_MEMORY_PROMISES=1000 \
  LIGHTMOUNT_RELATED_AGENT_MEMORY_OUTPUT=/home/donoughliu/code/lightmount3/target/related-agent-memory/847448b844-run1.json \
  LIGHTMOUNT_RELATED_AGENT_MEMORY_COMMIT=847448b8447f0d226567394d2e878265d3d0cafe \
  cargo nextest run -p lightmount-renderer-v8 --release --run-ignored only \
    -E 'test(/runtime::tests::related_page_script_agent_release_memory_acceptance$/)'
```

最终 repository gate：

```bash
cargo nextest run -p lightmount-renderer-v8 \
  -E 'test(/runtime::tests::related_page_script_agent_/)'
# 8 passed

cargo nextest run --no-fail-fast
# 15634 passed, 18 skipped

cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
# passed
```

按本文定义，Phase 2B 的 selective related-page shared-agent 可行性实验至此完成：功能、
Page-local 路由、non-LIFO ownership、realm retirement、isolate lifetime 和两次 release
长序列内存门均有证据。这个结论只授权 Phase 3 打开显式 relationship admission，不授权
把 owner-wide sharing 恢复为默认，也不表示 popup 已完成。Phase 3 第一纵切已经提升
related admission 和 Page reservation；第三纵切 A 又把 WindowProxy cross-realm link
提升到 production 的 opener-preserving popup 路径。production 的单一 initial Document
owner、multi-session Inspector event 顺序、
SharedWorker/ServiceWorker、跨 origin agent split 和真实 popup 并发 close/navigation 仍分别
属于 Phase 3-5。

### Phase 3：真实 auxiliary initial empty Page

- 在 renderer owner runtime 中创建 `PendingAuxiliaryPage`；
- 为它创建 stable main WindowProxy、独立 V8 Context 和 initial empty Document；
- `window.open('about:blank')` 返回这份真实 proxy；
- protocol target bind/adopt 同一个 `RendererPageResidenceIdentity`；
- immediate `document.write`、storage clone 和 Runtime evaluate 指向同一个 Document；
- close 从第一天就走统一 transaction。

优先只切 `about:blank` 垂直路径，因为它能验证最关键的同步创建、realm identity 和
target adoption，又不先引入网络导航。

实施状态（Phase 3 第一纵切：identity reservation / initial target adoption）：

- renderer Page script environment 现在持有一个不反向拥有 Page/VM 的窄 allocator；
  `window.open()` 或 hyperlink 确认新建 lightweight auxiliary context 时，同步产生
  `RendererPendingAuxiliaryPage`。它把 typed `AuxiliaryTopLevel` browsing-context id 与
  exact `RendererPageReservationToken` 绑定在一个不可拆错的 carrier 中；
- opener 可见时 reservation 显式携带 `RelatedAuxiliaryPage { opener_page_id }`，
  `noopener` / `noreferrer` 携带 `Fresh`。普通 Page 创建、`Target.createTarget` 和
  renderer owner 内其他 Page 不会隐式加入共享 agent；
- popup activation 将该 carrier 原样交给 protocol。新 target 的 `TargetPageSlot` 长期
  保留 auxiliary browsing-context id，并在 initial empty Document build 时一次性消费
  renderer Page reservation；protocol 不再为这条路径制造第二个 initial Page id；
- initial build 不再仅凭 BrowserContext metadata 选择 NavigationEngine。它会从当前与
  retained background engine 中找到能消费 exact reservation 的 opener renderer owner，
  再创建共享该 owner 的 engine wrapper；找不到 owner 时 fail closed，不放宽 token 校验。
  这保证 active、inactive background、BiDi user-context 和轻量测试 fixture 都不会把
  renderer 已接受的 auxiliary Page 偷换到另一个 owner；
- named lightweight target reuse 不产生第二份 reservation；尚未 materialize lightweight
  context 的 fallback action、browser-context action 和 service-worker action 仍走旧路径；
- production related Page 使用已验收的 script-agent router/membership。初始
  `about:blank` 集成回归证明 opener 与 popup 有不同 Page/Context/realm，但报告同一个
  `scriptAgentId` 和一个 live document isolate；`noopener` popup 采用不同 agent；
- `HeapProfiler.lightmountDiagnostics` 改为从 Page snapshot 汇总唯一 `scriptAgentId`，
  不再把 loaded Page 数机械等同于 V8 isolate 数。V8 heap、GC、Inspector default-context
  registry 和 foreground wake 的诊断 scope 相应标为 script-agent，而 target-document
  计数仍保持 Page/Document-local；
- shared-agent Inspector pause bridge 已从单一 target route 改为按
  `RendererDevToolsAgentToken -> Page output journal` 路由。关闭或替换 popup Page 只撤销
  该 Page 的 route、pause session 和 queued command，不会永久关闭 opener target；nested
  pause loop 也按 agent token 选择 V8 Inspector session，而不是假定一个 isolate 只有一个
  context group。Classic WebDriver 命名 popup 的“创建、切换、导航复用、再回到 opener
  click”路径覆盖了这个 lifetime。没有 concrete Page pause route 的低层 Inspector binding
  仍可把普通通知留在 agent-local queue；只有 `Debugger.paused` 必须有精确 Page route，
  防止共享 bridge 的 route 存在性误伤 replacement/overlap teardown；
- owner handoff 与 Inspector lifetime 由 initial adoption、inactive-background CDP、BiDi
  viewport inheritance、Classic named-popup reuse、replacement/overlap binding teardown 及
  `closing_related_page_route_keeps_opener_target_routable` 联合覆盖。

本纵切完成时的实跑证据：

```bash
cargo nextest run -p lightmount-renderer-v8 \
  -E 'test(/script_vm::inspector_pause::tests/) | test(/script_vm::inspector::tests::replacement_document_binding_does_not_adopt_previous_agent_outbound/) | test(/script_vm::inspector::tests::dropping_overlapping_peer_binding_does_not_deactivate_current_agent/) | test(/script_vm::tests::window_execution_context::strict_window_binding_resolves_registry_policy_and_rejects_retired_realm/) | test(/runtime::tests::related_page_script_agent/) | test(per_page_isolate_policy_keeps_window_open_routes_page_owned)' \
  --no-fail-fast
# 20 passed

cargo nextest run -p lightmount-protocol --no-fail-fast
# 3233 passed

cargo nextest run -p lightmount \
  -E 'test(websocket_bidi_set_viewport_user_context_inherits_through_window_open) | test(webdriver_classic_named_popup_reuse_navigates_existing_window)'
# 2 passed

cargo nextest run --no-fail-fast --status-level fail --final-status-level fail
# 15635 passed, 18 skipped
```

这一纵切只完成“renderer 先保留身份、protocol initial `about:blank` 接管”的不变量。
它尚未完成 Phase 3：`window.open()` 返回的仍是 opener PageVM 内 lightweight proxy，因而
opener immediate `document.write` 与 CDP target 仍不是同一个 Document；现有 protocol
cross-document `Page.navigate` 仍会分配 fresh Page/agent。

#### Phase 3 第二纵切 A：live Page replacement prepare 基础

本提交先收窄 prepared document 进入稳定 Page replacement path 前必须成立的 ownership
边界，没有提前改 protocol install：

- `RendererPageHandle` 通过 renderer owner command 异步预留 replacement Document；token
  保留原 Page id / owner-local host，并携带当前已提交 `PageVm` creation id 与唯一 nonce；
- owner-local store 只保留同一 Page 最新的未消费 reservation。旧 nonce 在 isolate
  bootstrap 前以 `superseded` 失败；Page 已发生其它 cross-document commit 时，旧
  generation 也在 bootstrap 前 fail closed；
- prepare 不再为该 token 创建 fresh/related isolate 或第二套 Page task sources，而是从
  stable Page slot 取得 `RendererPageScriptEnvironment`，调用既有
  `bootstrap_replacement_document_isolate()`。因此 reservation 已明确复用 script agent、
  isolate、agent membership、Page task producer routes、output journal，并声明复用 stable
  main WindowProxy；
- isolate reservation 现在区分“initial creation 自己拥有 output stream”和“replacement
  只借用 live Page stream”。replacement cancel、stale failure 或当前尚未开放的 commit
  拒绝只释放 prepared residence，不能发送 live stream `Closed`，也不能改变 isolate
  created/live/destroyed accounting；
- 在 replacement install/handle ownership 尚未接入前，`commit()` 显式报错并同步取消
  residence，避免误入只允许新 Page 的 `attach_page_entry_for_owner`，更不能靠同 Page id
  碰撞偶然失败。

聚焦回归同时覆盖了旧 prepared-document 自有 stream 的取消语义，防止修复 replacement
时把 initial creation 的 stream lifetime 放宽：

```bash
cargo nextest run -p lightmount-renderer-v8 \
  -E 'test(/runtime::tests::(canceled_prepared_document_closes_its_ordered_output_stream|prepared_external_raw_document_waits_for_matching_commit_permit|per_page_isolate_policy_reuses_navigation_isolate_and_replaces_contexts|related_page_script_agent_experiment_shares_isolate_and_survives_source_close|canceling_prepared_live_page_replacement_preserves_page_environment_and_output_stream|stale_live_page_replacement_reservation_fails_before_isolate_bootstrap|newer_live_page_replacement_reservation_supersedes_unconsumed_nonce)$/)' \
  --no-fail-fast
# 7 passed

cargo nextest run --no-fail-fast
# 15638 passed, 18 skipped

cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
# passed
```

#### Phase 3 第二纵切 B：stable Page replacement commit / core adoption

本提交已经完成上一节要求的 replacement commit 边界，但尚未改 protocol：

- `PreparedRendererDocument::commit_page_replacement()` 使用独立 owner command；只有
  `ExistingPageReplacement` admission 可以进入。初始 Page prepared commit 与 replacement
  commit 类型上分开，误用时释放 prepared residence 并 fail closed；
- commit 在拆旧 realm 前再次比对 reservation 记录的 `PageVm` creation id、stable slot
  generation 和当前 resident generation。prepare 后若发生另一场 cross-document commit，
  stale prepared Document 不能覆盖新 Document；
- 旧 Document lifecycle 以 `SupersededByCrossDocumentNavigation` 结束，旧 default Inspector
  context 和 Page-context resources 在新 realm bootstrap 前撤销；新 PageVm 复用原 script
  agent、isolate、agent membership、typed Page task routes、output journal 和 stable main
  WindowProxy；JavaScript dialog broker 与 Inspector pause bridge 属于 Document-scoped
  adoption artifact，由新 PageVm 重新产生并在 stable handle 上替换；
- streaming raw 与 NativeDom 两条 prepared bootstrap 都把结果直接安装到 checked-out 的原
  `RendererPageLocalEntry`。phase-one residence、pending location navigation 和 post-parse
  lifecycle 都沿用现有 live Page continuation，不进入 initial Page attach；
- replacement publication 使用同一 Page id，推进 `vm_creation_id` 和 `view_generation`，并
  通过 typed replacement-settled wake 解锁等待 committed view 的命令。response metadata
  同时更新原 stable `RendererPageState`，而不是保留旧 status/headers/initiator；
- `DocumentCommit` reply 可以在 streaming response 尚未结束时返回 non-owning replacement
  result，后台继续同一 phase-one/lifecycle。它保留 `ReturnWithPendingNavigation` policy，
  protocol-owned script navigation 不会被 standalone adapter 偷走；
- `RendererPageReplacementCommit` 只含 Page identity、新 Document DevTools agent token、
  新 Document dialog broker / pause bridge、Page state、creation diagnostics/artifacts 和可选
  download，不含 `RendererPageHandle`、Page cancel sender 或第二份 close authority；
- core `PreparedDocumentPage::commit_page_replacement(..., &mut Page)` 在 renderer commit 前校验
  exact owner/Page identity，并让原 `Page`/`RendererPageHandle` 显式采纳新 agent token 和
  state。原 handle 仍是唯一 command/close owner，已有 renderer-agent attachment id 不被
  偷换。

聚焦回归覆盖 stable identity、realm retirement、commit-time stale generation、open stream
DocumentCommit、NativeDom 和 core adoption：

```bash
cargo nextest run -p lightmount-renderer-v8 \
  -E 'test(/runtime::tests::(prepared_live_page_replacement_commits_in_stable_page_slot_without_new_handle|prepared_live_page_replacement_document_commit_replies_before_stream_completion|prepared_live_page_replacement_document_commit_preserves_browser_owned_tail_navigation|prepared_native_dom_live_page_replacement_uses_the_stable_page_slot|prepared_live_page_replacement_revalidates_generation_at_commit|canceling_prepared_live_page_replacement_preserves_page_environment_and_output_stream|stale_live_page_replacement_reservation_fails_before_isolate_bootstrap|newer_live_page_replacement_reservation_supersedes_unconsumed_nonce)$/)' \
  --no-fail-fast
# 8 passed

cargo nextest run -p lightmount-core \
  -E 'test(runtime::navigation_engine::tests::core_page_adopts_prepared_renderer_replacement_without_replacing_ownership)' \
  --no-fail-fast
# 1 passed

cargo nextest run --no-fail-fast
# 15644 passed, 18 skipped

cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
# passed
```

第二纵切 B 单独完成时仍不是 Phase 3 完成标志；它要求的 protocol 切换由下面的第二纵切 C
完成。

#### Phase 3 第二纵切 C：protocol stable Page navigation

本提交把已有 top-level Page 的 cross-document navigation 从“创建 fresh Page，再替换 target
slot”切换为“在同一 stable Page residence 中替换 Document”。这包括 `Page.navigate` 与
target-owned navigation，覆盖 active target、同 BrowserContext background target 和 inactive
background target：

- navigation detached work 启动前，同时捕获 exact `TargetPageResidenceIdentity` 与
  `RendererPageResidenceIdentity`，并从当前 core `Page` 取得 non-owning replacement
  reservation。后台 future 只持 reservation capability，不复制 `RendererPageHandle` 或 close
  authority；
- stable commit 保留同一个 core `Page` handle、renderer `PageId`、target Page residence
  generation 与 `TargetPageAttachmentId`。新 Document 改用新的 DevTools agent token、
  renderer-agent attachment、default realm、execution context 和 context group；旧 realm 与
  attachment 按 replacement 顺序退休；
- inline HTML、`data:`、普通网络 streaming response，以及 Fetch response-stage 的 buffered /
  captured response 都携带同一个 stable target carrier。Fetch pause 后才确定的 commit
  configuration 会先写回 prepared Document，再进入 renderer commit，避免 interception
  旁路重新分配 Page；
- stable reservation 不能只按 BrowserContext 或“最近一个 engine”选择 renderer owner。
  navigation 会比对 captured `RendererOwnerLocalHostId`，必要时从当前或 retained background
  engine 中取出 exact owner 的 `NavigationEngine`；找不到时在 prepare 前 fail closed。这修复
  了同 BrowserContext 内 background target 导航误用 active target renderer owner 的路径；
- renderer commit settlement 现在发布新 Document 的初始 output，建立 predecessor fence，
  再把 creation artifacts、lifecycle binding、main-resource body 和 navigation engine 交给同一
  target owner。protocol 不再因为 Page identity 稳定而漏掉 execution-context / console /
  lifecycle 可观察输出；
- renderer-agent candidate 先作为 transaction 准备，renderer Page commit 成功后再把 stable
  target 的 DevTools channel、全部 frontend session call route 和 Page state 切到新
  attachment。Document-scoped JavaScript dialog broker、Inspector pause bridge 与 dialog scope
  同步轮换，旧 Document 的 pending dialog 不会泄漏到新 Document；
- `pending_live_page_replacement_reservations` 只负责 prepare admission，另用 latest
  reservation 记录保持 commit-time ordering。于是两个已经 prepare 的并发 candidate 中，后
  reservation 可以提交，旧 candidate 以 `PagePreserved` 失败，不能覆盖新 Document 或关闭
  stable Page；
- replacement error 明确携带 `PagePreserved` / `PageRetired` disposition。pre-commit identity、
  nonce 或 candidate mismatch 会回滚 renderer channel 并保留旧 Page；一旦旧 realm 已退休，
  后续 materialization / protocol commit 失败就 fail closed 丢弃当前 Page，不能伪装成可回滚；
- 已从 scheduler registry claim、正在等待 Promise 的 Inspector command 也属于旧 Document。
  replacement 会遍历该 target 的 primary / auxiliary session，给这些 await 精确发送一次
  `Inspected target navigated or closed`，避免命令永久挂起；
- related popup 继续遵守 selective shared-agent 结论：opener 与 popup 在同一个 document
  isolate 中运行，但保持两个 Page realm / execution context，而不是把“两个 Document”误报成
  “两个 isolate”；
- `Target.createTarget` 的 background initial load 之后可能立即进入完整 Page navigation。
  该 target-to-Page future 边界显式 boxed，避免 test thread 同时保留 initial build、response
  plan 与 navigation state machine 导致确定性 stack overflow；这里没有加入 sleep、retry 或
  调大线程栈来掩盖问题。

本纵切的聚焦与 crate 级证据包括：

```bash
cargo nextest run -p lightmount-renderer-v8 \
  -E 'test(newer_live_page_replacement_supersedes_prepared_candidate_without_retiring_page)' \
  --no-fail-fast
# 1 passed

cargo nextest run -p lightmount-protocol \
  -E 'test(cross_document_page_navigate_replaces_realm_in_stable_page_residence) | test(interleaved_response_heads_only_commit_the_current_prepared_document) | test(runtime_evaluate_await_promise_pending_is_terminated_once_by_navigation_replacement) | test(same_context_background_session_can_stage_its_own_locale_and_timezone_before_promotion)' \
  --no-fail-fast
# 4 passed

cargo nextest run -p lightmount-protocol \
  -E 'test(local_storage_mutations_fan_out_across_targets_without_leaking_session_storage)' \
  --no-fail-fast
# default test stack 下连续 3 次通过

cargo nextest run -p lightmount-protocol --no-fail-fast
# 3234 passed

cargo nextest run --no-fail-fast --status-level fail --final-status-level fail
# 15646 passed, 18 skipped
```

第二纵切 C 单独完成时仍不是 Phase 3 完成标志：它只统一了已经存在的 Page 内的
cross-document replacement，还没有改变 `window.open()` 同步返回的 lightweight
WindowProxy/Document。下面的第三纵切 A 先统一 proxy identity；initial Document owner 和
mirrored load 仍是后续边界。

#### Phase 3 第三纵切 A：opener-visible stable WindowProxy handoff

本提交把已经在 Phase 2B 验收的 related-page 跨 realm WindowProxy 能力接入 production
popup creation，但刻意不把 initial Document mirror 误报为完成：

- `window.open()` / hyperlink 在确认需要新建 lightweight browsing context 后，先从 opener
  的 `RendererPageScriptEnvironment` 预留 exact related auxiliary Page。named target reuse
  不再分配 Page，也不会覆盖已有 handoff；
- opener-preserving 路径不再创建只能留在 opener realm 的 synthetic Window wrapper。它用
  normal Window global template 创建一个由临时 V8 Context 持有的真实 global proxy，立即
  安装现有同步 popup surface，并把这同一个对象返回给 author script。临时 facade 也初始化
  eager intrinsic interface registry；否则第二个命令里重新物化 `HTMLDocument` 等 wrapper 时
  会因为 facade realm 没有 prototype registry 而终止进程。只要 lightweight mirrored loader
  尚未删除，facade 还必须拥有独立 runtime-observable context token，并从 facade realm 内安装
  popup id / opener private slots；这样 handoff 前的 response script 仍能观察 creator，但不把
  Phase 5 尚未实现的 target opener graph 伪装成已完成；
- V8 handle 不进入可跨 renderer/protocol transport 的 `RendererPendingAuxiliaryPage`。opener
  Page 的窄 allocator 持有 owner-local registry，以 reserved target `PageId` 为 key 暂存
  `WindowProxy + facade Context + optional creator security token`；registry 不反向持有 Page、
  PageVM 或 protocol target，因此不会形成 ownership cycle；
- owner-local store 消费 `RelatedAuxiliaryPage { opener_page_id }` 时，必须先找到 exact live
  source Page environment，再一次性取走对应 proxy。目标 `RendererPageScriptEnvironment`
  在首个 realm bootstrap 前登记它，`ScriptVmContextBootstrap` detach 临时 facade，并把 exact
  proxy 作为 `ContextOptions::global_object` 交给真实 auxiliary default Context；没有 alias、
  proxy 状态复制或等待 protocol 的同步补丁；
- initial `about:blank` 对普通 origin 可以重新计算相同 internalized token，但 opaque origin
  与 `document.domain` mutation 使用 V8 unique token。handoff 因此额外只消费一次 creator
  token，保证 inherited initial realm 可由 opener 同步访问；后续 cross-document replacement
  不再复用该 token，而是按新 Document origin 正常计算；
- `noopener` / `noreferrer` reservation 仍是 `Fresh` agent，调用方返回 `null`，不会尝试把
  V8 object 搬到另一个 isolate；ServiceWorker `clients.openWindow` / notification fallback
  也显式保持无 handoff 的旧 lightweight 路径；
- facade inner global 退役后，旧的 lightweight popup private marker 不一定还能从 opener
  realm 直接读取。Classic WebDriver 的 Window reference adapter 因此先走 marker 快路径，
  再由 opener host 按其仍持有的 `LightweightPopupBrowsingContextRecord.window_proxy` exact
  identity 回查 popup id；这不会给 target realm 重新安装 opener-local popup marker，也不会
  把 target 的正常 top-level Window 行为重新路由回 lightweight owner；
- protocol 端到端回归不是比较 metadata：popup session 在真实 auxiliary realm 写入 global
  与 `document.body`，opener 保存的 handle 必须读取同一值；opener 再经该 handle 写入，popup
  session 必须反向观察到。这证明双方使用同一个 stable proxy/inner realm projection，而不是
  两个 proxy 的 mirror synchronization。

本纵切当前的聚焦证据：

```bash
cargo nextest run -p lightmount-renderer-v8 \
  -E 'test(per_page_isolate_policy_keeps_window_open_routes_page_owned) | test(related_page_script_agent_transfers_stable_window_proxy_objects_and_dom_wrappers)' \
  --no-fail-fast
# 2 passed

cargo nextest run -p lightmount-protocol \
  opener_window_handle_projects_the_renderer_owned_auxiliary_realm \
  --no-fail-fast
# 1 passed

cargo nextest run -p lightmount-protocol \
  window_open_hands_off_session_storage_snapshot_and_initial_storage_key \
  --no-fail-fast
# 1 passed；现阶段仍明确验证 mirrored loader 的双 owner characterization

cargo nextest run -p lightmount-protocol \
  window_open_named_target_reused_in_same_command_emits_one_page_event \
  --no-fail-fast
# 1 passed；覆盖 facade intrinsic registry 与 named reuse

cargo nextest run -p lightmount \
  webdriver_classic_execute_script_round_trips_window_and_frame_references \
  --no-fail-fast --stress-count 5
# 5/5 passed；覆盖 handoff 后 popup Window reference identity 回查

cargo nextest run -p lightmount-renderer-v8 \
  owner_scheduler_applies_popup_terminal_from_stable_page_route \
  --no-fail-fast --stress-count 5
# 5/5 passed；覆盖 facade token、opener projection 与 mirrored terminal application

cargo nextest run --no-fail-fast
# 15833 passed, 18 skipped
```

这仍不是 Phase 3 完成标志。同步调用期间 `popup.document` 仍由 opener PageVM 的
`LightweightPopupDocumentRecord` 提供；protocol 接管后 stable proxy 已投射到真实 auxiliary
realm，但此前的 DOM mutation 还不会成为 target initial Document。lightweight record 也仍
拥有 Document/navigation/close 状态，并对 non-empty URL 发起 mirrored load。下一纵切必须让
真实 auxiliary Page environment 从同步创建起就拥有唯一 initial realm/Document，删除对应
lightweight Document owner；之后 Phase 4 才能把 pending non-empty URL 收敛为一次
authoritative navigation。

#### Phase 3 第三纵切 B：in-scope main realm prebootstrap 基础

本提交先解决上一节下一步的 reentrancy 前置条件，尚未改变 popup production ownership：

- `window.open()` native callback 执行时，opener `ScriptVm` 已通过 document-isolate holder
  持有 `OwnedIsolate` 的可变借用。此时再次调用 `PageVm::new()` 会重入同一个 `RefCell`；临时
  释放借用、重入 owner loop 或从裸 isolate pointer 再造 scope 都不满足本文的不变量；
- main default realm 现在和 child default realm 一样有显式 in-scope primitive。
  `ScriptVmContextBootstrap::new_main_default_in_scope()` 接受调用方已经进入的
  `PinScope` 与 isolate global template，在同一 scope 内创建真实 V8 `Context`、安装 stable
  main WindowProxy、native bridge、runtime token 和完整 Window surface；
- `ScriptVmPageRealmBootstrap` 把这一步产出为
  `ScriptVmPreinspectorDefaultWorldBootstrap`。在该边界，独立 `DocumentRuntime` /
  `JsContextHost`、main Document resource authority、Window execution-context registration
  和 baseline globals 已经就绪，author script 可以同步访问该 realm；但 Inspector default
  context 尚未发布；
- callback/outer scope 退出后，`materialize_default_inspector_context()` 才借用 isolate-level
  Inspector backend，把同一个 Context 注册到对应 Page binding。它不创建第二个 Context、
  不 detach/reattach WindowProxy，也不重建或复制 Document；
- 普通 Page、replacement Document 和现有 related auxiliary target 的创建已经全部经过这份
  两段式实现，只是立即连续执行两段，因此现有 production event/ownership surface 不需要
  旁路；下一提交可以在两段之间暂存 renderer-owned auxiliary Page realm；
- 新回归在持有 shared isolate owner borrow 且已经进入模拟 opener Context 的情况下直接调用
  in-scope prebootstrap。Inspector registry 必须仍为 `0`；随后在预创建 realm 中保存
  `document` / `Array` identity 并写入 `document.body`，后置 materialization 后 registry 必须
  精确变为 `1`，且两个 identity 与 DOM mutation 全部保持。

聚焦证据：

```bash
cargo nextest run -p lightmount-renderer-v8 \
  main_default_realm_prebootstrap_preserves_window_and_document_until_inspector_materialization \
  --no-fail-fast
# 1 passed

cargo nextest run -p lightmount-renderer-v8 \
  -E 'test(main_default_realm_prebootstrap_preserves_window_and_document_until_inspector_materialization) | test(per_page_isolate_policy_keeps_window_open_routes_page_owned) | test(related_page_script_agent_transfers_stable_window_proxy_objects_and_dom_wrappers) | test(/script_vm::inspector_pause::tests/) | test(/script_vm::inspector::tests::replacement_document_binding_does_not_adopt_previous_agent_outbound/) | test(/script_vm::inspector::tests::dropping_overlapping_peer_binding_does_not_deactivate_current_agent/)' \
  --no-fail-fast
# 16 passed

cargo nextest run -p lightmount-protocol \
  -E 'test(opener_window_handle_projects_the_renderer_owned_auxiliary_realm) | test(window_open_hands_off_session_storage_snapshot_and_initial_storage_key) | test(window_open_named_target_reused_in_same_command_emits_one_page_event)' \
  --no-fail-fast
# 3 passed

cargo nextest run --no-fail-fast
# 15834 passed, 18 skipped
```

这仍不是 Phase 3 完成标志：`window.open()` 尚未创建
`ScriptVmPreinspectorDefaultWorldBootstrap`，protocol initial build 也尚未消费这份 staged
realm/Page residence。下一纵切应让 related auxiliary reservation 同步准备独立 DomHost、
Page task routes、resource/storage authority 和这份 in-scope realm，再由 protocol target 只做
Inspector/target adoption；不能把 lightweight DOM replay 到新 Page，也不能重新创建一份
initial Document。

#### Phase 3 第三纵切 C：in-scope related-agent admission 基础

这一提交继续收窄 native callback 内剩余的 isolate holder 重入点，仍不改变 production
popup 的 Document owner：

- `RendererScriptAgentPageMembership` 现在是 admission authority。只有仍 active 的 source
  Page membership 能为一个明确的 target Page route 调用 `admit_related_page()`；调用方不再
  为了取得 holder 内的 router 而借用 document isolate。普通 owner-lane related Page build
  也改走同一能力，避免同步路径与既有异步路径形成两套 admission 规则；
- `RendererDocumentIsolateBootstrap` 和稳定 `RendererPageScriptEnvironment` 缓存同一份
  `RendererInspectorIsolateBackendHandle`。创建 target Page binding 不需要在 callback 栈上
  重新进入 holder 读取 backend；handle 仍不暴露任何 V8/Inspector mutation authority；
- `NativeBridgeBindings::build_peer_in_scope()` 只复用 source isolate 的 Window global /
  cross-origin Window global templates，并在 caller 已进入的 `PinScope` 内重建独立 bridge、
  wrapper templates 和 cache。target `JsContextHost` 不会共享 opener 的 mutable bridge state；
- `RendererPageScriptEnvironment::bootstrap_related_page_document_isolate_in_scope()` 把上述两份
  capability 合并成 callback-safe bootstrap。失败或尚未 adoption 时，RAII membership 会撤销
  target Page route，不会给 shared script agent 留下幽灵成员；
- 回归在 holder 已持有 mutable borrow 且 opener Context 已进入时执行这份 admission，再用
  新 bindings 创建独立 target Context。它同时验证 exact isolate identity、target Page id、
  page-count `1 -> 2 -> 1` 和未 adoption bootstrap 的 rollback；旧实现会在读取 router 或
  Inspector backend 时直接触发 `RefCell` 重入。

聚焦证据：

```bash
cargo nextest run -p lightmount-renderer-v8 \
  related_page_isolate_admission_builds_peer_bindings_inside_entered_opener_scope \
  --no-fail-fast
# 1 passed

cargo nextest run -p lightmount-renderer-v8 \
  -E 'test(related_page_isolate_admission_builds_peer_bindings_inside_entered_opener_scope) | test(main_default_realm_prebootstrap_preserves_window_and_document_until_inspector_materialization) | test(per_page_isolate_policy_keeps_window_open_routes_page_owned) | test(related_page_script_agent_experiment_shares_isolate_and_survives_source_close) | test(related_page_script_agent_transfers_stable_window_proxy_objects_and_dom_wrappers)' \
  --no-fail-fast
# 5 passed

cargo nextest run -p lightmount-protocol \
  -E 'test(opener_window_handle_projects_the_renderer_owned_auxiliary_realm) | test(window_open_hands_off_session_storage_snapshot_and_initial_storage_key) | test(window_open_named_target_reused_in_same_command_emits_one_page_event)' \
  --no-fail-fast
# 3 passed

cargo nextest run --no-fail-fast
# 15835 passed, 18 skipped

cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
# both passed
```

这仍不是 Phase 3 完成标志。in-scope capability 目前只在窄回归中直接调用；production
`window.open()` 仍先创建 facade，owner 后续才创建 target `DomHost` / Page task residence。
下一提交应在 related auxiliary reservation 上同步创建并暂存 exact task sources、resource /
storage authority 和 `ScriptVmPreinspectorDefaultWorldBootstrap`，然后让现有 initial Page build
消费它并仅 materialize Inspector/target ownership。不能为消除 facade 而把 lightweight DOM
内容 replay 到第二份 Document。

#### Phase 3 第三纵切 D：synchronous Page residence / exact initial Document adoption

本提交完成了上一节定义的 production 纵切，但有意把准入范围限制在最能证明 identity 与
ownership 的 initial-empty 路径：

- 必须由 live opener Page 产生 `RelatedAuxiliaryPage` reservation；
- `noopener` / `noreferrer` 不进入，因为它们是 `Fresh` agent 且调用方也不应取得同步
  Window reference；
- target 必须是空字符串或 `_blank` 等非命名 target。可追踪 name 仍留给 group-level named
  registry 纵切，不能先创建一份无法复用的真实 Page；
- URL 必须是 `about:blank`，允许 fragment。non-empty URL 仍由 Phase 4 负责唯一 authoritative
  navigation，不能在本提交中继续保留 mirrored request 又声称 owner 已统一。

同步 callback 内的新拓扑是：

```text
opener PageVm / shared related script agent
    |
    | window.open("about:blank", "_blank")
    v
owner-local staged auxiliary PageVm
    +-- stable WindowProxy（立即返回给 opener）
    +-- independent V8 Context / inner Window
    +-- unique DomHost / initial Document
    +-- Page task sources / lifecycle / resource authority / storage authority
    +-- unopened Inspector binding + Page output journal
    |
    | exact RendererPageReservationToken
    v
protocol popup target
    +-- adopt 上述同一 PageVm / Context / Document
    +-- 只补 frame、session、Inspector 与 target configuration
```

具体 ownership 与同步语义如下：

- `open_lightweight_popup_window()` 在 legacy record 创建之前识别上述准入条件。它捕获 creator
  base URL、policy container、document referrer、inherited origin、request client、local /
  session storage、top-level storage key、IndexedDB 与 bucket authority；随后创建 detachable
  stable WindowProxy shell 和 canonical initial HTML DomHost；
- inherited origin 不再从 `about:blank` URL 重新算成 `"null"`。创建输入在 realm bootstrap 前
  安装到 root `DocumentRuntime` policy container 和 `DocumentFetchContext`，Window 的 runtime
  origin slot 也在 target Context 内同步覆盖。`Location.origin` 对 `about:blank` 读取该 inherited
  runtime origin；root `Document.referrer` 直接读取同一 policy container；default runtime realm
  inventory 也读取 Document resource authority，而不再从 URL 重算 origin。HTTP opener 回归同时
  验证 `window.origin`、`location.origin`、`document.referrer`、fallback `baseURI` 和 target
  `Runtime.executionContextCreated.origin`；
- `_blank` 是选择关键字，不是 browsing-context name。真实 realm 的 `window.name` 因此初始化为
  空字符串；不能把传入的 `_blank` 写进 target Window；
- source `JsContextHost` 不再保存对应 `LightweightPopupBrowsingContextRecord`。否则 opener host
  会通过 record 中的 `Global<WindowProxy>` 反向强持有 target realm，形成第二 owner，并使 target
  close 后的 realm containment 无法证明。protocol handoff 需要的 session-storage snapshot 与
  initial storage key 改为 `OpenedLightweightPopup` 上的一次性 carrier；
- Classic WebDriver 仍需把 opener 返回的 WindowProxy 编码成随后创建的 window handle。真实
  target Window 因此只带一个独立的 V8 private auxiliary-popup identity；host serializer 可以
  识别它，但 author script 无法伪造。这个 marker 不提供 Document、navigation、timer 或 close
  ownership，也不会让现有 lightweight API 把真实 Window 重新路由到 opener host；
- owner-local store 以 `(RendererOwnerLocalHostId, PageId)` 暂存完整 `PageVm`，而不是暂存可被
  replay 的 DOM snapshot。它在已经进入的 opener isolate scope 内创建 exact Page task source、
  typed producer routes、output stream、Page Inspector binding、related-agent membership、peer
  native bindings 和 pre-Inspector default realm；
- callback 内不能重新借用同一 document-isolate holder。无 restore session 时 bootstrap 不再
  为一次空 reattach 进入 holder；IndexedDB / bucket 初始 backend 也直接写入当前 target Context
  与 host。所有需要 V8 scope 的初始化都在 caller scope 完成，退出 callback 后才允许普通
  owner command 再进入 holder；
- staged initial Document 同步设为 `readyState=complete`，并用 typed lifecycle transition 记录
  DCL/load 已达成。后续空 lifecycle turn 识别“里程碑已完成且没有 work”并返回 Idle，不依靠
  sleep、retry、任意 drain 或重复 dispatch 修正状态；
- isolate reservation 在 `PageVm` construction 的异常区间暂时 disarm，避免失败析构递归借用
  当前 bound owner-local store；成功后 rearm，交回正常 Page lifetime。owner-local store 析构
  会先 drain staged Page，再显式撤销 reservation，避免 source Page 先析构时留下 related-agent
  membership 或 Inspector route。

protocol initial target build 现在是 adoption，而不是第二次 bootstrap：

- initial request 必须仍是匹配的 `about:blank`；owner command 用 exact reservation 一次性取走
  staged `PageVm`。protocol 为旧 creation path 预留的 service-worker client 被释放，保留同步
  Page 已经创建的 client；
- target 提供 root frame id、main-document commit、Inspector session restore、isolated worlds、
  bindings 和最终环境配置。adoption 把 lifecycle journal 绑定到 Page output stream，并在同一
  V8 Context 上 materialize Inspector；它不创建 Context、WindowProxy、Document 或 DomHost；
- opener 在 target activation 前已经可以改 DOM、设置 global、访问 storage 或产生 renderer
  observation。output journal 暂存尚未发布的 author records，adoption 先插入
  `executionContextsCleared -> frame commit -> contextCreated` 前缀，再原序追加这些 records；一旦
  stream prefix 已发布就 fail closed，不能用乱序事件掩盖 ownership 错误；
- adoption 只替换 protocol 提供的 request transport / network policy 和 Page-level runtime
  configuration，随后直接进入现有 Page creation phase two。环境应用模式显式区分普通创建、
  staged 创建和 staged adoption：adoption 不得用 synthetic `about:blank` response 的空 CSP、
  referrer policy、COEP 或 Document-Isolation Policy 覆盖 creator-derived policy container；
  sandbox 导出的 script-disable 只能保持或收紧，不能被 target 默认配置放宽（显式 CDP
  `Page.setBypassCSP` 仍是独立的调试器能力）。同步保存的 `Document` object、body mutation、
  global lexical/realm state、WindowProxy 和 Page id 全部保持原对象。

核心回归不再只比较 metadata：

- `opener_window_handle_projects_the_renderer_owned_auxiliary_realm` 在 opener 的同一次
  `Runtime.evaluate` 中保存 exact `popup.document`，写入 target global 与 body，并检查 name、
  origin、referrer 和 base URL；attach target 后验证 `window.opener` 保存的 Document 就是当前
  `document`，同步 realm/global/DOM 状态全部存活；target 再修改 DOM/global，opener 必须经原
  WindowProxy 和原 Document 看到变化，反向 proxy mutation 也必须成立；
- `window_open_hands_off_session_storage_snapshot_and_initial_storage_key` 使用 HTTP opener，证明
  非 opaque creator origin、referrer、base URL、localStorage 共享和 sessionStorage clone 在
  target adoption 前后保持一致；它也在同步 WindowProxy 与 attach 后的 exact target realm 两侧
  验证 creator response CSP 继续拒绝 `eval`。后者显式设置 CDP
  `allowUnsafeEvalBlockedByCSP=false`，避免把调试器默认的临时 CSP 豁免误判为 policy 丢失；
- Classic WebDriver round-trip 覆盖多个顺序不同的 `about:blank#fragment` popup。真实 proxy 的
  private target identity 必须稳定映射到 window handle，重复引用不能退化成循环对象 clone。

本纵切的聚焦与 repository gate 实跑证据：

```bash
cargo nextest run -p lightmount-protocol \
  -E 'test(opener_window_handle_projects_the_renderer_owned_auxiliary_realm) | test(window_open_hands_off_session_storage_snapshot_and_initial_storage_key) | test(popup_initial_empty_document_frame_tree_inherits_opener_origin) | test(popup_initial_empty_document_record_captures_creator_identity) | test(rust_cdp_chromium_target_window_open_empty_url_creates_about_blank_popup) | test(window_open_named_target_reused_in_same_command_emits_one_page_event)' \
  --no-fail-fast
# 6 passed

cargo nextest run -p lightmount \
  webdriver_classic_execute_script_round_trips_window_and_frame_references \
  --no-fail-fast
# 1 passed

cargo check -p lightmount-renderer-v8 --all-targets
cargo check -p lightmount-protocol --all-targets
# passed

cargo nextest run --no-fail-fast
# 15835 passed, 18 skipped

cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
# both passed
```

第三纵切 D 单独完成的是 Phase 3 的窄 initial-empty owner 不变量，不是 popup 完成标志。
该提交当时明确保留的缺口如下；其中 non-empty URL 的首个 owner 纵切已由下一节继续收敛：

- named target 与 `noopener` 仍走 legacy creation；non-empty URL 在该提交时也走 legacy，
  下一节 Phase 4 第一纵切 A 已只迁移保留 opener 的非命名、非 `javascript:` 路径；
- target 级 `close()` / `window.close()` 还没有统一 transaction，真实 target Window 也不能因
  一个旧 lightweight close callback 被误认为已关闭；
- protocol 的 document-start scripts、isolated worlds 和 runtime bindings 到 adoption 时才可得，
  尚未证明它们相对 opener 的同步 initial-Document mutation 具有 Chromium 一致的顺序；
- target activation 前启动 timer/fetch、触发 modal dialog 或发布 Inspector-sensitive output 的
  scheduler 边界尚缺专门回归。当前 output journal 对已发布 prefix 拒绝 adoption，这是安全门，
  不是这些时序已经完成的证据；
- initial request 与 staged URL 不匹配时会 fail closed；staged residence 目前依赖 owner teardown
  作为最终清理安全网，后续应增加不递归借用 owner store 的 eager reject/retire transaction；
- initial DomHost 继续沿用本项目 child initial-empty 的完整 HTML tree 约定。是否需要进一步对齐
  Chromium 对 doctype / parser state 的细节，应由 WPT/Chromium probe 决定，不能在 identity
  纵切中凭印象改树形。

### Phase 4：non-empty URL 单一导航

- pending URL 只交给 auxiliary Page owner；
- 接入 target admission / wait-for-debugger；
- Fetch/Network interception 绑定同一个 navigation token；
- 删除 lightweight mirrored load；
- 把“exactly two load owners”测试改为“exactly one authoritative navigation owner”；
- 验证 redirect、204/205、error page、history、DCL/load/done 和 opener immediate
  mutation。

完成标志：同一个 popup URL 不再因为实现结构产生两个请求。

#### Phase 4 第一纵切 A：non-named related popup 的唯一 navigation owner

本提交把上一节的 exact initial Page residence 扩展到保留 opener、非命名、URL 可解析且
scheme 不是 `javascript:` 的 `window.open()`。它解决的是最直接的双请求 owner，不把尚未
迁移的 name/group/opener policy 或全部 navigation terminal 语义混入同一提交。

renderer 同步路径现在遵守以下边界：

- non-empty destination 不是 initial Document URL。同步 callback 始终构造真实
  `about:blank` initial Document（显式 `about:blank#fragment` 仍保留其 fragment），继承
  creator origin、policy、referrer、base URL、storage authority 和 stable WindowProxy；
- stable WindowProxy shell 只负责 identity handoff；opener 的 `innerWidth`、`innerHeight`、
  `outerWidth`、`outerHeight` 和 `devicePixelRatio` 数值 surface 会在真实 target Context
  初始化时复制到最终 inner Window。把这些值只写到临时 facade 会在 realm handoff 时丢失，
  已由 Chromium/WPT 移植的 BiDi user-context viewport 回归覆盖；
- requested destination 只保留在 immutable `RendererPendingPopupActivation` 中。同步返回后，
  opener 可以立即修改 popup global 与 `document.body`；source `JsContextHost` 不创建
  `LightweightPopupBrowsingContextRecord`，也不调用
  `start_lightweight_popup_document_load()`；
- protocol 先用 exact `RendererPendingAuxiliaryPage` materialize/adopt 上述 initial Page，发布
  target/attach/Inspector ownership，然后才把 requested URL 变成绑定该 target residence 的
  `PopupTargetNavigationOwnerAction`。后续 fetch、response、replacement commit、lifecycle 和
  generation 继续走现有 stable Page navigation path，没有新增 protocol loader；
- `waitForDebuggerOnStart` 是明确 admission gate：等待期间 target session 可以观察 initial
  realm，但 destination 请求数必须为零；`Runtime.runIfWaitingForDebugger` 之后才释放这一份
  target-owned navigation；
- eligible staging 失败时不再 fall through 到 legacy lightweight loader。调用方失去同步 proxy
  并由普通 target fallback 继续处理，比悄悄恢复两个 authoritative owner 更安全；正常
  production owner-local path 由聚焦回归证明可成功 staging。

`window_open_hands_off_session_storage_snapshot_and_initial_storage_key` 现在同时覆盖两段：

1. initial `about:blank` adoption 前后保持 exact Document、origin/referrer、storage 与 CSP；
2. gated HTTP non-empty popup 在 admission 前保存 target proxy/Document/global/body mutation，
   auto-attached target session 必须看到同一对象；等待调试器时服务端计数为 `0`，resume 后
   完成 cross-origin replacement，最终计数严格为 `1`。旧注释与断言中的 two owners 已删除。

本纵切的实跑聚焦证据：

```bash
cargo nextest run -p lightmount-protocol \
  -E 'test(window_open_hands_off_session_storage_snapshot_and_initial_storage_key) | test(opener_window_handle_projects_the_renderer_owned_auxiliary_realm) | test(rust_cdp_chromium_target_window_open_blank_creates_popup_target) | test(rust_cdp_chromium_target_window_open_auto_attached_popup_materializes_initial_document) | test(rust_cdp_chromium_target_window_open_waiting_popup_routes_initial_document_after_resume) | test(window_open_emits_popup_target_created_from_runtime_work) | test(rust_cdp_chromium_target_window_open_javascript_url_still_reports_popup_target) | test(window_open_named_target_reuses_existing_popup_target) | test(rust_cdp_chromium_target_window_open_empty_url_creates_about_blank_popup) | test(popup_initial_about_blank_adopts_renderer_page_and_related_script_agent)' \
  --no-fail-fast
# 10 passed

cargo nextest run -p lightmount-renderer-v8 \
  -E 'test(window_open_non_about_returns_lightweight_popup_and_dispatches_load) | test(window_open_named_lightweight_popup_reuses_without_recloning_session_storage)' \
  --no-fail-fast
# 2 passed；standalone / named legacy 边界未被 production admission 改写

cargo nextest run -p lightmount \
  websocket_bidi_set_viewport_user_context_inherits_through_window_open \
  --no-fail-fast
# 1 passed；同步 stable WindowProxy 与 navigation 后 target 都继承 user-context viewport

cargo nextest run -p lightmount-renderer-v8 \
  window_open_lightweight_popup_inherits_opener_viewport_surface \
  --no-fail-fast
# 1 passed；保留 legacy fallback 的 viewport 行为

cargo nextest run --no-fail-fast
# 15837 passed，18 skipped（rebase 到 origin/master f16860e4fb 后）

cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
# both passed
```

#### Phase 4 第二纵切 B：Page-residence-bound navigation claim

第一纵切已经消除了 renderer/protocol 双 loader，但当时的
`PopupTargetNavigationOwnerAction` 只冻结 browser context id、target id 和 URL。target 与 CDP
session 可以跨 renderer Page replacement 存活，因此 target id 不是足够的导航权限；此外
`waitForDebuggerOnStart` 恢复路径仍会从当前 target URL 重新推导一次 initial navigation。旧
activation 若晚于 Page replacement 到达，就可能把属于 initial Page 的 destination 应用到新
residence。

本纵切把 navigation admission 收敛为以下状态机：

```text
RendererPendingPopupActivation
  -> capture exact target route + TargetPageResidenceIdentity + frozen URL
  -> TargetRuntimeSlot::Held(action)
       immediate admission       -> Published(claim) -> scheduler owner action
       waitForDebugger admission -> Published(claim) -> runIfWaitingForDebugger owner turn
  -> validate exact route + target + loaded_page_generation
  -> Consumed(claim) tombstone
       current -> one Page navigation
       stale   -> drop; never rescan target URL
```

具体边界如下：

- target creation 在发布 `Target.targetCreated` / attach lifecycle 前捕获 exact
  `TargetPageResidenceIdentity` 并把 initial destination stage 到该 target 的
  `TargetRuntimeSlot`；捕获或 staging 失败会回滚不完整 target，不能留下“只有 URL、没有 owner”
  的半接受状态；
- `PopupTargetNavigationClaimIdentity` 同时冻结 Page residence/generation、browser context、
  concrete target、URL 和 navigation kind。普通 admission 只把这一个 move-only action 发布给
  protocol scheduler；named-target reuse 暂时仍走 legacy group policy，但其既有 action 也获得
  相同的 Page-generation currentness 检查；
- `waitForDebuggerOnStart` 期间 action 保持 `Held`，`Page.enable` 和
  `Page.createIsolatedWorld` 不能触发 destination。`Runtime.runIfWaitingForDebugger` 是明确的
  target-owner admission turn：它把同一 action 变成 `Published` 后直接消费，并保留触发恢复的
  explicit popup session 作为 execution attachment，使 Fetch pause/fulfill 与后续 lifecycle 都
  继续路由到同一个 session；这里没有重新读取 target URL；
- completion 先把匹配的 `Published` claim 原子变成 `Consumed`，再检查 exact
  `TargetPageResidenceIdentity`。即使检查发现 Page generation 已变化，`Consumed` tombstone 也
  保留在 target slot；所有通用 initial-navigation 入口看到 `Held`、`Published` 或 `Consumed`
  都会拒绝从 target URL 制造第二份工作；
- Page replacement 不清除这份 authority。这样旧 action 必须在新 generation 上 fail closed，
  而不是因为状态被清空后被 generic fallback 重新解释；target teardown 则连同整个 slot 一起
  回收它；
- admission action 和其内部 Page navigation 都沿用现有 `Box::pin` orchestration 边界。若把
  这两个大 async state 直接内嵌进通用 initial-navigation future，即使普通
  `Target.createTarget` 不走 popup 分支，也会放大 Tokio worker 的栈布局；target-creation
  storage fan-out 回归把这个非业务分支的栈边界一并锁住。

聚焦回归分别证明正常 action、stale generation 和 debugger admission：

```bash
cargo nextest run -p lightmount-protocol \
  -E 'test(local_storage_mutations_fan_out_across_targets_without_leaking_session_storage) or test(popup_navigation_owner_action_rejects_replaced_page_and_cannot_be_rescanned) or test(rust_cdp_chromium_target_window_open_waiting_popup_routes_initial_document_after_resume) or test(popup_activation_creates_target_and_schedules_navigation_without_page_readback)' \
  --no-fail-fast
# 4 passed

cargo nextest run --no-fail-fast
# 15839 passed，18 skipped（rebase 到 origin/master c597ac97dc 后）

cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
# both passed
```

其中 stale 回归在 action 发布后推进 target 的 `loaded_page_generation`，随后证明：旧 action 不
产生 navigation/event、新 Page 仍是 `about:blank`，再次调用通用 target-URL initial-navigation
入口也不能复活该 destination。debugger 回归同时证明 waiting 期间请求数为零，resume 后只有
同一 explicit popup session 的一份 Fetch/Network 请求，并能完成 replacement lifecycle。
第一次全量运行让未 boxed 的 nested future 在上述 DOMStorage target-creation 回归中稳定触发
stack overflow；单独复现后恢复 heap-boxed orchestration 边界，4 个聚焦用例与第二次全量均
通过，因此没有把它归类为 flaky。

#### Phase 4 第三纵切 C：no-commit HTTP terminal 与 initial Document history

204/205 不是普通 transport failure，也不是一个可交给 renderer 构造新 Page 的空 HTML
response。它们已经收到 HTTP response，但导航必须以“不提交 Document”结束。旧实现把二者
继续送入 response-stage preparation / `Page` construction，因而可能替换 popup 的 initial
Document、丢掉 opener 同步写入的 realm/DOM 状态，并错误发布 `frameNavigated`、DCL、load 或
`loadingFinished`。只在 popup 调用点绕过 commit 也不够：streaming、buffered、Fetch
response-stage 和 background `Page.navigate` 会从不同入口到达同一个 load outcome 边界。

Chromium 对照给出的是一个明确的两层合同：

- `content/browser/renderer_host/navigation_request.cc` 将 204/205 归为不可 render/commit 的
  response，并中止这次 navigation；`navigation_controller_impl_browsertest.cc` 对应回归证明
  不会新增 NavigationEntry；
- `third_party/blink/renderer/core/dom/document.h` 用 `Document::IsInitialEmptyDocument()` 保存
  Document 身份，而不是从 URL 推断；`frame_loader.cc` 和
  `document_loader.cc::UpdateForSameDocumentNavigation` 在 URL/history update 步骤把 initial
  empty Document 上的标准导航转换为 replacement。fragment、`history.pushState()` 和
  `history.replaceState()` 不会让该 Document 失去 initial 身份，`document.open()` 则会显式
  退出；
- WPT `initial-empty-document/window-open-204-fragment.html`、
  `window-open-204-pushState-replaceState.html` 和 `window-open-history-length.html` 把这些
  行为连成同一个矩阵：204 后仍是原 initial Document，same-document 更新后
  `history.length == 1`，下一次成功 cross-document navigation 仍替换该唯一条目；
- inspector-protocol 的 `page/navigate-204.js` 和
  `network/navigation-204-loading-failed.js` 要求 `Page.navigate` 返回
  `net::ERR_ABORTED`，Network 顺序为 `responseReceived → loadingFailed(canceled=true)`，而不是
  `loadingFinished`。

Lightmount 现在把这条语义放在公共 navigation terminal 边界：

```text
HTTP response head (204/205)
  -> publish response metadata / redirect hops
  -> NavigationLoadOutcome::NoCommitResponse
  -> CompletedNoCommitResponseProgressTransfer
  -> FailedNavigationDocumentPolicy::PreserveCommittedDocument
     + FailedNavigationHistoryPolicy::RetainInitialEmptyDocumentReplacement
     + FailedNavigationResponseMode::CdpErrorTextResult
  -> responseReceived
  -> loadingFailed(errorText = net::ERR_ABORTED, canceled = true)
```

具体责任边界如下：

- `NavigationLoadOutcome::NoCommitResponse` 是独立 typed outcome，携带 final URL 和已经完成的
  main-document progress transfer。它不复用 `NetworkFailure(String)`：后者仍保留现有的 failed
  navigation / Document invalidation policy，避免在尚未决定 error-page 设计前悄悄改变普通网络
  错误；
- streaming response 和 captured/buffered response 都在 prepared-Document/Page construction
  之前识别 204/205；Fetch response-stage preparation 也拒绝为它们准备 Page。background
  `Page.navigate` 不再提前发送 success result，因此 terminal owner 能返回 Chromium 形状的
  `{frameId, loaderId, errorText: "net::ERR_ABORTED", isDownload: false}`；
- no-commit progress 先保留 response/redirect 元数据，再用同一 request id 发布 canceled
  `loadingFailed`。由于没有 renderer DCL/load boundary 可以在后续 turn 解锁 body phase，terminal
  turn 会显式让 response/body-failed 两阶段可见，但仍通过同一个 progress queue 保证源顺序；
- materialization 使用 `PreserveCommittedDocument`：popup 的 exact stable Page、WindowProxy、
  V8 Context、Document、global 与 body mutation 全部保持，且不会发布新
  `Page.frameNavigated`、DCL 或 load。failed response body 仍进入统一的 failed-body bookkeeping，
  不制造“协议终态完成但 body owner 悬空”的旁路；
- browser-owned initial history 在 popup 创建边界显式 stage
  `ReplaceInitialEmptyDocument`。no-commit terminal 不从“当前 URL/Document 看起来像 initial”反推
  新意图，而是只保留已经 pending 的 initial replacement；reload、traverse 和普通 append 的
  pending update 都会丢弃。这样先后遇到 204、205 后，下一次成功导航仍替换 popup 的唯一条目，
  同时普通顶层 `about:blank` 的首次 `Page.navigate` 仍按既有 Chromium 合同追加；
- renderer-owned history 在 `JsContextHost` 上保存持久的 root initial-Document bit。related
  auxiliary Page 在原 stable realm 构造时设置它，fragment、`pushState`、Navigation API
  same-document mutation 和后续 cross-document seed 都据此转换为 replacement；URL 变成
  `about:blank#...` 后仍然正确。`document.open()` 在 root Document replacement owner 边界清除
  该 bit，与 Blink 的 Document 合同一致；
- 后续 redirect success 继续走既有 replacement Page path。integration matrix 要求 redirect
  每个 hop 恰好一条 `requestWillBeSent`、共享 request id、后续 hop 携带前一跳
  `redirectResponse`，最终只出现一次 `frameNavigated`、一次 DCL 和一次 load，并把 initial realm
  替换为一个 history entry。

本纵切新增的 end-to-end popup 用例从 `waitForDebuggerOnStart` 的真实 initial Page 开始：等待时
写入 opener global/body marker；resume 后完成 204；依次执行 fragment 和 `pushState`；再执行
205；最后导航到 redirect chain。它同时检查每个 no-commit terminal 的 CDP 形状、事件顺序、
Document/realm identity、browser/renderer history projection，以及成功 replacement 的请求和
lifecycle 基数。renderer 附近的回归单独锁住 initial bit 对 fragment、`pushState`、cross-document
seed 和 `document.open()` 的作用；progress queue 与 browser history owner 也各有边界测试。

聚焦与全量验证：

```bash
cargo nextest run -p lightmount-protocol --no-fail-fast \
  -E 'test(completed_no_commit_response_progress_orders_response_before_canceled_failure) | test(active_target_initial_empty_document_record_tracks_navigation_lifecycle) | test(rust_smoke_fixture_serves_navigation_no_commit_routes) | test(page_navigate_network_failure_invalidates_previous_document) | test(popup_no_commit_responses_preserve_initial_document_before_redirect_replacement)'
# 5 passed

cargo nextest run -p lightmount-protocol --no-fail-fast \
  -E 'test(navigation_history_marks_reload_as_reload_transition) | test(navigation_history_supports_playwright_back_forward_commands) | test(navigated_within_document_matches_chromium_mixed_history_sequence) | test(navigation_history_is_preserved_per_parked_target) | test(renderer_history_back_uses_browser_owned_navigation_history) | test(rust_cdp_capability_page_navigation_history_round_trip)'
# 6 passed

cargo nextest run -p lightmount-renderer-v8 --no-fail-fast \
  -E 'test(root_initial_empty_document_replaces_same_and_cross_document_history_updates) | test(document_open_exits_root_initial_empty_history_replacement_mode) | test(window_open_204_popup_ignores_navigation_and_preserves_initial_empty_history) | test(window_open_without_url_replaces_initial_empty_history_on_first_navigation)'
# 4 passed

cargo nextest run --no-fail-fast
# 15844 passed，18 skipped

cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
# both passed
```

第一次全量运行暴露 6 个稳定可复现的 browser-owned history 回归：当时实现只要 target 仍在
initial empty Document，就在每次 cross-document start 无条件重新 arm replacement，导致普通顶层
`about:blank` 的首次导航、reload、traverse 和 parked-target history 少一个 entry。6 个用例聚焦
复跑为 0/6，因而没有归类为 flaky。最终实现把意图来源收回 popup 创建边界，并让 no-commit
terminal 只保留已经存在的 `ReplaceInitialEmptyDocument`；相同 6 个用例恢复为 6/6，第二次全量
为 15844/15844。

本纵切没有把以下问题伪装成已经完成：普通 DNS/connect/TLS/HTTP transport failure 应保留旧
Document、提交统一 error page，还是使旧 Document 失效，仍需下一纵切用 Chromium/CDP/WPT
矩阵明确命名 policy。当时尚缺的 Fetch fulfill/continue 204/205 interception 入口矩阵现已由
Phase 4 第五纵切 E 补齐；它也反向证明当时所谓“公共 response builder 已共享 classification”并
不完整，因为 response-stage synthetic buffered-body 入口仍存在一条直达 prepared Document 的旁路。

Phase 4 尚未完成，后续纵切仍必须补齐：

- 普通 network error/error-page 的 Document policy、CDP response shape、history、DCL/load/done
  和 opener-visible state 仍需专门 integration 矩阵；
- named target、`noopener` / `noreferrer` 和 `javascript:` URL 仍由 legacy policy/path 处理，
  分别属于 Phase 5 group/opener 纵切和后续 URL semantics 纵切；
- target admission 前启动 timer/fetch/modal 或先关闭 popup 的行为仍需和 close transaction 一起
  定义，不能靠当前 output journal 的 fail-closed 门槛代替正常时序。

#### Phase 4 第四纵切 D：DocumentCommit / exact continuation owner boundary

第三纵切证明了 no-commit terminal 不应构造 replacement Document，但成功 response 的另一条
边界仍不够准确：renderer 在 parser 尚未走到 DCL 时已经拥有可用的 replacement realm 和
Document，protocol 也必须发布新的 execution context、允许 debugger/configuration 控制并返回
早期 `Page.navigate` 结果；与此同时，DCL、load、最终 title/history 和 renderer output 又不能
由 command future 的返回时刻推断。原实现把这些状态压在一个 async completion 里，不同入口会
出现两类相反错误：要么为等待最终 Page snapshot 而锁死 debugger/Fetch 控制命令，要么过早取
一份仍在变化的 PageState，并让旧 Document 的异步完成污染 replacement generation。

Chromium 对齐约束不是“所有命令都等到 DCL”。已经 commit 的 Document 即使 parser-blocking
script 仍在等待网络，也可通过其 replacement execution context 执行 `Runtime.evaluate`，此时
`document.readyState == "loading"`、`document.body` 甚至可以仍为 `null`。只有 attachment 尚在
replacement cutover 时，document-bound 命令才必须等待；`Runtime.addBinding`、preload、isolated
world 和 debugger resume 等配置/控制面还必须穿过这个 cutover，才能在第一段 author script
之前生效。DCL 是独立的 lifecycle target，不是 realm usability gate。

本纵切把成功 navigation 收敛成以下两段 owner transaction：

```text
response / prepared replacement
  -> DocumentCommit
     -> adopt exact stable Page residence + replacement realm/Document
     -> publish attachment / executionContextCreated / early navigate result
     -> release renderer attachment cutover
  -> RendererDocumentContinuationObserver (exact loader + generation)
     -> renderer owner reaches this Document's DCL target
     -> capture RendererOutputFence + Arc<RendererPageState> in the same owner turn
     -> typed Send completion lane
     -> project predecessor, then apply PageState iff target + loader are still current
     -> refresh history title / Target.targetInfoChanged
     -> later continue to exact Load lifecycle observation
```

核心实现边界如下：

- renderer 的 `RendererDocumentContinuationPublisher/Observer` 是一次性 typed terminal。publisher
  被安装到创建或 replacement navigation 的真实 owner continuation，terminal 同时携带 exact
  `RendererOutputFence` 和 `Arc<RendererPageState>`；两者来自同一个 owner turn，protocol 不再在
  fence 之后另发 snapshot command，因而不会跨 turn 或跨 generation 观察到不一致状态。publisher
  drop 也会显式产生 canceled terminal，receiver 不会永久悬挂；
- continuation target 固定为该 committed Document 的 DCL。phase-one producer 被网络、parser
  source、debugger pause 或 location navigation 挡住时只保留真实 owner turn，不再因为
  `DocumentCommit` reply 已经发出就提前 settle producer park。若 parser script 发起 replacement
  navigation，源 Document 的 continuation 会按 generation 终止，不能补发源 DCL；successor
  navigation 拥有自己的 token、loader 和 terminal；
- owner 恢复 live Page 时先取得同一 renderer stream 的 output fence，再 settle terminal。这样
  lifecycle、Inspector、popup/worker/child-frame publication 都先按 concrete cursor 进入 protocol
  scheduler，PageState 才能替换 protocol cache；`PageState` 只在 gate 的 target id、session owner
  route 和 loader id 仍与当前 Page 匹配时应用，旧 completion 无权修改新 target/runtime；
- CDP scheduler 为 continuation 使用独立的 Send receiver，不与 background navigation completion
  或 background event gate 混成同一含义。background navigation gate 负责 early navigate result
  之后的 load residence/event ordering；continuation gate 只表达 exact committed Document 的 typed
  terminal。Classic WebDriver、WebDriver BiDi 和 CDP actor 都消费相同 completion，不各自重建
  renderer wait；
- attachment cutover 和 DCL gate 使用不同 command policy。document-bound Runtime/DOM/CSS 等命令
  在 renderer attachment suspended 时仍等待 replacement commit；persistent configuration/control
  命令可跨过 suspension。commit 之后，`Runtime.evaluate` 等命令可访问仍为 `loading` 的当前
  Document；当前只有依赖 DCL PageState title projection 的 `Page.getNavigationHistory` 等待 typed
  continuation。这个拆分由 parser-blocking WebSocket CDP 回归锁住，不能再把宽泛的
  `waits_for_document_navigation_to_finish` 直接复用为 DCL gate；
- prepared commit configuration 会在 author script 前安装 document-start preload、isolated world
  和 Runtime binding。browser-internal bootstrap script 使用专门的 Inspector execution path：它不
  触发 instrumentation pause，并使用真实 replacement origin；author script 仍按普通 Debugger
  policy 发布和暂停。这避免为“先配置后执行”重放一份 Document 或临时关闭 debugger；
- typed PageState 应用后刷新 browser-owned current history entry 的 title，并基于 exact target
  delta 生成 `Target.targetInfoChanged`。active、background、inactive/parked target 都沿用 target
  owner identity；旧 loader 的 completion 即使稍后到达也只会被消费和丢弃，不能改写当前 title；
- protocol-neutral direct command 测试不再以“pending queue 暂时为空”代替 load。Navigate、Reload
  和 TraverseHistory 的 `wait: Load` 路径从 command result 提取 exact loader，注册
  `RendererDocumentLifecycleMilestone::Load` waiter，驱动 typed scheduler input 到 `Reached` 后再
  完成对应 main-Document load residence。child-frame navigation、input、`Target.createTarget`、
  Classic 和 BiDi lane 因而验证的是同一 lifecycle invariant，而不是某次 executor 恰好先跑完；
- `TestContext` 保留 production-shaped owner ordering：队首若是 fixture-only、缺少 background owner
  lane 的 popup action，可以让另一个 target 的 ready work 越过；同一 command 的 follow-up 也可
  越过尚未 ready 的旧 load residence。这个有限规则使 parser script 的 `location` successor 能
  接管 owner，源 DCL 被压制；它不是任意 drain/retry，也没有加入 sleep 或无限 polling。

本纵切的回归矩阵覆盖：stable Page background DCL→load continuation、parser script replacement
及独立 response gate、preload/world/binding 先于 author script、Debugger instrumentation
pause/resume、Fetch auth/response-stage、target discovery 与 title delta、browser-session target
route、auto-attach owner、Playwright script execution disable、history back/forward、popup
create/navigate/close、child-frame protocol-neutral navigation、direct input、worker/AudioWorklet
owner continuation，以及 Classic/BiDi completion routing。

聚焦验证证据：

```bash
cargo nextest run -p lightmount-protocol --no-fail-fast \
  --success-output never --status-level fail --final-status-level fail --show-progress none
# 3295 passed

cargo nextest run -p lightmount-protocol-cdp --no-fail-fast
# 8 passed

cargo nextest run -p lightmount \
  websocket_cdp_raw_client_runtime_evaluate_immediately_after_page_navigate_succeeds \
  websocket_cdp_runtime_control_command_waits_for_navigation_attachment_cutover \
  websocket_cdp_runtime_evaluate_uses_committed_page_while_parser_blocking_source_is_pending \
  --no-fail-fast
# 3 passed；同时锁住 pre-commit cutover 和 post-commit loading Document 两侧

cargo nextest run --no-fail-fast
# 15884 passed，18 skipped

cargo fmt --all --check
# passed

cargo clippy --workspace --all-targets -- -D warnings
# passed
```

raw-CDP 立即 evaluate 回归不再把 body 已解析当作 attachment commit 的必要条件：它验证新
Document URL 和合法 readyState；若 readyState 已越过 `loading`，才要求 body link 已存在。这样
workspace 高并发下 continuation 尚未跑完不再被误判为 `NoDocumentLoaded`，而真正绑定旧
`about:blank` 或 attachment 尚不可用仍会失败。

以上是 rebase 前结果；若 rebase 改变 Rust 基线，必须在 rebase 后重复，而不能沿用这组结果。

本纵切仍不是 Phase 4 完成标志。除上一节列出的 network error、named/`noopener`/`javascript:`
矩阵外，target admission 前已经排队的 timer/fetch/dialog/Inspector output、`window.close()` 与
target close 的单一 transaction，以及 completion lane 关闭时的 production teardown 诊断仍需
后续切片完成。exact continuation 只建立了这些行为可依赖的 owner/lifecycle 基础，没有替它们
定义产品语义。

#### Phase 4 第五纵切 E：Fetch response-stage effective response terminal

第三纵切 C 已经证明直接网络 204/205 必须保留 popup initial Document，但当 response head 被
Fetch 拦截后，决定 navigation terminal 的不再只是服务器原始状态，而是 DevTools action 释放的
effective response。这个入口不能只补一条“原始 204 继续后仍失败”的容易路径；必须同时证明
override 能把可提交响应变成 no-commit，也能把原始 no-commit 响应变回可提交响应，否则实现仍
可能在原始 head 或 synthetic body 的某一侧过早作出不可逆决定。

Chromium 的责任链给出了明确合同：

- `third_party/blink/public/devtools_protocol/domains/Fetch.pdl` 要求
  `continueResponse` 修改 status 或 headers 时同时给出两者；全都省略则沿用原始 response head；
- `content/browser/devtools/protocol/fetch_handler.cc::ContinueResponse` 在完整 override 时直接复用
  `FulfillRequest` 构造新的 HTTP response head，不带 override 时才原样 continue；
- `content/browser/devtools/devtools_url_loader_interceptor.cc` 把 override 安装到下游可见的
  `URLResponseHead`，保留原 body 或替换 synthetic body。因而后续 navigation 看到的是 effective
  status/header，而不是一份只供 Network domain 展示的旁路 metadata；
- `content/browser/renderer_host/navigation_request.cc` 在处理这份 response head 时把 204/205 判为
  `response_should_be_rendered_ == false`，设置 `net::ERR_ABORTED` 并终止而不 commit；
- `content/browser/devtools/protocol/page_handler.cc::NavigationReset` 最终从同一个
  `NavigationRequest` 读取 net error，`Page.navigate` 因而返回 `errorText: net::ERR_ABORTED`。这也
  解释了为何不能先发 success，再在 Network domain 单独把请求标成 canceled。

本轮矩阵第一次运行确实发现了后一类分裂。streaming/captured 网络 builder 已在创建 replacement
Page 前识别 204/205，但 response-stage `Fetch.fulfillRequest` 使用的
`build_navigation_from_buffered_body_source_with_load_inputs_async` 自己重复了一份 Page reservation
和 prepared-document construction，直接把 synthetic response 包成 `ResponseCommitReady`。当原始
200 被 fulfill 为 204 时，旧实现会同时发布 `Network.responseReceived(status=204)`、成功的
`Page.navigate`、`Page.frameNavigated`、`DOM.documentUpdated` 和 DCL；Network projection 与真实
Document owner 互相矛盾。

修复没有在 Fetch command handler 追加 204/205 特判，而是删除这份重复 construction。buffered
body source 现在先构造 typed `ResponseHead`，再委托既有
`build_navigation_from_captured_raw_response_with_load_inputs_async`，由一个公共边界依次分类
no-commit、download 和 committable response。这样 classification 发生在 renderer Page reservation
之前；已有 200 response-stage prepared candidate 在 synthetic 204 terminal 被丢弃，原始 204 则
从未创建 candidate，而 synthetic 200 仍可在释放 pause 后创建并提交新 Document。最终路径是：

```text
PausedDocumentTransfer
  -> fulfill synthetic head/body OR continue original/overridden head + original body
  -> captured/streaming effective-response classifier
     -> 204/205: NavigationLoadOutcome::NoCommitResponse
     -> attachment: NavigationLoadOutcome::Download
     -> otherwise: ResponseCommitReady
  -> one shared materialized navigation terminal
```

新增集成矩阵如下；四格都从已经安装的 stable Page/Document 开始，并使用真实 response-stage
`Fetch.requestPaused`：

| 原始状态 | terminal action | effective 状态 | 预期结果 |
| --- | --- | --- | --- |
| 200 | `Fetch.fulfillRequest` | 204 | `ERR_ABORTED`，保留旧 Document/realm |
| 200 | `Fetch.continueResponse` 完整 override | 205 | `ERR_ABORTED`，保留旧 Document/realm |
| 204 | `Fetch.continueResponse` 无 override | 204 | `ERR_ABORTED`，保留旧 Document/realm |
| 204 | `Fetch.fulfillRequest` | 200 | 恰好一次新 Document commit |

前三格共同断言 effective `responseReceived` 先于同 request id 的
`loadingFailed(canceled=true)`，不存在 `loadingFinished`、`frameNavigated`、DOM update、DCL/load；
target Page residence、renderer Page residence、attachment、renderer agent、HTML 和 pending
navigation 状态全部保持。第四格反向断言 `responseReceived(200)`、`loadingFinished`、唯一
`frameNavigated`、DOM update、DCL/load 和 synthetic body，同时 stable Page/attachment 不变而
Document agent 必须变化。它还锁住原始 204 pause 不预建 renderer agent，避免未来为了优化
response-stage 又按原始 head 提前终止。第三纵切的真实 popup 204/205→fragment/pushState→redirect
用例继续负责 popup initial realm/history；本纵切在公共 Fetch/navigation 边界补入口矩阵，两者
组合覆盖 popup 与普通 stable Page，而不复制一份更大的 popup scenario。

聚焦验证：

```bash
cargo nextest run -p lightmount-protocol \
  response_stage_effective_no_content_statuses_abort_without_committing
# 1 passed

cargo nextest run -p lightmount-protocol \
  response_stage_fulfill_can_replace_original_no_content_with_committable_response
# 1 passed

cargo nextest run -p lightmount-protocol \
  -E 'test(/(continue_response_can_override_status_and_headers|fulfill_request_completes_navigation_with_synthetic_response|popup_no_commit_responses_preserve_initial_document_before_redirect_replacement)/)'
# 4 passed（同时命中一条同名 subresource override 回归）

cargo nextest run --no-fail-fast
# 15886 passed，18 skipped

cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
# both passed
```

本纵切补齐的是 204/205 effective-response interception terminal，不改变普通
DNS/connect/TLS/HTTP failure 的 Document/error-page policy，也不扩大到 named target、`noopener`、
`javascript:` 或 close transaction；这些仍是 Phase 4/5 后续切片。

#### Phase 4 第六纵切 F：pre-response transport failure 的 browser-owned error Document

第三纵切 C 和第五纵切 E 已经把“收到一个不可提交的 HTTP response”定义为保留旧 Document 的
no-commit terminal，但普通 DNS/connect/reset 等 transport failure 是另一类行为：请求尚未得到
可渲染 response，Chromium 会提交一个新的 browser-owned error Document。旧 Lightmount 路径把
`NetworkFetchFailure` 直接 materialize 为 failed navigation，并使 target 的旧 Document 不再可用；
popup initial `about:blank` 因而既没有 Chromium 的错误页，也不能继续通过原 stable Page/WindowProxy
观察新 realm。这个差异同时影响 `Page.navigate`、Target/history、Network terminal、Runtime realm
和 popup opener，不能只在错误返回字符串处补一个 HTML。

本纵切先明确一个窄而可验证的范围：普通 top-level main-document fetch 在 response metadata 之前
返回 `NetworkFetchFailure`。这里包括本轮连接接受后立即断开的最小复现，以及沿用同一 typed failure
的常见 DNS/connect 类错误。它不把证书 interstitial、HTTP 4xx/5xx、proxy CONNECT、显式 offline /
blocked URL、request-stage `Fetch.failRequest`、continued interception transport failure 或 policy
block 混成同一种产品语义；这些入口必须各自先确定 Chromium terminal，再决定是否复用 error
Document primitive。

##### Chromium 源码与二进制证据

对照基线为本地 `/home/donoughliu/chromium/src` commit
`a03603fe9af6230a12f1b2fb2c18a7d003a0d937`，`out/Default/chrome --version` 为
`Chromium 147.0.7709.0`。运行时 probe 使用：

```bash
/home/donoughliu/chromium/src/out/Default/chrome \
  --headless=new --disable-gpu --no-sandbox \
  --remote-debugging-port=9229 \
  --user-data-dir=/tmp/lightmount-chromium-network-error-probe \
  --noerrdialogs --no-first-run --ozone-platform=headless \
  --ozone-override-screen-size=800,600 --use-angle=swiftshader-webgl \
  about:blank
```

CDP probe 分别执行普通 `Page.navigate` 和 `window.open()`，目标是一个接受 TCP 后不返回 HTTP head
便关闭连接的本地 server。它记录 Target/Page/Network/Runtime 事件，并在 popup opener 中保留同步
返回的 WindowProxy 引用。观察结果如下：

| 可观察面 | Chromium 147 结果 |
| --- | --- |
| browsing context | frame id、target id 和 popup opener relation 保持；不是销毁 target 后另建错误页 target |
| Document URL | `location.href` / `Page.frameNavigated.frame.url` 为 `chrome-error://chromewebdata/` |
| 请求 URL | `Page.frameNavigated.frame.unreachableUrl`、Target URL 和当前 history entry 仍是失败 URL |
| Page.navigate | success-shaped callback，包含原 loader/frame identity、`errorText` 和 `isDownload: false` |
| Network | 同一 request id 上 `requestWillBeSent → loadingFailed → loadingFinished`，没有 `responseReceived` |
| realm/lifecycle | 旧 global/Document 状态消失，新 execution context 可执行；随后才有 DCL 和 load |
| popup initial history | error Document 替换 initial entry，`history.length == 1` |
| opener | popup 新 realm 中 `window.opener !== null`；opener 保存的 WindowProxy identity 不变 |

`loadingFailed` 后同 request id 又出现 `loadingFinished(encodedDataLength=0)` 看起来反直觉，但这是本地
Chromium 二进制的实际协议序列；实现和测试保留该事实，不用“一个请求只能有一个 terminal”的内部
直觉改写 CDP。probe 中 frame commit 位于二者之间，DCL/load 位于 `loadingFinished` 之后。

源码责任链与运行时结果一致：

- `content/browser/renderer_host/navigation_request.cc::CommitErrorPage` 仍走一次 cross-document
  commit，并通过 `ShouldReplaceCurrentEntryForFailedNavigation()` 决定 history replacement；initial
  entry 必须被替换；
- `content/renderer/render_frame_impl.cc::FailedNavigation` 把 Document URL 设置为
  `content::kUnreachableWebDataURL`，再把失败 URL 写入 `WebNavigationParams::unreachable_url`；同文件
  明确说明 HistoryItem 使用 unreachable URL，而不是内部错误页 URL；
- `content/public/common/url_constants.h` 将该内部 URL 定义为
  `chrome-error://chromewebdata/`；它不是一个可当作普通 WebUI 导航的 `chrome://` 页面；
- `third_party/blink/public/web/web_navigation_params.h` 和
  `third_party/blink/renderer/core/loader/document_loader.*` 把 unreachable URL 保存在 DocumentLoader
  上，供 frame/DevTools projection 使用；
- `content/browser/devtools/protocol/page_handler.cc::DispatchNavigateCallback` 从同一个
  `NavigationRequest::GetNetErrorCode()` 生成 `errorText`，所以不能先返回普通 success，再只在
  Network domain 标记失败；
- `third_party/blink/web_tests/inspector-protocol/page/frameNavigatedToUnreachableUrl.js` 直接锁住
  `frameNavigated.frame.unreachableUrl`，browser tests 还检查 error Document 的内部 URL和 opaque
  origin。

##### Lightmount owner transaction

实现复用已经成熟的 stable Page replacement/realm 基础，不创建第二个 Page，也不恢复 lightweight
popup loader。普通 pre-response failure 现在走：

```text
NetworkFetchFailure(original request / request id / net error)
  -> browser-owned NetworkErrorPageNavigation
     { error_text, unreachable_url }
  -> synthetic internal ResponseHead
     { final_url = chrome-error://chromewebdata/, status = 200, text/html }
  -> existing prepared replacement reservation
  -> DocumentCommit on the exact stable Page
     -> detach old default realm, keep Page-owned WindowProxy
     -> install error Document realm with opaque/insecure security state
     -> Page.frameNavigated(url = internal, unreachableUrl = requested)
  -> exact renderer DCL/load continuation
```

这里必须区分三种 URL，不能再用一个 `final_url` 同时满足所有观察面：

| owner / projection | 本纵切保存的 URL | 原因 |
| --- | --- | --- |
| renderer `Page` / Document / frame tree | `chrome-error://chromewebdata/` | 当前可执行 realm 确实是 error Document |
| `unreachableUrl` | transport failure 的 current request URL | DevTools 需要知道哪个资源不可达 |
| Target identity / browser history | transport failure URL | 地址栏、Target、history 代表用户请求，而不是内部实现 URL |

`RendererMainDocumentCommit` 因而新增可选 `unreachable_url`，frame commit 和 `Page.getFrameTree` /
resource tree 都从 Document identity 投影内部 URL和 unreachable URL；Target/history commit API 则显式
接收另一份 browser-visible URL。history snapshot 不再从 `Page.final_url()` 反推地址栏 URL，避免
后续 title refresh 把 error entry 偷换成内部 URL。error Document 使用 opaque origin，CDP frame /
Target security origin 投影为 `://`，secure-context type 为 `InsecureScheme`。

内部错误 HTML 只提供轻量、可脚本化、可诊断的 Document：title 使用失败 host，正文展示转义后的
URL和 net error。它通过与普通 response 相同的 parser、realm、DCL/load 和 PageState owner 路径
构建，但不是原网络请求的 response：Network domain 不发布 `responseReceived`，也不把 synthetic
body 存进 main-resource response-body cache。这样 `Runtime.evaluate`、DOM snapshot 和 lifecycle
都能观察真实新 Document，同时 `Network.getResponseBody` 不会伪装服务器返回了一份 200 HTML。

Network progress 使用一个专门的 two-boundary gate，而不是在 commit 调用点手排 JSON：

```text
response-visible boundary -> loadingFailed(errorText, canceled=false)
renderer output boundary  -> frame commit / contexts cleared + created
body-finished boundary     -> loadingFinished(encodedDataLength=0)
DCL/load continuation      -> DOMContentLoaded / load / stoppedLoading
```

`Page.navigate` result 在 failure progress 可见后返回 `{frameId, loaderId, errorText,
isDownload:false}`。无显式 navigate command 的 popup initial destination 仍消费相同 activity，只是不
制造 command response。active、background 和 stable replacement 共用这一 transaction；error
Document 的 main resource 不进入普通 response store，旧 loader/generation 的 completion 也仍受
existing Document token/currentness gate 限制。

##### Stable WindowProxy、realm 与 opener

popup 回归第一次运行暴露了一个比 Network 更底层的真实缺口：related auxiliary Page 已经复用
stable main WindowProxy，但 `window.opener` 只存在于旧 realm 的 `WINDOW_OPENER_SLOT`；
`detach_global()` 后新 error realm 得到 `null`。修复没有从 protocol `TargetInfo.openerId` 反向制造
JS object。`RendererPageScriptEnvironment` 现在在旧 main realm commit 前捕获该槽中的实际 V8 value，
在同一 stable WindowProxy 绑定新 Context、完成 bootstrap 后再恢复。于是：

- target/core Page residence 和 renderer Page/WindowProxy residence 跨失败保持；
- renderer attachment、execution context、global lexical state 和 Document generation 必须变化；
- popup 新 error realm 仍有 `window.opener`，旧 realm/body marker 不会泄漏；
- opener 同步保存的两个 popup WindowProxy 引用在失败前后仍严格相等；
- 保存的是实际槽值而不是一条 target id，未来 opener 被显式 sever 为 `null` 时也不会被导航重新连上。

同一回归也精确暴露了下一层边界：从 opener 读取已跨源 popup WindowProxy 的 `.closed` 目前仍得到
`SecurityError`。这不是 identity 或 opener graph 丢失，而是 top-level related Page 尚未接入 child
frame 已有的 restricted cross-origin access surface。测试因此用导航前保存的两个引用做 strict
identity 检查，并单独验证 popup realm 的 `opener !== null`；它没有把 `.closed` 硬编码成 `false`。
`closed`、`postMessage`、`location`、identity aliases、descriptor/enumerator 与 close 后动态状态应在
Phase 5 作为一个完整 WindowProxy whitelist 纵切复用 child-frame primitive，不能逐属性开洞。

##### 回归矩阵与当前边界

新增/扩展回归覆盖：

- Page domain：普通 navigate failure 提交内部 error frame、`unreachableUrl` 和 requested-URL
  history；
- Network domain：`requestWillBeSent < loadingFailed < frame commit < loadingFinished < DCL/load`，
  同 request id、无 `responseReceived`，并锁住 response/body 两个 progress boundary；
- Runtime：error Document 可执行，旧 global 不存在，stable target/core/renderer Page identity 保持而
  renderer attachment 更新；
- popup end-to-end：真实 connection drop、initial `about:blank` replacement、history length 1、
  Target opener metadata、new-realm opener 和 opener-side stable WindowProxy identity；
- Page frame/resource tree：Document URL 与 Target/history URL 不再互相覆盖。

本轮聚焦验证与全量门禁：

```bash
cargo nextest run -p lightmount-protocol navigate_failure --no-fail-fast
# 2 passed

cargo nextest run -p lightmount-protocol main_document_navigation_failure --no-fail-fast
# 2 passed

cargo nextest run -p lightmount-protocol \
  page_navigate_network_failure_commits_error_document_in_stable_page --no-fail-fast
# 1 passed

cargo nextest run -p lightmount-protocol \
  error_page_progress_releases_failed_before_finished_at_separate_boundaries --no-fail-fast
# 1 passed

cargo nextest run -p lightmount-protocol \
  popup_transport_failure_commits_error_document_in_stable_auxiliary_page --no-fail-fast
# 1 passed

cargo nextest run --no-fail-fast
# 15888 passed，18 skipped

cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
# both passed
```

以上矩阵仍不足以宣称“所有网络错误完成”。下一批必须至少分别覆盖 redirect-then-drop、DNS、TLS
证书/interstitial、proxy、offline/blocked policy、Fetch request-stage fail/continue、reload/traverse
到 error entry，以及 error page 上的再次导航。尤其 redirect failure 的 Network redirect metadata、
error HistoryItem method/state 和 `Network.getResponseBody` 错误形状目前证据较弱。named target、
`noopener` / COOP、full cross-origin WindowProxy surface 和 close transaction 仍按 Phase 5 处理。

### Phase 5：name、opener、cross-origin、sandbox 与 COOP

- group-level named target registry 和 related Page 查找；
- opener graph、setter/closed behavior；
- full restricted WindowProxy whitelist/descriptor 行为；
- popup blocker 和 transient activation consume；
- `allow-popups` / `allow-popups-to-escape-sandbox`；
- `noopener` / `noreferrer` group/agent/storage policy；
- COOP group switch 和 old proxy endpoint sever；
- focus/blur/close observable state。

### Phase 6：删除 lightweight 专用模型

当所有 production popup 都由真实 auxiliary Page 承担后删除：

- `LightweightPopupBrowsingContextRecord`；
- popup shared-context alias registration；
- popup 专用 `with(window)` script wrapper；
- mirrored popup parser/loader/lifecycle；
- protocol 中创建第二 PageVM/第二 navigation 的路径；
- root/child/lightweight 三分的 popup source 特例，改用统一 browsing-context identity。

删除应由 grep、focused nextest、WPT slice 和 CDP integration test 共同证明，不能只因
类型暂时没有调用而移除。

## 验收不变量

迁移完成至少要满足以下不变量。

### Identity / realm

- 同一 popup 的 WindowProxy 跨 navigation 身份稳定。
- popup 与 opener 是不同 realm：`popup.Array !== opener.Array`，global lexical state
  不共享。
- same-origin 同步读取、函数调用、DOM object access 作用于 popup 的真实 Document。
- cross-origin access 只暴露允许 surface，round trip 不复活旧 LocalWindow。
- initial empty same-origin 首次 commit 只在满足明确 policy 时复用 LocalWindow。

### Ownership / lifecycle

- 一个 browsing context 只有一个 current Document owner、一个 history 和一个 loader。
- 一个 navigation token 只有一个 authoritative load；redirect 不算第二 owner。
- 旧 Document/realm 的 timer、fetch、module、worker、message 和 callback 不能修改新
  generation。
- popup 可独立于 opener 存活和关闭。
- named target、opener edge、close state 只有一个 registry source of truth。

### CDP

- 一个 auxiliary top-level context 对应一个 target 和同一个 renderer Page residence。
- opener handle mutation 与 CDP `Runtime.evaluate` / DOM snapshot 观察同一个 Document。
- target create/attach/context-created/load/target-destroyed 顺序稳定。
- `waitForDebuggerOnStart` 不靠 sleep，且不会让 CLI 与 CDP 使用不同完成条件。
- Runtime object id、context id、binding、object group 和 exception event 按 target/session
  精确路由，即使多个 Page 共用 isolate。
- `Target.closeTarget` 与 `window.close()` 汇合到同一 close transaction。

### Policy / storage / network

- blocked popup 不创建隐藏 Page、target 或 storage namespace，也不误消耗 activation；
  allowed creation 按 Chromium 语义消耗 activation。
- `noopener` / `noreferrer` 返回值、opener、referrer、name/group 和 storage 行为由同一
  policy result 决定。
- session storage namespace 只分配/clone 一次。
- Fetch/Network 事件、cookie、cache、redirect 和服务端副作用来自唯一 loader。

## 测试建议

### 已有聚焦 nextest

popup 当前路径：

```bash
cargo nextest run -p lightmount-renderer-v8 -p lightmount-protocol \
  -E 'test(window_open_non_about_returns_lightweight_popup_and_dispatches_load) | test(window_open_emits_popup_target_created_from_runtime_work) | test(window_open_hands_off_session_storage_snapshot_and_initial_storage_key) | test(rust_cdp_playwright_multi_context_popup_route_and_evaluate_contract)' \
  --no-fail-fast --status-level fail --final-status-level fail
```

child stable proxy / realm 基线：

```bash
cargo nextest run -p lightmount-renderer-v8 -p lightmount-protocol \
  -E 'test(initial_empty_same_origin_commit_reuses_local_window_exactly_once) | test(same_origin_child_window_migration_to_cross_origin_installs_denied_surface) | test(child_window_proxy_identity_survives_cross_origin_round_trip) | test(per_page_isolate_policy_uses_distinct_isolates_and_isolates_contexts) | test(per_page_isolate_policy_reuses_navigation_isolate_and_replaces_contexts) | test(popup_target_diagnostics_report_distinct_page_vm_document_isolates)' \
  --no-fail-fast --status-level fail --final-status-level fail
```

迁移后第二组中的 per-page-isolate 测试需要被更精确的 script-agent policy 测试替代，
不能简单删除隔离覆盖。

### WPT 优先簇

优先运行本地 Chromium WPT checkout 中这些目录/文件：

- `html/browsers/browsing-the-web/navigating-across-documents/initial-empty-document/window-open-*`
- `html/browsers/browsing-the-web/navigating-across-documents/multiple-globals/`
- `html/browsers/windows/auxiliary-browsing-contexts/`
- `html/browsers/windows/browsing-context-names/`
- `html/browsers/the-window-object/open-close/`
- `html/browsers/the-window-object/window-open-noreferrer.html`
- `html/browsers/origin/cross-origin-objects/`
- iframe sandbox popup cases；
- anchor/area/form `_blank` + opener/noopener/noreferrer cases。

每轮要记录 Lightmount commit、Chromium/WPT commit、binary build profile、case timeout、
parallelism、case list 和 subtest 详情。focused WPT 不应覆盖仓库 full baseline list。

### 合并前仓库验证

按照仓库约定：

```bash
cargo nextest run --no-fail-fast
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
```

禁止使用 `cargo test`。若阶段只跑 focused nextest，提交说明必须列出未跑 full suite 的
原因。

## 风险与停止条件

### 最高风险：fresh-by-default / selective-related script-agent policy

`PageVm` / `RendererPageScriptEnvironment` 默认仍为每个 fresh Page 建立 agent；只有显式
related auxiliary admission 才共享 isolate。已有大量
`per_page_isolate_policy_*` 测试保护：

- isolate/context 隔离；
- navigation 替换 context 但复用 Page isolate；
- timer/fetch/module/worker/IndexedDB 的 page-owned routing；
- CDP object id、context id、binding 和 object group 的 page-local scope。

推荐方案不是“删除隔离”，而是把隔离 key 从 `PageId` 提升为 `ScriptAgentId +
Page/Realm owner`：V8 memory heap 可共享，所有可观察路由仍必须精确到 Page/Document/
session。

### reentrancy / borrow 风险

`window.open()` 在 opener V8 callback 内发生，此时 opener `ScriptVm` 正在执行。同步创建
另一个 Context/Page 不能通过重入同一个 owner loop、临时释放借用或全局裸指针实现。
Phase 3 必须在已经验收的 selective agent 基础上确定安全的 `PendingAuxiliaryPage`
构造边界和 ownership transfer。

### inspector 风险

多个 target 共享 isolate 后，inspector backend、execution context id 和 remote object
registry 很容易错误地按 isolate 全局化。任何一个 target 能 evaluate/release 另一个
target 的 object 都是阻断问题。pause loop 是 isolate 级，但 target close、Page output
journal、queued command 和 frontend session 必须按 renderer agent/Page 路由；Phase 3
第一纵切已修正 close/command/session 路由，跨 target remote-object 隔离仍由 Phase 2B
矩阵保护。

### COOP / agent split 风险

V8 object 不能跨 isolate 搬迁。若初始 popup 与 opener 共享 isolate，COOP 导航后的
group/agent split 必须新建 realm并把旧 WindowProxy 切到 disconnected/remote endpoint，
不能尝试移动 context。

### 明确停止扩大修改

出现以下任一情况时，应停止继续补调用方并重新检查 owner 设计：

- 同一个 popup 修复要复制到 lightweight 和 target Page；
- 为了等 protocol 创建 target，需要 sleep、drain、retry 或无限轮询；
- CLI 成功而 CDP 失败，且两者等待的是不同 Page/loader；
- stale popup navigation/realm completion 能写入新 Page residence；
- shared isolate 只能靠裸指针、泄漏 cache 或仅 debug assertion 保持路由；
- “性能提升”来自跳过 target event、DOM、network 或正确性检查；
- popup 被建模成 child 后开始携带 parent/frameElement/load-blocker 特例。

## 决策记录

本次评估建议记录以下架构决定：

1. auxiliary popup 必须只有一个 authoritative Page/Document/navigation owner。
2. protocol target 绑定 renderer-created Page，不再复制 Page。
3. 采纳 child-frame stable WindowProxy/realm 基础，但先抽成通用 browsing-context
   primitive；不把 popup 当隐藏 child frame。
4. 保留 opener 的 popup 需要独立 realm 和同步 WindowProxy access，因此要引入可承载
   多 Page realm 的 script agent；第一版共享 isolate，后续可演化 remote endpoint。
5. popup 仍是独立 PageVM/CDP target，可在 opener 关闭后存活。
6. `noopener` / COOP 是 group/agent/endpoint policy，不只是 `window.opener = null`。
7. Phase 2B selective shared-agent 实验已经通过；Phase 3 第一纵切只为 renderer 明确
   新建的 auxiliary context 打开 production relationship admission。fresh Page 与
   `noopener` 仍隔离，不恢复 renderer-owner-wide sharing，也不先做大范围 popup 迁移。
8. 最终删除 lightweight popup 专用 loader/parser/script/realm alias，避免长期双栈。

## 相关文档

- [Child Browsing Context Current Boundary](child-browsing-context-current.md)
- [V8: Isolate vs Context](v8-isolate-vs-context.md)
- [Chromium Context / Lazy WindowProxy / ScriptState](chromium-context-lazy-windowproxy-scriptstate-2026-06-15.md)
- [Popup Target and JavaScript Navigation Lifecycle](popup-target-and-javascript-navigation-lifecycle-2026-07-22.md)
- [CDP Target Engine and Initial Popup Document Case Study](cdp-target-engine-and-initial-popup-document-case-study-2026-05-24.md)
- [CDP Initial Empty Document Chromium Alignment Plan](cdp-initial-empty-document-chromium-alignment-plan-2026-06-18.md)
