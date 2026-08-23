# Popup / Auxiliary Browsing Context：现状、Chromium 对照与统一方案

日期：2026-08-02

状态：架构评估与分阶段迁移设计；`popup-refactor` 已完成 Phase 1 primitive 抽取、
Phase 2A script-agent identity / current-policy baseline，以及 Phase 2B 的 selective
shared-agent ownership、V8 foreground routing、核心可行性矩阵和两次 release 长序列
内存验收。Phase 2B 的 test-only 可行性实验已完成；production auxiliary admission
仍未开放，下一步是 Phase 3 的真实 initial-empty auxiliary Page。本文不表示 popup
迁移已经完成；尚未落地的拟议类型名只用于表达责任边界。

代码基线：

- Lightmount 原始评估：`2e351a545b04`，分支 `cdp-better-4784u`；实施状态以
  `popup-refactor` 当前分支为准。
- Chromium：`a03603fe9af6`，本地 checkout `/home/donoughliu/chromium/src`。

本文讨论 HTML `window.open()`、`target=_blank` 和命名 target 创建的 auxiliary
top-level browsing context。它不是 Blink 用于 `<select>`、权限气泡等 UI 的
`PagePopup` 机制。

## 结论

当前 popup 不是“缺几个 API”，而是同时存在两套相互独立的实现：

1. opener `PageVM` 内的 `LightweightPopupBrowsingContextRecord` 提供同步返回给 JS
   的 Window-like facade，并自行加载、解析和执行 popup 文档；
2. protocol 收到 renderer output 后再创建一个独立 popup target、独立 `PageVM`
   和独立 document isolate，并再次导航同一个 URL。

两条路径各自已有不少能力，但它们不是同一个 browsing context。当前测试甚至把
“popup URL 恰好有两个 load owner”写成了预期。这会导致网络副作用重复、opener
拿到的 Window 与 CDP target 看到不同 DOM、`close()` 与 target 生命周期分裂，继续
补 facade 只会扩大同步成本。

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
owners”。这不是推测，而是当前入库合同。

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

当前源码已经引入 typed `ScriptAgentId`，由 document isolate holder 分配并通过
Lightmount runtime memory diagnostics 暴露。`RendererPageScriptEnvironment` 是当前
agent identity 的稳定宿主：

- 同时存活的两个顶层 Page script environment 必须报告不同 `ScriptAgentId`；
- 同一个稳定 Page 的 cross-document navigation 必须保留 `ScriptAgentId` 和 main
  WindowProxy，但替换 Document realm/context generation；
- child default world、isolated world 和同 Page navigation generation 复用所属 Page
  agent；
- diagnostics 当前明确报告 `scriptAgentScope = page-script-environment`，尚未开放
  related-page admission。

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

实施状态（test-only selective admission，production policy 不变）：

- document isolate 现在持有 `RendererScriptAgentV8ForegroundTaskRouter`，production
  fresh Page 默认只注册一个 Page member，因此现有 per-Page containment policy 不变；
- stable `RendererPageScriptEnvironment` 持有 RAII agent membership；same-Page
  replacement 复用 membership，Page slot retirement 先撤销 route，再清理 Page tasks；
- `#[cfg(test)]` 的 related-page reservation 可以从同 renderer owner 的 live source
  Page 共享 isolate holder，同时创建独立 Page environment、main WindowProxy、V8
  Context、task sources、output journal 和 Inspector binding；production popup 尚不能
  使用该 admission；
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
长序列内存门均有证据。这个结论只授权进入 Phase 3，不授权把 owner-wide sharing 恢复
为默认，也不表示 popup 已完成。WindowProxy link 与 related admission 仍只在
`#[cfg(test)]` 暴露；production auxiliary Page ownership、multi-session Inspector event
顺序、SharedWorker/ServiceWorker、跨 origin agent split 和真实 popup 并发 close/navigation
仍分别属于 Phase 3-5。

### Phase 3：真实 auxiliary initial empty Page

- 在 renderer owner runtime 中创建 `PendingAuxiliaryPage`；
- 为它创建 stable main WindowProxy、独立 V8 Context 和 initial empty Document；
- `window.open('about:blank')` 返回这份真实 proxy；
- protocol target bind/adopt 同一个 `RendererPageResidenceIdentity`；
- immediate `document.write`、storage clone 和 Runtime evaluate 指向同一个 Document；
- close 从第一天就走统一 transaction。

优先只切 `about:blank` 垂直路径，因为它能验证最关键的同步创建、realm identity 和
target adoption，又不先引入网络导航。

### Phase 4：non-empty URL 单一导航

- pending URL 只交给 auxiliary Page owner；
- 接入 target admission / wait-for-debugger；
- Fetch/Network interception 绑定同一个 navigation token；
- 删除 lightweight mirrored load；
- 把“exactly two load owners”测试改为“exactly one authoritative navigation owner”；
- 验证 redirect、204/205、error page、history、DCL/load/done 和 opener immediate
  mutation。

完成标志：同一个 popup URL 不再因为实现结构产生两个请求。

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

### 最高风险：当前 per-page-isolate policy

`PageVm` / `RendererPageScriptEnvironment` 当前明确按 Page 持有 isolate。已有大量
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
target 的 object 都是阻断问题。

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
7. Phase 2B selective shared-agent 实验已经通过；production 仍保留 per-Page 默认隔离，
   只允许 Phase 3 通过显式 browsing relationship admission 打开窄路径，不恢复
   renderer-owner-wide sharing，也不先做大范围 popup 迁移。
8. 最终删除 lightweight popup 专用 loader/parser/script/realm alias，避免长期双栈。

## 相关文档

- [Child Browsing Context Current Boundary](child-browsing-context-current.md)
- [V8: Isolate vs Context](v8-isolate-vs-context.md)
- [Chromium Context / Lazy WindowProxy / ScriptState](chromium-context-lazy-windowproxy-scriptstate-2026-06-15.md)
- [Popup Target and JavaScript Navigation Lifecycle](popup-target-and-javascript-navigation-lifecycle-2026-07-22.md)
- [CDP Target Engine and Initial Popup Document Case Study](cdp-target-engine-and-initial-popup-document-case-study-2026-05-24.md)
- [CDP Initial Empty Document Chromium Alignment Plan](cdp-initial-empty-document-chromium-alignment-plan-2026-06-18.md)
