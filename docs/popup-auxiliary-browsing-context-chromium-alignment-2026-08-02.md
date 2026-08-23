# Popup / Auxiliary Browsing Context：现状、Chromium 对照与统一方案

日期：2026-08-02；最后更新：2026-08-04

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
`noopener` 显式使用 fresh agent。第四个提交又把 opener 同步拿到的同一 V8
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
`javascript:` URL 和 target admission 前的早期任务在当时仍需后续纵切收敛。Phase 5 第一纵切 A
已经把 child-frame stable WindowProxy 的 V8 access-check/handler primitive 扩展到同一
related-page script agent 中的真实 top-level Page：opener 现在可在跨源 commit 后观察 Chromium
restricted Window whitelist、own property/descriptor/symbol 形状和稳定 identity；跨 Page
`postMessage` 会保存真实 source WindowProxy 与 source origin，`window.location =` /
`location.replace()` 则进入目标 Page 已有的 navigation owner。该纵切同时修正了 shared isolate
中 host-local opaque LocalWindow id 碰撞造成的伪同源，以及 child primitive 的 configurable
descriptor、well-known symbol value 和 `[object Object]` 形状。动态 `closed`、`close()` /
target teardown 的 Phase 5 第二纵切 B 也已接通：related Page 的 same-origin / cross-origin
`close()` 会同步进入唯一 `Closing` 状态，经 target Page 自己的 output FIFO 交给 protocol，最终与
`Target.closeTarget` 共享 Page discard 和 stable WindowProxy closed facade；`open(url); popup.close()`
不会再启动 destination navigation。Phase 5 第三纵切 C 也已经完成 live relation/child projection：
related top-level cross-origin WindowProxy 的 index/name 不再复制到静态 surface，而是从目标 Page 的
child registry 动态解析到既有 stable child WindowProxy；插入、移除、重命名、`then` / `open` named
shadow 和 ownKeys 排除 named child 均有回归。opener 则由 Page-scoped edge 跨 realm 保存；显式
`window.opener = null`、opener 最终 discard 和后续 navigation 使用同一 sever 结果，关闭 popup 自身仍
按 Chromium 保留尚存活的 opener。script-closable policy、beforeunload/unload、focus transaction、
Phase 5D 第一纵切 D1 也已收敛 restricted Location internal methods：ownKeys 只保留
`href` / `replace` / `then` 和 3 个 fallback symbol，unknown get/has/descriptor/set/delete/define、
prototype mutation 与 preventExtensions 均按 WPT 形状处理。第二纵切 D2 进一步完成 restricted
Window internal methods：denied/unknown name 和 out-of-range index 不再以伪 own accessor 或
`undefined` 泄漏，delete/define/set、null prototype、extensibility 和 exact ownKeys 顺序均对齐本地
Chromium WPT；named child 可遮住 `document` / `open` / `then`，但不能遮住 `focus` / `close` 等
cross-origin exposed property。D2.5 又把同一 projection owner 扩展到 generic nested child：live
Document 的 get/query/descriptor/enumerator/length 全部直接读取 scoped child registry，同一 Document
内 insert/remove/rename 不再复活 surface snapshot；预物化 stable child WindowProxy facade 也改用唯一
security token 和正式 access surface，不会泄漏调用方 raw global。D3a 进一步完成
`CrossOriginPropertyDescriptorMap` 的 accessing-Realm 侧：Window/Location 的 method、getter、setter
按 incumbent Realm 缓存，具有该 Realm 的 `Function.prototype`、标准 name/length 与 accessor descriptor；
共享 wrapper 的 native callback 则从 receiver 解析真实 target Context/Page owner，避免在 opener host 上执行
popup 的 close、postMessage 或 Location navigation。D3b 又让非 top observer 的 `parent` / `top`
直接复用 stable top-level WindowProxy，并在 index/name lookup 时按 observer Realm 与 target child
origin 决定是否 materialize 同一个 stable child proxy；same-host sibling 与 related Page 跨 host
路径都已有回归。Phase 5E 第一纵切 E1 现已把 production 的非命名、非 `javascript:`
`window.open(..., "noopener|noreferrer")` 以及 hyperlink `_blank` implicit/explicit noopener
切到唯一 Fresh auxiliary Page：调用方返回 `null`，不创建 lightweight browsing context、镜像
Document 或第二 loader；initial empty Document referrer、目标导航的网络 `Referer` 与提交后
`document.referrer` 由 creator policy 一次冻结为三个独立投影。Phase 5E 第二纵切 E2A 又把
opener-preserving、非 `javascript:` 的 named `window.open()` 接到同一真实 initial Page，并在
related-page group 内以 live name/lifecycle registry 解析 stable WindowProxy；reuse activation 携带
精确 renderer Page residence，protocol 的 target-name map 降级为 projection。动态 `window.name`、
navigation 后 name 保留、closed target 排除，以及 existing target 的 noopener/null-return/opener 保留
均由同一 owner 处理。E2B 随后把新建 named noopener/noreferrer `window.open()` 收敛为保留真实 name
的 private Fresh Page；E2C 又把普通 named hyperlink 接入同一 renderer lookup/creation authority：existing
named iframe 仍优先，related Page 精确复用，新建 opener-preserving target 标记 `Related`，新建
noopener/noreferrer target 标记 `FreshNamed` 且不进入 protocol 全局 name projection。E2D 现已在 form
submission owner 中完成同一条 named / `_blank` 纵切：submitter/form/`<base target>` 先确定 effective
target，现有 named iframe 仍优先；related Page hit 或 Related/Fresh miss 与完整 GET/POST request、三类
referrer、Page reservation 和 form-specific target `NavigateEvent` 一起冻结。protocol 的
`Held → Published → Consumed` claim 现在携带 method、raw body、Content-Type 与 request kind，POST
不会再退化成 GET，也不会把 `_blank` 错误导航到 opener。E2E 进一步把 ordinary-name
`window.open()` 与 hyperlink 的 source-subtree / current Page / ordered related Pages 完整 frame-tree
查找接回 renderer owner：related Page 的 nested child 由它自己的 `JsContextHost` 导航，candidate 会执行
普通 origin/ancestor `CanNavigate`，初始 inherited `about:blank` 的 tuple origin 也不再被 URL 重算成 opaque
origin。E2F 已让 ordinary named form 的 exact GET/POST request 消费同一 resolver；E2G 又让 source form
持有 typed Page/child scheduler route 和精确 navigation-load generation，跨 Page retarget 可以取消旧 child
task、loader 与 parser ledger，而不会误取消同一 child 后来发生的无关导航。E2H 进一步把 child
`window.open()`、hyperlink 与 form 命中 current top 时的 initiating Window/Document、source URL、
Referrer Policy 和 suppression 冻结进同一 typed request；target Page 仍是唯一 scheduler/loader owner，
但 redirect、cross-origin 与 Fetch URL override 不再把 target root 误当 initiator，最终
`document.referrer` 也按实际 commit URL 重算。`javascript:` 的完整
target-realm execution、sandbox、top-level activation/opener 特例、focus transaction、COOP group sever 与
remote/disconnected endpoint 仍未完成。

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

`popup-refactor` 当前已经为 opener-preserving、非 `javascript:` URL（非命名、普通 named
`window.open()`、ordinary named hyperlink 和 full-creator form auxiliary target）建立 production
迁移路径：creator 与 target 共享同一
stable WindowProxy、initial realm、Document 和 Page residence；non-empty destination 在 target admission
后从该 Page 发起一次 replacement navigation，opener host 不再保存对应 lightweight Document record 或
启动 mirrored loader。非命名及普通 named 的 `noopener` / `noreferrer` 创建路径与 hyperlink/form
`_blank` 也已经进入独立 Fresh Page 的 single-owner 路径，只是不向 creator 暴露 local WindowProxy；
form POST 的 exact body/header 则沿同一 target-owned navigation claim 发出。上述双实现判断仍适用于
`javascript:` URL、缺少完整 creator capability 的 child-frame hyperlink/form source 和其余尚未迁移入口；
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
| authoritative Page | related 非命名、普通 named `window.open()`/hyperlink/full-creator form，以及 Fresh noopener 非命名/普通 named 路径已统一；`javascript:`、部分 child-source 路径仍可让 lightweight Page 与 target Page 并存 | 未迁移入口的 DOM、history、lifecycle 仍可分叉 |
| navigation owner | 已迁移入口一个 URL 只有一个 owner；legacy 入口仍可能有两个 load owner | 未迁移入口仍可能重复请求、cookie、服务端副作用和计时 |
| top-level initiator / referrer | E2H 已让 current-top `window.open()`、hyperlink/form 与 related target request 保存 exact source Window/Document 和 policy；preflight、transport redirect/Fetch URL override、最终 `document.referrer` 不再从 target root 反推 | redirect response 自身更新 policy、完整 Fetch response-stage override、`javascript:` 与 sandbox/origin creation policy 仍需独立收口 |
| realm | related 非命名/普通 named 路径（含 full-creator form）使用真实独立 realm；Fresh noopener（含普通 named）不向 creator 暴露 local proxy；legacy facade 仍可能共享 opener `Context` | `javascript:`/部分 child-source legacy handle 仍可能与 CDP execution context 无关 |
| synchronous access | 已迁移 related path 直接访问 target 的真实 Document；legacy facade 仍模拟部分 `w.document` | 未迁移入口的写入不会自然出现在 target DOM |
| cross-origin WindowProxy | 有局部 restriction/facade | 不是完整 outer/inner 或 local/remote proxy 模型 |
| `window.close()` | 未迁移 lightweight 路径仍与 target 分裂；真实 related auxiliary Page 已在 Phase 5B 统一；Fresh noopener 不向 opener 暴露 close handle | named/`javascript:` 等 legacy 路径仍可能让 `closed`、targetDestroyed、资源回收不一致 |
| focus/blur | top-level Window 上仍有 no-op surface | named-target focus 和事件不完整 |
| named target | E2A-E2D 已统一 `window.open()`、full-creator hyperlink/form 的 related lookup、Fresh group split 和 exact Page handoff；E2E 已补 child-source、related nested local frame-tree order 与普通 nested `CanNavigate`；E2F/E2G 已让 form 消费 typed target，并跨 Page 保存 cancellable scheduler generation；protocol map 仅为 projection/legacy fallback | 完整 sandbox/top-level `CanNavigate`、remote/fenced/COOP 后查找仍可能不一致 |
| opener / COOP | 有 opener suppression 字段和局部 policy | 没有完整 browsing-context-group split / opener sever |
| popup blocker | userGesture 被观测，部分 policy 已冻结 | 没有统一的 transient activation 消耗和创建 gate |
| sandbox | 有部分 frame policy 输入 | `allow-popups` / escape-sandbox 创建边界不完整 |
| initial empty Document | 已迁移 related 路径由 target 采纳同一份；Fresh noopener 只由目标 Page 创建；legacy 双栈仍各有一份 | 未迁移入口的同步 mutation 与 target attach 无法指向同一对象 |
| script loader | 已迁移入口只使用 Page loader；form POST request 也由 selected target 的唯一 loader 消费；legacy lightweight 仍有专用 parser/script wrapper | `javascript:`/部分 child-source 的 loader、module、CSP、currentness 仍会漂移 |

因此，继续为 lightweight 路径分别补 module、dynamic import、beforeunload、COOP、
cross-origin descriptor、CDP Runtime context 等功能，会让每个修复都复制到另一条路径。

### 7. 历史 WPT 风险快照（必须重跑后才能用于当前验收）

对 `lightmount-benchmark/wpt-cross-current/{passed,failed,timeout}-cases.txt` 使用下面的
粗粒度关键字切片：

```text
window-open|window_open|browsing-context-names|noopener|noreferrer|opener|auxiliary
```

当前清单的静态关键字计数为：

| 状态 | case 数 |
|---|---:|
| pass | 30 |
| fail | 12 |
| timeout | 28 |

这些 case list 的采集早于最近 E2A-E2H，不是当前 `popup-refactor` 的运行结果，也不能作为新 owner/scheduler
路径的验收证据。下一轮 WPT 应固定 commit、目标集、timeout、并发度与正确性输出后重新分类；在此之前，
下面的 case 只能用于选择 focused slice。

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
| named target | frame tree + related pages + policy | E2A 已统一 related `window.open()`；其余 producer/group split 仍有 legacy registry | group-level registry + single context identity |
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
- 该阶段的 named target、`noopener` / `noreferrer` 和 `javascript:` URL 仍由 legacy policy/path
  处理；其中新建非命名 noopener/noreferrer 后续已由 Phase 5E1 迁移，named/group 与
  `javascript:` semantics 仍未完成；
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
`javascript:` 或 close transaction；close transaction 后来已由 Phase 5B accepted-close 纵切完成，
其余仍是 Phase 4/5 后续切片。

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

同一回归也精确暴露了下一层边界：当时 opener 保存的 stable popup WindowProxy 在跨源 commit 后仍
保持 identity，但 `.closed` 会得到 `SecurityError`，说明 top-level related Page 尚未接入 child-frame
已有的 restricted cross-origin access surface。Phase 5 第一纵切 A 已在下节按完整 allowlist 处理该
边界，而不是只为 `.closed` 开洞；原 identity 回归也已扩展为 Window/Location descriptor、ownKeys、
`postMessage` source 和 target-owned location navigation 的端到端矩阵。动态 close state 仍属于下一
纵切；该记录描述 Phase 5A 当时边界，Phase 5B 现已用 shared Page lifecycle authority 替换常量值。

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
`noopener` / COOP、cross-origin WindowProxy 的每调用方细节仍按 Phase 5 后续纵切处理；动态 close
transaction 已由 Phase 5B accepted-close 闭环继续收敛。

### Phase 5：name、opener、cross-origin、sandbox 与 COOP

#### 第一纵切 A：复用 stable WindowProxy 的 related top-level 跨源 surface

本纵切已完成。范围刻意限定为：两个真实 top-level Page 已通过 production auxiliary admission
进入同一个 related-page script agent，opener 持有 popup 的实际 stable main WindowProxy，随后 popup
提交不同源 Document。它不建立 named-target registry，不改变 `noopener` / COOP 的 fresh-agent
policy，也不宣称 close transaction 已完成。

##### Chromium / WPT 合同

直接对照的主要证据是：

- Chromium `third_party/blink/renderer/bindings/core/v8/window_proxy.h`：同一个 browsing context
  保留稳定 WindowProxy，访问安全检查位于 proxy/realm 边界；
- WPT `html/browsers/origin/cross-origin-objects/cross-origin-objects.html`：锁住 Window / Location
  allowlist、own descriptor、ownKeys、well-known symbols、prototype、内部方法和每 incumbent wrapper；
- 同目录 `cross-origin-objects-function-{caching,length,name}.html`：锁住函数 identity、`name`、
  `length` 和 descriptor 返回的函数缓存；
- 同目录 `window-location-and-location-href-cross-realm-set.html`：锁住 Location setter 的 receiver、
  URL coercion 和异常 realm；
- popup error-Document 行为继续参考 Chromium `NavigationRequest::CommitErrorPage` 路径；该路径证明
  transport failure 替换 Document，而不是销毁 auxiliary browsing context 或 opener edge。

对不含子 frame 的跨源 Window，WPT 的 string allowlist 是：

| 类别 | 名称 | 本纵切状态 |
| --- | --- | --- |
| identity / relation | `window`、`self`、`frames`、`parent`、`top`、`opener` | 已接真实 stable WindowProxy；top-level 的 parent/top/self 均是自身，opener 来自实际 V8 opener slot |
| live scalar | `closed`、`length` | 可跨源读取；`length` 从目标 Page child count 读取，`closed` 由稳定 Page environment 的 `Active → Closing → Closed` authority 动态投影 |
| callable | `postMessage`、`blur`、`close`、`focus` | descriptor/name/length/缓存形状已对齐；`postMessage` 已跨 Page 交付，`close()` 已进入 target-owned close transaction，`blur` / `focus` 仍无动态事务 |
| navigation | `location` setter、`location.href` setter、`location.replace()` | 已进入目标 Page 原有 Location/navigation owner；读 `href` 与其他敏感属性抛 `SecurityError` |
| promise assimilation | `then` | own、值为 `undefined`；没有名为 `then` 的 child 时不会把 WindowProxy 当 thenable |
| well-known symbols | `Symbol.toStringTag`、`Symbol.hasInstance`、`Symbol.isConcatSpreadable` | own、non-enumerable、non-writable、configurable，值均为 `undefined` |

`globalThis` 不在 HTML cross-origin Window allowlist 中。本纵切从共用 child primitive 中移除了此前
错误暴露的 cross-origin `globalThis` identity alias；读取它与 `document`、`name`、任意未知属性
一样抛调用方 realm 的 `SecurityError`。well-known symbol 不再伪造 `"Window"` / `"Location"`
tag，因此 `Object.prototype.toString.call(crossOriginWindow)` 和 Location 都是 WPT 要求的
`[object Object]`，不是 `[object Window]` / `[object Location]`。

当前已锁住的 descriptor 规则是：

- allowlist string/symbol property 都作为 own property 投影；
- 普通值和方法 `writable:false`、`enumerable:false`、`configurable:true`；
- `location` 是带 getter/setter 的 non-enumerable、configurable own accessor；
- 数字 child index 应为 `writable:false`、`enumerable:true`、`configurable:true`；
- 未知 Window property 的 read、descriptor 和 `hasOwnProperty` 都抛 `SecurityError`；
- 无 child 的 `Object.getOwnPropertyNames(window)` 精确只含 14 个 string allowlist 名称，
  `Object.keys(window)` 为空，三个 symbols 按 WPT 顺序位于 symbol own keys 中；
- cross-origin Window / Location prototype 为 `null`。

##### Owner 设计：实际 proxy + target-owned surface

实现没有给 popup 再建一个“restricted facade”。现有 Window global template 本来就安装了 child-frame
使用的 V8 security-token access check 和 named/indexed property handlers；本纵切把其授权域从“同一
`JsContextHost` 的 top/child realm”窄化扩展为“显式 related、共享同一 script-agent isolate 的两个
current top-level default realms”：

```text
opener current Context
  -> saved popup stable WindowProxy (target Page owns the object)
     -> V8 security-token/access-check
        -> same effective tuple origin: access actual target global
        -> cross origin: target JsContextHost cross-origin access surface
           -> identity / descriptor / ownKeys
           -> target postMessage queue
           -> target Location navigation owner
        -> unrelated Page / stale realm / non-top endpoint: deny
```

关键责任边界如下：

1. `RendererPageScriptEnvironment::is_related_page_peer` 只接受不同 Page id、相同 isolate identity。
   一个 document isolate 对应一个 script agent；V8 access-check 发生时 holder 已被可变借用，因此热路径
   不能再次借 holder 查询 script-agent id。第一次回归确实捕获了该 reentrant `RefCell` borrow，当前
   实现使用稳定 `Rc` identity，不引入裸指针 cache 或临时释放借用。
2. `window_access_check_callback` 只有在两个 context 的 host 不同且满足 related-page gate 时才进入
   cross-Page origin 判断；fresh Page、`noopener` Page、stale owner 和非 top-level endpoint 不因此
   获得能力。
3. 每个 main default realm bootstrap 在目标 `JsContextHost` 中建立 cross-origin access surface，但
   surface 的 identity slots 指回 Page environment 持有的实际 stable main WindowProxy。navigation
   替换 Context 后重新建立 target-owned surface，调用方保存的 proxy object 本身不变。
4. surface 在恢复 navigation-persistent opener slot 之后读取实际 opener value。它没有根据 CDP
   `openerId` 反造 JS 对象，也没有把 opener target metadata 当作 JS graph authority。
5. unknown descriptor 路径由 handler 显式在 lexical/incumbent realm 创建 DOM `SecurityError`，避免
   V8 对 `undefined` descriptor 生成错误 realm 的普通 `TypeError`。

这正是“复用 child-frame stable WindowProxy/realm 基础”的含义：共享 access-check、origin、
handler、descriptor 和 Location primitive；popup 仍是独立 top-level Page/target，没有
`frameElement`、parent load blocker 或 iframe owner 特例。

##### child Document preload registry 必须按 owner 冻结

workspace 并发门禁同时暴露了 child-frame 基础中的一个既有时序缺口。一个 child Document 已经创建，
但它的 realm-materialization Page task 可能因调度负载晚于
`Page.addScriptToEvaluateOnNewDocument(runImmediately:true)` 执行；旧实现到 materialization body 才读
Page-wide 最新 script registry，于是本应只在当前 top-level world 立即执行的新脚本，又被追溯重放到
更早创建、同名的 child isolated world。聚焦运行通常先完成 child task，因此看不见；workspace 高负载
能稳定把它放大为 `typeof childMarker === "string"`。

这里没有给 CDP 命令加 drain、retry 或 sleep。责任边界提升到 exact child Document owner：

- initial-empty Document、普通导航 commit 和 `document.open()` replacement 创建新 owner 时，冻结当时
  可见的完整 document-start script registry；
- 后续 default/named child realm materialization 只消费该 owner 的快照，不再读取 Page-wide 最新脚本；
- 快照与 Document owner 同寿命，同一 Document 的测试性/内部 realm replacement 继续使用同一份配置，
  只在 owner retirement 时清理；
- later registry update 仍由 top-level `runImmediately` 处理，并被之后创建的 child Document 捕获，不会
  因修复而丢失 future-document preload。

低层回归用同一 Page 锁住“更新前 child 不追溯注入、更新后 child 正常继承”；原 protocol
world-name 回归在并发 core+renderer 负载下从修复前第 6/22/36 次内可复现，变为修复后连续
100 次通过。这项修复是复用 child-frame realm primitive 的必要收敛，不是 popup 调用点补丁。

##### opaque origin：不能比较 host-local LocalWindow id

真实 error popup 回归暴露了一个 shared-isolate 特有问题。`WindowAccessOrigin::Opaque` 过去用
`WindowExecutionContextOwner::Frame(LocalWindowId)` 作为 non-serialized identity；该 id 只在单个
`JsContextHost` 内唯一。两个 Page 都可能分配 `LocalWindowId(1)`，跨 host 直接比较会把两个独立
opaque origin 错判为同源，并绕过 restricted surface。

related-page 跨 host origin gate 现在不把 opaque owner 数值当作全局 nonce；两个 independently
created opaque realm 一律不能靠碰撞互访。initial `about:blank` 合法继承 creator 的 opaque 或
`document.domain`-mutated effective origin 时，Page admission 已把 creator 的精确 V8 security token
交给 initial Context，因此同源访问在到达该 fallback gate 前已经成立。当前生产回归覆盖
`data:` opener → initial inherited Document → opaque error Document 的转变。后续仍需用 WPT 覆盖
非 initial 的 inherited opaque navigation、sandbox-forced opaque origin 和 COOP split，不能把
host-local owner id 扩成伪全局 nonce。

##### cross-Page `postMessage`

跨源 `postMessage` 继续调用目标 Window surface 上的同一 native binding，但 acceptance 时需要同时
知道 target 和 incumbent source：

- binding 的 current context/host 是目标 popup Page，因此 payload 进入 popup 的 Page-owned
  WindowMessage task queue，target origin 在 dispatch 时再次检查；
- incumbent context 是 opener Page；只有它与 target 属于同一 related script agent、且是 current
  top-level default realm 时才接受；
- acceptance 保存 source origin、source owner/realm token，以及 source Page 的实际 stable main
  WindowProxy `v8::Global`；
- `MessageEvent` materialization 直接使用该 source proxy，所以 target 中
  `event.source === opener`，而不是 target global、`null` 或一份 synthetic facade；
- structured clone、transfer list、`messageerror`、target-generation/currentness 和 task ordering 继续
  复用原 WindowMessage owner path。

端到端测试在 error popup realm 注册 listener，由 opener 对保存的跨源 WindowProxy 发送 object，
锁住 `{data, origin:"null", sourceIsOpener:true}`。Phase 5B 又补上 source/target Page 最终关闭后的
断开边界：保留的 stable proxy 继续可读 `closed === true`，但旧 realm function / DOM wrapper 会在
解引用 native host 前 fail closed，不能因为 source `v8::Global` 仍存在就延长 Rust host 生命周期。

##### target-owned Location navigation

cross-origin Location object 不保存 protocol target id，也不从 opener host 发起 mirrored navigation。
它只保存目标 stable WindowProxy marker；`window.location = value`、`location.href = value` 和
`location.replace(value)` 完成 WebIDL USVString coercion 后，取目标 Window 的真实 public Location
slot，进入已有 `navigate_location_object` owner：

```text
opener expression
  -> popup restricted Location setter / replace binding
  -> popup current Window Location slot
  -> popup RendererPageScriptEnvironment task/output route
  -> exact popup TargetPageResidenceIdentity navigation
  -> replace popup Document/realm; keep Page + stable WindowProxy
```

CDP 回归从 opener 先执行 assignment、再执行 `replace()`，两次都等待 background popup target 的
typed scheduler state，而不是 sleep/drain；新 Document 的 title/body 只能在 popup session 观察，
target/core Page residence 和 renderer Page/WindowProxy residence 均保持，opener 保存的两个 proxy
引用在两次 navigation 后仍严格相等。

#### 第二纵切 B：统一 close transaction 与最终 WindowProxy 断开

本纵切已完成 close transaction 的第一条 production 闭环，范围是已经迁移为真实 related
auxiliary Page 的 top-level popup，以及既有 `Page.close` / `Target.closeTarget` target teardown。
它没有把 lightweight named/`noopener` popup 伪装成已经迁移，也没有在这一层实现 popup blocker、
sandbox、COOP 或完整 unload policy。

##### Chromium 合同与 Lightmount 对应边界

本地 Chromium `a03603fe9af6` 的关键事实如下：

- `DOMWindow::Close` 只接受 outermost main frame，并以 incumbent Document 的 `CanNavigate`、
  `OpenedByDOM` / history length、`ShouldClose` 等条件决定脚本是否可关；调用 `Page::CloseSoon()` 后又
  立即设置 `window_is_closing_`，保证延迟关闭真正发生前 `window.closed` 已经返回 `true`；
- `DOMWindow::closed()` 同时观察 `window_is_closing_`、Frame 是否存在和 Frame 是否仍有 Page；它不是
  bootstrap 时写死的普通 data property；
- `Page::CloseSoon()` 先把 Page 标记为 closing、停止 loader，再把 browser close request 排到当前
  JavaScript 完成之后；这样深层 JS 调用不会在嵌套 loop 中把正在执行的 realm 提前销毁；
- browser 侧 `RenderFrameHostImpl::ClosePage` 把 renderer-origin 和 browser-origin close 汇到同一
  unload / final close path，并在 renderer-origin request 已不再指向 active main frame 时拒绝误关新页；
  `WebContentsImpl::ClosePage` 也进入该入口。

Lightmount 对应采用两阶段状态，而不是从 `close()` callback 直接 drop `PageVm`：

```text
target Window.close()
  -> RendererPageScriptEnvironment: Active -> Closing（同步、幂等）
  -> target Page output FIFO: RendererOwnerAction::TopLevelClose
  -> protocol exact TargetPageResidenceIdentity preflight
  -> fail pending Inspector awaits / fetches + acquire renderer output fence
  -> PageTargetTerminationOwnerAction::WindowClose
  -> common target/session close path + targetDestroyed
  -> renderer final Page teardown: Closing -> Closed
  -> same stable WindowProxy reattached to a host-free restricted facade
```

这里有五个必须保持的 owner 不变量：

1. `RendererPageScriptEnvironment` 与 stable Page/WindowProxy 同寿命，并跨 replacement `PageVm` 复用；
   `Active → Closing` 只能成功一次，所以同一 turn 的重复 `close()` 只产生一个 owner action。普通
   cross-document navigation 不改变该状态。
2. same-origin direct Window 和 related cross-origin Window surface 都调用目标 Page 的 close authority。
   跨 Page 调用发生在 opener turn 时，typed `TopLevelCloseOutputHandoff` 只唤醒目标 Page owner 来冻结
   它自己的 FIFO，不携带第二份 close authority；busy Page 在本 turn 返回时结算，尚未 admission 的
   initial Page 在创建提交时结算。
3. protocol 在 renderer output 进入 ingress 时冻结 target id、session owner scope 和
   `TargetPageResidenceIdentity`。延迟 action 不能跟随一个 session 去关闭后来安装的 Page；pending
   Inspector await、navigation/subresource fetch 先产生 terminal output，renderer fence 通过后才发布
   最终 target termination。
4. initial Page creation diagnostics 会携带 `top_level_browsing_context_closing`。因此
   `const p = open(url); p.close()` 即使完全发生在 target admission 前，也仍先创建可观察的 target，
   随后按自己的 close FIFO 销毁；目标 URL 的 navigation claim 根本不会 stage/publish，不靠取消一个
   已经开始的请求来掩盖副作用。
5. 每个 `JsContextHost` 拥有一个 Document 级 liveness token，并把同一个 token 装进 default、isolated、
   child 和临时 facade Context 的非 owning host slot。child realm navigation 只退休自己的 owner/token，
   不提前熄灭 Document host token，因此保留的旧 child `fetch` / XHR / Runtime binding 仍按原 owner 语义
   fail closed；当整个 Document host retirement 时 token 一次性失效，即使某个旧 child Context 已从 live
   realm store 移除，任何 raw-pointer callback 也会先拒绝访问。当前仍可枚举的 Context 还会被显式标成
   disconnected、移除 host slot 并清空 bridge pointer。这个 Document 边界不改变 stable Page 的 close
   state；只有 stable Page 最终 discard 才进一步把原 main WindowProxy 从旧 global detach，挂到无
   `JsContextHost` 的 restricted facade。opener 保存的引用仍严格相等并观察
   `{closed:true, opener:<original opener>, length:0}`；敏感属性抛 `SecurityError`，旧 realm function 和 DOM wrapper
   抛 `TypeError`。Document replacement/cancel 回归明确锁住 `closed === false`，防止把 realm teardown
   提升成 browsing-context teardown。

`window.close()`、`Page.close` 和 `Target.closeTarget` 的触发前置条件不同，但最终 target/session
closure 和 renderer Page discard 已经共用同一责任边界。`WindowClose` 保留独立 termination kind，
用于诊断触发来源，而不是复制销毁逻辑。`targetDestroyed` 仍由既有 target-host closure event plan
统一生成，重复 close 或晚到 action 通过 exact target/Page currentness 变成 no-op。

##### 本纵切仍未实现的 Chromium close policy

本轮刻意没有把以下行为塞进 V8 callback 或 protocol 调用点：

- `OpenedByDOM`、history length 1 和浏览器设置共同决定的 script-closable gate；
- incumbent `CanNavigate`、sandbox navigation flag、COOP browsing-context group sever；
- `beforeunload` / `unload`、dialog、`ShouldClose` 和 renderer ACK/timeout；
- close 与已经提交的 navigation、named-target reuse、opener sever 的完整竞态矩阵。

这些是 creation/group policy 与通用 Page unload lifecycle 的后续纵切。当前实现对已迁移的 top-level
Page 允许脚本发起 close；证据只支持“请求一旦被接受，transaction、取消、target 事件与断开语义
一致”，不能解读成 Chromium 的所有“是否允许关闭”条件已经完成。

##### 聚焦证据

renderer 回归覆盖：跨 Page 两次 `close()` 只发布一个 target-owned action，target 自身与 opener
同步观察 `closed === true`；最终 close 后 stable proxy identity 不变，旧 function/DOM wrapper fail
closed；普通 navigation 和取消的 prepared replacement 保持 `closed === false`。另有组合回归先让
child realm 因 navigation 离开 live store，再退休整个 Document host：host 退休前旧 child
`fetch` 仍返回既有 shutting-down TypeError，退休后同一 closure 在原 Promise/TypeError realm 安全拒绝，
证明安全性不依赖保存所有旧 `Global<Context>`。

protocol 回归覆盖：`open(url); popup.close()` 的 evaluation response 和 `targetCreated` 都早于唯一
`targetDestroyed`，目标监听 socket 没有收到连接，target/session residence 被移除；随后 opener
仍观察同一个 closed proxy。另一个用例从 `Target.closeTarget` 关闭真实 popup，得到完全相同的最终
proxy facade，证明 browser-origin close 没有旁路 renderer teardown。

本地 Chromium 行为探针还校正了两个边界。关闭 DOM-opened popup 后，保存的 proxy 仍满足
`popup.opener === opener`，所以 closed facade 保留原 opener edge，而不是擅自 sever 为 `null`。另一方面，
Chromium 的 Oilpan/V8 lifetime 能让已关闭 popup 或已移除 iframe 的旧 Node 和函数继续读取 detached
Document；Lightmount 当前 DocumentRuntime/`JsContextHost` 还不是由 V8 wrapper 共同拥有，因而本纵切
只能在 host retirement 时让这些 raw-host-backed 值抛 `TypeError` 来保证内存安全。这是明确的兼容性
缺口，不应把“安全 fail closed”解读成 Chromium 的 detached-Document 完整语义。

`chromium 145.0.7632.116 --headless --dump-dom` 的本地最小探针结果为：移除 same-origin iframe 后，
`[savedNode.textContent, savedFunction(), savedWindow.closed]` 是 `['old','old',true]`；关闭 DOM-opened
popup 后，下一 task 中的
`[savedNode.textContent, savedFunction(), popup.closed, popup.opener === opener]` 是
`['old','old',true,true]`。这与上述 source lifecycle 分支一致，也把当前 Lightmount 的安全降级边界
固定成可复查的行为差异。

#### Phase 5C：live child relation 与 Page-scoped opener edge

Phase 5A 最初为 related top-level WindowProxy 建立的 restricted surface 仍保存了 bootstrap 时的
child count/name 和 opener value。`length` 虽然会读取目标 host，但 numeric index、named child、
ownKeys 和 opener getter 仍可能互相矛盾。Phase 5C 没有在 `appendChild`、attribute setter、remove 或
navigation 调用点逐个刷新 surface；它把投影重新接回已经拥有 browsing-context identity 的 owner。

top-level cross-origin Window 的 access surface 现在只保存稳定 allowlist/function/Location 基础，不再
保存 child index/name。V8 named/indexed handler 每次从目标 WindowProxy 的 creation context 找到精确
`JsContextHost`，先通过既有 child registry 同步当前 subtree，再按 frame-tree sibling order 解析 child，
最终调用 `child_browsing_context_window_proxy_for_top()`。因此 index、name 和 iframe `contentWindow`
返回的是同一个 child-frame stable WindowProxy，而不是另造 detached placeholder：

```text
related top-level WindowProxy handler
  -> target Page current JsContextHost
  -> top-level child browsing-context registry
  -> stable child WindowProxy / current LocalWindow realm
  -> caller-observed numeric or named value
```

named lookup 的次序按本地 Chromium source/WPT 校正为：cross-origin exposed IDL property 优先，其次是
document-tree child browsing-context name，最后才是 `then` / symbol fallback 或 SecurityError。于是名为
`close` 的 child 不能遮住 allowlist method；名为 `open` 的 child 在跨源观察方可见；名为 `then` 的 child
会遮住默认 `undefined`，移除后又恢复非 thenable fallback。普通 named child 不参加
`[[OwnPropertyKeys]]`，numeric child 则继续作为 enumerable index 出现在 keys；这与 Chromium
`WindowProperties::AnonymousNamedGetter()`、bindings generator 的
`CrossOriginGetOwnPropertyHelper` 以及 `v8_cross_origin_property_support.cc` 的 fallback/enumerator 顺序
一致。

opener 不再只是 LocalWindow private slot。`RendererPageScriptEnvironment` 保存 Page-scoped
`top_level_opener_edge`，initial auxiliary realm 直接绑定实际 stable opener WindowProxy；Document
replacement 只把同一 edge 投影到新 realm。`Window.opener` setter 对齐 Chromium
`DOMWindow::setOpenerForBindings()`：传入 `null` 先 sever browsing-context edge；无论值是否为 null，
随后都在当前 Window 上建立 writable/enumerable/configurable own data property。非 null 值因此只
shadow accessor，不改变底层 edge。跨源 popup opener accessor 读取目标 Page edge；opener Page 最终
discard 后，stable opener proxy 的 closed marker 会把存活 popup 的 edge 折叠为 `null`。反方向关闭
popup 不会错误 sever 其仍存活的 opener，closed facade 继续投影原 edge。

回归覆盖以下状态变化：

- 跨源 target 动态插入名为 `alpha`、`then`、`open` 的三个 iframe；observer 的 index/name/descriptor
  都指向同一 stable child WindowProxy，keys 只含 `0..2`，普通 named child 不进入 own names；
- rename `alpha → renamed` 并移除 `then` 后，length/indices/name 同步变化，旧 `alpha` 重新抛
  `SecurityError`，`then` 恢复 `undefined`，没有 stale index/name；
- target 执行 `window.opener = null` 后，保存的原 accessor getter、target 自身和跨源 observer 都看到
  `null`，own data descriptor 形状正确，Document replacement 后仍不重连；另有 non-null 赋值回归
  锁住“只建立 ordinary shadow、原 getter/edge 不变”；
- 不显式 sever 时，关闭 popup 自身仍保留 opener；关闭 opener Page 时，存活 popup 在最终 discard 后
  看到 `null`，导航后保持 sever。

本纵切仍没有实现 COOP/`noopener` 的 browsing-context-group switch 或 remote endpoint；Page-scoped
edge 是当前 related same-agent group 的唯一投影 owner，也是后续 group policy transaction 的接入点，
不能把“JS setter/本地 opener discard 已 live”解读为完整 COOP policy 已完成。

#### Phase 5D1：restricted Location internal methods

Phase 5A 的 cross-origin Location 用 null-prototype target 加 JavaScript `Proxy` 实现，但 target 同时
安装了 `origin`、`assign`、`reload` 等 denied accessor。这虽然让直接读取抛错，却产生两个错误事实：
denied name 会泄漏进 `[[OwnPropertyKeys]]`，其 descriptor/hasOwnProperty 还会被报告为 own property；
完全未知的 key 则沿普通 target 路径返回 `undefined`。handler 也没有拥有 `[[SetPrototypeOf]]` 与
`[[PreventExtensions]]`，因此这些 internal methods 会落回普通 extensible object 语义。

D1 把 restricted Location 的 target 缩成唯一可枚举事实：`href` setter、`replace` method、`then`
fallback 和 `Symbol.toStringTag` / `Symbol.hasInstance` / `Symbol.isConcatSpreadable`。Proxy handler 负责
其余策略，不再用 denied accessor 假装接口表：

```text
cross-origin Location Proxy
  -> minimal null-prototype target
       href / replace / then / three fallback symbols
  -> get / has / getOwnPropertyDescriptor
       allow target key; otherwise SecurityError in the accessing realm
  -> set
       href navigation only; otherwise SecurityError
  -> deleteProperty / defineProperty
       always SecurityError
  -> setPrototypeOf
       null => true; non-null => false
  -> preventExtensions
       false; target remains extensible
```

这与本地 Chromium 的
`third_party/blink/renderer/bindings/scripts/bind_gen/interface.py`（generated cross-origin
getter/descriptor/query/enumerator）和
`third_party/blink/renderer/platform/bindings/v8_cross_origin_property_support.cc`
（fallback/ownKeys）一致，也直接覆盖
`third_party/blink/web_tests/external/wpt/html/browsers/origin/cross-origin-objects/cross-origin-objects.html`
中的 Location 矩阵。回归锁住以下结果：

- `Object.getOwnPropertyNames(location)` 精确为 `['href','replace','then']`，string keys 不可枚举，
  3 个 symbol 位于其后且 descriptor 都是 non-writable/non-enumerable/configurable；
- `href` descriptor 只有本地 realm 可调用的 setter，`replace` 是 readonly method，fallback value 为
  `undefined`；existing identity、WebIDL conversion 和 navigation 路径不变；
- denied/unknown key 的 get、`in`、descriptor、hasOwnProperty、set、delete、define 均抛访问方 realm 的
  `SecurityError`，不再返回 `undefined` 或暴露伪 descriptor；
- prototype 保持 `null`：设为 `null` 成功，设为其他对象时 Reflect 返回 false、Object/legacy setter
  抛 `TypeError`；直接 `location.__proto__ = value` 仍属于 denied property set，抛 `SecurityError`；
- `Object.isExtensible()` 始终为 true，Reflect.preventExtensions 返回 false，Object.preventExtensions
  抛 `TypeError`，失败后不会偷偷冻结 target。

D1 没有声称完成整个 Phase 5D。Window 的 denied/unknown property、out-of-range index、delete/define、
prototype/preventExtensions 与 ownKeys 精确矩阵仍由 D2 负责；D1 当时的 cross-origin
Window/Location method 和 accessor 仍从 target surface 创建，后续已由 D3a 改成按 accessing
realm/incumbent 分配。

#### Phase 5D2：restricted Window internal methods

D2 延续 D1 的原则：restricted surface 只保存真实可观察事实，拒绝策略属于 WindowProxy internal
methods。旧 child surface 为 `document`、`setTimeout`、`open` 等大量 denied name 安装 throwing
accessor，导致这些 name 错误进入 own keys、descriptor 和 hasOwnProperty；不存在的 numeric index 又因
直接调用 V8 `get()` 而把“property missing”误判成值为 `undefined` 的 own property。这既不符合
`cross-origin-objects.html`，也阻止名为 `document` / `open` / `then` 的 child browsing context 按
Chromium 的 named getter precedence 出现。

D2 删除整张 denied accessor 表。stable WindowProxy 的 V8 named/indexed handler 与 detached fallback
Proxy 现在共同拥有以下矩阵：

```text
cross-origin WindowProxy
  -> minimal access surface
       live exposed properties / actual child indices / actual named children
       then fallback / three fallback symbols
  -> [[Get]] / [[HasProperty]] / [[GetOwnProperty]]
       exposed property or existing child => value/descriptor
       denied/unknown name or missing index => accessing-realm SecurityError
  -> [[Set]]
       location navigation only; every other name/index => SecurityError
  -> [[Delete]] / [[DefineOwnProperty]]
       every name/index, present or absent => SecurityError
  -> [[GetPrototypeOf]]
       null
  -> [[SetPrototypeOf]]
       null => success; non-null => false / TypeError according to caller API
  -> [[IsExtensible]] / [[PreventExtensions]]
       true; Reflect => false, Object => TypeError, target remains extensible
  -> [[OwnPropertyKeys]]
       numeric child indices, exposed strings, one final then, three symbols
       ordinary named children excluded
```

named lookup 继续复用 Phase 5C 的 child registry precedence，而不是恢复接口名称黑名单：只有
cross-origin exposed Window property 保留优先级。于是 `name="focus"` 仍得到 readonly `focus()`，
`name="document"` 则得到与 numeric index 相同的 stable child WindowProxy；named descriptor 是
non-writable/non-enumerable/configurable，且普通 named child 不进入 ownKeys。`then` 是唯一例外：named
child 可以遮住 fallback，但 ownKeys 中仍只有一个 `then`，并保持为最后一个 string key。enumerator
同时显式保证 indices 在前、3 个 well-known symbols 在末尾，不再依赖 target property 的安装顺序。

这里存在一个 V8 版本边界。仓库当前的 V8 137 会在 foreign global 的
`Object.setPrototypeOf` / legacy proto setter / `preventExtensions` 到达 interceptor 前先执行 security
access check，因而统一抛 `SecurityError`；本地 Chromium 对应 WPT 要求 non-null prototype 和
Object.preventExtensions 抛访问方 `TypeError`，Reflect 返回 false，而 null prototype 成功。D2 没有
为此包装或替换 stable WindowProxy identity。它在每个访问方 Window realm 安装 native intrinsic
adapter，并且只在参数同时满足以下条件时接管：

1. 参数严格等于其 creation Context 的 global proxy；
2. 当前 Context 与该 Context 不同；
3. 既有 Window access-check owner 判定当前调用方不能访问目标。

其余普通 object、same-origin Window 和非法参数全部调用保存在 callback data 中的原始 V8 intrinsic；
回归同时锁住 ordinary delegation、function name/length 和 native-code 形状。cross-origin Window 的
template 也标为 immutable-prototype exotic object，保证 same-origin 转发路径仍遵守 Window 自身的
immutable prototype 约束。detached fallback 本来就是 JavaScript Proxy，则直接由 ownKeys、
setPrototypeOf 和 preventExtensions traps 提供同一结果。

D2 回归覆盖：

- related popup、普通 cross-origin child 与 detached child fallback 的 missing index get/descriptor/
  hasOwnProperty/`in` 全部抛访问方 `SecurityError`；
- unknown named get/descriptor/hasOwnProperty/`in`/set，以及 present/absent index/name 的 set/delete/
  define 全部拒绝；`location` navigation、allowed method 和 receiver check 保持；
- Object、Reflect、legacy proto setter 和 direct `__proto__` 的完整 null/non-null 矩阵，错误分别属于
  访问方 `DOMException` / `TypeError` realm；preventExtensions 失败后仍 extensible；
- exact string/symbol own names、index/then/symbol 顺序、named child 排除，以及
  `document` collision 可见、`focus` collision 被 exposed method 压住；
- ordinary Object/Reflect/legacy proto 操作仍委托 V8，设置 prototype 和冻结普通对象的结果不变。

D2 完成的是 `cross-origin-objects.html` 的 Window internal-method 静态矩阵。当时 related top-level
已由 Phase 5C 动态读取 target Page registry，但 generic nested cross-origin child 仍可能回落到
refresh-time index/name snapshot。D2 将该风险明确留给 D3 前的独立 owner 抽取；下面的 D2.5 完成了
该抽取，没有通过 access 时 drain/retry 修补。

#### Phase 5D2.5：generic nested live child projection

D2.5 把 Phase 5C 的 related-top 特例收敛为一次 WindowProxy callback 内有效的
`CrossOriginWindowChildRegistryOwner { host, parent }`。`parent=None` 表示 target Page 的 top-level
children，`parent=Some(DomHandle)` 表示 generic nested Window 的 direct children。owner 只从 target
Context 已有 host/handle slot 临时解析，不进入全局 cache，也不延长 `JsContextHost` 生命周期；nested
owner 只有在对应 browsing context 仍 live 且拥有 current Document handle 时成立，parked/retired facade
不会伪装成 live registry。

named/indexed WindowProxy handlers 和 `length` accessor 现在共享同一查询流程：

```text
foreign WindowProxy callback
  -> resolve target Context host + scoped parent
  -> synchronously project that Document subtree into the child registry
  -> index/count/name lookup in scoped registry
  -> return the existing stable child WindowProxy for that browsing-context id
  -> missing live entry => SecurityError（then 单独回落到 undefined fallback）
```

因此 get/query/descriptor、indexed enumerator/ownKeys 和 length 不再从 live surface 读取 child slot。
real LocalWindow 的 cross-origin access surface 现在只安装 exposed Window properties、Location、methods、
`then` fallback 和 symbols，child index/name 的物理 seed 为零；只有尚无 current LocalWindow 的预物化
facade或 navigation realm gap facade 保留 snapshot seed，作为没有 live registry owner 时的安全 fallback。
live owner 一旦存在，旧 seed 即使还附着在 proxy storage 上也不参与 named/indexed internal methods，
所以 rename/remove 后不会从 backing surface 复活 stale name/index。

该改动也暴露并修复了一个更底层的 stable WindowProxy 缺陷。未物化 nested child 原先通过
`instantiate_window_proxy_shell()` 预留 identity，但 cross-origin facade 错误复用了创建方 security
token，而且没有把 host/child handle 与 access surface 接回 facade Context。旧静态 index proxy 掩盖了
这一点；live registry 首次返回该 shell 时会直接看到创建方 raw global/intrinsics。现在预物化 shell：

- 保留 V8 unique default security token，不与创建方形成 same-origin alias；
- 在暴露前安装 exact context-host liveness slot 和 `ChildWindowProxyFacadeContextHandle`；
- 在 facade realm 同时初始化 stable global proxy 与独立 minimal access surface；
- handler data 在没有 live LocalWindow 时使用该 cross-origin proxy，后续 realm materialization 仍 detach
  同一 facade 并把 exact proxy 交给新 Context，identity 不复制也不替换。

回归在一个已经跨源 commit 的 child Document 内保留原始 3 个 nested WindowProxy，随后由 child 自己
rename 第 0 个 frame、移除第 1 个、append 一个 `name="then"` 的新 frame。parent 侧在不重新获取 outer
`contentWindow` 的前提下证明：第 0 个 identity 保持，原第 2 个移动到 index 1，新 child 同时等于
index 2 与 named `then`，旧 `nestedNamed` / `document` 的 get/has/descriptor 全部立即抛访问方
`SecurityError`，普通 named child 仍不进入 ownKeys，且三个 returned child 都继续拒绝 `.document`。
原有 detached fixture 进一步证明 context 尚未物化时不会泄漏 raw global。

D2.5 统一的是 child registry authority，不提前冒充 D3 membrane。它完成时，child WindowProxy 的
observer-relative 选择仍复用现有 top projection helper；不同 same-origin incumbent 的
Function/accessor prototype、wrapper cache 和异常 realm 随后已由 D3a 收敛，非-top observer 的 endpoint
projection 仍留给 D3b。

#### Phase 5D3a：per-accessing-Realm `CrossOriginPropertyDescriptorMap`

D3a 已完成 Function/accessor membrane，范围是 HTML
`CrossOriginGetOwnPropertyHelper` 返回的 Window/Location method 与 accessor wrapper。它没有为每个
target 复制一套 wrapper；缓存 key 是“访问方 Realm + interface member”，target identity 仍由 stable
WindowProxy/Location object 承担。这一点与 Chromium 的边界相同，也是复用 child-frame stable
WindowProxy/realm 基础后必须补上的访问方投影层。

##### Chromium / WPT 合同

本地 Chromium `a03603fe9af6` 的
`third_party/blink/renderer/platform/bindings/v8_cross_origin_property_support.cc` 在 isolate 的 current
Context 中取得 `ScriptState`，按 world 与 callback 缓存 `FunctionTemplate`，再对 current Context 调用
`GetFunction()`。template 还绑定对应 Window/Location interface signature，让 receiver brand 在 native
callback 和 WebIDL 参数转换之前成立。generated binding 入口位于
`third_party/blink/renderer/bindings/scripts/bind_gen/interface.py`。

对应 WPT 不只要求“属性可调用”，还要求：

- Window 的 `close`、`focus`、`blur`、`postMessage` 是 readonly data descriptor，name 分别等于属性名，
  length 分别为 `0/0/0/1`；
- Window 的 `location/window/frames/self/top/parent/opener/closed/length` 是 accessor descriptor，getter
  name 为 `get <name>`、length 为 0；只有 `location` 有 `set location`，length 为 1；
- Location 的 `replace` 是 length 1 的 readonly method，`href` 只有 `set href`、length 1；
- 同一 Realm 重复 `[[Get]]` / `[[GetOwnProperty]]` 得到同一个 function；两个 same-origin observer Realm
  得到不同 function，且各自继承本 Realm 的 `Function.prototype`；
- observer 不同不会复制 target：双方仍观察同一个 WindowProxy 和同一个 Location identity。

直接证据来自 Chromium checkout 中的
`cross-origin-objects-function-{common,caching,name,length}.html/js` 与 `cross-origin-objects.html`。

新增 core 回归构造 A（parent）与 B（same-origin `srcdoc` observer）两个 Realm，共同观察跨源 child C。
在接入 D3a 前，红灯输出精确暴露了旧 target-surface 模型：四个 method 和可见 accessor 的
`Function.prototype` 均不是访问方 Realm；`window/frames/self/top/parent/opener` descriptor 没有 getter；
readonly attribute 仍带 throwing setter；A/B 之间 method、getter、Window location setter、
Location.replace 与 href setter 的 identity 全部相同。该失败不是测试等待或 fixture 时序问题，而是 wrapper
确实由 target realm 唯一创建。

##### Realm-local cache owner

新模块 `cross_origin_property_descriptor_map.rs` 以 accessing Context 的 V8 hidden
extras-binding object 作为 cache owner，并用 isolate-wide private symbol 区分每个 member：

```text
cross-origin [[Get]] / [[GetOwnProperty]]
  -> incumbent Context（无 incumbent 时才回落 current Context）
  -> Context::get_extras_binding_object()
  -> member private slot
       hit  => 返回该 Realm 已有 native Function
       miss => 在该 Context 创建、设置 name/length、写回 slot
  -> descriptor/value 返回给访问方
```

这个 owner 选择同时满足三条 lifetime 不变量：

1. wrapper 只被 V8 tracing 持有，不在 Rust host/global cache 中形成强引用环；
2. stable WindowProxy navigation rebind 到新 Context 时，新 Realm 自然得到新的 extras object，不会复用旧
   LocalWindow generation 的 function；
3. 同一 Realm 观察多个 cross-origin target 时共享 HTML 规定的 member wrapper，function 本身不捕获某个
   target Page。

Window 四个 method、九个 getter、唯一 location setter，以及 Location 的 replace/href setter 都经过该
cache。`[[GetOwnProperty]]` 不再复用 target surface 的旧 data descriptor：九个 Window attribute 统一返回
non-enumerable/configurable accessor descriptor，readonly attribute 的 `set` 精确为 `undefined`；method 与
Location.replace 继续是 non-writable/non-enumerable/configurable data descriptor。

##### target-neutral wrapper 与 receiver-owned execution

per-Realm wrapper 不能把创建时的 ambient `JsContextHost` 当作 target。否则 opener 首次取得 popup.close
后调用该缓存 function，会关闭 opener；同理，postMessage 和 Location navigation 会进入错误 Page 的
queue/URL owner。D3a 因而把 native callback 划成两个阶段：

```text
accessing Realm
  -> receiver / WebIDL 参数 / exception Realm
  -> receiver 上的 stable child handle 或 related-top target marker
  -> target WindowProxy creation Context
  -> liveness-checked Context host slot
  -> target Page owner 执行 close / postMessage / Assign / Replace
```

- URL 的 USVString conversion 留在 accessing Realm；解析到 target 后才进入 target Context 和 navigation
  owner，避免 target realm 泄漏转换异常；
- `postMessage` 仍由既有 endpoint resolver 决定 top/child/popup endpoint，但排队 host 从 receiver 的 target
  Context 取得，`event.source` 继续是 stable source WindowProxy；
- related popup `close()` 只请求 target Page 的唯一 close transaction；`focus()` / `blur()` 当前仍是通过
  receiver brand 检查的 no-op，与尚未完成的 focus transaction 边界一致；
- attribute getter 先校验 receiver，再在 target access-surface Context 读取 live relation/scalar；
  `window/self/frames` 直接返回 exact receiver，避免创建等价但不相等的 identity；
- Chromium 依赖 V8 interface signature 拒绝错误 Location receiver；当前 Lightmount 的 minimal
  cross-origin object 没有对应 template signature，因此 cached `href` setter 与 `replace` 在参数转换前
  显式验证 Location proxy brand。扩大矩阵曾捕获 `hrefSetter.call(null, ...)` 被错误当成访问方 global 的
  回归，修复后又加入 `replace.call(null, ...)` 防回归。

双 Realm 回归最终证明：每个 Realm 内所有 wrapper 重复读取稳定，A/B wrapper 全部不同且原型分别属于
A/B，name/length/descriptor shape 一致，非法 receiver 的 `TypeError` 与 unknown property 的
`SecurityError` 都属于发起 Realm；同时 A/B 观察到的 target Window 和 Location identity 仍完全相同。
related popup 端到端回归还同时覆盖跨 Page message source、target Location assignment/replace、
target-only close 和 transport-failure error Document 后的 stable WindowProxy。

D3a 不改变 child registry authority，也不宣称完成 observer-relative endpoint。当时 generic nested child
lookup 已经 live，但从非-top same-origin observer 返回 child WindowProxy 时仍复用 top-oriented projection
helper。下面的 D3b 已把这条历史缺口闭合：observer/target pair 只用于本次 callback 的授权判断，最终仍
返回 browsing-context-owned stable target identity，而不是把 child wrapper 放进 Realm-local function cache。

#### Phase 5D3b：observer-relative child endpoint projection

D3b 修复的是一个三方关系，不能简化成“parent 是否和 child 同源”：A 是 top，B/C 是 A 的两个 direct
child；A 与 B/C 跨源，而 B 与 C 同源。B 通过跨源的 `parent.frames[1]` 或 named property 取得 C 时，应得到
C 的 stable WindowProxy 并拥有完整同源访问；A 对同一对象仍必须只看到 restricted surface。旧实现虽然
已经由 D2.5 live-resolve 到 C 的 browsing-context handle，最后却无条件调用
`child_browsing_context_window_proxy_for_top()`，把 A/C 的关系误当成所有 observer/C 的关系。

##### Chromium 边界

本地 Chromium `a03603fe9af6` 的
`third_party/blink/renderer/core/frame/{dom_window.cc,window_properties.cc}` 与
`third_party/blink/renderer/bindings/core/v8/window_proxy.cc` 没有为每个 observer 克隆 child Window：

- `DOMWindow::AnonymousIndexedGetter()` 从 `FrameTree::ScopedChild(index)` 取得 child 的 `DomWindow()`；
- `WindowProperties::AnonymousNamedGetter()` 同样解析 scoped child，再通过 current Realm 的
  `ToV8Traits<DOMWindow>::ToV8(...)` 投影；
- `DOMWindow::Wrap()` 最终返回 `WindowProxyManager` 持有的 `GetGlobalProxy()`，因此不同 observer 共享
  browsing-context identity；
- 访问是否展开由 current Realm / `BindingSecurity` 和 child security origin 决定，而不是由 target parent
  预先选一份永久 restricted/full wrapper。

WPT
`third_party/blink/web_tests/external/wpt/html/browsers/windows/nested-browsing-contexts/frameElement-siblings.sub.html`
也直接通过 `parent.frames[0]` 验证 sibling Window 的访问结果随 same-origin-domain 关系变化。D3b 沿用
这条边界：registry 决定“是哪一个 child”，observer-relative access 决定“本次能否 materialize/展开”，
stable WindowProxy 决定“对象 identity 是哪一个”。

##### Lightmount cutover

实现没有给 synthetic facade 增加第二套动态 index/name trap，而是把责任收回已成熟的 stable
WindowProxy/realm 基础：

```text
non-top observer B reads parent.frames[index/name]
  -> parent/top is A's real stable top-level WindowProxy
  -> cross-origin handler resolves A's live scoped child registry
  -> callback-local observer = incumbent B execution-context identity
  -> compare B origin with target child C dispatch-scope origin
       same origin => promote/reuse C's exact stable WindowProxy + LocalWindow realm
       cross origin => keep C's restricted facade
  -> V8 access check continues to evaluate every later observer against C
```

- child Realm 的 `parent` / `top` 在 main top Realm 存在时直接引用其 stable global proxy；只在尚无 current
  top Realm 的 bootstrap gap 保留旧 detached safe projection fallback。这样 B 的 `parent`、C 的
  `parent/top` 和 A 自身不再是三份 synthetic identity；
- `CrossOriginWindowChildRegistryOwner` 仍只持 callback-scoped target host/parent authority，但新增独立的
  `CrossOriginWindowObserver { host, identity }`。observer 从 incumbent Context 的 liveness slot 解析，不被
  target holder creation Context 覆盖，也不会跨 callback 缓存 raw host pointer；
- same-host 路径复用 `window_execution_context_can_access_dispatch_scope()`；related Page 路径把原先只允许
  top-to-top 的检查推广到 target dispatch scope。后者仍要求访问方 identity current、两 Page 属于同一个
  related script agent，并对 target 的 live origin 做比较；opaque origin 不因两个 host-local owner id 碰巧
  相同而放行；
- observer 有权限时，预物化 restricted facade 会 detach 并把 exact proxy 交给 C 的正式 Context。A 早先保存
  的引用严格相等，但在 C materialize 后读取 `document` 或页面 marker 仍由 V8 access check 拒绝；
- index、named get 与两种 descriptor 都经过同一个 owner/observer 决策。Realm-local cache 继续只保存
  D3a 的 method/accessor function，不保存 WindowProxy。

##### 回归证据

core 回归构造 `localhost` top A 与两个 `127.0.0.1` child B/C。A 先保存 C 的 restricted facade，随后 B
通过 `parent.frames[1]` 和 `parent.observerTarget` 访问 C。接入 D3b 前稳定红灯为：lookup 已返回对象，但
首次 marker write 抛访问方 `SecurityError`；A 侧 identity 与两个 denial 均正常，证明失败不是加载等待或
错误 target。修复后同时证明：

- index/name/getOwnPropertyDescriptor 都返回同一个 C proxy；
- B 可读写 C Document、Location、intrinsics，且 `document.defaultView === target`；
- C 的 `parent/top` 与 B 观察的真实 A proxy identity 一致；
- A 保存的引用不变，且 A 对 C 的 Document/marker 继续得到 `SecurityError`。

related Page 回归又把 opener A 与跨源 popup P 放在两个 `JsContextHost` 中，并让 P 的第一个 child C 导航到
A 的精确 origin。P 对 C 的 Document 被拒绝；A 经跨源 `popup[0]` 取得同一 C proxy 后可以完整读取 C 的
Document/realm，且 C 的 `parent/top` 都严格等于 popup stable WindowProxy。这覆盖了 related-agent
cross-host dispatch-scope access，不把同宿主 sibling 通过误当成跨 Page 证据。

#### Phase 5E1：非命名 `noopener` / `noreferrer` 的 Fresh Page single-owner

E1 先处理不需要把 WindowProxy 同步交回 creator 的最小 creation-policy 纵切：production
`window.open()` 的空 target / `_blank`，以及 hyperlink `_blank` 的 implicit 或显式 noopener。它们仍然
创建可被 CDP/BiDi 观察的 auxiliary top-level target，但 author 调用方只得到 `null`，所以没有理由先在
opener Page 内创建一份 lightweight Window、Document 和 loader，再等 protocol 创建第二份真实 Page。

##### Chromium / WPT 合同

本地 Chromium `a03603fe9af6` 的责任顺序很重要：

- `LocalDOMWindow::open()` 先从 entered Window 完成 URL 和 feature parsing；生成 referrer 时，只有
  `noreferrer` 选择 `kNever`，单独的 `noopener` 仍使用 entered Document 的 Referrer Policy；
- 同一个函数随后调用 `FrameTree::FindOrCreateFrameForNavigation()`，对返回的 frame 发起导航，最后才在
  普通 target 的 `noopener` 分支返回 `nullptr`。`_self` / `_parent` / `_top` 在该 null-return 判断之前
  返回 existing Window；
- `FrameTree` 先查 current tree / related Pages / existing named context，找不到才调用
  `CreateNewWindow()`。因此 `noopener` 不是“永远跳过 named lookup 并新建窗口”的同义词；
- `CreateNewWindow()` 在真正新建 auxiliary context 前检查 sandbox popup flag，并且只在
  `!features.noopener` 时 clone opener session-storage namespace。

对应 WPT 把容易混淆的边界拆得很清楚：

- `the-window-object/window-open-noopener.html` 要求第二次带 noopener 的 named `window.open()` 仍导航
  已有 target，但返回 `null`，原 target 的 opener 不被改写；special target 则忽略 null-return policy；
- `the-window-object/window-open-noreferrer.html` 要求新窗口 name 为空、`document.referrer` 为空、
  `window.opener` 为 `null`；
- `referrer-policy/generic/inheritance/popup-inheritance-about-blank.html` 要求普通 initial
  `about:blank` popup 的 `document.referrer` 保留 creator 完整 URL，不受 creator Document 的
  Referrer Policy 截短；
- `webstorage/storage_session_window_noopener.window.js` 要求新 noopener window 不复制 creator 的
  session storage；`storage_session_window_reopen.window.js` 则要求普通 named reopen 保留同一 Window；
- `windows/noreferrer-window-name.html` 同时证明两件事：新建的 named noreferrer windows 不应互相进入
  同一可复用 name group，但一个预先存在的 named iframe/window 仍可以被 noreferrer navigation 命中。

本纵切阅读了上述 Chromium source/WPT，没有编译 Chromium，也没有运行 upstream WPT；这里的 WPT
结果是合同对照，不是 Lightmount 新的通过声明。另用本地 `out/Default/chromedriver` 驱动
`out/Default/chrome`（`Chromium 147.0.7709.0`，headless），从 HTTP creator 分别打开
`about:blank`：

| 调用 | target realm 观察值 `[document.referrer, opener===null, href, name, origin]` |
| --- | --- |
| `window.open('about:blank', '_blank', 'noopener')` | `[creator 完整 URL, true, 'about:blank', '', 'null']` |
| `window.open('about:blank', '_blank', 'noreferrer')` | `['', true, 'about:blank', '', 'null']` |

这个 probe 直接证明 HTTP header eligibility、initial empty Document referrer 和 destination
Document referrer 不能共用一个字符串或一个计算入口。

##### 稳定红灯与旧 owner 违反路径

renderer owner 回归先在 production Page 上执行
`window.open("about:blank#fresh-agent", "_blank", "noopener")`。旧实现虽然预留了
`RendererScriptAgentAdmission::Fresh` Page，activation 中仍带 `popup_id = Some(2)`，证明 opener host
同时创建了 lightweight browsing-context identity。期望改为 `popup_id = None` 后稳定失败。

protocol 集成回归又用真实 HTTP server 观察 `/noopener`。旧实现得到两次请求：一次来自 opener 内的
lightweight loader，且没有 `Referer`；一次来自 target Page，带 creator URL。失败形状为：

```text
[("/noopener", None),
 ("/noopener", Some("http://127.0.0.1:<port>/opener"))]
```

这不是 redirect、retry 或测试 server 误计数，而是两个独立 loader。切掉 lightweight loader 后，请求数与
header 已转绿，但增强后的 target-session probe 继续稳定失败：网络 `Referer` 已正确，committed realm 的
`document.referrer` 仍为空。该第二阶段红灯把缺口定位到 main-Document commit fact，而不是再给请求层加
header patch。拆出 destination Document referrer 后，`about:blank` 又稳定暴露第三阶段红灯：它与 target
的 initial URL 相同，不发生 replacement commit，target realm 仍观察到空 referrer。加入
`about:blank#fragment` 后 same-document 路径同样失败，证明 initial empty Document 必须在默认 realm
创建前独立接收 creator referrer，不能等待 navigation commit 补写。

##### Lightmount cutover

E1 把 creation policy、Page reservation、导航与 Document commit 串成一份 typed transaction：

```text
entered creator Document
  -> resolve opener/referrer policy + destination URL
  -> reserve RendererPendingAuxiliaryPage(Fresh)
  -> freeze { initial-document, network, destination-document } referrers
  -> emit PendingPopupActivation { popup_id: None, exact referrers, reservation }
  -> protocol creates one target and consumes that reservation
  -> fresh Page bootstrap installs initial-document referrer before its first realm
  -> target Page owns at most one destination navigation
  -> replacement commit installs destination-document referrer before the new realm
```

- `WindowOpenFeatures` 现在分别回答 `suppresses_opener()` 与 `suppresses_referrer()`；parser 仍保持
  `noreferrer ⇒ noopener`，但 `noopener` 不再错误清空 referrer；
- creator 在同一个 decision point 冻结三个不同结果：initial empty Document 使用 creator 完整 URL
  （`noreferrer` 时为空）；HTTP network referrer 额外受 header eligibility 约束；destination
  `document.referrer` 使用 navigation referrer policy，但 `about:blank` 保留 initial 值。
  `RendererPendingPopupActivation` 显式携带三者；`Some("")` 表示显式抑制，`None` 只留给尚未迁移、
  仍依赖 browser-context inference 的 producer；
- production 的非命名、可解析、非 `javascript:` suppress-opener 路径只预留 Fresh Page，不调用
  `open_lightweight_popup_window()`，不创建 opener-local WindowProxy/Document/loader，也不携带 creator
  session-storage snapshot；`window.open()` 同步返回 `null`；
- hyperlink `_blank` 使用同一边界。没有 `rel=opener` 时的 implicit noopener、`rel=noopener` 和
  `rel=noreferrer` 都进入 Fresh Page；只有 `noreferrer` 抑制 referrer。当前端到端回归直接覆盖 anchor，
  `<area>` 虽共享 hyperlink activation 路径但尚未单独运行 WPT；
- protocol 的 `PopupTargetCreation` 原样携带三个 referrer；initial 值在 fresh Page 默认 realm 创建前
  安装，network/destination 值继续进入 exact target-owner navigation claim。任何一项都不从新 target
  的 initial `about:blank` 或消费时的 current session 反推。target admission 后仍只有该 target Page
  发起 destination navigation；
- `NavigationDispatchState` 把 `document_referrer` 放在 heap-owned commit environment 中，与
  `request_headers` 分开冻结。Fetch interception 可以修改 transport header，但不能顺带改写已经接受的
  Document environment；随后
  `RendererMainDocumentCommitSeed → RendererMainDocumentCommit` 把值送进 renderer，在默认 realm 和
  document-start script 创建前安装到 `DocumentPolicyContainer`；
- fresh initial Page 使用独立的 `initial_document_referrer` bootstrap 输入；它只初始化 Document
  environment，不伪造 `MainDocumentCommit` observation。因此精确 `about:blank` 和
  `about:blank#fragment` 都能在没有 cross-document commit 时保持 Chromium referrer；
- 三个 popup referrer 收拢为 heap-owned typed bundle，target admission future 也在 generic renderer
  output projection 边界 `Box::pin`。destination Document referrer 与 source origin / secure-context
  则组成一个 heap-owned commit environment；这些结构既表达同一组冻结事实，也避免普通
  `Target.createTarget` 为未走到的 popup/navigation 分支预留大栈帧；
- 没有 production Page allocator 的 renderer standalone fixture 暂时保留 lightweight fallback，避免把
  单元测试适配器误当成真实 browser owner。production 回归要求 reservation 必须存在，因此该 fallback
  不会掩盖 CDP 双 loader。

这一纵切建立的窄不变量是：

1. 一个新建的非命名 suppress-opener auxiliary context 只有一个 browsing-context/Page identity 和一个
   destination loader；
2. `window.open()` 返回值、`window.opener`、script-agent admission、session-storage clone policy 和
   referrer policy 都来自同一 creator-side decision；
3. initial empty Document referrer、网络 `Referer` 与 destination `document.referrer` 在同一
   creator-side decision 中分别冻结；它们可以不同，而 `noreferrer` 明确把三者都置空；
4. target/session attach 观察的是上述 Fresh Page 的 committed realm，不是 opener-local mirror。

#### Phase 5E2A：related-page named `window.open()` 的 renderer group authority

E2A 处理 E1 有意跳过的第一类 named target：同一个 related script agent 中，由
`window.open()` 创建或复用的 top-level auxiliary context。这个范围先统一最重要的同步 identity：
新建 named popup 返回的 WindowProxy、creator 立即写入的 Document、protocol target 采纳的 Page，以及
下一次按 name 找到的 context 必须是同一实体。它不把 browser-context-wide target-name map 当成
browsing-context group，也不把所有 named producer 一次性塞进该 map。

##### Chromium / WPT 选择顺序

本地 Chromium `a03603fe9af6` 的 `LocalDOMWindow::open()` 与 `FrameTree` 给出以下责任边界：

1. 先在 renderer 的 frame tree / related Pages 中选择现有 target，找不到才请求创建新 Page；
2. current Page 的 frame tree 优先于 related top-level Page，因此 named iframe 不能被同名 popup 抢走；
3. closing frame 不参与查找；复用的是既有 frame/WindowProxy，导航不会制造第二个 browsing context；
4. existing target 且本次不 suppress opener 时更新该 target 的 opener；本次为 noopener/noreferrer 时仍导航
   existing target，但返回 `null`，并且不能用本次 suppressed edge 覆盖原有 opener；
5. 真正创建的新 noopener/noreferrer context 属于新的 group/name policy，不能因为 browser context 相同就被
   原 creator 再次按 name 命中。

`window-open-noopener.html` 直接覆盖第 4 点；`windows/noreferrer-window-name.html` 同时覆盖 existing
named target 可被命中与 newly-created noreferrer contexts 不应互相复用。E2A 沿用 E1 的源码/WPT 对照
边界：本轮没有编译 Chromium，也没有运行 upstream WPT，因此这里只声明 Lightmount 聚焦回归，不声明
上述 WPT 已通过。

##### 稳定红灯：同一个 name 的两套 owner

接入前，named popup 同时依赖两份 registry：

- renderer `JsContextHost::lightweight_popup_window_names` 返回 opener realm 中的 lightweight Window/Document；
- protocol `BrowserContext::target_window_names` 再选择一个独立 target Page。

新增 protocol 回归让 creator 执行：

```javascript
const popup = window.open("about:blank", "reportWindow");
popup.document.body.dataset.owner = "renderer-page";
```

creator 同步观察到 `reportWindow|renderer-page`，但 attach 新 target 后旧实现稳定得到：

```text
undefined||false
```

期望是 `renderer-page|reportWindow|true`。三个字段分别证明 target 看到了另一份 Document、另一份 name
状态和缺失的 opener edge；这不是 CDP attach timing。回归随后主动清空 protocol
`target_window_names`，再用动态改名后的 name 执行 noopener reuse，用来证明修复不能只是让两张 map
更勤快地同步。

第一次 workspace 门禁又捕获两条过期 characterization，而不是实现超时：

- renderer owner test 用 production named `window.open()` 后等待 opener-local popup loader。E2A 不再启动
  这份 mirrored loader，server 因而永久等不到请求；测试在 E2A 当时改为尚未迁移的 named hyperlink
  producer，只锁定 legacy popup terminal 的 stable Page route。E2C 迁移 hyperlink 后，这条 characterization
  同样过期并被删除：继续等待 opener-local response 已经与 single-owner 不变量相反，新的 renderer/protocol
  回归改为观察 typed activation、exact Page handoff 和 target realm；
- protocol background test 手工向 `target_window_names` 写入一个与 creator 无 related-page 关系的 target，
  然后期待 `window.open()` 被该 map 重定向。新回归反向锁定：renderer-selected named popup 必须创建自己的
  exact related Page，旧 background target 的 URL/Document 和 active target 都不变。

##### Lightmount cutover

E2A 的 renderer/protocol 流程如下：

```text
entered Window.open(url, name, features)
  -> current Page named-child lookup
       hit: navigate exact child; return Window or null according to suppress-opener
  -> related-page top-level group lookup
       hit: return stable proxy (or null), emit activation { exact renderer Page residence }
  -> no hit + opener preserved + non-javascript URL
       reserve RelatedAuxiliaryPage
       synchronously stage real initial PageVm/realm/Document with window.name
       return that Page's stable proxy
       emit activation { exact pending Page reservation }
  -> protocol projection
       resolved residence: navigate the target already owning that exact Page
       pending reservation: create one target and adopt that exact staged Page
```

具体 owner 变化如下：

- `RendererPageScriptEnvironment` 现在持有一个 `RendererRelatedPageGroup` 和一个
  `RendererRelatedPageTopLevelTargetState`。后者把 exact `{RendererOwnerLocalHostId, PageId}`、stable
  WindowProxy、Page-scoped opener edge、lifecycle 与 name 放在同一状态节点；group registry 只持
  `Weak`，不会用 name map 延长已关闭 Page 的 V8 lifetime；
- related auxiliary environment 从 live source environment clone group capability。它不在已经进入 V8 isolate
  时回借 isolate holder；首次实现确实被聚焦回归捕获为 `RefCell already mutably borrowed`，改为从 source
  capability 传递后消除了这条 reentrancy 路径；fresh Page 仍创建自己的 group；
- 初始 auxiliary realm bootstrap 在安装 WindowProxy/opener 的同时登记 `window.name`。公开 name setter 会
  原子地从旧 name bucket 移除并注册新 name；cross-document replacement 复用同一 Page state，并在新 realm
  bootstrap 后恢复 name。`Closing`/`Closed` 在 renderer 可观察时立即注销，lookup 也再次检查 lifecycle；
- lookup 对当前 top-level Page 本身优先，再按 group 注册顺序选择第一个 live top-level target。空 name 与
  `_self` / `_parent` / `_top` / `_blank` 不进入普通 name registry；special
  target 继续走既有 navigation authority；
- 新 named opener-preserving、非 `javascript:` popup 复用 E1 前已建立的 synchronous real initial realm
  staging，只是把 target name 带入该 realm，并移除“named 必须走 lightweight”分支。creator 的立即 DOM
  mutation 因而落在 target 后续采纳的 exact Document；
- existing related named target 不创建 `Page.windowOpen` 事件、不预留 Page，也不创建 lightweight record。
  非 suppress-opener 调用把 target 的 Page-scoped opener edge 更新为 entered Window；noopener/noreferrer
  调用不修改旧 edge、仍发出 exact-target navigation activation，并向 caller 返回 `null`；
- `RendererResolvedPopupTarget` 是 activation 上的 typed destination claim。protocol 通过 host id 与 Page id
  同时扫描 active/background target，找不到就 fail closed，不回退到 name；这避免同一个 renderer host 中
  多个 related Page 被误路由。migrated producer 还会显式设置 renderer-owned new-target disposition；只有
  该 fact 才让带新 Page reservation 的 activation 跳过 protocol name lookup。E2A 当时尚未迁移的 hyperlink
  producer 即使乐观预留了 Page，仍保留 legacy projection fallback，避免把后续 E2B/E2C 行为偷偷混入 E2A；
- `BrowserContext::target_window_names` 暂时保留，服务 DevTools projection 和未迁移 producer。E2A 回归在
  清空它后仍只导航原 target，证明它不再是 migrated related `window.open()` 的选择 authority。

这一纵切建立的窄不变量是：

1. 新建普通 named `window.open()` 只有一个 initial Page/realm/Document；creator 立即 mutation 与 CDP
   target evaluation 观察同一对象；
2. related top-level name lookup 返回同一 stable WindowProxy，并把 exact renderer Page residence 送到
   protocol，不靠 target name 二次选择；
3. 动态 `window.name`、Page navigation 和 close lifecycle 共享同一 renderer state；旧 name 或 closing
   target 不能继续被命中；
4. existing target 的选择与 noopener 返回/opener mutation policy 分开：noopener 仍导航 exact target、
   返回 `null` 且保留旧 opener；
5. named iframe lookup 保持在 related top-level lookup 之前，且 noopener 命中 existing iframe 时仍导航、
   返回 `null`，不会误建 popup。

E2A 本身仍不是完整 browsing-context-group 实现。多个 related Page/嵌套 frame 同名时的完整 Chromium
frame-tree ordering、`CanNavigate`、focus、跨 agent/remote endpoint 与 COOP group switch 仍需后续纵切；
E2A 当时保留的新建 named noopener/noreferrer fresh-group policy 由下一节 E2B 接手。当前 related registry
仍只覆盖 related same-agent top-level contexts。

#### Phase 5E2B：新建 named suppress-opener 的 Fresh group/name handoff

E2B 处理 E2A 明确保留的另一半 named `window.open()`：renderer 已按 current frame tree、related Page
group 完成查找，但没有 existing target，且本次 `noopener` / `noreferrer` 抑制 opener。此时 target name
仍属于新 browsing context 的真实状态，却不能让这个新 Page 回到 creator 的 related group，也不能借
browser-context-wide name map 让两个本应隔离的 Page 互相复用。

##### Chromium / WPT 合同与本轮范围

本轮继续对照本地 `~/chromium/src`：

- Blink `LocalDOMWindow::open()` 先调用 named target lookup；existing target 仍被导航，只有返回给 caller
  的 handle 受 noopener policy 影响。查找失败后才创建新 Window；
- Blink `FrameTree` 的 lookup 顺序仍是 current tree、Page tree、related Pages，并排除 closing Page；
- Content `RenderFrameHostImpl` 在 opener 被 suppress 的新建路径分配新的 virtual browsing-context group /
  `BrowsingInstance`。因此“先查 existing target”与“新 target sever group”是两个连续决策，不可合并为
  browser-context name lookup；
- WPT `auxiliary-browsing-contexts/named-lookup-noopener.html` 要求连续两次使用同一普通 name 的
  noopener `window.open()` 创建两个不同窗口，同时每个新窗口自己的 `window.name` 仍等于请求 name；
- WPT `windows/noreferrer-window-name.html` 对 noreferrer 锁定同样的“不互相复用”，并再次要求预先
  existing 的 named iframe/window 仍可先被命中。

这里仍是源码/WPT 合同对照，没有编译 Chromium，也没有运行 upstream WPT。本纵切只迁移 production
`window.open()` 的可解析、非 `javascript:`、普通 named suppress-opener 新建路径；hyperlink 已在下一节
E2C 迁移，form named target 又在 E2D 迁移；完整 nested-frame ordering、sandbox/COOP/remote endpoint
继续保留。

##### 稳定红灯与违反路径

renderer owner 回归在同一个 production opener Page 中，用相同 `isolated-popup-name` 先后执行 named
`noopener` 与 `noreferrer`。旧实现两次都落入 `open_lightweight_popup_window()`；第一条断言稳定得到：

```text
popup_id: left Some(2), right None
```

这证明即使 reservation 已标为 `RendererScriptAgentAdmission::Fresh`，opener host 仍额外拥有一份 caller
永远拿不到的 lightweight Window/Document identity。protocol 回归随后用同一 name 连续创建两个 target；
旧实现把后一个 target 写入 `BrowserContext::target_window_names`，稳定失败为：

```text
left: Some("TID-2")
right: None
```

这个 map 会把不相关 Fresh group 暴露成 browser-context-wide named target。代码审计同时确认 target name
只存在于 lightweight/protocol projection，fresh Page 的首个真实 realm 没有 creator-frozen name 输入。

##### Lightmount cutover

E2B 把 group policy 与首个 realm name 作为 renderer creation decision 的一部分：

```text
entered Window.open(url, ordinaryName, suppress-opener)
  -> current frame / related Page named lookup
       hit: navigate exact existing target; return null; preserve its opener edge
  -> no hit
       reserve RendererPendingAuxiliaryPage(Fresh)
       emit activation {
         popup_id: None,
         new_target_disposition: FreshNamed,
         target_name: ordinaryName,
         exact referrers + reservation
       }
  -> protocol creates one target, never consults/publishes the global name projection
  -> fresh Page bootstrap installs ordinaryName in the real Window slot and Page-group state
     before document-start scripts
  -> lookup from that Page may resolve itself; creator/other Fresh groups cannot resolve it
```

- 原来的 `renderer_selected_new_target: bool` 提升为
  `RendererPopupNewTargetDisposition::{Related, FreshUnnamed, FreshNamed}`。renderer 在 lookup/creation
  decision point 同时冻结“是否新建”“属于哪个 group”“首 realm 是否携带普通 name”；protocol 只消费该
  fact，不从 `can_access_opener`、target string 或 name map 重建 policy；
- suppress-opener 的空 target / `_blank` 继续标记 `FreshUnnamed`；新建 ordinary named
  `noopener`/`noreferrer` 标记 `FreshNamed` 并直接预留 Fresh Page，不再调用 lightweight popup owner。
  opener-preserving staged Page 显式标记 `Related`；E2B 当时尚未迁移的 `javascript:` / hyperlink producer
  不冒充已完成的 renderer decision；
- protocol 仅在 disposition 缺失时保留 legacy target-name fallback。`FreshNamed` target 不写入
  browser-context-wide `target_window_names`；`Related` 仍可保留 DevTools/legacy projection，但 exact
  renderer residence 才是 migrated lookup authority；
- `initial_top_level_browsing_context_name` 沿 initial empty-Document Page build 传到 renderer。
  `ScriptVmDefaultWorldBootstrap` 在 `finish()` 和 document-start scripts 之前，同时更新真实 V8
  `WINDOW_NAME_SLOT` 与 `RendererPageScriptEnvironment` 的 top-level name。后续 cross-document navigation
  继续复用 E2A 的 stable Page/group state，不需要 protocol 补写 realm；
- related staged Page 的这一 bootstrap 输入保持 `None`，避免 protocol adoption 用初始 target string 覆盖
  creator 在同步 WindowProxy 上已经完成的动态 `window.name` 修改。

这一纵切建立的窄不变量是：

1. 同一 opener 对相同普通 name 连续执行新建 `noopener` / `noreferrer`，每次都得到不同 Fresh Page，且
   不创建 opener-local lightweight owner；
2. 每个 fresh Page 的真实 realm 都观察到请求 name 与 `window.opener === null`；name 在首 realm 创建和
   document-start script 执行之间安装，并随该 stable Page 的 navigation 保留；
3. fresh Page 不进入 browser-context-wide name projection，也不进入 creator/其他 fresh Page 的 related
   lookup；它仍可在自己的 private Page group 中按 live name 精确命中自己；
4. existing named child/related target 的 lookup 仍先于新建，suppress-opener 只改变返回值和本次 opener
   mutation policy，不把 existing target 错误 sever 到 Fresh group；
5. group/name/referrer/session-storage policy 与 exact Page reservation 继续来自同一 renderer activation，
   protocol target admission 不产生第二份 Window、Document 或 loader。

#### Phase 5E2C：ordinary named hyperlink 的 renderer group lookup/creation

E2C 迁移 `<a>` 与共享 hyperlink activation 的 `<area>` 普通命名 target。它不新建另一套 link popup
registry，而是让已有 full creator capability 的 hyperlink producer 复用 E2A 的 related Page live registry
和 E2B 的 typed group disposition。与 `window.open()` 不同，hyperlink 没有同步 WindowProxy 返回值；但
target 选择、opener/referrer policy、initial realm name、Page admission 和 destination navigation 仍必须在
同一个 renderer decision point 完成，不能因此退回 protocol name map。

##### Chromium / WPT 合同与当前差距

本轮直接对照本地 Chromium `a03603fe9af6`：

- `third_party/blink/renderer/core/html/html_anchor_element.cc` 的 `HandleClick()` 先构造
  `FrameLoadRequest`，调用 `AnchorElementUtils::HandleRelAttribute()` 冻结 link relation，再把同一 request
  交给 `FrameTree::FindOrCreateFrameForNavigation()`；anchor 并没有一条绕过 frame-tree lookup 的独立
  browser-process name-map 路径；
- `anchor_element_utils.cc` 中 `noreferrer` 同时设置 no-referrer/noopener，`noopener` 只设置 noopener，
  `_blank` 在没有显式 `rel=opener` 时隐式 noopener。这些 policy 在 target 查找前已存在，但并不禁止命中
  existing named frame/window；
- `core/page/frame_tree.cc::FindFrameForNavigationInternal()` 的顺序是 source subtree、当前 Page 剩余
  frame tree、每个 non-closing related Page 的整棵 frame tree，最后才询问 embedder；每个候选还经过
  `CanNavigate()`。命中另一 Page 后会 focus，再次检查 detach；查找失败才由 `CreateNewWindow()` 新建；
- `core/page/create_window.cc` 只在 `!features.noopener` 时 clone session-storage namespace。因而
  “是否命中 existing target”“新 Page 属于 Related 还是 Fresh group”“是否 clone storage”是相邻但不同
  的决策，不能由 protocol 在 target 创建时从 name 字符串重建。

对应 WPT 给出 hyperlink 特有的可观察合同：

- `windows/auxiliary-browsing-contexts/named-lookup-noopener.html` 连续点击两个相同普通 target name 的
  `rel=noopener` anchor，要求得到两个不同 Window，同时两个真实 realm 的 `window.name` 都保留请求值；
- `windows/noreferrer-window-name.html` 要求两个新建同名 `rel=noreferrer` link 不互相复用，但预先存在的
  named iframe 和 named auxiliary window 仍分别可被同一个 noreferrer link 命中；existing window 的
  opener 状态不能被本次 suppressed relation 重写；
- 这些 case 同时说明 noopener policy 不能被实现成“先新建，再让 browser-context-wide name map 决定是否
  合并”。lookup 必须先在 source 可见的 browsing-context namespace 内完成，只有 miss 才执行 group split。

本轮没有编译 Chromium，也没有运行 upstream WPT，因此上述内容仍是源码/WPT 合同对照。Lightmount 当前
先覆盖 top-level 或 related auxiliary source 中具有完整 creator capability 的普通 named、可解析、非
`javascript:` hyperlink。现有 `navigate_hyperlink_target_browsing_context()` 仍保证当前 Page 的 named
iframe 先于 related top-level lookup；但 child-frame source 的完整 subtree/Page/related-Pages ordering、
related peer nested frame、`CanNavigate`、focus/detach transaction 尚未达到 Chromium 的完整算法。

##### 稳定红灯与违反路径

renderer 回归先让 production Page 点击一个 `target=related-hyperlink-name rel=opener` 的 link。旧实现虽
预留 auxiliary Page，却没有声明 renderer 已完成 group decision，稳定失败为：

```text
new_target_disposition: left None, right Some(Related)
```

同一回归随后用 `rel=noreferrer` 再导航相同 name，并要求 activation 携带第一次 reservation 的 exact
`{owner_local_host_id, page_id}`；最后连续点击两个相同 `isolated-hyperlink-name` 的 noopener/noreferrer
link，要求得到两个不同 Fresh reservation。

protocol 回归把违反路径拆成两个独立观察：

```text
related target realm: left "|false|#related-two"
                      right "relatedLinkName|true|#related-two"

same-name suppress-opener links: left 1 Target.targetCreated
                                 right 2 Target.targetCreated
```

第一条说明 opener host 的 named lightweight realm 与 target 采纳的 Page 仍是两份 identity；第二条说明
第二次 Fresh link 被 `BrowserContext::target_window_names` 合并进第一个 target。回归还主动清空 protocol
name projection，再执行 existing related target 的 `rel=noreferrer` 导航；只有 renderer group lookup
仍能精确复用原 Page，并保持它已有的 name/opener edge。

##### Lightmount cutover

E2C 将 hyperlink 路径改为下面的 owner 顺序：

```text
activate hyperlink(url, ordinaryName, rel)
  -> resolve source Document + named iframe lookup
       hit: navigate exact child
  -> freeze {opener exposure, three referrers, creator policy}
  -> related top-level Page lookup (independent of rel=noopener/noreferrer)
       hit: emit activation { exact renderer Page residence, no Page.windowOpen }
  -> no hit + opener preserved
       stage one real initial Page/realm with ordinaryName and opener
       emit activation { Related, popup_id, exact reservation }
  -> no hit + opener suppressed
       reserve one Fresh Page without opener-local lightweight owner
       emit activation { FreshNamed, popup_id: None, ordinaryName, exact reservation }
  -> protocol adopts/navigates the renderer-selected Page without name lookup
```

具体改动与边界如下：

- 原来只服务 `window.open()` 的 helper 更名为
  `related_page_named_target_for_navigation()`。`window.open()` 仍可传入 replacement opener；hyperlink
  lookup 始终传 `None`，所以 `rel=noopener/noreferrer` 命中 existing target 时只影响本次 source/referrer
  policy，不覆盖 target 已有的 Page-scoped opener edge；
- ordinary named hyperlink 在新建前调用同一 `RendererRelatedPageGroup` lookup。hit activation 只携带
  `RendererResolvedPopupTarget` 和 creator-frozen referrers，不预留 Page、不创建 lightweight record、也不
  产生 `Page.windowOpen`；protocol 通过 exact renderer residence 路由 navigation；
- opener-preserving miss 让 `open_lightweight_popup_window()` 启用 named real-Page staging。这里保留
  `popup_id` 只是同步 Window/initial auxiliary state 的 typed owner identity；staged Page、真实 realm、
  `window.name`、opener、session-storage clone 与 target admission 均复用 E2A 路径，并显式标记
  `RendererPopupNewTargetDisposition::Related`；
- suppress-opener miss 对 `_blank` 或 ordinary name 的非 `javascript:` URL 直接 reserve Fresh Page。
  `_blank` 现在显式标记 `FreshUnnamed`，ordinary name 标记 `FreshNamed`；两者都不创建 opener-local
  lightweight owner，Fresh target 也不写入 browser-context-wide name projection；
- `rel=opener target=_blank` 的新建 Related Page 同样得到显式 `Related` disposition。这让 E1 早期接入的
  `_blank` 两种 group admission 不再依赖 protocol 从 `exposes_opener` 推断；
- staged Related Page 的 session-storage store 与 initial storage key 直接取自 creation result，再以旧
  lightweight record lookup 作为 legacy fallback。真实 Page staging 不需要留下一份 mirrored record，
  因而不能在创建后反查那份本不应存在的状态；
- source 缺少完整 creator capability、URL 无法解析或为 `javascript:` 时仍走 legacy carrier。
  `javascript:` 需要在 selected target realm 中同步执行并受该 realm CSP/currentness 约束，不能仅靠
  async Page admission 机械迁移；
- 旧 `owner_scheduler_applies_legacy_hyperlink_popup_terminal_from_stable_page_route` 被删除。它等待
  opener-local loader 请求并从 mirrored Document 回写 opener，恰好要求已迁移路径保留第二 owner；新的
  renderer activation 和 protocol target-realm 回归覆盖正确责任边界。

这一纵切建立的窄不变量是：

1. full-creator ordinary named hyperlink 与 `window.open()` 使用同一 renderer Page-group name authority；
   protocol name projection 不能创建、合并或重定向 migrated target；
2. existing named iframe 仍先于 related top-level Page；existing related Page 精确复用且不产生第二个
   target，`rel=noreferrer` 不覆盖它已有的 opener edge；
3. opener-preserving miss 只创建一个 Related Page/realm/Document/loader；suppress-opener miss 每次创建
   不同 Fresh Page，同时每个真实 realm 都保留 ordinary `window.name` 且 opener 为 null；
4. `Related` / `FreshNamed` / `FreshUnnamed` 与 exact Page reservation 在 renderer creation point 一次冻结；
   referrer、session-storage 和 realm bootstrap 不由 protocol name map 反推；
5. reuse 不产生 `Page.windowOpen`，new target 只产生一次；target attach 观察的是被 renderer 选中的真实
   realm，而不是 opener-local mirror。

#### Phase 5E2D：form named / `_blank` 的 target + request 一体化迁移

E2D 迁移 HTML form submission 的 full-creator auxiliary target。这里不能把 E2C 的 hyperlink
helper 直接当作“打开 URL”：form target 选择完成时，HTTP method、encoded body、Content-Type、
submitter override、form data、referrer policy 和目标 Frame/Page 已经属于同一次 submission。若
renderer 只把 URL 交给 protocol，POST 会静默变成 GET；若只保留 request 而让 protocol 再查 name，
同名 Fresh/Related group 又会被 browser-context projection 错误合并。

##### Chromium / WPT 合同与旧实现违反路径

本轮继续使用本地 Chromium `a03603fe9af6`，直接核对以下 owner：

- `third_party/blink/renderer/core/loader/form_submission.cc::FormSubmission::Create()` 先复制 form
  attributes，再按“submitter attribute 是否存在”覆盖 `formaction` / `formenctype` / `formmethod` /
  `formtarget`。它随后构造一份 `ResourceRequest`；POST 在同一对象上设置 method、encoded body 和
  Content-Type；
- effective target 不是简单的 `form.target`：copied target 为空时使用 `Document::BaseTarget()`，再由
  `FrameLoadRequest::CleanNavigationTarget()` 清理。因而 `formtarget=""` 会覆盖 form 自己的非空
  target，并继续落到 `<base target>`，不能错误回退到 form target；
- form 的 `noreferrer` 同时设置 no-referrer/noopener，`noopener` 只抑制 opener，`_blank` 在没有
  `rel=opener` 时隐式 noopener。之后同一 `FrameLoadRequest` 和 effective target 进入
  `FrameTree::FindOrCreateFrameForNavigation()`；
- target lookup 返回的 `target_frame` 与完整 `resource_request_` 一起存入 `FormSubmission`。
  `HTMLFormElement::ScheduleFormSubmission()` 使用 target frame 的 scheduler，处理 target-local
  Navigation API / client-navigation cancellation；最终 `FormSubmission::Navigate()` 仍从保存的同一
  request 导航保存的同一 frame；
- WPT `form-submission-target/rel-{form,input,button,base}-target.html` 覆盖显式 form target、submitter
  `formtarget`、`<base target=_blank>` 与动态 rel；`resources/reltester.js` 要求 noopener 保留 referrer、
  noreferrer 清空 referrer，默认 `_blank` 不暴露 opener；
- `form-target-request-header.html` 明确向 `_blank` POST，并由服务端要求 Content-Type；
  `form-submission-0/submit-entity-body.html` 又覆盖 urlencoded、multipart、text/plain 的 exact entity
  body。E2D 没有运行 upstream WPT，因此这些仍是源码/WPT 合同对照，不是通过率声明。

Lightmount 旧路径在 form owner 内发生了两个稳定分叉：

```text
ordinary name
  -> only try named iframe
  -> miss: return false, no auxiliary Page/target

POST + _blank / other non-current target
  -> submit_post_form_to_top_level_browsing_context()
  -> navigate opener Page, ignore selected auxiliary target
```

接入前 renderer production 回归提交一个 `target=related-form-name rel=opener` 的 GET form，失败为
`popup activations: left 0, right 1`。两条 protocol HTTP 回归分别要求 ordinary named target creation
和 `<base target=_blank>` POST creation，均稳定失败为 `Target.targetCreated` 消息为空。后者同时说明旧
POST carrier 没有到达新的 target；它不是单纯少发了一个 CDP event。

##### effective target 与 existing-frame 优先级

form owner 现在先一次性计算 effective target：

```text
submitter has formtarget attribute ? exact formtarget value
                                 : form target attribute
  -> selected value empty/missing ? source Document first live base target
                                  : selected value
  -> still empty/missing => current browsing context
```

target lookup 顺序保持窄且与 E2C 一致：

1. ordinary name 先查现有 named iframe；命中后继续使用原有 deferred child request、FormData
   `NavigateEvent`、per-form pending-child cancellation 和 exact child handle；
2. named iframe miss 后才进入 shared element auxiliary selector；
3. ordinary name 在 renderer related Page group 中查 exact live top-level Page，不依赖 protocol
   `target_window_names`；
4. related hit 携带 `RendererResolvedPopupTarget`，不创建 Page、不产生 `Page.windowOpen`；
5. miss 且保留 opener 时 staging 一个真实 Related initial Page；miss 且抑制 opener 时 reserve
   `FreshNamed` 或 `_blank` 的 `FreshUnnamed` Page；
6. source 没有 full creator capability、`javascript:` 或其它尚未迁移条件继续 fail closed 到 legacy
   carrier，不能借本纵切声称 child-source 完整 ordering 已经完成。

`rel=noopener/noreferrer` 仍不改变 existing-target lookup。命中已有 related Page 时，本次 source 的
opener exposure/referrer policy 只进入 navigation activation；目标 Page 既有的 Page-scoped opener edge
不被改写。新建时 relation 才决定 Related/Fresh admission、initial opener、session-storage clone 和首
realm name。

##### 一个 typed request 穿过 renderer / protocol authority

原来 popup activation 只有 `url: String`，protocol target navigation 固定调用 GET helper。E2D 把已有
top-level location request 抽成公共 `RendererTopLevelNavigationRequest`：

```text
RendererTopLevelNavigationRequest
  { url, method, raw body bytes, explicit headers, navigation kind }

form target selection
  -> RendererPendingPopupActivation { boxed exact request, referrers, Page decision }
  -> PagePreparedPopupActivation
  -> PopupTargetCreation
  -> PopupTargetNavigationClaimIdentity
       { exact TargetPageResidenceIdentity, boxed exact request, referrers, kind }
  -> Held -> Published -> Consumed
  -> request-aware stable Page navigation
```

GET/window.open/hyperlink producers 仍通过 `RendererTopLevelNavigationRequest::get()` 产生相同默认行为；
form POST 改用 `new()` 保存 raw encoded bytes 和 `Content-Type`。activation 的 URL accessor、
`Page.windowOpen`、target URL projection 与最终 request 全部读取同一个 carrier，并用 invariant 拒绝
target-selection URL 与 request URL 分裂。

protocol existing-target reuse 与 new-target admission 都把 request 整体交给
`PopupTargetNavigationOwnerAction::capture()`。claim 发布/消费仍先校验 exact browser context、target、
Page residence generation 和 initial/named-reuse kind；验证通过后才调用 request-aware renderer navigation
entry。由此 wait-for-debugger held action、background target、named reuse 和 stale Page rejection 不会只
保留 URL 而丢失 POST 元数据。Network/Fetch 观察到的 method、postData、Content-Type 与服务端实体来自
同一个 target Page loader。

##### form-specific target event 与 referrer

shared element selector 只共用 target/group/referrer/creation primitive，没有删除 form owner：

- POST serialization 与 `formdata` event 仍在 form submission owner 完成；
- named iframe hit 继续走原 form-specific child path；
- related top-level hit 会在精确 target Window/realm 同步派发 cross-document `NavigateEvent`，POST
  `event.formData` 保留 source entries，source element 和 user-initiated fact 一并传入；目标
  `preventDefault()` 后 submission 返回 accepted/canceled，不生成 popup activation，也不启动网络请求；
- creator policy 仍一次冻结 initial empty Document referrer、HTTP Referer 和 destination
  `document.referrer`。noopener 只切 opener/group，noreferrer 同时把三者置空；
- new `_blank` 默认使用 FreshUnnamed，但保留网络/document referrer；`rel=opener` 则使用 Related。

这个 event 接入只覆盖已经能由 renderer group 精确解析的 related top-level hit。它不等于完整实现
Blink `Frame::ScheduleFormSubmission()`：同一 form 的跨 task supersession、target loader
`CancelClientNavigation()`、parser cancellation、RemoteFrame scheduler 与 sandbox `allow-forms` 仍需在
通用 form/navigation owner 后续补齐，不能靠删除旧 activation 或 drain queue 伪装正确。

##### 本纵切建立的不变量与证据边界

E2D 建立以下窄不变量：

1. form effective target、relation、exact request 和 renderer Page decision 在一个同步 owner 中冻结；
2. existing named iframe 优先且旧 FormData/cancellation 行为不回归；ordinary related Page reuse 不依赖
   protocol name projection；
3. named/`_blank` GET 与 POST 使用同一 Related/Fresh target algorithm，POST 不再导航 opener 或退化为
   GET；
4. `RendererTopLevelNavigationRequest` 从 activation 到 exact target-local claim 不拆 URL/method/body/header；
5. existing related target 可在自己的 realm 观察/cancel form navigation；new target 只产生一个 Page、
   Document、loader 和服务端副作用；
6. `<base target>` 与空 submitter override 使用 source Document 的 live base-target authority。

接入前/后的聚焦证据：

```bash
cargo nextest run -p lightmount-renderer-v8 \
  -E 'test(per_page_isolate_policy_keeps_window_open_routes_page_owned)' \
  --no-fail-fast --status-level fail --final-status-level fail
# 接入前：1 failed，named form popup activation left 0/right 1。
# 接入后：1 passed；覆盖 Related creation、exact named POST reuse、target NavigateEvent/FormData cancellation、
# 两个 same-name Fresh form、base target=_blank、空 submitter formtarget override 和 exact request fields。

cargo nextest run -p lightmount-protocol \
  -E 'test(named_form_post_reuses_renderer_group_target_and_preserves_exact_request) | test(base_target_blank_form_post_creates_fresh_target_with_exact_request)' \
  --no-fail-fast --status-level fail --final-status-level fail
# 接入前：2 failed，两个 case 的 Target.targetCreated 都为空。
# 接入后：2 passed；HTTP server 与 Network.requestWillBeSent 同时验证 POST、raw body、Content-Type、
# Referer/noreferrer、Related reuse、Fresh _blank、opener 保留/抑制、window.name 与唯一 response Document。

cargo nextest run -p lightmount-renderer-v8 -p lightmount-protocol \
  -E 'test(per_page_isolate_policy_keeps_window_open_routes_page_owned) | test(form_target_blank_reloads_rel_opener_policy_for_each_submission) | test(canceled_post_form_navigation_aborts_signal_without_synthetic_timer) | test(detached_child_form_submit_targets_named_iframe_without_shadow_controls) | test(formdata_event_appended_entries_are_submitted_to_named_iframe) | test(distinct_forms_keep_distinct_pending_child_target_submissions) | test(programmatic_form_submit_keeps_successive_distinct_child_targets) | test(form_top_and_parent_targets_queue_plain_top_level_navigation) | test(renderer_top_level_form_post_preserves_request_through_document_commit) | test(named_form_post_reuses_renderer_group_target_and_preserves_exact_request) | test(base_target_blank_form_post_creates_fresh_target_with_exact_request) | test(named_opener_hyperlink_creation_and_reuse_are_owned_by_renderer_group) | test(named_suppress_opener_hyperlinks_create_distinct_fresh_groups_with_live_names) | test(noopener_and_noreferrer_popups_have_one_real_navigation_with_creator_referrer_policy)' \
  --no-fail-fast --status-level fail --final-status-level fail
# 14 passed；覆盖 form current/child/auxiliary 三条 request owner 与相邻 hyperlink/referrer/group 路径。
```

#### Phase 5E2E：child-source 与 related nested frame-tree named resolver

E2A-E2D 的 renderer group authority 仍有一个结构性缺口：name-indexed registry 只描述 related
top-level Page，当前 Page 的 child lookup 又从整棵树根开始扫描。只要 source 本身是 child，或相同 name
出现在 related Page 的 nested frame 中，选择顺序和导航 owner 就会分裂。E2E 不再给这些调用方各加一次
fallback，而是把查找提升为 renderer-owned、source-relative 的 frame-tree resolver。

##### Chromium / WPT 合同

本轮继续直接对照本地 Chromium `a03603fe9af6`：

- `core/page/frame_tree.cc::FindFrameForNavigationInternal()` 先从 source frame 本身开始 preorder 遍历其
  subtree；再从当前 Page main frame 开始遍历整棵树，但排除刚才已经检查的 source descendants；最后按
  `Page::RelatedPages()` 顺序遍历每个 non-closing related Page 的 main frame 与完整 descendants。每个
  name match 都在原位置调用 `current_frame->CanNavigate(*frame, url)`，不能先选中再在调用方补权限；
- `core/frame/local_frame.cc::CanNavigate()` 的普通 nested-frame 路径允许 source 与 target 本身或 target
  任一 ancestor 同源；`javascript:` URL 更严格，必须与 target 本身同源。该函数还包含 sandbox navigation
  flags、top-level opener/user-activation、top navigation、fenced frame 等分支，E2E 没有把这些未建模的
  policy 伪装成已完成；
- WPT `browsing-context-names/duplicate-name-order.html` 构造同名 source descendant、current Page sibling
  和多个 popup，依次要求 `ChildA`、`SiblingB`、`PopupC`；
- WPT `windows/targeting-cross-origin-nested-browsing-contexts.html` 从 opener 尝试导航一个 cross-origin
  related Page 内的 nested name。因为 source 不能访问 target 及其任何 ancestor，旧 nested candidate
  必须被跳过，最终打开同名 top-level context；回传的 `isTop` 必须为 `true`。

本轮没有编译 Chromium，也没有运行 upstream WPT；以上仍是源码与 WPT 合同对照。Lightmount 的本地
回归复现相同的树顺序和普通 origin/ancestor 决策，不把它表述为完整 `LocalFrame::CanNavigate()`。

##### 稳定红灯与违反路径

首条 renderer 回归从一个 nested requester 执行五次 `window.open()`。旧实现稳定表现为：

```text
sourceSubtree  -> earlier-current-sibling      # 错过 requester descendant
currentTop     -> child-colliding-with-current-top
currentRemainder -> current-page-remainder     # 唯一正确项
relatedNested  -> null
relatedPageOrder -> null
```

nextest run `13005909-fbfa-4adc-a01c-30b8ccd0c0c8` 因此失败。违反路径有三条：

1. current Page lookup 从 main Document 的全局 child registry 起点扫描，不知道 source subtree；
2. `RendererRelatedPageGroup::named_targets` 只能返回同名 top-level Page，无法遍历该 Page 的 child registry；
3. hyperlink 命中不了 related nested child 后落入 auxiliary creation，navigation 不在目标 child 所属 Page
   执行。

调试过程中还暴露一个独立但同属访问 authority 的错误：production staged Page 的三个 nested handle
已经存在，name 也匹配，但普通 `CanNavigate` 仍返回 false。初始 `about:blank` 的 V8 security token、storage
origin 和 Window runtime state 都已继承 creator，Rust access check 却重新从 `document_url=about:blank`
构造一个 host-local opaque origin。修正后才可能让 frame-tree resolver 与 stable WindowProxy 的既有
same-origin 事实一致。

##### renderer owner cutover

E2E 建立以下 owner 链：

```text
ordinary named navigation(source Window/element, destination URL)
  -> resolve exact source WindowExecutionContextIdentity + child/top dispatch scope
  -> source subtree preorder
  -> current Page top + remaining preorder
  -> each live related Page in group order
       top WindowProxy
       current target Context -> target JsContextHost -> complete child preorder
  -> candidate-local CanNavigate filter
  -> typed result {current top | current child | related top | related child}
  -> navigate through the selected context's owner
```

具体实现边界如下：

- `RendererRelatedPageGroup` 在原有 top-level name index 之外保存 weak、按 Page admission 顺序排列的
  top-level target。weak entry 只有在 Page lifecycle 为 `Active`、stable main WindowProxy 存在且 current
  default Context 已绑定时才参与查找；close/discard 与尚未完成 bootstrap 的 Page 不会成为候选；
- 每个 top-level target 保存当前 `v8::Context`，main default realm 在 native bridge host slot 安装后绑定，
  navigation replacement 会覆盖为新 Context。stable WindowProxy 的 creation context 可能仍是 opener-side
  facade，不能作为 target `JsContextHost` 地址；current Context slot 才是当前 Document owner 的权威定位；
- source child 由 `window.open()` receiver 的 stable child marker 或 hyperlink source node 的 owner Document
  解析，而不是假设 callback 的 entered scope 必然是 top。当前 Page child handles 继续使用 live DOM/document
  order，subtree membership 由 child-parent registry 逐级判断；
- related Page 先从 current Context slot 解析 target host，再同步该 host 的 live child subtree。resolver
  返回的 related-child raw host pointer 只在当前 V8 callback 内存在；Page group 持久状态保存的是 V8
  `Global<Context>` 与 typed Page residence，不保存裸指针；
- 普通 nested candidate 只有在 source 可访问 target 或其任一 ancestor 时才命中；`javascript:` candidate
  额外要求 source 可访问 target 本身。通过筛选后，child WindowProxy 仍按 source observer realm 决定
  same-origin wrapper 或 restricted proxy，不把 target raw global 泄漏给 caller；
- related child navigation 调用 target host 的 child navigation owner；same-document / same-origin target
  event 和 cross-document child request 都由该 Page 自己产生。related top-level 结果仍携带 exact
  `RendererResolvedPopupTarget` 进入 E2A-E2D 的 Page activation/claim 路径；
- initial Document 的 effective serialized origin 现在与 loader fetch context、Window runtime state 和 V8
  token 一起传入 `JsContextHost` / main `FrameOwnerStore`。普通 URL 仍由 response URL 产生 origin；tuple
  origin 的 inherited initial Document 不再错误地从 `about:blank` URL 重建 opaque 身份；
- E2E 当时接入的 producer 是 ordinary-name `window.open()` 与 hyperlink；form E2D 的
  request/scheduler owner 当时尚未切到这份 typed result，避免只替换 name lookup。后续 E2F 已完成 local
  target owner cutover，跨 Page cancellable scheduler identity 仍按下节边界保留。

##### 本纵切建立的不变量与证据

E2E 建立以下窄不变量：

1. named lookup 顺序由 source-relative frame tree 与 related Page order决定，不由 name-indexed top-level map
   或 protocol target projection 决定；
2. current top、current child、related top 与 related child 都保留精确 owner；命中 related nested target
   不创建第四个 auxiliary Page，hyperlink 在 target Page 内导航原 child；
3. candidate name match 不代表可用；普通 nested target 必须通过 target/ancestor origin check，失败后继续
   搜索或创建新 context，且不得修改被拒绝 candidate；
4. tuple-origin initial inherited `about:blank` 的 Rust access origin 与已继承的 V8/security/storage
   authority 一致；
5. Page group 不持久化 `JsContextHost` raw pointer，replacement 后 lookup 只从 current Context 重新解析 host。

接入过程中与最终聚焦证据：

```bash
cargo nextest run -p lightmount-renderer-v8 \
  -E 'test(related_page_named_frame_lookup_follows_chromium_frame_tree_order)' \
  --no-fail-fast --status-level fail --final-status-level fail --failure-output immediate
# run 13005909-fbfa-4adc-a01c-30b8ccd0c0c8：接入前 1 failed，稳定暴露上述四个错误选择。
# run 46408fd0-0683-4ab8-b5db-c9637796afd1：origin/owner 修正后 1 passed。

cargo nextest run -p lightmount-renderer-v8 \
  -E 'test(related_page_named_frame_lookup_follows_chromium_frame_tree_order) | \
      test(named_frame_lookup_skips_candidate_the_source_cannot_navigate)' \
  --no-fail-fast --status-level fail --final-status-level fail --failure-output immediate
# run c3857b05-7b03-4184-91a6-1598b1f51407：2 passed。
# 第二条由 data: opaque child 发起 lookup；同名 current-Page candidate 及 top ancestor 均不可访问，
# 因而必须创建新 auxiliary Page，并验证旧 candidate marker/URL 未变化。

cargo nextest run -p lightmount-renderer-v8 -p lightmount-protocol \
  -E 'test(related_page_named_frame_lookup_follows_chromium_frame_tree_order) | test(named_frame_lookup_skips_candidate_the_source_cannot_navigate) | test(per_page_isolate_policy_keeps_window_open_routes_page_owned) | test(window_open_noopener_navigates_existing_named_iframe_and_returns_null) | test(hyperlink_target_blank_reloads_rel_opener_policy_for_each_activation) | test(hyperlink_javascript_url_csp_checks_the_source_document_before_target_selection) | test(named_opener_hyperlink_creation_and_reuse_are_owned_by_renderer_group) | test(named_suppress_opener_hyperlinks_create_distinct_fresh_groups_with_live_names) | test(named_form_post_reuses_renderer_group_target_and_preserves_exact_request) | test(base_target_blank_form_post_creates_fresh_target_with_exact_request)' \
  --no-fail-fast --status-level fail --final-status-level fail --failure-output immediate
# run 067d4968-3fc3-4692-b512-5c38faa76cf5：10 passed；覆盖 E2E 与 E2A-E2D/JavaScript-CSP 邻接面。

cargo nextest run -p lightmount-renderer-v8 \
  -E 'test(related_page_named_frame_lookup_follows_chromium_frame_tree_order) | \
      test(named_frame_lookup_skips_candidate_the_source_cannot_navigate)' \
  --stress-count 50 --flaky-result fail --test-threads 8 --no-fail-fast
# run fe5b9c3c-c503-4c60-8b55-b6586dddae3a：50/50 iterations passed，每轮 2/2。
```

#### Phase 5E2F：form exact request 消费 typed frame-tree resolver

E2D 已把 form 的 effective target、method/body/Content-Type/referrer 和 target-realm
`NavigateEvent` 冻结到同一 submission carrier，E2E 又建立 source-relative typed resolver；但两者仍未
接通。旧 ordinary-name form 路径只先查 current Page named iframe，miss 后直接进入 auxiliary selector。
因此 related Page nested frame 即使已经由 E2E 找到，也没有机会接收 POST request，更无法由目标 Page 的
child scheduler 执行。

##### Chromium 合同与本轮责任边界

本轮继续对照本地 Chromium `a03603fe9af6`：

- `core/html/forms/form_submission.cc::FormSubmission::Create()` 把 target、method、encoded body、headers、
  referrer policy 与 source form state 冻结进一次 `FormSubmission`；`Navigate()` 把同一 request 交给
  已选择的 target Frame，不在 target owner 内重新从 DOM 拼装；
- `core/html/forms/html_form_element.cc::HTMLFormElement::ScheduleFormSubmission()` 先通过
  `FindFrameForNavigation()` 取得精确 Frame，再让 target LocalFrame 的 scheduler 安排 navigation；同时
  target loader client navigation 会被取消，form 保存的上一份 cancellable closure 会在新 submission
  到来时作废；
- `core/frame/frame.cc::Frame::ScheduleFormSubmission()` 与
  `core/page/frame_tree.cc::FindOrCreateFrameForNavigation()` 共同说明 lookup result、request 与 scheduler
  owner 不能拆成三次晚绑定；RemoteFrame 可以有不同 scheduler endpoint，但仍消费同一个 selection；
- 这份合同不等价于“目标 entry 覆盖旧 pending request”。同一 form 从 target A 改投 target B 时，source
  form 持有的 cancellation identity 仍应取消 A；这是 E2F 明确保留到下一纵切的边界。

本轮没有编译 Chromium，也没有运行 upstream WPT。源码对照证明 owner 关系；本地回归只覆盖 local
Frame、同源 direct response 与当前已有的 Page scheduler，不把它外推成 RemoteFrame 或完整 sandbox
form submission。

##### 违反路径、测试校正与证据强度

接入前的实际路径是：

```text
ordinary named form
  -> current-Page named iframe lookup
  -> miss
  -> E2D auxiliary related-top selector
  -> related nested frame 不可表示
  -> 创建新的 auxiliary top-level target
```

最初诊断 run `d67dd295-4bef-4427-af76-4308c9764d11` 的 provisional 回归确实观察到新 popup，但随后确认
测试使用的 `create_related_test_html_page_for_script_agent_experiment()` 只共享 script agent/isolate，
没有加入 production browsing-context group；该 run 因 setup 不成立而废弃，不能当作浏览器语义红灯。
最终回归改为真实 `window.open()` 同步创建 initial realm，消费 activation 中的 exact Page reservation，
再 adopt staged `about:blank` Page 并在其中建立 named child。没有为这份校正后的 setup 保留可信的
pre-cutover run；接入前证据因此是上述可审计源码路径而不是红测 run id，强度低于 E2E 的稳定红灯。

第一轮完整邻接集 run `a4cb34d9-bf0a-44d7-a28e-282cacc85931` 为 16 passed / 2 failed，暴露了两个真实
回归，而不是用 drain/retry 掩盖：

1. typed GET request 让 standalone upstream fixture 从旧 URL fast path 变成无条件 async，nested target
   已命中、event 已允许且 request 已入队，但 fixture 不再同步 materialize；
2. HTTP `Referer` 已正确来自 source Page，child entry 应用 loaded policy 时却漏拷贝
   `document_referrer`，最终 response Document 仍观察到旧 initial `about:blank` referrer。

修复分别落在 request-aware fixture materialization 与 child policy commit owner，而不是 form caller 或
测试等待循环。

##### typed owner cutover

E2F 现在使用以下单向 owner 链：

```text
form submission owner
  -> freeze RendererTopLevelNavigationRequest {URL, method, body, headers, kind}
  -> E2E source-relative resolver
       CurrentTopLevel   -> exact current Context + Page pending-location owner
       CurrentPageChild  -> exact child handle + current Page child scheduler
       RelatedTopLevel   -> current target Context + exact RendererResolvedPopupTarget
       RelatedPageChild  -> callback-scoped target JsContextHost + exact child handle
       miss              -> E2D Related/Fresh auxiliary creation
  -> dispatch NavigateEvent/FormData in selected target realm
  -> queue the same immutable request through that target's owner
```

关键实现不变量如下：

- ordinary named GET/POST 不再各自做 name lookup。`FormSubmissionMethod` 先转成 E2D 已有的
  `RendererTopLevelNavigationRequest`，method、raw encoded body、Content-Type 与 browser navigation kind
  在 resolver 前冻结；
- typed top-level result 除 stable WindowProxy 外显式携带 current target `v8::Context`。stable proxy 的
  creation context 可能仍是 opener-side facade，target-realm `NavigateEvent` 和 target host 定位只能使用
  current Context slot；
- child-source form 的 creator 直接复用现有 child stable WindowProxy、base URL 与 policy container。
  related-top 命中不会因为旧 helper 只识别 root/lightweight Document 而退回无 reservation 的 popup action；
- current/related child 都消费 `ChildBrowsingContextNavigationRequest`。该 carrier 额外保存 source initiator、
  policy-filtered `Referer` 和目标 Document 应观察的 referrer；target loader 禁止再次按 target parent 推导
  referrer，避免 cross-Page handoff 后改写 source；
- inherited `about:blank` / `about:srcdoc` source 不能把字面 `about:` URL 当 tuple-origin authority。source
  carrier 从 child policy container 读取 creator URL，与现有 stable WindowProxy/security token 的继承事实
  对齐；
- child network response、local URL snapshot 与 request-aware GET fixture 都把 referrer 写回同一
  `DocumentPolicyContainer`。因此 request header、entry snapshot 和最终 `document.referrer` 不再由三份状态
  分别决定；POST 或非 fixture request 仍走真实 async loader；
- current-Page child 保留既有 per-form cancellation：submitter activation 取消该 form 的全部旧 child
  target，programmatic submit 取消同一 target 的旧 request，成功 queue 后才登记 pending target；
- related-child raw host pointer 仍只在一次 V8 callback 内使用。跨 Page cancellation 没有把它持久化到 form
  state；后续必须保存 typed Page/Frame scheduler identity，而不是泄漏 host pointer。

##### 本纵切建立的回归与验证

production-style renderer 回归锁住：

- real related Page 中的 nested named frame 消费 form POST，不产生第二个 popup activation；
- target child realm 观察 exact destination、`FormData`、FORM source element 与 `userInitiated=false`；
- target Page 自己完成 child lifecycle/Document commit，server 同时观察 POST path、urlencoded
  Content-Type、source-derived `Referer` 与 raw body；response child `document.referrer` 与 source URL 一致；
- E2E 原 frame-tree 回归又从 nested requester 提交两个可取消 POST，分别命中 related top 与 related child
  的精确 realm，证明 child-source 也复用了同一 resolver/WindowProxy 基础；
- 既有 current-child formdata、同 form/不同 form supersession、detached child fixture、Related/Fresh
  top-level protocol handoff 与 hyperlink/referrer 邻接行为保持不变。

```bash
cargo nextest run -p lightmount-renderer-v8 \
  -E 'test(detached_child_form_submit_targets_named_iframe_without_shadow_controls) | \
      test(related_page_named_form_post_uses_nested_target_owner_and_exact_request)' \
  --no-fail-fast --status-level fail --final-status-level fail --failure-output immediate
# run 4980908e-6d47-43ae-b615-4543169a5164：2 passed；覆盖 request-aware fixture 与
# related-child HTTP/Referer/document.referrer commit。

cargo nextest run -p lightmount-renderer-v8 -p lightmount-protocol \
  -E 'test(related_page_named_form_post_uses_nested_target_owner_and_exact_request) | test(related_page_named_frame_lookup_follows_chromium_frame_tree_order) | test(named_frame_lookup_skips_candidate_the_source_cannot_navigate) | test(per_page_isolate_policy_keeps_window_open_routes_page_owned) | test(form_target_blank_reloads_rel_opener_policy_for_each_submission) | test(canceled_post_form_navigation_aborts_signal_without_synthetic_timer) | test(detached_child_form_submit_targets_named_iframe_without_shadow_controls) | test(formdata_event_appended_entries_are_submitted_to_named_iframe) | test(submit_button_click_supersedes_programmatic_submit_after_target_change) | test(distinct_forms_keep_distinct_pending_child_target_submissions) | test(programmatic_form_submit_keeps_successive_distinct_child_targets) | test(form_top_and_parent_targets_queue_plain_top_level_navigation) | test(renderer_top_level_form_post_preserves_request_through_document_commit) | test(named_form_post_reuses_renderer_group_target_and_preserves_exact_request) | test(base_target_blank_form_post_creates_fresh_target_with_exact_request) | test(named_opener_hyperlink_creation_and_reuse_are_owned_by_renderer_group) | test(named_suppress_opener_hyperlinks_create_distinct_fresh_groups_with_live_names) | test(noopener_and_noreferrer_popups_have_one_real_navigation_with_creator_referrer_policy)' \
  --no-fail-fast --status-level fail --final-status-level fail --failure-output immediate
# run 90b62837-12de-4bbd-95b3-136a83d35c32：18 passed。

cargo nextest run -p lightmount-renderer-v8 \
  -E 'test(related_page_named_frame_lookup_follows_chromium_frame_tree_order) | \
      test(related_page_named_form_post_uses_nested_target_owner_and_exact_request)' \
  --stress-count 20 --flaky-result fail --test-threads 4 --no-fail-fast \
  --status-level fail --final-status-level fail
# run 3e035af5-4873-418b-9a2c-5fb201f70907：20/20 iterations passed，每轮 2/2。
```

E2F 有意没有声称完成 Chromium 的整个 scheduling contract：跨 Page same-form cancellation、target loader
`CancelClientNavigation()`、parser cancellation、RemoteFrame scheduler、sandbox `allow-forms` 与 top-level
`CanNavigate()` 仍未实现。child-source 命中 current top 虽已保留 method/body/headers 和 exact target owner，
但当时 top-level protocol carrier 仍只记录 root Document lifecycle，尚未携带 child source/referrer identity（后由
E2H 解决）；这与
redirect、cross-origin/downgrade referrer 再计算一起需要单独纵切，不能由本轮 related-child direct-response
回归外推。

#### Phase 5E2G：跨 Page same-form cancellable scheduler identity

E2F 已经把 request 交给 exact target child owner，但 cancellation state 仍是 source-host-local 的
`HashMap<form DomHandle, Vec<target DomHandle>>`。related child queue 成功后没有登记，因为 callback-scoped
`target_host_ptr` 不能持久化；同一 form 随后由 submitter 从 related target A 改投 B 时，A 的 child task、
main-resource loader 与 parser ledger 会继续存活。与此同时，旧 local map 只比较 target handle：如果 A 的
form navigation 已被普通 `location` navigation 替换，稍后的 submitter 仍会把这份较新的无关 navigation
一起清掉。

##### Chromium 合同、本轮边界与红测

本轮继续以本地 Chromium `a03603fe9af6` 为基线：

- `HTMLFormElement::PrepareForSubmission()` 在 user/submission path 调用保存的
  `cancel_last_submission_`；`submitFromJavaScript()` 则直接进入 `ScheduleFormSubmission()`；
- `HTMLFormElement::ScheduleFormSubmission()` 取得 exact target Frame 后，使用 target LocalFrame scheduler，
  并在提交前执行 target `CancelPendingJavaScriptUrls()` 与 loader `CancelClientNavigation()`；
- `Frame::ScheduleFormSubmission()` 保存 `form_submit_navigation_task_version_`，返回的 cancellation closure
  只有 version 仍匹配时才取消 target Frame 的 task。也就是说 cancellation identity 必须同时限定 target
  Frame 与 scheduler generation，不能只保存一个可复用的 DOM owner handle；
- RemoteFrame 会退回 source scheduler，因此本轮 local child binding 不能外推为完整 remote endpoint 设计。

实现前的 focused run 同时固定两条失败：

```bash
cargo nextest run -p lightmount-renderer-v8 \
  -E 'test(submitter_cancels_previous_same_form_navigation_in_a_related_page_child) | \
      test(submit_button_does_not_cancel_a_newer_non_form_navigation_in_the_previous_target)' \
  --no-fail-fast --status-level fail --final-status-level fail --failure-output immediate
# run 3a65beae-5ac1-4f24-98a8-b675ad5c768e：0 passed、2 failed。
# local case 中 replacement URL pending state 被旧 form 记录清空；related case 中 A/B 都未受 source-side
# exact cancellation 约束。
```

这两条红测只证明本地违反路径，不声称等价覆盖 Chromium 的所有 form task timing。最终 related 回归又被
加强为：先让 A 的 GET 真正进入 libcurl transport 并阻塞 response，再从 source Page 同步 click retarget 到
B；server 必须观察 A connection 被 cancel、B 是唯一完成 commit 的 response。

##### typed route 与 exact target-owner cancellation

E2G 的 owner 链现在是：

```text
source HTMLFormElement state
  -> PendingFormSubmissionChildNavigation {
       target:
         CurrentPage { BrowsingContextId }
         | RelatedPage {
             RendererResolvedPopupTarget,
             target root RendererDocumentLifecycleIdentity,
             BrowsingContextId
           },
       FrameDocumentNavigationLoadBinding
     }
  -> later submission takes the applicable source-owned route
  -> related Page residence resolves its current Context/JsContextHost
  -> root Document identity + BrowsingContextId resolve the exact live child
  -> target owner cancels only if current navigation-load binding still matches
```

关键不变量如下：

- persisted form state 不再保存 target `DomHandle` 或 `JsContextHost*`。`DomHandle` 只在当前 callback 内从
  stable `BrowsingContextId` 反查；related Page host 只通过 stable Page residence 的 current Context slot
  重新取得；
- related route 额外冻结 target root Document lifecycle identity。相同 Page residence 在 main Document
  replacement 后不能让旧 route 命中新 host 中恰好碰撞的 child/navigation allocator 值；
- `FrameDocumentNavigationLoadBinding` 同时限定 target Document task owner、navigation id 与 load-delay
  token。相同 child 已被普通 navigation 替换时，旧 form route 会从 source state 移除，但 target cleanup
  必须 no-op；
- submitter path 在新 target 是 child、top-level 或 miss/create 时都先消费该 form 的既有 child routes；
  programmatic path 保留既有回归合同，只替换相同 target 的 pending form navigation，不误删发往不同 child
  的 programmatic submissions；
- queue 成功后由 target owner 返回 exact navigation-load binding，再由 source form 登记。失败 queue 不会
  产生一个看似可取消、实际没有 scheduler generation 的 route；
- exact target cleanup 依次撤销 pending Window/entry seed、reserved service-worker client、child commit
  task、当前 Document parser/script ledger 与 exact pending main-resource load，随后 settle 同一个
  navigation/load-delay owner 并同步 child Window state；
- `NavigationResourceLoader::cancel()` 现在在 pending child load 被 owner 删除前显式调用。resource task
  持有的 clone 因而不能让 libcurl transport 在 exact form-cancel ledger 已清空后继续运行。普通 navigation
  supersession 仍保留既有 historical Network terminal，不套用这条主动 transport cancellation；
- related target commit 无法反向借用 source host 清理 form map。完成后的 route 可能暂时留到该 form 的
  下一次相关提交，但 exact root/child/load 三重校验会使它安全 no-op；每个 target identity 只保留一项，
  不按重复提交无限追加。

##### 本纵切建立的回归与当前证据

```bash
cargo nextest run -p lightmount-renderer-v8 \
  -E 'test(submitter_cancels_previous_same_form_navigation_in_a_related_page_child) | \
      test(submit_button_does_not_cancel_a_newer_non_form_navigation_in_the_previous_target)' \
  --no-fail-fast --status-level fail --final-status-level fail --failure-output immediate
# run 18d9c0d9-bcae-4c49-88ab-f38dfa5cb5a2：2 passed。
# related case 已是 in-flight HTTP 版本：A transport close、A 保持 about:blank、B request/Document commit
# 与 response body 均被断言；local case 证明 stale form token 不会清掉 replacement navigation。

cargo nextest run -p lightmount-renderer-v8 -p lightmount-protocol \
  -E '<E2G 两条回归 + E2F/form/popup 邻接矩阵 18 条>' \
  --no-fail-fast --status-level fail --final-status-level fail --failure-output immediate
# run 865e048e-dc64-4084-9d59-09947ce6d1ca：20 passed。

cargo nextest run -p lightmount-renderer-v8 \
  -E 'test(submitter_cancels_previous_same_form_navigation_in_a_related_page_child) | \
      test(submit_button_does_not_cancel_a_newer_non_form_navigation_in_the_previous_target) | \
      test(child_module_producer_boundaries_require_exact_task_owner)' \
  --no-fail-fast --status-level fail --final-status-level fail --failure-output immediate
# run a3cc7c41-c243-46cc-82a2-f7df67d7eb76：3 passed；第三条锁住 stale owner 不得清除
# replacement Document 的 module/parser ledger，exact current owner 可以清除。

cargo nextest run -p lightmount-renderer-v8 \
  -E 'test(submitter_cancels_previous_same_form_navigation_in_a_related_page_child) | \
      test(submit_button_does_not_cancel_a_newer_non_form_navigation_in_the_previous_target)' \
  --stress-count 20 --flaky-result fail --test-threads 4 --no-fail-fast \
  --status-level fail --final-status-level fail --failure-output immediate
# run af92a077-2cf8-4ff5-815c-6bab74f2e9d7：20/20 iterations passed，每轮 2/2。

cargo nextest run --no-fail-fast --status-level fail --final-status-level fail
# run 91820a56-3831-4bdf-94a0-7b7bcc049827：16011 passed、3 failed、18 skipped。
# 三条 failure 都要求普通 supersession 的 stale child response 继续产生 historical Network terminal；首版
# 将主动 loader.cancel() 错误放进通用 child-load clear，边界过宽。修复保留通用 historical terminal，
# 只在 exact form-cancel binding 命中时关闭 transport。

cargo nextest run -p lightmount-renderer-v8 \
  -E 'test(stale_same_root_terminal_does_not_settle_newer_exact_navigation) | \
      test(response_for_replaced_child_document_is_historical_network_only) | \
      test(nested_stale_child_response_retains_producer_captured_parent_frame) | \
      test(submitter_cancels_previous_same_form_navigation_in_a_related_page_child) | \
      test(submit_button_does_not_cancel_a_newer_non_form_navigation_in_the_previous_target)' \
  --no-fail-fast --status-level fail --final-status-level fail --failure-output immediate
# run a70995ca-0f6d-4e53-9337-dad7870c0707：5 passed；同时锁住 historical Network 与 exact cancel。

cargo nextest run --no-fail-fast --status-level fail --final-status-level fail
# run fbfb803b-8a02-4621-9c3d-d45abc71b5ab：16014 passed、18 skipped；执行阶段 106.353s。

cargo fmt --all --check
# passed。

cargo clippy --workspace --all-targets -- -D warnings
# passed；1m 37s。
```

parser cancellation 复用 child Document owner 已有的 exact-ledger cleanup；它会清除 classic/module
scheduler、parser store、ready tasks 与 load-delay。当前证据包含该 owner 边界的既有
`child_module_producer_boundaries_require_exact_task_owner` 回归，但本轮 HTTP probe 直接锁住的是 main-resource
loader transport，不把它夸大成“阻塞 external script socket 一定同步关闭”。完整 Blink
`CancelClientNavigation()`（包括任意来源在新 target 上已有的 client navigation）、self-target
`CancelParsing()`、RemoteFrame scheduler endpoint 仍需后续纵切。

#### Phase 5E2H：child-source current-top causal/referrer carrier

E2G 解决了 target child 的 scheduler cancellation，但 child form 命中 `_top` 时，request 到达 target
Page 后仍会重新读取 target root 的 lifecycle/referrer。method/body 因 E2F carrier 得以保留，initiator
却发生了替换：source child 的 URL、Referrer Policy 与 exact Window/Document identity 没有跨过
同步 `Location` setter 和 renderer-to-protocol handoff。direct request 可能因此发送 root `Referer`；redirect、
cross-origin/downgrade 与 Fetch URL override 更会继续基于错误 source 或一条过早冻结的 header。

##### Chromium 合同与失败基线

本轮继续对照 Chromium `a03603fe9af6`：

- `FrameLoadRequest(LocalDOMWindow* origin_window, ...)` 在 request 构造时保存 `origin_window_`，并从该
  Window 的 `OutgoingReferrer()` / `GetReferrerPolicy()` 生成 request referrer；target Frame 并不替换它；
- `FormSubmission` 用 `Member<LocalDOMWindow> origin_window_` 跨过 target selection/scheduler，最终
  `Navigate()` 仍以这一个 origin Window 构造 `FrameLoadRequest`；
- `FrameLoader` 后续继续用 `GetOriginWindow()` 选择 requestor origin、fetch client settings、CSP 与
  navigation policy 输入。也就是说 source 是 causal/security input，target Frame/Page 才是 scheduler、
  loader 与 commit owner，两者不能合成一项“当前 root”；
- Chromium 的 `ResourceRequest` 保存 referrer URL/policy，由 network navigation 对实际 destination 处理；
  因而 Lightmount 也不能把 initial URL 的最终 `Referer` 字符串当作 redirect/DevTools URL override 的
  authoritative transport input。

实现前的两个 protocol 回归分别固定 direct + redirect/cross-origin 与 Fetch URL override 的违反路径：

```bash
cargo nextest run -p lightmount-protocol \
  -E 'test(child_form_top_navigation_keeps_source_referrer_across_redirect) | \
      test(child_form_top_navigation_recomputes_source_referrer_after_fetch_url_override)' \
  --no-fail-fast
# 初始 red：run 137178af-b7af-4ad1-9a1b-c83fc4d99261 中 direct request 使用 top `/source`；
# run 6990993a-6bb0-41c1-8e2e-29ffc68627af 中 Fetch pause 没有 child Referer。
# 首版只在 Location callback 内捕获 source 后，run b7a662d5-015a-4434-95da-bdde201f964b 仍为 0/2：
# callback 已进入 target realm，两条 request 都错误捕获 top `/source`。这证明 source 必须在 target
# Window setter 之前冻结，并通过同步 callback scope 显式传入。
```

##### typed source、target owner 与三阶段 referrer 投影

E2H 新增 `RendererTopLevelNavigationSource`，它与完整 method/body/header request 一起进入
`RendererTopLevelNavigationRequest`：

```text
source element / entered Window
  -> RendererTopLevelNavigationSource {
       root_document,
       window: RootFrame | ChildFrame { frame_id, local_window_id, document_id }
             | LightweightPopup { popup_id, popup_document_id },
       source_url,
       referrer_policy,
       suppress_referrer
     }
  -> target selection / synchronous target Location setter
  -> target Page pending top-level slot + exact Page handoff
  -> protocol NavigationDispatchState
       preflight event projection
       network transport policy input
       final Document commit seed
```

边界与不变量如下：

- source capture 发生在 hyperlink/form node owner 或 `window.open()` entered Window 上；initial inherited
  `about:` Document 的 outgoing source URL 读取其 `DocumentPolicyContainer.document_referrer`，不把
  `about:blank` 本身当作 HTTP referrer；
- target Page 继续唯一拥有 pending slot、handoff、navigation currentness、loader 与 response commit。
  carrier 只保存 causal facts，不把 source Page 变成第二个 scheduler；
- child `_top`/`_parent` 或 named-current-top 会同步进入另一个 Window 的 Location setter。caller 在该次
  V8 setter 周围安装可嵌套、立即恢复的 source scope；callback 生成的 target-root source 必须被这个显式
  source 覆盖。scope 不跨 task 保存，也不持有跨 re-entry 的 Rust mutable borrow；
- renderer publication 同时移动完整 request 和 typed source。`source_document` 继续提供 Page output 的
  causal root identity；`window` variant 再精确到 child LocalWindow/Document，不能用 URL 相同来冒充
  `RootFrame`；
- protocol preflight 为 `Fetch.requestPaused` / Network event 临时投影按 initial destination 计算的
  `Referer`，并标记 `SourcePolicyGenerated`。真正构建 libcurl request 时只移除这种 generated header，
  将 source URL、document policy 与 inference flag 交给共享 network `Request`；每个 actual URL/redirect
  hop 因而重新计算；
- `Fetch.continueRequest` 只改 URL 时保留 generated mode，新 URL 会重新计算。调用方显式提供 headers 时
  切为 `ExplicitOverride`：这些 headers 原样进入 transport，缺少 `Referer` 也不会被自动补回；
- `RendererMainDocumentCommitSeed` 保存同一 source。response final URL（transport error Document 则使用
  unreachable URL）确定后，再独立计算 `document.referrer`，不复用 HTTP header eligibility 或 preflight
  字符串；
- 普通 browser/CDP initiated navigation 没有 typed renderer source，继续走原有 target-session preflight，
  不因本轮改动改变 referrer owner。

##### 本纵切建立的回归与当前证据

```bash
cargo nextest run -p lightmount-protocol \
  -E 'test(child_form_top_navigation_keeps_source_referrer_across_redirect) | \
      test(child_form_top_navigation_recomputes_source_referrer_after_fetch_url_override)' \
  --no-fail-fast
# run e928ceb1-531b-4483-bcf0-ede5d8d75869：2 passed。
# 第一条同时断言同源 direct `/redirect`、跨 port redirect final 与最终 document.referrer 都是
# unsafe-url policy 下的完整 child URL；第二条断言 Fetch pause、URL-overridden transport 与 commit
# Document 都保留同一 child source。

cargo nextest run -p lightmount-renderer-v8 -p lightmount-protocol \
  -E 'test(child_form_top_target_carries_exact_child_window_document_source) | \
      test(child_window_open_top_carries_exact_source_and_noreferrer_policy) | \
      test(child_form_top_navigation_keeps_source_referrer_across_redirect) | \
      test(child_form_top_navigation_recomputes_source_referrer_after_fetch_url_override)' \
  --no-fail-fast --status-level fail --final-status-level fail --failure-output immediate
# run 111a9b5f-af30-4895-8859-66dce7558312：4 passed。
# renderer 两条不是 URL-only 断言：它们比较 exact frame id/local-window id/document id，并锁住
# window.open(..., `_top`, `noreferrer`) 的 suppression bit。

cargo nextest run -p lightmount-protocol \
  renderer_navigation_source_recomputes_default_policy_for_actual_destination \
  --no-fail-fast
# run e0753570-aed5-4b57-8e6e-62c0b9f2bcb1：1 passed；默认 policy 的 same-origin full URL、
# cross-origin origin-only、HTTPS→HTTP downgrade 清空，以及 noreferrer/explicit-header inference gate
# 均在 carrier 边界锁定。

cargo nextest run -p lightmount-fetch -p lightmount-renderer-v8 -p lightmount-protocol \
  -E '<E2H 五条核心回归 + form/popup/Fetch/auth/response-stream 邻接矩阵 13 条>' \
  --no-fail-fast --status-level fail --final-status-level fail --failure-output immediate
# run e78b5f4e-eefb-4635-8380-fddb15fcf3bf：18 passed。

cargo nextest run -p lightmount-renderer-v8 -p lightmount-protocol \
  -E '<E2H 五条核心回归>' \
  --stress-count 20 --flaky-result fail --test-threads 4 --no-fail-fast \
  --status-level fail --final-status-level fail --failure-output immediate
# run f673a21b-a2ed-4c37-8a12-dbb643260909：20/20 iterations passed，每轮 5/5。

cargo nextest run --no-fail-fast --status-level fail --final-status-level fail
# run 2cc1656c-c45d-4842-ae86-8df49c33acac：16021 passed、2 failed、18 skipped。
# 两条失败分别是 websocket parser-script Network/DCL backlog 与 file-chooser document-replacement
# shared-id 观察；均不在本轮改动路径，但由于涉及 lifecycle/currentness，继续按 flaky 规则复跑。

cargo nextest run -p lightmount -p lightmount-protocol \
  -E 'test(websocket_cdp_parser_script_network_backlog_flushes_before_domcontentloaded) | \
      test(file_chooser_opened_renderer_backend_node_id_is_scoped_to_document_replacement)' \
  --stress-count 20 --flaky-result fail --test-threads 4 --no-fail-fast \
  --status-level fail --final-status-level fail --failure-output immediate
# run 5f5aa263-5d9f-4a8c-9545-2e43862fb272：20/20 iterations passed，每轮 2/2。

cargo nextest run --no-fail-fast --status-level fail --final-status-level fail
# run 9051ae3c-80e1-4f03-a0f8-eab5112f2f37：16023 passed、18 skipped；执行阶段 102.298s。

cargo fmt --all --check
# passed。

cargo clippy --workspace --all-targets -- -D warnings
# passed；1m 34s。

git pull -r origin master
# Current branch popup-refactor is up to date；origin/master 没有基线漂移，HEAD 未重写。

cargo nextest run -p lightmount-renderer-v8 -p lightmount-protocol \
  -E '<E2H 五条核心回归>' \
  --no-fail-fast --status-level fail --final-status-level fail --failure-output immediate
# 同步后 run ccdb722e-7685-4e10-96a3-e381b9d19b0a：5 passed。
```

本轮没有把共享 fetch runtime 扩张成完整 navigation Fetch Standard 实现。redirect response 自身通过
`Referrer-Policy` 改写后续 hop、Fetch response-stage fulfill/continueResponse 与显式 `Referer` 的全部
Chromium 关系仍需独立 probe；source security origin/CSP、sandbox/top-navigation permission 和
`javascript:` target-realm execution 也不能从这个 URL/policy carrier 外推。当前 enum 中保留
`LightweightPopup` 只是迁移期兼容 source identity，不是 Phase 6 可以删除双栈的信号。

##### E1/E2A/E2B/E2C/E2D/E2E/E2F/E2G/E2H 有意保留的 Phase 5E 范围

E1/E2A/E2B/E2C/E2D/E2E/E2F/E2G/E2H 不是 creation/group policy 完成标志，以下边界不能套用“非命名直接 Fresh Page”或
“所有 name 都查 related same-agent registry”的捷径：

- E2A 已让 `window.open()` 的 existing related top-level target 先执行 renderer group lookup；E2B 已让
  新建 named noopener/noreferrer context 使用 private Fresh group，并只在该 group 内保留/查找 live name；
  E2C 已让 full-creator ordinary named hyperlink 复用两项 decision，E2D 已把 full-creator form 的
  effective target 与 exact request 接入同一 decision；E2E 已让 `window.open()` / hyperlink 的 child
  source、related nested frame、完整 local frame-tree collision order 和普通 origin/ancestor filter 进入
  renderer resolver；E2F 已让 ordinary named form 的 exact request 消费同一 typed result，并由命中的
  current/related child owner 执行；E2G 已让同一 source form 跨 Page 保存 stable target route 与 exact
  scheduler generation，并由目标 child owner 取消 task/load/parser work；E2H 已让 current-top
  `window.open()`、hyperlink/form 保存 source Window/Document 与 referrer policy，同时保持 target Page
  的唯一 scheduler/loader authority；
- `_self` / `_parent` / `_top` 继续命中 existing context，并按 Chromium compatibility 行为返回 Window；
- `javascript:` URL 仍涉及同步 target realm/CSP 语义，本纵切保留 legacy path；
- form named/`_blank` 的 target/request carrier、E2E resolver integration、local/related-child repeated
  submission cancellation 与 child-source current-top causal/referrer identity 已完成；完整 target
  `CancelClientNavigation()`、sandbox forms gate 与 RemoteFrame 尚未完成；
- E2E 的 `CanNavigate` 只实现 local nested candidate 的 self / `javascript:` exact-origin / 普通
  target-or-ancestor origin 分支；sandbox navigation flags、top-level opener relation、sticky/transient user
  activation、top-navigation destination exception、fenced tree 与 embedder fallback 尚未实现；
- 跨 Page 精确继承的 opaque origin 目前仍只有 V8 security token 能区分；Rust
  `WindowAccessOrigin` 为避免 host-local owner id 碰撞会拒绝 related-host opaque equality，后续需要独立、
  group-safe 的 opaque origin nonce，不能把本轮 tuple-origin 修正外推到该路径；
- popup blocker/transient activation、sandbox `allow-popups` / escape-sandbox、COOP group switch、remote 或
  disconnected WindowProxy endpoint 仍没有统一 creation transaction；
- E2H 已覆盖同源 direct、跨 origin redirect hop、Fetch request-stage URL override、默认 downgrade policy
  与最终 `document.referrer`；redirect response policy mutation、Fetch response-stage override 和
  explicit header/document-referrer 的完整 Chromium 矩阵仍需单独 WPT/最小探针。

##### 已知差距与后续顺序

Phase 5C 已把 related top-level 的动态 child/opener 投影接回 owner，但还不是整份
`cross-origin-objects.html` 全通过。明确未完成项如下：

| 未完成项 | 当前事实 | 下一责任方 |
| --- | --- | --- |
| close policy / unload | accepted close transaction、动态 `closed`、task/fetch cancellation、target teardown 和 closed facade 已统一；script-closable、beforeunload/unload/ACK 尚未实现 | popup creation/group policy + 通用 Page unload lifecycle owner |
| focus / blur | descriptor 和调用许可已存在，没有 Page focus authority/事件事务 | browser-context active/focus owner |
| retained detached Document values | Document host retirement 后旧 function/DOM wrapper 当前安全抛 `TypeError`；Chromium 中被 JS 强引用的 detached Node/realm 仍可继续存活和读取 | 为 DocumentRuntime、realm 和 wrappers 建立 GC/owner 协同 lifetime，避免用 raw host pointer 决定对象寿命 |
| policy/group sever | E1 已统一新建非命名 noopener/noreferrer；E2A/E2B 已统一 `window.open()` 的 related name authority 与 Fresh group/name handoff；E2C/E2D 已复用到 full-creator ordinary named hyperlink/form；E2E 已统一 child-source 与 related nested local frame-tree lookup；E2F 已让 ordinary named form exact request 消费该 typed owner；E2G 已补 local/related child 的 same-form typed scheduler cancellation；E2H 已补 child-source current-top typed initiator/referrer carrier。完整 sandbox/activation/COOP 和 remote/disconnected endpoint 仍未统一 | browsing-context group / popup policy owner + top-level navigation carrier owner |

下一批按以下顺序推进，避免把动态状态继续塞进静态 surface：

1. **Phase 5B：close transaction（本纵切已完成 accepted-close 闭环）。** 已建立唯一
   browsing-context liveness authority；`window.close()`、`Target.closeTarget`、opener-side `.closed`、
   task cancellation、targetDestroyed 和 Page teardown 使用同一 typed transaction，并覆盖重复 close、
   target currentness 与早期 admission。script-closable/unload policy 按上节边界继续补齐。
2. **Phase 5C：live relation/child projection（本纵切已完成）。** Page-scoped opener edge 与 top-level
   child/name registry 已取代静态 surface snapshot，覆盖动态 frames、named child、`then` / `open`
   shadow、opener setter/discard sever 与 navigation persistence；COOP/remote group sever 留在 Phase 5E。
3. **Phase 5D：WPT internal methods/per-incumbent membrane（D1-D3b 已完成）。** D1 已完成 Location、D2
   已完成 Window 的 exact ownKeys、unknown/index、mutation、prototype/preventExtensions 静态矩阵，
   D2.5 已把 related/generic nested child 接回通用 live registry owner并修复预物化 restricted facade；
   D3a 已完成 Function/accessor 的 accessing-Realm prototype、identity、cache、异常 realm 与
   receiver-owned target dispatch；D3b 已完成 stable top identity cutover、callback-scoped observer/target
   child projection，以及 same-host / related-Page 两条访问矩阵。
4. **Phase 5E：creation/group policy（E1、E2A、E2B、E2C、E2D、E2E、E2F、E2G 与 E2H 已完成）。** E1 已覆盖
   `window.open()` 非命名 noopener/noreferrer 与 hyperlink `_blank` implicit/explicit noopener 的
   single-owner/referrer commit；E2A 已覆盖 related top-level named `window.open()` 的真实 initial Page、
   live name/lifecycle registry 与 exact target reuse；E2B 已覆盖新建 named suppress-opener 的 Fresh
   Page/private group、首 realm name 与 self-only lookup；E2C 已把 ordinary named hyperlink 的 existing
   lookup、Related/Fresh creation 与 exact target handoff 收进同一 authority；E2D 又把 form effective
   target、named/`_blank`、POST body/Content-Type/referrer 与 target-realm NavigateEvent 收进同一 request
   carrier；E2E 又把 `window.open()` / hyperlink 的 source subtree、current Page、ordered related Pages
   完整 local frame-tree lookup、related-child target owner 与普通 nested `CanNavigate` 收进同一 resolver；
   E2F 再让 ordinary named form 的 exact request、target realm event 与 local child scheduler 消费该 typed
   result，并复用 child stable WindowProxy/policy container；E2G 又让 source form 保存 stable Page/child route
   与 exact navigation-load binding，跨 Page 取消由目标 owner 撤销 task、loader/parser ledger 且不会误杀
   replacement navigation；E2H 再把 current-top 的 source Window/Document、URL/policy 与 suppression 保留到
   redirect/Fetch URL override/commit，而不改变 target Page 的唯一 scheduler/loader ownership。下一步处理
   popup blocker/transient activation、完整 sandbox/top-level `CanNavigate` 与 `javascript:` target-realm
   execution，再处理 focus/detach、COOP group switch 与 remote/disconnected WindowProxy endpoint。

Phase 5A 聚焦验证：

```bash
cargo nextest run -p lightmount-renderer-v8 \
  -E 'test(related_page_script_agent_exposes_chromium_cross_origin_window_proxy_surface) | test(same_origin_child_window_migration_to_cross_origin_installs_denied_surface) | test(data_url_child_document_is_cross_origin_to_parent) | test(child_window_proxy_identity_survives_cross_origin_round_trip)' \
  --no-fail-fast --status-level fail --final-status-level fail
# 4 passed

cargo nextest run -p lightmount-protocol \
  -E 'test(popup_transport_failure_commits_error_document_in_stable_auxiliary_page)' \
  --no-fail-fast --status-level fail --final-status-level fail
# 1 passed；包含 error commit、Window allowlist、postMessage、location assign/replace

cargo nextest run \
  -E 'test(child_document_creation_freezes_document_start_script_registry) | test(add_script_run_immediately_creates_top_level_world_even_when_child_world_name_matches)' \
  --no-fail-fast --status-level fail --final-status-level fail
# 2 passed；另在 concurrent core+renderer 负载下重复 protocol case 100 次通过
```

Phase 5B 聚焦验证：

```bash
cargo nextest run -p lightmount-renderer-v8 \
  -E 'test(canceling_prepared_live_page_replacement_preserves_page_environment_and_output_stream) or test(related_page_script_agent_exposes_chromium_cross_origin_window_proxy_surface) or test(related_page_script_agent_transfers_stable_window_proxy_objects_and_dom_wrappers) or test(related_page_window_close_is_synchronous_idempotent_and_disconnects_final_realm) or test(child_navigation_retires_runtime_binding_context_and_stale_function) or test(child_navigation_retires_local_window_owned_xhr) or test(child_navigation_aborts_fetch_and_detaches_keepalive)' \
  --no-fail-fast
# 7 passed

cargo nextest run -p lightmount-protocol \
  -E 'test(popup_window_close_retires_target_and_parks_stable_window_proxy) or test(target_close_parks_the_same_stable_popup_window_proxy) or test(popup_transport_failure_commits_error_document_in_stable_auxiliary_page)' \
  --no-fail-fast
# 3 passed

cargo nextest run -p lightmount-protocol \
  -E 'test(stale_window_close_termination_cannot_retire_current_page_residence)' \
  --no-fail-fast
# 1 passed；最终 termination continuation 会再次拒绝 stale Page generation
```

Phase 5C 聚焦验证：

```bash
cargo nextest run -p lightmount-renderer-v8 \
  -E 'test(related_page_script_agent_experiment_shares_isolate_and_survives_source_close) or test(related_page_script_agent_exposes_chromium_cross_origin_window_proxy_surface) or test(related_page_window_close_is_synchronous_idempotent_and_disconnects_final_realm) or test(related_page_script_agent_transfers_stable_window_proxy_objects_and_dom_wrappers)' \
  --no-fail-fast
# 4 passed；覆盖 live index/name/ownKeys、then/open shadow、显式 sever、opener discard、
# closed-popup opener retention 和两种 sever 的 navigation persistence
```

Phase 5D1 聚焦验证：

```bash
cargo nextest run -p lightmount-renderer-v8 \
  -E 'test(related_page_script_agent_exposes_chromium_cross_origin_window_proxy_surface) or test(data_url_child_document_is_cross_origin_to_parent) or test(same_origin_child_window_migration_to_cross_origin_installs_denied_surface) or test(child_window_proxy_identity_survives_cross_origin_round_trip)' \
  --no-fail-fast
# 4 passed；覆盖 related/detached top-level Location、generic child Location、
# origin migration 和 stable WindowProxy navigation round-trip

cargo clippy -p lightmount-renderer-v8 --all-targets -- -D warnings
# passed

cargo nextest run -p lightmount-core \
  -E 'test(cross_origin_window_proxy_exposes_standard_noop_shape)' \
  --no-fail-fast
# 1 passed；同步淘汰 core 集成层中 denied Location descriptor/has 的旧预期
```

Phase 5D2 聚焦验证：

```bash
cargo nextest run \
  -E 'test(related_page_script_agent_exposes_chromium_cross_origin_window_proxy_surface) | test(data_url_child_document_is_cross_origin_to_parent) | test(cross_origin_window_proxy_exposes_standard_noop_shape) | test(cross_origin_window_proxy_exposes_named_child_frames)' \
  --no-fail-fast --status-level fail --final-status-level fail
# 4 passed；覆盖 related/generic/detached Window internal methods、ordinary intrinsic
# delegation、exact ownKeys、document/focus named collision 和 navigation 后 stale name 拒绝

cargo nextest run \
  -E 'test(cross_origin_window_proxy_exposes_standard_noop_shape) | test(cross_origin_top_window_proxy_length_tracks_top_child_lifecycle) | test(cross_origin_window_proxy_exposes_named_child_frames) | test(child_cross_origin_window_denials_use_the_child_dom_exception_realm) | test(same_origin_child_window_migration_to_cross_origin_installs_denied_surface) | test(child_window_proxy_identity_survives_cross_origin_round_trip) | test(child_browsing_context_cross_origin_post_message_reply_preserves_source_identity) | test(captured_cross_origin_content_window_matches_message_source_after_child_navigation) | test(captured_cross_origin_content_window_keeps_safe_surface_during_realm_gap) | test(data_url_child_document_is_cross_origin_to_parent) | test(related_page_script_agent_exposes_chromium_cross_origin_window_proxy_surface)' \
  --no-fail-fast --status-level fail --final-status-level fail
# 首次 10/11 passed；唯一失败是 document named-child collision 仍期待旧 denied accessor。
# 按 Chromium precedence 更新该回归后单独复跑通过；上述最终 4-case owner matrix 随后全通过。

cargo check -p lightmount-renderer-v8
# passed
```

Phase 5D2.5 聚焦验证：

```bash
cargo nextest run -p lightmount-core --test history_child \
  -E 'test(cross_origin_window_proxy_exposes_named_child_frames)' \
  --no-fail-fast --status-level fail --final-status-level fail
# 新 mutation matrix 在旧实现上稳定失败：child 已回复完成 rename/remove/append，parent 读取
# renamedNested descriptor 得到 SecurityError；接入通用 owner 后 1 passed。

cargo nextest run \
  -E 'test(data_url_child_document_is_cross_origin_to_parent) | test(cross_origin_window_proxy_exposes_named_child_frames)' \
  --no-fail-fast --status-level fail --final-status-level fail
# 2 passed；同时证明预物化 child shell 仍是 restricted facade、不会泄漏 raw global。

cargo nextest run \
  -E 'test(cross_origin_window_proxy_exposes_standard_noop_shape) | test(cross_origin_top_window_proxy_length_tracks_top_child_lifecycle) | test(cross_origin_window_proxy_exposes_named_child_frames) | test(child_cross_origin_window_denials_use_the_child_dom_exception_realm) | test(same_origin_child_window_migration_to_cross_origin_installs_denied_surface) | test(child_window_proxy_identity_survives_cross_origin_round_trip) | test(child_browsing_context_cross_origin_post_message_reply_preserves_source_identity) | test(captured_cross_origin_content_window_matches_message_source_after_child_navigation) | test(captured_cross_origin_content_window_keeps_safe_surface_during_realm_gap) | test(data_url_child_document_is_cross_origin_to_parent) | test(related_page_script_agent_exposes_chromium_cross_origin_window_proxy_surface)' \
  --no-fail-fast --status-level fail --final-status-level fail
# 11 passed；覆盖 related/generic live owner、pre-materialized shell、realm-gap snapshot、
# navigation round-trip、source identity、Location/Window internal-method matrix。

cargo check -p lightmount-renderer-v8
# passed
```

Phase 5D3a 聚焦验证：

```bash
cargo nextest run \
  -E 'test(data_url_child_document_is_cross_origin_to_parent) | test(cross_origin_property_wrappers_are_cached_per_accessing_realm)' \
  --no-fail-fast --status-level fail --final-status-level fail
# 2 passed；覆盖两个 accessing Realm 的完整 wrapper 矩阵，以及 Location cached
# href/replace wrapper 的 receiver brand / WebIDL 边界。

cargo nextest run \
  -E 'test(cross_origin_property_wrappers_are_cached_per_accessing_realm) | test(cross_origin_window_proxy_exposes_standard_noop_shape) | test(cross_origin_top_window_proxy_length_tracks_top_child_lifecycle) | test(cross_origin_window_proxy_exposes_named_child_frames) | test(child_cross_origin_window_denials_use_the_child_dom_exception_realm) | test(same_origin_child_window_migration_to_cross_origin_installs_denied_surface) | test(child_window_proxy_identity_survives_cross_origin_round_trip) | test(child_browsing_context_cross_origin_post_message_reply_preserves_source_identity) | test(captured_cross_origin_content_window_matches_message_source_after_child_navigation) | test(captured_cross_origin_content_window_keeps_safe_surface_during_realm_gap) | test(data_url_child_document_is_cross_origin_to_parent) | test(related_page_script_agent_exposes_chromium_cross_origin_window_proxy_surface) | test(popup_transport_failure_commits_error_document_in_stable_auxiliary_page) | test(cross_origin_location_proxy_only_allows_href_and_replace_navigation)' \
  --no-fail-fast --status-level fail --final-status-level fail
# 首次 13/14 passed：唯一失败把 href setter 的 null receiver 当成访问方 global；
# 在 WebIDL conversion 前补 Location brand，并新增 replace null-receiver probe 后，最终 14 passed。

cargo clippy -p lightmount-renderer-v8 --all-targets -- -D warnings
# passed
```

Phase 5D3b 聚焦验证：

```bash
cargo nextest run -p lightmount-core --test history_child \
  -E 'test(cross_origin_child_endpoint_projection_is_relative_to_the_observer)' \
  --no-fail-fast --status-level fail --final-status-level fail
# 旧实现稳定失败：B 经 parent.frames[1] 得到的 C 仍按 A/C origin 保持 restricted，
# 首次 marker write 抛 SecurityError；stable A-side identity 与 A-side denial 同时成立。
# stable top WindowProxy + observer/target projection 接入后 1 passed；随后加入 named lookup
# 与 named/indexed descriptor identity 后仍为 1 passed。

cargo nextest run -p lightmount-renderer-v8 \
  -E 'test(related_page_script_agent_exposes_chromium_cross_origin_window_proxy_surface)' \
  --no-fail-fast --status-level fail --final-status-level fail
# 1 passed；跨 host related opener 与 popup parent 分别对同一个 child 得到 allowed/denied，
# 并验证 child parent/top 指向 popup stable WindowProxy。

cargo nextest run \
  -E 'test(cross_origin_child_endpoint_projection_is_relative_to_the_observer) | test(cross_origin_property_wrappers_are_cached_per_accessing_realm) | test(cross_origin_window_proxy_exposes_standard_noop_shape) | test(cross_origin_top_window_proxy_length_tracks_top_child_lifecycle) | test(cross_origin_window_proxy_exposes_named_child_frames) | test(child_cross_origin_window_denials_use_the_child_dom_exception_realm) | test(same_origin_child_window_migration_to_cross_origin_installs_denied_surface) | test(child_window_proxy_identity_survives_cross_origin_round_trip) | test(child_browsing_context_cross_origin_post_message_reply_preserves_source_identity) | test(captured_cross_origin_content_window_matches_message_source_after_child_navigation) | test(captured_cross_origin_content_window_keeps_safe_surface_during_realm_gap) | test(data_url_child_document_is_cross_origin_to_parent) | test(related_page_script_agent_exposes_chromium_cross_origin_window_proxy_surface) | test(popup_transport_failure_commits_error_document_in_stable_auxiliary_page) | test(cross_origin_location_proxy_only_allows_href_and_replace_navigation)' \
  --no-fail-fast --status-level fail --final-status-level fail
# 15 passed；覆盖 D3a membrane、D3b same-host/cross-host endpoint、live registry、
# navigation/realm gap、source identity、popup error Document 与 Location/Window internal methods。

cargo check -p lightmount-renderer-v8
# passed
```

Phase 5E1 聚焦验证：

```bash
cargo nextest run -p lightmount-renderer-v8 \
  -E 'test(per_page_isolate_policy_keeps_window_open_routes_page_owned) | test(noreferrer_implies_noopener_and_last_value_wins) | test(hyperlink_target_blank_reloads_rel_opener_policy_for_each_activation) | test(window_open_noopener_lightweight_popup_uses_fresh_session_storage)' \
  --no-fail-fast --status-level fail --final-status-level fail
# 4 passed；覆盖 production activation 不再携带 popup_id、Fresh agent、feature precedence、
# hyperlink 动态 rel policy 和 standalone fresh session-storage fallback。

cargo nextest run -p lightmount-protocol \
  -E 'test(noopener_and_noreferrer_popups_have_one_real_navigation_with_creator_referrer_policy) | test(anchor_blank_target_uses_implicit_noopener) | test(popup_initial_about_blank_adopts_renderer_page_and_related_script_agent)' \
  --no-fail-fast --status-level fail --final-status-level fail
# 3 passed；覆盖 noopener/noreferrer/implicit-noopener 的 single request、network Referer、
# initial/destination document.referrer、精确 about:blank 与 fragment same-document、null opener、
# target attach，以及保留 opener 路径不回归；主矩阵内包含 6 条 activation case。

cargo nextest run -p lightmount-fetch \
  navigation_referrer_is_distinct_from_http_header_eligibility \
  --no-fail-fast --status-level fail --final-status-level fail
# 1 passed；证明非 HTTP destination 的 Document referrer 与 HTTP Referer eligibility 分离。

cargo nextest run -p lightmount-protocol \
  local_storage_mutations_fan_out_across_targets_without_leaking_session_storage \
  --no-fail-fast --status-level fail --final-status-level fail
# 默认 nextest 栈首次全量稳定 SIGABRT 后，detached HEAD 基线通过；heap-owned commit
# environment 修复后首次 + 连续 10 次聚焦复跑均通过。

cargo check -p lightmount-fetch -p lightmount-renderer-v8 -p lightmount-protocol --tests
# passed
```

Phase 5E2A 聚焦验证：

```bash
cargo nextest run -p lightmount-protocol \
  window_open_named_target_reuse_is_owned_by_the_renderer_page_group \
  --no-fail-fast --status-level fail --final-status-level fail
# 红灯：target 初次观察为 `undefined||false`，而 creator 已观察到
# `reportWindow|renderer-page`；接入 exact real Page/group 后 1 passed。
# 回归包含动态 window.name、主动清空 protocol target_window_names、noopener
# exact reuse、无新 Target.targetCreated，以及原 opener edge 保留。

cargo nextest run -p lightmount-protocol window_open_named_target \
  --no-fail-fast --status-level fail --final-status-level fail
# 4 passed；覆盖既有 named target、same-command reuse、renderer group authority
# 与旧 catchall/target projection 兼容面。

cargo nextest run -p lightmount-renderer-v8 \
  per_page_isolate_policy_keeps_window_open_routes_page_owned \
  --no-fail-fast --status-level fail --final-status-level fail
# 1 passed；首次 named activation 携带 RelatedAuxiliaryPage reservation，普通和
# noopener reuse 均携带同一 RendererResolvedPopupTarget，且不预留第二个 Page。

cargo nextest run -p lightmount-renderer-v8 \
  window_open_noopener_navigates_existing_named_iframe_and_returns_null \
  --no-fail-fast --status-level fail --final-status-level fail
# 1 passed；existing iframe 仍导航、返回 null，且不产生 popup activation。

cargo nextest run -p lightmount-protocol \
  protocol_name_projection_cannot_redirect_popup_to_unrelated_background_owner \
  --no-fail-fast --status-level fail --final-status-level fail
# 1 passed；手工写入的 protocol name projection 无权把 renderer 新建决定重定向到
# unrelated background Page，且不会 promote/导航该旧 target。

cargo check -p lightmount-renderer-v8 -p lightmount-protocol
# passed
```

Phase 5E2B 聚焦验证：

```bash
cargo nextest run -p lightmount-renderer-v8 \
  per_page_isolate_policy_keeps_window_open_routes_page_owned \
  --no-fail-fast --status-level fail --final-status-level fail
# 接入前稳定失败：named suppress-opener activation 的 popup_id 为 Some(2)，预期 None。
# 接入后 1 passed；同时覆盖 FreshUnnamed/FreshNamed/Related typed disposition、
# noopener+noreferrer 各自的 Fresh admission、相同 name 的两次 reservation 使用不同 Page id。

cargo nextest run -p lightmount-protocol \
  named_suppress_opener_window_open_creates_distinct_fresh_groups_with_live_names \
  --no-fail-fast --status-level fail --final-status-level fail
# 接入前稳定失败：browser-context target_window_names 暴露后一个 Fresh target（Some("TID-2")）。
# 接入后 1 passed；覆盖两个 Target.targetCreated、无全局 name projection、两个真实 realm 的
# requested window.name/null opener、每个 private group 的 self-only exact reuse 与另一 target 不被导航。

cargo check -p lightmount-protocol
# passed
```

E2A 当时用于保护尚未迁移 hyperlink lightweight terminal 的 owner-scheduler characterization，已在
E2C 完成后删除；它要求 opener-local mirrored loader 发起请求，不再是合法的 green 条件。

Phase 5E2C 聚焦验证：

```bash
cargo nextest run -p lightmount-renderer-v8 \
  per_page_isolate_policy_keeps_window_open_routes_page_owned \
  --no-fail-fast --status-level fail --final-status-level fail
# 接入前稳定失败：named opener hyperlink 的 new_target_disposition 为 None，预期 Some(Related)。
# 接入后 1 passed；同时覆盖 existing target 的 exact renderer residence、noreferrer 不暴露/重写 opener，
# 以及两次同名 suppress-opener hyperlink 得到两个 FreshNamed Page reservation。

cargo nextest run -p lightmount-protocol \
  -E 'test(named_opener_hyperlink_creation_and_reuse_are_owned_by_renderer_group) | test(named_suppress_opener_hyperlinks_create_distinct_fresh_groups_with_live_names)' \
  --no-fail-fast --status-level fail --final-status-level fail
# 接入前 2 failed：Related target realm 为 `|false|#related-two`，且两个同名 suppress-opener link
# 只产生一个 Target.targetCreated；接入后 2 passed。回归覆盖清空 protocol name projection 后 exact reuse、
# existing opener edge 保留、Fresh target 不发布全局 name，以及两个真实 realm 的 name/null opener。

cargo nextest run -p lightmount-protocol \
  -E 'test(window_open_named_target_reuses_existing_popup_target) | test(window_open_named_target_reuse_is_owned_by_the_renderer_page_group) | test(named_suppress_opener_window_open_creates_distinct_fresh_groups_with_live_names) | test(window_open_named_target_reused_in_same_command_emits_one_page_event) | test(anchor_blank_target_uses_implicit_noopener) | test(anchor_blank_target_with_rel_opener_preserves_exact_opener) | test(protocol_name_projection_cannot_redirect_popup_to_unrelated_background_owner)' \
  --no-fail-fast --status-level fail --final-status-level fail
# 7 passed；覆盖相邻 window.open name authority、E2B Fresh split、hyperlink `_blank` 两种 opener policy
# 与 unrelated protocol projection 不回归。

cargo nextest run -p lightmount-renderer-v8 \
  -E 'test(hyperlink_target_blank_reloads_rel_opener_policy_for_each_activation) | test(window_open_noopener_navigates_existing_named_iframe_and_returns_null)' \
  --no-fail-fast --status-level fail --final-status-level fail
# 2 passed；覆盖动态 rel policy 与 existing named iframe 优先级。
```

Phase 5A 提交门禁结果：

```bash
cargo nextest run --no-fail-fast --status-level fail --final-status-level fail
# 15891 passed, 18 skipped

cargo fmt --all --check
# passed

cargo clippy --workspace --all-targets -- -D warnings
# passed
```

Phase 5B 提交门禁结果：

```bash
cargo nextest run --no-fail-fast --status-level fail --final-status-level fail
# 15895 passed, 18 skipped

cargo fmt --all --check
# passed

cargo clippy --workspace --all-targets -- -D warnings
# passed
```

Phase 5C 提交门禁结果：

```bash
cargo nextest run --no-fail-fast
# 15895 passed, 18 skipped

cargo fmt --all --check
# passed

cargo clippy --workspace --all-targets -- -D warnings
# passed
```

Phase 5D1 提交门禁结果：

```bash
cargo nextest run --no-fail-fast
# 首次：15892 passed, 3 failed, 18 skipped。
# 其中 core cross_origin_window_proxy_exposes_standard_noop_shape 是本纵切应淘汰的旧
# denied Location descriptor/has 预期，更新后聚焦通过；另两个 websocket/parser backlog
# case 与本纵切路径不相交，在首次高并发 workspace 运行中失败。

for run in {1..5}; do
  cargo nextest run -p lightmount \
    -E 'test(websocket_cdp_parser_script_network_backlog_flushes_before_domcontentloaded) or test(websocket_cdp_runtime_evaluate_uses_committed_page_while_parser_blocking_source_is_pending)' \
    --no-fail-fast || exit 1
done
# 5 rounds passed；每轮 2/2

cargo nextest run --no-fail-fast
# 最终：15895 passed, 18 skipped

cargo fmt --all --check
# passed

cargo clippy --workspace --all-targets -- -D warnings
# passed
```

Phase 5D2 提交门禁结果：

```bash
cargo nextest run --no-fail-fast
# 首次：15894 passed, 1 failed, 18 skipped。唯一失败是
# webidl_callback_source_boundary_tests::direct_v8_call_inventory_is_frozen：D2 新增的原始
# Object/Reflect intrinsic delegate 尚未登记 source-level inventory。

cargo nextest run \
  -E 'test(direct_v8_call_inventory_is_frozen) | test(related_page_script_agent_exposes_chromium_cross_origin_window_proxy_surface) | test(data_url_child_document_is_cross_origin_to_parent) | test(cross_origin_window_proxy_exposes_standard_noop_shape) | test(cross_origin_window_proxy_exposes_named_child_frames)' \
  --no-fail-fast --status-level fail --final-status-level fail
# 将该调用按 captured native intrinsic 分类为 NativeForwardingOrScript 后，5 passed。

cargo nextest run --no-fail-fast --status-level fail --final-status-level fail
# 最终：15895 passed, 18 skipped

cargo fmt --all --check
# passed

cargo clippy --workspace --all-targets -- -D warnings
# passed
```

Phase 5D2.5 提交门禁结果：

```bash
cargo nextest run --no-fail-fast --status-level fail --final-status-level fail
# 15895 passed, 18 skipped

cargo fmt --all --check
# passed

cargo clippy --workspace --all-targets -- -D warnings
# passed
```

Phase 5D3a 提交门禁结果：

```bash
cargo nextest run --no-fail-fast --status-level fail --final-status-level fail
# 15896 passed, 18 skipped

cargo fmt --all --check
# passed

cargo clippy --workspace --all-targets -- -D warnings
# passed
```

Phase 5D3b 提交门禁结果：

```bash
cargo nextest run --no-fail-fast
# rebase 到当时 origin/master 后最终为 15904 passed, 18 skipped

cargo fmt --all --check
# passed

cargo clippy --workspace --all-targets -- -D warnings
# passed
```

Phase 5E1 提交门禁结果：

```bash
cargo nextest run --no-fail-fast --status-level fail --final-status-level fail
# rebase 前：15906 passed, 18 skipped。
# rebase 到 origin/master 后，前两次 workspace 高并发运行各出现一个互不相同的既有
# timing case：parser-script network backlog 一次、sandboxed blob/OPFS message 一次；
# 其余均为 15961 passed。两个失败分别连续 10 次聚焦复跑通过。

cargo nextest run --no-fail-fast --status-level fail --final-status-level fail
# rebase 后最终：15962 passed, 18 skipped

cargo fmt --all --check
# passed

cargo clippy --workspace --all-targets -- -D warnings
# passed
```

Phase 5E2A 提交门禁结果：

```bash
cargo nextest run --no-fail-fast
# 首次：15962 passed, 2 failed, 18 skipped。一个失败是 protocol-only name map
# 仍被旧测试当作 renderer lookup authority；另一个旧 owner test 等待已迁移 named
# window.open 的 mirrored loader，380.779s 无进展后手动中断。两条均按上文责任边界
# 改写并分别聚焦通过。

cargo nextest run --no-fail-fast
# 接入提交 rebase 前：15964 passed, 18 skipped；99.385s。

cargo fmt --all --check
# passed

cargo clippy --workspace --all-targets -- -D warnings
# passed

# `git pull -r origin master` 把 34 个 popup 提交重放到 ef44056fe9 后再次执行：
cargo nextest run --no-fail-fast
# 15992 passed, 18 skipped；100.406s。

cargo fmt --all --check
# passed

cargo clippy --workspace --all-targets -- -D warnings
# passed
```

Phase 5E2B 提交门禁结果：

```bash
cargo nextest run --no-fail-fast
# 最终 typed API 收口后重跑：15993 passed, 18 skipped；100.737s。

cargo fmt --all --check
# passed

cargo clippy --workspace --all-targets -- -D warnings
# passed
```

Phase 5E2C 提交门禁结果：

```bash
cargo nextest run --no-fail-fast
# 15994 passed, 18 skipped；执行阶段 100.077s。

cargo fmt --all --check
# passed

cargo clippy --workspace --all-targets -- -D warnings
# passed；1m 29s。

# `git pull -r origin master` 把 36 个分支提交从 ef44056fe9 重放到 cac2e67294 后再次执行：
cargo nextest run --no-fail-fast
# 15994 passed, 18 skipped；执行阶段 100.649s。

cargo fmt --all --check
# passed

cargo clippy --workspace --all-targets -- -D warnings
# passed；1m 30s。
```

Phase 5E2D 提交门禁结果：

```bash
cargo nextest run -p lightmount-core \
  -E 'test(wpt_compat_case_form_submitter_target_fallback_basic)' \
  --no-fail-fast --status-level fail --final-status-level fail
# 1 passed。首次全量门禁暴露本地 port 仍把显式空 formtarget 当作 missing；fixture 按当前
# Chromium owner 修正为“显式空值 -> 提交时冻结的 base target，缺失属性 -> form target”后聚焦通过。

cargo nextest run --no-fail-fast
# 最终重跑：15996 passed, 18 skipped；执行阶段 99.384s。

cargo fmt --all --check
# passed

cargo clippy --workspace --all-targets -- -D warnings
# passed；最终 Rust 改动下 1m 29s。

git diff --check
# passed
```

E2D rebase 后集成门禁：

```bash
git pull -r origin master
# 无文本冲突；把 37 个 popup 分支提交从 cac2e67294 重放到 b016375769，E2D 提交变为
# d209eb3430。

cargo nextest run -p lightmount -p lightmount-protocol \
  -E 'test(websocket_cdp_parser_script_network_backlog_flushes_before_domcontentloaded) | \
      test(parser_tail_dom_mutations_precede_the_dcl_binding_refresh)' \
  --stress-count 100 --flaky-result fail --test-threads 8 --no-fail-fast
# 100/100 iterations；每轮 2/2 passed；22.816s。

cargo nextest run --no-fail-fast --status-level fail --final-status-level fail
# run id a8546408-dd85-4300-8578-8ec4e4c21ee4；16000 passed, 18 skipped；98.000s。

cargo fmt --all --check
# passed

cargo clippy --workspace --all-targets -- -D warnings
# passed；1m 36s。

git diff --check
# passed
```

这次 rebase 没有文本冲突，但 master 新增的 stable Page navigation 暴露了 Document/stream
identity 边界：Page-scoped output stream 会跨 replacement 保持 identity，DOM mutation batch
却必须属于 producer Document。纯 master 聚焦用例通过，逐提交二分首次落在
`67ca127c1b`；修复后每个 DOM batch 自带 exact Document agent token，protocol 只绑定匹配的
current attachment。全量并发随后又暴露两处测试采样歧义：held-parser 请求发出不等于 parser 已
到达该 live-tree 位置，初始 `about:blank` DCL 也不能代表目标 URL generation。fixture 现分别用
已执行的 inline `document.write()` 和目标 URL `Page.frameNavigated` 建立确定边界，没有增加
sleep、retry 或放宽事件顺序断言。

Phase 5E2E 提交门禁结果：

```bash
cargo nextest run --no-fail-fast --status-level fail --final-status-level fail
# run ffc9f453-4050-420b-b9bb-ccc2d33121bf：16002 passed, 18 skipped；执行阶段 99.771s。

cargo fmt --all --check
# passed

cargo clippy --workspace --all-targets -- -D warnings
# passed；1m 31s。

git diff --check
# passed
```

E2E rebase 后集成门禁：

```bash
git pull -r origin master
# origin/master 从 b016375769 前进到 744e161dad；39 个 popup 分支提交完成重放。
# 旧 continuation-fence 提交与新 master 在 lightmount-protocol-cdp/src/wire.rs 有一个内容冲突：
# 合并结果同时保留 master 的 Debugger/IO-route exceptions，以及旧提交的 Runtime control-method
# exceptions 和 Page.getNavigationHistory continuation fence。

cargo nextest run -p lightmount-protocol-cdp -p lightmount-renderer-v8 -p lightmount-protocol \
  -E 'package(lightmount-protocol-cdp) | test(related_page_named_frame_lookup_follows_chromium_frame_tree_order) | test(named_frame_lookup_skips_candidate_the_source_cannot_navigate) | test(per_page_isolate_policy_keeps_window_open_routes_page_owned) | test(window_open_noopener_navigates_existing_named_iframe_and_returns_null) | test(hyperlink_target_blank_reloads_rel_opener_policy_for_each_activation) | test(hyperlink_javascript_url_csp_checks_the_source_document_before_target_selection) | test(named_opener_hyperlink_creation_and_reuse_are_owned_by_renderer_group) | test(named_suppress_opener_hyperlinks_create_distinct_fresh_groups_with_live_names) | test(named_form_post_reuses_renderer_group_target_and_preserves_exact_request) | test(base_target_blank_form_post_creates_fresh_target_with_exact_request)' \
  --no-fail-fast --status-level fail --final-status-level fail --failure-output immediate
# run ce377834-850c-48c9-8f2d-4b4f867db090：19 passed；包含 wire crate 全部单测和 10 条 popup 邻接回归。

cargo nextest run --no-fail-fast --status-level fail --final-status-level fail
# run f83a2567-7fda-490e-bc18-63a6a7f73f8e：16011 passed, 18 skipped；执行阶段 100.329s。

cargo fmt --all --check
# passed

cargo clippy --workspace --all-targets -- -D warnings
# passed；1m 31s。

git diff --check
# passed
```

Phase 5E2F 提交前门禁结果：

```bash
cargo nextest run --no-fail-fast --status-level fail --final-status-level fail
# run 6cf8a817-01c0-4554-a5f2-f0771db5db3d：16009 passed、3 failed、18 skipped。
# 三个 failure 都是 charset/data URL form 用例仍断言 GET named iframe 必须使用旧 URL bootstrap；
# runtime 已正确产生 typed Request(GET, body=None)。断言改为同时验证编码 URL、method/body 和 Referer。

cargo nextest run -p lightmount-renderer-v8 \
  -E 'test(form_submission_rewrites_charset_control_from_accept_charset) | \
      test(form_get_submission_uses_document_encoding_for_query) | \
      test(iso_2022_jp_get_form_data_url_target_posts_stateful_values)' \
  --no-fail-fast --status-level fail --final-status-level fail --failure-output immediate
# run 16862d74-5fbf-4f21-b48e-3ab142a6de83：3 passed。

cargo nextest run --no-fail-fast --status-level fail --final-status-level fail
# run d09b90aa-93dc-4fad-8c6c-ba09b2165071：16012 passed、18 skipped；执行阶段 99.264s。

cargo fmt --all --check
# passed

cargo clippy --workspace --all-targets -- -D warnings
# passed；1m 29s。

git diff --check
# passed
```

Phase 5E2F rebase 后集成门禁：

```bash
git pull -r origin master
# origin/master 从 744e161dad 前进到 768c70dfd7；40 个 popup 分支提交无冲突重放。
# master 新增 test(cdp): stabilize shadow DOM navigation fixtures，只修改 protocol DOM 测试 fixture，
# 不与 E2F form/child owner 代码重叠。

cargo nextest run -p lightmount-renderer-v8 -p lightmount-protocol \
  -E 'test(related_page_named_form_post_uses_nested_target_owner_and_exact_request) | test(related_page_named_frame_lookup_follows_chromium_frame_tree_order) | test(named_frame_lookup_skips_candidate_the_source_cannot_navigate) | test(per_page_isolate_policy_keeps_window_open_routes_page_owned) | test(form_target_blank_reloads_rel_opener_policy_for_each_submission) | test(canceled_post_form_navigation_aborts_signal_without_synthetic_timer) | test(detached_child_form_submit_targets_named_iframe_without_shadow_controls) | test(formdata_event_appended_entries_are_submitted_to_named_iframe) | test(submit_button_click_supersedes_programmatic_submit_after_target_change) | test(distinct_forms_keep_distinct_pending_child_target_submissions) | test(programmatic_form_submit_keeps_successive_distinct_child_targets) | test(form_top_and_parent_targets_queue_plain_top_level_navigation) | test(renderer_top_level_form_post_preserves_request_through_document_commit) | test(named_form_post_reuses_renderer_group_target_and_preserves_exact_request) | test(base_target_blank_form_post_creates_fresh_target_with_exact_request) | test(named_opener_hyperlink_creation_and_reuse_are_owned_by_renderer_group) | test(named_suppress_opener_hyperlinks_create_distinct_fresh_groups_with_live_names) | test(noopener_and_noreferrer_popups_have_one_real_navigation_with_creator_referrer_policy)' \
  --no-fail-fast --status-level fail --final-status-level fail --failure-output immediate
# run b226a25f-d8b8-4e8b-bc6f-17a35490c5e1：18 passed。

cargo nextest run --no-fail-fast --status-level fail --final-status-level fail
# run f08691b1-0ee2-4332-a3fb-3c4c0f6fbd8d：16012 passed、18 skipped；执行阶段 101.151s。

cargo fmt --all --check
# passed

cargo clippy --workspace --all-targets -- -D warnings
# passed；1m 30s。

git diff --check
# passed
```

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
