# Browser Core 与协议前端分离改造计划

最后更新：2026-08-06

状态：核心改造已完成，进入稳定期。本文保留迁移顺序和历史实现记录；当前验收以 authority、
lifetime 和 progress dependency 为准，不再以统一类型名、消除所有 `await` 或让 standalone
`Browser` 经过 DevTools Host 作为完成条件。类型名是工作名，所有权边界比命名优先。

## 结论

Moli 长期要把当前混在 `CdpConnection` / `CdpScheduler` 里的两种责任拆开：

1. **Browser Core / Browser Owner** 自主拥有 BrowserContext、Target、Page、NavigationEngine、
   navigation/replacement/termination、history、网络和 profile/storage 运行时；
2. **协议前端** 只拥有 transport、frontend session、domain enable/subscription、command correlation、
   wire shape、DevTools renderer channel 和 browser fact 的协议投影。

这里的“拆开”首先指 authority、state、lifetime 和 scheduler lane 分开，不要求立即拆 OS
process。第一阶段仍使用同一进程、同一 dedicated current-thread runtime / local executor，避免
增加 V8 thread-affinity 风险和进程级资源成本。只有 typed command/fact 边界稳定、且性能或隔离
证据支持时，才重新评估 IPC 或多进程。

这不是把当前的跨线程 CDP 和 Browser Core 搬到一起。当前 CDP scheduler 与承担 Browser Core
职责的 `CdpConnection` / `NavigationEngine` 控制状态本来就在同一个 `moli-cdp-owner`
线程。改造目标是在这个线程内先拆出两个独立的逻辑 owner queue：

```text
Browser Owner queue：唯一执行 browser action，并自主推进 navigation
CDP frontend queue：提交 typed command、订阅 fact，并生成 CDP response/event
```

因此第一阶段的完成标准是 execution authority 和 progress dependency 分离，而不是线程数发生
变化。两个 queue 可以长期共用同一个 current-thread executor；同线程只保证 turn 串行，不自动
保证 ownership 和 progress 正确。

最终原则是：

```text
CDP / BiDi / Classic / CLI command ----+
                                        |
renderer navigation intent ------------+--> Browser/Page Owner
                                             |
                                             +-> network / history
                                             +-> navigation / replacement
                                             +-> target / page lifetime
                                             +-> renderer lifecycle
                                             |
                                             v
                                      browser facts/outcomes
                                             |
                         +-------------------+-------------------+
                         v                   v                   v
                        CDP                 BiDi            high-level wait
```

CDP 仍然可以发起 `Page.navigate`，但它只提交命令；renderer 的 `location.href` 也只提交 intent。
两种来源都由同一个 Browser Owner 安装并执行 navigation。协议 observation、WebSocket flush、慢
客户端和 pending DCL/load attachment 都不能成为 browser progress 的隐式开关。

## 这份文档解决什么问题

本文集中回答以下长期问题：

- `CdpConnection` 中哪些状态属于浏览器，哪些属于协议 frontend；
- 页面 JS 导航和 `Page.navigate` 最终由谁执行；
- Browser Owner 如何在没有下一条 frontend command 时继续推进；
- DCL/load、navigation commit、target lifecycle 和 network activity 如何投影到多个协议；
- frontend disconnect、慢 socket、pending command 和 event backpressure 是否影响页面；
- 现有 `moli-core`、`moli-protocol`、renderer 和应用层怎样分阶段迁移；
- 如何避免一次性重写 `CdpConnection`，以及每一阶段用什么证据验收。

本文不重新设计 V8 Inspector session restore，也不重新定义 DCL/Load/Done。相关语义分别由：

- [`chromium-aligned-devtools-navigation-redesign-2026-07-27.md`](chromium-aligned-devtools-navigation-redesign-2026-07-27.md)；
- [`document-milestone-navigation-completion-design-current.md`](document-milestone-navigation-completion-design-current.md)；
- [`page-lifecycle-current.md`](page-lifecycle-current.md)

继续负责。本文只定义它们与 Browser Core / protocol frontend 的交界。

## 当前线程拓扑与问题定性

截至 2026-08-03，`serve` 路径的相关执行拓扑是：

```text
application Tokio multi-thread runtime
  -> protocol transport / registry / service tasks

lm-protocol-seq shared protocol OS thread
  -> Tokio current-thread runtime + LocalSet
     -> independent application owner task / input composition
        -> BrowserHostActor (Core mailbox + turn capability owner)
           -> BrowserHostTurn
        -> BrowserHostExecutionLane (application pending/completion residence)
        -> CdpScheduler
        -> protocol frontend residences
        -> CdpConnection
           -> BrowserHostTurn short physical start/completion adapter
           -> BrowserHostHandle (producer endpoint only)
           -> BrowserContext / Target state
           -> active/background NavigationEngine control

render_runtime dedicated OS thread
  -> renderer owner loop
     -> V8 / DOM / parser / timer / microtask / Document lifecycle
```

也就是说：

- CDP transport 不必和 owner 在同一线程；输入通过 channel 进入 owner；
- `CdpScheduler`、DevTools Host adapter 与 Browser Host owner task 在同一个 `lm-protocol-seq`
  current-thread runtime 上独立调度；共享物理线程不等于共享 actor/state，frontend wait 会正常 yield；
- renderer 已经是另一个专用线程，通过 command、publication 和 completion channel 与 owner 通信；
- 第20切片建立了第一条 protocol-neutral Browser Owner queue；第31切片进一步把 mailbox receiver/selection
  收进 Core `BrowserHostActor`，并让 `CdpConnection` 在该 input boundary 只保存 cloneable
  `BrowserHostHandle` producer endpoint。
  renderer top-level navigation 现在直接 publish 到 handle，不再经过 protocol scheduler envelope；
- 第32切片删除公开的 raw-input pop/complete API。只有 Core actor 能从 mailbox 构造
  `BrowserHostTurn` capability，并在持有唯一 mutable turn 权限期间调用 protocol-neutral executor trait；
  `CdpScheduler` 和行为 fixture 的调度路径不再取得或转交裸 `BrowserOwnerInput`；
- 第33切片把 `BrowserHostActor` residence 从 `CdpScheduler` 字段移到 application input composition；CDP、BiDi、
  Classic 的 blocking select 以及 direct command wait 都直接监听 Core mailbox。actor 在 receive 时内部保留 exact
  selected turn，application 只得到无 payload marker，再于 select branch 外非取消地调用 executor；
- 第34切片把 `BrowserHostTurnExecutor` 改成同步短 turn：renderer/network 等 participant wait 被封装成
  move-owned `PendingBrowserHostTurn`，由 application `BrowserHostExecutionLane` 登记并异步等待，再把
  `CompletedBrowserHostTurn` 作为独立 wake 回投。Browser Host actor 和 `CdpConnection` 都不会跨该 participant
  wait 被借用，后续 mailbox input 可以继续启动；
- 第35切片把 response-ready navigation completion 再拆成 network load、prepared-Document configuration、
  renderer Document commit 三个 move-owned phase。direct Host completion 和 production background navigation
  共用 `domains/page/navigation_completion.rs` 状态机；background gate 只在最终 commit/error phase 结算，两个中间
  participant wake 不再借用 `CdpConnection`，也不会伪装成 terminal navigation；
- 第36切片继续把 response-ready navigation tail 中的 renderer Inspector replay 拆成 move-owned participant。
  每次只启动一个 exact Page dispatch，wait 不借用 `CdpConnection`，completion apply 后才启动下一 replay；同一个
  background gate 贯穿 commit 与全部 replay，只有 replay batch 终止后才结算。target-keyed `NavigationEngine` 也改为
  在 Document commit 的同步 apply turn 内 adoption，不能被迟到的旧 replay tail 写进 successor Page；
- 第37切片把 generic materialized `Loaded` / `Download` / `Failed` outcome 也接入同一个 navigation-tail
  participant seam。body apply 结束后先在当前 Document commit turn 内 adoption engine，再逐项发布 Inspector replay；
  background lifecycle completion 不再把整个 replay batch inline 等完，exact gate 继续到最后一个 replay 才 terminal；
- 第38切片把 generic loaded Page 的 Runtime/Fetch/permission restore 拆成 move-owned participant；第39切片又把
  physical replacement 与退出 residence 的旧 Page close 拆成独立 disposal participant。两个 wait 都不持有
  `CdpConnection`，engine/Page residence 在各自 exact completion apply turn 提交；
- 第40切片确认 disposal 后的 exact DedicatedWorker retirement 没有真实异步 teardown：旧 Page close 已先结算
  renderer lifetime，剩余 worker target/session registry commit 在同一个 owner apply turn 同步完成。它不再借用
  generic async binding-cleanup wrapper，也没有为了形式拆分新增一个永远 ready 的 participant；
- 第41切片继续删除 loaded lifecycle/activity projection 的伪异步 future：response、Target/network、DCL prefix 在同一
  owner apply turn 同步投影，真正的 load wait 以 `MainDocumentLoadOwnerAction` 在函数返回前发布；
- 第42切片把 loaded-navigation 的 BiDi preload listener startup 拆成 exact Page-owned participant：commit 时冻结
  realm inventory，按 `realm × handoff` FIFO 每次只启动一个 proxy/listener/cleanup renderer operation；pending wait
  move-own Page dispatch，不借用 `CdpConnection`，completion apply 每次先重验 Page generation。listener 首条 deferred
  reply 继续走独立 response-ready lane，不再成为 navigation terminal 条件；
- 第43切片继续把这些 preload operation 共用的 Runtime output normalization 拆成 exact renderer-attachment
  participant。context-id compatibility 与 DOM-node subtype lookup 每次只启动一个 Page command，wait move-own
  completion，apply 同步重验冻结的 BrowserContext/Target/renderer attachment；loaded-navigation 的 proxy、listener 与
  cleanup completion 因而不再通过通用 async normalization 借用 `CdpConnection`；
- 第44切片把 `Page.createIsolatedWorld` 的 post-create BiDi preload listener startup 也接入命令自身的 participant
  状态机。world 创建结果先冻结 execution-context id，再由 exact Page realm-inventory participant 和第42切片的
  preload batch 逐步推进；所有 wait 都 move-own pending participant，不借用 `CdpConnection`。realm inventory apply 前若
  Page 已 replacement，旧命令仍返回已经创建出的 context id，但不会进入 successor Page 启动 proxy/listener；
- 第45切片把 execution-context 到 BiDi preload listener 的 realm inventory 与 listener batch 收敛成 Runtime
  `BidiPreloadListenerSetup`，并让 `Page.addScriptToEvaluateOnNewDocument` 的当前 Page 安装、run-immediately
  execution-context 处理和 listener startup 进入独立 `preload/add_command.rs` 状态机。脚本注册先提交到 Target；
  renderer wait 冻结 exact attachment，replacement 后丢弃旧 completion 但保留注册成功，不能把旧结果应用到
  successor Page。Page command 的每次 wait 都 move-own participant；protocol-neutral direct 入口只保留显式
  compatibility drain；
- 第46切片把普通 Runtime command 与 `Runtime.add/removeBinding` 的 Inspector output normalization 也并入
  command-owned participant chain。context-id/DOM-node lookup wait 不再发生在 completion apply 内；participant
  冻结原 command owner、deferred-response receiver 与 Page attachment，replacement 后以 exact stale error 终止旧
  command 并清理 await，不能等待旧 response channel 或进入 successor Page；
- 第47切片把 scheduler-facing protocol-neutral `ReleaseObjects` 从 start 阶段的多 handle inline drain 改成逐 handle
  command-owned participant chain。每个 `Runtime.releaseObject` 复用 exact attachment/stale completion 语义，外层事务
  move-own 剩余 handles、当前 correlation、protocol events 与 renderer predecessor；首个 stale completion 会终止整个
  release command，不能继续进入 successor Page；
- 第48切片完成 Phase 3 exit audit。renderer top-level navigation 的唯一 execution authority 已是 Core
  `BrowserHostActor`；production `ProtocolSchedulerWork` 不再包含该 action，Page domain subscription、下一 frontend
  command、socket writer readiness 与 frontend detach 都不是它的 progress trigger。剩余 popup/termination owner work
  归 Phase 4，fact journal 和 Host lifetime 分别归 Phase 5/6，不再反向阻塞 Phase 3；
- 第49切片开始 Phase 4 的 command lane cutover。raw CDP 顶层跨 Document `Page.navigate` 现在只在 frontend
  解析/冻结参数并 publish `BrowserFrontendCommand::Navigate`；Core actor 选择 exact `BrowserHostTurn` 后才启动
  navigation。Core input 只携带 opaque Browser command id、exact Page residence、URL/referrer，CDP id/session/result
  payload 留在 Protocol projection；Browser Host 不可用时返回 typed error，没有 direct fallback。non-flattened
  `Target.sendMessageToTarget` 也已把 nested command 改成逐段 participant chain，不能在 Target handler 内 inline drain
  Browser Owner command。same-document、child-frame、BiDi/Classic direct navigation 以及已启动 navigation 的 neutral
  command completion 仍是 Phase 4 后续切片；
- 第50切片把 actor-selected raw CDP `Page.navigate` 的 participant lifetime 也交给 Browser Host。frontend 不再收到
  `PageCommandTaskStep::Pending` 并代替 owner drain；`PendingBrowserHostTurn` move-own exact Page participant、detached
  command context 与 terminal projection sender，每个 completion 都作为独立 Browser Host wake 回到应用 owner loop。
  frontend 慢或在 selection 前后丢弃 wait 都不再改变导航 completion；live frontend 仍在原 command/nested Target
  wrapper 内投影一次 response。terminal payload 尚是 Protocol `CommandOutputPlan`，下一切片继续拆成 neutral outcome；
- 第51切片把 Browser Host terminal envelope 中已有的 command response 拆成 Core
  `BrowserNavigateCommandOutcome` 与 Protocol `BrowserNavigateCommandProjection`。outcome 只保存 requested URL、Target/
  loader、navigation error/download classification 或 typed rejection，不包含 CDP id/session、JSON、wire error code 或
  output queue；sidecar 只保存 response 插入位置、session routing、wire decoration、扩展字段和 renderer boundary 相对侧。
  live frontend 在原 wrapper 内重新投影，detached frontend 只丢 response 而保留 Browser effects。普通成功导航当时仍由
  `BackgroundNavigationEarlyResult` 稍后回包，因此 Host terminal 对该路径明确返回 deferred `None`，没有伪造 completed
  outcome；
- 第52切片把上述 early-response producer 改为 `BackgroundNavigationEarlyOutcome`。network job 在 response headers ready
  时发布 Core `BrowserNavigateCommandOutcome`；CDP command id/session、未知 result 字段与 wire error decoration 只留在
  `BrowserNavigateCommandOutcomeDelivery` projection sidecar。它与 response-head Network progress 继续共用同一个物理 FIFO，
  receiver/frontend ingress 才恢复 `BackgroundProtocolEvent`，因此没有用第二个 channel 引入跨 channel 猜序；普通成功
  `Page.navigate` 的 early completion 已 neutral 化，但 lifecycle/Network/Target facts 仍是既有 Protocol event，不能据此
  宣称 Phase 5 fact journal 已建立；
- 第53切片把 raw CDP 顶层 same-Document `Page.navigate` 也汇入同一个 `BrowserFrontendCommand::Navigate`。frontend
  不再依据 mutable current URL 预分类 fragment navigation；Core actor 先选择 exact Page，physical Browser Host executor
  重验 residence 后才在该 owner route 内分类并启动既有 same-Document participant。该分支用 owner Target 重建无
  `loaderId` 的结果，只把 response correlation 留给 frontend；renderer publication 继续由独立 scheduler ingress 推进，
  不依赖 command receiver poll。child-frame、reload/history 与 BiDi/Classic direct adapter 仍待后续汇流；
- 第54切片把 raw CDP 顶层 `Page.reload` 迁入新增的 protocol-neutral `BrowserFrontendCommand::Reload`。Core input 只携带
  opaque command id、exact Page residence 与 reload options，刻意不携带 frontend 读取的 current URL；Browser Host 选中并
  重验 exact Page 后才在 owner route 内解析 URL、标记 history replace 并持有 reload participant。navigate/reload 共用一个
  frontend-navigation correlation map 和 neutral outcome/projection seam，没有新增平行 owner queue；slow frontend 不再延迟
  reload HTTP request 或 replacement。history traversal、child-frame 与 BiDi/Classic direct adapter 仍待后续汇流；
- 第55切片把 raw CDP `Page.navigateToHistoryEntry` 迁入
  `BrowserFrontendCommand::TraverseHistory`。frontend 只提交 entry id 与 exact Page，不再预读 destination URL；Core history
  owner 新增唯一的 entry/delta resolver，依据当前 cursor 与 Document sequence 返回 no-op、same-Document delta 或
  cross-Document destination。Browser Host 选中 exact turn 后才完成解析、分类并持有 participant；same-Document renderer fact
  与 cross-Document replacement 都不依赖 frontend receiver poll。BiDi/Classic direct adapter 已复用 Core resolver，但其 admission
  尚未汇入 owner mailbox；child-frame 也仍由 Page owner direct route 处理；
- 第56切片把带 DCL/load wait 的 BiDi/Classic 顶层 navigate/reload 也接到同一个 Browser Owner mailbox。Protocol start
  adapter 只冻结 exact Page 和 frontend result projection；pending wait move-own neutral Browser completion，不借用
  `CdpConnection`。application scheduler 在等待 typed reply 时持续处理 Browser Host turn、participant completion 与 background
  output；新 Page renderer stream 在 exact terminal commit boundary 前由显式 ingress gate 保留，不能抢先进入尚未安装 replacement
  projection 的 Protocol 状态。direct frontend 不再靠 inline drain 取得 Browser execution authority。child-frame navigate 仍由
  Page/renderer owner 处理；`wait:none` 暂留既有 background outcome FIFO，避免在 Phase 5 前再造第二套 correlation queue；
- 第57切片把带 DCL/load wait 的 BiDi/Classic 顶层 history traversal 也接入同一 mailbox。Core command input 现在接受
  entry 或 delta；Browser Host 选中 exact Page 后才依据 browser-owned current cursor 解析 delta、查 URL 并分类 no-op、
  same-Document 或 cross-Document。Classic back/forward 删除了 frontend `GetNavigationHistory -> entry` 预计算，越界只在
  Classic result projector 映射为成功 no-op。same-Document renderer 拒绝并回退 URL load 时，terminal neutral outcome 会从
  `SameDocument` 改记为 `CrossDocument`，frontend 不再从 URL 或旧 history snapshot 猜测 realm 是否 replacement；direct
  `wait:none` history 仍与其他 `wait:none` navigation 一样暂留 Phase 5 前的 mixed outcome FIFO；
- 第58切片把 Page 发起的 crash/close terminal action 接入同一个 Browser Owner mailbox。Core input 只保存 move-only
  `BrowserTargetTerminationRequest`，其中冻结 BrowserContext、Target 与 Page generation；actor-selected turn 同步提交 Core
  authority 与 physical Page absence，只有真实 retired Page destruction 才作为独立 Host participant 返回 completion mailbox；
- 第59切片把显式 top-level `Target.closeTarget` 的 Core commit、retired-Page disposal 与 retained-Target promotion 接入同一
  Host participant lane；第60切片再把 popup/auxiliary Target navigation 从 Protocol scheduler 搬到 Browser Owner input；
  第61切片最终删除 Page/Target termination 的 paused-fetch `ProtocolSchedulerWork` admission。command completion 仍先投影
  exact renderer predecessor，但 termination input 已直接进入 Host mailbox，真正执行只由稍后的 actor selection 授权；
- 第62切片把 direct BiDi/Classic `wait:none` 顶层 navigate/reload/history 也接入既有 Browser Owner mailbox。
  frontend 不再因 wait policy 回退到 Page direct path；exact Host turn 选中并启动 action 后即返回 protocol-neutral accepted
  outcome，detached load 再独立推进 commit/DCL/load。旧 background command correlation 在 admission 时清除，因此后台完成
  不能生成第二个 frontend response；detached load completion 仍暂住既有 background completion/output transport，尚不是
  Phase 5 Browser fact channel；
- 第63切片把 raw CDP `Page.stopLoading` 的 admission、当前 Document 选择与首个 renderer stop participant 迁入同一
  Browser Owner mailbox。frontend 只提交 opaque command id 和 stable Target/Page-slot capability；Host turn 被选中后才解析
  slot 的当前 generation。renderer wait 期间不借用 `CdpConnection`，replacement 后的旧 completion 按 exact generation
  stale-drop，frontend 丢失也不取消 Browser action。旧的 frontend fake-pending/direct completion path 已删除；paused Fetch
  cancellation 的逐项 materialization tail 当时仍是迁移期 inline apply；
- 第64切片继续把该 paused-Fetch cancellation tail 拆成 Browser Host 可见的逐项 participant chain。主文档
  request/auth/response failure 复用既有 navigation completion participant，subresource request/auth/response failure 各自
  start 一个 exact Page renderer participant；每次 completion apply 都重新验证 frozen Page generation，旧 Document 的
  cancellation 不能进入 replacement Page。原 frontend session 只保留为 response/event projection destination，不再参与
  action route 选择；终止和 BrowserContext disposal 等尚未迁移的调用方暂时通过同一状态机的 compatibility drain 执行，
  避免维护第二套 cancellation 语义；
- 第65切片把 raw CDP request-stage 主文档 `Fetch.failRequest` 也改成 Browser Owner input。Core 只接收 opaque command id、
  exact Page residence 与 protocol-neutral failure decision；move-owned `PendingFetchNavigation`、CDP response correlation 和
  Network/Page event projection 留在 Protocol sidecar。Host selection 后通过独立 `domains/fetch/navigation_decision.rs`
  participant chain materialize/apply failure，frontend wait 丢失不取消动作，replacement 后旧 completion 只结算原
  navigation 的 stale error，不能 invalidate successor Page。Host publication 失败会恢复已取走的 paused request，不存在
  direct fallback；typed BiDi/Classic DevTools command 和 response-stage transfer 仍明确保留 compatibility completion，等待其
  frontend scheduler task seam 一并迁移；
- 第66切片继续把 raw/nested CDP request-stage 主文档 `Fetch.continueRequest` 接到同一个
  `ResolvePausedNavigation` owner input。request URL/method/body/header 与 response-stage intent 只在 exact Host turn 被选中并重验
  Page generation 后应用；Host admission 失败会原样恢复 paused request。普通 load 复用既有
  `BackgroundNavigationLoadJob`，intercepted network fetch、auth challenge body collection、buffered/streaming Document build 与
  response-stage prepared Document 各自成为 move-owned Host participant，等待时不借用 `CdpConnection`；frontend receiver 丢失
  只丢 `Fetch.continueRequest` wire response，不取消 navigation。exact pending command 同时持有 raw renderer publication gate，覆盖
  Host completion 到 frontend command insertion boundary 的 handoff，避免 `MainDocumentCommit` 在 physical Page install 前按旧
  loader stale-drop；它不阻止 Host 自身推进，也不缓存/伪造 DCL。typed BiDi/Classic continue、`continueWithAuth`、response-stage
  continue/fulfill/fail 仍保留 compatibility 路径；
- 第67切片继续把 raw/nested CDP 主文档 `Fetch.continueWithAuth` 接到同一个
  `ResolvePausedNavigation` owner input。Core auth decision 只表达 abort、expose challenged response 或以 browser credentials
  retry，不携带 Fetch request id、CDP action/session 或 401/407 response；Protocol sidecar move-own 原 auth pause、response body 与
  projection route。Host turn 在 apply auth decision、网络 retry、Document build 和每个 renderer completion 前重验 exact Page；
  frontend receiver 丢失只丢 auth ACK，不能取消 retry/commit，Host admission 失败则把同一个 auth pause 连同原 response `Arc`
  原样放回 registry。Digest retry 保留 libcurl buffered path，Basic response-stage interception 保留 streaming path；raw
  Default/Cancel/ProvideCredentials 都进入 owner lane，但 chained multi-session auth 的下一次纯 projection、typed BiDi/Classic auth 与
  response-stage credentials compatibility 路径未在本切片顺带迁移；
- 第68切片补齐 BrowserContext exact-instance capability。Core-issued `BrowserContextHandle` 以单调 instance identity 区分 public id
  复用，registration/removal 与 physical projection 都校验 exact handle；因此排队的旧 Context command 不能在同 id 重建后命中新
  instance。该能力是 whole-Context owner action 的 admission 前置条件，不是仅为 disposal 增加的 Protocol token；
- 第69切片把 production raw CDP 和 typed BiDi/Classic `Target.disposeBrowserContext` 接入同一个 Browser Owner mailbox。
  Host selection 后才建立 move-only Context disposal reservation，阻止新 Target/navigation/Page replacement 进入；paused-Fetch
  cancellation、每个 exact Target/Page close 与 residual Page disposal 都成为 Host participant，terminal turn 才依据当前 topology
  删除 exact Context 并选择 successor。frontend timeout/drop 只丢 reply，不能取消已经 accepted 的 cleanup；
- 第70切片把 raw/nested CDP response-stage 主文档 `Fetch.continueResponse` 接入既有
  `ResolvePausedNavigation` owner input。response transfer 与 Fetch/projection identity 留在 Protocol sidecar，Core 只看到 exact Page
  和 status/header decision；Host selection 后才释放 response pause，并把 buffered/captured/streaming Document build 表示为
  move-owned participant。frontend 丢失只丢 Fetch ACK，不能取消已经接受的 response replay；Host admission 失败会原样恢复 transfer，
  active body stream 则作为 command rejection 返回，不能错投给原 `Page.navigate`；
- 第71切片把 raw/nested CDP 主文档 request/response-stage `Fetch.fulfillRequest` 与 response-stage
  `Fetch.failRequest` 收入同一 `ResolvePausedNavigation` owner lane。Core synthetic-response decision 只保存 status/header/body，
  exact Fetch identity、paused request/transfer 与 projection route 仍在 Protocol sidecar；Host selection 后才能消费 pause，
  synthetic Document build 作为 move-owned participant 继续运行。frontend 丢失只丢当前 Fetch ACK，原 navigation 仍会
  commit/fail；Host admission 失败会恢复 exact sidecar，不存在 direct fallback；
- 第72切片把 production typed BiDi/Classic 的五类 terminal Fetch decision 接入 scheduler-visible task seam。
  `ContinueRequest`、`ContinueResponse`、`ContinueWithAuth`、`FailRequest` 与 `FulfillRequest` 不再由一个借用
  `CdpConnection` 的 frontend future 隐藏 participant wait：主文档 decision 进入同一 Browser Owner mailbox，subresource
  decision 保留 exact Page participant；application scheduler 在等待 typed result 时继续选择 Host turn、participant completion
  与独立 protocol ingress。direct `CdpConnection` 入口只为没有 application Host loop 的测试/嵌入调用保留显式 compatibility
  drain，不再是 production BiDi/Classic 路径；
- 第73切片把 download action progress 与 frontend response flush 拆开。网络、流式写盘、artifact rename 与 registry terminal
  立即推进；有界 projection gate 只在 response permit 后释放 start/progress event，permit cancellation 也不能取消下载。每个被 gate
  挡住的 download 暂有一个短生命周期 projection waiter，但没有新增 Browser Owner drain/pump；Phase 5 统一 fact projector 建立后必须
  替换该 watcher，不能扩散成逐功能投影机制；
- 第74切片把 `Target.createTarget`、`Runtime.runIfWaitingForDebugger` 与 `Page.enable` 的 initial target URL replacement 归一为
  `BrowserInitialTargetNavigationInput`。三个 trigger 只发布 exact Page + immutable URL；Host selection 后再由 Core 检查 initial
  Document、pending request 与 generation，并启动既有 navigation participant。Host 不可用时返回 typed error，不存在 Protocol direct
  start fallback；
- 第75切片把 renderer-produced top-level joint-session history traversal 也汇入同一 mailbox。renderer output ingress 冻结 exact
  Page residence 与 delta，Browser Host 选中后才按 Core authoritative cursor 解析 entry/URL 和 same-/cross-Document 分类；stale Page
  generation、越界 delta 或已消失的 destination 都只在 owner turn no-op/drop。Runtime command response 不再 inline 完成 traversal，旧的
  session-routed direct completion path 已删除；
- 第76切片把 `Page.createIsolatedWorld` 的 requested initial URL prerequisite 改为
  `BrowserFrontendCommand::EnsureInitialTargetNavigation`。Core input 只携带 opaque `BrowserCommandId`、exact Page 与 immutable URL；
  command task 只保存 Protocol sidecar receiver，Browser Host selection、navigation participant 与 terminal projection 不再由 CDP task
  启动或推进。frontend wait 丢失时 Host 仍完成 action，并保留 exact renderer insertion boundary；旧的 nested navigate/Fetch continuation
  phase 及其最后两个 direct navigation helper 已删除。Phase 4 exit audit 因而通过；fact/outcome journal 与 Host lifetime 继续分别归
  Phase 5/6；
- actor 创建/teardown 目前仍与 frontend owner loop 同 lifetime，物理执行/投影 adapter 也仍借用
  `CdpConnection`；BrowserContext disposal 与 Fetch 的 direct-call compatibility wrapper、runtime
  realm-created/initial-target 的 BiDi listener
  compatibility 入口、targeted preload direct adapter 以及部分无 scheduler participant loop 的 direct command adapter
  仍可能在 Protocol 内 `await`，输出也仍是 `CdpTurnOutcome`。
  bounded writer 已保证慢 frontend 不阻塞 Browser Owner；独立 Host lifetime 和 protocol-neutral fact channel 尚未完成，
  但它们分别是 Phase 6 和 Phase 5 的 gate；
- `CdpConnection` 仍混合 protocol projection state 和尚未迁出的 physical BrowserContext/Target/Page payload。

所以 403 Document 的 DCL 后 successor navigation 曾经不推进，不是 CDP 与 Browser Core 跨线程
造成的 mutex/thread deadlock，而是同一个 owner thread 内的**逻辑 progress starvation**：renderer
已经发布 navigation intent，但它曾被表示为 `ProtocolSchedulerWork`；protocol residence 没有选择该
work 时，Browser navigation 就没有获得 execution authority。第20切片已把 renderer top-level navigation
从该 residence 移除；第49切片又把 raw CDP 顶层跨 Document `Page.navigate` 的 start authority 移入同一 actor。
第62切片继续把 direct `wait:none` navigation 的 admission/start authority 汇入该 actor；第63切片再把
`Page.stopLoading` 的 action selection 与 renderer stop participant 汇入该 actor；第64切片又把 stop-loading Fetch
cancellation 的 renderer waits 暴露给同一 Host participant loop；第65切片再把 raw CDP
request-stage 主文档 `Fetch.failRequest` 的 action authority 汇入该 lane，第66切片继续把 raw/nested CDP request-stage
`Fetch.continueRequest` 的 request mutation、network fetch 与 Document build 汇入同一 participant lane，第67切片再把同一条
navigation 的 raw/nested CDP auth decision、credential retry 与 challenged-response handling 收回该 lane。detached navigation
completion、direct-call compatibility wrapper 和完整 Browser Host lifetime 仍按后续 phase 继续迁移；production raw CDP 与 typed
application scheduler 的 whole-Context action 已由第69切片迁入 Host，raw/nested CDP response-stage continue 已由第70切片迁入同一
lane，其余 main-Document terminal fail/fulfill decision 又由第71切片收口；第72切片再让 production typed BiDi/Classic terminal
Fetch 通过 application scheduler 驱动同一 owner/participant chain。第73切片继续把 download action progress 与 frontend
response-flush projection 拆开：网络、流式写盘和 registry terminal 不再等待 CDP response，事件 gate 只保留有限 start prefix 与最新
progress。第74切片把三个 initial target URL trigger 归一成 exact Browser Owner input，第75切片又把 renderer top-level history delta
从 session-routed direct completion 搬入同一 mailbox。第76切片再把 `Page.createIsolatedWorld` 的 requested initial URL prerequisite
改为 opaque Browser command + Protocol continuation sidecar，Browser Host 独立拥有 selection、participant 与 terminal，旧 direct wrapper
已删除。Phase 4 exit audit 已通过：`ProtocolSchedulerWork` 不含 navigation/replacement/popup/termination payload；现存
`MainDocumentLoadOwnerAction` 与 detached outcome transport 是 Phase 5 fact/outcome 工作，完整 Browser Host lifetime 是 Phase 6 工作，不能为了
“零 Protocol await”把这些边界重新误算成 Phase 4 action。

2026-08-02 的短期修复已经消除已知的 `pending_load` 错误全局阻塞。长期改造不再重复修这个具体
guard，而是让普通 CDP observation 从类型和所有权上失去阻塞 Browser Owner progress 的能力。

## 术语

### Browser Core

协议无关的浏览器运行时和所有权边界。它不是 GUI、paint/compositor 或完整 Chromium browser
process 的同义词。对 Moli 而言，它至少包含：

- browser instance 和 BrowserContext lifetime；
- Target/Page registry；
- active/background `NavigationEngine`；
- top-level navigation、history traversal、reload、popup 和 termination owner；
- profile、cookie、storage partition、network policy 和 download 的浏览器级状态；
- renderer intent 的消费与 exact browser fact 的发布。

初始实现放在 `moli-core` 的 browser/page orchestration 边界，不先新增 crate。

### Browser Host / Browser Owner

`Browser Host` 指一个运行中的 Browser Core 实例；`Browser Owner` 指它的单 owner actor/lane。
二者可以是同一结构的不同视角。工作名可以调整，但必须保持：

- browser mutable state 只有一个执行 owner；
- action 只能在 owner lane 执行一次；
- frontend 只能通过 typed command/handle 访问。

### Protocol Frontend

把外部或产品 API 翻译为 Browser Core command，并把 browser facts 投影为对应结果的适配器：

- raw CDP WebSocket；
- WebDriver BiDi；
- WebDriver Classic。

CDP frontend 额外拥有 DevToolsSession、Target domain subscription、V8 Inspector command
correlation、CDP error/event shape 和 socket flush ordering。这些状态不能搬进 Browser Core。

standalone CLI / MCP 是 `moli-core::runtime::Browser` 的高层调用方，不是 DevTools 协议
frontend。它们可以采用一次调用级 Browser lifetime，并按书面 policy 返回 `Page`/raw document；
它们必须复用 exact Document、typed navigation handoff、replacement 和 wait 语义，但不要求经过
`DevToolsHostAdapter`、`CdpScheduler` 或同一种 application service actor。把这条 public API
机械包装成 channel 只会多一层 task，并不会减少 Browser authority。

### Renderer Owner

拥有 V8 isolate/context、DOM、parser、timer、microtask、page task 和 exact Document lifecycle 的
renderer lane。renderer 可以产生 browser intent 和 lifecycle fact，但不拥有网络导航、Target
route、BrowserContext 或 Page replacement。

### Browser Fact

Browser Core 已确认发生、带 exact identity 和 sequence 的不可变事实，例如 navigation accepted、
committed、Page replaced、Document DCL、Target closed。fact 不是 CDP JSON，也不携带 session id。

### Observer

读取 browser fact 或 renderer lifecycle terminal 的消费者。DCL/load waiter、CDP event
subscription、benchmark listener 都是 observer。observer 的等待、超时和断开只影响自身。

### Commit Participant

少数按浏览器语义明确允许阻塞特定 navigation/request 阶段的参与者，例如 Fetch
interception、`waitForDebuggerOnStart` 或 author JS 前必须完成的 DevTools renderer attachment。
participant 必须持有 exact request-scoped typed permit；它和普通 observer 完全不同。

## 当前实现基线

### 当前 stack lifetime

raw CDP 已通过 application-owned `SharedCdpOwnerRegistry` 复用 owner actor；单个 raw CDP WebSocket
只 attach/detach frontend，断开不会直接销毁 shared/default owner。owner actor 内部仍是：

```text
Shared CDP owner task
  -> BrowserHostOwnerLane
       -> BrowserHostActor / BrowserHostState
            -> BrowserNavigationOwner / NavigationEngine
            -> browser-global behavior policy
  -> CdpScheduler
       -> DevToolsHostAdapter
            -> CdpConnection (legacy inner name)
            -> BrowserContext / Target renderer-DevTools projection
            -> BrowserHostState access capability
       -> ProtocolAdapterScheduler
  -> CdpFrontendEndpoint / CdpFrontendRouter
       -> per-socket command queue and output sink
```

raw CDP 使用 `SharedCdpOwnerRegistry` 的 owner actor；standalone BiDi 与每个 Classic session 则通过共同的
application-owned `DevToolsHostService` 创建 session-owned Host，Classic 升级出的 BiDi attach 到 exact 同一
service。三条 WebDriver 路径不再各自复制 Browser wake、renderer publication、navigation completion pump。
raw CDP 中必须区分 actual socket frontend 与 owner-task-resident DevTools adapter：前者是
`CdpFrontendEndpoint/Router/Receivers`，后者是跨 socket 存活、负责 renderer/DevTools 投影的
`DevToolsHostAdapter`。`CdpConnection` 是后者的 legacy inner name，不代表一个 WebSocket connection。
因此完整 `BrowserContext`/Target renderer-DevTools projection 不要求搬进 Browser Core；只有在 frontend 不存在时仍决定
browser fact、navigation 或 runtime lifetime 的 authoritative state 才必须进入 Core。把合法的 DevToolsAgentHost 类投影也机械
拆进 Core，会混淆 Browser owner 与 DevTools adapter，而不是加强二者边界。

### DevTools Host adapter 当前包含的迁移状态

`moli-protocol/src/conn.rs` 的 legacy `CdpConnection` 当前由 application-owned
`DevToolsHostAdapter` 唯一持有；它不是 socket frontend。其内部同时持有：

- browser/session routing、Target control 和 auto-attach policy；
- BrowserContext、active/inactive context、Target/session/attachment projection 与 non-owning Page access；
- download、global IO、network collector 和 physical BrowserContext 内的 applied policy projection；
- network collectors、storage-partition handle 和 download registry；
- scheduler hooks、scheduler-visible queues；
- application-owned `BrowserHostState` 的迁移期 strong capability；
- pending Runtime/Inspector command 与 frontend session state。

这些字段目前位于一个结构里不等于它们属于 Browser Core，也不等于每个 socket frontend 各有一份。Phase 7 仍应把真正的
per-frontend wait/subscription/routing 继续收敛到 frontend；但 renderer DevTools agent、output projection 与 non-owning Core access
可以合法驻留 application-owned adapter。

### `CdpConnection` 初始字段归属清单

下面是基于当前 struct 的第一轮 owner inventory。标记为“拆分”的字段不能整体搬迁，需要先把
authoritative browser state 与 frontend projection 分开。

| 当前字段/字段组 | 最终方向 | 预计阶段 | 备注 |
| --- | --- | --- | --- |
| `browser_context` / `inactive_browser_contexts` | DevTools Host adapter projection；authoritative 部分 Browser Core | Phase 2/6 | authoritative topology、Page runtime/lifetime、Target `sessionStorage` namespace 与 per-context renderer/network root 已进 Core；剩余 renderer channel、Inspector/Network/Log/worker projection 属于 application-owned adapter，不等于 socket frontend，也不要求机械搬进 Core |
| `browser_session_ids` / `next_session_id` | CDP frontend | Phase 6 | 纯 frontend session identity |
| `auto_attach*` / discovery/filter/listener state | CDP frontend | Phase 5/6 | policy 可以向 host 注册，但 subscription truth 留 frontend |
| `target_control` | 拆分 | Phase 1/6 | BrowserTarget registry 进 core；session/agent-host route 留 protocol |
| ServiceWorker auto-attach related owner state | protocol frontend | Phase 6/7 | service worker runtime identity 来自 browser facts，attach policy 属于 frontend |
| `next_bc_id` / `next_target_id` / shared target allocator | Browser Core | Phase 6 | 已完成：Context/command sequence 属于单 Host；Target sequence 可由 application 跨 Host 共享 |
| Page subscription generation | CDP frontend | Phase 5 | 只影响事件投影 |
| internal Runtime command id / pending Inspector await state | protocol/renderer channel | 保持 | 不进入 Browser Core command identity |
| no-session route override | 删除或收敛为 frontend typed route | Phase 1/8 | 不能被 renderer intent 依赖 |
| network request id allocator | Browser Core/network runtime | Phase 6 | 已完成；protocol 使用 Host 分配的只读 request identity |
| `window_bounds` / permission / geolocation / network condition | Browser Core policy state | Phase 6 | browser-global 部分已进入 `BrowserHostPolicyState`；context/Target override 仍随 physical runtime 提取 |
| user agent / headers / cache / proxy / TLS config | Browser Core/network runtime | Phase 6 | browser-global truth 已完成；现有 Page 的 applied projection 仍由迁移期 Protocol participant 更新 |
| initial storage partition / cookie/profile state | Browser Core/storage services | Phase 6 | 第一模块已完成：共享 application-owned live store；frontend close 只请求 flush，不再反向 merge snapshot |
| network data collectors | 拆分 | Phase 5/6 | producer/body store 进 browser/network owner；subscription/projector 留 frontend |
| global IO streams / download registry | 拆分 | Phase 6 | 下载/数据 owner 与 CDP IO handle/stream projection 分开 |
| `scheduler_hooks` / target-host lifecycle observer | typed host/frontend channels | Phase 3/5 | 不保留反向执行 authority |
| `scheduler_state` | protocol frontend scheduler | Phase 5/8 | 删除其中 browser-owner payload |
| `engine` / retained background engines | Browser Core | Phase 2 | 最早提取的 strong runtime owner |

这张表是迁移起点，不是通过移动字段就能完成的机械 checklist。每个“拆分”项都必须先写出
authoritative state、projection/cache、command 和 fact 的边界。

### 当前 renderer 导航链路

页面 JS 执行 `location.href` 时，当前路径是：

```text
renderer turn
  -> freeze RendererDocumentSourcedTopLevelLocationNavigation
  -> protocol output ingestion
  -> capture exact protocol-neutral Page residence
  -> BrowserHostHandle::publish
  -> Core BrowserHostActor mailbox
  -> application select wakes directly from Host mailbox
  -> actor retains one exact selected turn
  -> Core-issued BrowserHostTurn capability
  -> CdpConnection executor migration adapter projects Page-owned navigation
```

发布阶段明确不执行 navigation，Core `BrowserHostActor` mailbox 是 renderer action 的唯一 selection
authority；该 input 不再进入 protocol scheduler event/residence，也不继承 client-turn/load-observation
predecessor。当前 `CdpConnection` 在 publication 边界只保存 producer handle，并实现一个只能消费
Core-issued turn capability 的 physical Page/event executor。actor 已不再是 `CdpScheduler` 字段，且 mailbox wake
不依赖 frontend/renderer channel；但 actor teardown、physical execution 和 output flush 仍在同一个 application
owner loop 内，尚未形成独立 Host lifetime。

### 已完成的短期 containment

2026-08-02 的短期修复已经把 `pending_load` 从“所有 protocol residence 的全局锁”收窄为：

- 一个 exact main-document load observation attachment 的容量约束；
- residence 自身声明的 explicit `load_predecessors`。

它修复了 passive CDP wait 的确定性死锁，但 owner action 仍在 protocol scheduler 中。长期改造
不能把这次 guard 收窄误写成最终架构。

## 根本问题

### 1. 协议 observation 拥有了 browser execution authority

只要 navigation/termination action 是 `ProtocolSchedulerWork`，protocol queue 的 pending state、
client turn、command response flush 或 adapter attachment 就可能再次影响 Browser Owner progress。
短期可以修一个 guard，不能穷举未来所有环路。

### 2. renderer intent 携带了 frontend route

renderer 产生的页面导航属于 `{browser context, target, page residence, source document}`，不属于
某个 CDP session。当前 action 捕获 `CommandOwnerScope` 是迁移期便利，也让浏览器行为依赖 frontend
attach/detach 状态。

### 3. browser lifetime 绑定 frontend lifetime

每个 WebSocket 拥有独立 BrowserContext/NavigationEngine，会造成：

- frontend disconnect 与浏览器 teardown 难以区分；
- 多 frontend 无法自然观察同一 Browser Host；
- BiDi/Classic/CLI 复用时容易复制 owner loop；
- browser profile 和 service-level state 需要在 connection close 时反向 merge。

### 4. browser fact 与 protocol event 尚未彻底分层

当前不少 lifecycle/network/target 输出在执行 browser action 的同时直接生产 protocol event。
这使“事实是否发生”“哪个 frontend 订阅”“event 排在 response 前还是后”落在同一个调用栈。

### 5. 协议 backpressure 仍有机会传导到浏览器

CDP writer 使用 bounded queue 和 flush acknowledgement 是正确的 transport 约束，但慢 frontend
只能阻塞该 frontend 的输出/命令完成，不能阻塞 Browser Host 的 renderer、network 和 navigation
owner lane。

## Chromium 的三种核心角色

针对本文问题，可以把 Chromium 理解为三种角色，但不能把它们写成三个同等级、各自独立的
“浏览器运行核心”：

```text
                         CDP client
                             |
                       DevTools / CDP
                      control & observe
                       /             \
                      v               v
             Browser owner <------> Renderer owner
```

更准确的描述是：**两套运行时 owner，加一套横跨两边的控制/观察面**。

### Renderer

Renderer 拥有当前 Document 的页面执行：

- Blink DOM、HTML parser 和 style/layout 相关页面状态；
- V8、页面 JS、timer、microtask 和 task source；
- DCL/load 等 exact Document lifecycle；
- `location.href`、form submit、history 等 renderer navigation intent；
- renderer-side Blink Inspector Agent 和 V8 Inspector endpoint。

Renderer 可以请求顶层导航，但不拥有 browser-level network request、FrameTree/Target lifetime 或
Page replacement。

### Browser

Browser owner 拥有浏览器级状态和动作：

- FrameTree/BrowserContext/Target/Page lifetime；
- `NavigationRequest`、redirect、commit、replacement 和 history；
- network、cookie、storage partition 和 browser policy；
- renderer/Document candidate 选择与切换；
- 接收 renderer `BeginNavigation` 并执行真正的顶层导航。

### DevTools

DevTools 是跨 Browser 和 Renderer 的 control/observation plane，不一定对应独立 OS process：

- browser-side DevToolsAgentHost、DevToolsSession 和 Page/Target/Network handler；
- renderer-side Blink Inspector Agent、V8 Inspector 和 frontend channel；
- 根据 command 类型路由到 Browser 或 Renderer owner；
- 把两边产生的 response/fact 投影成 CDP event；
- 只在 Fetch interception、pause-on-start 等显式功能中持有 exact request permit。

三个典型流程说明权限边界：

```text
Page.navigate
  -> browser-side DevTools PageHandler
  -> Browser NavigationRequest
  -> commit renderer/Document

Runtime.evaluate
  -> DevToolsSession / renderer channel
  -> renderer-side V8Inspector
  -> current JS context executes

DOMContentLoaded
  -> renderer Document::FinishedParsing()
  -> renderer/browser instrumentation fact
  -> DevTools frontend projection
  -> CDP Page.lifecycleEvent
```

因此：

- DevTools 可以命令 Browser 导航，但不亲自执行导航；
- Renderer 可以产生导航意图，但不亲自管理顶层网络和 Page replacement；
- DevTools 可以观察 DCL，但 pending observer 不拥有 Browser navigation queue；
- renderer-side DevTools agent 物理上靠近 Renderer，也不等于 DevTools 获得 Renderer owner；
- Browser/Renderer 没有 frontend 订阅时仍然必须正常运行。

Chromium 实际还有 Network Service、Storage Service、GPU 等更多 service/process。本文采用三角色
模型是为了冻结 lifecycle/navigation 权限，不复制完整进程数量。

Moli 的长期映射是：

| Chromium 角色 | Moli 目标边界 |
| --- | --- |
| Browser owner | `moli-core` Browser Host/Page owner + fetch/storage/profile services |
| Renderer owner | `moli-renderer-v8` + DOM/parser/WebAPI runtime |
| DevTools control/observation | `moli` frontend actors + `moli-protocol` + renderer inspector endpoint |

本文真正要复制的是这三种责任边界，而不是 Chromium 的 Mojo 或多进程拓扑。

## 方案比较与决策

### 方案 A：继续强化 shared protocol scheduler

做法：保留所有 browser owner action 作为 `ProtocolSchedulerWork`，不断补 explicit predecessor、
priority 和 adapter guard。

判断：只适合作为短期 containment。它可以修复已知环路，但 protocol observation 和 browser action
仍共享 execution lane，未来 Fetch、termination、multi-session、writer backpressure 仍可能形成新
的隐式依赖。

### 方案 B：让 renderer 自己执行顶层导航

做法：`location.href` 在 renderer 内直接启动 network 并替换 Document，protocol 只观察。

判断：拒绝。renderer 不拥有 BrowserContext/Target/history/cookie/storage partition、跨 target route、
download 和 network policy。这样会复制 browser state，并让 command navigation 与页面 navigation
再次分叉。

### 方案 C：独立 Browser Owner，同进程 typed boundary

做法：Browser Core 成为唯一 browser execution owner；renderer 和所有 frontend 都向它提交 typed
command/intent，结果通过 outcome/fact 返回。初期同进程、同 executor。

判断：采用。这一方案直接修正 authority 和 lifetime，同时保留 Moli 的轻量资源目标，并为
未来 IPC 留出边界。

### 方案 D：立即复制 Chromium 多进程

做法：Browser、Renderer、DevTools 立即拆成多个 OS process 和 IPC channel。

判断：暂不采用。进程数量不会自动修复 owner 错位，反而先引入 serialization、crash recovery、
shared profile/service 和性能成本。等逻辑边界稳定后再按证据评估。

### 方案 E：每个协议各自持有 browser runtime

做法：CDP、BiDi、Classic 各保留一套 Page/navigation owner，只共享 renderer 或低层 helper。

判断：拒绝。同一个 served logical browser 的三个协议必须进入同一个 Browser Host，不能让每个
协议复制 owner。standalone `Browser` 是另一种 Browser Core deployment，不是第四个协议 owner；
它与 hosted 路径共享 renderer lifecycle/navigation contracts，但可以拥有不同的书面 completion
policy，例如 `FollowBeforeReply`。

## 目标架构

### 进程内逻辑结构

第一版目标保持同进程：

```text
moli application runtime
|
+-- BrowserHostActor (single owner, current-thread/local executor)
|   |
|   +-- BrowserCoreState
|   |   +-- BrowserContextRegistry
|   |   +-- TargetPageRegistry
|   |   +-- NavigationOwner
|   |   +-- active/background NavigationEngine
|   |   +-- Profile/Storage/NetworkPolicy owners
|   |   `-- Download/IO browser state
|   |
|   +-- BrowserOwnerQueue
|   +-- CommitParticipantRegistry
|   `-- BrowserFactJournal
|
+-- CDP frontend actor(s)
|   +-- socket/session/domain state
|   +-- DevTools renderer channel
|   +-- pending command registry
|   `-- CDP fact projector/output queue
|
+-- BiDi / Classic frontend actor(s)
|
`-- standalone Browser high-level facade (separate one-shot deployment)
```

Browser Host 通过 typed handle 接收命令。frontend 不获得 `&mut NavigationEngine`、`&mut Page` 或
可执行 owner action 的引用。最后一行不是 hosted Browser Host 的第四个 frontend；standalone
facade 自己就是一次调用级 Browser Core owner，并按高层 API 合法返回 `Page`。

### 两条逻辑队列的职责

`queue` 在这里首先是 execution lane / ownership mailbox，不承诺独立 OS thread。第一版可以让两者
在同一个 `moli-cdp-owner` current-thread runtime 上交替运行，但不得共享执行权限：

| 队列 | 唯一职责 | 可以持有 | 不得持有或决定 |
| --- | --- | --- | --- |
| Browser Owner queue | 执行并自主推进 browser action | BrowserContext、Target、Page、NavigationEngine、request/replacement/termination state、fact sequence | CDP session、command id、domain subscription、socket flush 状态 |
| CDP frontend queue | 提交 typed command，观察 fact，投影 CDP wire result/event | DevToolsSession、domain enable、command correlation、observer、output ordering | mutable Page/NavigationEngine、owner action、下一轮 navigation 是否可运行 |

两条输入路径必须在 Browser Owner queue 汇合：

```text
CDP Page.navigate --typed command--+
                                     +--> Browser Owner queue --> navigation request state machine
renderer location.href ----intent---+
```

Browser Owner 接受命令后自行运行到 accepted、commit、replacement、failure 等 terminal，并发布
带 exact identity 的 fact。CDP frontend 可以等待某个 terminal 来完成自己的 response，但这个
waiter 只是 observer；它的 pending、timeout、disconnect 和 output backpressure 都不能改变 Browser
Owner 是否继续运行。

只有 Fetch interception、`waitForDebuggerOnStart`、document-start DevTools attachment/commit
participant 等明确功能可以暂停 exact request。即使如此，request 和 pause state 仍由 Browser
Owner 拥有；frontend 只能持有和完成一个 request-scoped permit，不能获得通用 scheduler gate。

### 最终 ownership 矩阵

| 状态/行为 | 当前主要位置 | 最终 owner | 说明 |
| --- | --- | --- | --- |
| Browser instance lifetime | CDP socket stack / CLI call | served：application + Browser Host；standalone：一次调用级 Browser Core | served frontend detach 不隐式销毁 host；standalone drop 显式结束自身 instance |
| BrowserContext / Target / Page | `CdpConnection` | Browser Core | protocol 只保留投影和 subscription |
| `NavigationEngine` / retained engines | `CdpConnection` | Browser Core | 仍保持 thread affine |
| 页面 JS top-level navigation | renderer -> neutral Browser Owner input；application queue，physical projection adapter 暂在 protocol | Browser Owner queue | renderer intent 不含 session id，且不进入 protocol residence |
| `Page.navigate` / reload / history | CDP dispatcher + connection | Browser Owner queue | command 与 renderer intent 汇合 |
| Page replacement / termination | protocol owner action | Browser Core | exact generation 验证后执行一次 |
| cookie/storage/profile | connection + service merge | Browser Core / storage services | frontend 通过命令修改 policy |
| network request state | connection/domain/runtime 混合 | Browser Core/network runtime | protocol collector 是只读投影 |
| CDP session / domain enable | `CdpConnection` | CDP frontend | 永不进入 Browser Core |
| DevTools renderer channel / V8 cookie | protocol + renderer | protocol target agent host | 遵循现有 DevTools redesign |
| command id / response flush | CDP scheduler | CDP frontend actor | 不进入 Browser Owner queue identity |
| DCL/load 产生 | renderer | renderer exact Document owner | Browser Core 记录/转发 fact |
| lifecycle/network/target event shape | protocol call stack | protocol projector | browser fact 不含 CDP JSON |
| high-level fetch wait policy | standalone/core caller | adapter/wait policy | 只能观察，不能控制 owner progress |

### Browser target 与 DevTools target 的区别

现有 DevTools redesign 写“protocol target owner”，指 DevToolsAgentHost、frontend session、renderer
inspection channel 和 attachment route 的 owner。本文写“Browser Core owns Target/Page”，指浏览
上下文、页面执行、导航和 lifetime 的 owner。两者不是同一个概念：

```text
BrowserTarget
  -> owns Page/navigation/lifetime
  -> exposes renderer endpoint + browser facts

DevToolsTargetAgentHost projection
  -> refers to BrowserTargetId
  -> owns DevToolsSession[] and renderer inspection channel
  -> projects browser/renderer facts to subscribed frontends
```

Browser Core 不保存 `sessionId`；protocol target agent host 不执行浏览器 navigation。

## 必须成立的核心不变量

### Authority

1. 一个 browser action 只有一个 Browser Owner execution authority。
2. renderer 和 frontend 只能发布 intent/command，不能直接替换 current Page。
3. protocol subscription、pending observation 和 socket state 没有 Browser Owner queue 权限。
4. 有意暂停必须是 exact request-scoped permit，不能靠连接级布尔状态。

### Identity

1. action、completion 和 fact 都绑定 exact browser context、target、Page generation 和必要的
   navigation/Document identity。
2. old Page/request/renderer agent 的 late input 只能释放自身或 stale-drop。
3. frontend attach/detach 不改变 browser generation。
4. DevTools attachment identity 不替代 Browser Page identity，反之亦然。

### Progress

1. ready browser action 不依赖下一条 frontend command。
2. Browser Owner 不等待 WebSocket flush 或普通 observer terminal。
3. frontend command flood、renderer wake、network completion 之间有书面 fairness。
4. no subscriber、slow subscriber 和 disconnected subscriber 下 browser trace 一致。
5. Browser Owner 每轮处理完 command、renderer publication、task 或 microtask 后，只根据自己的
   request/intent/permit 状态决定是否继续；不得查询 CDP pending observation 来计算 done。

### Lifecycle

1. DCL/load 是 renderer exact Document fact。
2. Page replacement 原子切换 current residence，旧 Page 可异步清理。
3. navigation accepted、commit、DCL/load 和 high-level wait 是不同 terminal。
4. high-level waiter 可以跟随 successor，但不能阻止 successor 被创建。

### Projection

1. Browser Core 不构造 CDP/BiDi wire shape。
2. frontend 是否 enable domain 只影响 projection。
3. command response correlation 与 browser fact occurrence 分开。
4. slow projector 的 bounded failure 不能反压 Browser Host。

## Typed contract 设计

### Identity

优先复用并移动现有 exact identity，不制造另一套 generation truth。长期至少需要：

```text
BrowserInstanceId
BrowserContextId
BrowserTargetId
PageResidenceIdentity { target, slot_instance, loaded_page_generation }
NavigationRequestId
RendererAgentAttachmentId
RendererDocumentLifecycleIdentity
BrowserFactSequence
```

约束：

- 外部 CDP target id 可以保持现有 wire 值，但内部 owner identity 必须是 typed；
- `PageResidenceIdentity` 决定旧 Page action 是否 stale；
- `NavigationRequestId` 决定 redirect/auth/interception completion 是否仍属于 current request；
- `RendererDocumentLifecycleIdentity` 决定 DCL/load fact 的 Document 归属；
- `RendererAgentAttachmentId` 只服务 DevTools channel，不代替 browser Page identity；
- generation 不能因为 protocol attach/detach 递增。

如果 renderer 和 protocol 都需要某个 neutral identity，而 crate dependency 不允许放在
`moli-core`，只把最小 carrier 放进 `moli-page-types`。不能为了迁移让 renderer 反向依赖
`moli-core`。

### Browser command

工作形状：

```text
BrowserCommandEnvelope {
    browser_request_id,
    owner_identity,
    command,
    reply,
}

BrowserCommand =
    CreateBrowserContext
  | DeleteBrowserContext
  | CreateTarget
  | Navigate
  | Reload
  | TraverseHistory
  | CloseTarget
  | SetBrowserPolicy
  | ...
```

`browser_request_id` 是内部 correlation，不是 CDP command id。frontend 用 oneshot/ticket 将自己的
command id 与 Browser Core reply 关联，Browser Core 永远不解析 CDP/BiDi JSON。

command reply 只表达 owner-level 结果，例如：

```text
Accepted { navigation_id, loader_token }
Completed
TargetGone
StalePageResidence
NavigationRejected
InvalidInput
Canceled
```

协议适配器负责映射 CDP/BiDi error；standalone high-level facade 单独把 Core/renderer error 映射为 CLI status。

### Renderer intent

renderer 在 producing turn 边界冻结并移动 immutable intent：

```text
RendererBrowserIntent =
    TopLevelNavigate {
        page_residence,
        source_document,
        url,
        cause,
    }
  | TopLevelHistoryTraverse { ... }
  | OpenAuxiliaryTarget { ... }
  | RequestTargetTermination { ... }
  | ...
```

intent 不允许包含：

- CDP/BiDi session id；
- WebSocket command id；
- protocol subscription state；
- “下一条 frontend command 后执行”一类 ordering flag。

Browser Owner 接收后先验证 `page_residence`，再决定 accept、drop stale、merge same-document 或启动
replacement。renderer 不直接调用 network/history/target registry。

### Browser fact

第一阶段只迁移 navigation/lifecycle/target 必需事实：

```text
BrowserFactEnvelope {
    sequence,
    browser_instance,
    context,
    target,
    page_residence,
    fact,
}

BrowserFact =
    TargetCreated
  | NavigationAccepted
  | NavigationCommitted
  | NavigationFailed
  | PageReplaced
  | DocumentLifecycleReached { document, milestone, stamp }
  | DocumentLifecycleTerminated { document, last_reached, termination }
  | TargetCrashed
  | TargetClosed
```

这里的名称表达 Browser-owned transition，不要求复刻 CDP event 名称。当前 request state machine 中
`NavigationAccepted` 已经是唯一的 Browser navigation start commit；除非以后出现一个可独立失败、可由 Browser
观察的 network-start transition，否则不再为了对齐 `NavigationStarted` 名字发布重复事实。
`Target.targetInfoChanged` 更不能整体进入 Core：URL/title 等 Browser metadata、DevTools attached 状态和 frontend
discovery subscription 必须先拆开，只把确有 Browser authority 的 metadata transition 另立 neutral fact。`TargetCreated`
对应 Target topology commit，仍是 Phase 5 后续应补的 producer；它不能由现有 CDP discovery event 反向证明。

后续再按证据迁移 network/download/storage facts。不能先创建一个复制全部 CDP event 字段的巨型
`BrowserEvent`；那只是把 CDP JSON 换成 Rust enum，没有完成语义分层。

fact 必须：

- immutable；
- 带 exact source identity；
- 在 Browser Host 内分配单调 sequence；
- 不包含 protocol listener 判断；
- 不因没有 frontend subscriber 而省略 browser state transition；
- 可以被 trace、测试和多个 projector 重放。

不是所有 renderer/protocol 消息都要变成 Browser Fact：

- V8 Inspector command response；
- Runtime/Console/Debugger notification；
- renderer-local DOM/CSS/Accessibility command result；
- 大型 DOM snapshot、network body、download stream payload

继续由它们各自的 renderer/browser owner 和 DevTools renderer channel 传递。Browser Fact journal
只承载协议无关的 browser state transition。`BrowserFactSequence` 是 browser fact 的顺序，不是把
Inspector call response 和所有 transport message 强行排成一个全局序列；跨来源的 command/event
ordering 仍由 typed causal attachment 和 frontend actor 处理。

### Command reply 与 browser fact 分离

`Page.navigate` reply 和 DCL/load 不是同一 terminal：

```text
CDP Page.navigate
  -> BrowserCommand::Navigate
  -> NavigationAccepted
  -> CDP command response

later:
  response/commit
  -> NavigationCommitted fact
  -> renderer DCL fact
  -> CDP Page.frameNavigated / Page.lifecycleEvent
```

BiDi 的 `wait` 和 Classic page-load strategy 可以在 hosted command accepted 后附加各自 observer；
standalone CLI wait 绑定自己 deployment 中的 exact Document observer。两者都不能让 observer 获得
navigation execution authority。

## Browser Owner scheduler

### 单 owner，不阻塞等待

Browser Host actor 必须保持 run-to-completion 的短 turn。network、renderer、download 或 protocol
participant 的长等待不能直接 `await` 在 owner turn 中；应注册 exact pending operation，释放 turn，
由 completion input 再推进。

owner 输入至少包括：

```text
BrowserOwnerInput =
    FrontendCommand
  | RendererIntent
  | NavigationNetworkCompletion
  | CommitParticipantCompletion
  | RendererLifecycleFact
  | TargetLifetimeCompletion
  | OwnerWake
```

每个输入必须带 owner/request generation。late completion 只能完成自己的 pending operation，不能
扫描“当前 target”并修改新 Page。

### 自主 progress

一旦 action ready，Browser Owner 自己安排下一 turn。以下行为都不能成为 progress trigger：

- 下一条 CDP/BiDi command；
- frontend 读取 socket；
- `Runtime.evaluate("0")` / `Page.getNavigationHistory` 等 noop；
- lifecycle observer 注册/释放；
- benchmark heartbeat；
- 固定 sleep、polling 或反复 `yield_now`。

### 显式 predecessor

真正需要等待的 dependency 必须挂在具体 owner action/request 上：

```text
NavigationRequest N
  waits for response interception permit P
  waits for renderer commit attachment permit A
```

不能在 adapter 层增加：

```text
pending load observation precedes all browser actions
frontend command response precedes all browser actions
one slow subscriber precedes all facts
```

capacity constraint 也必须局部表达，例如“一个 adapter 同时只能附着一个 exact load waiter”或
“一个 target 同时只有一个 current cross-document navigation”，不能升级成全局 execution lock。

### Fairness 与优先级

本文不先冻结完整优先级表，但必须满足：

- 当前 owner turn 完成后才进入下一输入；
- exact terminal 与它完成的 request 原子结算；
- ready replacement/termination 不能被无关 observer 饿死；
- frontend command flood 不能永久饿死 renderer/network completion；
- renderer wake flood 不能永久饿死 explicit browser command；
- 同一 target 的冲突 navigation 按明确 request order/cancellation policy 处理；
- 不依赖 Tokio 偶然 poll 顺序证明协议语义。

Phase 1 trace 应先记录现有 input order，再通过行为回归冻结真正需要的 ordering，而不是复制当前
队列的所有 incidental order。

## Commit participant 与合法暂停

协议 observation 不得阻塞 Browser Owner，不代表协议永远不能影响 navigation。以下行为按浏览器
语义可以显式暂停特定 request：

- CDP Fetch request/response-stage interception；
- auth challenge decision；
- `waitForDebuggerOnStart`；
- final response commit 前的 DevTools renderer channel/session attachment；
- 用户显式 `Page.stopLoading`、cancel 或 close target。

统一模型应是 request-scoped permit：

```text
CommitParticipantPermit {
    navigation_request_id,
    participant_kind,
    owner_generation,
    resolution_policy,
    deadline/cancellation,
}
```

约束：

- permit 只能阻塞其绑定 request/stage；
- frontend disconnect 时按书面 policy `continue`、`cancel` 或 `fail`，不能留下 orphan permit；
- passive `Page.enable`、lifecycle subscription、DCL/load waiter 不创建 permit；
- DevTools session attachment 是 protocol agent-host 对 Browser Core commit barrier 的显式参与，不是
  protocol scheduler 获得整个 browser lane 的执行权；
- 没有 DevTools frontend 或无需 document-start 配置时，attachment participant 立即完成；
- deadline 是产品/协议 timeout，不用短 sleep 模拟 readiness。

这与现有 DevTools navigation redesign 的 `ResponseCommitReady -> ReadyToCommit -> renderer commit
permit` 对齐。Browser Core 拥有 request 和 barrier；protocol DevTools agent host 只完成自己的
participant。

## Browser fact journal 与 backpressure

### Journal

Browser Host 为已提交 fact 分配 sequence，并向订阅者提供 cursor。第一版可以是同进程 bounded
ring/fanout，不要求持久化。

用途：

- CDP/BiDi/Classic event projection；
- deterministic trace 和 differential test；
- frontend attach 后的有限 bootstrap/snapshot。

journal 不是第二份 browser mutable state。需要 current snapshot 的 command 仍向 Browser Host 请求
typed snapshot；不能从 event replay 猜当前 Page。

### 慢 frontend

慢 frontend 的策略必须是 bounded：

- frontend projector 有自己的 cursor/output queue；
- Browser Host 不等待 WebSocket flush；
- subscriber 落后超过 retention 时，frontend 明确 disconnect、报告 lag 或重新 snapshot；
- 不无限复制 DOM/network body；大 payload 使用已有 owner store/handle；
- command response correlation 由 frontend 自己维护，不让其它 subscriber backpressure 影响它。

CDP 对 command response 与紧邻 event 的 wire ordering 仍由 CDP actor 负责，但 actor 消费的是已经
发生的 facts/outcomes，不能反向冻结 Browser Host。

### 无 subscriber

没有 protocol subscriber 时：

- Browser Host 仍推进 navigation、renderer 和 target lifetime；
- 可以不构造昂贵的 protocol projection；
- 不能省略 state transition 或 exact lifecycle fact；
- retention policy 可以只保留诊断需要的最小窗口。

## 关键控制流

### CDP `Page.navigate`

```text
CDP frontend parses command
  -> validate frontend session/domain parameters
  -> resolve session route to BrowserTargetId
  -> send BrowserCommand::Navigate(target, url, cause)

Browser Owner turn
  -> validate target/page owner
  -> cancel/replace conflicting request by policy
  -> allocate NavigationRequestId/loader token
  -> install request
  -> reply NavigationAccepted
  -> launch network work

CDP frontend
  -> map accepted reply to Page.navigate response

later Browser Owner / renderer facts
  -> commit / frame navigation / DCL / load
  -> CDP projector emits events
```

`Page.navigate` response timing 不能偷偷变成“等 DCL”。如果 command 选择 wait，waiter 是独立 observer。

### renderer `location.href`

```text
renderer JS turn
  -> create immutable RendererBrowserIntent
  -> move intent at producing turn boundary
  -> notify Browser Owner

Browser Owner turn
  -> validate exact PageResidenceIdentity/source Document
  -> drop stale or install navigation
  -> launch network work
  -> publish browser facts
```

该路径不解析 session route，也不经过 protocol residence。CDP frontend 是否连接只影响 facts 是否
被投影。

### DCL/load

```text
renderer exact Document task
  -> renderer lifecycle terminal(document X, milestone)
  -> Browser Host records fact(sequence, target, page, document X)
  -> each frontend projector checks its own subscription
  -> emit CDP/BiDi/Classic shape
  -> high-level wait evaluates its policy
```

DCL/load observation 没有 navigation queue 权限。若 A 已经 DCL 后请求 B，可以依次发布
`DCL_A`、replacement、`DCL_B`；高层 wait 是否跟随 B 是产品 policy。

### Page replacement

```text
NavigationRequest N wins
  -> validate N is current
  -> create/commit successor PageResidence B
  -> atomically mark A retired and B current
  -> publish PageReplaced(A, B, N)
  -> old A cleanup may finish later
```

旧 A 的 intent、network completion、timer/lifecycle wake 和 DevTools attachment output 必须按 exact
identity 变成 stale。不能要求先析构 A 才允许 B 成为 current。

Phase 2 迁移期曾允许 physical `Page` 暂时留在 protocol slot，但只能使用下面的 single-turn bridge：

```text
Browser Core prepare exact replacement permit
  -> protocol prepares renderer/DevTools participant (may fail)
  -> Browser Core commits request + Page generation + history
  -> protocol synchronously projects the physical successor Page
  -> old Page closes asynchronously
```

core commit 与 physical projection 之间禁止 `await`、frontend flush、observer callback 或 fallible
participant work。Phase 6 的 physical Page runtime owner 模块已 supersede 这条迁移形状：Core transaction 直接发布
move-owned Page payload owner，Protocol slot 只持 non-owning access；Target `sessionStorage` namespace 也已进入 exact
Target registry。剩余 physical Target/frontend projection 拆分仍不得把旧 bridge 扩展成另一套 replacement owner。

### Target termination

`Page.crash`、`Page.close`、`Target.closeTarget` 和 BrowserContext disposal 必须汇合到同一个
Browser-owned Target transaction，而不是分别从 session route 清理一组 protocol 字段：

```text
capture {context, target, exact Page residence, crash|close}
  -> Browser Core prepare exact permit
  -> protocol prepares Inspector/session projection inputs
  -> Browser Core commits Page generation + request/runtime/history/Target state
  -> protocol synchronously projects physical Page/Target absence
  -> retired Page close / active-target promotion may await
  -> frontend projects response, detach and Target/Inspector events
```

和 Page replacement 一样，从 core commit 到 physical absence projection 之间禁止 `await`、frontend
flush 或 callback。迁移期 protocol projection 可以收集 session/event 所需数据，但不能重新判断
termination authority，也不能再次推进 Page generation。

Crash 不能建模成 Close：crashed Target 仍可查询、关闭或通过新 navigation 恢复。Browser Core 至少要
区分 `Crashed` 与 `Recovering(exact BrowserDocumentNavigation)`：重复 crash 被拒绝；只有那条 exact
recovery request 可以安装 successor Page；失败或被替代的 recovery 回到 `Crashed`；成功后 Target
重新成为可 crash 的 live 状态。`Target.closeTarget` 在 `Crashed` 状态仍必须可用。Close 自身也必须
one-shot；迁移期即使 protocol 物理 slot 尚未删除，Core 的 `Closed` gate 也要独立拒绝第二次 capture。

termination action payload 不得保存 `CommandOwnerScope`、`sessionId` 或发命令的 frontend lifetime。
frontend detach/reattach 只影响最终 response/event 投影，不能取消已经 capture 的 Browser action；反过来，
旧 Page generation 的 delayed action 必须 stale-drop，不能关闭 replacement Page。

### frontend disconnect

```text
frontend disconnect
  -> cancel that frontend's pending command replies
  -> remove subscriptions/projector cursor
  -> resolve its explicit interception/debug permits by policy
  -> detach DevTools sessions
  -> Browser Host continues
```

WebDriver session 或 incognito context 若声明“frontend owns context”，adapter 应发送显式
`DeleteBrowserContext`；不能依赖 drop `CdpConnection` 顺带销毁 browser state。

### browser close

browser close 是 Browser Host command/lifecycle，不是 socket close 的别名：

```text
BrowserHostClose
  -> stop accepting new commands
  -> retire contexts/targets/pages
  -> resolve pending browser operations
  -> publish terminal facts
  -> detach frontends
  -> stop renderer/network owners
```

## 高层 wait 与 `done`

Browser Core 分离不改变 DCL/load 的 exact-Document 语义。hosted wait 观察 Browser fact journal，standalone
wait 消费 renderer/Core 的 typed exact-Document terminal；两者保持同一个终止门禁：

```text
observe milestone for bound Document
  -> evaluate typed replacement/navigation outcome
  -> if policy follows replacement, bind successor
  -> otherwise complete
```

必须保持：

- DCL 不等待未来 timer；
- `Done` 在产品另行决策前仍是现有兼容语义；
- `domstable`/selector/script wait 可以继续驱动页面，但每个 bounded turn 后先观察 navigation；
- waiter timeout 只结束 waiter；
- waiter cancellation 不取消已 accepted navigation，除非调用方发送显式 stop/cancel command。

分离后的收益是 hosted frontend 读取统一 Browser fact trace；standalone high-level policy 复用同一
exact Document、typed navigation handoff 与 replacement contract。两者都不再依赖 frontend noop 或
无类型 pending-navigation 扫描，但不要求共享同一个 journal instance。

## Crate 与模块边界

### `moli-core`

新增或逐步形成以下工作模块：

```text
moli-core/src/browser_host/
  mod.rs
  identity.rs
  page_residence.rs
  target_handle.rs
  owner_input.rs
  actor.rs
  handle.rs
  turn.rs
  command.rs
  outcome.rs
  fact.rs
  journal.rs
  commit_participant.rs
  navigation_owner.rs
  navigation_owner/
    types.rs
    context_registry.rs
    target_registry.rs
    target_transaction.rs
    engine_registry.rs
    page_runtime.rs
    document_build.rs
    request.rs
    request_registry.rs
    history.rs
    history_registry.rs
    page_registry.rs
    page_replacement.rs
    target_termination.rs
```

职责：

- Browser Host single-owner state；
- typed command/intent/outcome/fact；
- context/target/page/navigation lifetime；
- application-facing handle；
- protocol-neutral snapshot；
- exact commit participant registry。

现有 `runtime/navigation_engine.rs`、Page orchestration 和 storage/profile service 可以先被新 owner
封装，再逐项移动；不能一次机械搬完整目录。

### `moli-page-types`

只有 renderer、core 和 protocol 都必须共享的最小 neutral identity/carrier 才放这里。禁止加入：

- CDP session/command id；
- protocol event/error shape；
- Browser Host mutable registry；
- V8 handle 或 executor/channel。

### `moli-renderer-v8`

继续拥有：

- V8/DOM/parser/task/microtask/timer；
- exact Document lifecycle；
- renderer DevTools agent endpoint；
- immutable renderer browser intent producer。

不新增对 `moli-core` 的 production dependency，不直接获取 Browser Host mutable state。

### `moli-protocol`

Phase 2 迁移期的 replacement/termination 代码也必须按边界拆开，不能重新堆回 domain handler 或
`CdpConnection` 主文件：

```text
moli-protocol/src/conn/
  browser_owner_input.rs                       # Browser Host publication adapter
  browser_host_turn_executor.rs                # short start + exact participant completion adapter
  browser_page_replacement.rs                 # core transaction adapter
  browser_target_termination.rs               # core transaction adapter
  browser_context/
    loaded_page_projection.rs                 # physical replacement projection
    target_termination_projection.rs          # physical termination projection

moli-protocol/src/domains/page/
  navigation.rs                              # navigation admission/start and legacy materialized drain adapter
  navigation_completion.rs                   # exact load/configure/commit/replay participant state machine
  navigation_commit.rs                       # physical loaded Page commit projection
  navigation_tail.rs                         # renderer finish/replay start-wait-apply boundary
```

application 侧的 mailbox 与其他 owner inputs 独立放在：

```text
moli/src/cdp_scheduler/
  owner_inputs.rs                              # Host execution lane + input/completion residences
```

adapter 只负责 core permit/commit、participant 顺序和迁移期 async cleanup；projection 只修改
active/background physical Target/Page 与 DevTools attachment 状态；domain handler 只保留 command 参数、
pending response correlation 和 CDP event shape。任一层都不能复制 core request/history/generation/Target
lifecycle registry。

迁移后保留：

- DevToolsSession / renderer channel / V8 state cookie；
- target/session route projection；
- domain enable/subscription；
- DevTools command/result facade 的协议语义；
- browser fact -> DevTools/CDP-neutral event projection；
- protocol-specific interception/debug participant adapter。

逐步移出：

- strong `NavigationEngine` owner；
- authoritative BrowserContext/Target/Page mutable state；
- browser navigation/history/termination execution；
- protocol scheduler 中的 browser owner action。

`CdpConnection` 可以在迁移中先变成：

```text
CdpConnection {
    browser_host: BrowserHostHandle,
    frontend_state: ...,
    devtools_agent_hosts: ...,
    pending_commands: ...,
    projectors: ...,
}
```

最终是否重命名为 `DevToolsFrontendState` 或拆成多个结构，在 ownership 真正移动后决定；不能只重命名
结构而保留全部 browser state。

### `moli`

应用层负责：

- 启动和关闭 Browser Host runtime；
- 为 raw CDP/BiDi/Classic 提供 owner endpoint 或 typed service handle；
- 为 CLI/MCP 创建一次调用级 standalone `Browser` Core facade；
- 选择 host lifetime/profile；
- 启动各 protocol frontend actor；
- 不重新实现 Page owner loop。

raw CDP `CdpScheduler` 长期只负责 frontend command/pending response/output ordering 和 fact projection
drain，不再负责 Browser Owner action selection。

### 暂不新增 crate

现有 dependency direction 已允许 `moli-protocol -> moli-core`。第一轮不要新增
`moli-browser-core` crate。只有满足以下条件才重新评估：

- `moli-core` 因 Browser Host API 产生明确、持续的依赖膨胀；
- neutral runtime 可以在不依赖 renderer/protocol 的情况下独立编译；
- 新 crate 能删除依赖环或显著缩短 build，而不是只移动文件。

## 分阶段实施

每个 phase 可以拆成多个小 PR，但一个 PR 只能有一个 execution authority。迁移期允许 adapter，
不允许 old/new owner 同时可能执行同一 action。

### Phase 0：冻结基线和名词

状态：已完成架构 exit gate；扩展性能基线继续作为 benchmark backlog，不阻塞 owner 分离。

已完成：

- exact Document lifecycle identity 和 typed replacement terminal；
- renderer navigation intent 在 producing turn 冻结/移动；
- passive CDP navigation progress 回归；
- pending load global guard 的短期 containment；
- Chromium browser/renderer/DevTools ownership 调研；
- `CdpConnection` 第一轮字段组 owner inventory；
- production navigation trace 第一段：renderer/frontend action、Core request、response commit-ready、Page
  replacement、renderer lifecycle observation 和 frontend projection 已用同一 correlation key 串通。
- schema v1 JSONL transport、CLI/CDP/BiDi/Classic 同源 exact-Document fixture，以及固定 release binary 的
  10 轮 short-navigation latency/PSS/RSS/sampled-CPU 第一版基线。
- Browser fact journal 已加入 monotonic fact sequence，Target/navigation/lifecycle producer inventory 已完成。

非阻塞后续：

- 增加 Chromium machine differential；
- 补 event-heavy、多 Target、idle footprint 基线，并为 sub-100ms CLI workload 提供比 1--2 个 sample 更稳定的
  CPU/peak-memory 证据。

Exit gate：文档和 trace 能回答一个 navigation 的 source、owner、request、Page generation、Document、
执行 turn 和 frontend projection sequence。

2026-08-06 final audit：production trace、Core fact sequence 和 exact frontend projection 已能回答上述
问题，因此 exit gate 通过。Chromium machine differential、event-heavy、多 Target、idle footprint 与短进程
CPU/peak-memory 增强仍有产品价值，但它们验证性能和兼容面，不再作为 Browser/CDP authority 拆分的前置条件。

### Phase 1：neutral identity 与 renderer intent 去 session 化

状态：已完成。

目标：先清掉 action 对 frontend route 的依赖，不改 scheduler/lifetime。

工作：

1. 把 `TargetPageResidenceIdentity` 中浏览器语义部分迁到 neutral owner identity；
2. renderer-produced top-level navigation action 不再保存 `CommandOwnerScope/sessionId`；
3. 使用 `{context, target, page residence}` 直接定位 browser Page owner；
4. protocol route 只在输出 projector/DevTools attachment 侧使用；
5. 增加 park/promote、auxiliary session、detached session 下的 stale/action regression。

迁移适配期仍可由 `CdpConnection` 调用执行方法，但方法签名必须不再需要 frontend session 才能找到
Page。

Exit gate：同一个 renderer intent 在无 session、primary session、auxiliary session 和 session
reattach 下指向同一个 BrowserTarget/Page；detach 不能改变是否执行。

2026-08-02 实现记录：

- `PageResidenceIdentity` 已迁到 `moli-core::browser_host`，内部使用 typed
  `BrowserContextId` / `BrowserTargetId`；protocol 中的 `TargetPageResidenceIdentity` 只保留迁移别名；
- `TopLevelLocationNavigationOwnerAction` 已删除 `CommandOwnerScope` 和 `sessionId`，payload 只保留
  exact Page residence、source Document 和 navigation intent；
- owner lookup 只根据 `{browser context, target, slot instance, loaded_page_generation}` 选择 active/background
  Page，frontend detach/reattach 不参与 authorization；
- legacy session-owner navigation API 暂时通过无 session 的 exact owner route adapter 调用；该 adapter
  随 Phase 2 owner seam 一起删除，不能重新进入 action payload；
- primary reattach、background auxiliary detach 和 stale Page generation 均有直接回归，完整 CDP 与
  WebDriver Classic/BiDi/Selenium release smoke 通过。

### Phase 2：提取 Browser navigation owner state

状态：已完成。BrowserContext authoritative identity/selection registry、Target authoritative
identity/context membership/active-background topology registry、stable Target instance handle/lifecycle
capability、strong exact Page-slot capability registry、active/background strong engine registry、typed operation
facade、target joint history registry、cross-document request lifecycle registry、loaded Page replacement、Target
termination 的 authoritative commit transaction、initial-empty-Document/Target creation metadata，以及
selected/retained engine 的完整 target-keyed identity/handoff 已进入 core owner。第十六切片已关闭最后一条
Page generation mutation authority：initial Page materialization 与 failed-navigation Page discard 也改为
Core prepare/commit、Protocol 同 turn 同步投影。release build 中 Protocol 不再能推进或任意设置 Page
generation，本阶段 Exit gate 已通过。

物理 BrowserContext/Target/Page payload 在本阶段可以继续按“Core commit 后、同一 actor turn 同步投影”的
bridge 暂存 Protocol；是否搬走这些 frontend/renderer payload 属于后续 lifetime 提取。自主 Browser Owner
queue 与 Browser fact journal 分别是 Phase 3 和 Phase 5 的工作，也不应倒过来成为 Phase 2 的退出条件。

目标：先形成可以被独立 actor 拥有的 state seam。

工作：

1. 在 `moli-core` 形成 `BrowserPageOwner` / `BrowserNavigationOwner` 工作结构；
2. 移入 active/background `NavigationEngine` 的 ownership 和 exact Page lookup；
3. 移入 navigation request、replacement、history 和 termination 的 owner-level API；
4. `CdpConnection` 只通过 facade 调用，不直接访问 engine；
5. 现有 protocol event 先通过 adapter 返回，行为不变。

这一阶段不急于改 WebSocket lifetime，也不创建第二条 actor。目的是先让 state 边界可编译、可测试。

Exit gate：Browser Core 拥有 keyed engine 以及 authoritative BrowserContext/Target/Page mutable registry；
`CdpConnection` 不再直接持有或替换 engine，也不再决定 Target/Page lifetime。owner-level API 不接受
protocol event buffer、session id 或 CDP command shape。

2026-08-02 第一切片实现记录：

- `moli-core::browser_host::BrowserNavigationOwner` 成为 active engine 与 keyed retained
  background engine 的唯一 strong registry，key 使用 protocol-neutral `BrowserPageOwnerKey`；
- active replacement、background retain/take/forget 和 renderer publication sender 向新旧 engine 的
  传播由该 owner 执行；`CdpConnection` 不再保存 engine/map 字段，也不再把 renderer publication sender
  保存为 protocol scheduler hook；
- context/target park、promote 和 diagnostics 已改为调用 owner registry；
- 为保持本切片行为不变，旧 navigation/build/resource-runtime 方法仍通过明确命名的
  `active_engine_for_migration[_mut]` bridge 借用 engine。该 bridge 只允许减少调用方；删除 bridge、把
  navigation/replacement/history/termination 以及 exact Page lookup 变成无 session owner operation，才
  能把 Phase 2 标成完成。

2026-08-02 第二切片实现记录：

- `BrowserPageOwner` 已封装 active/background `NavigationEngine`，协议侧的
  `active_engine_for_migration[_mut]` bridge 已删除；`CdpConnection` 的 active document build、fetch、
  resource-runtime、lifecycle 和 diagnostics 路径只调用 `BrowserNavigationOwner` 的 typed operation；
- core owner 按责任拆为 `navigation_owner.rs` registry、`navigation_owner/types.rs` neutral input/key、
  `navigation_owner/page_runtime.rs` 页面运行操作和 `navigation_owner/document_build.rs` 文档构建操作，
  避免把迁移期 facade 继续堆进一个大模块；
- protocol-only 的 `TargetNavigationLoadInputs` 展开与 active/detached builder 选择留在独立
  `conn/browser_document_page_builder.rs` adapter，Browser Core 不依赖 protocol command/session/event
  类型；detached background job 仍临时持有自己的 engine，active owner 不再向 protocol 暴露 engine
  引用；
- engine install/take、detached background job、exact Page lookup，以及
  navigation/replacement/history/termination 的 execution authority 仍在 `CdpConnection` 路径中。下一
  切片必须继续把这些整体 owner operation 移入 core；本记录不表示 Phase 2 已完成。

2026-08-02 第三切片实现记录：

- browser navigation request identity 已移入独立的
  `moli-core::browser_host::navigation_owner::request` 模块，形成 protocol-neutral
  `BrowserNavigationRequestId` 和 `BrowserDocumentNavigation`；它只携带 typed target、loader 和
  browser request identity，不再携带 `sessionId` 或 CDP command identity；
- protocol 的 `DocumentNavigationToken` 暂时只是 core 类型的迁移别名。pending/committed navigation、
  renderer Page binding 和 late-completion authorization 因而比较同一个 browser-owned request，不能
  因 frontend detach/reattach 改变；
- background protocol gate 需要的 frontend session correlation 继续从
  `NavigationDispatchState` 派生并留在 `conn/scheduler_state.rs`，没有回填到 browser request；直接回归
  覆盖 event session 优先、command session fallback 以及二者共享同一 browser request identity；
- 本切片只迁移 request identity，没有宣称 protocol 已失去 navigation execution authority。exact Page
  registry/lookup、history、replacement、termination 和 request install/cancel 仍需按各自模块继续移入
  Browser Core，不能把这些职责重新集中到 `request.rs` 或 `CdpConnection` adapter。

2026-08-02 第四切片实现记录：

- joint session history 的 entries、cursor、entry/document sequence allocator、pending replace/traverse 和
  same-document update 算法已整体移入独立的
  `moli-core::browser_host::navigation_owner::history` 模块；原 protocol
  `conn/state/navigation.rs` 只保留公开 API 的兼容别名，不再实现或复制一份 history 状态机；
- history owner API 只接收 browser history entry、entry id 和 renderer 的
  `SameDocumentHistoryUpdate`，不接收 session、CDP command 或 event buffer；原有同文档 traversal、reload
  replace、forward pruning、document sequence 判定继续由 core 单元测试和 protocol 行为测试覆盖；
- 当前 core history 值仍作为迁移字段嵌在 protocol `TargetOwnerState` 中，读取也仍通过 session-route
  adapter 定位 target。只有 exact Page/Target registry 进入 `BrowserNavigationOwner`、history command 和
  renderer update 都通过 typed owner operation 后，execution authority 才算完全移出 protocol；
- 下一切片优先提取 exact Page lookup/state residence，随后再把 history 值随 Page owner 一起迁移。不得为
  提前宣称完成而在 core 新建一份 target/history mirror registry。

2026-08-02 第五切片实现记录：

- exact Page identity 被拆成两个独立 core 模块：
  `browser_host/page_residence.rs` 只拥有稳定 slot instance、generation、handle 与 identity；
  `browser_host/navigation_owner/page_registry.rs` 只负责从 protocol-neutral
  `BrowserPageOwnerKey` 弱索引到物理 slot，并完成 capture/resolve。主 `navigation_owner.rs` 只组合
  registry，不承载 Page 身份算法；
- registry 不复制 generation、alive 或 active/background 状态。authority 始终在
  `BrowserPageResidenceHandle` 背后的 core state：安装、替换或退休一个 renderer `Page` 会推进同一
  handle 的 generation；复用物理 active slot 给另一个 Target 时会分配新 handle instance；slot 被释放后
  weak registration 自动失效；Target termination 也通过 core `forget_target` 立即移除 lookup index，
  即使 protocol 暂时还持有关闭中的 slot，也不会延长 browser authority。两者都不需要 protocol
  teardown flag 或第二份 generation map；
- `TargetPageSlot` 当前仍是迁移期的物理容器，但只持有 core handle，不再拥有裸
  `loaded_page_generation`。target park/promote 搬运整个 runtime slot，也就搬运同一个 capability；
  `document.open()` 只替换同一 Page 内的 Document，不替换 slot，所以不会错误推进 Page residence；
- frontend session 只允许在 capture 起点解析一次 `{browser context, target}`。之后的 exact lookup 先由
  `BrowserNavigationOwner` 验证 instance + generation，再由 protocol adapter 把已经授权的
  `BrowserPageOwnerKey` 投影成临时 active/background route；detach/reattach 不参与 browser action
  identity，也不能让旧 action 跟随 replacement Page；
- renderer Page 输出路由与 current Page action identity 明确分开：尚未 commit 的 Page stream 绑定
  `BrowserPageOwnerKey + RendererPageResidenceIdentity`，不能借用旧 current Page 的 generation；输出
  ingress 通过 exact renderer Page reservation/installation 路由，Browser action 才通过 exact current
  `PageResidenceIdentity` 授权。这样 replacement 推进 generation 后，新 Page stream 不会被误判 stale，
  旧 stream 也不能因为 target key 相同而跟随 replacement；
- live browser work 必须从 handle capture identity。保留的 `PageResidenceIdentity::new` 与
  `set_generation_for_migration` 仅服务尚未迁完的测试 fixture；合成 identity 没有 slot instance
  capability，不能通过 core owner lookup，后续应随 fixture 收口删除；
- core 单测覆盖 successor generation、同 key slot replacement 与 slot drop；protocol 回归覆盖 frontend
  session churn、background auxiliary detach、隐式 active target replacement，以及真实 Page
  install/replacement/retirement 自动推进 generation。exact residence authorization 已迁入 core，但
  `Page` 对象及 navigation/replacement/termination 的执行动作仍由 protocol target slot 路径触发，下一
  切片不能把本记录误读为 Phase 2 已完成。
- 本切片最终验证为 core/protocol 全量 nextest `5703 passed, 13 skipped`、workspace clippy、release
  build、清代理默认 CDP smoke `210/210` 和 WebDriver Classic/BiDi/Selenium smoke `59/59`。旧 fixture
  暴露的 synthetic identity 均改为从真实 slot capture；相关失败集额外进行 10 轮 stress，未通过放宽
  stale authorization 或 retry 隐藏失败。

2026-08-02 第六切片实现记录：

- joint session history 的**值 ownership** 已从 protocol `TargetOwnerState` 移出，成为
  `BrowserNavigationOwner` 内按 `BrowserPageOwnerKey { context, target }` 索引的唯一 authoritative
  registry；protocol 不再保存 entries、cursor、entry/document allocator 或 pending replace/traverse，
  active/background park/promote 也不再搬运或清理 history 值；
- 模块继续按责任拆分：`navigation_owner/history.rs` 只实现 history 值类型与 cursor/update 算法，
  `navigation_owner/history_registry.rs` 只实现 Target lookup、lazy seed 和 lifetime，protocol 新增的
  `conn/browser_navigation_history.rs` 只把 frontend session route 一次解析成 neutral target key，并投影
  core snapshot；主 `navigation_owner.rs` 只组合子 registry，没有吸收 history command 细节；
- core 新增 `BrowserNavigationHistoryPageSnapshot` / `BrowserNavigationHistorySeed` typed input。迁移期
  protocol 可以提供 initial empty Document 或当前 Page 的 URL/title，但 entry id、transition 处理和
  document sequence 只由 core 分配；`Page.getNavigationHistory`、reset、reload/replace、traversal、
  loaded commit 与 renderer same-document update 全部调用同一 core owner operation；
- Target lifetime 明确区分三种动作：park/promote 不碰 history；navigation failure 只
  `discard_target_page_runtime`，保留 joint history；crash 退役 Page runtime 并在 core 留下显式 empty
  history tombstone，防止尚存的 protocol initial-document metadata 把 `about:blank` 重新 seed；真正 Target
  close/rollback 才 `forget_target` 并删除 history registry entry；
- 本条记录的是第六切片当时的迁移形状：initial empty Document 曾由 protocol Target staging 保存 immutable
  seed metadata，history 第一次被查询、same-document 更新或 loaded commit 时由 core lazy materialize。
  第十五切片已经 supersede 这条 bridge：creation seed 与 lifecycle 现由 Core registry 唯一持有，Protocol
  不能再反向提供 initial-Document history seed；
- core registry/算法与 protocol adapter 的 history 聚焦集共 `94/94` 通过，覆盖 initial seed、pending
  replace、same-document traversal、reset、background/inactive target、park/promote、crash 不重 seed、
  failed-navigation 保留 history；core/protocol 全量 nextest `5710 passed, 13 skipped`、workspace
  all-target clippy、release build 均通过；明确使用 `target/release/moli` 且清空大小写 proxy/no-proxy
  环境变量的外部 smoke 为 CDP `210/210`、WebDriver Classic/BiDi/Selenium `148/148`。

2026-08-02 第七切片实现记录：

- cross-document navigation 的 pending/committed request lifecycle 已成为
  `BrowserNavigationOwner` 内按 `BrowserPageOwnerKey` 索引的唯一 authoritative registry。新的
  `navigation_owner/request_registry.rs` 只实现 start、exact pending/body/committed acceptance、commit、
  failed-pending clear 和 Target retirement；`navigation_owner/request.rs` 继续只保存不可变 request id、
  target 与 loader identity，主 `navigation_owner.rs` 只组合 registry，不吸收状态机细节；
- protocol 新增独立 `conn/browser_document_navigation.rs` adapter。它只在入口把 frontend
  session/target route 解析成 neutral owner key，随后调用 core typed operation，并把结果投影到 renderer
  attachment、initial-empty-document metadata、resource publication 与 protocol event；它不保存第二份
  pending/committed request，也不让 session detach/reattach 改变 request authority；
- `TargetPageSlot` / `TargetRuntimeSlot` / `BrowserContext` 已删除 pending/committed navigation token、
  current/committed loader 与 late-completion acceptance 状态。Page slot 只保留 pending renderer Page
  reservation 和 exact-Document lifecycle projection；`DevToolsRendererChannel` 只保留 Inspector
  attachment lease、输出 suspend/resume 和 projection diagnostics，不能决定 browser request 是否仍为
  current；
- DCL/load 直接受益于权威状态收口，但语义没有被改成跨 Document：renderer 仍为每个真实 Document
  产生 exact lifecycle fact，protocol 只有在 lifecycle binding 的 optional request token 仍等于 Browser
  Core committed request 时才允许观察、注册 waiter 或结算 root load/network-idle。403 Document 的 DCL
  仍是真的；如果页面随后触发 replacement，是否继续跟随由高层 wait/done 和 Browser Owner progress
  决定，不能靠把前一个 Document 的 DCL 判假；
- navigation background event、document body completion、Fetch response-head candidate commit 与
  lifecycle waiter 都通过同一 core request identity gate。交错 response head 仍能区分“被更新 request
  supersede”和普通 non-pending，同时 superseded candidate 不能切换 renderer attachment；
- failed navigation、Target crash 和 Target close 分别通过 core discard/forget lifetime operation 清理
  request registry；park/promote 只改变 protocol residence，不改变 request authority。core 单测覆盖
  supersede、late committed body、failed-pending restore 与 Target discard；protocol 回归额外证明清除
  renderer projection 不能反向清除 Browser Core pending request。
- 本切片聚焦验证为 core request registry `4/4`、跨 core/protocol request/lifecycle/Fetch 集合
  `15/15`；core/protocol 全量 nextest `5712/5712`，workspace 全量 nextest `15516/15516`、
  `17 skipped`，workspace all-target clippy、fmt check 和 release build 均通过；显式清除大小写
  proxy/no-proxy 环境变量并固定 `target/release/moli` 的外部 smoke 为 CDP `210/210`、
  WebDriver Classic/BiDi/Selenium `148/148`。验证中发现一个既有 direct-command 测试会丢弃可选
  renderer fence：dispatch 组基线 5 轮失败 2 轮；改为完整消费 command output 后，121-test dispatch
  组连续 10 轮全部通过，该测试同步修复独立提交，没有混入 Browser Core 状态机。

本切片完成了 Phase 2 的 request lifecycle authority 迁移，但不表示完整 navigation action、Page
replacement 或 Target termination action 已离开 `CdpConnection`。下一切片应迁移一个完整 owner
operation，不得建立第二份 protocol request/history cache，也不得把 replacement 或 termination 逻辑堆入
`request_registry.rs`。

2026-08-02 第八切片实现记录：

- loaded cross-document Page replacement 的 owner transaction 已进入独立的
  `moli-core::browser_host::navigation_owner::page_replacement` 模块。prepare 阶段同时验证 exact
  pending `BrowserDocumentNavigation` 和 exact physical Page residence，但不修改 request、history 或
  generation；commit 阶段在一次 `&mut BrowserNavigationOwner` operation 中推进共享 Page generation、
  提交同一个 request，并记录 joint history。permit/result 不携带 frontend session、CDP command 或 event
  buffer；request、history 和 Page registry 继续留在各自模块，主 owner 只组合它们；
- protocol 也按两层拆分：`conn/browser_page_replacement.rs` 只编排 core prepare/commit 与 participant；
  `conn/browser_context/loaded_page_projection.rs` 只处理 active/background Target 的 renderer attachment、
  target metadata 和物理 Page slot 投影。`page_state/loaded.rs` 保留 DevTools renderer participant 细节，
  没有把这些 protocol 类型反向放入 core；
- 当前迁移期顺序被冻结为：core authorization → renderer/DevTools participant prepare → core owner commit
  → 同一 actor turn 内同步投影 successor Page → 异步关闭旧 Page。从 core commit 到物理 slot swap 之间
  没有 `await`、外部 callback 或 frontend flush；旧 Page 的 teardown 只有在 target URL、initial-empty
  state、session/document projection、network cursor 和 current slot 都切到 successor 后才开始；
- `TargetPageSlot` 新增的投影入口必须用 core 返回的 successor `PageResidenceIdentity` 证明共享 handle
  已经是 current，并且不会再次推进 generation。普通 install/clear/termination 路径仍使用原入口自行
  推进 generation，避免在 termination 尚未迁移前削弱其 stale-work 防护；直接回归证明一次 replacement
  只推进一次 generation，并覆盖错误 physical slot、superseded request、background route 和迟到 initial
  Page build；
- renderer/DevTools participant 可能已经为 response-ready candidate 提交 attachment，随后 request 又被
  更新 navigation supersede。此时 core prepare 会拒绝旧 replacement，protocol adapter 只允许回滚与该
  candidate transaction 完全相同、且仍为 current 的 attachment，再关闭 candidate Page；它不能回滚更新
  request 的 attachment，也不能反向修改 core request/history/generation。该补偿逻辑归
  `conn/browser_page_replacement.rs`，精确 attachment 状态机仍归 `DevToolsRendererChannel`，没有塞进
  Target route projection 或 core transaction；
- DCL/load 因而在 lifecycle binding 前已经看到 committed browser request 和 successor Page
  generation。旧 Document 的 DCL 仍是真实 exact-Document fact；replacement 只让旧 Page/Document 的后续
  completion 按 exact identity stale-drop，不把 403 DCL 改成“假事件”，也不把 high-level successor-follow
  policy 塞进 protocol slot；
- production loaded-commit 路径不再分别执行“替换 Page、更新 target identity、提交 request、记录
  history”四个可分离动作。当前仍是迁移边界而非最终原子内存布局：物理 `Page`、Target metadata 和
  renderer attachment 暂留 protocol projection，且尚未发布 `PageReplaced` browser fact。Phase 2 下一
  切片应迁移 Target termination owner operation；Phase 3/5 再建立自主 Browser Owner queue 与 fact
  journal，不能把这次同步 adapter 当成最终 scheduler。

本切片最终聚焦验证为 Page replacement core/protocol 与 supersede rollback 集合 `12/12`；
core/protocol 全量 nextest `5718/5718`、`13 skipped`，workspace 全量 nextest `15522/15522`、
`17 skipped`，workspace fmt check、all-target clippy 和 release workspace build 均通过。显式清除大小写
proxy 变量、设置 `NO_PROXY=*` / `no_proxy=*` 并固定 `target/release/moli` 的外部 smoke 为 CDP
`210/210`、WebDriver Classic/BiDi/Selenium `148/148`。

2026-08-02 第九切片实现记录：

- Target terminal lifecycle 与 authoritative commit 已进入独立的
  `moli-core::browser_host::navigation_owner::target_termination` 模块。neutral
  request/permit/result 只携带 `{BrowserPageOwnerKey, exact PageResidenceIdentity, Crash|Close}`；prepare 与
  commit 都重新验证 exact slot instance + generation。commit 在一次 owner mutation 中推进共享 Page
  generation、清除 pending/committed request、退役 keyed retained engine，并按 crash/close 分别清空或
  删除 joint history；Crash/Close terminal state 都由 core exact-once gate，protocol projection 不能再次
  推进 generation；
- 模块按三层保持拆分：`conn/browser_target_termination.rs` 是 core transaction adapter；
  `conn/browser_context/target_termination_projection.rs` 只负责 active/background physical Target/Page、
  renderer channel、network artifact 与 session state 投影；`domains/page/termination.rs` 和 Target domain
  只负责命令前置清理、response/event/detach shape。原 `target_session_owner.rs` 中重复的 crash、Page.close
  和 Target.close mutation 已删除，没有把新逻辑转移进另一个总管文件；
- `PageTargetTerminationOwnerAction` 已删除 `CommandOwnerScope/sessionId`，只保存 neutral core request 与
  projection kind。primary session 被 detach/reattach 后 action 仍作用于 capture 时的 Browser Target；如果
  Page 已 replacement，旧 generation 的 delayed action 会被 core 拒绝，不能关闭 successor；
- `Page.crash`、`Page.close`、active/background `Target.closeTarget` 与 BrowserContext disposal 都通过
  同一个 transaction adapter。迁移期固定顺序为 projection inputs prepare → core commit → 同一 actor turn
  内同步 physical absence projection → 异步关闭 retired Page/必要时 promote；core commit 与同步投影之间
  没有 `await`、frontend flush 或 callback；
- crash 是可恢复的 Target 状态，不是永久 close。core 保存 `Crashed` /
  `Recovering(exact BrowserDocumentNavigation)` gate：重复 crash 被拒绝，crashed Target 仍能
  `Target.closeTarget`；启动恢复 navigation 后只有该 request 能 replacement，失败会恢复 crash gate，成功
  commit 后 Target 可再次 crash。扩展回归最初稳定暴露 crash 后恢复 navigation 丢 response（20/20
  失败），修正 owner state 后原用例与 crashed-target close 连续 20 轮 `40/40` 通过，没有增加 sleep、
  timeout 或 retry；
- popup 创建尚未成为 live Target 前的失败回滚仍允许直接 `forget_target`，因为它是在撤销未提交的
  staging，而不是执行一个已存在 Target 的 terminal transition。这个例外不得扩展给 Page/Target command
  或 BrowserContext disposal；
- 当前仍是 Phase 2 bridge：physical Target/Page registry、active engine 的完整 target-keyed identity 与
  promotion handoff 仍在 protocol，termination action 也仍由 protocol scheduler 调用；尚未发布
  `TargetCrashed/TargetClosed` Browser fact。下一步不能把 adapter 当作最终 Browser queue，也不能把
  frontend event 状态反向塞入 core。

本切片最终聚焦验证为 core/adapter termination 集合 `15/15`、修复后 exact recovery/close stress
`40/40`、protocol close/crash 扩展集 `62/62`；core/protocol 全量 nextest `5730/5730`、`13 skipped`，
workspace 全量 nextest `15534/15534`、`17 skipped`，workspace fmt check、all-target clippy 与 release
workspace build 均通过。显式清除大小写 proxy 变量、设置 `NO_PROXY=*` / `no_proxy=*` 并固定
`target/release/moli` 的外部 smoke 为 CDP `210/210`、WebDriver Classic/BiDi/Selenium
`148/148`。

最终 workspace 验证还暴露了一个与 termination 无关的既有测试同步竞争：blocked defer 测试把
“defer source 请求已经到 fixture server”误当成“parser 已到 EOF、`readyState` 已进入
`interactive`”，因此在全量负载下出现 `15533 passed, 1 failed`，独立基线却是 `30/30`。测试改为让
`Runtime.evaluate` 在 defer 响应仍未释放时通过 `readystatechange` 精确等待 `interactive`，继续断言
目标 Document 已提交、defer 尚未执行且 load 尚未发生；没有增加 sleep、poll 或 retry。修正后精确
用例 `50/50`、四个相邻 parser/defer 生命周期用例 15 轮共 `60/60`，随后 workspace 全量通过；该测试
同步修复使用独立 commit，未混入 Browser Core 状态机提交。

2026-08-02 第十切片实现记录：

- selected/retained `NavigationEngine` 的**身份与交接 authority** 已完整收进独立的
  `moli-core::browser_host::navigation_owner::engine_registry` 模块。该 registry 是
  `selected engine + Option<BrowserPageOwnerKey> + keyed retained engines` 的唯一 strong owner；未绑定状态
  只允许用于启动期或当前 BrowserContext 没有 Target。主 `navigation_owner.rs` 只组合 engine、Page
  residence、history、request 和 termination registry，不再实现 active/background map 或 Target engine
  切换算法；页面执行与 document build sibling 模块也只能通过 core-private selected-engine accessor
  工作，protocol 不能借回 `&mut NavigationEngine`；
- core 暴露 protocol-neutral 的 typed transaction：同 BrowserContext 使用
  `BrowserTargetEngineHandoff`，跨 BrowserContext 使用 `BrowserContextEngineHandoff`，旧 selected engine
  的处理必须显式声明为 `Unbound`、`Discard(exact owner)` 或 `Retain(exact owner)`。registry 在 mutation
  前验证当前 exact owner；selected A 时声称 current B 的 stale handoff 会直接失败，不能依赖 protocol
  当前指针把错误 runtime 搬给新 Target。`Unbound` 也不是 wildcard：它只能匹配 Core 确实尚未绑定的
  selected engine，不能替 frontend 为一个漏登记的 current Target 补造历史；同 context handoff 的
  可失败构造器还会拒绝 current/next BrowserContext 不同的 key，跨 profile 复用不能靠 frontend 自律；
- 交接语义按物理 Page lifetime 固化：同 context 的当前 Target 没有 resident Page 时可以把同一 selected
  engine 重新绑定给下一个 Target；有 resident Page 时必须按 exact key park，优先恢复下一个 Target 的
  retained engine，否则创建 replacement；跨 context 即使旧 Target 没有 Page，也不能复用旧 context 的
  engine，必须恢复 next context 的 exact retained engine 或用 next context runtime 新建。这样 storage、
  cookie、renderer context runtime 不会因为“恰好没加载 Page”跨 profile 泄漏；
- protocol 侧相应拆出唯一的
  `moli-protocol::conn::browser_target_engine_handoff` 投影 adapter。它可以读取迁移期 physical
  active/background Target/Page slot 来决定 `Retain` 或 `Discard`，但只向 Core 提交
  `{browser_context_id, target_id}`，不读取 CDP attachment/session 来授权交接。Target 创建、同 context
  promote/park、BrowserContext 激活和 idle reset 都经过该 adapter；`CdpConnection` 不再拥有名为
  `replace_navigation_engine` 的无身份入口；
- loaded Page commit 现在把 Core 已授权并提交的 exact `BrowserPageOwnerKey` 作为结果返回，detached
  engine 据此直接收养到 selected/retained residence，不再在 commit 后通过 frontend session 重新猜
  owner。session-based adoption 只保留 `#[cfg(test)]` 的迁移 fixture adapter；background residence 也按
  Target key 而不是 session key。Target crash 保留 selected owner 以便 exact recovery，真正 close 才解绑
  dead Target；A → B → A 的 loaded Target promote 回归证明两个 renderer owner 会被精确 park/restore；
- 本切片完成的是 engine registry 与 handoff，不是最终 Browser Owner queue。physical Target/Page registry、
  Target metadata 和 engine replacement 所需的 fetch/runtime construction inputs 仍暂留 protocol
  projection；下一切片应先迁移 physical Target/Page registry，再让 navigation action scheduling 和
  Browser fact producer 建立在同一个 Core owner 上。不得把 physical projection 搬进
  `engine_registry.rs`，也不得让新 frontend 直接调用 engine adoption 绕过 typed Target/context handoff。

严格 `Unbound` gate 在 workspace 验证中集中暴露了两个 Target 测试 fixture：它们过去直接赋值 physical
`browser_context/active_target_id`，没有经过 Core context handoff；其中 loaded fixture 甚至先用 unbound
engine 构建 Page、最后才补 Target。两个共享入口现统一先通过生产 `insert_browser_context` 建立 exact
owner，再加载/收养 Page engine；没有逐测试补登记，也没有给测试新增绕过 Core 的后门。修正后整个
Target domain `553/553` 通过。

本切片聚焦证据为 Core engine registry 与 protocol projection `11/11`；完整 `scripts` 测试二进制中
218 个已运行用例连续 10 轮全部通过，DCL snapshot 的两条合法竞态用例连续 20 轮 `40/40` 通过。
最终 core/protocol nextest `5739/5739`、`13 skipped`（run ID
`7252715f-5a9c-4e83-afcf-24bcf5a56b56`），workspace 全量 nextest `15543/15543`、`17 skipped`
（run ID `9e380b24-7d78-42f9-94d9-566faa9031f1`）；workspace fmt check、all-target clippy 和 release
workspace build 均通过。显式清除大小写 proxy 变量、设置 `NO_PROXY=*` / `no_proxy=*` 并固定
`target/release/moli` 的外部 smoke 为 CDP `210/210`、WebDriver Classic/BiDi/Selenium
`148/148`，两者失败列表均为空。

最终 workspace 的前一轮（run ID `dede38d3-2b89-4c4c-88d0-e4b66b107a5c`）曾在最后一项观察到
与本切片无依赖路径的 renderer
`worker_messageport_close_preserves_same_task_queued_messages` 触发 30 秒外层 test timeout；当时页面侧
初始化变量都尚未出现，其余 `15542` 项通过。该精确用例随后以 `--stress-count 20 --flaky-result fail`
连续 `20/20` 通过（run ID `28e532c8-a353-47ab-b544-205c7e718221`），下一轮 workspace 全量也通过。
证据支持全量 CPU/V8 压力下未复现的执行饥饿，不支持归因到 Browser engine handoff；本切片没有修改
renderer 生产代码、timeout、retry 或测试调度。

2026-08-02 第十一切片实现记录：

- BrowserContext 的**存在、当前选中项、inactive 顺序和拓扑 revision** 已成为
  `BrowserNavigationOwner` 内唯一 authoritative registry。Core 按责任新增独立
  `navigation_owner/context_registry.rs`。第68切片又为可由 frontend 显式指定、删除后可复用的 public id 增加
  Core-owned exact `BrowserContextHandle`；registry 保存 typed `BrowserContextId`、exact handle 与 revision，engine strong
  ownership 继续留在 sibling `engine_registry.rs`，registry 回归单独放在
  `navigation_owner/context_registry/tests.rs`。主 `navigation_owner.rs` 仍只组合子 registry，没有吸收
  BrowserContext 算法；
- Core 暴露 `register_browser_context`、`activate_browser_context`、
  `prepare_browser_context_removal` 和两种 exact commit。duplicate/unknown context、selected physical
  projection 不一致、selected engine owner 不一致，以及 revision 过期的 removal permit 都在任何 topology
  mutation 前被拒绝。selected context 的 removal successor 由 Core 在 permit 中冻结，Protocol 只能为该
  exact successor 提供尚未迁移的物理 runtime 输入，不能临时改选另一个 context；
- BrowserContext topology 与跨 context engine handoff 现在由同一个 `&mut BrowserNavigationOwner`
  transaction 提交。切换时 Core 先验证 `{selected context, selected target engine}` 的 exact projection，
  再按 context/target key restore retained engine 或建立 replacement，最后提交 selected/inactive topology；
  raw `BrowserContextEngineHandoff` 已降为 `engine_registry` 的 Core-private contract，Protocol 不再能够绕开
  context registry 单独搬 engine；
- Protocol 侧也明确拆成两层：`conn/browser_context/registry_projection.rs` 只执行 Core transaction，并在
  同一 actor turn、无 `await`/callback/frontend flush 的区间内同步 `Option<BrowserContext> + Vec` 物理
  payload 投影；`conn/browser_context/lifecycle.rs` 只保留 command routing、fetch override、resource
  invalidation 和 preferred-context restore。`has_browser_context_id` 已查询 Core authority，不再用物理
  payload 的偶然存在授权 Browser/Target/Page 命令；default-context bootstrap 和 create-target 也先查询
  Core count。每次注册、切换、删除后，projection adapter 都断言 Core 与 physical payload 的 count、
  selected id、identity 唯一性和 membership 一致，不允许错位状态静默流入下个命令；
- 初始化顺序因而成为明确 owner invariant：必须先通过正式 registry 路径注册 BrowserContext，再由该
  context 的 selected engine 加载并安装 Page。严格 gate 暴露了过去“先用 startup engine 加载 Page，最后
  直接赋值 physical BrowserContext”的旧测试夹具；这些夹具统一改为生产入口，没有增加 test-only registry
  同步、fallback、sleep 或 retry。否则注册 context 时旧 engine 被正确 replacement，先前 Page 的 renderer
  runtime 也会随错误 owner 关闭；
- 本切片没有把 `BrowserContext` 的 DevTools session/domain state、storage/cookie payload、Target/Page
  物理容器或 frontend event projection 搬进 Core。它们仍是 transition projection，而不是第二份 topology
  authority。当前 public physical fields 仍允许尚未迁完的内部 fixture 直接构造 payload；任何需要
  BrowserContext existence/selection 的 production path 都必须经过 Core registry，最终应随 Target/Page
  registry 迁移删除这条可绕过的表示；
- 下一切片应把 Target topology（context membership、active/background identity、lifetime）迁到独立 Core
  registry，再让 physical Target/Page payload 通过 exact handle 投影。BrowserContext disposal 目前先经
  Target termination transaction 清理各 Target，再提交 context registry removal；在 Target registry
  完成前，不能把这组 protocol cleanup 编排误称为最终 Browser Owner 原子操作，也不能把 Target/Page
  metadata 塞进 `context_registry.rs`。

本切片聚焦验证包括 Core context registry `6/6`（run
`7bd79ddf-c592-4836-b91b-0c33d5a75668`）、严格 projection/bootstrap/create-target 扩展集
`252/252`（run `3deabc5c-03b2-49fb-8bb6-f4c2c56e0677`），以及夹具迁移后的 Protocol 全量
`3225/3225`。最终 workspace nextest `15549/15549`、`17 skipped`（run
`c7ef7251-2912-4198-a6fd-68496c7516e2`），workspace fmt check、all-target clippy `-D warnings` 与
release workspace build 均通过；release binary SHA-256 为
`38a647ee45eb1f4a2d431deccf4af3609999233f364d798b58a04193fc98274a`。显式清除大小写全部
proxy/no-proxy 变量、设置 `NO_PROXY=*` / `no_proxy=*` 并固定该 release binary 的外部 smoke 为 CDP
默认全组 `210/210`、WebDriver Classic/BiDi/Selenium `148/148`，两者失败列表均为空。本记录仍不表示
Phase 2 已完成。

2026-08-02 第十二切片实现记录：

- top-level Target 的**存在、全局唯一 identity、BrowserContext membership 和 active/background
  topology** 已成为 `BrowserNavigationOwner` 内唯一 authoritative registry。Core 继续按责任拆分：
  `navigation_owner/target_registry.rs` 只保存拓扑和值校验，
  `navigation_owner/target_transaction.rs` 只组合 Target 注册、激活和 staging rollback 与 engine handoff；
  transaction 回归放在独立 `target_transaction/tests.rs`。主 `navigation_owner.rs` 只组合 sibling
  registry，并把通用 runtime-state cleanup 保持为 Core 内部入口，不重新吸收 Target 算法；
- BrowserContext 注册会在同一个 Core operation 中提交它的初始 Target topology；context 切换和 removal
  直接读取 Core active Target，而不再让 Protocol 传入一个可能过期的 `target_id`。context removal 同时删除
  该 context 的全部 Target membership，并清理 keyed engine、Page residence、request、history 与
  termination state；Target id 在所有 BrowserContext 间全局唯一；
- Core 暴露 protocol-neutral 的 typed Target transaction：background/active registration、exact activation
  和尚未成为 live browsing context 的 background staging rollback。live Target close 仍只允许走已有的
  `target_termination` transaction：Close 在同一次 owner commit 中删除 Target topology，Crash 保留 Target
  membership 以便 exact recovery；active close 后是否选择 successor 是后续独立 activation transaction，
  不能由 physical `Vec` 自动决定。Protocol 的 close projection 也直接消费 Core 返回的 authoritative
  `Active|Background` residence，不再重新读取物理容器做第二次决策；
- Protocol 侧保持三个窄模块：`conn/browser_context/target_registry_projection.rs` 只执行 Core transaction
  和同 turn 的物理 Target payload 投影；`conn/browser_context/engine_factory.rs` 只捕获创建 replacement
  engine 所需的迁移输入；Target domain/lifecycle 模块只做 command、session、event 和异步 Page 同步编排。
  `conn.rs` 只保留模块接线和 worker id collision 查询，不承载 Target topology mutation；
- 迁移期顺序被冻结为：校验 exact physical projection → Core topology/engine commit → 同一 actor turn 内
  同步 active/background payload projection → 再允许 DevTools session、loaded Page 同步或旧 Page teardown
  await。Core commit 与物理 projection 之间没有 `await`、callback、frontend flush 或 renderer call；每次
  context/target 注册、激活、关闭和 rollback 后都断言 Core 与全部 physical payload 的 count、membership、
  active identity、background 顺序和全局唯一性完全一致；
- `Target.createTarget`、renderer popup、`Target.activateTarget`、Target/Page close、BrowserContext disposal、
  popup failure rollback 和 Target identity lookup 已接入同一 Core authority。popup rollback 只接受
  background staging，并且在清理 tab/session 之前先验证该不变量；旧的“active popup staging rollback”
  测试被改为明确断言非法状态，而不是继续保留一条绕过 typed transaction 的兼容路径；
- 严格 projection gate 在首轮 Protocol 全量中暴露了 754 个旧测试 fixture：它们曾直接赋值
  `browser_context`、`inactive_browser_contexts` 或 `background_targets`，有些还先用错误 engine 加载 Page
  再补 Target。共享 fixture 和精确用例全部改为正式 context/Target registry 与 navigation installation
  入口；没有增加 Core 自动收养、test-only topology sync、fallback、sleep、retry 或 `yield_now`。迁移后
  整个 Target domain `615/615`、Protocol 全量 `3229/3229` 通过；
- 首轮 workspace 集成还暴露了一个真正缺失的 owner operation：production default BrowserContext 先注册
  unclaimed active placeholder，第一次 `Target.createTarget` 会在物理 payload 中替换它；如果 Core 走普通
  active registration，就会把 placeholder 降为 background，形成 Core 两个 Target、physical 一个 Target。
  修复没有在应用层加特判写 registry，而是在 `target_transaction.rs` 新增 exact bootstrap active-target
  replacement：先校验 selected context、完整 physical topology、当前 engine owner 和 expected active
  placeholder，再原子绑定 replacement engine、替换 active identity、退休旧 Target runtime state，且绝不产生
  background Target；Protocol 的 `replace_active_target_projection` 只负责同 turn 投影。Core 与 Protocol 都有
  “替换后仍恰好一个 Target”的边界回归；
- 本切片仍没有把 `BackgroundTarget` / active Target 的 DevTools session、opener/window metadata、storage
  snapshot、renderer `Page` 和 domain state 搬入 Core，也没有建立 Browser fact journal 或自主 Browser
  Owner queue。下一切片应继续拆 physical Target/Page payload 与 browser-owned handle/fact producer；不能把
  protocol session/event state 塞进 `target_registry.rs`，也不能把这次同 turn projection 当成最终 actor。

本切片聚焦验证包括 Core navigation owner `40/40`（run
`2bd909ad-a490-48eb-9f56-d1630fbd7cdd`）、Target domain `615/615`（run
`f38b1903-538c-402f-90a7-2ea15a333c20`）和 Protocol 全量 `3229/3229`（run
`b97faf6f-f872-4f12-ab0a-bdf909ead802`）。Protocol 前一轮唯一失败是既有
`isolated_context_hash_navigation_emits_navigated_within_document` 偶发得到 `Null`；没有增加 retry、sleep、
放宽断言或改产品路径，而是把该 exact case 连续跑到 `20/20`（run
`376a57f0-416e-4663-af8b-5013a424211b`），再把所属 runtime navigation 模块 10 轮跑到 `110/110`（run
`b02e5ece-8ad3-4849-a592-3bc8f93a0688`），随后 Protocol 全量通过。证据只支持保留为未复现的既有并发风险，
不支持把它归因于本切片 Target topology 改造。

最终 workspace nextest `15557/15557`、`17 skipped`（run
`cedbedb5-0d7b-400e-8636-d61b52ce2e9d`），workspace fmt check、all-target clippy `-D warnings` 与
release workspace build 均通过；release binary SHA-256 为
`12212bd23d4cf756d4ea7b11d0d3da39409fc0756c9451e9984f11aed2c17f21`。显式清除大小写全部
proxy/no-proxy 变量、设置 `NO_PROXY=*` / `no_proxy=*` 并固定该 release binary 的外部 smoke 为 CDP
默认全组 `210/210`、WebDriver Classic/BiDi/Selenium `148/148`，两者 `ok=true`、失败列表均为空。本记录
仍不表示 Phase 2 已完成。

2026-08-02 第十三切片实现记录：

- Core 新增独立 `browser_host/target_handle.rs`，只定义一个 top-level Target 实例的 stable
  capability 与 `staged -> live -> retired` exact-once lifecycle。clone 共享同一个 instance state；相同
  public `targetId` 新建出的另一个 handle 不是同一实例，不能授权旧实例或 replacement。handle 不保存
  URL/title、opener/window、DevTools session、storage、renderer `Page` 或 domain state，避免把物理 payload
  重新聚合成一个 Core 大对象；
- 模块职责继续拆开：`navigation_owner/target_registry.rs` 只保存
  `{targetId -> BrowserContext + exact handle}` 与 active/background topology，并校验 physical projection 携带
  的 handle 是同一 live instance；`navigation_owner/target_transaction.rs` 只组合注册、激活、placeholder
  replacement 和 engine handoff；`target_termination.rs` 继续拥有 crash/close transaction。主 owner 只组合
  sibling registry，不吸收 Target lifecycle 算法；
- 新 Target 的 handle 由 Core typed transaction 分配并随 `BrowserTargetRegistration` 返回，Protocol 必须把
  这个 exact capability 同 turn 投影到物理槽位。唯一不同的是 BrowserContext 初次注册：physical context
  先显式创建 staged initial handle，Core 在 context commit 时校验每个 handle 尚未使用，再统一 activate。
  production default-target bootstrap 因此只保留一次性的
  `stage_active_target_for_browser_context_registration` 入口；任意写 raw `targetId` 的便利 setter 已限制到
  test fixture；
- Protocol 的 `ActiveTargetState` 与 `BackgroundTarget` 现在都直接携带 `BrowserTargetHandle`；旧的
  `BrowserContext.target_id: Option<String>` 和 `BackgroundTarget.target_id: String` 已删除，所有 public ID
  读取都从 capability 派生。park/promote、active demotion、popup staging 和 loaded/unloaded slot snapshot 都
  搬运同一个 handle，不再根据字符串重建 Target identity；
- lifecycle 由 Core 单方推进：background staging rollback、close、BrowserContext removal 和 bootstrap
  placeholder replacement 都 retire 精确 handle；Target crash 保留 live handle 以支持 exact recovery；新
  registration/initial context commit 只 activate staged handle。Protocol 只能查询 `is_live/is_retired` 或用
  `target_handle_is_current` 验证 projection，不能 activate/retire；
- projection gate 不只比较 count、membership、active ID 和 background 顺序，还比较 exact handle instance 与
  live state。新增回归证明“同 public ID、错误 instance handle”不能 activate Target，placeholder 的旧 handle
  会 retired、replacement handle 会 live，rollback/close/context removal 都只退休所属实例，而 crash 不会；
- 严格 handle projection 在首轮 Protocol integration 中暴露两个旧 fixture：它们在 context 已注册后又写入
  同一个 `targetId`，过去只是字符串幂等赋值，现在会错误地制造 staged instance。test-only 同 ID setter
  被定义为幂等并保留已有 Core handle；这不是自动收养、retry 或 fallback。`cargo check -p
  moli-protocol` 证明非测试生产构建不再依赖该 raw-ID setter；
- 本切片仍没有把 active/background Target 的 URL/title、opener/window metadata、session/storage/domain
  state 或 renderer `Page` 移入 Core，也没有建立 Browser fact journal 或自主 Browser Owner queue。
  `BrowserTargetHandle` 只是 physical payload 与 Core authority 之间的 exact capability，不代表 physical
  Target/Page registry 已迁完。下一切片应继续拆出 browser-owned immutable Target creation/initial-Document
  metadata 与 physical Page handle 边界；不能把 DevTools session/event projection 塞进 handle 或 registry。

本切片聚焦验证包括 Core Target handle `2/2`（run
`c46602ca-16be-416a-8f11-bb370df1745a`）、Core navigation owner `40/40`（run
`ecf51968-8a4f-423a-8e50-d8a0786e6cc8`）、Protocol exact Target projection `6/6`（run
`7f0fc3e6-1142-45f3-ae16-4e5f6d182d55`）、Target domain `615/615`（run
`2b88659c-b05b-41ca-b3c1-8061649b5d9f`）和 Protocol 全量 `3231/3231`（run
`951e8574-a559-42e6-846c-fb0b90812434`）。

首轮 workspace 全量 `15560/15561`（run `ae6516fd-805b-44f8-9df4-42dd99860e1f`）唯一失败是与本切片
不相交的 ServiceWorker 测试时间假设：JS 在固定 20 个 zero-delay timer 后永久快照，当时 lifecycle 已到
`activating`，后续 `activated` 无法更新结果。测试改为等待真实 `statechange:activated` completion，没有
增加 sleep、timeout、retry 或修改 production 行为；原失败独立复现后，精确 stress `20/20`（run
`261e0c78-b3bc-4e90-b290-8e31faa7f0c5`）和 ServiceWorker 组 `448/448`（run
`7fe38175-5d17-4055-9187-a37583cab80a`）通过，该测试同步修复保持为独立 commit。

最终 workspace nextest `15561/15561`、`17 skipped`（run
`720ec5ee-a153-46f7-90d4-59e703984f0e`），workspace fmt check、all-target clippy `-D warnings` 与 release
workspace build 均通过；release binary SHA-256 为
`07d31b4215707af9878250c2b4cc7ed31bd16e5b2fbde396924e431b38e064e4`。显式清除大小写全部
proxy/no-proxy 变量、设置 `NO_PROXY=*` / `no_proxy=*` 并固定该 release binary 的外部 smoke 为 CDP
默认全组 `210/210`、WebDriver Classic/BiDi/Selenium `148/148`，两者 `ok=true`、失败列表均为空。本记录
仍不表示 Phase 2 已完成。

2026-08-02 第十四切片实现记录：

- top-level Target 的 **exact Page-slot capability** 已从“Protocol 持有 strong handle、Core 只建 weak
  lookup”迁成 `BrowserNavigationOwner` 内唯一 authoritative strong registry。BrowserContext 初次注册时，
  Core 原子收养 staged `{Target handle, Page residence handle}`；后续 Target registration 则由 Core 同时分配
  两个 exact capability，并通过 `BrowserTargetRegistration` 返回给同 turn 物理投影。丢弃 Protocol clone
  不再撤销 Browser authority，同 `targetId` 的另一个 Page handle 也不能伪造 authority；这项新语义明确
  supersede 第五切片记录的迁移期 weak-index 方案；
- `BrowserTargetTopologyProjection` 已从 Target-handle 列表升级为
  `BrowserTargetSlotProjection { target, page_residence }`。Core 分别由 `target_registry.rs` 校验
  identity/context/active-background topology、由 `page_registry.rs` 校验 Page capability instance 和一对一
  slot 关系；Protocol 的 `target_registry_projection.rs` 只组装 exact physical projection、执行 typed
  transaction，并在无 `await`、callback 或 frontend flush 的同一 actor turn 内把 Core 返回的 capability
  clone 绑定到 active/background 物理 slot；
- replacement 与 termination 不再接收 Protocol 反向传入的 `BrowserPageResidenceHandle` 作为授权材料。
  `page_replacement.rs` 和 `target_termination.rs` 只以 Core-owned `{BrowserContext, Target}` 找到注册
  capability，再 capture exact generation、prepare 和 commit。Protocol adapter 仍负责 renderer/DevTools
  participant 与物理 Page swap/teardown，但不能通过选择一个 handle 改变哪个 Page 是 current；
- lifetime 规则已经收口：loaded replacement 在 Core commit 中推进共享 generation；Crash 推进 generation
  但保留 Target 与 Page-slot registration，供 exact recovery/后续 Close 使用；Close、staging rollback、
  bootstrap placeholder replacement 和 BrowserContext removal 删除 registration；navigation failure 的 Page
  runtime discard 只清 engine/request 等 page-runtime work，保留 live Target 的 Page-slot capability，因为
  物理 slot 已通过同一个共享 handle 暴露 successor generation；
- 模块边界保持拆分：`page_residence.rs` 只实现 stable instance/generation primitive，
  `page_registry.rs` 只实现 Target-to-Page authority 与 lifetime，并使用普通 owner-owned map，只有
  `&mut BrowserNavigationOwner` transaction 能注册、删除或推进 authority；`target_registry.rs` 只实现
  Target topology 及 paired projection guard，`target_transaction.rs` 只组合新 Target capability allocation
  与 engine/topology commit；Protocol 的 `state/page_slot.rs`、`page_state/parked.rs` 继续只保存和搬运物理
  payload，replacement、termination、Target projection 仍是三个独立 adapter，没有把 session、CDP event
  或 renderer payload 塞进 Core handle/registry；
- strict projection gate 暴露了一批旧 Protocol test fixture：它们直接赋值 physical `browser_context`、
  Target 或 Page，绕过 Core registration，能执行 renderer JS，却没有资格让后续 publication 进入当前 Page。
  受影响 fixture 改为生产 `insert_browser_context` / exact Target staging 入口，没有增加自动收养、test-only
  authority fallback、sleep、retry 或 timeout。最后两条 IndexedDB fixture 在修正前精确压力复跑连续
  `3/3` 超时；修正后分别连续 `20/20` 通过（run
  `115e95ba-f906-4deb-a931-77bcd12771c9`、`3f420da2-49b7-4230-9cc0-ca54e8f4be33`），证明根因是
  split physical/Core state，而不是 IndexedDB 事务速度或调度饥饿；
- 本切片的 Core 全量为 `2529/2529`、`13 skipped`，Protocol 全量为 `3232/3232`（run
  `e677e7b3-de0d-473e-a8b8-7cd8830ad638`），workspace 全量为 `15565/15565`、`17 skipped`（run
  `eb7a4049-36ed-4f3c-af2e-34de8de074cc`）；workspace fmt check、all-target clippy `-D warnings` 与 release
  build 均通过，release binary SHA-256 为
  `92f4a1d426beccf129978f853fdd792af81b2611730a1b24b3d6e9eb78b1a316`。显式清除大小写全部
  proxy/no-proxy 变量、设置 `NO_PROXY=*` / `no_proxy=*` 并固定该 release binary 的外部 smoke 为 CDP 默认
  全组 `210/210`、WebDriver Classic/BiDi/Selenium `148/148`，两者 `ok=true`、失败列表均为空。

本切片仍不表示 Core 已拥有 renderer `Page`、initial-empty-Document/Target creation metadata、Page
generation 的物理 mutation 或自主 Browser Owner queue。下一切片先迁移 immutable creation/initial-Document
metadata，并做一次严格 Phase 2 exit audit；在 physical Page payload、navigation scheduling 或 fact producer
仍依赖 `CdpConnection` 时，不得提前进入 Phase 3，也不得把这些 payload 聚合进 `page_registry.rs`。

2026-08-02 第十五切片实现记录：

- initial-empty-Document 的 creation seed、creator security context、storage key、stable loader id、materialized、
  exited 与 pending cross-document navigation 已从 Protocol `TargetOwnerState` 移出，成为
  `BrowserNavigationOwner` 内按 `BrowserPageOwnerKey { context, target }` 索引的唯一 authoritative registry。
  Protocol parking state 已删除对应 metadata、lifecycle flag、history seed helper 和 diagnostics，Target
  park/promote、frontend detach/reattach 不再搬运或重建这份 browser state；
- 模块继续按责任拆分：`navigation_owner/initial_document.rs` 只定义 immutable creation value 与只读
  snapshot，`initial_document_registry.rs` 只实现 initial Document lifecycle/lifetime，`target_creation.rs`
  只定义可扩展的 protocol-neutral Target creation envelope；`target_transaction.rs` 在 Target handle、Page
  residence 与 topology 注册的同一 Core transaction 内安装 creation metadata。Protocol 新增的
  `conn/browser_initial_document.rs` 只把一次 frontend route 解析成 owner key、查询 Core snapshot，并校验
  同 turn 物理 Page projection，不保存第二份状态；
- ordinary active/background Target creation、bootstrap active replacement 和 renderer popup 都通过
  `BrowserTargetCreationMetadata` 把 initial-Document seed 一次性交给 Core。default BrowserContext bootstrap
  因 context topology 必须先整体注册，保留一个显式 bootstrap registration adapter；它只在同一同步调用路径
  中补注册已经存在的 exact Target，不经过 frontend event、callback 或 `await`。这是第十五切片当时的迁移形状；
  第28切片已把这条补注册 adapter 删除，并把 metadata 并入 BrowserContext registration transaction；
- cross-document request start 在 Core 内标记 initial Document pending，failed request clear 同时清 pending，
  request commit 与 loaded Page replacement 原子标记 exited。history snapshot/same-document/loaded commit
  优先从 Core registry 派生 initial seed，Protocol 只能提供已加载 Page 的 fallback snapshot，不能反向声明
  initial Document。`document.open()` 的 exact renderer lifecycle ingress、Target crash、failed Page-runtime
  discard 与 Target close 分别执行 exit、exit、exit 与 forget；creation metadata 在普通 navigation/crash 后
  保留供 history/diagnostics 使用，只有 Target lifetime 结束才删除；
- initial Page materialization 仍是 Phase 2 same-turn physical projection：先向 Core 查询 current creation
  record，再同步安装 active/background `Page` 与 exact Document lifecycle binding，立即在 Core 标记
  materialized，并在任何旧 Page teardown `await` 之前断言“materialized current initial Document 必有物理
  Page”。如果 replacement/exit 已发生，迟到的 initial Page build 被 stale-drop，不能覆盖 successor Page；
- connection diagnostics 新增从 Core registry 直接生成并按 target id 排序的
  `browserOwnerInitialEmptyDocuments`；旧 physical Target owner diagnostics 不再输出一份看似 authoritative 的
  initial Document。Core transaction、request/replacement/history/termination 与 Protocol initial Page、
  popup creator/origin、frame tree、`document.open()`、stale build 回归的聚焦 nextest 为 `76/76`（run
  `96b2f73c-66aa-4316-a208-1e7e4b490a97`）。
- 首轮 workspace nextest 为 `15563/15569`、`17 skipped`（run
  `ada7392c-1be1-4afb-9911-4a6c45392514`），没有用重跑掩盖失败：五条 navigation history 用例独立复现
  `5/5` 失败，统一少了 initial `about:blank` entry。根因是共享 test fixture 仍把已被删除的 Protocol
  staging 参数误当 creation authority；修复让 fixture 在 topology commit 后走与 production default-context
  相同的显式 Core bootstrap registration。修复后五条精确用例 `5/5` 通过（run
  `55ba3005-88c9-4761-b47b-81133221a3c6`），并连续 `20/20` 轮、共 `100/100` 次通过（run
  `10db3f2b-85d6-4aa5-aa3e-705ec4bb3f37`）；没有恢复 Protocol metadata、增加 fallback、sleep 或 retry；
- 首轮剩余一条 `runtime_insert_then_remove_iframe_preserves_attach_before_detach` 失败与本切片没有已证明的
  数据依赖，单跑随后通过。按 flaky 证据流程，精确用例连续 `30/30`（run
  `ec2b2cba-6d08-4acb-ab91-88d7e5c823a0`）、所在六条 iframe navigation 邻域连续 `10/10` 轮、共
  `60/60` 次（run `eb4a7301-4224-406c-ab32-650a230bc039`）均通过，未修改产品行为或放宽断言；证据只支持
  记录为首轮未复现的并发风险，不支持宣称已经找到独立 iframe 根因；
- 修复后 Protocol 全量 `3231/3231`（run `ab1f565a-4dc6-4d01-838d-3a282a415cc5`），最终 workspace
  nextest `15569/15569`、`17 skipped`（run `562b6dcc-6761-4d43-a6ba-2fa8c72f4636`）。workspace fmt
  check、diff check、all-target clippy `-D warnings` 与 release workspace build 均通过；被测 binary 为
  `target/release/moli`，SHA-256 为
  `c6b514d1b520f8bb0965804f839488144ea571f94768076e2988a17accea5c55`。显式清除大小写全部
  proxy/no-proxy 变量、设置 `NO_PROXY=*` / `no_proxy=*` 并固定该 release binary 的外部 smoke 为 CDP 默认
  全组 `210/210`、WebDriver Classic/BiDi/Selenium `148/148`，两者 `ok=true`、失败列表均为空。clippy
  删除 test-only navigation commit adapter 的空尾分支后，initial/document-navigation 聚焦集再跑
  `66/66`（run `1b543f56-615c-4c58-beb7-dbed6149a73c`）通过。

本切片后的严格 Phase 2 exit audit 仍**不允许**进入 Phase 3：

- `BrowserPageResidenceHandle::advance_generation` 仍是 public production API；
- Protocol `TargetPageSlot::replace_loaded_page_with_reason` 在 initial Page install、generic Page install/clear 和
  failed-navigation discard 路径直接调用它；replacement/termination 虽已由 Core transaction 推进 exact
  generation，这些剩余路径仍允许 physical projector 决定 Page lifetime；
- 下一内聚切片必须新增 Core-owned Page residence transition operation，由 Core 校验 exact Target/Page
  capability、推进 generation 并返回 successor identity；Protocol 只能同步投影 `Some(Page)` / absence，且
  不得再次推进。完成所有 production call-site 迁移后，把 `advance_generation` 收窄到 Core module，删除
  production migration setter，再重新执行 Exit gate audit；
- renderer `Page` 物理对象暂留 Protocol 不构成该 audit 的失败，拥有 generation mutation authority 才构成。
  自主 scheduling 和 fact journal 继续分别留给 Phase 3/5，不能为了提前开始 actor 而把 Page authority 缺口
  带入下一阶段。

2026-08-02 第十六切片实现记录：

- 新增独立 Core 模块 `navigation_owner/page_transition.rs`，定义 protocol-neutral
  `BrowserPageResidenceTransitionKind`、exact prepare permit 与 committed transition。initial Document
  materialization 和 failed-navigation discard 都先冻结 `{BrowserPageOwnerKey, previous Page residence,
  kind}`；Core commit 重新校验 exact Target 与 generation，只推进一次共享 capability，并返回 exact
  successor identity。permit/result 不含 `sessionId`、CDP command、event buffer 或物理 `Page`；
- initial materialization commit 现在把 Page generation 与 initial-empty-Document `materialized` one-shot flag
  一次性提交。没有 creation record、已经 materialized、已经 exited 或 generation 已被 successor 替换时，
  Core 都不再签发 permit；Protocol 不能通过“slot 当前为空”重新制造 initial Document authority；
- failed-navigation discard commit 把 Page generation、target-keyed engine discard、pending/committed request
  清理、crash-recovery request 取消和 initial Document exit 放在同一个 Core mutation 中；Target topology、Page
  slot capability 与 joint history 保留。旧的 `CdpConnection -> discard_target_page_runtime(targetId)` 二段式
  bridge 已删除，failed-navigation history 继续保持不变；
- Protocol 新增独立 `conn/browser_context/page_residence_projection.rs`。它在 Core commit 前只校验 active 或
  background physical slot 携带 permit 的 exact capability；Core commit 后无 `await` 地投影 `Some(Page)` 或
  absence，并断言 successor generation/kind。renderer lifecycle binding、session-local projection 清理和
  background cursor/websocket 清理仍属于物理 participant；退役旧 `Page` 的 `close_async()` 只允许在 Core
  commit 与同步投影之后执行；
- `TargetPageSlot` / `TargetRuntimeSlot` / `BrowserContext` / `BackgroundTarget` 的 generic
  `replace_loaded_page`、`clear_loaded_page_with_reason` 已收窄为 test-only fixture API。普通 loaded replacement
  和 Target termination 继续消费各自已有的 Core transaction，initial/failure 消费本切片的新 transaction；
  BrowserContext/Target 已被 Core forget 后的残余 payload cleanup 使用明确的 field-only projection，不推进
  已失去 owner 的 capability；
- `BrowserPageResidenceHandle::advance_generation` 已收窄为 Core crate-private；旧 production migration setter
  已删除。为仍需合成 stale generation 的下游单元测试新增默认关闭的 Core `test-support` feature，Protocol
  只在 dev-dependency 打开 `advance_generation_for_test_fixture` / `set_generation_for_test_fixture`。普通
  release dependency graph 不编译这两个入口；测试 fixture API 不能重新成为 production authority；
- active initial Page、background initial Page、late initial build、failed-navigation history 与 stale generation
  的首轮聚焦回归为 `24/24`（run `4249c54a-5717-47b6-801f-2a89b4020454`）；Core 新 transition 模块
  `3/3`（run `fb3ac350-561f-4902-a77b-58d5e43d6906`），增加“恰好推进一次”和旧 generation 立即失效
  断言后的跨 Core/Protocol 聚焦集 `7/7`（run `55c85f70-4d86-473b-9953-642578f60022`）；
- 首轮 Core + Protocol 全量没有被重跑掩盖：`5762 passed / 6 failed / 13 skipped`（run
  `b9b2c996-7f8c-4ae5-a897-82b065e403e0`）。六条失败单独运行稳定 `0/6`（run
  `4f2b85fb-ec1e-4b1c-a27f-843a5d2f2016`），共同根因是 Target/attachment 测试仍用旧 fixture 只注册
  targetId 或空 creation metadata，却要求 materialize initial Page；另一个 isolated-world fixture 直接清掉
  物理 Page 后试图第二次消费同一 one-shot initial Document。修复让这些 fixture 显式注册
  `BrowserTargetCreationMetadata` 或新的 bootstrap initial creation epoch，没有放宽 Core acceptance、恢复
  Protocol fallback、增加 sleep/retry 或弱化断言；修复后精确六条 `6/6`（run
  `f549692c-a174-4ce0-9b9f-c4e9492544ca`），连续 `20/20` 轮、共 `120/120` 次通过（run
  `9666ed02-cd62-4433-b42d-0430deb9f64b`），Protocol 全量 `3231/3231`（run
  `e8665472-121d-4667-b2f1-12cb326832d1`）。
- 最终 workspace nextest `15572/15572`、`17 skipped`（run
  `d0fafa90-4746-43cf-85a6-1e9eea0d10d3`）。workspace fmt check、diff check、all-target check、all-target
  clippy `-D warnings` 与 release workspace build 均通过；被测 binary 为 `target/release/moli`，
  SHA-256 为 `5a807e20b074946063d099af4306f39c2b79e2a7624a3288bd184829c92cb0c3`。显式清除大小写全部
  proxy/no-proxy 变量、设置 `NO_PROXY=*` / `no_proxy=*` 并固定该 release binary 的外部 smoke 为 CDP 默认
  全组 `210/210`、WebDriver Classic/BiDi/Selenium `148/148`，两者 `ok=true`、失败列表均为空。

第十六切片后的严格 Phase 2 exit audit：**通过**。

- production Protocol 已没有 Page generation mutator；所有 production Page lifetime change 都由 Core-owned
  replacement、termination 或 residence-transition transaction 决定 successor，再由 Protocol 同 turn 投影；
- Core Page/Target/engine/request/history/initial-Document API 不接受 frontend session、wire command 或 event
  storage，physical Page payload 暂留 Protocol 只是一项 Phase 6 lifetime migration，不再反向授权 Core；
- Context removal/Target rollback 后的物理 payload disposal 不再伪造 successor generation，因为 Core 已先
  删除 exact owner，旧 work 无法通过 owner lookup；
- 因而 Phase 3 可以从一个稳定的 state seam 建立 Browser Owner lane。Phase 0 的统一 production trace、同源
  differential fixture 与 release latency/RSS/CPU baseline 仍未完成；开始 PR D 的 queue cutover 前应先补齐
  这项可观测性 gate，不能把 frontend noop/pump 当作新 actor 的诊断方法。

2026-08-02 第十七切片实现记录（Phase 0 production trace 第一段）：

- 新增 neutral `BrowserActionId`（`moli-page-types/src/browser_identity.rs`）。renderer top-level
  location intent 在 producing turn 分配一次，clone/move 不改 identity；frontend/browser action 使用同一
  process-local identity space。它不复用 CDP command id、`sessionId`、loaderId 或 renderer output sequence；
- 新增独立 Core 模块 `navigation_owner/navigation_trace.rs`，集中定义 `BrowserInstanceId`、trace source、
  request correlation context 和 typed event builder。Core context 只含 Browser instance、action origin、exact
  source Page residence 和可选 source Document；不含 frontend route、wire payload、event buffer 或 socket；
- request registry 把可选 trace sidecar 与 exact pending/committed `BrowserDocumentNavigation` 放在同一
  target entry。new request 只替换 pending sidecar，commit 把它原子迁到 committed sidecar，failed pending
  cleanup、Target forget 和 runtime discard 与 request authority 一起清理；因此 trace 开启时每 Target 仍只有
  bounded pending + committed correlation，不新增 unbounded cache/journal，trace 关闭时不创建 request
  context；
- 当前 production 路径已记录 `renderer_intent_published` / `browser_action_published` /
  `browser_owner_accepted|rejected` / `navigation_request_started` / `network_request_admitted` /
  `response_commit_ready` / `page_replacement_committed` / exact renderer lifecycle reached/observed /
  frontend DCL/load projected。Page replacement event 使用 successor Page generation；action/request event 的
  Document 是 causal source Document，DCL/load event 显式覆盖成 committed successor Document；
- `frontend_projection_sequence` 是单 `CdpConnection` projection-local 单调序列，只在 trace 开启且 exact
  request correlation 存在时分配。它描述 output-buffer projection，不授权 Browser action，也不与 renderer
  lifecycle sequence、protocol work publish sequence 或未来 Browser fact sequence 比较；
- trace 通过 `MOLI_BROWSER_OWNER_TRACE=1` 默认关闭，统一 target 为
  `moli_browser_owner`。当前字段显式保留 `browser_fact_sequence=None`，因为 Browser fact journal 尚未
  建立；不能用 renderer lifecycle sequence 冒充全局 fact order。trace 不记录 URL、cookie value、
  authorization、request/response body；
- 这一步**没有**迁移 execution authority：trace 中 renderer action 的当前 residence 仍明确经过
  `protocol-scheduler`，`TopLevelLocationNavigationOwnerAction` 也仍是 `ProtocolSchedulerWork`。因此它只是
  Phase 3 cutover 的观测前置，不得宣称 Browser Owner queue 已独立；
- 开启 trace 的真实 raw CDP probe 从 command navigation 加载 Document A，A 的 timer 再执行
  `location.href` 到 Document B，全程不发送额外 progress/noop command。结果为 `loads=2`、最终
  `document.title=B`；command action `1/request 1` 从 Page generation `1 -> 2` 并完成 DCL/load，随后
  renderer action `2/request 2` 从 generation `2 -> 3`，其 source Document 为 Page `2`，successor DCL/load
  为 Page `3`。这证明同一 correlation key 能区分连续真实 Document，未把前一个 DCL 判假或把 frontend
  command 当 progress pump；
- identity/request sidecar、stale Page action、scheduler residence、session reattach、`document.open()`、
  replacement 和 exact DCL/load 聚焦集 `14/14` 通过（nextest run
  `4a22eaab-6cd1-435b-91ac-20b3f6432d97`）；新增 projection sequence order-domain 用例 `1/1` 通过
  （run `032743e7-827c-4271-bd6f-4b0976935b31`），renderer action clone/distinct identity 用例 `1/1`
  通过（run `5cab76c4-5ace-479b-965a-1ab1df9f4b80`）；相关五个 crate all-target clippy
  `-D warnings` 通过；
- workspace clippy 首轮准确暴露 trace context 使 `ProtocolSchedulerWork` 变大后，durable
  `ProtocolSchedulerResidence` 的 inline variant 达到约 `377B`。修复只在该 residence storage seam 把
  concrete work 改成 `Box<ProtocolSchedulerWork>`；publication order、client-turn predecessor、exact load
  predecessor 与 completion API 均不变。对应 `cdp_scheduler` 邻域 nextest `41/41` 通过（run
  `60a4ccc5-13eb-41c1-95fc-759877086085`），最终 workspace clippy `--all-targets -- -D warnings` 通过；
- 最终 workspace nextest `15618/15618`、`17 skipped`（run
  `12703160-1ce1-4bf3-a3c6-d0240875cedd`）。workspace fmt check、diff check 与 release workspace build
  均通过；被测 `target/release/moli` SHA-256 为
  `24010b0323b713dd570ef98a7905ef795591310e4e2591f94759b6ea46e8377a`。显式清除大小写全部代理变量、
  设置 `NO_PROXY=*` / `no_proxy=*` 并固定该 release binary 后，CDP 默认全组和 WebDriver
  Classic/BiDi/Selenium 均返回 `ok=true`，没有失败记录。

本切片后 Phase 0 仍是“部分完成”：下一步要建立可机器比较的 CLI/CDP/BiDi/Classic 同源 trace fixture，
把 Browser fact journal 的 sequence 接入，细化剩余 field inventory，并冻结 release latency/RSS/CPU baseline。
在这些证据完成前，不开始 Phase 3 execution-authority cutover。

2026-08-02 第十八切片实现记录（machine trace transport）：

- `moli-trace` 新增 schema version `1` 的 `BrowserOwnerTraceRecord` 和
  `BrowserOwnerTraceDocument`。字段与 human trace 同源，Document 拆为 renderer Page id、Document
  generation 和 lifecycle epoch；renderer lifecycle kind/reason/last-reached 使用稳定 kebab-case label，
  不要求 benchmark 解析 Rust `Debug` 文本；
- 显式设置 `MOLI_BROWSER_OWNER_TRACE_JSONL=<path>` 时，每条完整记录在 process-local mutex 下追加为
  单行 JSON 并立即 flush。sink 仍默认关闭；打开 JSONL 不再隐式打开 human `tracing` 输出，原
  `MOLI_BROWSER_OWNER_TRACE=1` 继续单独控制人类日志。文件 open/serialize/write 失败不 panic、不卡住
  Browser Owner，consumer 把缺失或截断记录当自己的 gate failure；
- schema 明确不包含 URL、cookie、authorization、body、frontend session 或 wire command payload；所有
  尚未建立的 authority 字段（尤其 `browser_fact_sequence`）保留显式 `null`，不能把不完整 correlation
  误当完整 Browser fact；
- Core typed action/request trace、renderer intent 和 renderer exact-Document lifecycle 都写入同一个 JSONL
  sink。schema/lifecycle-label/identity/request 聚焦 nextest `6/6` 通过（run
  `aa2b9df4-69c0-4517-a2d1-e15d36f4a5b3`），相关四个 crate all-target clippy `-D warnings` 通过。真实
  CLI probe 只设置 JSONL path、未设置 human flag，stderr 没有 Browser Owner human record；JSONL 为合法
  schema `1`，按 `started -> dom-content-loaded -> load -> terminated` 输出同一个 exact Document。

本切片只稳定 trace transport，不新增 journal、subscriber 或 owner queue。下一切片由 benchmark 模块消费
JSONL，建立 CLI/CDP/BiDi/Classic 同源 fixture；在 fixture 和 baseline 完成前 Phase 0 仍是“部分完成”。

2026-08-02 第十九切片实现记录（cross-frontend trace fixture 与第一版 release baseline）：

- `moli-benchmark navigation-trace` 新增同一个本地 HTTP source 驱动的 standalone CLI、raw CDP、
  standalone WebDriver BiDi 和 WebDriver Classic 四路径 differential。Document A 在自己的 `load` handler
  中同步设置 `location.href` 到 Document B，B 的 `load` handler 请求 `/complete`；fixture server 按 run token
  记录 `a -> b -> complete` 的事实顺序，并通过 condition 等具体请求，不用固定 sleep、retry 或浏览器轮询；
- benchmark 代码按责任拆为 `navigation_trace_fixture`（HTTP source/factual wait）、
  `navigation_trace_records`（schema/identity normalization）、`navigation_trace_frontends`（四类 client）和
  `navigation_trace`（gate/report orchestration）；CLI/serve 只提供命令注册和 diagnostics env/resource sampler
  参数，不把 protocol client 或 trace parser 重新揉进总入口；
- 这里刻意使用 load-boundary 内已经产生的 navigation，而不是未知 future timer。standalone 当前
  `Done == Load`，但 `FollowBeforeReply` 必须先消费同一个 load owner turn 已经产生的 replacement；本 fixture
  不偷改 `Done` 产品合同。future-timer/challenge 是否进入 B 仍由明确的高层 wait policy 负责；
- CDP/BiDi/Classic 各自先加载 bootstrap Document，使用一次**测量前**的读取命令切开 bootstrap JSONL；正式
  A navigation 发出后，在 B 的真实 load 和 `/complete` 到达前不再发送 frontend command。因而这个 barrier
  只隔离 trace，不是推动 A -> B 的 heartbeat。CDP/BiDi 还分别要求自己的 wire lifecycle 精确为
  `A:DCL -> A:load -> B:DCL -> B:load`；Classic 在 `/complete` 前不读取 URL/title/source；
- consumer 校验 schema version、完整 JSONL record boundary、HTTP request 顺序、两个 exact renderer
  Document 的 DCL/load、最终 URL/title/body phase，以及 post-navigation frontend command 数为零。它把
  process-local action/request/Page/Document 数字首次出现顺序归一化为 `A1/R1/P1/D1...`，排除
  `frontend_*` projection record 后比较 CDP/BiDi/Classic 的 Browser Owner transition shape；projection 仍由
  各自 wire gate 单独检查，不能反向成为 Browser execution authority；
- suite 输出 `runs.csv`、完整 `runs.json`、`cross-frontend.json`、`summary.json` 和每次原始 JSONL。process
  resource sampler 对这个短 workload 使用 10ms interval，记录 process-tree PSS/RSS 和 sampled lifetime CPU；
  latency scope 明确区分 CLI 的 cold process-to-successor-load 与三种协议的 warm host
  command-to-successor-load，不能把四个延迟数字直接当同口径排名；
- Python fixture/schema/normalization/CLI/env 聚焦 unittest `24/24` 通过，benchmark 全量 unittest
  `362/362` 通过。debug binary 的 bounded stress 为 10 轮、40 个 frontend run 全部成功，10/10
  visible-shape 与三协议 owner-shape 都一致，没有重试失败轮次；
- release binary SHA-256 为
  `30a8a37fbe527d55d5df98f1ecc1fc13d40425e6f2a7474b85c00861728d903b`。命令
  `moli-benchmark navigation-trace --runs 10 --timeout 10` 的 artifact 位于
  `/tmp/moli-navigation-trace-rebased.YbmygW`；40/40 frontend run、10/10 cross-frontend gate
  通过。第一版数值如下（median / p95）：

| frontend | navigation ms | peak PSS MiB | peak RSS MiB | sampled CPU % | samples/run |
| --- | ---: | ---: | ---: | ---: | ---: |
| CLI | 29.71 / 31.32 | 28.89 / 63.62 | 31.20 / 65.82 | 0.0 / 100.0 | 2 / 2 |
| CDP | 15.10 / 16.56 | 88.66 / 93.94 | 91.24 / 96.47 | 50.0 / 63.63 | 5 / 5 |
| BiDi | 16.16 / 16.75 | 82.91 / 86.20 | 85.38 / 88.59 | 53.55 / 62.32 | 4 / 5 |
| Classic | 19.78 / 34.86 | 82.01 / 84.05 | 84.44 / 86.35 | 42.7 / 50.0 | 6 / 6 |

同一工作树已 rebase 到 `origin/master` 的 `2d4f2baf47356903bfbef0c5f30d282420b5042f`。rebase 后最终 gate
为 `cargo nextest run --no-fail-fast` 15630/15630 通过、17 skipped（run ID
`4d3c3d0c-683b-4a0b-9681-9ff58761ba2f`），benchmark unittest 362/362、fmt、diff check、workspace
clippy 和 release workspace build 均通过。显式清除大小写全部 proxy 变量、设置 `NO_PROXY=*` /
`no_proxy=*` 并固定上述 release binary 后，CDP 默认全组 210/210、WebDriver
Classic/BiDi/Selenium `--continue-on-failure` 148/148；两者 `ok=true`、失败列表为空。

CLI 只有 1--2 个短进程 sample，CPU 的 `0/100` 量化尤其弱，PSS median 也可能漏过短峰值；当前 raw
artifact 保留 sample count，后续 cutover 比较必须使用相同 sampler 和 latency scope，并优先看 p95/重复轮次，
不能把这组 CPU 数字包装成精确性能结论。event-heavy、多 Target 和 idle footprint 仍由 startup/其它 Phase 0
workload 补齐。

本切片仍不新增 Browser fact journal 或独立 owner queue。它冻结了四种 Moli frontend 的第一条同源
machine gate 和 release 基线；Phase 0 仍为“部分完成”，因为 `browser_fact_sequence`、更细字段 inventory、
Chromium machine differential 以及 event-heavy/多 Target/idle baseline 尚未全部收口。

2026-08-02 第二十切片实现记录（renderer Browser Owner lane 第一刀）：

- Core 按责任新增 `browser_host/owner_input.rs` 与 `owner_queue.rs`。前者只定义 protocol-neutral
  `BrowserOwnerInput` / renderer intent，payload 只含 exact Page residence、source Document navigation 和
  可选 trace；后者只实现 single-owner FIFO，不含 session、command id、domain subscription、socket 或
  protocol predecessor；
- renderer prepared output 现在发布 `BrowserOwnerInputPublished`。该 migration envelope 只把 neutral input
  交给 application composition root，不分配 `ProtocolWorkPublishSequence`，也不进入
  `ProtocolSchedulerResidence`；旧 `TopLevelLocationNavigationOwnerAction` 文件、payload kind、ready branch 和
  protocol execution path 已删除，因而同一 renderer action 不存在 old/new 双 execution authority；
- raw CDP、standalone BiDi 和 Classic session actor 都在各自 loop 顶部执行同一 selection rule：先完成已经成为
  authoritative fact 的 exact old-Document load projection，再检查 Browser Owner queue；两者都没有时才接受
  socket command、renderer publication 或普通 adapter work。这里的 load 优先级只覆盖 terminal 已 ready 的
  front residence（包括跨过其既有 client-turn boundary、但尚未 attachment 的状态）或已 attachment 的 terminal，
  pending observer 不会阻塞 replacement。external navigation wait 仍先 drain Browser Owner queue，再检查其
  同步 protocol lifecycle snapshot；
- 这里的“下一 turn”不是递归调用 renderer-output producer。producer turn 只 capture/publish input 并返回；
  queue turn 才执行 navigation。Browser Owner selection 不读取 Page-domain enable 或 client-turn predecessor。
  边界回归把一个未满足 client-turn predecessor 的 protocol residence 与 ready owner input 同时放入
  scheduler，证明 owner input 能完成且不会满足、消费或进入 frontend queue；
- `CdpConnection::complete_browser_owner_input_turn` 是迁移期 physical Page/event projection adapter：它只能执行
  application queue 已选择的 neutral input，不能建 residence 或决定运行时机。该 adapter 与
  `CdpScheduler` composition 持有 queue 的形状必须在后续切片收敛为 `BrowserHostActor/Handle`；本切片不把
  它冒充最终 Browser Host lifetime，也没有提前建立 Phase 5 fact journal；
- stale exact Page residence 仍在 owner lookup 处拒绝；same-Page `document.open()`、frontend session reattach
  和 runtime output barrier 保持原语义。`TestContext` 也新增独立 Browser Owner pending queue，并只在下一
  scheduler turn 消费；依赖旧同步 residence 的 4 个 fixture 改为等待真实 scheduler state，而不是恢复同步
  执行或发送 noop command；
- actor 回归新增 Page domain 从未 enable 的 parser-blocking `location.href`：`Page.navigate` response 后不再
  发送任何 frontend command，replacement HTTP request 仍自主开始。原 Page-enabled passive-progress 用例继续
  验证 replacement Document 的 exact loader/DCL；Core FIFO/exact Page、protocol stale/reattach/
  `document.open()`、Runtime barrier 和 application queue/client-predecessor 边界均有聚焦覆盖；
- 第一次 post-pull release trace 暴露出一条跨 queue 因果缺口：renderer 已按 exact Document 发布 A 的 load
  fact 和 `A -> B` intent，但 raw CDP 的 adapter 尚未来得及把 ready load residence 跨过 client-turn/attachment
  边界，loop 顶部便先选择 B。结果 HTTP `a -> b -> complete` 全部成功，而 CDP wire 稳定缺失 `A:load`（0/10）；
  Classic 同一顺序出现 1/10 波动。问题不是 403/DCL 虚假，也不是 pending load 应全局阻塞 owner，而是两个独立
  queue 之间遗漏了“已成为事实的旧 Document terminal 必须先投影”这条局部 edge；
- `ProtocolAdapterScheduler` 现在保留 exact load observer 的只读 readiness probe。若 front load residence 已
  terminal、但尚未 attachment，它复用既有 coalesced adapter self-turn 依次跨过 producer boundary、建立
  attachment、消费 terminal；若 attachment 已存在则从专用 completion channel 消费，普通 self-turn 不能插队。
  三个 frontend actor 只在这一窄条件下让 load projection 先于 Browser Owner；observer 仍 pending 时 owner
  立即运行，因而 replacement/termination 仍能成为产生 `Superseded`/`Unavailable` terminal 的必要前驱。实现
  没有增加 sleep、`yield_now`、retry、heartbeat，也没有把 navigation 重新放回 protocol residence；
- 这条 `load_projection_precedes_browser_owner` 是 physical Page/lifecycle 仍住在 `CdpConnection` 时的迁移期
  safety edge，不是最终 Browser Host 可以等待 CDP frontend flush 的许可。独立 actor 完成后，应由 Browser
  lifecycle ingress/fact journal 先记录 A load fact、再接受 B；frontend 只按 fact sequence 异步投影，即使 writer
  慢或已断开也不能反向延迟 B。该 edge 必须随 physical lifecycle authority 一起迁入 Browser Core，而不能把
  `ProtocolAdapterScheduler` 搬进 `BrowserHostActor`；
- WebSocket 回归新增 load handler navigation，锁住 source loader 的 `Page.lifecycleEvent(load)` 必须先于
  replacement loader 的 `Page.frameNavigated`，同时保留 parser-blocking/Page-disabled 两条自主进度覆盖；该
  exact 回归 nextest stress 20/20 通过。修复后的 release navigation trace 为 CLI/CDP/BiDi/Classic 各 10/10、
  cross-frontend 10/10、`gate_failures=0`，没有放宽 benchmark 的 owner-stage 或 wire lifecycle 断言；
- renderer action 的 trace `browser_action_published.owner_state_after` 从历史
  `protocol-scheduler` 改为 `browser-owner-queue`。frontend command action 仍是 Phase 4 前的既有路径，不能把
  这项 trace 变化误写成 command navigation 已经汇合。

本切片的 queue 无外部直连 producer；每个 application actor 在接收下一 external input 前持续优先 drain，
而每个 Page prepared-output slot 最多保留一个 top-level navigation，因此不会跨 external turn 无界累积。Phase 4
扩大 command/popup/termination source 前仍需冻结正式 channel capacity/coalescing/backpressure policy；不能只靠
当前结构性 drain 假设宣称最终资源 gate 已完成。

当前 raw/BiDi/Classic loop 仍要等产生该 input 的 renderer-publication turn 完成现有 protocol output routing 后，
才回到 loop 顶部选择 queue。因此“slow writer 不延迟 replacement start”和“frontend disconnect 后 accepted
action 继续”这两条 actor regression 尚未通过结构性证明；它们要求下一切片把 Browser Host turn/lifetime 从
frontend flush 返回值中再拆开。这里不能把“queue 不读取 socket 状态”偷换成“当前控制流已不等待 socket”。

本切片早期证据：Core FIFO/exact Page `1/1`（nextest run
`fc60960a-5f42-4f19-9f59-3829f3a09121`）；application queue + Page-enabled/Page-disabled passive actor
回归 `3/3`（run `cc71edc4-0aee-492a-ae49-ea765eceb542`）；`moli-protocol` 全包
`3233/3233`（run `d26334c5-8c61-4dc2-9c0e-9cb393884877`）；navigation-trace Python 单元
`6/6`；`moli-core` / `moli-protocol` / `moli` all-target clippy `-D warnings`、workspace
fmt check 与 diff check 通过。最终 post-pull release binary SHA-256 为
`086086d68c9da8a17b42d157df9d1f39068e837d90ca7fd2d44303f086e20569`；四 frontend 10 轮 artifact 位于
`/tmp/moli-owner-queue-trace-final.l8ZpOZ`，40/40 frontend run、10/10 cross-frontend gate、零失败。
新增 exact actor 回归 stress 20/20（run `03032ed4-59fb-4e85-851c-c6956c45aa1b`）；最终
`moli` + `moli-protocol` 包级 nextest 3789/3789（run
`746f82a3-e114-4faf-abf9-ab08c836fb75`），navigation-trace unittest 6/6，三个受影响 crate all-target
clippy `-D warnings` 通过。显式清除大小写 proxy、设置 `NO_PROXY=*` / `no_proxy=*` 并固定上述最终 binary 后，
CDP 默认全组 210/210、WebDriver Classic/BiDi/Selenium 148/148，二者 `ok=true` 且失败列表为空。按本轮
约定没有重复 workspace 全量 nextest。

### Phase 3：建立独立 Browser Owner lane

状态：已完成（第48切片 exit audit）。第20切片已完成 renderer top-level navigation 的 queue/residence cutover，并让 CDP、BiDi、
Classic application actor 在接收下一 frontend/renderer input 前先结算 causally-ready exact lifecycle
projection、再执行 ready Browser Owner input；第21切片已把 Page replacement/residence transition 的 stale
permit 改为 typed outcome，并关闭 Page/Target handle 的 Acquire/Release 审计；第22切片已把 Target
termination 的 stale/Crash-Recovering divergence 改为可回滚 typed commit；第23切片已把 Target engine
adoption 的 Core/physical owner divergence 改为 typed rejection，并由 Core 决定 selected/retained residence；
第24切片已把 BrowserContext registration/activation/removal projection 的 miss/divergence 改为可回滚 typed
transaction；第25切片已把 Target registration/activation/rollback projection 改为完整 identity/capability
校验与 same-turn staged transaction；第26切片已把 Core Target registry 的 context/Target registration、active
replacement、activation、removal 与 handle lifecycle 发布改为 registry-owned exact transaction；第27切片已把
Core Page registry 的 context/Target registration 改为 staged/live exact transaction，并与 Target/engine participant
形成可回滚的联合提交；第28切片已把 default BrowserContext bootstrap 的 initial-Document creation metadata
并入同一个 Core registration transaction，删除 Protocol 的 production 补注册与 `expect`；第29切片已把
physical Page residence participant 改为 Core commit 前完成 exact staging/typed validation、Core rejection 时恢复
原 context/target slot 的联合事务；第30切片已把 renderer same-document history 更新改为 exact Page + typed Core
commit，并让 physical Target metadata 与 frontend event 都服从该 commit 结果；第31切片已建立 Core
`BrowserHostActor` mailbox 与 cloneable `BrowserHostHandle`，删除 renderer input 的
`CdpSchedulerEvent::BrowserOwnerInputPublished` envelope；第32切片再以 Core-issued、non-cloneable
`BrowserHostTurn` capability 收回 FIFO selection/execution permission，删除 application/fixture 的 raw input
pop 与 `CdpConnection` 的 raw-input completion API；截至第32切片，actor 仍由 `CdpScheduler` composition root
驱动。第33切片已把 actor residence 移出 `CdpScheduler`，并让三种 frontend loop 和 direct command wait 直接
监听 mailbox wake；physical Page executor 仍借用 `CdpConnection`，actor teardown 也仍跟随 application owner
loop，typed outcome/fact channel 尚未建立，因而本阶段 Exit gate 尚未宣告通过。
第34切片进一步把 Core executor contract 收紧为同步启动，并在 application composition 建立 exact participant
completion mailbox；等待 renderer/network participant 时不再持有 Browser Host actor 或 `CdpConnection`。但
第35切片又把 response-ready completion 的 configuration/renderer commit wait 从 direct 与 background apply
turn 中移出，并让 background gate 延迟到 terminal phase 才结算。第36切片继续把 response-ready tail 的每个
renderer Inspector replay dispatch 拆成独立 participant；commit completion 在 replay batch 结束前保持同一 gate
为 non-terminal。第37切片进一步让 generic materialized outcome 在 body apply 后复用同一 tail participant seam，
不再由 Browser Host completion inline drain Inspector replay。第38切片将 generic loaded Page restore wait 移入
move-owned participant；第39切片将旧 Page close 移入 disposal participant；第40切片删除 disposal completion 中
DedicatedWorker registry retirement 的伪异步 wrapper，使它在 exact owner apply turn 同步提交。第41切片又把 loaded
lifecycle/activity prefix 改为同步投影，并在返回前发布真正异步的 load owner action。第42切片进一步把
loaded-navigation 的 BiDi preload proxy/listener/cleanup command 拆成 exact Page participant；listener 的 deferred
message reply 仍是独立 response-ready input。第43切片又把该路径的 Runtime output context-id/DOM-node normalization
拆成 exact renderer-attachment participant，使 loaded-navigation completion apply 不再跨 Page lookup wait。第44切片
又把 `Page.createIsolatedWorld` 的 realm lookup 和 preload listener batch 变成该 Page command task 的显式 participant；
replacement 后的旧 world 响应不会把 listener 工作带入 successor Page。第45切片再把 realm inventory 与 listener
batch 抽成可复用 exact-Page setup，并让 `Page.addScriptToEvaluateOnNewDocument` 的 renderer install 与
run-immediately listener startup 成为命令自身的 participant；旧 Page completion 不能进入 replacement Page。
第46切片又把普通 Runtime 与 Runtime-binding Inspector completion 的 context-id/DOM-node normalization 变成
Runtime command 自身的 participant；stale completion 会结束旧 command，而不是等待已经失去 owner 的 response
receiver。
第47切片再让 scheduler-facing protocol-neutral `ReleaseObjects` 的每个已知 handle 成为外层 Runtime command task
可见的 participant；`start_devtools_runtime_command_dispatch` 不再串行等完整个 handle list。
Fetch continue/BrowserContext disposal compatibility drain、其他 BiDi listener/direct compatibility 入口、
`CdpTurnOutcome` projection 和 Host lifetime 仍在 frontend owner loop，protocol-neutral Browser fact/outcome channel
尚未建立；第48切片 audit 将它们分别归入 Phase 4、Phase 5 和 Phase 6，不再把后续 phase 的完成条件误作
renderer-navigation lane cutover 的 blocker。

#### 当前 production compatibility 清单

截至第64切片，不能把“局部 legacy 已清”描述成整个 Browser/CDP 拆分完成。Production 仍明确保留：

1. `domains/page/navigation_tail.rs::finish_materialized_navigation_tail_async`：尚未暴露 participant boundary 的
   调用方仍 inline drain renderer replay；
2. `domains/page/navigation_commit.rs::commit_loaded_navigation_async`：尚未迁移调用方仍通过 compatibility wrapper
   等待 loaded commit；
3. `domains/runtime/dispatcher.rs::start_bidi_preload_channel_listeners_for_execution_context_*`：loaded-navigation、
   `Page.createIsolatedWorld` 与 Page command 形式的当前 Page preload install 已不再调用该 inline helper；runtime
   realm-created 和 create-target initial `about:blank` 入口仍会 inline drain；targeted protocol-neutral preload 则由
   `preload/add_command.rs::execute_direct_async` compatibility adapter 本地 drain 同一 move-owned task；
4. Fetch continue 与 BrowserContext disposal termination 的 Fetch cancellation compatibility drain：两者尚未成为独立
   Browser Host participant。`Page.stopLoading` 的 admission、当前 Document 选择和 renderer lifecycle stop 已在第63切片进入
   Host，第64切片再把其 paused main-navigation/subresource Fetch cancellation 拆成逐项 Host participant；旧
   `fail_pending_fetch_state_background_events_async` 仍供尚未迁移的 termination/context-disposal 调用方使用，但只兼容地本地
   drain 同一个 exact-Page cancellation 状态机，不再维护另一份 cancellation 实现；
   direct `wait:none` 的 admission/start 已在第62切片进入 Browser Host，但 detached load completion、lifecycle 与 Network
   output 仍通过既有 background navigation completion/output transport 回到 application owner loop；
   Page.crash/Page.close 已在第58切片迁移，显式 `Target.closeTarget` 已在第59切片迁移为 exact Host input +
   disposal/promotion participant，第61切片又删除 paused-fetch cancellation 的 Page/Target termination
   `ProtocolSchedulerWork` admission。command renderer predecessor 仍由 command completion fence 先投影，但不再拥有或延迟
   Browser input；
5. `complete_runtime_protocol_message_for_session_owner_async` 已成为 `cfg(test)` fixture helper，production caller
   清零；scheduler-facing `ReleaseObjects` 已使用逐 handle participant chain。无 scheduler participant loop 的 direct
   protocol-neutral adapter 仍会本地 drain release/normalization，runtime realm-created、initial `about:blank` 与部分
   direct/replay 路径也仍可能在 frontend owner loop 内等待 renderer participant，且尚无 protocol-neutral Browser
   outcome/fact channel；
6. `BrowserHostActor` teardown 仍跟随 frontend/application owner loop，Browser Host lifetime 与 frontend lifetime
   尚未分离。

第42--47切片关闭的是 loaded-navigation owner gate、`Page.createIsolatedWorld`、Page command add-preload、普通
Runtime/Runtime-binding command 与 scheduler-facing `ReleaseObjects` 内会隐藏 owner participant 的真实 wait；它不宣称
所有 BiDi listener/direct adapter 或所有 Runtime completion 调用方已完成迁移。第58切片关闭 Page-originated
termination execution authority，第59切片继续关闭显式 top-level `Target.closeTarget` 的 execution authority；两者都不改变
上述 Runtime compatibility 边界。第60切片又删除 popup/auxiliary navigation 的 Protocol scheduler execution
payload；这同样不表示 Runtime compatibility wrapper、paused-fetch admission 或 Host lifetime 已经迁完。
第61切片删除了该 paused-fetch admission，但不表示 Fetch continue、BrowserContext disposal、fact channel 或 Host lifetime
已经迁完。第62切片关闭 direct `wait:none` 的旧 Page admission path，但 detached navigation completion 仍通过既有
background completion/output transport 回到 owner loop；这条 transport 要由 Phase 5/6 的 neutral fact/outcome 与独立
Host lifetime 收口。第63切片又关闭 `Page.stopLoading` 的 frontend execution authority，第64切片进一步关闭它在 renderer
stop 之后隐藏的 paused-Fetch inline wait；但 cancellation output 仍是 Protocol event/projection，其他
termination/context-disposal caller 也仍通过 compatibility drain 执行，所以不能把“stopLoading participant 已完整”写成
“neutral fact channel 或 Host lifetime 已完成”。

#### Phase 3 防雕花判据与 exit audit

Phase 3 的目标不是消灭 Protocol 中每一个 `await`。frontend 可以等待自己的 response；只有满足至少一项的等待才必须
迁成 Browser/Renderer owner participant：

1. frontend 不继续 poll/dispatch 时会阻止 Browser Owner 导航或其他浏览器事实推进；
2. 跨等待持有 Browser Host、`CdpConnection` 中的共享 physical owner 或不可冻结的 authority；
3. completion 可能按 target/session 重新发现 Page，从而进入 replacement Document；
4. slow/disconnected frontend 会改变 browser-owned navigation trace、commit/fact 顺序或 terminal disposition。

第48切片已逐项完成该 audit：无 frontend input 和 Page domain disabled 时 Browser Owner 都会自主推进；bounded socket
writer enqueue 不等待 peer，overflow 只关闭该 frontend；page/browser frontend detach 后 owner 与 Target 仍存活；剩余
Runtime/preload direct adapter wait 只延迟自己的 command/adapter，不拥有 renderer top-level navigation queue。它们只有在
后续实际命中上述四条判据时才迁移，不能再以“Phase 3 还有 await”为由扩大本阶段。

目标：让 renderer navigation 完全离开 protocol residence。

工作：

1. application/local runtime 启动 `BrowserHostActor` 和 `BrowserHostHandle`；
2. renderer intent 直接进入 Browser Owner input；
3. `TopLevelLocationNavigationOwnerAction` 从 `ProtocolSchedulerWork` 删除；
4. Browser Owner 自主 schedule next turn；
5. renderer/network participant completion 通过 typed `CompletedBrowserHostTurn` input 返回 owner loop；完整
   navigation/lifecycle outcome/fact journal 按 Phase 5 建立，不作为本阶段的隐藏追加 gate；
6. 保持同进程、同 current-thread runtime，不先引入跨线程 V8 handle。

2026-08-03 第四十八切片实现记录（Phase 3 exit audit）：

- production type audit 确认 `ProtocolSchedulerWorkKind` 只剩 protocol observation、main-document load、BiDi channel、
  popup navigation 与 Target termination；renderer `TopLevelLocationNavigation` prepared output 直接调用
  `publish_prepared_top_level_location_navigation_input`，失败时返回 typed Host publication error，既不会重建
  scheduler fallback，也不会执行 navigation；
- Core `BrowserHostActor` 独占 FIFO selection 并签发 non-cloneable `BrowserHostTurn`。application 的
  `BrowserHostExecutionLane::recv_wake` 直接监听 actor mailbox 和 exact participant completion；CDP、BiDi、Classic
  loop 与 direct wait 都把该 wake 作为独立输入，不需要下一 frontend command 或 loop-top polling；
- slow frontend 的 wire boundary 是 bounded `CdpSocketSink::enqueue_message`：enqueue 同步返回，queue/byte overflow
  只请求关闭该 sink。`CdpFrontendRouter` 对每个 frontend 独立路由，失败不会 await writer，也不会停止 Browser Host。
  frontend detach 只移除 route/session projection；已有 dynamic Target 与 renderer state 继续存活；
- compatibility 清单重新分类：popup/Target termination 是 Phase 4 尚未汇流的真实 browser-owner action；
  `CdpTurnOutcome`/BackgroundProtocolEvent 到 neutral fact 的迁移是 Phase 5；actor 与 frontend/application teardown
  分离是 Phase 6。navigation tail、preload direct adapter 和 Runtime local drain 当前不持有 renderer top-level
  navigation execution authority，不能为了清零 wrapper 留在 Phase 3；
- 聚焦证据：parser script navigation 在无后续命令和 Page domain disabled 两种 production WebSocket 路径均通过
  `2/2`（run `b410b739-c869-4037-a155-51668d8bd48a`）；browser frontend detach 后动态 Page/Target 保持存活
  `1/1`（run `b66755b2-c624-4fb0-bd11-3cfc7a83c340`）；Browser Host mailbox 独立于 protocol client-turn
  predecessor `1/1`（run `6c38cd00-bcf2-4347-bd60-321fd5407fd8`）；renderer intent 无 protocol scheduler
  fallback `1/1`（run `6b71fab6-b434-43fc-a896-1030d7bbf07c`）；
- Phase 3 Exit gate 因此通过。下一 production slice 进入 Phase 4，把 frontend `Page.navigate` command 与 renderer
  intent 汇入同一 Browser Owner queue；不得先回头继续清 Runtime/preload wrapper，也不得把 Phase 5/6 的 gate
  偷塞回 Phase 3。

#### 独立 BrowserHostActor 前的 panic / 并发安全审计

2026-08-02 评审确认下面 12 组既有问题；第18--20切片没有引入它们。第21切片已关闭第 1、9、12 组，
第22切片已关闭第 2 组，第23切片已关闭第 3 组，第24切片已关闭第 4 组，第25切片已关闭第 5 组，第26切片
已关闭第 6 组，第27切片已关闭第 7 组，第28切片已关闭第 8 组，第29切片已关闭第 10 组，第30切片已关闭
第 11 组。12 组既有问题至此均已有独立内聚切片和回归证据；这只完成 actor 前的安全审计，不表示 Browser Host
lifetime/queue 已脱离 frontend stack，也不授权提前把 physical Page/V8 payload 移到跨线程执行：

1. **第21切片已关闭**：Core `page_replacement.rs` 的 commit 重新验证 exact request、Target recovery 和 Page
   generation，返回 `BrowserPageReplacementCommitError`；request 预提交在 Page CAS 失败时同 turn 回滚；
2. **第22切片已关闭**：Core `target_termination.rs` 的 commit 返回
   `BrowserTargetTerminationCommitError`；stale Page、重复 Crash 及 Crash/Recovering 冲突不再触发 production
   panic，Close 的 termination state、Target topology 与 Page generation 通过同轮 rollback transaction 保持原子；
3. **第23切片已关闭**：Protocol `browser_target_engine_handoff.rs` 不再用五处 production `expect` 处理
   Core/physical engine divergence；exact adoption 由 Core 校验 Target topology、selected engine owner 与
   physical selected-Target projection，并返回 `BrowserTargetEngineAdoptionError`；
4. **第24切片已关闭**：Protocol `browser_context/registry_projection.rs` 的 registration/activation/removal
   先验证完整 physical identity set；会移动 payload 的路径以 exact `Option`/`Vec` slot transaction 包围 Core
   commit，Core typed rejection 时恢复 selection 与原 vector index。`lifecycle.rs` 不再把 physical/Core miss
   视为 process panic；能返回 command error 的入口传播 `BrowserContextProjectionError`；
5. **第25切片已关闭**：Protocol Target topology mirror 在任何 Core mutation 前校验完整 physical Target set、
   context/active residence、exact Target handle 与 Page residence；registration、activation、rollback 传播
   `TargetProjectionError`。会移动 payload 的路径先暂存 exact BrowserContext/Target slot，Core typed rejection 时
   恢复原 selected/inactive index 与 background index；一致性断言只保留为 `debug_assert!` 诊断；
6. **第26切片已关闭**：Core Target registry 在 engine handoff 前以 exact transaction 暂存 BrowserContext/Target
   topology 与 reverse owner，并预留 handle activation/retirement；Core typed rejection 同 turn 恢复原 context、
   active/background slot 与 vector index，成功后才发布 lifecycle。missing reverse owner、context mismatch、
   non-staged/non-live handle 都返回 typed error；production registry 路径不再以 `assert!`/`expect` 处理 divergence；
7. **第27切片已关闭**：Core Page registry 以 staged/live record 和不可伪造 transaction token 管理
   BrowserContext/Target registration；staged residence 不参与 live lookup/projection，Page participant 或 engine
   拒绝时先恢复 exact Page entry、再恢复 Target transaction，成功时 Target lifecycle 最后发布。重复 owner、
   non-live residence 与跨 Context capability alias 返回 typed error，production registration 不再使用 `assert!`；
8. **第28切片已关闭**：default BrowserContext 的 active Target creation metadata 与 Context/Target/Page topology
   由同一个 Core registration transaction 接收并发布；Protocol 不再在 Core commit 后另行补注册
   initial-Document，也不再以 production `expect` 维持两步时序；
9. **第21切片已关闭**：Core `page_transition.rs` 返回
   `BrowserPageResidenceTransitionCommitError`；initial-Document materialization 预提交在 Page CAS 失败时同 turn
   回滚，不再以 production `assert!`/`expect` 处理 stale permit；
10. **第29切片已关闭**：Protocol Page residence bridge 在 Core commit 前校验完整 Target topology、exact permit
    generation/instance 和 initial slot absence，并暂存 exact BrowserContext/active-or-background Target participant；
    Core typed rejection 恢复原 selected/inactive context slot 与 background vector index，成功后不再按 id 做 fallible
    lookup；physical divergence 返回 `PageResidenceProjectionError`，一致性断言只保留为 `debug_assert!`；
11. **第30切片已关闭**：Core same-document history 以 exact Page residence + typed result 原子提交；Protocol 在
    Core 接受前不再修改 physical Target URL/origin，拒绝时也不再返回 frame id 或发布 frontend navigation event；
12. **第21切片已关闭**：Core `page_residence.rs` 的 generation read 使用 Acquire，exact generation advance 使用
    AcqRel CAS、失败使用 Acquire；`target_handle.rs` lifecycle read/CAS 使用同样配对。instance-id allocator 只负责
    唯一数字分配，继续使用 Relaxed，不承担 payload 发布同步。

处理原则不是机械把所有断言改成日志：prepare permit 应携带不可伪造的 exact generation/instance，commit 应返回
typed stale/divergence outcome；projection mirror 的诊断断言与真正可恢复错误必须区分；history 必须传播 Core
结果；跨线程可见 handle 必须建立并用测试证明 Acquire/Release 配对。上述审计现已收口，但 Phase 3 仍只允许保持
同 current-thread runtime 的逻辑 actor/queue 拆分；独立 actor lifetime、typed fact channel 和 payload residence
仍须通过各自 Exit gate，不能把当前单线程时序假设包装成线程安全。

2026-08-02 第二十一切片实现记录（Browser Host actor 前的 exact commit safety）：

- `BrowserPageResidenceRegistry::commit_replacement/commit_transition` 不再返回无原因的 `Option`，而是用 exact
  instance + generation compare/exchange 返回 `UnknownTarget`、`ProjectionMismatch`、`StaleTransition` 或
  `GenerationExhausted`；stale permit 是可观察 owner rejection，不是 process invariant；
- loaded replacement 先把 exact pending navigation 预提交为 committed，再执行 Page generation CAS；CAS
  失败会用私有 rollback token 恢复 previous committed request/trace 与 pending request/trace，history、recovery、
  initial-Document exit 都不会提前变更。成功后其余 owner mutation 保持同 turn、无 fallible participant；
- initial empty Document materialization 同样在 Page CAS 前预提交 lifecycle bit，CAS 失败精确恢复；failed
  navigation discard 只在 generation 成功后清理 runtime/request/termination state；
- protocol migration adapter 把 commit stale 当作 candidate 失效：loaded candidate Page best-effort close，initial
  materialization 返回既有 `Stale`，failed-navigation discard 保持原 Page；没有 retry、sleep 或吞掉 successor；
- `BrowserPageResidenceHandle` 和 `BrowserTargetHandle` 的共享状态已建立 Acquire/Release 配对。这里关闭的是
  generation/lifecycle capability 的跨线程可见性，不代表 renderer `Page`/V8 payload 可以跨线程移动；Phase 3
  仍保持 current-thread Browser Host actor；
- 新增 regression 覆盖 request supersede、Page generation stale 与 initial materialization stale 三条
  prepare/commit 漂移路径，并验证 request/history/lifecycle rollback；聚焦 3/3（run
  `3d839c5e-f514-466e-a38d-27536f805e1a`）。最终 `moli-core` + `moli-protocol` + `moli`
  包级 nextest 6332/6332、13 个既有 skip（post-rebase run
  `64c3ad4c-59a2-4461-be32-26ef8a7832a4`），三个受影响
  crate all-target clippy `-D warnings`、workspace fmt/diff check 通过；release binary SHA-256 为
  `01feef3f36b4139b823b5662cfdad29df7031354393f550c9fa926dde77d555f`。显式清除大小写 proxy、设置
  `NO_PROXY=*` / `no_proxy=*` 并固定该 binary 后，CDP 默认全组 210/210、WebDriver
  Classic/BiDi/Selenium 148/148，二者 `ok=true` 且失败列表为空。分支已通过 `git pull -r origin master`
  rebase 到 `92294b5ca4`，上述三包、clippy、release 和 smoke 均为 post-rebase 证据。本切片未重复 workspace
  全量 nextest。

2026-08-02 第二十二切片实现记录（Target termination exact commit safety）：

- `commit_target_termination` 不再假设 prepare permit 永远新鲜。commit 重新验证 exact Target instance，先用
  私有 rollback token 预提交 `Crashed/Closed`，并将重复 Crash、Crash permit 撞上 `Recovering`、已关闭 Target
  映射为 `BrowserTargetTerminationCommitError::TargetNoLongerAcceptsTermination`；这些是 owner rejection，不是
  process invariant；
- Close 在推进 Page generation 前执行私有 `BrowserTargetRemovalTransaction`：从 active/background topology 与
  reverse owner map 暂存 exact record，但保持 Target handle live。Page CAS 失败时按原 residence/index 恢复
  topology 和此前的 `Live/Crashed/Recovering` termination state；成功后才 retire handle 并清理 engine、request、
  history 与 initial-Document state。因而 stale commit 不会退休 Target、丢 pending recovery、清 history/runtime，
  也不会留下 Page-generation/Target-topology 半提交；
- `BrowserPageResidenceRegistry::commit_termination` 复用 typed exact generation CAS，返回
  `BrowserPageResidenceRegistryError`，不再把 stale/unknown residence 压成 `Option`。Target owner lookup 对 retired
  handle、缺失 context topology 和 reverse/topology divergence 也返回 typed `BrowserTargetRegistryError`；
- Protocol 仍在同一 actor turn 同步投影成功结果，但现在显式处理 Core commit rejection。per-session Inspector
  crash-delivery 位移到 Core 成功之后写入，避免 owner rejection 后产生“Browser 未 crash、frontend 已记录
  crash”的假状态；本切片没有新增 await、retry、sleep 或 frontend authority；
- 新增 Core regression 覆盖 prepare 后 Page generation stale、第二个 Crash permit 撞上 Recovering、Recovering
  Close 在 Page CAS 失败后恢复 active topology 与 exact recovery authorization。Core 聚焦 12/12（run
  `f069f0b9-66e1-41ce-8f84-f634c02cf866`），Protocol 聚焦 5/5（run
  `9b6c6741-a1a9-41e2-b0a1-e8c83291824b`）；最终 `moli-core` + `moli-protocol` +
  `moli` 包级 nextest 6335/6335、13 个既有 skip（run
  `6201f7cf-a5e8-48d6-9c30-7195734d9465`），三个受影响 crate all-target clippy `-D warnings` 与 workspace
  fmt/diff check 通过；release binary SHA-256 为
  `3f545e8aca9985f743430be5fa5611847e2c844f9fdbd493cb4a6c31ba4c53ed`。显式清除大小写 proxy、设置
  `NO_PROXY=*` / `no_proxy=*` 并固定该 binary 后，CDP 默认全组 210/210、WebDriver
  Classic/BiDi/Selenium `--continue-on-failure` 148/148，两者 `ok=true` 且失败列表为空。本切片按约定没有重复
  workspace 全量 nextest。

2026-08-02 第二十三切片实现记录（Target engine adoption divergence safety）：

- Core `engine_registry.rs` 新增 `BrowserTargetEngineAdoptionError`，区分 unknown/mismatched Target、selected
  engine owner mismatch 与 physical selected-Target projection mismatch。`adopt_registered_target_engine`
  在同一个 `&mut BrowserNavigationOwner` 调用中验证 exact Target membership/residence，并由 Core 自己决定
  `Selected` 或 `Retained`；Protocol 不再先读取 Core topology、在稍后的 registry mutation 中重新提交 residence；
- 对仍未携带 neutral Target key 的迁移入口，Protocol 只提交当前物理投影看到的
  `Option<BrowserPageOwnerKey>`。Core 从 selected BrowserContext + active Target registry 重新求出权威 owner，
  两者不一致时在任何 engine-registry mutation 前拒绝；没有 selected Target 时也必须同时满足 Core 与 physical
  projection 都是 unbound，不能用 idle reset 把一个已绑定 engine 静默改成 startup engine；
- navigation commit、materialized completion、initial Document build、runtime direct load 与 idle-memory reset
  已显式消费 typed result。尚未发送 `Page.navigate` response 的路径返回 CDP `-32000`，已经提前响应的异步路径
  记录带 error 的 warning，直接 runtime API 返回 `Err`，idle reset 返回
  `reason=engine-owner-diverged`；background promotion 的 physical BrowserContext 丢失也返回 typed string error。
  loaded Page 已经 commit 时，错误只替换尚未发送的 success response，不得提前返回并吞掉该 exact Document
  已产生的 DCL/load lifecycle tail；这些路径没有 retry、sleep、fallback owner 或 production panic；
- rejection 在候选 engine 写入前完成，回归验证旧 selected engine payload、owner key 与 retained registry 均不变。
  这里保证的是 engine registry 的失败原子性，不宣称 physical `Page` 与 engine adoption 已成为跨 actor 的联合
  transaction；物理 Page strong lifetime 仍在 Protocol，随 Phase 6 owner payload migration 一并消除；
- Core/Protocol、lifecycle tail 与 close/recreate fixture 聚焦回归 `11/11`（nextest run
  `53659f2e-f90a-450c-b6c4-72bc97b5c48e`）通过，覆盖 registry owner 分歧、physical/Core selected Target
  分歧、unknown exact Target、idle reset divergence、正常 selected/retained park/restore，以及同 targetId 的
  新 Target instance 必须先重新注册再加载 engine；另有回归证明 error response 会压掉后续 success response，
  但不会压掉已 commit Document 的 DCL tail；
- strict validation 后首轮 Protocol 全包没有靠重跑掩盖：`3217 passed / 20 failed`（run
  `966f08da-45df-4fd7-8b23-f51bb747f769`）。20 条失败的共同根因是旧测试先直接写
  `conn.browser_context = Some(...)` 制造 physical active Target，再调用 direct runtime load，让旧无身份 adoption
  为 Core 不存在的 Target 补绑 engine owner。fixture 已统一改走生产 `insert_browser_context` 或 exact
  `register_active_target_projection`，没有给 product path 加 fallback、跳过 Core validation 或恢复 raw residence
  选择；修复后最终 Protocol 全包 `3237/3237`（run `d53c4e1f-84a5-41fc-9824-5bf568f7e838`）通过；
- rebase 前 `moli-core` + `moli-protocol` + `moli` 包级 nextest `6339/6339`、13 个既有
  skip（run `b6632901-1217-44ed-a7b8-65752de9f92c`）通过。切片完成后按约定执行
  `git pull -r origin master`，29 个分支提交无冲突重放到 `b1970bd4d4`；因最终树吸收了 DOM、样式、Network
  测试与锁文件变化，又在 post-rebase 树上执行 workspace 全量 nextest：`15645/15645`、17 个既有 skip（run
  `984319f2-3ee0-4693-bd7e-56667f56b4f6`）。workspace all-target clippy `-D warnings`、workspace fmt/diff
  check 与 release workspace build 均通过；最终 `target/release/moli` SHA-256 为
  `b99b2674f110af22c296a2b87f427a5e6ccd5007dc5cc7e70584585fcd112f40`。显式清除大小写全部
  proxy/no-proxy 变量、设置 `NO_PROXY=*` / `no_proxy=*` 并固定该 binary 后，CDP 默认全组 `210/210`、
  WebDriver Classic/BiDi/Selenium `--continue-on-failure` `148/148`，两者 `ok=true` 且失败列表为空。

2026-08-02 第二十四切片实现记录（BrowserContext projection failure atomicity）：

- Protocol 新增 `BrowserContextProjectionError`，保留 Core `BrowserContextRegistryError`，并区分 physical context
  count、selected identity、duplicate identity、Core membership 与 exact payload miss。registration、activation、
  selected/inactive removal 不再以 `Option<bool>`、`expect` 或 production assertion 压平这些结果；完整 physical
  BrowserContext identity set 在任何 Core mutation 前验证；
- activation 和 removal 先从 `browser_context: Option<_>` / `inactive_browser_contexts: Vec<_>` 暂存 exact payload
  与原 index，再同步调用 Core transaction。Core 因 engine owner、selection、revision 或 topology projection
  拒绝时，Protocol 在返回 typed error 前恢复原 selected payload 与 vector index；Core 成功后才发布 matching
  physical selection。这里没有 await、retry、sleep、fallback owner，也没有把 physical projection 提升为
  authority；
- `lifecycle.rs` 的 worker/target physical match 不再因 Core 未注册 context 而 panic。legacy bool activation API
  对 unknown context 保持 `false`，对 divergence 记录结构化 warning；create/dispose context、create target、BiDi
  emulation/preload 与 Network lazy-default-context 等能返回 command error 的入口走 `try_*` typed API，并通过统一
  `DevToolsErrorKind::Internal` 映射传播。默认 Target bootstrap 的 context registration 若被拒绝会在 initial-
  Document metadata 之前停止，不能把本切片 error 转移成后继 bootstrap panic；
- 回归覆盖 duplicate registration、Core-known/physical-missing activation、activation engine-owner rejection、
  selected removal with successor、最后一个 selected removal、inactive 中间 vector slot rollback，以及 physical
  context match/Core miss。首个 corrupt fixture 仍试图通过第23切片已经禁止的 public engine adoption 注入错误
  owner，首轮 `3 passed / 2 failed`（run `356b719f-c676-4a11-bcb5-fc860f001151`）；fixture 随后改为合法构造
  “physical Page owner 已出现、Core engine owner 尚未发布”的迁移窗口，没有放宽 product validation。最终专项
  rollback 回归 `7/7`（run `246849ad-91c7-4bba-863a-7f6830706c34`），typed create/dispose 调用链聚焦 `6/6`
  （run `88a12609-91dd-4624-aac5-626a21bab41a`）通过；
- 最终 workspace 全量 nextest `15652/15652`、17 个既有 skip（run
  `2746090c-b070-4fb1-9126-08df723d8a03`），workspace all-target clippy `-D warnings`、fmt/diff check 与 release
  workspace build 均通过。固定 `target/release/moli` SHA-256
  `4de87f893753d47eb373fe04c70edf0de9480981f4271261a8b7e9c99c9a1544`，显式清除大小写全部 proxy/no-proxy
  变量、设置 `NO_PROXY=*` / `no_proxy=*` 后，CDP 默认全组 `210/210`、WebDriver Classic/BiDi/Selenium
  `--continue-on-failure` `148/148`，两者 `ok=true` 且失败列表为空。

2026-08-03 第二十五切片实现记录（Target projection failure atomicity）：

- Protocol 新增 `TargetProjectionError`，保留 Core `BrowserTargetRegistryError`，并区分全局/per-context Target
  count、active residence、duplicate identity、Core context membership、exact Target handle、exact Page residence、
  physical Context/Target miss。完整 physical Target set 在任何 Core mutation 前
  验证；release 路径不再以 `expect`/`assert!` 处理 registration、activation、rollback 或 topology divergence，
  完成后的镜像检查只保留 `debug_assert!` 诊断；
- registration/replacement 先从 selected `Option` 或 inactive `Vec` 暂存承载 Target 的 exact BrowserContext payload，
  Core typed rejection 时恢复原 slot/index，Core 成功后才把新 handle 与 Page residence 投影进物理 payload。
  activation 还会在 Core call 前摘下 exact background Target slot 与 parked auxiliary state；拒绝时恢复原 index，
  成功后直接消费 staged payload 完成 active/background swap，不再在 authoritative commit 后进行第二次可失败查找。
  incomplete-popup rollback 同样先摘下 exact background payload，Core 拒绝时原 index 恢复；整个 transaction 无
  await、sleep、retry 或 fallback authority；
- 模块按责任拆成 `target_projection_error.rs`（错误词汇）、`target_topology_projection.rs`（只读 exact mirror
  校验）、`target_context_projection.rs`（physical BrowserContext slot staging）和
  `target_registry_projection.rs`（Core transaction coordinator）；background Target slot 的可逆 staging 留在
  `page_state/parked.rs`，没有把 session/event orchestration 混入 owner transaction；
- `Target.createTarget` 可返回的入口把 projection rejection 映射为 Internal command error；renderer popup 及
  incomplete-popup cleanup 无同步 command response 可承载错误，因而记录包含 context/target/error 的结构化 warning
  并停止后继 projection。cleanup 在 owner rejection 前不再先拆 tab/page session；误传 active Target 时无损拒绝，
  不再触发 production panic。Target promotion 不再把 Core rejection 压成 `None`/`String`，而是传播
  `BrowserTargetPromotionError`；`Target.activateTarget` 将 owner/projection rejection 映射为 Internal，只有真正
  unknown Target 仍返回 NoSuchTarget，`Page.bringToFront` 的既有 string 边界只在最外层格式化 typed error；
- 回归覆盖正常 registration/activation、bootstrap active replacement、staged rollback、同 public id 错 Target
  handle、错 Page residence、physical-only rogue Target，以及 duplicate Core registration rejection 后 exact physical
  Context slot 恢复，并增加 inactive Context 中间 slot rollback。当前最终树的 projection + activation + popup
  rollback 调用面聚焦 `27/27`（其中 projection 专项 `9/9`，run
  `88d45b9e-1834-4f7c-853c-3fb38e7c1851`），最终 Protocol 全包 `3246/3246`、protocol all-target clippy
  `-D warnings` 通过；
- 最终 workspace 全量 nextest `15654/15654`、17 个既有 skip，workspace all-target clippy `-D warnings`、fmt/diff
  check 与 release workspace build 均通过。固定 `target/release/moli` SHA-256
  `140f8c246052ea501febabadeda5ea2bb4357574ef1de21a704b24c14b2555b3`，显式清除大小写全部 proxy/no-proxy
  变量、设置 `NO_PROXY=*` / `no_proxy=*` 后，CDP 默认全组 `210/210`、WebDriver Classic/BiDi/Selenium
  `--continue-on-failure` `148/148`，两者 `ok=true` 且失败列表为空。本切片提交后执行
  `git pull -r origin master`，`origin/master` 为 `b1970bd4d4` 且分支已是最新；上述 workspace、clippy、release
  和 smoke 均为 pull 后最终树证据。

2026-08-03 第二十六切片实现记录（Core Target registry lifecycle transaction）：

- `BrowserTargetRegistry` 不再在 validation 与不可回滚 commit 之间依赖“调用方不会插入可失败步骤”。
  BrowserContext registration/removal、background/active Target registration、bootstrap active replacement、
  background activation 与 Target removal 都先创建 registry-owned exact transaction；engine handoff 拒绝时恢复
  reverse owner、active/background slot 和原 vector index，成功后才消费 transaction。activation 不再使用会改变
  其余 background 顺序的 `swap_remove`；三 Target 回归证明 rollback 与 commit 都保持原相对顺序；
- `BrowserTargetHandle` 增加只对 Core owner 可见的 activation/retirement reservation。reservation 期间 public
  observer 仍分别看到 staged/live，commit 以 AcqRel CAS 发布 live/retired，rollback 恢复原状态；同一 public
  target id 的其他 handle 仍不能取得该 reservation。missing reverse owner、reverse-owner context mismatch、
  non-staged/non-live handle 分别返回 `BrowserContextRegistryError` / `BrowserTargetRegistryError`，不再触发
  production panic；
- 模块按责任拆为 `target_registry.rs`（authority、projection validation 与 typed error）、
  `target_registry/transaction.rs`（可逆 topology/lifecycle transaction）和 `target_registry/tests.rs`（边界回归）；
  sibling `target_transaction.rs` 只编排 engine participant 与 registry transaction，没有把 session、event 或
  physical Protocol payload 放回 Core registry；
- 回归覆盖 commit 前 handle 可观察状态、non-staged/non-live rejection 无部分 mutation、missing/mismatched
  reverse owner、context removal exact rollback、active registration/replacement rollback、三 background Target
  activation 顺序，以及 engine participant 前 rejection 不污染 successor/engine。聚焦回归 `25/25`（run
  `1ce626b1-1ebb-400e-bae6-287a1b5d02b9`）、Core 全包 `2558/2558`、13 个既有 skip（run
  `67f6030d-e730-445f-9777-1f7a586df8c1`）和 Protocol 全包 `3246/3246`（run
  `70a359e8-2934-45a8-9db5-b4c88762243f`）通过；
- 最终 workspace 全量 nextest `15665/15665`、17 个既有 skip（run
  `574a57ca-007e-4f62-ace9-dd594769c5cb`），workspace all-target clippy `-D warnings`、fmt/diff check 与 release
  workspace build 均通过。固定 `target/release/moli` SHA-256
  `c9167a1138795c85db73e44172ad770f7f12bd0f0779e027e4299f642fe72232`，显式清除大小写全部
  proxy/no-proxy 变量、设置 `NO_PROXY=*` / `no_proxy=*` 后，CDP 默认全组 `210/210`、WebDriver
  Classic/BiDi/Selenium `--continue-on-failure` `148/148`，两者 `ok=true` 且失败列表为空；
- 本切片只关闭审计第 6 组。`page_registry.rs` 的 context/Target registration 原子性仍属于第 7 组，不能因
  Target topology 已可回滚而宣称完成；bootstrap initial-Document、physical Page residence projection 与
  same-document history rejection 也仍分别留在第 8、10、11 组。

2026-08-03 第二十七切片实现记录（Core Page registry registration transaction）：

- `BrowserPageResidenceRegistry` 的 entry 现在显式区分 `Staged` / `Live`。context/Target registration 先以
  exact owner + `BrowserPageResidenceHandle` 创建私有 transaction；staged entry 占用 identity、阻止重复注册，
  但 `capture_page_residence`、generation transition、termination、handle lookup 和 physical projection validation
  都不会把它当作 live authority。commit 发布 exact staged record，rollback 只删除 transaction 自己的实例；
- BrowserContext registration、background/active Target registration 与 bootstrap active replacement 都先暂存
  Target transaction，再暂存 Page transaction。Page staging 或 engine handoff 拒绝时按相反顺序恢复 Page 与
  Target；成功路径先安装 initial-Document/runtime sidecar、发布 Page record，最后以 Target handle AcqRel commit
  作为 capability lifecycle 发布点。这里没有 await、sleep、retry、fallback owner，也没有在 caller 重做
  registry insert；
- context bootstrap 还会把 projected Page capability 与全部已注册/暂存 entry 比较；同一 physical Page capability
  不能被两个 Context/Target 同时注册。错误继续通过 `BrowserPageResidenceRegistryError` 嵌入
  `BrowserContextRegistryError` / `BrowserTargetRegistryError`，并新增 staged-not-live 的精确诊断；
- 模块按责任拆为 `page_registry.rs`（record authority、live lookup、projection 与 generation transition）、
  `page_registry/transaction.rs`（registration staging/commit/rollback）和 `page_registry/tests.rs`（边界回归）；
  context/Target orchestrator 只组合 registry participant，没有把 Protocol payload 或 frontend session 引入 Core；
- 回归覆盖 staged entry 对 live lookup 不可见、exact rollback 后可重新注册、multi-slot context rollback、跨 Context
  Page capability alias、outstanding Page transaction 与新 Target/active replacement 冲突，以及 joint Context +
  Target rollback 不泄漏 handle/topology/engine authority。聚焦回归 `26/26`（run
  `355b0be6-e5ef-4ba8-ac66-9574d781c104`）、Core 全包 `2565/2565`、13 个既有 skip（run
  `e6fc2756-0724-49ff-81bb-5f7b90543161`）和 Protocol 全包 `3246/3246`（run
  `ec7d6a6a-f8bf-49ba-a87d-e08f0c332ed2`）通过；
- 最终 workspace 全量 nextest `15672/15672`、17 个既有 skip（run
  `9d87b807-14a6-4a17-9e19-e1e2f6375a52`），workspace all-target clippy `-D warnings`、fmt/diff check 与 release
  workspace build 均通过。固定 `target/release/moli` SHA-256
  `26931867ab3b309a9a9afb255304e69574c241ac59b96f3823e6c306e37356f6`，显式清除大小写全部
  proxy/no-proxy 变量、设置 `NO_PROXY=*` / `no_proxy=*` 后，CDP 默认全组 `210/210`、WebDriver
  Classic/BiDi/Selenium `--continue-on-failure` `148/148`，两者 `ok=true` 且失败列表为空；
- 本切片只关闭审计第 7 组。在第27切片结束时，Protocol bootstrap initial-Document 时序、physical Page
  residence projection 和 same-document history rejection 仍分别属于第 8、10、11 组；其中第 8 组随后由
  第28切片关闭，Phase 3 Exit gate 仍未通过。

2026-08-03 第二十八切片实现记录（bootstrap initial-Document registration transaction）：

- 新增独立的 `context_creation.rs`，用 protocol-neutral `BrowserContextRegistrationMetadata` 承载 exact active
  Target 的 `BrowserTargetCreationMetadata`。它与 physical `BrowserTargetTopologyProjection` 分离：前者是 Browser
  creation input，后者仍只是迁移期 topology/capability guard，没有把 Page payload、session 或 CDP event 塞进
  Core metadata；
- `BrowserNavigationOwner::register_browser_context_with_metadata` 在任何 registry mutation 前验证 metadata 必须有
  exact staged active Target；缺失 active Target 返回
  `ActiveTargetCreationMetadataWithoutActiveTarget`，不会创建 replacement engine、Context、Target、Page residence
  或 initial-Document sidecar。全部 fallible Target/Page/engine participant 接受后，Core 在发布 live Page/Target
  capability 前安装 creation metadata；Target lifecycle 仍是最后发布点；
- default BrowserContext bootstrap 现在只提交一次带 metadata 的 Core registration。原来
  `try_insert_browser_context(...) -> register_target_initial_empty_document(...).expect(...)` 的两步 production 路径
  已删除；`browser_initial_document.rs` 只保留 snapshot/lifecycle projection，以及显式 `#[cfg(test)]` 的 typed
  fixture helper，不再含 production `expect`。普通无 initial metadata 的 BrowserContext registration 继续通过空
  envelope 走同一实现；
- `target_creation.rs` 统一拥有 creation metadata 的安装操作，ordinary background/active registration、bootstrap
  active replacement 与 BrowserContext bootstrap 共用这一逻辑；context registry、target transaction 和 Protocol
  projection 只做各自 participant 编排，没有复制 initial-Document registry mutation；
- 回归覆盖 metadata 与 default Context/Target topology 同时提交，以及 metadata 没有 active Target 时在任何 mutation
  前 typed rejection。最终 Core context-registry 聚焦 `9/9`（run
  `f001de2b-d231-4504-9a7b-2b0c9da275ff`）、Protocol default-bootstrap 聚焦 `1/1`（run
  `2bc7f5f3-122c-48ac-bb03-203d0b744ac7`）通过；workspace 全量 nextest `15675/15675`、17 个既有 skip（run
  `1de7442a-9608-44c3-a8b5-1b8479af711c`），workspace all-target clippy `-D warnings`、fmt/diff check 与 release
  workspace build 均通过。固定 `target/release/moli` SHA-256
  `a4e5237eb031de9ae15cb496ff223492696a988ba6c59a9f0c3332ad7adf2a7d`，显式删除大小写全部 proxy/no-proxy
  变量并设置 `NO_PROXY=*` / `no_proxy=*` 后，CDP 默认全组 `210/210`、WebDriver
  Classic/BiDi/Selenium `--continue-on-failure` `148/148`，两者 `ok=true` 且失败列表为空；
- 本切片只关闭审计第 8 组。第 10 组 physical Page residence projection divergence 随后由第29切片关闭；剩余
  第 11 组 same-document history rejection 仍未关闭，独立 `BrowserHostActor/BrowserHostHandle` 和 Phase 3
  Exit gate 也仍未完成。

2026-08-03 第二十九切片实现记录（physical Page residence projection transaction）：

- `page_residence_projection` 按责任拆为 orchestration、`transaction.rs`、typed error 和 tests。事务先验证完整
  Core/physical Target topology，再用 Core permit 冻结 exact Page instance/generation；随后从 selected/inactive
  Protocol slot 暂存整个 BrowserContext。background 路径还会取出 exact Target 并保存原 vector index，active
  路径直接持有 staged context 内的 active participant；
- initial-Document materialization 在 Core mutation 前额外验证物理 slot 没有 loaded Page。wrong residence、missing
  physical Target/Context 和 occupied initial slot 分别返回 `PageResidenceProjectionError` / wrapped
  `TargetProjectionError`；candidate Page 只会在 physical context 已恢复后异步关闭；
- Core commit typed rejection 会在同 turn 恢复 exact BrowserContext slot 和 background index。Core 成功后不再按
  context/target id 重新 lookup，也不再存在 post-commit `expect`；transition kind、owner 和共享 residence 的检查只
  保留为 `debug_assert!` 诊断。Page replacement 的同步字段投影结束、physical context 恢复后，才允许 await 旧
  Page disposal；
- failed-navigation production caller 不再丢弃 physical projection error，而是记录包含 session/request URL 的
  warning；initial build caller把 typed error 映射到既有 build failure surface。normal stale permit 仍是
  `Stale`/`None`，不会被错误升级为 process failure；
- 新增三条边界回归：wrong physical residence 在 Core generation 变化前 typed reject；occupied initial slot 在
  materialization flag/generation 变化前 reject 并恢复 context；background discard 保持 exact Target vector index、
  只推进一次 generation 且 topology 继续一致。聚焦 transaction 回归 `3/3`（nextest run
  `7db789c0-56af-4803-a891-13e35b5ca36c`），initial-build/failed-discard 既有回归 `7/7`（run
  `6d130d8f-bad2-4abe-8843-e51e32fdeb1b`）通过；
- 最终 workspace 全量 nextest `15678/15678`、17 个既有 skip（run
  `7e7b0be6-ac59-4023-ad70-375daf176ec7`），workspace all-target clippy `-D warnings`、fmt/diff check 与 release
  workspace build 均通过。固定 `target/release/moli` SHA-256
  `8141ed7353376e5ced83fe1c14ab301063e2452b5f151f207184de2fc7c5f430`，显式删除大小写全部
  proxy/no-proxy 变量并设置 `NO_PROXY=*` / `no_proxy=*` 后，CDP 默认全组 `210/210`、WebDriver
  Classic/BiDi/Selenium `--continue-on-failure` `148/148`，两者 `ok=true` 且失败列表为空；
- 切片完成后执行 `git pull -r origin master`，35 个分支提交重放到 `29a5e3228cdf2403201244efe944d1d66e5f3cbd`。
  rebase 同时带入 renderer form POST、dedicated worker replacement、Critical-CH 与 XHR network 回归。集成层保留
  master 的 request method/body/headers 与 worker retirement，并保留本分支的 Browser Owner queue、exact Page
  residence 和 lifecycle observer；form POST 回归显式推进下一次独立 Browser Owner turn，不再假设
  `Runtime.evaluate` 的 frontend turn 同步执行 renderer navigation；
- 首次 clean workspace nextest 为 `15750/15753`、17 skipped（run
  `94a84221-a243-43d5-b14d-74960ec76a9d`）。三个失败均由 rebase 带入的测试 fixture 直接写
  `CdpConnection.browser_context`、绕过 Core topology registration 导致；精确复跑 `0/3`（run
  `8841c55f-9fb0-4d60-88fc-3b9e82d6ff10`）证明它们是确定性边界错误，不是 flaky。fixture 改用
  `insert_browser_context` 后连续 5 轮 `15/15`（run `3a41f154-0f95-4758-a0cc-4593847eaea8`）；dedicated
  worker fixture 同样从 Core 读取 authoritative residence。最终 workspace nextest `15753/15753`、17 skipped
  （run `21aa1af6-c3a8-4b3f-b9de-e04233334e03`）通过；
- post-rebase workspace fmt/diff check、all-target clippy `-D warnings` 与 release workspace build 均通过。最终
  `target/release/moli` SHA-256 为
  `2ff43b6cd590cb4652e214b04a367e5c266243dcee6733f9223647c190a73590`；显式删除大小写全部
  proxy 变量并设置 `NO_PROXY=*` / `no_proxy=*` 后，CDP 默认全组 `210/210`、WebDriver
  Classic/BiDi/Selenium `--continue-on-failure` `148/148`，两者 `ok=true` 且失败列表为空；
- 本切片只关闭审计第 10 组。第 11 组 same-document history rejection 是独立的 Core outcome 传播问题，留给下一
  内聚切片；physical `Page` payload 仍暂存在 `CdpConnection`，因此这也不表示独立
  `BrowserHostActor/BrowserHostHandle` 或 Phase 3 Exit gate 已完成。

2026-08-03 第三十切片实现记录（same-document history exact commit）：

- `BrowserNavigationHistory::record_same_document_update` 不再用无原因的 bool 表示 traversal 失败，而是返回
  `BrowserSameDocumentHistoryUpdateError`。无 current entry、index overflow、越界 target 和 renderer/browser URL
  drift 都在 cursor mutation 前形成 typed rejection；push/replace 继续由 Core 分配或保留 entry/document id；
- `BrowserNavigationOwner::commit_same_document_navigation_history` 接收 exact `PageResidenceIdentity`，先验证该
  generation/instance 仍是当前 Page，再提交 joint history。lazy seed 也成为事务 participant：若后续 traversal
  被拒绝，会恢复 seed 前完整 registry value，包括既有 pending replace/traverse；新建的空 registry entry 也不会
  因失败泄漏；
- 模块边界保持分离：`history.rs` 只拥有值算法与 algorithm error，`history_registry.rs` 只拥有 exact Page gate、
  seed/lifetime transaction 和 Core commit error；Protocol `browser_navigation_history.rs` 只把 session route 解析为
  exact Page，并在 Core commit 前暂存可直接修改的 active/background physical Target participant。Core 成功后才
  原地更新 URL/security origin；Core 或 topology 拒绝后没有 post-commit lookup、部分 physical mutation 或
  `Page.navigatedWithinDocument` event；
- Page producer 继续先丢弃 stale Page 和 pending cross-document navigation；通过这两个 gate 后若 history commit
  仍被拒绝，会记录含 exact context/target/generation 的诊断并停止 projection。旧的 mutation-first Target helper
  已删除，没有加入 sleep、retry、fallback owner 或 production panic；
- Core/Protocol 聚焦回归分别通过 `10/10`（nextest run
  `00fa2d11-2896-44e7-b738-4a1868ab72be`）和 `13/13`（run
  `f5b5b146-1f0e-44e7-b738-4a1868ab72be`）；三个受影响 package 全量 `6404/6404`、13 个既有 skip（run
  `29dcbb82-7612-4c37-b598-a003eb886c41`），workspace 全量 nextest `15759/15759`、17 个既有 skip（run
  `af448d4f-9f89-4ab7-8d37-5fcb38661547`）通过；
- workspace all-target clippy `-D warnings`、fmt/diff check 与 release workspace build 均通过。固定
  `target/release/moli` SHA-256
  `689119a12c634c71a9d3d229102d29ffeb13e04a1cc2ca7d5f328a421cc53f24`，显式删除大小写全部
  proxy/no-proxy 变量并设置 `NO_PROXY=*` / `no_proxy=*` 后，CDP 默认全组 `210/210`、WebDriver
  Classic/BiDi/Selenium `--continue-on-failure` `148/148`，两者 `ok=true` 且失败列表为空；
- 切片提交后执行 `git pull -r origin master`，37 个分支提交重放到 `b6759c82ff`。master 增量只有一笔
  Cloudflare 调研文档整理；旧/new 切片 tree 对 `moli-core`、`moli-protocol` 和本规划文件的逐路径
  diff 为空，没有改变源码、构建配置或上述 release binary 输入。因此没有机械重复全量测试和 smoke，post-pull
  只重新执行 workspace fmt/diff check；前述 nextest、clippy、release 与 smoke 仍对应 exact same build inputs；
- 本切片关闭审计第 11 组，因此 actor 前列出的 12 组既有 panic/divergence/ordering 问题均已收口。physical
  `Page` payload 仍暂存在 `CdpConnection`，独立 `BrowserHostActor/BrowserHostHandle`、自主 host lifetime 与
  typed outcome/fact channel 尚未实现，所以 Phase 3 Exit gate 仍未通过；下一切片回到 actor extraction，不能继续
  把 Protocol adapter 当作最终 Browser Owner residence。

2026-08-03 第三十一切片实现记录（Browser Host input actor/handle seam）：

- Core 新增职责分离的 `browser_host/actor.rs` 与 `browser_host/handle.rs`：`BrowserHostActor` 是唯一 mailbox
  receiver 和 FIFO selection authority，`BrowserHostHandle` 是 cloneable producer endpoint。publish 只完成同步
  admission，不递归执行 navigation；actor 停止后返回携带 exact input kind 的 typed rejection；
- 删除临时 `BrowserOwnerQueue` 类型和 `CdpSchedulerEvent::BrowserOwnerInputPublished`。renderer prepared output
  直接经 `CdpConnection` 保存的 handle 进入 Core mailbox；未安装或已停止的 Host 分别返回 typed publication
  error 并记录 rejection trace，不允许 Protocol 重建 fallback queue/event；成功 admission 后才记录既有
  `browser_action_published` trace。`browser_owner_input_published` runtime trace 同样移到 successful admission
  边界，但 producer 不再读取 actor queue length；
- application `CdpScheduler`、Protocol 测试 actor 都在 composition 时创建 actor/handle pair。CDP、BiDi 与 Classic
  的既有 selection rule 现在直接查询并 pop Core actor mailbox；producer turn 仍只发布，下一独立 owner turn 才
  调用迁移期 physical adapter，因此没有把 renderer output callback 变成递归 navigation executor；
- actor 边界回归覆盖 FIFO/exact Page identity、一个 producer handle clone detach 不终止 Host、Host shutdown 的
  typed rejection、无 Host 时不回退 Protocol scheduler，以及 prepared/barrier navigation 只在下一 owner turn
  执行。handle clone 测试只冻结 endpoint ownership，不冒充“frontend stack 已可整体 drop”的 lifetime 证明；
- mailbox 暂时保持与旧 `VecDeque` 相同的无界 admission 语义，且 production producer 仍只有 owner thread 内的
  renderer-output ingestion。扩展 command/external producer 前必须在 Phase 4 冻结 capacity、coalescing、wake 和
  overload outcome；不能把当前同 turn producer wake 条件当作最终自主 actor loop；
- actor/owner 聚焦回归 `106/106`（nextest run
  `4e02d8ee-863a-4bcd-8523-bdc15db4b88b`）通过。首次 workspace 全量为 `15760/15762`（run
  `675d93e4-5d37-4133-aac8-98ff6573a5ed`），两个失败都位于未修改的 renderer-v8 Blob/OPFS 与 Worker
  MessagePort 用例；两者随后共同精确复跑 `2/2`（run `044f7cca-772f-482e-8639-bbd7411fce72`）、20 轮
  stress `20/20`（每轮两条，run `fe45e11e-9600-4adb-b03a-397c729209c4`）和 renderer-v8 全包
  `6970/6970`（run `be926a5b-3f5d-4d7e-b9cf-25b3cb4c055a`）均通过；没有为此加 retry、sleep、timeout 或
  修改 renderer 测试/产品代码。最终 workspace 原命令重跑 `15762/15762`、17 个既有 skip（run
  `44404f08-b815-44d8-93cc-3a2b023d7f34`）通过；
- workspace all-target clippy `-D warnings`、fmt/diff check 和 release workspace build 通过。固定
  `target/release/moli` SHA-256 为
  `2e4f0ec8d1446facc615cc589d5094546d9f4ac79eb8d45dd298fc665d215e9d`；显式删除大小写全部 proxy/no-proxy
  变量并设置 `NO_PROXY=*` / `no_proxy=*` 后，CDP 默认全组 `210/210`、WebDriver
  Classic/BiDi/Selenium `--continue-on-failure` `148/148`，两者 `ok=true` 且失败列表为空；
- 本切片只建立 input ownership/type seam。`BrowserHostActor` 仍作为字段位于 `CdpScheduler` application object，
  physical BrowserContext/Target/Page 与 input execution adapter 仍在 `CdpConnection`；BiDi/Classic socket flush、
  frontend disconnect、独立 Host lifetime 和 typed outcome/fact journal 均未结构性解决，Phase 3 Exit gate 仍未
  通过。

2026-08-03 第三十二切片实现记录（Browser Host turn capability seam）：

- Core 新增独立 `browser_host/turn.rs`。`BrowserHostTurn` 是 non-cloneable、`#[must_use]` 的 exact turn
  capability；其构造权限限制在 Core Browser Host 模块，production 只由 actor 在 FIFO selection 后签发。
  executor 能看到 input kind 和 selection 后 ready-count snapshot，并只能消费已签发 turn，不能从 raw renderer
  intent 伪造 selection；
- `BrowserHostActor::complete_next_turn` 同时拥有 mailbox selection 与 executor invocation；唯一 mutable actor borrow
  跨 executor future 保持，因而 application 不能在第一轮尚未完成时选出第二个 input。空 mailbox 不调用 executor，
  原公开 `pop_next_input` 已删除；
- Protocol 按责任拆成 `browser_owner_input.rs` publication adapter 与 `browser_host_turn_executor.rs` physical Page
  executor。`CdpConnection` 实现不含 frontend/session/socket contract 的 Core executor trait，但不再暴露接收 raw
  `BrowserOwnerInput` 的 inherent completion API；既有 `browser_owner_input_start` trace 保留在 exact executor
  entry，记录 Core turn 的 kind 与 remaining snapshot；
- application `CdpScheduler`、Protocol `TestContext`、Page producer 与 runtime barrier fixture 都只请求 actor 完成
  下一 turn，不再取得、缓存或转交 raw owner input。fixture 的 marker 只表达“这里轮到 Browser Host”，实际 exact
  input 仍在执行时由 actor 从 mailbox 选择；只有 Core/Protocol capability 边界单元测试使用 recording executor
  消费 turn 后检查 payload identity，不参与 production scheduling；
- actor/owner 聚焦回归 `107/107`（nextest run
  `821c0dd4-5ee9-4056-acaf-8bd2ce12020e`）通过；最终 workspace 全量 nextest `15763/15763`、17 个既有
  skip（run `a60b49d2-4f57-46df-bbfe-b3aad0822d73`），workspace all-target clippy `-D warnings`、fmt/diff
  check 与 release workspace build 均通过。固定 `target/release/moli` SHA-256
  `091fab9ecf8dd56426033c51133d30668a6484d47a6a66c4c1ec942dcb9707f3`，显式清除大小写全部
  proxy/no-proxy 变量并设置 `NO_PROXY=*` / `no_proxy=*` 后，CDP 默认全组 `210/210`、WebDriver
  Classic/BiDi/Selenium `--continue-on-failure` `148/148`；两套 JSON 汇总均为 `ok=true` 且失败列表为空；
- 切片提交后执行 `git pull -r origin master`，远端 `master` 与当前 merge-base 都是
  `b6759c82ff847aa5153e53d4189062ecaa645856`，Git 报告当前分支已是最新，没有 rebase、冲突或源码/build-input
  变化；因此前述 nextest、clippy、release 与 smoke 仍对应最终代码输入，没有机械重复执行；
- 这一步只关闭 turn authority 的类型边界。production loop 仍通过 `has_ready_input` polling 驱动 actor，没有把
  mailbox receiver/wake 纳入独立 application `select!`；actor 仍是 `CdpScheduler` 字段，physical executor 仍可能
  await 既有 Page command，typed outcome/fact projection、frontend disconnect/slow writer 隔离、独立 Host lifetime
  和 bounded admission 均未完成。因此不能把本切片称为自主 Browser Owner lane，也不能宣告 Phase 3 Exit gate
  通过。

2026-08-03 第三十三切片实现记录（application-owned Host receive/wake seam）：

- `BrowserHostActor` 新增 cancellation-safe `select_next_turn_when_ready`。pending Tokio mpsc receive 被 sibling
  select branch 取消时不消费 input；receive 成功后在同一次 poll 内把 exact `BrowserHostTurn` 存回 actor，只向
  application 返回 typed `Selected/Closed`，不暴露 raw input 或 turn payload。selected slot 使重复 selection
  只能观察同一轮，不能在前一轮执行前 dequeue 第二轮；
- application 新增独立 `cdp_scheduler/owner_inputs.rs`，集中放置 Host actor、background completion/event 与
  renderer publication residences。`CdpScheduler` 不再含 `BrowserHostActor` 字段，只保留 physical/protocol
  projection state；constructor 同时创建 actor/producer pair，再把 receiver residence交给 application inputs、
  把 cloneable producer handle 安装到 `CdpConnection`；
- CDP owner、standalone BiDi、Classic-with-BiDi 和 bare Classic 四条 production blocking loop 都把 Host mailbox
  receive 放进 biased `tokio::select!`。Host ready 后先退出 selection future，再调用 actor/executor 并完成输出
  routing，避免 sibling frontend readiness 取消已 dequeue 的 navigation。ready Host 也先于 renderer transport
  terminal 结算；不再靠 loop 顶部 `has_ready_input` polling 才获得 wake；
- direct CDP/WebDriver command waits 的统一 `recv_interleaved_input` 新增 payload-free `BrowserHostTurn` marker，
  因而 lifecycle/navigation wait 阻塞在内层时也能推进 Host turn，不必等控制返回最外层 frontend loop。specialized
  exact renderer cursor fence 仍只消费其因果所需的 concrete renderer stream，没有让无关 Host input 越过 fence；
- Core 回归覆盖 mailbox 从空闲状态被 publication 唤醒、selection 与 execution 分离、pending selection cancellation
  不吞下一 input，以及 sender shutdown 的 typed terminal；application 回归覆盖没有 protocol/background/renderer
  输入时 Host mailbox 独立唤醒 direct wait，并保持 protocol client-turn predecessor 不变；
- actor/owner 聚焦回归 `98/98`（nextest run `8e2e86df-1e41-4a88-b65e-686dffdc595d`）、受影响包级
  `3132/3132`（run `3ed66bfa-417b-4f09-b999-b5ace05ab99a`）和最终 workspace 全量
  `15766/15766`、17 个既有 skip（run `75447eef-81ab-49b3-ab00-ed47eb0147c7`）通过。4 条新增
  mailbox selection/cancellation 回归以 `--stress-count 20 --flaky-result fail` 重复 20 轮，共 80 次断言无失败
  （run `4aaae340-b8bb-40e1-ae5d-2518e3d2fab9`）；workspace all-target clippy `-D warnings`、fmt/diff
  check 和 release workspace build 均通过；
- 固定 `target/release/moli` SHA-256 为
  `2422de207654cd68ff46a5924ec370385c36fe4f401be50d3cbc16f96cce418e`。显式清除大小写全部
  proxy/no-proxy 变量并设置 `NO_PROXY=*` / `no_proxy=*` 后，CDP 默认全组 `210/210`、WebDriver
  Classic/BiDi/Selenium `--continue-on-failure` `148/148`；两套 JSON 汇总均为 `ok=true` 且失败列表为空；
- 切片提交后执行 `git pull -r origin master`，远端 `master` 与当前 merge-base 都是
  `b6759c82ff847aa5153e53d4189062ecaa645856`，Git 报告当前分支已是最新，没有 rebase、冲突或源码/build-input
  变化；因此前述 nextest、clippy、release 与 smoke 仍对应最终代码输入，没有机械重复执行；
- 这一步完成的是 receive/wake residence，而不是独立执行 task。actor 仍与各 frontend application owner loop 一起
  创建和销毁，selected turn 仍借用 `CdpConnection` 的长 physical Page executor，output flush 也仍在同一 loop；
  slow writer/disconnect 隔离、短 Browser commit、typed outcome/fact channel 和 bounded admission 尚未完成，不能
  宣告 Phase 3 Exit gate 通过。

2026-08-03 第三十四切片实现记录（short Host turn 与 participant completion residence）：

- Core `BrowserHostTurnExecutor` 不再返回一个由 actor `await` 的 future，而是同步消费 exact
  `BrowserHostTurn` 并立即返回 dispatch。`BrowserHostActor::complete_next_turn` 因而只在一个无 suspension 的
  run-to-completion turn 内持有 actor 与 executor mutable authority；网络、renderer 或 protocol participant wait
  不能再藏在这个 trait boundary 后面；
- Protocol 把 renderer top-level navigation 拆成同步 start 与后续 completion：同步 start 完成 stale Page
  validation、navigation admission 和可立即完成的 background registration；需要等待时返回不可 clone、
  `#[must_use]` 的 `PendingBrowserHostTurn`。该 capability 保留原 `CommandOwnerScope`、exact Page/renderer command
  token 和 command context；`wait(self)` 消耗它并产生 move-owned `CompletedBrowserHostTurn`，late completion
  继续由原有 exact generation/token 校验拒绝，不能扫描 current Target 来修改 replacement Page；
- application `cdp_scheduler/owner_inputs.rs` 新增独立 `BrowserHostExecutionLane`，集中拥有 Core actor 和
  participant completion channel。start dispatch 若含 pending，lane 在 local executor 上登记 wait 并立即把 start
  outcome 交给 scheduler；完成值以后作为 `BrowserHostExecutionWake::ParticipantCompleted` 回到同一输入选择边界。
  completion wake 优先于新的 Host mailbox selection，且两者由 lane 内一次 `select!` 完成，避免对同一 lane 的
  双重 mutable borrow 或应用层取得 raw input；
- CDP actor、standalone BiDi、Classic-with-BiDi、bare Classic 与 direct lifecycle/navigation wait 全部消费同一
  lane wake contract。ready mailbox input 只同步 start；pending wait 期间 actor 已释放，后续 owner input 可继续
  dequeue/start。Protocol output 仍由各 frontend 现有 projector 发送，没有新增 sleep、retry、`yield_now` 或把
  403/DCL 当成伪 lifecycle 的规则；
- 回归在 direct physical adapter 上先启动一个真实 pending top-level load，在尚未等待其 completion 时发布并选择
  第二个 stale Page input，证明第一个 participant wait 不再持有 Browser Host actor；随后完成原 exact load，并
  继续验证 `Page.frameStartedNavigating` 的 target/loader/url 和 automation event。Core 既有 FIFO、selection
  cancellation 与 shutdown regressions 继续覆盖 turn capability；
- Core actor 聚焦回归 `7/7`（nextest run `3bb5c1e5-6e5d-4c75-8503-aad07bb39351`）、跨
  Protocol/application/WebSocket 聚焦回归 `6/6`（run `1265dec3-330d-4e95-811e-4c45418df1aa`）和受影响三包
  `6411/6411`、13 个既有 skip（run `341d9299-f169-4b92-bc72-f160429161ba`）通过。新增真实 pending/第二输入
  回归以 `--stress-count 20 --flaky-result fail` 重复 20 轮全部通过（run
  `6c06286d-cb99-4dcc-873b-f09a7fc04394`）；最终 boxing/layout 修正后的 workspace 全量为
  `15766/15766`、17 个既有 skip（run `3df25789-b242-42e0-ac88-6a86562a2805`）；workspace all-target clippy
  `-D warnings`、fmt/diff check 和 release workspace build 均通过；
- 固定 `target/release/moli` SHA-256 为
  `6ae535aec7f0b57ee3d7344bbccf2c5851ead4ea06dad6257e618169ac688c7e`。显式清除大小写全部
  proxy/no-proxy 变量并设置 `NO_PROXY=*` / `no_proxy=*` 后，CDP 默认全组 `210/210`、WebDriver
  Classic/BiDi/Selenium `--continue-on-failure` `148/148`；两套 JSON 汇总均为 `ok=true` 且失败列表为空；
- 本切片只建立 **typed participant completion residence**，不是最终 Browser Fact channel。同步 start 已经是短
  turn，但 `CdpConnection::complete_browser_host_turn` 对 Fetch continue、materialized Page commit 等迁移期路径
  仍可能异步执行；`BrowserHostTurnDispatch` 的即时 projection 也仍携带 `CdpTurnOutcome`。下一切片应把 completion
  application 继续拆成短 Core commit / 再登记 participant 的状态机，并把 navigation result/fact 从 CDP event
  shape 中分离；随后才能把 Host task/lifetime 与 frontend writer/teardown 分开。Phase 3 Exit gate 仍未通过。

2026-08-03 第三十五切片实现记录（response-ready navigation completion 状态机）：

- 新增独立 `domains/page/navigation_completion.rs`，不再把 continuation phase 堆进 4k 行的 navigation start
  handler。一个 direct navigation 现在显式经历 `Load` → `ConfigurePreparedDocument` →
  `CommitPreparedDocument`；每个 pending phase move-own exact `BrowserDocumentNavigation`、冻结的
  `NavigationDispatchState`、prepared Page/commit permit、renderer attachment transaction 与 engine，不跨 wait
  借用 `CdpConnection`，也不从 current Target 重新推断 owner；
- network completion 的 apply turn 只做 current-token check、materialization 和 commit configuration snapshot，
  随即登记 configuration participant；configuration completion 只做 exact renderer candidate transaction，再登记
  renderer Document commit participant。commit failure 仍以原 transaction rollback，stale completion 仍由 exact
  token/attachment/Page generation 拒绝，没有新增 sleep、retry、timeout 放宽或 DCL 特判；
- direct `PendingBrowserHostTurn` 与 production background navigation 共用同一状态机。background 路径新增 opaque
  `BackgroundNavigationParticipantCompletion`，application 只能经既有 completion channel 路由，不能取得 Page、
  renderer 或 navigation mutable capability；原 command/session route、`CommandDispatchContext`、requested URL 和
  `BackgroundNavigationGateKey` 跨 phase 原样保留；
- background drain 返回 typed `BackgroundNavigationTurnDisposition`。`ParticipantPending` 不结算 navigation gate；
  只有最终 success/error 的 `Terminal` 才让 scheduler 调用 `note_navigation_completion_drained`。因此 configuration
  与 renderer commit 两个独立 wake 不会被误当成导航结束，也不会提前开放依赖该 exact loader/request 的后续
  protocol residence；main-document body completion 仍是独立非 gate input；
- 独立 transport 暴露了一个必须显式维持的顺序：prepared renderer 可以先发布
  `MainDocumentCommit`，但 Protocol 不能在 Browser commit 前拿旧 frame/loader 状态投影这条事实，否则会把
  `frameNavigated`/execution-context init 当成 stale 丢掉。application 新增
  `NavigationRendererPublicationBuffer`，只从首次包含 `MainDocumentCommit` 的 exact renderer stream 开始保留 raw
  publication；stream control 与其他 stream（包括 popup）仍可通过，普通 command fence 也可跳过被保留的导航
  stream，只有该 navigation terminal cursor 才释放自己的 stream。这里保留的是 commit publication，不是延后或
  合并 DCL；403 Document 与 successor Document 的 DCL 仍各自属于 exact Document；
- `pending_top_level_location_navigation_releases_the_browser_host_actor` 现在进一步断言 load completion 会登记
  configuration wait、configuration completion 会再登记 renderer commit wait；新增 production-shaped
  `background_navigation_commit_participants_resume_as_separate_inputs` 直接验证 lifecycle → participant → participant
  三次 input 和同一 gate key，并验证前两次 disposition 非 terminal、最后一次才 terminal。最初把 navigation gate
  解释成“冻结全部 renderer raw input”后，Protocol 全包第一次运行通过 `3279/3280`，但
  `window_open_named_target_reuses_existing_popup_target` 持续不结束（run
  `268c3f3c-d8b9-470f-8895-83489406f17e`，手动终止）；原因是旧 stream command fence 不断取到并重新排队新
  popup 的 `Opened` control。改为上述 per-stream invariant 后，该用例单独在 `0.123s` 通过（run
  `9988120d-16c1-409e-aa54-ddc4770737cd`），没有增加 sleep/retry 或放宽 timeout；
- 最终 8 条聚焦边界/回归用例 `8/8`（nextest run
  `ef5c9b62-f038-44d3-a1ce-d3de2a504902`）；三条 protocol ordering/race 回归以
  `--stress-count 20 --flaky-result fail` 共执行 60 次无失败（run
  `9ed0d514-ee79-400d-b933-1e5f6a6a168b`）；`moli-protocol` 全包 `3280/3280`（run
  `a15d0d69-ddab-4014-a1cb-d70715e3f8a4`）；最终 workspace 全量为 `15770/15770`、17 个既有 skip（run
  `c4dcc0c0-9995-4125-a0f3-db0a1be30515`），workspace all-target clippy `-D warnings` 与 fmt/diff check
  通过；
- release workspace build 通过，固定 `target/release/moli` SHA-256 为
  `02422d5d4e0cbf67251c1c9d7e2ced7e431dda061725167d1ecd77bd110e557f`。显式清除大小写
  HTTP/HTTPS/ALL/FTP proxy 与既有 no-proxy 变量，再设置 `NO_PROXY=*` / `no_proxy=*` 后，CDP 默认全组
  `210/210`、WebDriver Classic/BiDi/Selenium/Semantics `--continue-on-failure` `157/157`；两套 JSON 均为
  `ok=true` 且失败列表为空；
- 本切片仍不是完整 short completion apply。Fetch request/auth/response continue 仍可能在持有 `CdpConnection` 时
  进行 network/prepare 工作；renderer commit 完成后的 loaded Page runtime restore、replacement/worker teardown、
  lifecycle activity 和 Inspector replay 也仍是 async physical apply。`CdpTurnOutcome` 仍是即时输出类型，Host 与
  background participant channel 也仍依附 frontend owner lifetime。下一切片应优先把这些 remaining suspension
  points 逐个变成 exact participant 或短 commit，再建立 protocol-neutral outcome/fact channel；Phase 3 Exit gate
  仍未通过。

2026-08-03 第三十六切片实现记录（navigation-tail Inspector replay participant）：

- `conn/runtime_eval.rs` 新增 `PendingRendererCallReplayBatch` / `CompletedRendererCallReplayBatch`。一次 pending
  capability move-own 当前 `PendingRuntimeProtocolMessageDispatch`、exact renderer correlation/response lease、目标
  attachment 和尚未启动的 FIFO replay；`wait(self)` 不接收、保存或借用 `CdpConnection`。completion apply 消费
  exact Page completion、恢复 frontend command id、投影本轮 output，然后才同步尝试启动下一 replay。无效 JSON、
  stale route/attachment 和 Page miss 仍沿用原 terminal error 语义，不新增 retry、sleep 或 current-Target fallback；
- 新增独立 `domains/page/navigation_tail.rs`，集中拥有 old-Document renderer finish、released output routing、
  non-replayable call termination、replay start/apply 与 exact loader clear。`navigation_completion.rs` 增加
  `ReplayRendererCalls` phase：response-ready commit completion 若启动 replay 就再次返回 `Pending`，direct Host 与
  background path 仍走同一 opaque participant channel；每个 replay completion 若还有 successor replay 继续返回
  `ParticipantPending`，只有整个 batch 终止才清理 matching loader 并返回 `Terminal`；
- `navigation.rs` 只保留 generic materialized path 的 compatibility drain。Fetch continuation、termination 以及尚未
  进入 response-ready phase state machine 的 materialized outcome 仍会经这个 wrapper inline 等完 replay；它们是
  后续切片的明确 suspension point，不能用本切片宣称所有 navigation tail 已脱离 `CdpConnection`；
- 允许 replay wait 期间启动 successor Browser input 后，旧的“tail 结束再 adoption engine”会暴露新的跨代写入：
  当前 engine registry 只用 `{browserContext,target}` key，迟到的 A replay tail 可能覆盖已经提交的 B engine。
  因此 `NavigationEngine` 改为在 A 的 exact Document commit apply turn 内、发布 replay participant 前完成 adoption；
  replay completion 不再携带或写 engine。loader clear 继续用 exact loader compare，B 已开始时 A tail 不能清掉 B；
- runtime 边界回归 `renderer_replay_batch_waits_one_move_owned_page_dispatch_at_a_time` 用两个真实
  `Console.clearMessages` Inspector dispatch 验证 FIFO start/apply、旧 response lease retirement、原 frontend
  receiver completion，以及 participant 存活期间 connection 可独立访问。production-shaped
  `background_navigation_commit_participants_resume_as_separate_inputs` 现在人为保留一个 replayable old-attachment
  call，验证 lifecycle → configuration → renderer commit → Inspector replay 四段 wake 使用同一 gate；commit apply
  为 non-terminal，只有 replay apply 才 terminal。两项用例聚焦 `2/2`（run
  `2f8ba619-81ef-42f6-96ae-d43a652e65d8`），`--stress-count 20 --flaky-result fail` 20/20 轮通过（run
  `e90177cc-a1cd-4a16-9e58-4db8341714ab`），`moli-protocol` 全包 `3281/3281`（run
  `3cd750d5-11d3-46b4-adcb-2328f514c7ca`）；
- 最终 workspace 全量为 `15771/15771`、17 个既有 skip（run
  `4c944313-19ee-4f74-82c7-0dda4ceee28f`）；workspace all-target clippy `-D warnings`、fmt/diff check 与 release
  workspace build 均通过。固定 `target/release/moli` SHA-256 为
  `008c10c89bd7e703a8a8ce8a8efcc4a1a8bb41d83c7f1177bc3d2d6c59ad9d9d`；显式清除大小写
  HTTP/HTTPS/ALL/FTP proxy 与原 no-proxy、设置 `NO_PROXY=*` / `no_proxy=*` 后，CDP 默认全组
  `210/210`、WebDriver Classic/BiDi/Selenium/Semantics `--continue-on-failure` `157/157`，两套 JSON 均为
  `ok=true` 且失败列表为空；
- replay completion apply 内的 runtime context/node remote-object normalization 仍可能 `await`；loaded Page runtime
  restore、physical replacement/old Page close、worker teardown、Fetch continue、generic materialized drain、即时
  `CdpTurnOutcome` projection 与 Host/frontend lifetime 也尚未拆出。DCL/load 仍保持 exact-Document 事实，本切片
  只改变高层 navigation completion 的 participant ownership 与 terminal gate 条件；Phase 3 Exit gate 仍未通过。

2026-08-03 第三十七切片实现记录（generic materialized navigation-tail participant）：

- `navigation.rs` 把 materialized outcome 的 body apply 与 renderer tail 明确拆开：
  `apply_materialized_navigation_into_buffer_async` 只处理 `ResponseCommitReady` / `Loaded` / `Download` / `Failed`
  的物理 outcome 和协议事实，不再隐式等待 Inspector replay；原
  `complete_materialized_navigation_into_buffer_async` 仅作为尚未迁移调用方的 compatibility drain；
- `navigation_completion.rs` 的非 `ResponseCommitReady` 分支不再回到 `CdpConnection` legacy drain，而是在 body apply
  后直接调用同一个 `finish_or_suspend_navigation_tail`。因此 direct command 和 production background lifecycle
  completion 都会把每个 replay 返回为 move-owned `PendingNavigateCommand::ReplayRendererCalls`；background gate 在
  generic `Loaded` commit 后仍为 non-terminal，只有 exact replay batch 结束才清理 matching loader 并 terminal；
- engine adoption 与第36切片保持同一不变量：materialized body 得到 exact committed
  `BrowserPageOwnerKey` 后，在释放第一个 replay participant 前完成 target-keyed engine adoption。迟到的旧 replay
  completion 既不携带 engine，也只能按 exact loader 清理，因此不能覆盖 successor Target engine 或清除 successor
  navigation gate；
- production-shaped 回归 `background_generic_loaded_navigation_tail_resumes_as_participant_input` 先让真实 data URL
  response-ready Page 完成 renderer commit，随后用同一个 production payload 进入 legacy `Loaded` outcome，保留一个
  真实 `Console.clearMessages` old-attachment replay。用例验证 lifecycle apply 为 non-terminal、旧 response lease 已
  retirement、successor participant 保留同一 gate，且只有 replay apply terminal；没有构造假的 `Page` payload，
  也没有加入 sleep/retry；
- 新旧两条 background participant 回归聚焦 `2/2`（run
  `10156b71-e062-48a9-b4f9-4e26b0f14756`），以 `--stress-count 20 --flaky-result fail` 共执行 40 次无失败（run
  `190e8633-cdfd-4375-b691-43f59ccc1528`）；`moli-protocol` 全包 `3282/3282`（run
  `80dce44d-e616-49ae-bb66-7bb1a66fa54e`）；
- 最终 workspace 全量为 `15772/15772`、17 个既有 skip（run
  `2539124d-b9f1-4e33-9652-089111ea64bd`）；workspace all-target clippy `-D warnings`、fmt/diff check 与 release
  workspace build 均通过。固定 `target/release/moli` SHA-256 为
  `b282bda12944e441e654f37c67193b78c9943715e5289eb2a2ba158f90f673f0`；显式清除大小写
  HTTP/HTTPS/ALL/FTP proxy 与原 no-proxy、设置 `NO_PROXY=*` / `no_proxy=*` 后，CDP 默认全组
  `210/210`、WebDriver Classic/BiDi/Selenium/Semantics `--continue-on-failure` `157/157`，两套 JSON 均为
  `ok=true` 且失败列表为空；
- 本切片只关闭 Browser Host direct/background generic outcome 的 tail wait。Fetch continue 和 Target termination
  仍通过 compatibility wrapper inline drain；generic `Loaded` body 内的 runtime restore、physical Page replacement、
  old Page/worker teardown 仍可能跨 `await` 借用 `CdpConnection`，replay completion apply 的 output normalization 也
  尚未拆开。`CdpTurnOutcome`、typed Browser fact/outcome channel 与 Host/frontend lifetime 均未改变，Phase 3 Exit
  gate 仍未通过。

2026-08-03 第三十八切片实现记录（generic Loaded Page restore participant）：

- 新增独立 `domains/page/loaded_page_restore.rs` 模块，集中拥有 loaded Page 的 Runtime/V8 session restore、Fetch
  subresource interception restore、permission override restore，以及随后安装 Page 所需的 exact renderer attachment
  capability。`TargetLoadedNavigationCommitState` 只在 start apply turn 从 session owner 冻结为值快照；
  `PendingLoadedPageRestore` move-own 新 `Page`、该快照、permission 列表、candidate/committed attachment 和 trace
  identity。其 `wait(self)` 只向自己拥有的 Page 发 renderer command，不接收、保存或借用 `CdpConnection`、
  `CommandOutputBuffer` 或 `CommandDispatchContext`；
- `navigation_commit.rs` 将 loaded commit 明确建模为 `start → pending/ready → complete`。外层
  `PendingLoadedNavigationCommit` 同时保留 resource、creation lifecycle、progress gate、inner engine 和 activity
  continuation，但等待期间不执行这些 projection。generic/legacy `Loaded` 返回 pending；正常
  `ResponseCommitReady` 的 renderer configuration 已由 prepared-Document participant 提交，因此返回 ready 并在同一
  apply turn 继续，不人为增加一个 scheduler wake。缺失 commit-state、restore error 和 stale attachment 仍保持原
  success/error/drop 语义；Runtime output predecessor 只在 completion apply 时并入 command context，且即使后续
  Fetch/permission restore 失败也随 failure completion 保留，不丢失原错误响应前的 renderer ordering fence；
- `navigation_completion.rs` 新增 `RestoreLoadedPage` pending/completed phase。generic `Loaded` 的 lifecycle apply 只
  注册 opaque Page restore participant，此时旧 renderer attachment 和旧 Page 仍是 current；restore completion
  重新进入 Browser Host owner turn 后才允许 physical replacement、旧 response lease retirement、worker cleanup、
  lifecycle commit 与后续 Inspector replay。restore 和 replay 各自保持同一个 exact
  `BackgroundNavigationGateKey`，前者 completion 若发布 replay 仍为 `ParticipantPending`，只有最终 tail 才
  `Terminal`；
- stale ordering 继续由 exact navigation token/attachment capability 决定，不靠 current Target 猜测。新增
  `superseded_generic_loaded_restore_cannot_install_stale_page`：A 的 Page 被 restore participant 独占期间启动 B，A
  completion apply 必须被 replacement transaction 拒绝，不能替换旧 attachment、不能写 B 的 engine，也不能要求
  frontend 用下一命令 pump。`background_generic_loaded_navigation_tail_resumes_as_participant_input` 进一步验证
  lifecycle apply 后 attachment 仍为旧实例，restore apply 后才替换并发布真实 old-attachment
  `Console.clearMessages` replay。旧实现的红测 run `b28e99dd-a407-463c-9da8-c21deb29ca24` 精确失败为 lifecycle
  apply 已把 attachment 从 `1` 提前换成 `2`；没有通过 sleep、retry、放宽 timeout 或把 DCL 判假来改变结果；
- compatibility 边界保持显式：`navigation.rs::commit_loaded_navigation_async` 对 Fetch continue、Target termination
  和其他尚未进入 Browser Host phase state machine 的调用方仍会 inline 等待同一个 move-owned restore；这避免双
  execution authority，但还不是这些调用方的最终 queue cutover。response-ready fast path 没有新增 participant，
  既有 `background_navigation_commit_participants_resume_as_separate_inputs` 继续保持
  lifecycle → configuration → renderer commit → replay 的原 wake 数；
- 最终三条 participant/ordering 回归以 `--stress-count 20 --flaky-result fail` 共执行 60 次无失败（run
  `3e676216-6460-492c-9fd4-06152c45c73d`）；`moli-protocol` 全包 `3283/3283`（run
  `212587d0-1ca9-445c-90ff-1ed968d4ca8c`）；workspace 全量 `15773/15773`、17 个既有 skip（run
  `a9d54cbb-ef41-4355-a150-6cbede507140`）。workspace all-target clippy `-D warnings`、fmt/diff check 与 release
  workspace build 均通过；固定 `target/release/moli` SHA-256 为
  `b8fdc7eff8f059a58a9e8f5430c3264e9612d6c117e0a76fe446ef4263487e83`。显式清除大小写
  HTTP/HTTPS/ALL/FTP proxy 与原 no-proxy、设置 `NO_PROXY=*` / `no_proxy=*` 后，CDP 默认全组 `210/210`、
  WebDriver Classic/BiDi/Selenium/Semantics `--continue-on-failure` `157/157`，两套 JSON 均为 `ok=true` 且失败
  列表为空；
- 本切片只拆出 author-JS 后 generic Page configuration restore 的等待 ownership。restore completion apply 内的
  physical replacement/旧 Page close、dedicated-worker retirement、BiDi preload listener startup，以及外层 loaded
  lifecycle activity 仍可能跨 `await` 借用 `CdpConnection`；Fetch/termination compatibility drain、replay output
  normalization、protocol-neutral `CdpTurnOutcome`/fact channel 和 Host/frontend lifetime 也尚未拆开。Phase 3 Exit
 gate 仍未通过，下一切片应从 physical replacement disposal 或 Fetch/termination compatibility drain 中选择一个
 单一 authority 边界继续切，不把两类 mutation 混进同一个 future。

2026-08-03 第三十九切片实现记录（physical Page replacement disposal participant）：

- `conn/browser_page_replacement.rs` 将 loaded Page replacement 拆成同步
  `start_loaded_page_replacement_for_session_owner` 与 move-owned `PendingLoadedPageReplacement`。start turn 仍连续完成
  exact Browser owner/导航许可检查、renderer attachment preparation、Core request/Page generation/history commit 和
  physical Page-slot projection；这段保持无 `await`，因此不会出现 Core 已指向 successor、physical slot 仍指向旧
  Page 的可观察窗口。只有已经退出 physical residence 的旧 Page，或被 stale transaction 拒绝的新候选 Page，才与
  terminal `Committed/Rejected/Failed` outcome 一起移入 disposal capability；`wait(self)` 只执行其自有
  `Page::close_async()`，不接收、保存或借用 `CdpConnection`，也没有 session/Target current fallback；
- 新增独立 `domains/page/loaded_page_install.rs`，没有继续扩张第38切片的 restore 模块。`loaded_page_restore.rs` 只拥有
  Runtime/Fetch/permission configuration wait；install 模块拥有同步 Browser/physical replacement、Page disposal 以及
  尚未迁出的 dedicated-worker/BiDi compatibility continuation。`navigation_commit.rs` 再把 resource/lifecycle/activity
  continuation 与 `PendingLoadedPageInstall` 组合成 `PendingLoadedNavigationPageDisposal`，等待值仍不持有 frontend
  output/context 或 connection mutable capability；
- `navigation_completion.rs` 增加 `DisposeReplacedPage` pending/completed phase。正常 response-ready 路径现在是
  lifecycle → configuration → renderer commit/install → retired Page disposal → Inspector replay；generic `Loaded` 是
  lifecycle → Page restore → install/disposal → Inspector replay。每一段沿用同一个 exact
  `BackgroundNavigationGateKey`，install/disposal completion 仍为 `ParticipantPending`，只有 replay 或 rejected disposal
  后没有其他 tail 时才 `Terminal`。这不是 sleep/drain/retry：Page close acknowledgement 成为 Browser Host 可登记、
  frontend 不必用下一命令泵动的普通 participant input；
- engine residence 必须与 exact replacement 同 turn 提交。inner loaded engine 与 materialized outer engine 都在
  replacement 已得到 `BrowserPageOwnerKey` 后、发布 disposal participant 前 adoption；成功路径从 continuation 中取走
  engine，late disposal/replay completion 不再携带或回写它。测试给 lifecycle engine 注入共享同一 renderer owner、
  但带唯一 user-agent marker 的 payload，并在 disposal 尚未完成时直接验证 marker 已成为 active engine；因此慢旧
  Page close 不能让迟到 A engine 覆盖已经提交的 successor engine；
- lower-boundary 回归 `adapter_commits_request_history_and_exactly_one_page_generation` 先安装真实 predecessor Page，验证
  start 返回 disposal participant 时 Core generation、request commit 和 physical slot 已同步完成，再独立等待旧 Page
  关闭。production-shaped response-ready/generic 回归分别验证新增 disposal wake、旧 attachment response lease 在
  disposal 前已 retirement、disposal 后才启动真实 `Console.clearMessages` replay；
  `superseded_generic_loaded_restore_cannot_install_stale_page` 现在验证 A restore 被 B supersede 后先发布 rejected Page
  disposal，late disposal completion 不能更换 renderer attachment，且无需 replay 即 terminal。旧 wake 断言在新实现
  上稳定 `0/3`，失败均精确落在“下一个 participant 已不是 replay/terminal”的旧假设（nextest run
  `b8bb0f79-2ba8-4ae5-bfed-9ed150e681ae`）；最终四条聚焦回归 `4/4`（run
  `d3e90415-4084-4805-90dc-812491cf635b`），以 `--stress-count 20 --flaky-result fail` 共执行 80 次无失败（run
  `0bfb4edb-1102-47e2-ae3f-b2cfe0e215f5`），`moli-protocol` 全包 `3283/3283`（run
  `00c3215c-b1a2-400f-a072-b7730e23be34`）；
- clippy 要求将体积较大的 `BrowserPageReplacement` outcome 装箱后，最终四条聚焦回归再次 `4/4`（run
  `83afca1e-9d3d-46d1-b8bf-89b611795475`）；最终源码的 workspace 全量为 `15773/15773`、17 个既有 skip（run
  `fca92778-5c67-43bd-8ea7-bf9846110f17`）。workspace all-target clippy `-D warnings`、fmt/diff check 与 release
  workspace build 均通过；固定 `target/release/moli` SHA-256 为
  `04b843f649e2b90132a19e1355664cea3370a5c709873720e9cd2fbb7e80be64`。显式清除大小写
  HTTP/HTTPS/ALL/FTP proxy 与原 no-proxy、设置 `NO_PROXY=*` / `no_proxy=*` 后，CDP 默认全组 `210/210`、
  WebDriver Classic/BiDi/Selenium/Semantics `--continue-on-failure` `157/157`；两套 JSON 均为 `ok=true` 且失败
  列表为空；
- 本切片仍没有把 disposal 后的 dedicated-worker retirement、BiDi preload listener startup 或 loaded
  lifecycle/activity projection 变成独立 participant；这些 compatibility continuation 仍可能跨 `await` 借用
  `CdpConnection`，必须按 exact current Page/fact publication 语义另切，不能塞回 Page close wait。Fetch/termination
  compatibility drain、replay output normalization、protocol-neutral `CdpTurnOutcome`/fact channel 与 Host/frontend
  lifetime 也未改变。Phase 3 Exit gate 仍未通过；下一切片优先从 worker/BiDi post-install continuation 或
  Fetch/termination drain 中选一个单一边界。

2026-08-03 第三十九切片后 master 集成与 owner-progress 响应屏障修复记录：

- 本轮第一次 rebase 到 master `a3c9f06e2c` 后保留了上游新增的 network observation journal：Browser Core 的
  navigation fetch 继续返回 `NetworkFetchResult<NavigationResponse>`，protocol 在解包 response 时同时取得
  journal，不退化成只返回 body/head 的旧接口。direct `Target.createTarget(url)` 的首条 history 也不再从
  protocol 临时构造 initial-empty seed；调用方只通过 exact `BrowserPageOwnerKey` 标记
  `ReplaceInitialEmptyDocument`，authoritative initial-Document metadata 仍由 Core target registry 提供。受 master
  新增 Core topology 约束影响的测试 fixture 统一通过 production `insert_browser_context` / navigation adapter 注册
  context、Target、Page residence，没有重新开放 physical-only registry 作为 authority；
- proxy-cleared CDP tracing 复现出一个独立于 navigation participant 的循环等待：

  ```text
  Runtime.evaluate(awaitPromise)
    -> 页面同步创建 blob: DedicatedWorker
    -> auto-attach 要求 Worker pause-on-start
    -> Worker 等 Runtime.runIfWaitingForDebugger
    -> frontend 必须先收到 attachedToTarget 才能发 resume
    -> 旧 output slot 又把 DedicatedWorker lifecycle 扣到 evaluate response 之后
  ```

  LLDB 和临时 trace 共同确认 Worker 已创建 isolate、进入 pre-bootstrap debugger pause，Tokio owner loop 在正常
  park；不是 V8 自旋或 CPU profiler deadlock。诊断探针在修复前已全部删除。根因是
  `DedicatedWorkerTargetLifecycle` 虽然已经分类为 `OwnerAction`，却仍被错误地放在 `AfterResponse`；而当前 command
  正在等待只有该 owner action 才能解锁的 Worker，因此 response 本身永远不会产生；
- 冻结新的迁移期不变量：**response ordering fence 可以排序 observation，但不能扣住 pending command 完成所必需的
  owner progress / permit publication**。`DedicatedWorkerTargetLifecycle` 因此改为 `BeforeResponse`：frontend 可先收到
  `Target.attachedToTarget`，发送 `Runtime.runIfWaitingForDebugger`，Worker 执行并回传首条 message，最后原
  `Runtime.evaluate` response 才完成。这与 Chromium 的可观察顺序一致，也没有加入 sleep、retry、drain、额外 timeout
  或把任意 DCL/event 判成假的。新增
  `dedicated_worker_lifecycle_can_unlock_awaiting_runtime_response` 锁定 delivery 与 response order；通用 slot table 同时
  锁定该映射；
- 该例外不表示所有 CDP event 都是 owner progress：SharedWorker/ServiceWorker target lifecycle 和纯 observation 仍按
  自己的 occurrence/response 语义排序。判断依据不是事件名称，而是 exact output 是否携带当前 command 继续执行所需的
  browser/renderer permit。长期改造应把这种 permit-bearing progress 从 frontend response slot 中提取到 Browser Owner
  queue；届时 CDP 只投影已经发生的 Target fact。当前修改只消除迁移期 response barrier 的循环依赖，没有建立 typed
  Browser fact journal，也没有完成 Browser Owner queue / CDP frontend queue 分离，Phase 3 Exit gate 仍未通过；
- 首次修复时两条 slot 边界回归分别 `1/1`（nextest runs `b019e8f8-f410-472e-9710-90e6bf801d9f`、
  `41e5a58a-db32-4bb0-a09d-30ded8a977d1`）；proxy-cleared tracing group 完整通过后又连续复跑 10 次无失败。按本轮
  要求提交后再次执行 `git pull -r origin master`，master 从 `a3c9f06e2c` 前进到 `532fc0bb93`，48 个分支提交无
  conflict 重放。新 master 的 `d96aa3eaf6` 已包含相同的 production `BeforeResponse` 映射和强化 tracing smoke；rebase
  后本分支只额外保留 named invariant regression 与本设计记录，没有第二条 worker resume authority；
- 最终 master 基线上的 slot table/named regression 为 `2/2`（run
  `8baaeef0-f7a6-44e2-b778-a933b0511dbf`），`moli-protocol` 全包 `3327/3327`（run
  `518b9b4f-22b4-4867-9d7a-f47773dce266`），workspace 全量 `15960/15960`、17 个既有 skip（run
  `2ab4a775-354f-4f68-9f2e-052df9c1fc8a`）；workspace all-target clippy `-D warnings`、fmt/diff check 和 release
  workspace build 均通过。固定 `target/release/moli` SHA-256 为
  `7e463a829a4aa4846188e9fe76c60dfa80d637c9aa699c2a70d9387059e3f3b3`；显式清除大小写 HTTP/HTTPS/ALL/FTP proxy
  与原 no-proxy、设置 `NO_PROXY=*` / `no_proxy=*` 后，CDP 默认全组 `210/210`（其中 tracing profiler 为 107 个
  trace event、7 个 profile、5042 个 sample），WebDriver Classic/BiDi/Selenium/Semantics
  `--continue-on-failure` `157/157`；两套 JSON 均为 `ok=true` 且失败列表为空。

2026-08-03 第四十切片实现记录（Page replacement DedicatedWorker retirement 同步 apply）：

- 审计 `loaded_page_install.rs -> worker_target.rs -> session_owner.rs` 后确认，旧 Page 的 `close_async()` 已在
  disposal participant 中先结算 renderer lifetime；随后 exact owner Page 的 DedicatedWorker retirement 只剩
  Protocol worker target/session registry、Target observer delta 和 frontend session bookkeeping。原实现之所以跨
  `.await` 借用 `CdpConnection`，只是复用了同时覆盖 active Page binding cleanup 的通用 async helper；
  DedicatedWorker 分支本身没有 I/O 或会返回 `Pending` 的 operation；
- `retire_dedicated_worker_targets_for_replaced_page` 因此改为普通同步 owner apply。它先按 exact
  `TargetPageResidenceIdentity` 冻结属于 predecessor Page 的 renderer instance 列表，再逐个 prepare terminal outputs；
  `commit_dedicated_worker_retirement_sync` 在不可插入其他 actor turn 的同一调用中先移除该 worker registry/host，随后
  只从 prepared snapshot 投影 `targetInfoChanged -> detachedFromTarget -> targetDestroyed`。并行 Page residence 的
  worker 仍按 owner identity 保留，session route 和 Target host 均在函数返回前结算；
- 这刀没有创建一个永远 ready 的 `PendingDedicatedWorkerRetirement`，也没有增加 wake、sleep、retry 或 drain。generic
  worker lifecycle projector 仍保留 async terminal commit，供不是 Page-disposal exact batch 的其他 Target 路径使用；
  下一条真实异步边界仍是 BiDi preload listener startup，不能把它和本轮同步 registry transaction 合并；
- 边界回归改名为
  `page_replacement_retires_only_its_owned_dedicated_workers_synchronously`，调用点不再 `.await`，同时继续锁定 exact owner
  isolation、三类 CDP event 顺序、session route 清理和 Target host retirement。该回归单次 `1/1`（nextest run
  `af397305-e4af-4ade-b3a9-6f11ca821179`），stress `20/20`（run
  `c5d457f1-6c3c-4e11-a797-4b0c51dec18b`）；与 debugger-resume retirement、production-shaped disposal/replay actor
  回归组合为 `3/3`（run `13a6adba-af32-444c-b01f-4d45e4a5bb4f`）；
- 首次 `moli-protocol` 全包在 `3326/3327` 后暴露既有 named background popup fixture 的确定性过早条件（run
  `9c068854-ecc1-4d90-9be3-fa6e1c0cf632`）：fixture 看到 successor Page 已安装便发送下一条 Runtime command，却没有
  等同一 navigation 的 disposal/replay terminal，导致 command predecessor publication 已到达、仍被 exact navigation
  stream gate 合法阻挡。干净 `HEAD` detached worktree 同样 `1/1` 失败（run
  `a7ec1c99-5b77-48a9-a888-077ad0b39e0c`），证明不是本轮 worker 修改；测试 helper 现捕获唯一已 admission 的
  `BackgroundNavigationGateKey`，只消费真实 scheduler input 直到该 exact key 被 terminal completion 移除，不延长
  timeout。修复后原例 stress `20/20`（run `29baba4d-6188-465e-bfd3-afbc63501beb`），整个 47 项
  background-staging 模块 stress 3 轮 `141/141`（run `a83652a6-4f12-4b6d-a702-9c8d3fa1b526`），Protocol 全包最终
  `3327/3327`（run `27b83242-baae-4e75-a4cf-02e76224a0f6`）；
- workspace 全量最终为 `15960/15960`、17 个既有 skip（run
  `05894e27-cb0a-4333-a157-8585d0c9760f`）。workspace all-target clippy `-D warnings`、fmt/diff check 和 release
  workspace build 均通过；固定 `target/release/moli` SHA-256 为
  `93cc3887f5819ad5f5f0c4c6b0e2eccf18bda51ed53302fff4dc57a2704aee15`。显式清除大小写
  HTTP/HTTPS/ALL proxy 与原 no-proxy、设置 `NO_PROXY=*` / `no_proxy=*` 后，CDP 默认全组 `210/210`、WebDriver
  Classic/BiDi/Selenium/Semantics `--continue-on-failure` `157/157`；两套 JSON 均为 `ok=true`，进程退出码为 0；
- Chromium machine differential 使用 `/home/donoughliu/chromium/src/out/Default/chrome`
  (`Chrome/147.0.7709.0`) 和 Xvfb、清代理环境。默认配置会把 predecessor Document 放入 BFCache，因而其
  DedicatedWorker 在 successor load 后仍可存在；这不是 hard Page replacement 的可比样本。以 fresh profile 并禁用
  `BackForwardCache` 后，raw CDP 观察到 worker 依次发生
  `Target.targetCreated(attached=false) -> Target.targetInfoChanged(attached=true) -> Target.attachedToTarget ->
  Target.targetInfoChanged(attached=false) -> Target.detachedFromTarget -> Target.targetDestroyed`，随后
  `Target.getTargets` 已无该 worker。对同一 fixture 的 release Moli 探针得到相同六事件顺序；替换后的第一条真实
  command 同时读取 DOM 并执行 `fetch('/api')`，两边均返回 `plain ok` / `fixture api body`，且均不需要额外 pump command。
  这项 differential 证明本轮同步的是 hard-replacement 后已发生的 exact worker retirement；它不主张 Moli 已实现
  Chromium BFCache lifetime；
- 本切片只关闭 disposal completion 中 DedicatedWorker registry cleanup 的伪异步 borrow。navigation tail inline
  drain、`commit_loaded_navigation_async` compatibility wrapper、BiDi listener 真实 wait、Fetch continue/Target
  termination drain、loaded lifecycle/activity projection、replay `CdpTurnOutcome` normalization、typed Browser
  fact/outcome channel 和 Host/frontend lifetime 均仍在 production；Phase 3 Exit gate 继续保持未通过。

2026-08-03 第四十一切片实现记录（loaded lifecycle/activity prefix 同步 owner apply）：

- 对 `MainDocumentNavigationActivity::emit_loaded_navigation_commit_async` 做逐调用审计后确认，其函数体没有任何
  renderer/network wait：它只投影已冻结的 navigation result、Target info、main-resource metadata、pre-DCL network
  backlog 与 initial renderer lifecycle journal，并将后续 load observation 发布为
  `MainDocumentLoadOwnerAction`。原 `async fn` 和调用方的 `Box::pin(async move { ... }).await` 只是历史 future 形状，
  不是 participant；
- 该入口现改为同步 `emit_loaded_navigation_commit`。response metadata、renderer output fence、DCL/termination prefix 和
  deferred-load admission 在不可插入其他 actor turn 的同一 apply 中完成；若 Document 在 DCL 前已终止，同 turn 取消
  exact load visibility barrier。真正可能等待 renderer load 的 observer 仍由既有 load owner action 拥有，不被内联
  drain，也没有添加 sleep、retry、yield 或新 compatibility wrapper；
- 回归改为普通 `#[test]`，除继续锁定 lifecycle 只消费 frozen journal、不从 current Page 重新发现 child activity 外，
  还断言 `emit_loaded_navigation_commit` 返回前已经发布 exact `MainDocumentLoadOwnerAction`。单次 `1/1`（nextest run
  `a8b32a17-d00a-4021-9fbd-d8e48c8992e4`），stress `20/20`（run
  `7cb30f33-2d86-4f00-b63d-6d4f54decf6c`）；`moli-protocol` 全包 `3327/3327`（run
  `d09202b5-37f5-4fc5-8803-f85a1235f0fc`），workspace 全量 `15960/15960`、17 个既有 skip（run
  `515061ba-85fd-4ce1-9203-86a299e13269`）；workspace all-target clippy `-D warnings`、fmt/diff check 和 release
  workspace build 均通过。固定 `target/release/moli` SHA-256 为
  `ac209ed58617f9cd463be83b890dce25f361a7c530c911576e29adcb1cbe9653`；显式清除大小写
  HTTP/HTTPS/ALL/FTP proxy 与原 no-proxy、设置 `NO_PROXY=*` / `no_proxy=*` 后，当前 CDP 默认全组实际为
  `242/242`，WebDriver Classic/BiDi/Selenium/Semantics `--continue-on-failure` 为 `157/157`；两套 JSON 均为
  `ok=true`、失败列表为空；
- 本切片不改变可观察 event/response 顺序，也没有把 BiDi preload channel listener 的真实 renderer command wait
  同步化。`complete_loaded_navigation_page_install_async` 仍会跨该真实 wait 借用 `CdpConnection`；下一切片应把 BiDi
  listener startup 建模成 exact Page-owned participant，而不是把它伪装成同步 registry transaction。navigation tail
  inline drain、`commit_loaded_navigation_async` wrapper、Fetch/termination drain、protocol-neutral fact/outcome channel 和
  Host/frontend lifetime 也仍未迁移，Phase 3 Exit gate 继续保持未通过。

2026-08-03 第四十二切片实现记录（loaded-navigation BiDi preload listener participant）：

- 模块边界分成三层。`conn/runtime_eval.rs` 只拥有可复用的单次 `StartListener` / `ReleaseObjectGroup` renderer owner
  action 状态机；新模块 `domains/runtime/preload_listener.rs` 拥有 loaded-navigation 专用的 `realm × handoff` FIFO batch；
  `loaded_page_install.rs`、`navigation_commit.rs` 和 `navigation_completion.rs` 只负责把该 batch 表示成
  `StartPreloadListeners` navigation phase。没有把 realm 枚举、proxy materialization、listener restart 和 navigation
  terminal policy 重新集中到一个总管函数；
- loaded Page restore 不再只保留 execution-context id，现冻结并去重完整 `RendererRuntimeRealmInfo`。post-install apply
  因而不需要跨 wait 后重新查询“当前 realm”；它从 committed Document 的 frozen realm id、frame id 和 context id 构造
  exact listener job，保留 realm 顺序和每个 realm 内 handoff 顺序；
- 每个 job 依次启动 proxy handle materialization、listener dispatch，失败时再启动 exact object-group cleanup。所有真实
  Page dispatch/deferred-response wait 都由 `PendingBidiPreloadListenerBatch::wait(self)` 或
  `PendingBidiChannelOwnerAction::wait(self)` move-own；等待函数不接收、保存或借用 `CdpConnection`。每次 completion
  re-enter owner apply 后先验证 `BidiChannelPageOwner` 的 BrowserContext/Target/Page generation，再解析 handle、注册
  realm/object group 或启动下一 command。replacement generation 的旧 completion 直接 terminal，不进入 successor Page；
- listener startup 的完成条件是 listener command 已被 exact Page 接受并接入 response-ready lane，不是“收到第一条
  channel message”。若首条 reply 已在初始 renderer turn 中 ready，则其 `script.message` 暂存在同一 navigation output
  的 after-response 区；若仍 deferred，则 navigation gate 结束后由既有 response-ready input 独立发布并重启 listener。
  因而页面从不调用 channel 时也不会把 navigation 永久挂起；
- `preload_listener_batch_completion_rejects_replacement_generation_before_apply` 使用真实 Page 启动 proxy participant，
  在 wait 与 apply 之间推进 loaded Page generation，证明迟到 completion 不发 event、listener 或 cleanup 到 replacement
  Page。该回归 `1/1`（run `0d8e3013-0bbf-450a-ae2a-a6bc701f7706`）；lower listener/cancellation/stale 组合 `5/5`
  （run `646e185e-b354-4670-bd2a-a02a8432d9d6`），BiDi channel owner action 组合 `5/5`（run
  `632bf1ff-85fe-4ca7-9530-a3313dcda041`），direct/background navigation participant 组合 `2/2`（run
  `7cadc91b-c39e-4497-a047-3cd061ff5b65`）；
- WebSocket 集成覆盖 mutation observer、sandbox realm、token gate、existing Classic session、两 channel 等场景，组合
  `9/9`（run `4da34927-8c64-4dcf-a5ab-d25a0f7e1b85`）。`wait=none` loaded-navigation 回归进一步改为两个 channel，锁定
  navigate response 先于 `script.message` 且 first/second FIFO，单次 `1/1`（run
  `1d76535c-ef2f-473a-a58b-30aa23346df4`）；
- Chromium machine differential 使用 `/home/donoughliu/chromium/src/out/Default/chrome` 与 `chromedriver`
  (`147.0.7709.0`)、Xvfb、fresh WebDriver session 和清代理环境。raw BiDi 的 `wait=none` timeline 为
  `navigate response -> DOMContentLoaded -> load -> first script.message -> second script.message`；`wait=complete` 为
  `DOMContentLoaded -> load -> navigate response -> first script.message -> second script.message`。两种 wait policy 都证明
  navigation response 不等待首条 listener reply，且多个 channel 按 preload argument 顺序投影；本轮只把稳定的
  response-before-message 与 FIFO 纳入 Moli 回归，不额外绑定 DCL/load 的 frontend flush 位置；
- 首次 Protocol 全包在 `3327/3328` 暴露本轮引入的确定性 test-thread stack overflow（run
  `1260837a-d504-44f9-9a5b-996698ab708b`）。失败用例
  `local_storage_mutations_fan_out_across_targets_without_leaking_session_storage` 聚焦复跑为 `0/20`（run
  `5babd70f-adab-4dc6-8893-065a27bfbb62`），而 detached `a634a1a5af` 基线为 `1/1`（run
  `8e06d248-01ff-4371-8474-b052b5c8084d`）：原因不是 DOMStorage race，而是新增具体 preload async future 把通用
  `complete_pending_navigate_command` 从 `22392` 膨胀到 `28272` bytes，并继续嵌入所有 Page command wait；
- 修复没有增大线程栈或改变测试 timeout，而是在 `PendingNavigateCommand::wait` 与
  `complete_pending_navigate_command` 模块 API 上返回 type-erased boxed future。上层
  `PendingPageCommandDispatch::wait` 由修前 `28320` 降至 `7832` bytes，原失败用例修后 `1/1`（run
  `0119ec94-7547-4ba1-8f0c-78c892553b19`）、stress `20/20`（run
  `ab864324-65ef-49b5-94d0-d5ca699ca1ff`），Protocol 全包最终 `3328/3328`（run
  `838545b7-596b-4029-857f-b0bcbe0187df`）。这条边界同时防止未来新增 navigation phase 把具体 future 布局泄漏到
  所有无关协议 command；
- workspace 全量为 `15961/15961`、17 个既有 skip（run
  `56a7207b-0158-45c2-8f3d-7e54248ecbd6`）；workspace all-target clippy `-D warnings`、fmt/diff check 和 release
  workspace build 均通过。固定 `target/release/moli` SHA-256 为
  `e88e8c201a262c16edc7d29d3607d42e6006e7d316497969d4324f9b031e9e23`；显式清除大小写
  HTTP/HTTPS/ALL/FTP proxy 与原 no-proxy、设置 `NO_PROXY=*` / `no_proxy=*` 后，CDP 默认全组为
  `242/242`，WebDriver Classic/BiDi/Selenium/Semantics `--continue-on-failure` 为 `157/157`；两套 JSON 均为
  `ok=true`、失败列表为空；
- 本切片仍没有拆除非 loaded-navigation 的 BiDi listener compatibility helper；proxy/listener completion apply 也仍
  复用通用 async renderer output normalization，后者可能为 context id/DOM node 启动 Page lookup。navigation tail
  inline drain、`commit_loaded_navigation_async` wrapper、Fetch/termination drain、protocol-neutral fact/outcome channel 和
  Host/frontend lifetime 同样未迁移，Phase 3 Exit gate 继续保持未通过。

2026-08-03 第四十三切片实现记录（Runtime output normalization participant）：

- 新增 `conn/runtime_eval/normalization.rs`，把“消费冻结的 Runtime Page dispatch 输出”和“投影 CDP
  response/event”之间可能发生的 Page lookup 建模成独立状态机。状态机冻结 session、BrowserContext、Target、
  renderer-agent attachment 和 renderer Inspector session；先枚举 execution-context compatibility job，再枚举缺少
  subtype 的 object/function RemoteObject，按稳定 FIFO 每次只启动一个 `EnsureContextWorlds`、`ResolveContextId` 或
  `ResolveNode` Page command；
- `Page` 的 isolated-world attach 与 inspector-context-id lookup 新增对称的 `start_*` / `finish_*` API，原 async API
  只保留为兼容组合。`PendingRuntimeProtocolMessageNormalization::wait(self)` move-own 唯一
  `PendingPageCommand`，不保存或借用 `CdpConnection`；completion apply 先按冻结 route 重验 exact renderer attachment，
  失效时保留 immutable old-Page output 但停止剩余 lookup，随后 normalized routing 再按旧 attachment terminal drop，
  不从 successor Page 重新发现 node/context；
- loaded-navigation 的 `NormalizeProxy` batch operation 和 lower `Normalize` BiDi owner action 现在显式承接上述
  participant。proxy materialization、listener startup 以及失败 cleanup 的 completion apply 均为同步函数；
  `loaded_page_install.rs`、`navigation_commit.rs` 与 `navigation_completion.rs` 不再为了 Runtime normalization 跨
  `await` 借用整个 connection。listener 的首条 deferred reply 仍是独立 response-ready input；内部
  `Runtime.releaseObjectGroup` response 不携带 context id 或 RemoteObject，因而可直接进入 already-normalized route；
- `node_normalization_completion_does_not_enter_replacement_renderer_attachment` 使用真实旧 Page 执行
  `Runtime.evaluate(document.body)` 并等待 DOM-node lookup 完成，然后在 apply 前切换到新的 renderer attachment；回归
  证明旧响应不会借用 replacement Page 补 subtype、stale attachment 输出不会投影 frontend，且 successor attachment
  保持不变（nextest `1/1`，run `03fccd48-e3ea-4e4f-89d1-58f43ee4103d`）。既有 node subtype、isolated-world
  console context-id、preload replacement-generation、BiDi listener/channel 回归继续锁定非 stale 路径的可观察语义；
- node/context/stale/DOMStorage 聚焦组为 `6/6`（run `41291738-df30-45fc-804e-36bc509de92c`），BiDi channel/listener
  owner 组合为 `10/10`（run `aecaeea8-fb21-4172-adf3-977281379112`），WebSocket preload 集成为 `12/12`
  （run `41d9b6eb-39ae-4487-adc7-77459a7d6f0d`）。首次 workspace clippy 精确发现 normalization continuation
  内嵌后造成三个 large-enum variant；修复没有添加 lint allowance，而是让 Runtime step、BiDi owner action 与 preload
  batch 全程保持 boxed participant，避免把大 continuation 再复制进外层 command/navigation future；
- 最终 `moli-protocol` 全包为 `3329/3329`（run `6237eb3e-9a61-418d-866b-13adad34658b`），workspace
  为 `15962/15962`、17 个既有 skip（run `1880aeda-ec7b-4e50-9d94-6a50313f8025`）。workspace all-target
  clippy `-D warnings`、fmt/diff check 与 release workspace build 均通过；固定 `target/release/moli` SHA-256 为
  `51917858c7026a27fdaab54f3dd2660ac0a0abe6048931d5ed5e30b9415854e9`。显式清除大小写
  HTTP/HTTPS/ALL/FTP proxy 与原 no-proxy、设置 `NO_PROXY=*` / `no_proxy=*` 后，CDP 默认全组为 `242/242`，
  WebDriver Classic/BiDi/Selenium/Semantics `--continue-on-failure` 为 `157/157`；两套 JSON 均为 `ok=true`、失败
  列表为空；
- 本切片没有删除 `complete_runtime_protocol_message_for_session_owner_async`：它现在是显式 drain 新 participant 的
  compatibility wrapper，仍服务 runtime realm-created、`Page.createIsolatedWorld`、run-immediately preload、initial
  `about:blank` 和部分 replay 调用方。Fetch continue/Target termination drain、navigation tail/loaded commit wrapper、
  protocol-neutral Browser fact/outcome channel 与 Host/frontend lifetime 也仍未迁移，Phase 3 Exit gate 继续保持未通过。

2026-08-03 第四十四切片实现记录（`Page.createIsolatedWorld` preload participant）：

- `Page::runtime_realm_inventory_async` 被拆成对称的 `start_runtime_realm_inventory` /
  `finish_runtime_realm_inventory`，async API 只保留为兼容组合；Core `Page` 继续拥有 renderer command 的启动与结算，
  Protocol 不直接读取 renderer realm payload；
- `CreateIsolatedWorldCommandTask` 新增 `RuntimeRealmInventory` 和 `PreloadListeners` 两个显式 phase。world renderer
  command 完成后先冻结 `executionContextId`，随后每个 pending value 独占自己的 participant wait；apply turn 才重新取得
  `CdpConnection` 和 exact Page。listener/proxy/cleanup 复用 `domains/runtime/preload_listener.rs` 的 FIFO batch，未在
  Page domain 复制第二套 BiDi channel 状态机；
- realm-inventory completion 复用 world 创建时冻结的 `RendererAgentAttachmentId` 做 exact revalidation。若旧 Page 在
  wait 期间被 replacement，命令保持已经冻结的 world 成功响应，但停止 listener startup，不能
  从新 Page 重新发现同 id realm，也不能修改 successor attachment；listener batch 自身仍在每次 operation apply 前
  重验 frozen Page owner；
- 新增 `create_isolated_world_channel_listener_advances_through_owned_participants`，证明实际 phase 顺序为 renderer world
  command → realm inventory → preload listener batch；新增
  `stale_realm_inventory_does_not_enter_replacement_page`，证明 replacement 后保留冻结响应且不产生 successor
  Inspector await。全部 `create_isolated_world` 聚焦用例为 `20/20`（run
  `83c9d277-5d90-4584-8f4f-c41d537bbfc1`），BiDi preload WebSocket 集成为 `12/12`（run
  `9a7a3570-ad72-4afa-915a-32a298148f4c`）；
- 首次 workspace 全量在 `15962` 项通过后有两个与本切片无关的 worker DNS-failure 用例触发 5 秒外层 timeout；
  两项以 nextest `--stress-count 10 --flaky-result fail` 复核为 `10/10`（run
  `c5516fef-3839-43ef-b765-e3b5a28ebc1f`），未改测试、timeout 或产品行为；随后相同 workspace 全量为
  `15964/15964`、17 个既有 skip（run `e412d57a-c440-4a8a-b0d7-4c5ac9d1f18e`）；
- 按约定提交后执行 `git pull -r origin master`，实际引入 `f16860e4fb` 的 process-signal crate 并无冲突地重放
  本分支 54 个提交。最终 rebased tree 的 `moli-protocol` 为 `3331/3331`（run
  `72c479e6-4804-4306-be85-d428f3a411a5`），workspace 为 `15966/15966`、17 个既有 skip（run
  `33841610-833a-4d5c-ae59-50b457e9f9ea`）。workspace all-target clippy `-D warnings`、fmt/diff check 与 release
  workspace build 均通过；固定 `target/release/moli` SHA-256 为
  `a84c1c64950f1756fdf0e3c8badf8d129ca0c1de6595fd58838f1eb5cdcbfec9`；显式清除大小写 HTTP/HTTPS/ALL/FTP
  proxy 与原 no-proxy、设置 `NO_PROXY=*` / `no_proxy=*` 后，CDP 默认全组为 `242/242`，WebDriver
  Classic/BiDi/Selenium/Semantics 为 `157/157`，两套结果均为 `ok=true`、失败列表为空；
- 本切片只迁出 `Page.createIsolatedWorld` 的 post-create compatibility wait。runtime realm-created、run-immediately
  preload、initial `about:blank` 与部分 replay 仍通过 generic Runtime completion compatibility wrapper；Fetch
  continue/Target termination drain、navigation tail/loaded commit wrapper、protocol-neutral Browser fact/outcome
  channel 与 Host/frontend lifetime 也仍未迁移，Phase 3 Exit gate 继续保持未通过。

2026-08-03 第四十五切片实现记录（Page add-preload command participant）：

- `domains/runtime/preload_listener.rs` 新增可复用的 `BidiPreloadListenerSetup`。它把 execution-context id 到 exact
  realm inventory、再到既有 `realm × handoff` listener batch 串成同一 move-owned participant chain；realm phase 冻结
  `BidiChannelPageOwner`，wait 只拥有 `PendingPageCommand`，apply 先重验 exact BrowserContext/Target/Page attachment，
  replacement 后直接终止旧 setup。`Page.createIsolatedWorld` 因而删除自己复制的 `RuntimeRealmInventory` phase，
  只保留一个委托给该 setup 的 `PreloadListeners` phase；
- 新模块 `domains/page/preload/add_command.rs` 内聚 target-scoped
  `Page.addScriptToEvaluateOnNewDocument` 的状态机。Target 脚本注册表是先提交的 durable state；当前 Page renderer
  install/run-immediately 是冻结 `RendererAgentAttachmentId` 的独立 phase，BiDi channel 的 realm/listener 工作再委托给
  Runtime setup。每个 pending value move-own 唯一 renderer participant，`PendingPageCommandDispatch::wait` 不借用
  `CdpConnection`，completion apply 才重新进入 command owner route；
- replacement 不回滚已提交的 Target 注册，也不把旧 renderer completion 重放到 successor Page：若 attachment 在
  install wait 期间改变，命令返回已经分配的 identifier，旧 completion 被丢弃，新 Page 后续只通过其正常
  document-start registry 输入取得脚本。`CompletedAddScriptToEvaluateOnNewDocumentCommand` 同时透传 renderer output
  predecessor，保持 command/event 的 causal ordering；
- Page command 路径不再用“立即 ready 的 pending command”包住一个随后 inline-await 的实现。BiDi/其他
  protocol-neutral target route 暂时通过 `execute_direct_async` compatibility adapter 本地 drain 同一状态机；这条
  adapter 没有复制语义，但仍属于待迁出的 frontend-owned async 组合，不能据此宣称所有 preload caller 已解耦；
- 新增 `channel_run_immediately_advances_through_owned_participants`，用真实 V8 isolated world 与 channel handoff 证明
  renderer install → realm inventory → listener batch 的 phase 顺序；新增
  `stale_renderer_completion_keeps_registry_without_entering_replacement_page`，证明 replacement 后 identifier/Target
  registry 保留、successor attachment 不变且没有遗留 Inspector await。连同 create-world setup replacement 回归为
  `4/4`（run `e90074a5-802c-4b5b-b2c4-305249e59ddb`）；既有 add-script/create-world/DOMStorage 聚焦组为 `41/41`
  （run `3c4babaf-9aae-47ca-8999-9b0f23d97be7`），BiDi command mapping 为 `4/4`（run
  `6fb36a22-26a1-41ef-a386-7c3846881a2e`），BiDi preload WebSocket 集成为 `12/12`（runs
  `4fd2e288-f44c-4d77-9a53-ffa15f4d50b5`、`66f3e8ee-ee17-426e-9406-d56e491db25d`）；
- 提交前 `moli-protocol` 全包为 `3333/3333`（run
  `9d6a16c1-c36f-469a-bbc5-61452c693830`），workspace 为 `15968/15968`、17 个既有 skip（run
  `b5ab6cdf-7b2a-4983-9050-5466ff0d4345`）；随后按约定执行 `git pull -r origin master`，实际吸收
  `c597ac97dc` 的 WebGL shader pipeline compatibility 修复，55 个分支提交无冲突重放。最终 rebased tree 的
  workspace 全量为 `15969/15969`、17 个既有 skip（run
  `1bd03d94-d392-4f13-8c74-f0a2cd7e14d3`），workspace all-target clippy `-D warnings`、fmt/diff check 与
  release workspace build 均通过；固定 `target/release/moli` SHA-256 为
  `ee240e810bef9c50a36e7f19de89c71aea9b7e08ef15f2a981a3d67a1577d9e7`。显式清除大小写
  HTTP/HTTPS/ALL/FTP proxy 与原 no-proxy、设置 `NO_PROXY=*` / `no_proxy=*` 后，CDP 默认全组为 `242/242`，
  WebDriver Classic/BiDi/Selenium/Semantics `--continue-on-failure` 为 `157/157`；两套 JSON 均为 `ok=true`、失败
  列表为空；
- 本切片没有拆除 runtime realm-created/create-target initial `about:blank` 的 listener compatibility helper，也没有迁出
  `execute_direct_async`、Fetch continue/Target termination drain、navigation tail/loaded commit wrapper、
  protocol-neutral Browser fact/outcome channel 或 Host/frontend lifetime，Phase 3 Exit gate 继续保持未通过。

2026-08-03 第四十六切片实现记录（Runtime command normalization participant）：

- 新模块 `domains/runtime/dispatcher/command_normalization.rs` 内聚 Inspector dispatch completion 到 exact-Page
  normalization participant 的 transition。普通 Runtime command 与 `Runtime.add/removeBinding` 共用
  `RuntimeCommandNormalizationContinuation`；它 move-own deferred-response receiver、page-owner access permit、binding
  task 与 timing state，不把这些 continuation 字段散回总 dispatcher；
- `PendingRuntimeCommandKind::ProtocolMessageNormalization` 现在是 Runtime command 的显式 phase。Inspector dispatch
  apply 只调用 `start_runtime_protocol_message_completion`；遇到 context-id compatibility 或 DOM-node subtype lookup 时
  立即返回 move-owned pending command，wait 不借用 `CdpConnection`，completion input 回来后才调用
  `complete_runtime_protocol_message_normalization`。多 job normalization 每次只启动一个 exact Page command；
- normalization 已完成的 output 只走同步
  `route_normalized_renderer_command_turn_output_into`，删除无人使用的 async
  `route_renderer_command_turn_output_into` wrapper，不能在 final routing 中再次隐式执行第二轮 Page lookup；renderer
  output predecessor 仍随同一个 Runtime command plan 投影；
- replacement 回归第一版稳定悬挂超过 60 秒，暴露出新 participant boundary 上原先不可能插入的真实 interleaving：旧
  Page normalization completion 被 stale-drop 后，command 仍等待旧 deferred-response receiver。修复在 final apply 前
  比较 frozen `RendererAgentAttachmentId`；attachment 已 stale 时同步返回
  `Execution context was destroyed by navigation`、清理 pending Inspector await 并丢弃 receiver，不能进入 successor
  Page 或形成 orphan command；没有增加 timeout、retry、sleep 或 drain；
- 新增 `node_result_normalization_is_an_owned_runtime_command_phase`，证明 `Runtime.evaluate(document.body)` 依次经过
  Inspector dispatch 与 command-owned normalization 两个 participant，并保留 `subtype=node`；新增
  `stale_owned_runtime_normalization_does_not_enter_replacement_page`，证明 replacement 后旧 command 以 error 终止、
  successor attachment 不变且没有 pending await。两条聚焦回归 `2/2`（run
  `ca8d4a16-300e-409b-b46d-69bfae18ffd3`），Runtime evaluate/binding 组 `141/141`（run
  `cfe1e6da-c278-46db-a584-687e2d7c26cb`），Protocol 全包 `3335/3335`（run
  `cba829d3-ff5a-4c91-b973-ce43f4af91e1`），Protocol all-target clippy `-D warnings` 与 fmt/diff check 通过；
- 本切片完成后已执行 `git pull -r origin master`，重放到 `origin/master@03ef15db11`。rebase 在前序 Runtime
  normalization extraction 与 master 新增的 DOM whitespace projection 参数上发生一次内容冲突；合并保留 extracted
  exact-Page participant，并在启动 node snapshot 时冻结当前 session 的 `include_whitespace`，没有恢复已删除的 async
  route wrapper。rebase 后 `cargo nextest run --no-fail-fast` 为 `15979/15979`、17 skipped（run
  `f385df49-e08c-485d-8441-70d1adc38bc3`），workspace all-target clippy `-D warnings`、fmt 与 diff check 通过；
- rebase 后 workspace release build 通过，smoke 固定使用
  `target/release/moli`（SHA-256
  `b9dc6bb8c3e6b74b0c9c436e7dde82f28d088d3bc6b5c8ebca99ce2e177d8e06`）。清空大小写
  HTTP/HTTPS/ALL/FTP proxy 与原 no-proxy、设置 `NO_PROXY=*` / `no_proxy=*` 后，CDP 默认全组为 `244/244`，
  WebDriver Classic/BiDi/Selenium/Semantics `--continue-on-failure` 为 `157/157`；两套 JSON 均为 `ok=true`、失败
  列表为空；
- `complete_runtime_protocol_message_for_session_owner_async` 的 production caller 已从普通 Runtime/Runtime-binding
  completion 删除，只剩 protocol-neutral `ReleaseObjects` helper compatibility drain。本切片没有迁出 runtime
  realm-created/create-target initial listener、targeted preload direct adapter、Fetch continue/Target termination、
  navigation tail/loaded commit wrapper、protocol-neutral Browser fact/outcome channel 或 Host/frontend lifetime，Phase 3
  Exit gate 继续保持未通过。

2026-08-03 第四十七切片实现记录（protocol-neutral `ReleaseObjects` participant chain）：

- 新模块 `domains/runtime/dispatcher/release_objects.rs` 内聚一个外层 `ReleaseObjects` transaction。状态机 move-own
  `remaining_handles`、当前 object correlation、已产生的 protocol events 与最新 exact renderer predecessor；没有把
  多 handle continuation 塞回通用 dispatcher 或 Browser Core；
- `start_devtools_runtime_command_dispatch(ReleaseObjects)` 不再调用会串行完成整个 handle list 的 async helper。它先按
  target/realm 过滤未知或错误 realm 的 handle，再把每个已知 handle 作为标准 `Runtime.releaseObject` command task
  启动；外层 scheduler 每次只收到一个 `PendingDevToolsRuntimeCommandDispatch`，完成后才在同一个 apply turn 启动下一
  handle。重复 handle 在首次成功注销后自然跳过，不产生额外 renderer command；
- 每个内部 release 复用现有 Runtime dispatcher 的 attachment freeze、normalization 和 deferred-response correlation，
  不另造第二套 Inspector completion 语义。当前 handle 的 plan 先吸收该等待期间的 interleaved protocol events；多
  handle 产生的 events 按顺序累积，renderer predecessor 取顺序链中最新的 exact fence，最终与外层 Empty/error result
  一起返回；
- replacement 发生在旧 Page command wait 与 completion apply 之间时，既有 exact attachment 检查把首个 response
  转成 `Execution context was destroyed by navigation`。外层 release state 立即 terminal，不会重新按 target 找 Page
  或启动剩余 handles。未知 handle 与真实 V8 `NoSuchHandle` 仍保持 WebDriver/BiDi disown 的幂等语义；没有增加
  sleep、retry、timeout 或 owner drain；
- `complete_runtime_protocol_message_for_session_owner_async` 已降为 `cfg(test)` fixture helper，production caller 为
  零。无 scheduler participant loop 的 direct protocol-neutral adapter 仍明确保留本地 release/normalization drain；
  本切片不以消灭所有 Protocol `await` 为目标，也没有把 frontend-local response wait 误称为 Browser Owner blocker；
- 新增 `devtools_release_objects_exposes_each_known_handle_as_an_owned_participant`，覆盖 unknown + 两个已知 + duplicate
  handle，证明恰好产生两个不同 internal command participant 且最终 registry 清理；新增
  `stale_release_objects_participant_does_not_enter_replacement_page`，在首个 renderer completion 后替换 Page，并给
  successor registry 注入同 ID handle，证明旧 outer command 以 stale error 终止且不会删除 successor handle。两条
  聚焦回归 `2/2`（run `fc175008-5d62-44cc-a357-2e0e0e808023`）；既有 direct script evaluate/callFunction 语义回归
  `1/1`（run `54bef051-af94-4cd2-8654-a17819b143a7`）；
- 本切片 rebase 前验证：Protocol 全量 `3344/3344`（run `50f0a851-18a9-4d7f-8b3e-383e4f16cc35`）；workspace 全量
  `15981/15981`、17 skipped（run `f4d1a7fe-349c-4152-84cd-0dd941cb5203`）；Protocol 与 workspace
  `cargo clippy --all-targets -- -D warnings` 均通过；release workspace build 通过，`target/release/moli`
  SHA-256 为 `3d213c2c12801bdda24657d30aa225a4376ca5458f6491b4d4a8d30051e5ff62`；完全清空代理环境并锁定该
  二进制后，CDP smoke `244/244`、WebDriver Classic/BiDi/Selenium/semantics smoke `157/157`；
- 本切片完成后先执行 Phase 3 exit audit，不继续按 wrapper 数量推进。navigation tail/loaded commit、runtime
  realm-created/initial `about:blank`、targeted preload direct adapter、Fetch continue/Target termination、Browser
  outcome/fact channel 与 Host/frontend lifetime 仍需按上述四条防雕花判据重新分类，Phase 3 Exit gate 继续保持未通过。

2026-08-03 第四十七切片 rebase 集成记录：

- 已执行 `git pull -r origin master`，57 个分支提交重放到 `origin/master@37a4e248a0`。冲突合并保留 master 新增的
  完整 `BrowserIdentityProfile` 与 reload request kind，同时保留本分支 Browser Owner typed operation、navigation
  trace 和 exact participant 边界；没有恢复已经删除的 Protocol execution authority；
- rebase 后补齐两处 production seam：renderer reload 的 trace wrapper 继续把
  `BrowserNavigationRequestKind` 传入 owner navigation；runtime load configuration 比较从 Core-owned active fetch
  configuration 读取 BrowserIdentity，不再回到已经删除的 physical engine fetch-config authority；
- master 新增的 reload header 回归改为并发推进 test-only Browser Host owner turn 与等待服务器请求，验证无需额外
  frontend command，renderer reload 仍自主到达网络层；BrowserIdentity 回归改为通过 Core registry 安装
  BrowserContext，并等待 exact background navigation gate 后再读取新 Document，避免测试 fixture 绕过 owner 或跨
  Document 竞争；
- workspace 首次验证 `16010/16011` 时，唯一失败
  `devtools_command_executes_input_key_command_without_cdp_sidecar` 在相同用例 20 次压力复跑中为 `17/20`。根因是测试
  把 DCL 后独立发布的 post-parse autofocus rendering task 当成 load completion 的组成部分；按键命令可能在 autofocus
  前合法完成，因此 input value 偶发仍为空。这是测试同步错误，不是 key dispatch completion 或 scheduler sidecar
  丢失；fixture 改为 parser script 同步 `focus()`，并新增 key dispatch scheduler sidecar 必须为空的断言。没有增加
  sleep、retry、timeout，也没有放宽结果断言；修复后同一用例压力复跑 `50/50`（run
  `a5ba8c5e-1666-4b55-aa15-c404502fedfe`），Input 边界 `71/71`（run
  `6486380f-e19f-40ed-9816-5e411a0c3310`）；
- rebase 集成最终验证：Protocol 全量 `3348/3348`（run `6109c928-7081-4f8c-9287-bbb8cecfa842`），workspace
  全量 `16011/16011`、17 skipped（run `01185ece-f856-41a6-864c-d8be46cfe2f5`），workspace all-target clippy
  `-D warnings`、fmt 和 diff check 通过；workspace release build 通过，`target/release/moli` SHA-256 为
  `6d7f513b3bf6fa84184ae984395af9f847478d4d2e4fcaa6dfc2beb6eb1e0172`。完全清空大小写 HTTP/HTTPS/ALL/FTP
  proxy 与原 no-proxy、设置 `NO_PROXY=*` / `no_proxy=*` 后，CDP smoke `244/244`、WebDriver
  Classic/BiDi/Selenium/semantics smoke `157/157`；
- 本次 rebase 只做 master 语义与既有 owner 边界的窄集成，没有扩大 Phase 3 范围。下一步仍先执行严格 exit audit，
  再优先推进 protocol-neutral outcome/fact channel 或 Host/frontend lifetime；不能因为这些集成修复宣称 Phase 3
  已完成。

必须新增的 actor regressions：

- `Page.navigate` response 后 parser script 导航，不发下一命令也进入 B；
- frontend 完全不 enable Page domain，renderer navigation 仍执行；
- 插入/删除 noop command，browser trace 完全相同；
- slow protocol output queue 不延迟 replacement HTTP request start；
- old Page intent 在 B commit 后被 stale-drop。

Exit gate：protocol scheduler 中不存在 renderer-sourced top-level navigation execution authority。

### Phase 4：command navigation 与 renderer navigation 汇合

状态：已完成（第76切片 exit audit）。第49切片已迁移 raw CDP 顶层跨 Document `Page.navigate` 的 admission/start authority，第50切片再把
selection 后的 participant lifetime 迁给 Browser Host，第51切片拆开 Host terminal 中已有的 neutral outcome 与 Protocol
projection sidecar，第52切片又把 response-head early response 改成 neutral outcome + frontend projection，第53切片把
raw CDP 顶层 same-Document `Page.navigate` 的分类/start authority 也迁入 exact Browser Host turn，第54切片再迁移 raw CDP
顶层 `Page.reload` 的 admission、current-URL selection 与 participant lifetime，第55切片把 raw CDP
`Page.navigateToHistoryEntry` 的 destination resolution、Document 分类与 participant lifetime 汇入同一 lane，第56切片再把
带 DCL/load wait 的 BiDi/Classic 顶层 navigate/reload 接入该 mailbox 与 Host participant loop，第57切片继续迁移 direct
history traversal，并删除 Classic 的 frontend history snapshot/entry 选择，第58切片再把 Page.crash/Page.close 的 exact
terminal action 接入同一 mailbox，第59切片又把显式 top-level `Target.closeTarget` 的 ordinary admission、Core commit、
retired-Page disposal 与 retained-Target promotion 接入同一 Host participant lane，第60切片再把 popup/auxiliary Target
navigation 从 Protocol scheduler 搬到 protocol-neutral Browser Owner input + Host participant lane，第61切片最终删除
Page/Target termination 的 paused-fetch Protocol admission，第62切片继续把 direct BiDi/Classic `wait:none` 顶层
navigate/reload/history 的 admission/start authority 汇入同一 mailbox，第63切片再迁移 raw CDP `Page.stopLoading` 的
admission、current-Document selection 与 renderer stop participant，第64切片继续把 stop-loading paused-Fetch cancellation 的
每个真实 renderer wait 暴露为同一 Host participant lane，第65切片再迁移 raw CDP request-stage 主文档
`Fetch.failRequest` 的 admission/decision participant，第66切片继续迁移 raw/nested CDP request-stage 主文档
`Fetch.continueRequest` 的 exact request mutation、network fetch、auth transition 与 Document build participant，第67切片再把
raw/nested CDP 主文档 `Fetch.continueWithAuth` 的 terminal/retry decision 与后续 network/Document participant 收入同一 lane；
第68切片在 Phase 4 exit audit 后先补齐 BrowserContext exact instance capability，作为 context disposal owner admission 的必要
前置条件；第69切片再把 production raw CDP 与 typed BiDi/Classic 的 `Target.disposeBrowserContext` admission、logical reservation、
paused-Fetch/Page cleanup participant lifetime 和 terminal Context removal 接入同一 Browser Host lane；第70切片继续把 raw/nested CDP
response-stage 主文档 `Fetch.continueResponse` 的 admission、response release 与 Document build 接入同一个
`ResolvePausedNavigation` participant lane；第71切片再把 raw/nested CDP 主文档 request/response-stage
`Fetch.fulfillRequest` 与 response-stage `Fetch.failRequest` 接入该 lane；第72切片继续把 production typed BiDi/Classic
terminal Fetch decision 暴露成 application scheduler task：main Document 进入相同 Owner lane，subresource 保留 exact Page
participant，二者等待期间都不借用 `CdpConnection`；第73切片再删除 download network/artifact action 对 frontend response flush 的
progress dependency，并以有界 projection gate 保持 response/event 顺序；第74切片继续把 `Target.createTarget`、
`Runtime.runIfWaitingForDebugger` 与 `Page.enable` 的 initial target URL replacement 汇入同一个 protocol-neutral Browser Owner input；
第75切片又把 renderer-produced top-level history delta 从 output projector 的 session-routed direct completion 改为 exact Page
`RendererTopLevelHistoryTraversalInput`，由 Browser Host selection 解析 Core history 并持有后续 Page participant；第76切片最终把
`Page.createIsolatedWorld` 的 initial-target prerequisite 迁成 opaque Browser command + frontend continuation sidecar，并删除旧 nested
navigate participant 与 direct helper。child-frame 仍是 Page/renderer action；无 Host loop 的 direct-call compatibility wrapper 不再是
production blocker，detached `wait:none` completion transport 归 Phase 5 outcome audit。background-target audit 已确认普通 parked Page
navigate/reload/history completion 不会 promotion 或绕过 exact owner。Phase 4 Exit gate 已通过：production
`ProtocolSchedulerWork` 不含 navigation、replacement、popup 或 termination browser-owner payload。loaded-Document tail 仍可能进入既有
`MainDocumentLoadOwnerAction` compatibility residence，但它是 Phase 5 fact/outcome subscriber 迁移项，不再误算为 Phase 4 navigation
action。

目标：所有 top-level owner action 进入同一个 Browser Owner queue。

按风险依次迁移：

1. `Page.navigate`；
2. reload、history traversal、same-document navigation；
3. popup/auxiliary target navigation；
4. Page/Target termination；
5. download navigation 和 `Page.stopLoading`；
6. background target park/promote 中的 navigation completion。

每迁一类必须删除对应 protocol execution path，不能长期双写。

command response 使用 Browser Core accepted/completed outcome；CDP/BiDi/Classic 各自附加 wait policy。

2026-08-03 第四十九切片实现记录（raw CDP top-level cross-Document `Page.navigate` start cutover）：

- Core 新增 protocol-neutral `BrowserFrontendCommand::Navigate` 与 `BrowserCommandId`。mailbox input 不包含 CDP
  request id、session、domain subscription、wire payload 或 socket；只包含 opaque correlation、exact
  `PageResidenceIdentity`、URL 和 referrer；
- raw CDP frontend 仍负责参数解析以及 child-frame/same-document 分类。确认是顶层跨 Document 后，它只把 command
  publish 到 `BrowserHostHandle` 并等待一次 exact start reply；不再直接调用 network/navigation start。Host 未安装或
  已停止会返回 typed publication error，禁止回退到旧 direct path；
- CDP id/session/result payload 保存在 `CdpConnection` 的临时 prepared projection，以 opaque `BrowserCommandId`
  与 Core turn 关联。Core actor selection 后，physical executor 先重验 exact Page residence，再取出 projection 并启动
  现有 navigation state machine，因此 stale Page 返回 `NoSuchTarget`，不会在 successor Page 上执行；
- actor 只把已经启动的 `PageCommandTaskStep` 交还 frontend command 的 participant chain。等待期间 production actor
  仍独立 poll Browser Host；若 frontend 在 start reply 前消失，send 失败会把该 step 留给 Browser Host participant
  chain，而不是取消已选择的 Browser action。完整 protocol-neutral accepted/completed outcome 留到本阶段后续切片，
  本切片不冒充 Phase 5 fact channel。actor 已成功交出 reply 后，后续 participant 当前仍属于该 frontend command；
  因而“selection 后 frontend 消失也不影响 completion”尚不是本切片保证；
- 保留既有 raw CDP `Page.navigate` 的 background/early-response policy；迁移只改变 start authority，不把导航改成
  foreground drain。全量初跑曾把该 policy 错设为 foreground，稳定暴露 lifecycle、child-frame、download 与 response
  ordering 回归；恢复 background policy 后代表性五项 WebSocket ordering 回归通过，禁止以后为了 owner cutover 再改变
  frontend wait policy；
- non-flattened `Target.sendMessageToTarget` 原先在 Target completion 内调用 direct helper 并 inline await 完整 nested
  command。nested `Page.navigate` 进入 Browser Host 后，这会持有应用 turn、无法回到 actor `select!`，在全量矩阵最后
  两项稳定悬挂。现在 outer Target command、nested command context、scheduler sidecar 与 renderer boundary 都由
  `PendingSendMessageToTargetCommand` move-own；每一段 nested pending 都返回 application actor，Browser Owner selection
  后再恢复原 Target wrapper。shared direct-turn settle 只负责最终 output projection，不获得 Browser Host execution
  authority；
- 新增 exact regression：frontend dispatch 后 URL 保持 `about:blank` 且 actor mailbox 恰有一个 command；只有
  Browser Host selection 后才产生 navigation participant，最终 response 保留原 CDP id/session 并提交目标 URL。
  另一个回归在 selection 前丢弃 frontend wait，验证 Browser Host 接管 participant 后目标 URL 仍提交；两项
  owner-selection 回归包含在 Page navigation `77/77`（run `54234356-b6aa-4acf-9e5c-34fb12097064`）。nested Target
  owner-selection/full-response 回归 `2/2`（run `52977f74-364b-4340-abdb-d3934a10d0ae`），所有
  `sendMessageToTarget` 回归 `7/7`（run `60dc0d29-4ae2-4665-a6f8-62f30b73ff32`），Target domain 全组
  `557/557`（run `67c3797a-8578-4ae0-8921-82ab42da9e67`）；
- workspace 首轮在 HTTPS proxy CONNECT auth 用例暴露唯一 legacy fixture：该用例直接赋值 physical
  `browser_context`，没有调用 `insert_browser_context` 注册 Core Target/Page authority，因此 owner-selected navigation
  正确返回 `NoSuchTarget`。修复只把该 fixture 改为统一注册入口，未放宽 407/ExtraInfo/loadingFailed/errorText 断言；
  exact stress `10/10`（run `d7a1a428-f6a5-43bc-a9c8-79bf38b967cc`）、Fetch navigation-subresource
  `28/28`（run `17e2a1cb-f72b-4ab8-b6fb-04bc860bd1d6`），最终 workspace nextest
  `16013/16013`（run `729c58a1-d21a-4b92-b3b3-66cc3d20f3d4`）；
- 最终 gate 通过 `cargo fmt --all --check`、`cargo clippy --workspace --all-targets -- -D warnings` 和
  `cargo build --workspace --release`。release binary SHA-256 为
  `55863f8d8b1d2ae3b9802f4e9c4a1f59e3b9d41cc405699eba1490c882f7e1a6`；清除大小写
  HTTP/HTTPS/ALL/FTP proxy 与原 no-proxy、设置 `NO_PROXY=*` / `no_proxy=*` 并固定该 binary 后，CDP 默认全组
  `244/244`、WebDriver Classic/BiDi/Selenium/Semantics `--continue-on-failure` `157/157`，两套 JSON 均为
  `ok=true`、失败列表为空；
- 本切片刻意不迁移 same-document/child-frame 分类、reload/history 或 BiDi/Classic direct adapter，也不为了“零 await”
  清理 Runtime/preload wrapper。下一刀先决定 command participant completion 是否应继续回到 frontend，还是由
  Browser Host 保持到 accepted/completed neutral outcome，再按同一边界迁移 reload/history。

2026-08-03 第五十切片实现记录（Browser Host-owned command navigation participant lifetime）：

- actor selection 后不再把 `PageCommandTaskStep` 交给 frontend。`BrowserHostPageStepCompletion` 随
  `PendingBrowserHostTurn` / `CompletedBrowserHostTurn` move-own terminal disposition；application-side
  `BrowserHostExecutionLane` 注册每一段 exact wait，并把 completion 作为独立 owner wake 重新 apply。frontend 是否 poll
  自己的 command reply 不再决定 network、renderer commit、replacement 或 navigation tail 是否继续；
- prepared projection 只给 Browser Host 一份共享原 command response-flush lifetime 的空 detached
  `CommandDispatchContext`，不复制 frontend 已产生的 event/boundary。terminal 时 live frontend 收到
  `CompletedBrowserOwnerNavigateCommand { plan, context }`，再把 renderer predecessor、insertion boundary 和
  post-response ordering 原样接回原 command；它只会收到 terminal plan，不会再次获得 pending participant；
- frontend 在 selection 前或 selection 后丢弃 receiver 时，oneshot send failure 不取消 Browser action。Browser Host
  删除 abandoned command response，但通过 `into_composite_command_prefix` 保留 owner/background events、renderer
  boundary、predecessor 与 scheduler sidecar，继续由应用 owner loop 投影；因此不能以“没人收 response”为由丢掉
  已发生的 browser side effect；
- non-flattened `Target.sendMessageToTarget` 仍必须在原 outer Target wrapper 内封装 nested response/event。为此 live
  receiver 接收 terminal Protocol projection，而不是让 Browser Host 旁路写入 CDP socket；这是 Phase 5 fact journal
  建立前的显式迁移形状。下一切片应把 terminal plan 拆为 protocol-neutral accepted/completed outcome 与独立 projection
  sidecar，而不是复制这份 Protocol completion 到 reload/history；
- 新增三条 exact lifetime 回归：frontend 在 selection 前丢弃、selection 后丢弃、以及 frontend 保持 live 但完全不 poll
  terminal reply，Browser Host 都能提交目标 URL；live frontend 最终仍只投影原 id 的一次 response。owner-selection
  `3/3`（run `0ff81b30-f724-45fb-b294-bd5d7d975a4d`），slow frontend exact `1/1`（run
  `2df7f868-e8df-49f4-a554-0d1240c7f039`），Page navigation 全组 `79/79`（run
  `f63d18c3-d6cb-48e5-8d7d-a949664b603b`），`sendMessageToTarget` `7/7`（run
  `cb9f0968-08cd-4dc7-ac4c-47bc178663d8`），application Browser Host wake 回归 `1/1`（run
  `6062324a-f456-4edb-a6b5-4f0e3b5cc2ce`）；
- protocol package 首轮 `3352` 项仅有一个无关 run-immediately binding 用例在全并发下失败；exact stress `20/20`
  （run `399b70bc-3587-48e6-bd58-6ab5c0460a21`）和 lifecycle 模块 `57/57`（run
  `3d45e8b2-3dca-4b49-9647-dd017787b1f0`）均通过；protocol package 原命令重跑 `3352/3352`（run
  `26f07707-2604-4d6b-9dac-02c822754f80`），最终 workspace `16015/16015`、17 个既有 skip（run
  `6316b0d9-4163-4ade-ac25-08340ac936f4`）。没有用 retry/sleep 或放宽断言掩盖，也没有在缺少因果证据时修改无关
  production 路径；
- 最终 gate 通过 `cargo fmt --all --check`、`git diff --check`、
  `cargo clippy --workspace --all-targets -- -D warnings` 和 `cargo build --workspace --release`。release binary
  SHA-256 为 `40b0350045b3f2e1bd926b0877a0325dab376f9067eaf8ebf1c0edb4a320b1d1`；清除大小写
  HTTP/HTTPS/ALL/FTP proxy 与原 no-proxy、设置 `NO_PROXY=*` / `no_proxy=*` 并固定该 binary 后，CDP 默认全组
  `244/244`、WebDriver Classic/BiDi/Selenium/Semantics `--continue-on-failure` `157/157`，两套 JSON 均为
  `ok=true`、失败列表为空。

2026-08-03 第五十一切片实现记录（neutral navigate outcome / exact Protocol projection sidecar）：

- Core 新增独立模块 `browser_host/command_outcome.rs`，定义
  `BrowserNavigateCommandOutcome::{Completed, Rejected}`、protocol-neutral result/error/kind。`Completed` 只表示 command
  response policy 已到 response boundary，不表示 Document 已达到 DCL/load；类型不保存 frontend command id、session、
  CDP code、JSON 或 output route；
- Protocol 新增聚焦模块 `domains/command_output/browser_navigate.rs`。Browser Host terminal 先从
  `CommandOutputPlan` 拆出 typed outcome，再把剩余 owner/background events、post-response events、renderer predecessor 与
  insertion boundary 留在 `BrowserNavigateCommandProjection`。sidecar 只保留 CDP response 的 exact insertion index、是否在
  renderer boundary 之前、session inheritance、wire error code/data 和未知扩展字段；frontend 收到 terminal reply 后才在
  原 command 或 non-flattened `Target.sendMessageToTarget` wrapper 内复原一次 wire response；
- response 与 renderer boundary 相邻时，删除 response 后两者索引可能相等，不能根据新索引猜原侧。sidecar 因而显式冻结
  `before_renderer_boundary`，投影时只按该事实恢复 boundary；单元回归同时锁住 response 前后 Browser event、未知 result
  字段、Target/loader、error code/data 与 session shape。若 frontend 已丢弃，Browser Host 直接消费 sidecar 的 effects-only
  plan，既不制造 `id: null` response，也不丢 Browser event/fence；
- 一个正确 terminal plan 最多有一个 response。异常的多 response 或 outcome/projection shape divergence 不使用 production
  panic；adapter 记录 error 并收敛成一个 internal command error。普通 successful background navigation 的 Host terminal
  plan 当前没有 response，因为 `BackgroundNavigationEarlyResult` 会在 response headers ready 后直接向 Protocol background
  channel 发送 typed CDP response；本切片将其表示为 `outcome: None`，不把“Host participant 已结束”误写成“command response
  已 ready”。下一切片应迁移这个 early-response producer，而不是把 `None` 长期扩散到 reload/history；
- outcome/projection 单元与现有 Page navigation、frontend selection 前/后 drop、slow frontend、Browser Host wake、nested
  `sendMessageToTarget` 聚焦合计 `91/91`（run `3882b100-8596-4e9f-a7b6-fbd7b839def5`）；Protocol package
  `3355/3355`（run `ab5b8b9e-f121-4dd1-96aa-733c085f2c77`）；workspace `16019/16019`、17 个既有 skip
  （run `68466a36-0601-48e1-b08f-22ee9dcdbb99`）；
- `cargo fmt --all --check`、`git diff --check`、`cargo clippy --workspace --all-targets -- -D warnings` 和
  `cargo build --workspace --release` 均通过。固定 `target/release/moli` SHA-256 为
  `2753ab1d01803880d4317c2b6a99aed78be23c9c021568f6f4c747ac43fe9a41`；清除大小写 HTTP/HTTPS/ALL/FTP proxy
  与原 no-proxy、设置 `NO_PROXY=*` / `no_proxy=*` 后，CDP 默认全组 `244/244`、WebDriver
  Classic/BiDi/Selenium/Semantics `--continue-on-failure` `157/157`；两套 JSON 均为 `ok=true`、失败列表为空。

2026-08-03 第五十二切片实现记录（early navigate outcome / shared Browser-frontend output FIFO）：

- 删除 network load job 中保存 raw `navigate_id + session_id + result_payload`、并直接构造
  `BackgroundProtocolEvent::command_success` 的 `BackgroundNavigationEarlyResult`。新
  `BackgroundNavigationEarlyOutcome` 只持有 shared output sender 与 `BrowserNavigateCommandOutcomeDelivery`；到达 inline/data
  Document commit 或普通 HTTP response headers boundary 时，它发布 Core `BrowserNavigateCommandOutcome`，不在 producer
  task 内构造 wire response；
- Protocol sidecar 继续独立保存 `FrontendCommandId`、`DevToolsSessionKey` 和第51切片的
  `BrowserNavigateCommandProjection`。Core outcome 仍只保存 requested URL、Target/loader、error/download 分类，不新增
  CDP id、session、JSON、wire code 或 event route。frontend 消费 delivery 时才按 sidecar 恢复 exact command response；
  projection 异常记录 error 并收敛为一个 internal command error，不增加 production panic；
- 不能为 neutral outcome 另开 mpsc。HTTP 路径当前依赖同一 sender 上的 `requestWillBeSent/responseReceived` progress、early
  command response 和后续 Network progress 的 exact FIFO；两个 channel 即使都先 drain ready prefix，也无法判断“另一个
  channel 中的 outcome”发生在某个 Network event 之前还是之后。为此新增聚焦模块
  `conn/background_output.rs`：private `BrowserBackgroundOutput::{ProtocolEvent, NavigateCommandOutcome}` 共用一个物理
  channel，`BackgroundEventSender` 兼容包装既有 fact producer，`BrowserBackgroundOutputReceiver` 是唯一 projection
  ingress。application、TestContext、CDP/BiDi/Classic loops 只消费投影后的 event，不获得 Browser execution authority；
- FIFO 单元回归直接读取物理 carrier，锁住 `Protocol event -> neutral navigate outcome -> Protocol event`，并证明中间元素在
  receiver projection 前不是 `BackgroundProtocolEvent`。early producer 回归再锁住原 id/session/result wire shape；Page
  navigation/participant `81/81`（run `c553c3d6-6f6e-43cd-b94e-fa3217c0eed3`）、nested
  `Target.sendMessageToTarget` `7/7`（run `42866be3-3c75-428e-bf6d-52924e4987f3`）、application scheduler ordering
  `4/4`（run `e85ba5c2-efea-46a0-bd18-17314ebed40a`），outcome/FIFO 聚焦 `5/5`（run
  `c53a2120-a5bf-4899-8d7e-3df6c2915975`）；
- Protocol 全包 `3356/3356`（run `a37af90a-250d-4fec-80e7-1cd61dc20cb4`）；workspace
  `16020/16020`、17 个既有 skip（run `27349be2-5050-494a-bc14-3ac6f640edc6`）。
  `cargo fmt --all --check`、`git diff --check`、`cargo clippy --workspace --all-targets -- -D warnings` 和
  `cargo build --workspace --release` 均通过；固定 release binary SHA-256 为
  `bce3714d3f1cf13873297b5e352d970b0684a31715f8599c13a15c1f9a534785`；清除大小写
  HTTP/HTTPS/ALL/FTP proxy 与原 no-proxy、设置 `NO_PROXY=*` / `no_proxy=*` 后，CDP 默认全组 `244/244`、WebDriver
  Classic/BiDi/Selenium/Semantics `--continue-on-failure` `157/157`，两套 JSON 均为 `ok=true`、失败列表为空；
- 这是 Phase 4 的 command outcome transport 切片，不是 Phase 5 fact journal。Network/lifecycle/Target producer 仍可直接
  生成 `BackgroundProtocolEvent`，channel 仍是 frontend-owned unbounded migration queue，Browser Host lifetime 也没有
  因此独立。下一刀应审计 reload/history/same-document 中哪一条已真正取得 owner authority，并让它复用同一 neutral
  command outcome seam；不得把 mixed FIFO 包装描述成最终 journal，也不得为了消除 `outcome: None` 延迟普通
  `Page.navigate` 的 response-head ack。

2026-08-03 第五十三切片实现记录（raw CDP top-level same-Document `Page.navigate` owner classification）：

- raw CDP `Page.navigate` frontend 只保留参数解析与 child-frame 判断；所有顶层命令都冻结 exact
  `PageResidenceIdentity` 并 publish 同一个 protocol-neutral `BrowserFrontendCommand::Navigate`。frontend 不再在 publish 前
  读取 current URL、决定 same/cross Document，也没有 Host unavailable 时的 direct fallback；
- Browser Host turn 先以 exact residence 解析 owner route，再以 none-session owner override 读取该 Page 当前 URL并执行
  fragment classification。same-Document 分支也以 none-session owner route start/complete Page participant，原 frontend
  session 只留在外层 pending response projection；它不能成为 renderer operation 的 execution route。cross-Document
  分支保持第49—52切片既有 start、background response-head ack 与 neutral outcome policy；
- frontend 预建的 cross-Document result payload 在 same-Document 分支不再成为事实。Browser Host 依据 frozen owner Target
  重建 `{ frameId }` result，明确不生成 `loaderId`，最终仍通过第51切片的 neutral outcome + Protocol sidecar 恢复原 CDP
  id/session。`Page.navigatedWithinDocument` concrete publication 走独立 renderer scheduler ingress；测试在完全不 poll
  command receiver 时先观察到该 publication 与 URL/history projection，之后才取 frontend response；
- 新增 owner-selection 回归锁住：dispatch 后 mailbox 为 1 且 URL 未变；Browser Host selection 后 same-Document
  participant 独立完成；renderer ingress 不等 frontend；response 保留原 session/frame 且无 `loaderId`。exact `1/1`
  （run `7ef321b1-2793-4da9-8798-68bd6c8130d6`）、same-document 聚焦 `14/14`
  （run `2e8bd848-7055-4ab4-aadd-645171b03b3e`）、Page navigation `80/80`
  （run `e65400c2-0832-4ed6-a2c1-31ff8661768b`）、nested `sendMessageToTarget` `7/7`
  （run `ed829be1-c791-4c93-a46e-15107dc5527f`）、Browser Host independent wake `1/1`
  （run `19d6cdfa-2415-4a59-a24c-dcedeb29519d`），Protocol package `3357/3357`
  （run `1db41986-ad5c-4973-8b1b-adbfa9cae329`）；
- 首次 workspace 全量为 `16020 passed / 1 failed / 17 skipped`（run
  `a78b0610-62ef-481d-a34c-a27393f7125b`）。唯一失败是既有 renderer lifecycle FIFO witness 在 2 秒后 timeout，
  不是顺序反转：fixture 在 Page 创建返回后无条件 `try_recv` 清空 publication queue，高负载下会删掉已经到达的 exact
  DCL/action 证据。测试修复删除该无条件清空，仍按 exact Page 与 record kind 过滤，且保留严格
  `lifecycle -> action` 断言和原 timeout；修复前 exact stress `20/20` 未复现（run
  `b581a220-ed63-4a47-a1a1-b6135ee0f764`），修复后 exact `50/50`（run
  `6092cd5f-0dc1-4bc4-9e2c-98af43aa9e99`）、相邻 handler-navigation 6 项 × 20 轮 `120/120`
  （run `63c96a54-edb9-4c75-b883-d64a759f2a90`）、renderer package `7013/7013`、3 个既有 skip
  （run `47d05f08-2271-477c-b5d3-afe9e9cd663b`）通过。该 test-only 修复独立提交，不改变 renderer production
  ordering、断言、timeout 或重试 policy；
- 最终 workspace 全量 `16021/16021`、17 个既有 skip（run
  `f9aeb89a-3ada-402b-b5b4-9dc0e9e58638`），workspace all-target clippy `-D warnings`、fmt 和 diff check
  均通过；workspace release build 通过，`target/release/moli` SHA-256 为
  `0786599c8871eff90f50c07062fab2fbefa45919dbd59d9e83bc9ace5aa0fff4`。显式清空大小写
  HTTP/HTTPS/ALL/FTP proxy 与原 no-proxy、设置 `NO_PROXY=*` / `no_proxy=*` 并固定该 binary 后，CDP 默认全组
  `244/244`、WebDriver Classic/BiDi/Selenium/Semantics `--continue-on-failure` `157/157`；两套 JSON 均为
  `ok=true`、失败列表为空；
- 本切片只关闭 raw CDP 顶层 same-Document command 的 owner classification。renderer-sourced same-Document intent 已走
  Browser Owner action，但 history traversal、child-frame 与 protocol-neutral direct adapters 仍有各自 authority；下一刀
  优先把 raw CDP reload 的 admission/start 变成新的 protocol-neutral Browser frontend input，不回到逐 wrapper 清零。

2026-08-04 第五十四切片实现记录（raw CDP top-level `Page.reload` owner cutover）：

- Core 新增 `BrowserFrontendCommand::Reload` / `BrowserReloadCommandInput` 与独立
  `BrowserOwnerInputKind::FrontendReload`。input 只保存 opaque `BrowserCommandId`、exact `PageResidenceIdentity`、
  `ignoreCache` 和 `scriptToEvaluateOnLoad`，没有 CDP id/session/JSON/result route，也刻意没有 frontend 快照的 current URL；
  reload options 穿过 neutral boundary，但本切片不改变 physical engine 对这两个既有参数的支持程度；
- raw CDP frontend 仍负责 `ReloadParams` 解析和既有 `BrowserContextNotLoaded` / `TargetNotLoaded` 前置错误；确认存在 exact
  Page 后只 publish Browser Owner input，并以 detached command context 等待 terminal projection。Host 未安装或停止仍返回
  typed publication error，禁止恢复 direct fallback；
- navigate/reload 没有各建一份 prepared registry 或 completion channel。原 navigate-only correlation map、pending/completed
  envelope 与 Page completion variant 已泛化为 frontend navigation 共用结构；Browser Host executor 先把 Core command
  归一化为物理 navigation action，再统一取一次 prepared Protocol sidecar。现有
  `BrowserNavigateCommandOutcome` / projection seam 继续承载空 reload response，frontend session 只在最终 `{}` response
  projection 中出现，不参与 execution route；
- Browser Host 只有在选择 exact turn、通过 `target_page_owner_route_if_current` 后，才以 none-session owner route 读取当前
  URL、收集 crash recovery observers、标记当前 history entry 为 reload replacement 并启动 Page participant。stale residence
  直接返回 `NoSuchTarget`，不能 reload successor Page；raw CDP owner path 与尚未迁移的 direct adapter 共用
  `start_reload_current_page_command`，没有复制 history/request/commit 语义；
- Core neutral-input exact 回归 `1/1`（run `d49ca64e-5111-4e96-bcef-5d3211ce9603`），Browser Host 边界
  `98/98`（run `b2514cde-fa91-4592-8a77-1b834a9712d5`）。reload owner 回归证明 frontend dispatch 后 mailbox 为 1
  且 HTTP request count 不变；Browser Host 在完全不 poll frontend receiver 时完成第二次 request/replacement，之后 frontend
  才收到原 session 的空 result。该 exact 回归最终 `1/1`（run `6b83d1e8-7e4f-426f-bdfa-b11f718db7a9`），reload
  相关过滤 `66/66`（run `f2d97da8-9e87-4c9c-89d8-d17898dfc1f8`），Page navigation `80/80`
  （run `65b2b782-eb2f-4b3f-bacd-7ede30be4ebe`）；
- Protocol 首次全量的唯一失败是既有 preload run-immediately 用例：同名 child world 偶发已收到 top-level script。单独
  `50/50` 通过，但 lifecycle 模块 `20` 轮复跑在第 15、19 轮复现，证明它依赖 suite load/order，而非 reload 路径；仅把
  fixture 改成 production-shaped exact owner 安装后，模块复跑仍失败 `2/20`，因此没有把该修改误当成修复。最终定位为测试
  在 child Document 尚未发布 exact `Page.frameStoppedLoading` 时注册 preload，合法地与 child world materialization 竞争；
  fixture 现在先等待该 child frame 的 exact stopped-loading fact，再清空事件并注册脚本，没有增加 sleep/retry/timeout 或
  放宽断言。相邻同类用例 `50/50`（run `06071cbb-04b8-4ef0-9241-a90c6d569d05`）、lifecycle 模块
  `1140/1140`（57 tests × 20，run `b3a72282-51dd-4f27-b03a-c78ca6712b6b`）；
- 修正测试同步后 Protocol 全量 `3356/3356`（run `0fbbe029-35f7-4743-b994-4f239cf793ca`），workspace 全量
  `16021/16021`、17 skipped（run `deedd704-ef4f-462b-b0da-e3d79aab1464`）；
- `cargo fmt --all --check`、`git diff --check`、`cargo clippy --workspace --all-targets -- -D warnings` 与
  `cargo build --workspace --release` 均通过；固定 `target/release/moli` SHA-256 为
  `7e7da1d0202776f3258992138cf62773349363bbb7b097bf391d04b509c02b48`。显式清除大小写
  HTTP/HTTPS/ALL/FTP proxy 与原 no-proxy、设置 `NO_PROXY=*` / `no_proxy=*` 后，CDP 默认全组 `244/244`、WebDriver
  Classic/BiDi/Selenium/Semantics `--continue-on-failure` `157/157`，两套 JSON 均为 `ok=true`、失败列表为空；
- 本切片不宣称 Phase 4 完成。raw CDP history traversal 仍在 frontend 解析 destination/current history 并直接启动
  participant，child-frame navigate 和 BiDi/Classic protocol-neutral adapters 也仍有 direct authority；下一刀优先审计并迁移
  raw CDP `Page.navigateToHistoryEntry`，同时保持 same-Document/cross-Document destination classification 在 exact owner turn。

2026-08-04 第五十五切片实现记录（raw CDP history traversal owner resolution）：

- Core history 模块新增 protocol-neutral `BrowserHistoryTraversalDestination`、
  `BrowserHistoryTraversalResolution` 与 typed error。entry、delta、current-entry no-op、目标 URL 和
  `document_sequence_number` 的 same/cross-Document 分类现在只在 `BrowserNavigationHistory::resolve_traversal` 实现；原
  Protocol snapshot 分类算法已删除，旧的 frontend `entry_id -> URL` lookup API 也从 Core/Protocol 整条删除；
- `BrowserNavigationOwner::resolve_exact_navigation_history_traversal` 先按 `PageResidenceIdentity` 重验 generation，再允许 lazy
  seed 与 destination resolution。stale Page 返回 exact typed error，既不能读取 successor history，也不改变 cursor/seed；
  direct DevTools entry/delta adapter 暂未迁入 mailbox，但已只做 destination shape/error 投影并调用同一个 Core resolver；
- raw CDP frontend 只解析 `entryId`、冻结 exact Page 并 publish
  `BrowserFrontendCommand::TraverseHistory(BrowserHistoryTraversalCommandInput)`。input 不含 destination URL、current index、
  same-Document flag、CDP session/id/JSON；navigate/reload/history 继续共用既有 prepared correlation map、Host participant 与
  neutral outcome/projection seam，没有新增 history queue 或 completion channel；
- Browser Host 选中 input 后进入 exact none-session owner route，在同一 turn 调用 Core resolver。no-op 直接完成；
  same-Document 先启动 renderer history delta participant，renderer 拒绝时才按既有语义 fallback 到 URL navigation；
  cross-Document 先标记 Core pending traversal，再启动同一 Browser navigation request/commit pipeline。missing entry 保留 raw CDP
  `-32000 / Navigation history entry not found`，stale residence 使用 `-31998 / NoSuchTarget`；
- exact 聚焦回归 `5/5`（run `50deab10-1fc2-4769-a043-274a91ee76d6`）：Core 锁住 Document-sequence 分类且 resolution 不移动
  cursor，stale Page 不能解析；cross-Document background Target 在 frontend receiver 完全未 poll 时完成 traversal 且不 promotion；
  missing entry 只在 owner turn 解析；same-Document traversal 在 response receiver 未 poll 时仍发布 renderer fact、保持原 Page
  residence 与 JS realm。Core history 过滤 `70/70`（run `83cbb689-f60b-412b-933d-0f19ccf6e4b8`），Protocol history
  `37/37`（run `1e70bdec-3593-4c85-9a11-782946041a5b`），Page navigation `81/81`
  （run `2e118e4c-33c3-4ecb-bb48-0420d62014de`）；
- Protocol 全量 `3356/3356`（run `9cf1f7ee-dee9-4b09-a19f-19a4e9d9d937`），workspace 全量
  `16024/16024`、17 skipped（run `7f84665c-1a0f-4c81-a80d-c958e0140f30`）；
- `cargo fmt --all --check`、`git diff --check`、`cargo clippy --workspace --all-targets -- -D warnings` 与
  `cargo build --workspace --release` 均通过；固定 `target/release/moli` SHA-256 为
  `be39221cf2b09691dc3940efc2cf1c657d671988c094c4cd0e56f56c9231ebdb`。显式清除大小写
  HTTP/HTTPS/ALL/FTP proxy 与原 no-proxy、设置 `NO_PROXY=*` / `no_proxy=*` 后，CDP 默认全组 `244/244`、WebDriver
  Classic/BiDi/Selenium/Semantics `--continue-on-failure` `157/157`，两套 JSON 均为 `ok=true`、失败列表为空；
- 本切片只关闭 raw CDP entry-based history command 的 owner authority。BiDi/Classic/direct DevTools traversal 仍在调用方本地
  admission/drain；child-frame `Page.navigate` 属于 Page/renderer owner，不应仅为“所有 Page 命令都入 Browser queue”而错误
  提升为 top-level Browser action。下一刀先审计 direct frontend navigation adapter 的 Host composition 与 wait policy，再决定
  迁移该入口，或转向 popup/termination 这类明确的 Browser owner action。

2026-08-04 第五十六切片实现记录（direct BiDi/Classic navigate/reload owner admission）：

- production BiDi/Classic 的顶层 `Navigate` / `Reload` 在 wait policy 为 DCL 或 load 时，先通过新的
  `navigation_owner_adapter` 冻结 exact Page residence，再 publish 既有 protocol-neutral
  `BrowserFrontendCommand::{Navigate, Reload}`；current URL、same/cross-Document 分类、HTTP start 与 replacement 仍只在
  Browser Host 选中 turn 后发生。adapter pending envelope 只 move-own Browser completion、DevTools context 与 result
  projection，不保存 `&mut CdpConnection`，也没有 Host unavailable 时的 direct execution fallback；
- application 新增 navigation dispatch participant loop。它等待 direct frontend reply 时，在同一个 scheduler owner turn 中
  interleave Browser Host selection、exact participant completion 与 background output；每个已经 dequeue 的 concrete input 都完整
  apply 后才重新竞争 reply/timeout。foreground navigation 持有显式 renderer ingress gate，新 Page stream 只有在 terminal Host
  projection 提供的 exact renderer boundary 上才能定向进入；不能在 replacement 的 root-Document binding 安装前由普通 renderer
  receiver 抢跑。frontend timeout 把 move-owned completion 交给 `BrowserHostExecutionLane`，该 lane 继续持有同一 gate、完成 projection
  并释放 exact stream；frontend receiver drop 不取消已经 publish 的 Browser command。child-frame navigate 明确保留 Page/renderer
  route，不能为了形式统一提升为 top-level Browser action；
- `allow_background_navigation` 是 Protocol prepared sidecar 的 response-boundary policy，不进入 Core neutral input。raw CDP
  继续使用 response-head early outcome；direct DCL/load 命令使用 foreground Host participant。Classic reload 没有 wire command
  id，因此 prepared sidecar 以 `BrowserCommandId` 生成只在 physical Page pipeline 内使用的 opaque correlation id，使 terminal
  neutral outcome 能返回原 waiter；该 id 不进入 Core execution semantics，也不暴露到 CDP/BiDi/Classic result；
- `wait:none` 本切片刻意保留既有 direct background path：它的 early completion 目前仍进入第52切片的 mixed physical FIFO，
  在 Phase 5 neutral outcome receiver 建立前再增加一条 direct correlation queue 会重新引入跨 channel 猜序。direct history
  traversal 也尚未迁入 mailbox；这两项是显式 migration boundary，不是静默 fallback；
- 调度顺序回归暴露了一个既有错误假设：Browser Host terminal completion 与 renderer DCL/load publication 是两个独立输入，
  Host completion 先到时，drain deferred load work 并不能证明 exact lifecycle fact 已被 scheduler dequeue。external navigation
  wait 现在对 DCL 和 load 都按 frozen Document token + lifecycle epoch + milestone 观察；renderer 内部状态已经 `Reached` 仍不能
  代替 frontend 实际观察到对应 fact。DCL delivery 因此也携带 exact renderer Document/epoch，而不再只有 frame/loader 文本；
- 最初实现允许 pending direct navigation 的普通 renderer receiver 与 Host completion 竞争，稳定暴露出一个真实违反路径：新 Page
  的 child-frame attach 先按旧 root-Document binding 进入 Protocol 并被 stale-drop，随后 load fact 却能进入，最终 BiDi `getTree`
  看不到 renderer 已存在的 iframe。修复点是上述 commit-before-renderer ingress gate，不是增加 tail drain；iframe/DCL/timeout 组合
  聚焦 stress `10/10`、共 `40/40` execution 通过（run `20c13508-21f4-4ddc-8142-7a5c40366399`）；
- Fetch auth pause 是另一种边界：该 Browser turn 可以只发布 auth-required/browser effects，而原 navigation command result 要由
  `continueWithAuth` 以同一 correlation 稍后完成。direct adapter 保留既有 `MissingDevToolsCommandResult` 非终态，不把它改写成
  `BrowserNavigateOutcomeMissing` 真错误，也不伪造 terminal outcome；该精确 BiDi 回归 stress `20/20` 通过（run
  `7e9ff6ff-3730-435c-8b57-7cb3d99dca1c`）。Fetch continue 仍是 Phase 3/4 的 compatibility participant，后续 neutral fact/outcome
  channel 应接管这段 correlation；
- exact Protocol 回归证明 direct BiDi command 的 frontend reply 完全不 poll 时，Browser Host 仍先完成 same-Document action并
  发布 renderer fact，之后才投影 typed reply。Page navigation 模块 `82/82`（run
  `f3848157-d387-4d8c-aabc-2cc92bddecfc`）；最终 Protocol 全包 `3357/3357`（run
  `35496ad9-08c1-4e85-8e07-fc940f1a9ef3`）；最终 application 全包 `562/562`（run
  `0a85f966-5bf7-4ef3-b419-42938e4fa5bd`）；workspace 全量 `16026/16026`、仓库既有 skip `17`（run
  `64f8de94-6a79-40e8-890f-705d9bb0d588`）。workspace clippy、fmt check、release build 均通过；清空代理后 release
  默认 CDP smoke 返回 `ok:true`，WebDriver `classic,bidi,selenium,semantics` 四组也返回 `ok:true`。没有 sleep、retry、timeout
  放宽或把旧 Document 的 DCL 判成假事件；
- 本切片不宣称 Phase 4 完成。下一刀应优先迁移 direct history traversal，或进入 popup/Target termination 这类明确的
  Browser-owner action；`wait:none` 应等待 neutral outcome ingress 设计，不为追求“零 direct wrapper”而建立临时第二队列。

2026-08-04 第五十七切片实现记录（direct BiDi/Classic history traversal owner admission）：

- `BrowserHistoryTraversalCommandInput` 从 entry-only 改为携带 protocol-neutral
  `BrowserHistoryTraversalDestination::{Entry, Delta}`。它仍只包含 opaque command id、exact Page residence 和 destination；
  current cursor、destination URL、Document sequence、frontend session/id/JSON 都不跨 Core input boundary。raw CDP 继续提交
  entry，BiDi 和 Classic 提交 delta，三者由同一个 Browser Host turn 在重验 exact Page 后调用 Core resolver；
- `navigation_owner_adapter` 现在接收带 DCL/load wait 的 direct `TraverseHistory`，复用第56切片的 Browser completion waiter、
  application Host participant loop、timeout detach 与 renderer ingress gate，没有建立 history 专用 queue 或 inline drain。
  `wait:none` 仍显式返回既有 background path，原因与 navigate/reload 相同：Phase 5 前 neutral early-outcome correlation 尚未
  建立，不能为了形式统一再增加第二条跨 channel 结果队列；
- Core terminal `BrowserNavigateCommandResult` 新增可选 `BrowserHistoryTraversalResult::{Noop, SameDocument,
  CrossDocument}`。Browser Host 初始 resolution 冻结分类；若 renderer 的 live same-Document history operation 返回 false 并
  进入 URL fallback，exact participant completion 把分类改成 `CrossDocument` 后再生成 neutral outcome。BiDi/Classic projector
  因此只把 neutral metadata 变成 `Empty` 或 `TraverseHistory { same_document }`，不读取 mutable Page/history；raw CDP wire response
  仍保持 `{}`；
- Classic back/forward 删除 production `GetNavigationHistory` 前置命令、frontend cursor arithmetic、entry URL snapshot 和对应
  helper，直接提交 `Delta(-1/+1)`。Browser Owner 对越界返回 typed `NoSuchHistoryEntry`；Classic frontend 按 WebDriver
  语义把它映射为成功 no-op，BiDi 仍投影 `no such history entry`，没有把协议差异写进 Browser authority；
- 聚焦 Core/Classic command/三组 BiDi/Classic integration 共 `7/7`（run
  `4c706769-aaa6-45b3-a83d-5b14ebd43baa`）；exact history integration 首轮 `4/4`（run
  `4f04ff34-2175-4bf1-9408-20892f63d281`）后按 race-prone async 验证纪律执行 `20` 轮 stress，共 `80/80`
  execution，无 flaky（run `e88c24d4-0705-4492-ba8f-ec5605df58e9`）。同一组还覆盖 delta
  cross-Document、delta zero no-op、hash/pushState same-Document、iframe rejection、restored same-Document entry 的 URL fallback
  与 Classic 初始越界 no-op；
- workspace 首轮全量 `16028/16028`、仓库既有 skip `17`（run
  `8d3fded3-5b65-4dca-ba7e-b8201f04d2ec`）。本切片没有 sleep、retry、timeout 放宽或 assertion 弱化；也没有为了清理一个
  wrapper 新建状态机，改动只关闭 direct history 的 frontend authority；
- `cargo fmt --all --check`、`git diff --check`、workspace all-target clippy `-D warnings` 与 workspace release build 均通过；
  `target/release/moli` SHA-256 为
  `1e80df5dc9ed48e773ff258d88b9044774d9af876d49f2ca30cfcad0a1b7fb40`。显式清除大小写
  HTTP/HTTPS/ALL/FTP proxy 与原 no-proxy、设置 `NO_PROXY=*` / `no_proxy=*` 并固定该 release binary 后，CDP 默认 smoke 与
  WebDriver Classic/BiDi/Selenium/Semantics `--continue-on-failure` 两套 JSON 均为 `ok=true`、失败列表为空；
- 本切片不宣称 Phase 4 完成。下一步应在 popup/auxiliary Target 与 Target termination 中选择一个真正的 Browser-owner
  action，或者先为 `wait:none` 设计单一 neutral outcome ingress；不能继续以“Protocol 中还有 await”为理由清理与 Browser
  progress 无关的局部 frontend wait。

2026-08-04 第五十八切片实现记录（Page termination Browser Owner admission）：

- Core `BrowserOwnerInput` 新增 `PageTermination(BrowserPageTerminationInput)`；payload 只 move-own Phase 2 已建立的 exact
  `BrowserTargetTerminationRequest`，没有 command id、session id、frontend lifetime、subscription 或 response route。queued
  close/crash 因此在 actor selection 后仍按 captured Page generation 重验，replacement 后只能 stale-drop，不能通过当前 session
  lookup 关闭 successor Page；
- `ProtocolSchedulerWork` 把旧的混合 termination variant 拆成 `PageTargetTerminationAdmission` 与
  `TargetCloseOwnerAction`。前者是 renderer-output predecessor 的一次性准入 gate：ready turn 只 publish Core input，Target 在该
  protocol turn 后仍保持 live；真正的 Core commit、physical Page absence 与 crash/close projection 只能由下一次 actor-selected
  Host turn 执行。后者刻意保留显式 `Target.closeTarget` 的现状，因为跨 BrowserContext close 仍可能先 await engine handoff；
- Browser Host participant envelope 从仅支持 `PageCommand` 扩为 typed `PageCommand | PageTermination`。termination start 在一个
  无 await 的短 turn 内完成 Core commit 与 matching physical absence；若存在 retired renderer Page，才 move-own Page 并在
  application completion mailbox 中异步 dispose，完成后投影 Inspector/Target events。没有新增 sleep、retry、poll loop 或
  direct fallback，也没有让 Core 接触 `BackgroundProtocolEvent`；
- stateful `TestContext` 补齐 production mailbox 的独立 wake：protocol admission 新发布的 Browser input 被排成后续 concrete
  `BrowserOwnerTurn`，而不是递归执行。手工 boundary 回归同时证明 admission 后 Target 仍 live、actor turn 才 retirement；另一个
  exact-generation 回归证明 queued old-Page close 在 replacement 后无事件 stale-drop。聚焦 `4/4`（run
  `ca74fe93-774a-471f-ba7c-570224b0c2c8`），同组 `20/20` stress、共 `80/80` execution（run
  `da413dcd-d6ed-4ff0-a756-6932992075c8`），全体 crash/close 相关 Protocol 回归 `116/116`（run
  `c420ee18-f725-4eff-8b7c-e62a2769e317`）；
- workspace 全量 `16029/16029`、仓库既有 skip `17`（run
  `72bbc89f-984d-469f-ae0f-75f37f340758`）；`cargo fmt --all --check`、`git diff --check`、workspace
  all-target clippy `-D warnings` 与 workspace release build 均通过。`target/release/moli` SHA-256 为
  `7e8f6f97698f6082f74e45192937345c94d741cd854fdec76f70f0da89e15c57`。显式清除大小写
  HTTP/HTTPS/ALL/FTP proxy、设置 `NO_PROXY=*` / `no_proxy=*` 并固定该 release binary 后，CDP 默认 smoke
  连续两次均为 `ok=true`，计数复核 `244/244`；WebDriver Classic/BiDi/Selenium/Semantics
  `--continue-on-failure` 连续两次均为 `ok=true`，计数复核 `157/157`；
- 本切片不宣称 termination 全部迁移，更不宣称 Phase 4 完成。下一刀应单独设计显式 `Target.closeTarget` 的 context-selection/
  engine-handoff participant，或迁移 popup/auxiliary Target；不能把 Page termination 已汇流外推为 BrowserContext disposal、worker
  Target close 或所有 termination 都已离开 Protocol。

2026-08-04 第五十九切片实现记录（top-level `Target.closeTarget` Browser Owner admission）：

- Core `BrowserOwnerInput` 新增独立的 `TargetTermination(BrowserTargetTerminationInput)`，继续只 move-own exact
  `BrowserTargetTerminationRequest`。显式 close 的 command/session/result 与 Target/Inspector event projection 留在 frontend；
  mailbox input 不包含 CDP、BiDi、Classic identity，也不允许 Protocol 构造 raw Host turn；
- ordinary `Target.closeTarget` 在 pending fetch/inspector cleanup 后只 publish Host input。frontend response 可以先投影，真正的
  Core termination、physical Target/Page absence 与 retained background Target promotion 只能由 actor-selected Host turn 执行；
  未安装或已停止 Browser Host 时返回 typed Internal error，不再记录错误后仍谎报 `success: true`。若 cleanup 先产生 exact
  renderer predecessor，则 `TargetCloseAdmission` 只负责在该 cursor 之后 publish 同一个 Core input，不再拥有 execution path；
- Browser Host participant envelope 增加与 Page termination 分离的 `TargetClose` variant。start turn 同步完成目标
  BrowserContext selection、Core termination commit 与 matching physical projection；retired renderer Page disposal、promoted
  Page 的 header/network/script/fetch/surface state synchronization 分别由 move-owned participant 承担，完成值带 exact Page
  residence，不能按后来 frontend session 或“当前 Page”重新发现 owner。旧的 async helper只作为尚未迁移的 context-disposal/test
  adapter drain 同一状态机，不再包含第二套 termination 实现；
- 临时 BrowserContext selection 不跨 participant wait：start turn 在返回 retired-Page participant 前先恢复原 selection；后续
  completion 若要 promotion，只在一个同步 owner apply turn 内临时选择目标 context、启动下一 participant，并立即恢复该 turn
  开始时观察到的 selection。若等待期间真实 owner turn 已从 A 切到 C，旧 close completion 最终仍保持 C；promoted Page 的 exact
  completion 也只有在其 context 仍 selected 时才刷新全局 active loader，不能让 inactive-context completion 重置新 owner；
- 模块继续按责任拆分：Core input/admission 位于 `browser_host/owner_input.rs`，Host turn/participant composition 位于
  `conn/browser_host_turn_executor.rs`，Target close transaction 位于 `conn/browser_target_termination.rs`，retained Target engine
  handoff 位于 `conn/browser_target_engine_handoff.rs`，CDP/automation event completion 留在
  `domains/page/termination.rs`。没有把这些职责重新堆进 Target command dispatcher；
- exact 回归覆盖 mailbox selection 前 Target 保持 live、replacement generation stale-drop、participant 不持有临时 context
  selection、Host 缺失 typed rejection，以及 response 在前、Inspector/Target detach event 在后且 typed sidecar 不丢失。
  Target close 最终源码聚焦 `41/41`（run `6e137243-2ce8-4727-af31-730691bda9e0`）；其中 mailbox selection、context
  selection 与无 legacy fallback 三项按 race-prone async 纪律执行 `20` 轮，共 `60/60` execution，无 flaky（run
  `50b8af91-1c33-41f6-843a-619d419f765f`）。Protocol 全包 `3362/3362`（run
  `49f98e44-bbfe-4297-856a-fe169c58aa02`）；
- workspace 最终全量 `16033/16033`、17 个仓库既有 skip（run
  `657cd14f-1d06-419c-b863-eca2a605de41`）；`cargo fmt --all --check`、`git diff --check`、workspace all-target
  clippy `-D warnings` 与 workspace release build 均通过。固定 release
  `target/release/moli` SHA-256 为 `3855f219fb259aea4e1cfe95be783c6d5baf92472f1c7b0581ae9698c1427854`；
  显式清除大小写 HTTP/HTTPS/ALL/FTP proxy、清除 inherited smoke group、设置 `NO_PROXY=*` / `no_proxy=*` 后，CDP
  默认全组 `244/244`、WebDriver Classic/BiDi/Selenium/Semantics `--continue-on-failure` `157/157`，两套 JSON 均为
  `ok=true`、失败列表为空；
- 本切片不宣称 Phase 4 完成。`ProtocolSchedulerWork::TargetCloseAdmission` 的 renderer-predecessor gate、popup/auxiliary
  navigation、direct `wait:none`、Fetch continue 与 BrowserContext disposal 仍是明确迁移边界；下一刀应在这些真正的
  Browser-owner action 中选择，不回头按 Protocol `await` 数量雕花。

2026-08-04 第六十切片实现记录（popup/auxiliary Target navigation Browser Owner cutover）：

- Core 新增独立模块 `browser_host/auxiliary_navigation.rs`，定义 protocol-neutral
  `BrowserAuxiliaryNavigationInput` 与 `BrowserAuxiliaryNavigationKind`；`RendererBrowserIntent::AuxiliaryNavigation`
  只携带 captured Page-slot authority、URL 与 `InitialDocument | NamedTargetReuse` browser kind，不包含
  `CommandOwnerScope`、session、CDP event、opener projection 或 socket lifetime；
- 两类 authority 被明确分开。`InitialDocument` 只为替换 exact bootstrap Document，actor selection 时必须仍匹配原
  Page generation，并再次确认 initial navigation 仍需要；`NamedTargetReuse` 是已接受给同一 browsing context/Target
  的新 navigation request，允许该 Target 自己的上一轮导航推进 generation，但必须匹配稳定 Page-slot instance。
  Core 新增 same-slot resolution：它可跨 generation，却会拒绝删除后以同一 public targetId 重建的新 Target/Page slot；
- Protocol publication adapter 在 popup target 创建或 named-target resolution 已完成后直接 publish Browser Host
  mailbox。Host turn 才选择 exact input、建立既有 `PageCommandTaskStep`，network/renderer/configure/commit 等等待继续
  作为 move-owned `PendingBrowserHostTurn` participant 返回 application owner loop。initial navigation terminal 后的
  `Target.targetInfoChanged` 仍由 Target projector 生成，并用同一稳定 Page-slot instance 防止事件进入同 id 新 Target；
- 删除 `conn/popup_navigation_work.rs`、`PopupTargetNavigationOwnerAction`、对应 `ProtocolSchedulerWorkKind/payload/ready`
  variant、publish method、protocol-output async completion 和仅供该旧路径使用的测试 scheduler 停驻分支。Popup
  navigation 不再持有 Protocol `CommandOwnerScope`，也不存在 Host 缺失时的 direct/scheduler execution fallback；
- 扩大 popup/window-open/auxiliary 相关矩阵首轮在 `51/52` 暴露确定性语义回归：同一 JS command 对同一 named Target
  连续接受 first/second 两次 navigation 时，若把 named action 错绑 Page generation，first commit 后 second 会被 stale-drop。
  这证明 named action 的正确 authority 是 exact Target/Page slot 而不是 exact Document generation；修正后 Core payload、
  stable-slot、initial stale-drop 与全部相关矩阵 `54/54` 通过（run
  `1e0593f7-77d7-4f92-852e-a1a404c03146`）。同-command named reuse、initial generation stale-drop、background named
  reuse 三项执行 `20` 轮、共 `60/60` execution，无 flaky（run
  `bf46feae-1cb1-4f50-a7de-894f4e2d6936`）；
- Protocol 全包 `3363/3363`（run `f25bd6ec-86c6-4ecb-8fac-cae74e154894`）；workspace 全量
  `16036/16036`、17 个仓库既有 skip（run `831088e7-2df1-49d3-94c0-e777c51e94d4`）。`cargo fmt --all --check`、
  `git diff --check`、workspace all-target clippy `-D warnings` 与 workspace release build 均通过；固定 release
  `target/release/moli` SHA-256 为 `25c122cb344024e35a4a49b236e478c8e0ccb7f1cf626412201bbe11d7783092`；
- 显式清除大小写 HTTP/HTTPS/ALL/FTP proxy、原 no-proxy 与 inherited smoke group，设置 `NO_PROXY=*` /
  `no_proxy=*` 后，CDP 默认全组连续两次均为 `ok=true`，计数复核 `244/244`；WebDriver
  Classic/BiDi/Selenium/Semantics `--continue-on-failure` 连续两次均为 `ok=true`，计数复核 `157/157`；
- 本切片不宣称 Phase 4 完成。`TargetCloseAdmission`/Page termination 的 paused-fetch renderer predecessor 仍暂住
  Protocol，direct `wait:none`、Fetch continue 和 BrowserContext disposal 也尚未全部汇入 owner lane；下一刀应围绕这些
  Exit gate payload/owner action，而不是继续清理只影响 frontend 自身延迟的局部 `await`。

2026-08-04 第六十一切片实现记录（termination predecessor admission cutover）：

- Page.crash/Page.close 与显式 `Target.closeTarget` 在完成 pending-fetch cancellation 后都直接 publish 已捕获的 exact
  `BrowserOwnerInput::{PageTermination, TargetTermination}`。删除 `PageTargetTerminationOwnerAction`、
  `publish_page_target_termination_owner_action`、`publish_target_close_admission` 以及
  `ProtocolSchedulerWorkKind/payload/ready` 中两种 termination admission；Protocol residence 不再 move-own、延迟或二次发布
  Browser termination input；
- ordering 不靠新的 queue 或 sleep。若 cancellation 产生 `RendererOutputFence`，command context 继续持有该 exact fence；同一
  application owner turn 返回 command completion 后，production `flush_completed_command_output` 必须先把 fence 对应的 renderer
  publication 送过 ordered ingress，才能刷新 response 并回到外层选择已独立唤醒的 Browser Host turn。Host input publication
  本身不执行 termination，actor selection 前 Target 保持 live；没有让 Browser Host 读取 client-turn predecessor；
- Host 未安装或已停止时不再先回成功后日志丢弃。Page.close/Page.crash 投影
  `-32000 BrowserHostPageTerminationAdmissionFailed`，Target.closeTarget 投影 typed Internal
  `BrowserHostTargetCloseAdmissionFailed`；已经生成的 pending-fetch terminal events 与 renderer predecessor 仍保留，且 rejected
  admission 不修改 Target authority；
- exact Page direct-admission、Page Host rejection、Target 带 predecessor direct-admission/Host rejection、既有 Target ordinary
  Host rejection 共 `5/5`（run `4cf74845-92ed-4702-81ce-90a7ec251066`）；Page direct boundary、Target predecessor
  direct boundary、Page/Target paused request-stage cancellation 四项执行 `20` 轮，共 `80/80` execution，无 flaky（run
  `f2e3f298-21d6-44ab-a0f4-354f5e808909`）；扩大 crash/close/termination 矩阵 `128/128`（run
  `5d63b463-e506-4747-95ab-ec8d989a8d0a`）；
- `moli-protocol` 全量 nextest `3366/3366`（run `84ef4a69-74bb-467a-bba3-fd09c94cbae1`），workspace
  全量 nextest `16039/16039`、既有 skip `17`（run `97e32aba-bc28-4c47-b42c-2b0aa12270ae`）；
  `cargo check --workspace --all-targets`、`cargo fmt --all --check`、`git diff --check`、
  `cargo clippy --workspace --all-targets -- -D warnings` 与 `cargo build --workspace --release` 均通过。release
  `moli` SHA-256 为 `e1533c29cce224596abdb0b25c82c98ac04c0abdc0a4557eb630e994105d5e1a`；清理代理后连续两轮
  CDP smoke 均为 `244/244`，连续两轮 WebDriver Classic/BiDi/Selenium/semantics smoke 均为 `157/157`；
- 本切片通过了 Phase 4 的“Protocol scheduler 不再包含 termination browser-owner payload”子 gate，但不宣称 Phase 4 完成。
  direct `wait:none`、Fetch continue、BrowserContext disposal 以及 download/stopLoading/background-target 边界仍需单独 audit，
  不能把 termination admission 清零外推为所有 top-level owner action 已汇流。

2026-08-04 第六十二切片实现记录（direct `wait:none` navigation Owner admission cutover）：

- `navigation_owner_adapter` 删除按 wait policy 把顶层 `Navigate`、`Reload`、`TraverseHistory` 退回 Page direct execution
  的分支。三类命令现在都先冻结 exact Page residence 并 publish 既有
  `BrowserFrontendCommand::{Navigate, Reload, TraverseHistory}`；current URL、same/cross-Document 分类、history cursor 与
  network start 仍只在 actor-selected Host turn 内解析。child-frame navigation 继续属于 Page/renderer owner，没有为了
  “所有 navigation 都进顶层 queue”而改变责任边界；
- `wait:none` 仍保留自己的 response policy，而不是伪装成 DCL/load wait。对成功进入 detached cross-Document load 的
  `CompleteImmediate` start，Host 用 exact Page Target 与 action kind 构造 protocol-neutral accepted outcome；typed frontend
  在这个 start boundary 返回，commit、DCL、load 与 replacement 作为后续 Browser progress 独立发生。Noop、same-Document、
  Fetch pause 与 start error 仍使用各自 exact plan/outcome；实现没有解析 Protocol JSON 来反推 Browser identity；
- direct admission 会清除迁移期 `background_command_id`，因为 typed result 已由 exact Host reply correlation 返回。detached
  load 仍发布 Browser effects，但其 terminal completion 不再携带旧 command id，不能在 background output FIFO 中制造第二个
  response。response-free projection 显式保留 renderer fence、Network/Page event 与 post-response segment，不把“无第二个
  wire response”误写成“无 Browser effects”；
- 聚焦回归覆盖三类命令在 actor selection 前均不执行、Navigate/Reload/History 的 typed result、detached Navigate 在 commit
  前即可返回，以及 background completion 不保留第二个 command correlation。最终聚焦 `3/3`（run
  `cf87ed2f-acb8-46fb-bc0c-396627e052cf`）；两条 race-prone async 边界各执行 `20` 轮，共 `40/40` execution，无 flaky
  （run `80dc0d78-5238-4461-8577-26590eee3217`）。最终 Page navigation 扩大矩阵 `84/84`（run
  `8a65450a-636f-4ee8-a429-5f9903302510`）；raw CDP background handoff、BiDi `wait:none` 返回/后续 command/preload
  与 Classic page-load strategy none 的 application transport 矩阵 `5/5`（run
  `d0ec73f8-038f-4f5b-9c57-932ce7c4f6c2`），同五项各执行 `20` 轮、共 `100/100` execution，无 flaky
  （run `60f86583-574e-44e6-bb03-8cdc9fa5c098`）；
- `moli-protocol` 全量 nextest `3369/3369`（run `3b02a333-f8e5-4435-abfd-f60f73af15eb`），workspace
  全量 nextest `16042/16042`、既有 skip `17`（run `dd67f431-3c71-49c6-99f9-6dd0d7b44541`）。
  `cargo check --workspace --all-targets`、`cargo fmt --all --check`、`git diff --check`、
  `cargo clippy --workspace --all-targets -- -D warnings` 与 `cargo build --workspace --release` 均通过；release
  `moli` SHA-256 为 `8fc67a51e1230e774ec0534632a359ffd919bd425ae336c5fa18ce5027bf0ab9`。显式清除
  大小写 HTTP/HTTPS/ALL/FTP proxy、原 no-proxy 与 inherited smoke group，设置 `NO_PROXY=*` / `no_proxy=*` 后，默认
  CDP smoke 连续两轮均为 `244/244`、`ok=true`，WebDriver Classic/BiDi/Selenium/Semantics
  `--continue-on-failure` 连续两轮均为 `157/157`、`ok=true`；
- 本切片不宣称 Phase 4 完成，也不把 accepted outcome 当成 DCL/load fact。detached load 的 completion、lifecycle 与 Network
  output 仍通过既有 background navigation completion/output transport 回到 application owner loop；Fetch continue、
  BrowserContext disposal、download/stopLoading/background-target audit 也仍未完成。下一刀应优先关闭这些 owner action 或
  建立 Phase 5 neutral fact/outcome channel，不能继续按 Protocol 中局部 `await` 的数量雕花。

2026-08-04 第六十三切片实现记录（`Page.stopLoading` Browser Owner admission cutover）：

- Core 新增 protocol-neutral `BrowserFrontendCommand::StopLoading`。input 只携带 opaque `BrowserCommandId` 与 exact
  `PageResidenceIdentity`，不包含 CDP request/session、wire payload、domain subscription 或 output queue。其 hidden slot
  instance capability 防止已删除 Target 的同 id 重建被误命中；admission 时的 generation 只是基线，Host FIFO 真正选中 turn
  后才以 same-slot authority 解析当时的当前 Document。这使排在 replacement 后面的 stop 命令作用于 successor，而不是冻结
  frontend 解析时偶然可见的旧 Document；
- Browser Host 选中后以 target-owner route 启动真实 `StopDocumentLifecycle` Page command，并把 pending renderer wait
  move-own 到 `PendingBrowserHostTurn::StopLoading`。`Page` 的 stop API 拆成 start/finish 两段，finish 会保留 exact
  `RendererOutputFence`；等待期间不借用 actor 或 `CdpConnection`。completion apply 前重新验证 exact generation，replacement
  后的旧 renderer completion 直接成功 stale-drop，不能刷新 successor Page state，也不能消费 successor 的 Fetch bucket；
- frontend 原有 `PendingPageCommandKind::StopLoading` fake pending、`complete_stop_loading_command_dispatch` 和临时切换/恢复
  active BrowserContext 的 direct path 已删除。Host 未安装或停止会返回 typed
  `BrowserHostStopLoadingAdmissionFailed`，不会重建 Protocol scheduler/direct fallback。live frontend 只投影 response；若它在
  selection 前后丢弃 wait，Host 仍完成 action，并只结算 Browser-visible events/fence，不产生迟到 wire response；
- Chromium ownership 证据与这个边界一致。在本地 Chromium source commit
  `a03603fe9af6230a12f1b2fb2c18a7d003a0d937` 中，browser-side
  `content/browser/devtools/protocol/page_handler.cc::PageHandler::StopLoading` 调用
  `WebContentsImpl::Stop`；后者遍历 `FrameTree::StopLoading`，`FrameTreeNode::StopLoading` 以 `ERR_ABORTED` 取消已启动的
  `NavigationRequest`，并由 `RenderFrameHostManager::Stop` 向 current/speculative renderer 发 stop。用本地 Chromium
  `147.0.7709.0` 的 browser-session CDP probe 验证：request-stage `Fetch.requestPaused` 的主文档导航收到
  `Page.stopLoading -> {}` 后产生 `Network.loadingFailed(net::ERR_ABORTED)`，紧接着的 successor navigation 正常提交；
  另一个已进入 provisional inactive RFH 的 slow-response probe 返回 `-32000 Not attached to an active page`，说明 Chromium
  还存在 RFH-active 的精确 error 语义。Moli 本切片只迁移既有 stop/cancel 行为的 owner，不在缺少同构 RFH 状态时伪造
  该错误；它作为后续兼容性差异保留；
- 最终 stop-loading 聚焦矩阵 `12/12`（run `18268442-d894-45cb-94d3-efb38b49c256`）；current-generation、stale completion
  与 frontend-drop 三条跨等待边界各执行 `20` 轮，共 `60/60` execution，无 flaky（run
  `f78d8e23-6627-48b0-932d-2bc124b6c21e`）。Page navigation/stop-loading/browser-owner-input 扩大矩阵 `93/93`
  （run `ed30532c-0150-4c81-940a-8a84be71bd5b`）；
- `moli-protocol` 全量 nextest `3373/3373`（run `87874237-2ed3-4aa5-8d9e-b356e47e731b`），workspace
  全量 nextest `16047/16047`、既有 skip `17`（run `1294c3d1-3b0c-42c7-80a6-92cf4ab6d7ad`）。
  `cargo check -p moli-core -p moli-protocol --all-targets`、`cargo fmt --all --check`、`git diff --check`、
  workspace all-target clippy `-D warnings` 与 workspace release build 均通过；release `moli` SHA-256 为
  `62a9d2146d5b86b3bb064b0651fb8ca1f6cb550d65ee2b83f9f8496a2adc53b8`。显式清除大小写
  HTTP/HTTPS/ALL/FTP proxy、原 no-proxy 与 inherited smoke group，并设置 `NO_PROXY=*` / `no_proxy=*` 后，默认 CDP smoke
  连续两轮均为 `244/244`、`ok=true`，WebDriver Classic/BiDi/Selenium/Semantics `--continue-on-failure` 连续两轮均为
  `157/157`、`ok=true`；
- 本切片不宣称 Phase 4 完成。stop action 的 admission/selection/renderer stop authority 已唯一，但
  `fail_pending_fetch_state_background_events_async` 仍在 Host completion apply 中 inline 等 paused-navigation/subresource
  cancellation materialization；Fetch continue、BrowserContext disposal、download/background-target 与 detached navigation
  completion 也仍在清单中。下一刀应从这些真实 owner/progress 边界或 Phase 5 neutral fact/outcome channel 中选择，不继续按
  frontend 局部 `await` 数量雕花。

2026-08-04 第六十四切片实现记录（`Page.stopLoading` paused-Fetch participant chain）：

- 新增独立 `domains/page/fetch_cancellation.rs`，把 pending main navigation、auth navigation、response-stage navigation 以及
  subresource request/auth/response 的取消收敛为一个 move-owned 状态机。它保持既有分组 FIFO 顺序，每次最多启动一个真实
  renderer/navigation participant；`wait()` 消费 exact capability，completion 回到 Browser Host mailbox 后才同步 apply 并启动
  下一项，Host completion 不再一次 inline drain 整个 cancellation batch；
- stop-loading task 现在显式区分 `RendererStop` 与 `FetchCancellation` 两个 phase。renderer stop 完成后先按第63切片冻结的
  `TargetPageResidenceIdentity` 重验当前 Document，再从该 owner route 取走 paused Fetch state；每个后续 subresource
  participant 又冻结其自己的 installed Page identity，并在 finish 前再次重验。若中途 replacement，旧 completion 被 stale-drop，
  successor Page 新注册的 Fetch work 保持 pending；
- main-document cancellation 复用既有 `CompletedNavigateCommand::materialized` 与
  `complete_pending_navigate_command`，因此 navigation failure 后可能产生的 replay/tail 也继续作为 Host 可见 participant，而不是
  被新 helper 隐藏。原 `navigate_session_id` 在 action start 前从 navigation state 取出，只作为 command response/event 的
  projection destination；执行 route 由 exact Page owner 决定，不能通过同 id 的 frontend session 重新选择 Page；
- renderer stop 与全部 cancellation 产生的 `RendererOutputFence` 按同一 stream tail 合并，只有 participant chain terminal 后才
  生成一次成功 response plan。frontend 在 selection 前后断开只丢弃 response projection，不取消 chain。该状态机目前仍携带
  `BackgroundProtocolEvent` sidecar，因此这不是 Phase 5 的 protocol-neutral fact channel；
- termination、target detach 与 BrowserContext disposal 等尚未迁入 Host participant lane 的调用方，继续调用
  `fail_pending_fetch_state_background_events_async`，但该 compatibility helper 已删除旧的六段独立循环，改为本地 drain 同一个
  cancellation 状态机。后续迁移这些 owner 时只需暴露现成 participant boundary，不再复制行为；
- 回归把 current-Document selection 测试扩展为两个 paused subresource，显式观察 renderer-stop 后连续两个 Host
  participant，并新增 replacement 恰好发生在 subresource cancellation wait 中的 stale-apply 用例。stop-loading 聚焦矩阵 `13/13`（run
  `e2e605e9-18f5-42e8-b7af-5c20969e9f22`）；current generation、replacement stale-drop 与 frontend-drop 三条边界各执行
  `20` 轮，共 `60/60` execution，无 flaky（run `f7aa6845-9dd0-465c-bc34-4123170d40e8`）；termination/context-disposal/
  Fetch-disable compatibility 扩大矩阵 `35/35`（run `7347db5a-67e8-4288-b7ea-1dbbbb1694ad`）；
- `moli-protocol` 全量 nextest `3374/3374`（run `6e690de4-f08a-4d3f-bb17-b77ab407ae36`），最终源码树的 workspace
  全量 nextest `16048/16048`、既有 skip `17`（run `0e79bcf4-4a9d-4b11-afde-632b680e4016`）。`cargo fmt --all --check`、
  `git diff --check`、workspace all-target clippy
  `-D warnings` 与 workspace release build 均通过；release `moli` SHA-256 为
  `b3a474c42644f2ae1bbfe49315bcef1c11c9b03735c10f86cb354537ff6030c6`。显式清除大小写 HTTP/HTTPS/ALL/FTP
  proxy、原 no-proxy 与 inherited smoke group，并设置 `NO_PROXY=*` / `no_proxy=*` 后，CDP 默认全组连续两轮均为
  `244/244`、`ok=true`，WebDriver Classic/BiDi/Selenium/Semantics `--continue-on-failure` 连续两轮均为
  `157/157`、`ok=true`、失败列表为空；实现未加入 sleep、retry、`yield_now` 或第二条 owner queue；
- 本切片只关闭 stop-loading 的隐藏 renderer wait，不宣称 Fetch continue、BrowserContext disposal、detached navigation
  completion、download/background-target、neutral fact channel 或独立 Host lifetime 已完成。下一刀应优先选择这些仍会改变
  Browser Owner progress/lifetime 的真实边界，不能因为共享状态机已经存在就把所有 compatibility caller 一次性塞进同一提交。

2026-08-04 第六十五切片实现记录（raw CDP request-stage `Fetch.failRequest` owner decision cutover）：

- Core 新增 `BrowserFrontendCommand::ResolvePausedNavigation` 和
  `BrowserPausedNavigationDecision::Fail`。input 只携带 opaque `BrowserCommandId`、exact `PageResidenceIdentity` 与 failure text，
  不携带 Fetch request、CDP id/session、wire response、domain subscription 或 output queue；这为后续 continue/auth/fulfill
  共用同一 paused-navigation decision 边界，但本切片只实现无网络 I/O 的 terminal fail；
- Protocol prepared sidecar move-own `PendingFetchNavigation`、detached `CommandDispatchContext` 与 reply sender。raw CDP
  `Fetch.failRequest` 取走 request 后先按 pending navigation 的 Target route 捕获 exact Page，再发布 owner input。Host 未安装或
  mailbox 已停止时 sidecar 会把 request 交还原 exact owner bucket并返回 typed admission error；禁止内联执行旧 completion；
- 新增独立 `domains/fetch/navigation_decision.rs`。由于 `BrowserHostTurnExecutor` 是同步 selection seam，首个 participant 只做
  selection 到 async apply lane 的 move-owned handoff；apply turn 再次验证 exact generation、切到无 session 的 owner route，
  使用既有 `MaterializedNavigationCompletion` / `complete_pending_navigate_command` 执行失败与后续 renderer replay/tail。每个真实
  tail wait 继续作为独立 Host participant 返回，等待时不借用 `CdpConnection`；
- 原 `navigate_id` / `navigate_session_id` 在执行前从 navigation state 取出，只在 terminal 时把 navigation plan 投影为原
  `Page.navigate` 的 background response/event；outer `Fetch.failRequest -> {}` 仍保持既有先后顺序。若 selection 后 Page 被
  replacement，old generation 只生成 `Navigation aborted` 给原 navigation，不发 `Network.loadingFailed`、不 discard successor
  physical Page。若 frontend completion receiver 已丢失，Host 仍结算原 navigation 的 failure/event，只删除迟到的 Fetch response；
- typed BiDi/Classic `DevToolsCommand::FailInterceptedRequest` 仍在单个借用 `CdpConnection` 的 async frontend future 内。若在该
  形状中 publish 后原地等待 Host reply，反而会阻止 Host 取得 physical executor，因此它明确保留 compatibility completion；
  response-stage `PausedDocumentTransfer::fail` 也未纳入本切片。迁移它们必须先暴露 scheduler-visible frontend task，不能把
  “所有 Fetch await 清零”当作目标；
- 回归覆盖 owner admission/participant 顺序、replacement generation stale-drop、frontend wait loss，以及 Host stopped 时 request
  恢复且无 direct fallback。Core neutral input 聚焦 `1/1`（run `9a8f15f9-7ea2-4853-a9e8-ea3076730b2e`），最终
  Fetch fail 矩阵 `15/15`（run `c72f3b45-ae95-401e-8ebf-e5cbb8298db6`）；current owner、replacement stale-drop
  与 frontend-loss 三条边界各执行 `20` 轮，共 `60/60` execution，无 flaky（run
  `1a7fb8a8-30b5-4e67-bda5-e5003479010e`）。Fetch fail/disable、stop-loading、termination 与 context-disposal 扩大兼容
  矩阵 `57/57`（run `559d66fa-1810-4479-8d7f-5faeb15abe61`）；publication-failure 大载荷装箱后的 request 恢复聚焦
  `1/1`（run `17f3e3c0-6ca0-4d1e-bc77-5472bf6b8946`）；
- `moli-protocol` 全量 nextest `3378/3378`（run `8cab79af-f471-46fc-967c-ab42a321297a`），workspace
  最终源码树全量 nextest `16053/16053`、既有 skip `17`（run `b0b294c2-69f0-4d89-99df-16a47ec66c41`）。
  `cargo fmt --all --check`、`git diff --check`、workspace all-target clippy `-D warnings` 与 workspace release build 均通过；
  release `moli` SHA-256 为 `5b45b68decdf49405229ee8ec1cae6ac361f8c547e08088fbb349152149161f8`。显式清除
  大小写 HTTP/HTTPS/ALL/FTP proxy、原 no-proxy 与 inherited smoke group，并设置 `NO_PROXY=*` / `no_proxy=*` 后，CDP
  默认全组连续两轮均为 `244/244`、`ok=true`，WebDriver Classic/BiDi/Selenium/Semantics
  `--continue-on-failure` 连续两轮均为 `157/157`、`ok=true`、失败列表为空；实现未加入 sleep、retry、`yield_now`
  或第二条 owner queue；
- 本切片仍携带 Protocol `CommandOutputPlan` / `BackgroundProtocolEvent` projection sidecar，不是 Phase 5 fact channel，也不宣称
  Fetch continue、auth/fulfill、BrowserContext disposal、detached navigation completion、download/background-target 或独立 Host
  lifetime 已完成。下一刀应优先设计 request/auth/response continue 的 move-owned network job，或进入 neutral fact/outcome
  channel；不能把 response-stage streaming 与 context disposal 强塞进本提交。

2026-08-04 第六十六切片实现记录（raw/nested CDP request-stage `Fetch.continueRequest` owner cutover）：

- Core 的 protocol-neutral `BrowserPausedNavigationDecision` 新增 `Continue`，只保存解析后的 URL、method/body/header override 与
  response-stage intent；它不携带 Fetch request id、CDP session、wire response 或 output queue。raw/nested CDP frontend 取走
  paused main-Document request 后只发布 exact Page-scoped decision，真正 mutation 发生在 actor-selected Host apply turn。Host 未安装/
  已停止时 sidecar 把完全未修改的 request 恢复到原 owner bucket，不允许退回 inline frontend execution；
- 新增独立 `domains/fetch/navigation_resume.rs`。普通无 interception load 直接复用 `BackgroundNavigationLoadJob` 与既有
  `PendingNavigateCommand`；intercepted request 的 streaming fetch、auth challenge stream collection、buffered/streaming response
  Document build、response-stage prepared Document 各自是 move-owned participant。每次 completion apply 前都由外层
  `navigation_decision` 重新验证 frozen `PageResidenceIdentity`，等待期间不借用 `CdpConnection`，也没有新增第二条 owner queue；
- `runtime_load` 把上述网络/renderer wait 拆成只持有 frozen load inputs、resource client、NavigationEngine 与 exact future-Page
  reservation 的 job。buffered/streaming build 在 `DocumentCommit` boundary 返回，再进入共享 navigation configure/commit/
  replacement/tail 状态机；401/407 先作为独立 body collection participant 完成，再同步注册原 authRequired continuation；
  response-stage pause先完成 exact prepared-Document participant，再登记可继续的 streaming body source。旧 inline helper 复用同一
  preparation job，未维护第二套 renderer build 语义；
- 原 `navigate_session_id` 在 Continue 路径继续保留在 paused navigation state，因为 auth/response-stage 后续 command 仍需沿原
  frontend projection route 完成；Browser execution authority 由 exact Page owner 决定，session 只影响最终 response/event
  destination。frontend wait 被丢弃后 Host 仍完成 network/commit，并只丢迟到的 `Fetch.continueRequest` response；replacement
  generation 变旧时在发起下一 participant 前返回原 navigation 的 `Navigation aborted`，不能请求网络或安装 successor Page；
- 真实 Playwright network smoke 暴露了 Host participant completion 与 frontend pending-command completion 之间的 handoff
  窗口：如果 application scheduler 在这个窗口读取 raw renderer stream，`MainDocumentCommit` 会在 physical Page 尚未安装时按旧
  loader stale-drop，随后 DCL/load 可以先于 `Page.navigate` response 到达，最终 `Page.goto(waitUntil=load)` 因缺少
  `frameNavigated` 悬挂。修复由 exact `PendingCdpCommandDispatch` 持有既有
  `NavigationRendererPublicationBuffer` gate，从 owner decision publish 一直覆盖到该 command 的 renderer insertion boundary；Host
  自身继续推进，gate 不依赖 frontend socket flush，也不依赖 Host reply oneshot 与 actor `select!` 的先后。command requeue 时根据下一
  phase 重新计算 permit，terminal completion 在同一 actor turn 先发布 Continue/Page.navigate response prefix，再释放
  `MainDocumentCommit`、`frameNavigated` 与 lifecycle；
- 这里没有把 DCL 判成假的，也没有在 Protocol 增加 pending lifecycle buffer。调试中验证过“先缓存 DCL/load、Page install 后补发”会
  让 load observer 在 command response/commit boundary 前过早完成，因此已删除；正确边界是暂存 raw renderer publication，让既有
  commit/output state machine 按原顺序消费。新增 production-shaped WebSocket 回归同时要求 `Fetch.continueRequest` ACK 和原
  `Page.navigate` loader metadata 先于目标 `Page.frameNavigated`，从而覆盖上述极短 handoff race；
- typed BiDi/Classic `DevToolsCommand::ContinueInterceptedRequest` 仍在借用 `CdpConnection` 的 direct future 内，因此保留原
  compatibility completion；raw `continueWithAuth`、response-stage continue/fulfill/fail 与 BrowserContext disposal 也未被本切片
  顺带迁移。它们需要各自 scheduler-visible task seam 或 permit policy，不能因 request-stage Continue 已迁移就直接 publish 后原地
  等 Host reply；
- owner 专项回归覆盖 exact Page、frontend receiver loss、replacement stale-drop、Host stopped 原样恢复与 renderer publication
  handoff ordering。Core neutral decision、request override/auth/response-stage 与四条 owner/handoff 边界聚焦 `8/8`（run
  `397d6e87-58d7-40e2-b4c7-9afe76668faf`）；WebSocket handoff、frontend-loss、replacement stale-drop 与 response-stage 四条
  race-prone async 边界各执行 `20` 轮，共 `80/80` execution，无 flaky（run
  `896d9d72-1541-4843-a0a8-08ee2bd1351e`）；Fetch navigation/auth/response-stage/subresource 扩大矩阵 `86/86`
  （run `708f8242-d1f2-4932-92d3-ebb63628c162`）；
- `moli-protocol` 全量 nextest `3381/3381`（run `9928e819-a487-4c83-9eb4-4fd46901e4b4`），最终源码树的
  workspace 全量 nextest `16058/16058`、既有 skip `17`（run `da5d277e-a61b-4d94-b192-1a519011611a`）。
  `cargo fmt --all --check`、`git diff --check`、workspace all-target clippy `-D warnings` 与 workspace release build 均通过；release
  `moli` SHA-256 为 `2895aee900ee14364577a230da47ef914a1e232a8b705c52f348857282b93c95`。显式清除大小写
  HTTP/HTTPS/ALL/FTP proxy、原 no-proxy 与 inherited smoke group，并设置 `NO_PROXY=*` / `no_proxy=*` 后，真实 Playwright CDP
  network 聚焦组通过，CDP 默认全组连续两轮均为 `244/244`、`ok=true`；WebDriver Classic/BiDi/Selenium/Semantics
  `--continue-on-failure` 连续两轮均为 `157/157`、`ok=true`、失败列表为空。实现未加入 sleep、retry、`yield_now`、lifecycle
  补发或 fallback drain；

本切片不宣称 Phase 4 完成，也不把 Fetch/Network/Page event projection 误称为 Browser fact channel。下一步应在
typed/auth/response-stage Fetch decision、BrowserContext disposal、detached completion transport 与 Phase 5 neutral fact/outcome
channel之间做 exit audit 后选择主干，不继续以“Protocol 中每个 await 都要删除”为目标。

2026-08-04 第六十七切片实现记录（raw/nested CDP main-Document `Fetch.continueWithAuth` owner cutover）：

- Core 的 protocol-neutral `BrowserPausedNavigationDecision` 新增 `Auth`，只表达 abort navigation、expose challenged response
  或用已经解析的 browser credentials retry。它不携带 Fetch request id、CDP `Default/CancelAuth/ProvideCredentials` wire 名、
  session、401/407 response 或 output queue；Basic/Digest、server/proxy 等 credential 语义复用 browser page 层已有的 typed auth
  capability；
- Protocol paused-navigation sidecar 从单一 `PendingFetchNavigation` 扩成显式 `Request/Auth` variant。raw/nested CDP
  `Fetch.continueWithAuth` 取走 main-Document auth pause 后，先按该 navigation 的 Target route 捕获 exact Page，再把 auth pause、原
  response body/journal 与 projection identity move-own 到 sidecar，只把 exact Page 和 neutral decision 发布给 Core actor。Host
  未安装或 mailbox 已停止时，sidecar 按 current exact route 把同一个 pause 放回 registry；回归以原 auth response `Arc` identity
  证明不是重建近似状态，也没有 direct fallback；
- chained multi-session auth 的 `Default` 若只需把同一个 challenge 投影给下一个 session，仍在 frontend 同步生成下一次
  `authRequired`，因为这一步不推进 browser。unsupported challenge 也继续在 admission 前返回 `NotImplemented` 并恢复 pause。
  typed BiDi/Classic `DevToolsCommand::ContinueWithAuth` 与 response-stage credentials 暂留既有 compatibility future，避免在其
  direct adapter 尚未暴露 scheduler-visible task 时 publish 后原地等待 Host、反而阻住 physical executor；
- `navigation_decision` 新增 exact `AuthApply` phase，并复用第66切片的 navigation resume participant chain。每次 auth apply、
  network completion、Document build、commit/replacement 与 renderer tail 前都重新验证 frozen Page generation；frontend reply
  receiver 被丢弃后 Host 仍继续 retry/commit，只丢 outer auth ACK。replacement 恰好发生在首个 participant wait 中时，旧 action
  在发起 credential HTTP request 前返回原 `Page.navigate` 的 `Navigation aborted`，不发 response/frame facts，也不能进入
  successor Page；
- credential retry 为 `BackgroundInterceptedNavigationFetchJob` 新增 move-owned buffered mode。Digest 继续由 libcurl buffered
  path 完成内部 challenge round，Basic + response-stage interception 继续走 streaming head/body path；先前 401/407 的
  `NetworkObservationJournal` 与 retry journal 按物理顺序拼接。Default failure 仍 materialize navigation error，Cancel 仍暴露原
  challenged response，并保留 HTTPS proxy CONNECT 407 无 ExtraInfo、最终 network failure 的既有特殊语义；没有为了 owner cutover
  维护第二套 auth/load 规则；
- Core neutral input 聚焦 `1/1`（run `a17d5acc-cedc-48c2-a799-28f82041e8f3`）；exact Page、frontend-loss、stale-generation 与
  Host-stopped rollback 三条 owner 回归聚焦 `3/3`（run `e9558ffc-b90c-4d0b-96ba-8597b20295a7`），并用 nextest
  `--stress-count 20` 完成 `60/60` execution、无 flaky（run `06353b0d-aaa5-400d-8ef4-60a9b858c81b`）。ContinueWithAuth/
  navigation-auth 兼容矩阵 `36/36`（run `641b4262-443d-4c0d-8175-3abf1a0c7856`），Fetch navigation 全矩阵
  `89/89`（run `1a1e3082-8713-477f-98da-b0fc13f50e04`），`moli-protocol` 全量 `3384/3384`（run
  `6a6eb314-49af-4ca1-9f08-7f73998e4446`）；
- 最终源码树 workspace nextest `16062/16062`、既有 skip `17`（run
  `9fed3e95-a1ff-41a8-af00-19e1b20c9ff8`）。`cargo fmt --all --check`、`git diff --check`、workspace all-target
  clippy `-D warnings` 与 workspace release build 均通过；release `moli` SHA-256 为
  `325af3a6f09aed544dd0ea20e02877830c103a4e0a5a360f487d52fddc000034`。显式清除大小写 HTTP/HTTPS/ALL/FTP
  proxy、原 no-proxy 与 inherited smoke group，并设置 `NO_PROXY=*` / `no_proxy=*` 后，真实 Playwright CDP network 聚焦组
  `ok=true`，其中 main-Document auth continue/cancel/response-stage 均通过；CDP 默认全组连续两轮均为 `244/244`、
  `ok=true`，WebDriver Classic/BiDi/Selenium/Semantics `--continue-on-failure` 连续两轮均为 `157/157`、`ok=true`、失败
  列表为空。实现未加入 sleep、retry、`yield_now`、lifecycle 补发或 fallback drain。

本切片关闭的是“request-stage owner navigation 到 401/407 后又把 execution authority 交回 raw CDP frontend”的真实断点，
不是继续追求 Protocol 零 `await`。Phase 4 仍未完成；下一步应重新做 exit audit，在 typed/response-stage Fetch decision、
BrowserContext disposal、detached completion transport 与 Phase 5 neutral fact/outcome channel 中选择会实际影响 Browser progress/
lifetime 的主干，不再逐个迁移只影响单个 frontend response latency 的局部 wrapper。

2026-08-04 第六十八切片实现记录（BrowserContext exact instance capability）：

- Phase 4 exit audit 对比了三个候选：detached `wait:none` navigation 的 start/progress 已由 Browser Host 拥有，剩余主要是
  Phase 5/6 completion transport；response-stage Fetch 的局部 wait 只影响该命令时延，不能仅按“Protocol 里还有 await”判定；
  `Target.disposeBrowserContext` 则仍由一个 frontend completion 跨 paused-Fetch cancellation、worker teardown、每个 Page
  termination、Context removal 和 renderer close 借用整个 `CdpConnection`，确实会把 BrowserContext lifetime/progress 绑回
  frontend loop。因此下一条 Phase 4 production cut 明确选择 BrowserContext disposal；
- audit 同时发现不能直接把 `browser_context_id: String` 放入 mailbox。typed
  `DevToolsCreateBrowserContextCommand` 允许 frontend 显式提供 id，Context 删除后可以用同一 public id 重建；排队的旧 disposal
  若在 Host selection 时按字符串 lookup，会删除新实例。Core 因而新增独立模块 `browser_host/context_handle.rs`，以单调
  instance identity + `Arc` capability 区分同名 Context；它只包含 staged/live/retired 生命周期，不携带 session、profile payload、
  Target/Page 或 protocol output；
- physical `BrowserContext` 在构造时创建 staged handle，正式 registration 将 exact handle 与 Context/Target/Page/engine transaction
  一起激活。任何 target/page/engine 验证失败都会回滚 handle activation；removal 则在同一无 await transaction 中先 reserve exact
  Context retirement，与所有 Target retirement/engine handoff 一起 commit，失败时恢复 live。状态原子使用 Acquire/Release 配对，
  cloneable/send capability 不依赖单线程 relaxed ordering；
- Core registry 现按 `{public id -> exact handle}` 保存 instance authority，并提供
  `prepare_browser_context_removal_for_handle`。removal permit 同时冻结 revision、Core 选择的 successor 和 exact handle；因此同名
  Context 的旧 capability 即使 public id 再次存在也返回 typed `BrowserContextHandleProjectionMismatch`，不能重定向。Protocol
  registration/removal 已使用 physical context 携带的 handle，完整 topology projection 也逐 Context 校验 exact capability，不再只
  对比 count/selected/public-id；
- Core 回归覆盖 handle lifecycle/rollback、错误 public id、removal 失败回滚和“删除 A、同 id 重建 B、旧 A capability 不能删除 B”；
  Protocol projection 回归覆盖相同 public id 的错误 physical handle 在 Core mutation 前被拒绝。四条 exact-instance/rollback/
  projection 边界各执行 `20` 轮，共 `80/80` execution，无 flaky（run
  `c46c69a6-5695-4012-acc0-68ea0e608812`）；`moli-core` + `moli-protocol` 全量 nextest
  `5982/5982`、既有 skip `13`（run `eacd9445-c4d8-47aa-bc93-213a4021deec`），最终源码树的 workspace 全量 nextest
  `16068/16068`、既有 skip `17`（run `3d5b3963-58e1-49e2-877b-61d8bb6a3fb2`）；
- `cargo fmt --all --check`、`git diff --check`、workspace all-target clippy `-D warnings` 与 workspace release build 均通过；release
  `moli` SHA-256 为 `721eda8bc3e122d23b65fc862945ebbab83d81198097ba7f09480586183c5e9a`。显式清除大小写
  HTTP/HTTPS/ALL/FTP proxy、原 no-proxy 与 inherited smoke group，并设置 `NO_PROXY=*` / `no_proxy=*` 后，CDP 默认全组两轮均为
  `244/244`、`ok=true`，WebDriver Classic/BiDi/Selenium/Semantics `--continue-on-failure` 为 `157/157`、`ok=true`、失败
  列表为空；
- 该切片没有把 disposal async chain 伪装成已迁移，也没有新增 queue、sleep、retry、yield 或 compatibility drain；下一切片才能以
  这个 exact handle 作为 `BrowserOwnerInput` authority，把 Context logical removal 与每个真实 cleanup participant 接入 Browser Host。

2026-08-04 第六十九切片实现记录（BrowserContext disposal Browser Owner cutover）：

- Core 新增 move-only `BrowserContextDisposalReservation`。Host 选中携带 exact `BrowserContextHandle` 的
  `BrowserFrontendCommand::DisposeBrowserContext` 后才建立 reservation；仅发布命令不会提前改变 Context/Target topology。reservation
  期间 ordinary Context activation、Target registration、Document navigation、Page transition/replacement 都拒绝新 work，已经归
  disposal owner 的 exact Target/Page capability 仍可完成 cleanup。同 public id 重建的 Context 不能满足旧 input 或 reservation；
- disposal 不再为了复用 frontend helper 临时激活 inactive Context，也不再在多次 participant wait 后恢复 admission 时捕获的旧
  selection。每个 Page 使用 exact owner route 和预先捕获的 termination capability；terminal turn 才由 Core 根据当时 authoritative
  topology 选择 successor，并在同一个 transaction 中 retire reservation/Context/remaining Target。关闭 disposal Context 中的 active
  Target 即使临时 promote retained Target，也不执行普通 frontend settings synchronization，因为该 retained Target 紧接着由同一
  disposal chain 关闭；
- 原来隐藏在一个 `&mut CdpConnection` future 内的工作拆成显式 Host participant chain：每个 paused main-Document Fetch cancellation
  的真实 renderer replay、每个 retired Page close，以及 registry 外残留 physical Page close。Fetch cancellation 的无等待前半段改成
  同步 start，只在确有 renderer replay 时返回 participant；SharedWorker/ServiceWorker 已经在 prepare 阶段停止，剩余 exact
  attachment/version retirement 只是 connection-local apply，因此放入独立同步 projector，没有制造 ready future 或伪 async 边界；
- raw CDP `Target.disposeBrowserContext` 现在只解析参数、捕获 exact Context capability、保存 protocol projection sidecar 并 publish 到
  既有 Browser Host mailbox；Host 未安装/停止时返回 admission error，禁止回退到旧 direct execution。typed BiDi/Classic 新增独立
  `browser_context_disposal_owner_adapter`，application scheduler 在等待 command reply 时继续服务 Host/protocol/renderer input；BiDi
  `TargetDestroyed` prefix 仍按既有顺序投影；
- frontend timeout、receiver drop 或 raw pending command drop 只放弃该 frontend reply。已经 accepted 的 Host state machine 持有
  reservation 并继续完成 Context cleanup；terminal protocol effects 通过 detached/background projection 结算。Core input 不携带
  CDP id/session、BiDi context、socket 或 event buffer，frontend 也不能 drive 已接受的 cleanup；
- 新回归覆盖 reservation rollback/新 work rejection、terminal retirement 与 same-id ABA、raw publish-before-selection、queued old
  capability 不删除同名 replacement、frontend reply drop 后 Host 仍删除 Context，以及 typed BiDi prefix/Host selection。首轮 workspace
  gate 还实际抓出两条边界缺口：inactive Context 的 physical Target close 曾错误借用当前 selected Context；迁移期未注册 Context fixture
  又曾被与 `disposing` 相同的布尔判定误拒绝。前者改为按 termination capability 的 exact Context lookup，后者由 Core typed
  `try_start_document_navigation_with_trace` 只拒绝已知 disposing Context，不让 Protocol 猜测 authority；对应 BiDi、navigation fixture、
  inactive disposal 与 Core reservation 聚焦回归均通过；
- 最终源码树 workspace 全量 nextest `16076/16076`、既有 skip `17`（run
  `f17fb928-2291-4130-a098-f85e32108fa1`）；`moli-protocol` BrowserContext 全组 `242/242`、application scheduler
  owner-dispatch `1/1` 也通过。`cargo fmt --all --check`、`git diff --check`、workspace all-target clippy `-D warnings` 与
  `cargo build --workspace --release` 均通过；固定 `target/release/moli` SHA-256 为
  `f5fc8c491e5dd19e4e8e228cdb6d537fb0c6b3bab1721897c3231d022f96dd80`。显式清除大小写 HTTP/HTTPS/ALL/FTP proxy、设置
  `NO_PROXY=*` / `no_proxy=*` 并固定该 binary 后，CDP 默认全组 `244/244`、WebDriver
  Classic/BiDi/Selenium/Semantics `--continue-on-failure` `157/157`；两套 JSON 均为 `ok=true`、失败列表为空；
- direct `CdpConnection` 测试/尚未迁移的嵌入调用仍保留一个 inline compatibility wrapper，但它只 drain 同一套 owner task，不复制
  execution semantics；production raw CDP 与 typed application scheduler 均不经过该 wrapper。本切片迁移的是会影响 Browser lifetime/
  progress 的真实等待，不以删除所有 Protocol `await` 为目标，也不宣称 Phase 4 或 Phase 5 fact channel 已完成。

2026-08-04 第七十切片实现记录（raw/nested CDP response-stage `Fetch.continueResponse` owner cutover）：

- 这一刀迁移的不是“Protocol 里又一个 `await`”，而是 response-stage 主文档 pause 的 release/start authority。此前 raw CDP frontend
  取走 `PausedDocumentTransfer` 后可以直接启动 prepared Document、streaming replay 或 buffered build；现在 Core 的
  `BrowserPausedNavigationDecision::ContinueResponse` 只保存 browser-level status/header override，Protocol sidecar move-own Fetch
  request id、response body、原 navigation projection 与 reply sender。只有 actor-selected exact Page turn 能消费 sidecar 并释放 pause；
- raw/nested CDP 通过 scheduler-visible pending command 发布 decision；Host 未安装/停止或 Page owner 捕获失败时，同一个 response
  transfer 会按 exact current route（否则原 session route）放回 registry，禁止 direct fallback。typed BiDi/Classic
  `DevToolsCommand::ContinueInterceptedResponse` 仍是借用整个 `CdpConnection` 的 direct adapter，暂留既有 compatibility path；在它尚未
  暴露 scheduler task seam 前 publish 后原地等待会阻止 Host 取得 physical executor，因此本切片没有为了“入口一致”制造自我死锁；
- `domains/fetch/navigation_resume.rs` 现在同时拥有 request/auth/response 三类 paused-navigation resume。prepared streaming response
  继续复用原 background body-completion transport，Fetch ACK 不等待 body EOF；non-streaming buffered/captured/streaming response 则变成
  move-owned build participant，job 只持有 frozen load inputs、`NavigationEngine`、future-Page reservation、response head/body 和
  network journal，等待 parser/renderer 时不借用 `CdpConnection`。显式 response override 保留既有 synthetic-head 语义；无 override
  的 download detection、cookie/network metadata 与 prepared renderer-agent identity 继续保留；
- owner outcome 显式区分“decision 被拒绝”和“decision 已接受但 navigation 失败”。`takeResponseBodyAsStream` 已占用 transfer 时，Host
  原样恢复 active stream 并把 `ResponseBodyStreamActive` 返回给当前 `Fetch.continueResponse`；它不能先 ACK Fetch，再把错误错投给原
  `Page.navigate`。相反，已经接受后的 token/replacement failure 仍结算原 navigation。交错 response head 的旧请求现在由
  browser-owned navigation token 在 prepared renderer candidate commit 前统一 stale-drop 为 `Navigation aborted`，不会深入旧
  renderer channel 或安装 successor Page；
- exact Page + frontend receiver loss 回归连续 `20/20` 轮通过（run
  `db14defd-ae4c-4020-aab4-f15c5fe1ed59`）；continue-response 兼容矩阵 `20/20`（run
  `ecb1a4c8-bec0-4f9f-97b6-3c05dd77f062`），完整 response-stage 矩阵 `29/29`（run
  `eed6f73b-8fb5-434f-9fe4-4a0c64538acb`），`moli-protocol` 全量 `3390/3390`（run
  `8afb9970-0521-4fe0-8452-5ecb558e38c2`）；首轮 workspace 全量还准确暴露
  `continue_response_header_override_keeps_streaming_parser_body` 在高并发下会于 renderer load 前读取 partial DOM（run
  `c8d7800a-0dbb-464e-9912-a9f19ea9f12a`），模块压力在第 `6/20` 轮复现为 `1` 次失败（run
  `f5902b7b-9124-422d-a06b-be6e2c310ae0`）。修复没有增加产品 sleep/timeout：测试 server 的 one-shot signal 改成可保留
  permit 的 `notify_one`，最终 DOM 读取改为等待 exact `Target + loaderId` 的 renderer-owned load state；修复后完整
  response-stage 模块连续 `20/20` 轮、共 `580/580` 次通过（run `fc7123f3-e373-418d-825e-2e93151a0291`）；
- 第二次 workspace 全量唯一失败是未改动的 renderer-v8 OPFS structured-clone 用例在全仓并发下仍为 `pending`（run
  `c364f48d-06fc-4227-94ea-acdc635424e6`），该用例独立压力 `50/50` 通过（run
  `229524b9-b702-4115-b7de-12894555671e`），因此没有把无关 renderer 改动混入本切片。随后 pre-rebase 标准 workspace
  nextest `16078/16078`、既有 skip `17` 全部通过（run `31e78cde-f1cb-4dd2-958c-e51e347e2d38`）；本切片提交后执行
  `git pull --rebase origin master`，上游带入 CSS/computed-style、CDP smoke 与 V8 build 等真实 tree 变化，因此对最终 tree
  重新运行 workspace nextest，`16085/16085`、既有 skip `17` 全部通过（run
  `2d4e7b82-da8c-4ee5-85c4-ab37042c98b2`）。最终 tree 的 `cargo fmt --all --check`、`git diff --check`、workspace
  all-target clippy `-D warnings` 与 `cargo build --workspace --release` 均通过；固定 `target/release/moli` SHA-256 为
  `f9c18b23b294c1459f7aac6971c8046bbb047a91ee0633fc634ea6f2e7bd4ceb`。显式清除大小写 HTTP/HTTPS/ALL/FTP proxy、原
  no-proxy 与 inherited smoke group，并设置 `NO_PROXY=*` / `no_proxy=*` 后，最终 CDP 默认全组为 `245/245`、
  `ok=true`；WebDriver Classic/BiDi/Selenium/Semantics `--continue-on-failure` 为 `157/157`、`ok=true`，两套失败列表均为空；
- 本切片没有建立第二条 queue，也没有把 Fetch ACK、Network/Page event 或 DCL/load 冒充 Browser fact。response-stage
  `failRequest`/`fulfillRequest`、typed direct Fetch adapter、detached completion transport、protocol-neutral fact/outcome channel 和独立
  Host lifetime 仍分别属于后续 Phase 4/5/6；下一刀应重新做 exit audit，优先选择仍拥有 browser action/progress 的边界，不以清除
  每个局部 frontend `await` 为目标。

2026-08-04 第七十一切片实现记录（raw/nested CDP main-Document Fetch terminal decision owner cutover）：

- Phase 4 exit audit 确认这一刀仍是 browser action 所有权，不是为了消除局部 `await`：此前 raw CDP
  request/response-stage `Fetch.fulfillRequest` 会在 frontend command future 内直接启动 synthetic Document build，response-stage
  `Fetch.failRequest` 也会直接结算原 navigation。现在三者都发布 exact Page `ResolvePausedNavigation` input，只有
  actor-selected Host turn 能消费 pause 并选择 commit/failure；
- Core 新增 protocol-neutral `BrowserPausedNavigationFulfillDecision`，只保存 HTTP status、header pairs 和可选 bytes。
  Fetch request id、CDP session/command id、paused request/response transfer 与 output route 仍 move-own 在 Protocol sidecar。Host 未安装、
  已停止或 exact Page 捕获失败时恢复同一 sidecar，禁止 direct fallback；selection 后每个 apply/participant turn 继续重验
  `Target + Page generation`，replacement 后的旧 decision/completion 不能进入 successor Page；
- synthetic response 不再跨等待借用 `CdpConnection`。request-stage pause 保留 requested URL/cookie report，response-stage
  pending transfer 保留原 body progress source，active body stream 还保留 response final URL 与 request-cookie report；随后统一构造
  move-owned captured-response load job。job 显式使用 `LifecycleTarget` reply boundary，与迁移前 synthetic fulfill 的完成语义一致；
  第70切片的 ordinary response continue 仍使用 `DocumentCommit` boundary，两者没有被错误合并。synthetic response 不进入
  download detection，response `Set-Cookie` 仍由 frozen load inputs 写入；
- response-stage failure 复用既有 exact-page failure apply participant；fulfill 复用同一 navigation build/materialize/apply chain。
  Fetch command response 仍只是 frontend projection：receiver 被 drop 后不再投递该 Fetch ACK，但 Host 必须继续完成原
  `Page.navigate` 的 commit/error。新增两个 production-shaped 回归分别覆盖 request-stage fulfill 与 response-stage fail，
  同时断言 exact Page identity、frontend drop 不取消 Browser action，以及 synthetic DOM/原 navigation error 的最终结果；
- Core neutral-decision 聚焦用例 `1/1`（run `18806ac9-0084-44cb-bb6a-0a56e265a803`）；既有 request fulfill
  回归 `1/1`（run `7867bd09-6b38-441e-a5e5-8b60f618d41c`），response fail/fulfill 与 active-stream 矩阵 `4/4`
  （run `19989ac9-b533-4c51-825e-8148155fba7b`）；两个新 exact-owner/frontend-drop 用例 `2/2`（run
  `d22e7514-9bd1-4e0c-8c2c-08da91c88f1d`），单用例 stress `20/20`、共 `40/40` 次通过（run
  `5d1d6f27-984c-4cea-879f-c693154f41bd`）；navigation control/response-stage 整个 `53/53` 矩阵连续
  `10/10` 轮、共 `530/530` 次通过（run `a9a6ec9e-5cce-4dc1-87e9-43cef65605ea`）。typed BiDi direct
  compatibility 回归 `1/1`（run `5abcae67-cfef-4503-9a6d-823042a07a60`），`moli-protocol` 全量
  `3392/3392`（run `5a8ce3d3-bbdc-4ac8-83f0-89701fc9aff6`）与 protocol all-target clippy `-D warnings` 均通过；
- workspace 全量 nextest `16088/16088`、既有 skip `17` 全部通过（run
  `a22f6e8b-7ec1-43c8-ae08-8905960fc384`）；`cargo fmt --all --check`、`git diff --check`、workspace all-target
  clippy `-D warnings` 与 `cargo build --workspace --release` 均通过。固定 `target/release/moli` SHA-256 为
  `af5403b3cf7e0ba6722f05a990ba49b8e4bc9ca0d1ad01304fdd0dc89703b6ed`；显式清除大小写
  HTTP/HTTPS/ALL/FTP proxy、原 no-proxy 与 inherited smoke group，并设置 `NO_PROXY=*` / `no_proxy=*` 后，CDP
  默认全组 `245/245`、`ok=true`，WebDriver Classic/BiDi/Selenium/Semantics `--continue-on-failure`
  `157/157`、`ok=true`，两套失败列表均为空；
- typed BiDi/Classic `DevToolsCommand` 目前仍在一个借用 `CdpConnection` 的 future 内执行；在该 frontend 暴露
  scheduler-visible task seam 前，publish 后原地等待会让 Host 无法取得 physical executor，所以本切片明确保留
  typed direct compatibility path。这不是第二套 Browser semantics，但仍是 Phase 4 未通过的边界；detached navigation
  completion transport、download/background-target、Phase 5 fact channel 与 Phase 6 Host lifetime 也未在本切片扩大处理。

2026-08-04 第七十二切片实现记录（typed BiDi/Classic terminal Fetch scheduler task cutover）：

- 本切片关闭的是 production typed frontend 的 progress dependency，不以清除所有 Protocol `await` 为目标。此前 BiDi/Classic
  虽已复用第65至71切片的 paused-navigation state machine，但 `execute_devtools_fetch_command_async_with_protocol_events`
  会在借用整个 `CdpConnection` 的 future 内等待；若让主文档 decision 进入 Host mailbox，该 future 又会阻止 application
  scheduler 取得同一个 physical executor。现在 `PendingDevToolsFetchCommand` move-own command route、success result、projection
  context 与 exact pending participant，wait 不再借用 connection；
- application 新增独立 `cdp_scheduler/fetch_dispatch.rs`，只接管五类 terminal decision：`ContinueRequest`、
  `ContinueResponse`、`ContinueWithAuth`、`FailRequest` 和 `FulfillRequest`。scheduler 在等待 typed completion 时继续选择
  Browser Host turn、Host participant completion、background navigation/output 与 renderer publication；terminal Host apply
  同时发布原 navigation 与当前 Fetch completion 时，先观察 exact command completion，再处理无关 post-command work，保持
  原 response/event fence 顺序；
- main-Document decision 以 `admit_main_document_to_browser_owner=true` 进入既有 `ResolvePausedNavigation` mailbox，并显式持有
  navigation renderer-publication gate，直到 exact Host projection 安装完成；subresource decision 不伪装成 Browser action，仍走
  exact Page participant，但同样由 application scheduler 等待而不是藏在 borrowed frontend future 里。两条路径共享原 Fetch
  completion/result normalization，没有复制 request/auth/response semantics；
- production 调用点 audit 确认所有 BiDi DevTools dispatch 都进入带 `CdpSchedulerEventReceivers` 的 external-load-wait 入口；五类
  `network.*` terminal command 由 BiDi adapter 映射到上述五个 typed variant。Classic command executor 也统一进入同一入口，
  但当前不暴露 Fetch interception API。不带 receivers 的 BiDi helper 只执行 realm、BrowserContext、layout/FrameTree 与 Target
  等只读查询，不能承载 terminal Fetch decision；
- direct `CdpConnection::execute_devtools_command` 没有 application Host participant loop，因此继续以
  `admit_main_document_to_browser_owner=false` drain 同一套 state machine，明确只作为测试/嵌入 compatibility adapter。它不是第二套
  production action authority，也不再作为 Phase 4 production blocker；本切片没有建立新 queue、sleep、retry、`yield_now` 或
  Browser fact channel；
- production-shaped typed fulfill 回归证明 main Document task 暴露 exact Host wait、renderer gate 与 Page owner，且 Host action
  完成后原 navigation response、typed empty result 和 synthetic DOM 都在既有 fence 上投影；该用例连续 `50/50` 轮通过（run
  `0addfce3-cc24-4edb-af10-80f88f948e66`）。production WebSocket BiDi Network 的 request-stage、response-stage、auth、
  main-Document 与 subresource 共 `8` 个场景连续 `20/20` 轮、共 `160/160` 次通过（run
  `a522747c-efe7-45bb-a18b-b48501d0a20e`）；`moli-protocol + moli` 全量 nextest `3959/3959`（run
  `2d322586-2deb-41f4-9d8f-eb1d2e241548`），workspace 全量 nextest `16090/16090`、既有 skip `17`（run
  `f16b501e-1666-4ac6-96e7-62c61348027c`）；
- pre-rebase tree 的 `cargo fmt --all --check`、`git diff --check`、workspace all-target clippy `-D warnings` 与 workspace release
  build 均通过；固定 `target/release/moli` SHA-256 为
  `e6bcc792dc029add44059c9b3a5324f3e9366727aa314b9f98235ad690ce5f9a`。显式清除大小写 HTTP/HTTPS/ALL/FTP proxy、
  原 no-proxy 与 inherited smoke group，并设置 `NO_PROXY=*` / `no_proxy=*` 后，CDP 默认全组 `245/245`、WebDriver
  Classic/BiDi/Selenium/Semantics `157/157`，两套均为 `ok=true`、失败列表为空；
- 提交后执行 `git pull -r origin master`，上游从 `efeed5c372` 前进到 `29e5ec97a7`，带入 Streams owner cleanup 与 direct
  input focus 稳定化等真实 tree 变化。84 个分支提交在旧整合切片重放时只产生一处测试注释冲突；resolution 保留上游以
  programmatic focus 取代 `autofocus` race 的意图，没有 skip commit。rebased conflict/typed-owner 聚焦回归 `2/2`（run
  `7cc47665-0858-44e3-aa9d-f2dc5391e325`），最终 workspace nextest `16146/16146`、既有 skip `17`（run
  `75c0630b-8413-4e05-9e6b-2182cb7b82cc`）；最终 tree 的 fmt、diff-check、workspace all-target clippy 与 workspace release
  build 再次通过，release SHA-256 为 `e053f65ac00e1f35762df0a9580478b3728f577ff294045eadff1696308134fa`。按相同无代理
  环境复跑，CDP 新总数 `246/246`、WebDriver 四组 `157/157`，两套均为 `ok=true`、失败列表为空；
- 本切片仍不宣称 Phase 4 完成。detached `wait:none` completion 已不拥有 start authority，但其 transport 应归 Phase 5 outcome
  audit；download/background-target 是否还存在 frontend-owned action 必须以 production trace/call graph 单独确认。Phase 6 Host
  lifetime 与 direct fixture compatibility wrapper 也不能因为 typed production path 已迁移就顺带删除。

2026-08-04 第七十三切片实现记录（download action progress / frontend projection separation）：

- Phase 4 exit audit 找到一个真实 owner-progress dependency：navigation download 与 renderer pending download 虽然已在独立 task
  中完成，但 task 会先等待当前 CDP command 的 `response_flush`；frontend 不 flush 或 permit 被取消时，网络请求、流式写盘与
  download registry terminal 都不会发生。403/DCL 修复所要求的同一个不变量同样适用这里：response fence 只能排序 observation，
  不能授权或取消已经接受的 Browser action；
- 新增独立 `conn/download_event_projection.rs`，把 action 和 frontend event gate 拆成两个并行 participant。下载 task 立即执行；
  projection waiter 只观察 response permit，flush 后按序释放，permit cancellation 则丢弃 frontend events，但两者都不影响 action。
  gate 在 flush 前只保留有限的 will-begin/initial-progress prefix 和**最新一批** progress；body 有多少 chunk 都不会令事件缓存线性
  增长。实现上每个被 response gate 挡住的 download 增加一个只等 frontend flush 的 Tokio task，但没有新增 Browser Owner drain/pump；
  flush 后继续直接投递，不新增 sleep、retry、轮询或另一套 download semantics。这个 per-download waiter 是迁移 plumbing，Phase 5 的统一
  fact journal/projector 建立后应由 subscriber gate 取代，不能复制成各功能各自一套 watcher；
- navigation/prefetched download 的 body 写盘现在先完成，再把固定大小的完整事件序列交给 gate；pending streaming download 则在每个
  chunk 后更新 coalesced progress，registry `Completed/Canceled` 与 artifact rename 都发生在 frontend projection 之外。无 active
  response permit 的 direct adapter 由 gate 立即 release，保持既有同步/测试语义；早期 start 仍留在 command 的 post-response
  segment，后续 progress 不可能越过它；
- 三条因果回归分别锁定 gate 的 `start + latest progress` 顺序、navigation artifact 在 held flush 下已经完成，以及 pending download
  在 held flush 下已经向真实本地 HTTP server 发出请求。聚焦 `3/3` 通过（run
  `b8180639-eab0-4e68-b6c9-b5eb8fed72b3`）；三条共同 `--stress-count 20 --flaky-result fail`，共 `60/60` 次通过（run
  `af58814a-2ee5-4ba9-b544-cf68fce8fcef`）。本轮修改前 workspace 基线为 `16145/16146`，唯一失败是无关的
  `sandboxed_blob_iframe_keeps_opaque_storage_context_for_opfs_messages`；该 exact case 随后 `10/10` 全过（run
  `504af16d-761d-4fba-b4be-caa31c1c30c6`），没有借本切片修改 structured-clone/OPFS 或放宽测试；
- 修改后 `moli-protocol` 全量 nextest `3397/3397`（run `22f06b7d-68ce-4443-9d8b-45d2cb42b4bc`），workspace
  全量 nextest `16149/16149`、既有 skip `17`（run `d2b0f54f-c1b1-4600-895d-c61f661ec107`）。workspace
  all-target clippy `-D warnings`、workspace fmt/diff check 与 workspace release build 均通过；固定
  `target/release/moli` SHA-256 为
  `18ea61a4cbd9d43ae2a1b5aeb76bca2cf9f26525879e81560c9536bad1011498`。显式清除大小写 HTTP/HTTPS/ALL/FTP
  proxy、原 no-proxy 与 inherited smoke group，并设置 `NO_PROXY=*` / `no_proxy=*` 后，CDP 默认全组 `246/246`、
  WebDriver Classic/BiDi/Selenium/Semantics `157/157`，两套均为 `ok=true`、失败列表为空；
- 提交后按约定执行 `git pull -r origin master`，上游从 `29e5ec97a7` 前进到 `ef7439ba17`，新增 Spider benchmark
  resource timeline 报告实现与测试；85 个分支提交无冲突重放，没有 skip commit。rebased tree 的上述三条专项回归
  `3/3`（run `0c2ad64e-7606-4972-93fc-03d581091a44`），workspace 全量 nextest `16149/16149`、既有 skip `17`
  （run `9aeb9353-351a-49a1-b4af-da07a73a7f52`）。workspace fmt/diff check、all-target clippy `-D warnings` 与
  workspace release build 再次通过；release SHA-256 保持
  `18ea61a4cbd9d43ae2a1b5aeb76bca2cf9f26525879e81560c9536bad1011498`。按相同无代理环境复跑，CDP 默认全组
  `246/246`、WebDriver 四组 `157/157`，两套均为 `ok=true`、失败列表为空；
- background-target call graph audit 同时排除了普通 parked navigation completion：background `Page.navigate`/reload/history 都按 exact
  `PageResidenceIdentity` 进入既有 Owner request/replacement state machine，loaded/failed projection 直接写回原 background slot，
  不会先 promotion，也没有 `ProtocolSchedulerWork` fallback。target activation 的 Core topology commit 也发生在 renderer state
  synchronization `await` 之前，不是 navigation completion authority；
- audit 仍发现三个同源的 initial target URL 触发器尚未汇流：`Target.createTarget` 的 post-create continuation、
  `Runtime.runIfWaitingForDebugger` 和 `Page.enable` 都可能调用 Protocol 的
  `start_initial_document_target_url_navigation_if_needed_background_events_async` / `start_initial_document_navigation_for_session_owner`，
  再直接启动 background load，而不是发布 exact Browser Owner input。因此本切片关闭 download progress blocker，但不宣称 Phase 4
  完成；下一 production cut 应把这三个触发器归一成一个 protocol-neutral initial-target navigation input，再删除 direct start path。

2026-08-04 第七十四切片实现记录（initial target URL Browser Owner admission cutover）：

- Core 新增独立模块 `browser_host/initial_target_navigation.rs` 与
  `BrowserInitialTargetNavigationInput`。input 只携带 exact `PageResidenceIdentity` 和 immutable destination URL，不包含 CDP command id、
  session/target route、domain subscription、response gate 或 output queue；`Target.createTarget`、debugger resume 与 `Page.enable` 只是三个
  advisory trigger，不再各自拥有 navigation start authority；
- `BrowserNavigationOwner::accepts_initial_target_navigation` 统一检查 exact Target instance/Page generation、仍处于 initial empty
  Document、没有已接受的 cross-Document navigation，并拒绝 initial URL 本身。frontend publication 前可用它跳过明显 no-op，但
  Browser Host 真正选中 mailbox turn 后必须再次检查；因此两个 trigger 在 selection 前同时入队也只会有第一个启动 request，后一个会在
  Core 已标记 pending 或 generation 已变化后 no-op，旧 Page input 不能追随 successor；
- `Target.createTarget` 继续遵守 `waitForDebuggerOnStart`：无需等待时只按新 Target 的 exact route 发布 input；
  `Runtime.runIfWaitingForDebugger` 在 inspector resume 成功后发布同一 input；`Page.enable` 在既有 initial-document policy 判定需要 replacement
  时也只发布。Host 未安装/已关闭时三者返回 typed publication error，不会回落到 Protocol direct start。旧的
  `start_initial_document_target_url_navigation_if_needed_background_events_async` 与
  `start_target_url_navigation_if_allowed_background_events_async` production path 已删除；
- Browser Host executor 在选中 input 后重新解析 exact owner route，并复用既有 Page navigation participant 完成 network、commit、Page
  replacement 与 target-info projection；等待期间 input、actor 和 frontend response 不互相借用。command turn 本身不发布
  `ProtocolSchedulerWork`，也不会在 response completion 内执行导航。Document commit 后仍可能发布既有
  `MainDocumentLoadOwnerAction` 来等待 exact load terminal；这是下一阶段应迁入 neutral fact/outcome channel 的 compatibility tail，
  本切片没有把它伪装成已消失，也没有新增 drain、pump、watcher 或另一套 navigation state machine；
- 修改前 clean HEAD workspace 基线为 `16149/16149`、既有 skip `17`（run
  `43ffde0c-2c5b-407a-80a2-9c20bdd69876`）。Core input/acceptance、Host selection、`Page.enable`、
  `Target.createTarget` 与 debugger-resume/Fetch pause 共六条因果回归 `6/6`（run
  `f4628d19-a1e8-4a9f-abd4-a6156a48029d`）；同六条各执行 `20` 轮、共 `120/120` execution，无 flaky（run
  `f77b2cd8-388e-471d-bd4b-2d4261da3eb7`）。`moli-core + moli-protocol` 扩大矩阵
  `6002/6002`、既有 skip `13`（run `4ed0eb72-524f-411b-8705-31b68604ba64`）；
- pre-rebase workspace 全量 nextest `16152/16152`、既有 skip `17`（run
  `0705266b-5b6f-47cf-a9e7-f33407e6b9dd`）。`cargo fmt --all --check`、`git diff --check`、workspace
  all-target clippy `-D warnings` 与 workspace release build 均通过；固定 `target/release/moli` SHA-256 为
  `876d452fc74abea3d706f47348af7b71a12f8513c50fbbd440b9673e7064c0a8`。显式清除大小写
  HTTP/HTTPS/ALL/FTP proxy、原 no-proxy 与 inherited smoke group，并设置 `NO_PROXY=*` / `no_proxy=*` 后，CDP 默认全组
  `246/246`、WebDriver Classic/BiDi/Selenium/Semantics `157/157`，两套均为 `ok=true`、失败列表为空；
- 提交后执行 `git pull -r origin master`；`origin/master` 仍为 `ef7439ba17`，Git 明确返回 current branch up to date，因而没有 commit
  replay、skip 或冲突。post-pull 六条因果回归 `6/6`（run `51a9e9a3-c27b-4d6c-83b5-730d803ba3e0`），workspace
  全量 nextest 再次 `16152/16152`、既有 skip `17`（run `8b0c461e-8d28-4738-8a03-7eb166599c08`）；fmt、diff-check、workspace
  all-target clippy 与 workspace release build 再次通过，release SHA-256 保持
  `876d452fc74abea3d706f47348af7b71a12f8513c50fbbd440b9673e7064c0a8`。按相同无代理环境复跑，CDP 默认全组
  `246/246`、WebDriver 四组 `157/157`，两套仍为 `ok=true`、失败列表为空；
- production call graph 复核后，三个已命名 trigger 都只剩 typed publication，test-only renderer direct helper 已显式
  `#[cfg(test)]`。但 `Page.createIsolatedWorld` 仍因“先确保 requested initial URL committed，再创建 world”的 command dependency 调用
  `start_initial_document_navigation_for_session_owner`；renderer top-level history output 仍会进入
  `traverse_session_owner_history_from_renderer_background_events_async`。这两条必须作为后续独立 owner cut 迁移，不能借本切片宣称 Phase 4
  完成；Phase 5 的 loaded-tail/fact projection 与 Phase 6 Host lifetime 也保持未完成。

2026-08-04 第七十五切片实现记录（renderer top-level history Browser Owner cutover）：

- 旧路径把 `RendererOwnerAction::TopLevelHistoryTraversal { delta }` 留到 Page output projector，再依据 mutable session route 调用
  `traverse_session_owner_history_from_renderer_background_events_async`；该函数会解析 history destination、直接启动 Page navigation，并在
  output completion 内等待整个 renderer/navigation step。renderer intent 因而虽已产生，execution authority 仍藏在 Protocol projection
  turn。现在 output ingress 先把 raw delta 绑定为 `PagePreparedTopLevelHistoryTraversal { exact Page, traversal }`，projector 只把它
  move 进 Core `RendererTopLevelHistoryTraversalInput`；input 不含 session、frontend correlation、resolved entry/URL、event queue 或 wait
  policy；
- Browser Host actor 选中 input 后才重验 `PageResidenceIdentity`，在 exact none-session route scope 内调用 Core authoritative history
  resolver，并复用第55/57切片已有的 no-op、same-Document 与 cross-Document classifier/participant。Page replacement 或 generation advance
  会在 selection 前 stale-drop；越界 delta、history 已清除或 destination 已消失都按 renderer `history.go()` 语义静默 no-op，不能追随
  successor Page，也不能回退到 frontend session；
- history source 继续显式携带 `HistoryTraversalStartSource::Renderer`，包括 same-Document renderer 拒绝后 fallback 到 URL load 的分支，因而
  CDP scheduled/requested/cleared-scheduled lifecycle shape 不会被误投影成 browser-command navigation。`Runtime.evaluate` response 只证明
  JavaScript 已排入 traversal intent，不是 traversal terminal；Browser Host turn 可在 response 后独立推进。旧的
  `start_session_owner_history_traversal_from_renderer`、`traverse_session_owner_history_from_renderer_background_events_async` 与 async output
  completion 调用已删除，Host 未安装时返回 typed publication error，不存在 `ProtocolSchedulerWork` 或 direct-start fallback；
- 两条新增因果回归分别冻结 stale generation 和 parked background Page route：旧 Page input 在 Host selection 前 advance generation 后不会
  改动 URL/history/request；background Target 的 `history.back()` 在 Runtime response 后仍停在第二个 URL，唯一 ready Host turn 完成后才回到
  第一个 URL，active Target identity/URL 始终不变。既有前后退/越界用例也改成显式消费具体 Browser Host input，不再把 Runtime response
  completion 当作 traversal completion；没有加入 sleep、retry、轮询、`yield_now` 或宽松成功条件；
- 修改前 clean tree 的 workspace 基线为 `16152/16152`、既有 skip `17`（run
  `8b0c461e-8d28-4738-8a03-7eb166599c08`），专项基线 `4/4`（run
  `d50fd7e2-91b4-4f4b-8108-fff99634cf91`）。首轮实现后专项为 `6/7`（run
  `59dc6249-6ab9-4aeb-bbf3-4fe10b4c1201`）：唯一失败精确暴露既有测试仍假定第二次 `Runtime.evaluate` response 会 inline 完成 traversal；修正
  测试因果边界后同组 `7/7`（run `b66dd962-2d9d-4599-b1c2-8d6251cac39e`），最终 renderer-history 名称组 `6/6`（run
  `3b94db73-1017-4d07-970d-bf245cae2469`，包含 renderer scheduled/requested/cleared probe 顺序）。同六条各执行 20 轮、共
  `120/120` 次通过（run
  `5de4b63b-a390-45dd-adfc-044d8ccc13af`）；
- `moli-core + moli-protocol` 扩大矩阵 `6006/6006`、既有 skip `13`（run
  `6aafe1c4-fd65-44c4-8624-c0cc27244f95`），workspace 全量 nextest `16156/16156`、既有 skip `17`（run
  `f42d06fa-375f-4487-9846-ad8132e4b8df`）。`cargo fmt --all --check`、`git diff --check`、workspace all-target clippy
  `-D warnings` 与 workspace release build 均通过；固定 `target/release/moli` SHA-256 为
  `0da80892f3dedfab2e7d442a0327fe701285049cf292f4e32a21f63460188706`。显式清除大小写 HTTP/HTTPS/ALL/FTP proxy、原 no-proxy 与
  inherited smoke group，并设置 `NO_PROXY=*` / `no_proxy=*` 后，CDP 默认全组 `246/246`、WebDriver
  Classic/BiDi/Selenium/Semantics `--continue-on-failure` `157/157`，两套均为 `ok=true`、失败列表为空；
- 提交后按约定执行 `git pull -r origin master`，`origin/master` 从 `67fb30bbe2` 前进到 `fa3dd36527`，87 个分支提交全部重放，
  没有 skip。第 43 个旧切片 `detach navigation inspector replays` 与 master 新增的 target-qualified `uniqueContextId` rewrite 在
  `runtime_eval.rs` 测试尾部发生一次 content conflict；resolution 同时保留 master 的 current/foreign Target realm-id 两条回归与分支的
  move-owned replay participant 回归，没有选择整侧覆盖。history + conflict boundary 专项 `11/11`（run
  `06929d08-9c61-4cb2-82b6-148f005817cf`），同组连续 20 轮、共 `220/220` 次通过（run
  `f8cceda2-a61e-4a8c-93aa-77a8f7a58285`）；
- rebased tree 的 workspace 全量 nextest `16181/16181`、既有 skip `17`（run
  `66a38ef6-e692-485c-b38f-cbd6ba138aa2`）；`cargo fmt --all --check`、`git diff --check`、workspace all-target clippy
  `-D warnings` 与 workspace release build 再次通过。最终 `target/release/moli` SHA-256 为
  `e055042fa90cdb993e4706918dba4fd63784057e093046c26f1bbfba83ebd25f`。按相同无代理环境复跑，上游新增两条 smoke 后 CDP
  默认全组为 `248/248`，WebDriver Classic/BiDi/Selenium/Semantics 仍为 `157/157`；两套均为 `ok=true`、失败列表为空；
- production call graph 复核确认 renderer history direct function 已无引用，`TopLevelHistoryTraversal` output slot 现在只负责 typed
  publication。Phase 4 仍不宣称完成：`Page.createIsolatedWorld` 的 requested initial URL prerequisite 还在 command-owned task 内直接调用
  `start_initial_document_navigation_for_session_owner`。既有 `MainDocumentLoadOwnerAction` 是 Phase 5 要替换的 exact load subscriber/
  outcome transport，不能借本切片改名为 Browser action，也不能为了“零 Protocol work”提前再造 history/load 专用 queue；Phase 6 Host
  lifetime 同样保持未完成。

2026-08-04 第七十六切片实现记录（`Page.createIsolatedWorld` initial-target prerequisite Browser Owner cutover 与 Phase 4 exit audit）：

- 旧 `createIsolatedWorld` task 在判断 initial empty Document 有 replacement URL 后，会直接调用
  `start_initial_document_navigation_for_session_owner`，再把 `PendingNavigateCommand` 或 paused-Fetch continuation 嵌进自己的 Page command
  phase；CDP task 因而同时拥有“等待这个前置条件”和“启动/推进顶层导航”两种责任。现在 Core 新增
  `BrowserFrontendCommand::EnsureInitialTargetNavigation`：input 只含 opaque `BrowserCommandId`、exact
  `PageResidenceIdentity` 与 immutable URL，不含 CDP id/session、world 参数、response route、event subscription 或 socket state；
- Protocol 以 opaque id 保存一次性 continuation sidecar，`createIsolatedWorld` 的 initial phase 只 move-own receiver。Browser Host actor
  选中 input 后才重验 Core initial-Document/pending-request/generation authority，并复用同一 page-owned navigation participant。terminal
  navigation plan 回到 dependent command，保留 nested navigation events、renderer predecessor 与 exact insertion boundary，再继续解析当前
  renderer attachment 并创建 world；Host 未安装/停止会 typed reject，禁止回退到 Protocol direct start；
- dependent frontend wait 若在 terminal 前消失，oneshot send failure 会把 navigation plan 交给 Host 的 detached projection，Browser action、
  protocol events 和 renderer fence 都继续结算，不能因 CDP command lifetime 取消。actor selection 时 action 已被另一 exact transition 满足或
  变 stale，则只完成空 prerequisite；后续 world phase 重新解析当前 attachment，不能追随旧 generation 的物理 Page；
- 旧 `InitialDocumentNavigation` / `InitialDocumentNavigationContinue` command phase 与
  `start_initial_document_navigation_for_session_owner`、`start_navigate_to_url_command` 两个零引用 direct helper 已删除。production call graph
  中 `createIsolatedWorld` 只剩 typed publication；没有新增 sleep、retry、`yield_now`、poll loop、drain/pump 或逐功能 watcher；
- 修改前专项基线 `21/21`（run `fb9cf58f-184b-487c-9372-09814f16d861`）。最终 Core/input、Host absence、selection-before-start、
  terminal-before-reply、detached frontend、initial URL commit 与既有 create-world 行为名称组 `28/28`（run
  `1e35ee68-3ce4-4f2e-b9ca-124cb523ff3e`）。期间新增 detached regression 首次为 `27/28`（run
  `c23e1a68-1660-4840-846d-1e13d2cb9478`）：失败来自测试误用只允许无 fence 输出的 flatten helper，保护断言正确拒绝压平 renderer
  boundary；回归改为检查 explicit command-turn boundary 后单条通过（run `5d395389-2599-4180-8cd5-9709c35a2ee3`），没有修改 product
  guard。五条核心因果回归连续 `20` 轮、共 `100/100` 次通过（首尾 run
  `8be869d7-121a-4363-9e52-95050bf750d4` / `afe9aa18-e75c-4770-bfee-abd719dc254d`）；
- `moli-core + moli-protocol` 扩大矩阵 `6026/6026`、既有 skip `13`（run
  `da8531e3-76dd-458b-9247-a15423053a75`），workspace 全量 nextest `16185/16185`、既有 skip `17`（run
  `6706b93a-e216-4d89-8f90-b217bb5f9214`）。`cargo fmt --all --check`、`git diff --check`、workspace all-target clippy
  `-D warnings` 与 workspace release build 均通过；固定 `target/release/moli` SHA-256 为
  `998c6e830bfa2dc83972f1601f15aa1d9cff1fc837706f01510a9726b0fc50e8`。显式清除大小写 HTTP/HTTPS/ALL/FTP proxy、原
  no-proxy 与 inherited smoke group，并设置 `NO_PROXY=*` / `no_proxy=*` 后，CDP 默认全组与 WebDriver
  Classic/BiDi/Selenium/Semantics `--continue-on-failure` 连续两轮均为 `ok=true`、失败列表为空；计数复核分别为
  `248/248` 与 `157/157`；
- 提交后按约定执行 `git pull -r origin master`，`origin/master` 从 `fa3dd36527` 前进到 `ef44056fe9`，88 个分支提交全部重放，
  没有 content conflict 或 skip。rebased tree 的本切片专项 `28/28`（run
  `d6e3e3c9-8d31-4549-8070-a095cc47c0a5`）。首轮 workspace 全量为 `16187/16188`（run
  `f9efa71f-fba2-4004-aae7-7c4450cf7ba6`）：唯一失败是上游新增的 child-frame Page event fan-out fixture 直接赋值
  `conn.browser_context`，绕过 Phase 2 后 Core-owned topology 注册，因而 exact root Document attachment 单独稳定失败（run
  `f579f697-0410-472e-a3b9-6fd8f555c9ec`），不是调度 flaky。fixture 改走 production/test 共用的
  `insert_browser_context` 注册边界后单条通过（run `67403dc1-51b5-45ea-b772-11297be2`），20 个独立 nextest 进程共
  `20/20`（首尾 run `99a8ea40-4e43-496a-a668-3b1830b5507e` / `c00b6ebc-c344-416c-862c-5bff74ec48ca`），相关
  child-frame producer 组 `10/10`（run `dd572fbe-f665-41a7-b701-569b0dcaad7f`）。最终 workspace 全量
  `16188/16188`、既有 skip `17`（run `e8871796-848d-4205-b8fc-732067865e86`）。`cargo fmt --all --check`、
  `git diff --check`、workspace all-target clippy `-D warnings` 与 workspace release build 再次通过；最终
  `target/release/moli` SHA-256 为
  `8ed077c4ebc18cf3c5455a5611196297b450218eeaf878543bc97cf9de7d1f93`。按相同无代理环境复跑，CDP 默认全组
  `248/248`、WebDriver Classic/BiDi/Selenium/Semantics `--continue-on-failure` `157/157`，两套均为
  `ok=true`、失败列表为空；
- Phase 4 exit audit 确认 `ProtocolSchedulerWorkKind` 仅余 `ProtocolObservation`、Phase 5 的
  `MainDocumentLoadOwnerAction` 与 BiDi channel continuation，不含 navigation、replacement、popup 或 termination browser action；所有 production
  顶层 command/renderer/auxiliary/Fetch/termination/initial-target action 都经 Browser Host mailbox 与同一 Core request authority。因此 Phase 4
  正式完成。下一刀进入 Phase 5 fact/outcome journal；不能把 `MainDocumentLoadOwnerAction` 改名后继续算 Phase 4，也不能为“零 Protocol await”
  清理无 application Host loop 的测试/embedding compatibility adapter。

Exit gate：`ProtocolSchedulerWork` 不再包含 navigation、replacement、popup 或 termination browser-owner
payload；所有来源对同一 target 的 conflict/cancel 使用同一 request state machine。

### Phase 5：navigation/lifecycle/target fact journal

目标：把 browser state transition 与 protocol event shape 分开。

工作：

1. 发布 `NavigationAccepted/Committed/Failed`、`PageReplaced`、exact lifecycle、Target lifetime facts；
2. 为 CDP 实现 projector，保持现有 event shape/session filtering/loader identity；
3. 为 BiDi/Classic/high-level wait 增加同源 consumer；
4. pending DCL/load attachment 改成 subscriber/wait ticket，不再是 owner action；
5. 建立 per-subscriber cursor 和 bounded lag policy；
6. 将 command response ordering 与 fact occurrence time 分开记录。

状态：已完成（2026-08-05 exit audit）。唯一 Core transition 已发布 top-level Target creation、navigation
admission/terminal、loaded commit + Page replacement、Target crash/close 与 exact Document lifecycle facts。CDP 使用单一
`CdpBrowserFactProjector` 和 bounded cursor；BiDi、Classic 与 high-level wait 使用同源 exact fact ticket/outcome，不再向
Protocol Page slot 注册 waiter，也不从 CDP result/error JSON 反推 Browser outcome。application scheduler 只接收 payload-free
coalesced wake；subscription、response visibility 和 wire ordering 只影响 frontend 投影，不参与 Browser progress。

exit audit 逐项确认了本阶段六项工作：journal 有显式 lag/cursor policy；旧 Page fact 会按 replacement/failure/termination
topology 退休；root lifecycle sidecar 只能由 exact fact sequence 与冻结 Document binding 授权；Target bootstrap 来自 Core
current-state snapshot。Core transition 不构造 `BackgroundProtocolEvent`，关闭 frontend subscription 不改变 Browser trace。
当前 fact projector、response correlation 和部分 transport 仍物理驻留 `CdpConnection`，这是 Phase 6 的 Host lifetime/transport
迁移问题，不是第二份 Browser authority，因此不再阻塞 Phase 5 exit。

2026-08-04 第七十七切片实现记录（bounded Browser fact journal 与 exact lifecycle 首个 producer）：

- `moli-core::browser_host` 新增独立 `fact_journal` 模块，工作契约为
  `BrowserFactSequence`、immutable `BrowserFactEnvelope`、`BrowserFact`、typed publish/receive/lag error 与
  `BrowserFactSubscriber`。首个 fact 只有
  `DocumentLifecycleReached { document, milestone, stamp }`：envelope 携带同一 Browser Owner 的
  `BrowserInstanceId`、typed BrowserContext/Target、exact `PageResidenceIdentity`；fact 携带 exact
  `RendererDocumentLifecycleIdentity` 与 renderer sequence/timestamp stamp，不含 session id、Page domain enable、loader wire shape、
  `BackgroundProtocolEvent` 或 WebSocket state；
- journal 使用固定 `1024` 条 retained ring 和同容量 non-blocking fanout。每个 subscriber 先获得创建 cursor 时仍保留的有界 bootstrap，
  再接收 future facts；slow consumer 得到显式 `Lagged { skipped }`，publish 不 `await`、不等待 frontend。没有 subscriber 时 retained
  ring 仍保存 transition，Browser fact sequence 仍单调推进；大型 DOM/network/download payload 不进入 journal；
- journal 当前作为 `BrowserNavigationOwner` 的字段保存，以复用 Phase 2 已建立的唯一 `BrowserInstanceId` 与 exact Page authority；它的类型和
  API 位于 Browser Host 模块，Protocol 只能请求 Core 记录已经被权威入口接受的 lifecycle record。这个物理 residence 是 Phase 6 前的迁移形状：
  `CdpConnection` drop 仍会 drop owner/journal，不能宣称 Browser Host lifetime 已独立；Phase 6 应整体移动 owner state，而不是在 actor 中再分配
  第二个 browser instance 或复制 journal；
- `TargetPageSlot` 的 renderer lifecycle ingress 现在显式返回 `authoritative` 与 `visible` 两个集合。exact binding、Document/epoch、严格递增
  renderer sequence 通过后，record 立即进入 authoritative cursor 和 Browser fact producer；command-response load visibility barrier 只决定
  record 是否进入当轮 CDP-visible 集合。释放 barrier 只投影原 deferred tail，不再写第二条 fact。因此语义固定为：

  ```text
  renderer exact lifecycle record accepted
    -> authoritative Page lifecycle state
    -> BrowserFactJournal append(sequence, exact Page, exact Document)
    -> optional CDP visibility barrier
    -> existing CDP event projector
  ```

- 普通 committed Document bind/ingest 与 initial-empty-Document materialization 的 active/background 直接 Page 投影都接入同一 Core fact
  入口；只对 Page registry 中 exact current generation 发布。stale/targetless Page 返回 typed rejection并保留诊断，不允许旧 Page milestone 进入
  successor。Started/terminated record 仍更新 renderer lifecycle state，但本切片只将实际 reached 的 DCL/load 作为 Browser fact；
- 现有 CDP event shape、session fan-out、loader id、Page subscription replay 与 response visibility barrier 没有改由 Core 构造。
  `MainDocumentLoadOwnerAction` 也仍是 Protocol exact-load observer/terminal projection transport；把它改成 journal subscriber、让 CDP projector
  消费同源 fact、再发布 navigation/target facts，属于后续 Phase 5 切片。本切片不新增 sleep、retry、poll/drain/pump、per-feature watcher 或
  Browser progress gate；
- 修改前 renderer lifecycle 基线 `16/16`（run `e2fa94f0-02da-4551-b521-0b906f16fc59`）。Core journal retention/bootstrap、
  subscriber lag 与 stale Page generation 三条契约回归 `3/3`（run
  `12e47f04-fae7-482c-8c78-138f42990e64`）；authoritative/visible 分离、既有 lifecycle cursor 与关键 Page integration
  因果组 `14/14`（run `831eb927-eee0-47a1-92ac-6ee59d14ddd7`）。关键用例证明在 Page lifecycle subscription 尚未 enable、load
  仍被 visibility barrier 隐藏时，journal 已按 exact Document 顺序持有 DCL/load；随后 enable/disable frontend subscription 与释放 barrier
  不新增、删除或重排 fact。background initial-Document direct projection exact-owner 单条通过（run
  `42502aa2-8f42-4adc-97e2-2142c39a8f1f`）；四条核心 causal/identity regression 用 20 个独立 nextest run 共
  `80/80` 次通过（首尾 run `e7c6506e-8e14-4413-8ce6-985c94af7437` /
  `30edb908-a16d-429b-8237-f53cf6a948b9`）；
- `moli-core + moli-protocol` 扩大矩阵 `6031/6031`、既有 skip `13`（run
  `32164bfd-d548-4db1-a896-bfd7a63de3e2`），workspace 全量 nextest `16191/16191`、既有 skip `17`（run
  `f9939ab2-b9da-452a-b7dd-18c9d6007c7d`）。`cargo fmt --all --check`、`git diff --check`、workspace all-target clippy
  `-D warnings` 与 workspace release build 均通过；固定 `target/release/moli` SHA-256 为
  `7c55d866c6aa446ec85f21a7f61a65dcc54715939fab7d75d6ded728d4149fac`。显式清除大小写 HTTP/HTTPS/ALL/FTP proxy、原
  no-proxy 与 inherited smoke group，并设置 `NO_PROXY=*` / `no_proxy=*` 后，CDP 默认全组连续两轮均为 `ok=true`，计数复核
  `248/248`；WebDriver Classic/BiDi/Selenium/Semantics `--continue-on-failure` 为 `157/157`、`ok=true`；两套失败列表均为空；
- 提交后按约定执行 `git pull -r origin master`，`origin/master` 从 `ef44056fe9` 前进到 `cac2e67294`，89 个分支提交全部重放，
  没有 content conflict 或 skip。rebased tree 的 lifecycle 聚焦组 `16/16`（run
  `04f30ec2-1d48-429f-9819-8f1ce64fcb93`），workspace 全量 nextest `16191/16191`、既有 skip `17`（run
  `f42ec092-2c4c-43c0-b89b-5d9cd7350b90`）；`cargo fmt --all --check`、`git diff --check`、workspace all-target clippy
  `-D warnings` 与 workspace release build 再次通过，release SHA-256 保持
  `7c55d866c6aa446ec85f21a7f61a65dcc54715939fab7d75d6ded728d4149fac`。按相同无代理环境复跑，CDP 默认全组因上游新增
  case 为 `249/249`，WebDriver 四组为 `157/157`，两套均为 `ok=true`、失败列表为空。

2026-08-04 第七十八切片实现记录（loaded Page replacement 首个 navigation fact producer）：

- `BrowserFact` 新增 protocol-neutral
  `PageReplaced { previous_page: A, navigation: N }`。fact envelope 统一携带已提交 successor `B`、Browser instance、typed
  BrowserContext/Target 与全局单调 sequence；payload 保留 retired predecessor `A` 和 exact
  `BrowserDocumentNavigation N`（request id、Target、loader identity）。它不携带 CDP session、command id、event shape、response gate 或
  renderer physical `Page`；因此 frontend attach/detach 不能改变 replacement truth；
- 唯一 producer 位于 `BrowserNavigationOwner::commit_loaded_page_replacement`。只有 request identity、Target recovery eligibility、Page
  generation、joint history 与 initial-empty-Document exit 已在同一 Core turn 成功提交后才 append fact；`prepare` 不发布，stale/superseded
  permit、renderer attachment rollback 与 Protocol physical projection 均无发布权限。固定顺序为：

  ```text
  validate exact pending request N and predecessor A
    -> atomically commit request + Page generation B + history/recovery/initial-document state
    -> append PageReplaced(A, B, N)
    -> return replacement to the physical Page projector
    -> asynchronously dispose predecessor Page A when needed
    -> later exact successor lifecycle ingress appends DCL_B / load_B
  ```

- fact append 不 `await`、不等待 subscriber，也不依赖 predecessor disposal；sequence exhaustion 只产生显式 production diagnostic，不能在
  browser state 已提交后 panic 或伪造 rollback。successor DCL/load 复用同一 journal，因此必须取得更大的 Browser fact sequence；旧 Page lifecycle
  仍由 exact generation 拒绝；
- Core 回归同时断言 exact context/Target、`A/B/N` identity、唯一 sequence、replacement-before-DCL 与 stale permit 零 fact。Protocol adapter
  回归进一步证明 fact 在旧 Page `close_async()` participant 开始等待前已经可观察，且 superseded request 即使完成 renderer candidate rollback 也不会
  发布 replacement fact。本切片没有建立 CDP projector，也没有更改既有 CDP/BiDi/Classic event shape 或 loaded-tail waiter；后续 consumer 必须从
  journal 投影，不能从 physical Page storage 反推第二份 replacement truth；
- 开始本切片前按用户要求立即执行 `git pull -r origin master`：`origin/master` 从 `cac2e67294` 前进到 `b016375769`，分支 89 个提交全部重放，
  无 conflict/skip；新上游 parser DOM refresh/DCL barrier 与既有 lifecycle fact 基线 `15/15`（run
  `e8c34c5b-3f8a-4b26-a962-0c7863650e1d`）。本切片 Core 聚焦 `5/5`（run
  `41ecb03c-f2cf-4872-be30-8095ffea5e5d`），Protocol replacement adapter `2/2`（run
  `6698ec6d-f2fe-42a7-a04a-931ee415d965`）；四条 exact/stale Core + Protocol 回归使用 nextest 原生
  `--stress-count 20 --flaky-result fail` 共执行 `80/80`，零 flaky（run
  `b25ed6ad-3661-40f2-ace7-9f309407c9eb`）；
- 首次 Core + Protocol 扩大矩阵在机器同时为另一工作树 `/home/donoughliu/code/moli0` 编译两份 V8-heavy Rust test binary、24 核 load
  average 达到 `33.25` 时为 `6030/6033`（run `ca0442be-3f95-4715-84e8-52676370addc`），三个失败均位于未修改的 DOM test。
  三条精确复跑 `3/3`（run `a07002d1-05f1-418f-ae37-8a45518d09b2`），原生 stress `60/60`（run
  `d883a369-92d1-4e78-a185-6159b512a9a2`），Protocol 全包 `3424/3424`（run
  `d6a9c1e0-bb90-4cd8-89a0-610f96f9ba1e`）。相同扩大矩阵在外部链接仍运行时为 `6031/6033`（run
  `d3c5c880-1845-4344-9a8a-6f94ded34d1f`）：一个相同 DOM identity failure，另一个 test-only detached commit wait 在 CDP 已投影
  DCL/load 后超时；后者精确 stress `20/20`（run `f8213799-a463-46a3-949c-4c6cdc1f706c`）。未修改 production 或这些无关测试，未降
  nextest 并发、增加 timeout 或使用 retry。外部编译结束、机器负载回落后，原命令最终为 `6033/6033`、既有 skip `13`（run
  `67fa3de9-92d1-4ada-8efe-b4339281459a`）；
- 最终 workspace 全量 nextest `16195/16195`、既有 skip `17`（run
  `4ca7f313-e4f0-49c8-a46e-ca2997bfe37d`）。`cargo fmt --all --check`、`git diff --check`、workspace all-target clippy
  `-D warnings` 与 workspace release build 均通过；固定 `target/release/moli` SHA-256 为
  `adc4dfe339400de4c67ad2ec0120ffc0338a817bc150d5e7816acf5ec3825327`。显式清除大小写 HTTP/HTTPS/ALL/FTP proxy、原
  no-proxy 与 inherited CDP smoke group，并设置 `NO_PROXY=*` / `no_proxy=*` 后，CDP 默认全组因上游新增 parser DOM case 为
  `250/250`、WebDriver Classic/BiDi/Selenium/Semantics 为 `157/157`；两套均 `ok=true`、失败列表为空。提交后的收尾
  `git pull -r origin master` 再次确认 `origin/master` 仍为 `b016375769`，Git 报告当前分支 up to date；无 conflict、skip 或 commit
  rewrite，代码树和上述固定 release 二进制均未变化。

2026-08-04 第七十九切片实现记录（exact Target crash/close fact producer）：

- 把迁移 `MainDocumentLoadOwnerAction` 所需 terminal 反向推导了一遍：只有 DCL/load reached fact 不足以替换 renderer lifecycle observer；等待
  期间 exact Page 还可能被 replacement、Target crash 或 Target close 终止。第七十八切片已有 `PageReplaced`，因此本切片先补唯一 Target
  termination truth；Document lifecycle interruption 仍须由后续切片补齐，不能在 journal consumer 中回头读取 Protocol Page slot 猜 terminal；
- `BrowserFact` 新增 `TargetCrashed { previous_page: A }` 与 `TargetClosed { previous_page: A }`。两者的 envelope 均携带
  Target terminal Page generation `T`、exact Browser instance/Context/Target 和单调 sequence。crash 表示 `A -> T` 后 Target 仍 live、可由一个
  exact recovery navigation 恢复；close 表示 `A -> T` 与 Target topology removal 同一 Core commit，`T` 即使已不能从 live registry lookup，仍是
  immutable terminal identity。fact 不携带 session、inspector listeners、close reason、physical `Page` 或 Target promotion projection；
- 唯一 producer 位于 `BrowserNavigationOwner::commit_target_termination`，且只在 termination state、Page generation、Target topology、request
  registry、engine ownership、history 与 initial-empty-Document state 全部成功提交后 append。capture/prepare 不发布；stale Page generation、delayed
  crash、rollback 和 Protocol projection 均无发布权限。固定顺序为：

  ```text
  validate exact Target + Page A + crash|close permit
    -> atomically commit Target state + terminal Page T + request/runtime/history cleanup
    -> append TargetCrashed(A,T) or TargetClosed(A,T)
    -> return immutable termination result
    -> synchronously project physical absence
    -> asynchronously dispose retired Page / promote successor Target when needed
  ```

- crash 后 close 会得到严格连续的两条 fact：第一条 envelope `T1` 是第二条 payload `previous_page`，第二条 envelope 为 `T2`；stale close 零
  Target terminal fact。Protocol production-shaped 回归覆盖 Page.close、Page.crash、Target.closeTarget，并证明 loaded background Target 的
  `TargetClosed` 在 retired Page `close_async()` participant 开始等待前已进入 journal。本切片不改既有 CDP/BiDi/Classic crash/close event shape，
  不创建 watcher、drain/pump 或 frontend cache；现有 projection 继续消费 transaction result，下一步再让 neutral wait ticket 消费 facts；
- 修改前 Core + Protocol Target termination 基线 `21/21`（run `883ae3d8-ffa3-444e-8fce-583d96587e4d`）。本切片 exact
  identity/sequence/stale/cleanup-before-wait 聚焦 `7/7`（run `8bb4b0a5-3d93-4c83-adb3-1241721de1c6`），完整 Target
  termination 因果组 `21/21`（run `633288ad-f736-400e-b672-e8770d66cc9b`）；七条关键回归用 nextest 原生
  `--stress-count 20 --flaky-result fail` 共执行 `140/140`，零 flaky（run
  `0fe63ff7-a512-4667-9833-cbe92b18375c`）；
- 扩大验证没有用 retry 取最后一次绿色。最初两次 Core + Protocol 全矩阵都为 `6031/6033`、既有 skip `13`（run
  `59ff97cf-ea6d-49e8-b782-0debde4d3e34`、`777036bc-bd14-45c1-8adf-54516635c9fa`），随机落在四条 DOM 测试；精确与
  原生 stress 均不复现。保持默认并发继续 stress 后又抓到 patchright DOM、search 与 child-frame Runtime 用例。共同违反路径是测试把
  `Page.navigate` 的 early command response 或 root Document load 当成新 DOM/child realm 已完成；这恰好违反本文已规定的
  `navigate reply != DCL/load terminal`，不是 Target fact producer 的生产竞态；
- 独立测试提交 `685ae3b7df` 没改生产等待语义：DOM fixture 按 exact `frameId + loaderId` 等 renderer-owned load，child-frame Runtime
  fixture 在 navigation 前订阅 Page facts 并等 exact child `frameStoppedLoading`。所有等待仅消费真实 scheduler input，无 sleep、retry、timeout
  放宽或降并发。54 条相关路径原生 `--stress-count 20 --flaky-result fail` 共 `1080/1080`（run
  `18abb7db-7af6-485f-8782-79d8bde49988`）；随后 Core + Protocol 完整矩阵连续两轮共 `12066/12066`、每轮既有 skip `13`（run
  `d1b582e5-d60a-4bee-ab21-a42018f2b2db`）。此前单次未复现的 file-chooser 症状也包含在该聚焦矩阵和完整矩阵中，但证据仍是有界 stress，不能声称
  证明未来任意调度均不再出现；
- 随后的首次 workspace 全量为 `16194/16195`、既有 skip `17`（run `e16fc438-fca1-4036-af3a-e03f6f575de3`），唯一失败位于
  renderer-v8 SharedWorker stale-event 测试：测试要求一个 HTTP streaming navigation owner turn 必须越过 phase one，但生产状态机合法返回
  `PendingPhaseOne`。精确原生 stress `100/100`（run `95695f2a-5ae9-45d5-9522-44e600575d08`）证明它只在扩大调度窗口出现；源码审计则确认
  断言本身不成立。测试改用已物化的 `data:` replacement Document，保留“真实 PageVm replacement 后 stale event 不得进入新 realm”的契约，移除与
  该契约无关的 HTTP phase-one timing。修改后精确 stress `100/100`（run `9caa8750-71a2-4310-ba59-11b0ad88abc8`），整个
  SharedWorker client-event 邻域 `7 * 20 = 140/140`（run `cd0a4775-d585-4e87-a2fd-71202c43131a`）；未增加 sleep、retry、timeout、skip
  或生产 drain；
- 按用户要求在收尾前立即执行 `git pull -r --autostash origin master`：`origin/master` 从 `065e3145d0` 前进到 `744e161dad`，分支 91 个
  commit 全部重放，autostash 完整恢复，无 content conflict 或 skip。rebased tree 的完整 Target termination 因果组 `21/21`（run
  `1e31ebb0-c6e7-47ba-ae01-e74a6dfeb498`）；workspace 全量 nextest 一次通过 `16204/16204`、既有 skip `17`（run
  `9c7ada40-870c-49c5-b4a9-06d3afcd2331`）；
- `cargo fmt --all --check`、`git diff --check`、workspace all-target clippy `-D warnings` 与 workspace release build 均通过；固定
  `target/release/moli` SHA-256 为 `0194aa36d3d861cc06f2c54f9c385731c72e04b16519bd3761bad54d683ad884`。显式清除大小写
  HTTP/HTTPS/ALL/FTP proxy、原 no-proxy 与 inherited smoke group，并设置 `NO_PROXY=*` / `no_proxy=*` 后，CDP 默认全组连续两轮均为
  `ok=true`，第二轮计数复核 `251/251`；WebDriver Classic/BiDi/Selenium/Semantics `--continue-on-failure` 为 `157/157`、
  `ok=true`；两套失败列表均为空。

2026-08-04 第八十切片实现记录（main-Document load neutral fact wait ticket）：

- 从 deferred main-Document load 的全部 terminal 反向审计 owner：只补 renderer `Terminated` 不够。等待旧 Document load 时，新 cross-Document
  request 一旦成为 Target 的 pending request，旧等待就应立即 `Superseded`；不能等到新请求成功 replacement，因为请求若在 commit 前失败，旧
  Page-slot observer 已被 navigation admission 终止，而只消费 `PageReplaced` 的 journal waiter 会永久 pending。因此本切片同时加入
  `NavigationAccepted { navigation }` 和 `DocumentLifecycleTerminated { document, last_reached, termination }`；
- `NavigationAccepted` 只由 `BrowserNavigationOwner::try_start_document_navigation_with_trace` 在 request registry、Target recovery state 与
  initial-empty-Document pending state 已共同接受 exact request 后发布。fact envelope 是请求所替代的 exact Page `A`，payload 是 Core request
  identity `N`；disposing Context rejection、same-Document navigation 和未进入 request registry 的 intent 零 fact。固定因果为：

  ```text
  accept cross-Document request N2 against Page A
    -> request registry pending = N2
    -> BrowserFact::NavigationAccepted(A, N2)
    -> pending load wait(A, Document D) = Superseded
  ```

- renderer lifecycle 的唯一 authoritative ingress 现在把 exact `Terminated` 与 reached DCL/load 一样投影进 Browser journal。termination fact 保留
  exact renderer Document/epoch、`last_reached`、renderer sequence/timestamp/reason；load waiter 若 `last_reached >= Load` 则仍为 `Reached`，否则为
  typed `Interrupted`。`PageReplaced(A, B, N)`、`TargetCrashed(A, T)`、`TargetClosed(A, T)` 继续作为 replacement/Target terminal 的 fallback；
- Core 新增 move-only `BrowserDocumentLifecycleWaitTicket` 与 cloneable read-only readiness。ticket 只消费 immutable Browser facts，并按 exact
  Browser instance journal、Context/Target/Page generation、renderer Document/epoch 和 milestone 过滤；结果只有 `Reached`、带 termination stamp 的
  `Interrupted`、`Superseded` 或 typed `Unavailable`。journal close 与 subscriber lag 都显式 terminal，不能回读 Protocol Page、猜当前状态或静默
  悬挂；
- async wait cursor 和 scheduler readiness cursor 由 Core 一次 `subscribe_browser_fact_pair()` 在同一个 retained/future cut 创建。两个 cursor
  独立消费但共享只写一次的 terminal：readiness 可在下一个 Browser Owner turn 被选择前同步看见已发布 fact，不依赖 spawned wait task 是否获得
  调度；async cursor 又不会被 readiness 偷走。两者都没有 publish/mutate 能力，slow frontend 不会 backpressure Browser Owner；
- 删除 Protocol `document_lifecycle_observer` 模块、Page-slot observer publisher 字段，以及 navigation/replacement/clear 时反向完成 observer 的 callback
  authority。保留的 `RendererDocumentLifecycleWaiter` 仅服务现有显式 DevTools polling wait key，并继续由自己的 release protocol 管理。回归明确证明：
  仅把 physical `CdpConnection.browser_context` 投影移除不会制造 Browser terminal，必须先有 Core fact；
- `ProtocolSchedulerWorkKind::MainDocumentLoadOwnerAction` 同步改名为 `MainDocumentLoadFactProjection`。它仍是保证既有 CDP output causal ordering 的
  durable Protocol continuation，但不再拥有或推进 renderer/Browser lifecycle；CDP response/event shape、load visibility barrier 与 session routing
  未改。当时尚待迁移的通用 CDP projector、navigation outcome facts 和其他 high-level wait consumer 仍属于后续 Phase 5，不能据此宣称 Host
  lifetime 已独立；
- 修改后的 Core ticket 与跨层 deferred-load 精确回归 `14/14`（run `c1df4ef8-e261-44ff-831b-a3aadaa226e9`）；其中覆盖 retained load、future
  termination、navigation acceptance、retained replacement、unrelated Target fact、显式 lag、DCL 不满足 load、terminal-before-adapter-wait、
  physical projection loss 与 Target crash。final lock implementation 改用仓库规定的 `parking_lot::Mutex` 后，同组再次 `14/14`（run
  `71368f8e-da64-4faa-b335-a47652f2e4d3`），并以 nextest 原生 `--stress-count 20 --flaky-result fail` 共执行 `280/280`、零 flaky
  （run `7bdcb4a5-d194-4948-a77f-831cd25a69de`）；fact producer、Page replacement、Target termination 与 deferred-load scheduler
  扩大因果组 `55/55`（run `8808ace4-d944-4bcd-9ebc-22629618f918`）；
- 首次 Core + Protocol + application 完整矩阵为 `6604/6605`、既有 skip `13`（run
  `a49257c4-d24a-403f-9b26-13e0dde948a0`），唯一失败是未修改的 file-chooser `document.open()` 测试在 replacement 后用“当前 Document”
  registry 反查旧 Document backend id。原用例精确 stress `100/100`（run `b40a71dc-ced6-427d-a294-c588f8b0c470`），全部 file-chooser
  邻域 `12 * 20 = 240/240`（run `6ed46812-ed79-4f3a-81b3-d287c0673a6b`），证明不是本切片的确定性生产回归，但原断言混合了两个
  Document scope。独立测试提交 `edaf87d292` 把 current-Document shared-id registration 与同一 JS turn 的 click -> `document.open()` stale
  backend isolation 分成两条真实 Page 契约；没有改生产代码、sleep、retry、timeout 或并发。两条精确回归 `2 * 100 = 200/200`（run
  `478fae86-3781-4f54-b15a-e33aa338f16d`），扩大后的全部 file-chooser 邻域 `13 * 20 = 260/260`（run
  `05dbef9d-92de-4e1b-ae8d-33eb86b5f4e7`）；随后三 crate 完整矩阵 `6606/6606`、既有 skip `13`（run
  `ada6e9f2-fbcf-43d3-8e7c-04ecf38a3685`）；
- 按用户要求在本切片收尾开始时立即执行原样 `git pull -r origin master`；Git 因工作树有本切片未提交修改拒绝 rebase。随后把完整工作树
  （含新增文件）临时 `stash -u`，再次执行同一 pull，再立即 `stash pop`；`origin/master` 仍为 `744e161dad`，分支 up to date，恢复无
  content conflict，临时 stash 已删除。本次同步未改写提交或代码；
- 最终 workspace 全量 nextest `16210/16210`、既有 skip `17`（run
  `7c4fa601-2e50-476e-a0a1-7b2a348904f5`）。`cargo fmt --all --check`、`git diff --check`、workspace all-target clippy
  `-D warnings` 与 workspace release build 均通过；固定 `target/release/moli` SHA-256 为
  `17bbff39c4e2fab2b5f809fe518b30ecd1adeb9757a3338edfcd01d9571e133a`。显式清除大小写 HTTP/HTTPS/ALL/FTP proxy、原
  no-proxy 与 inherited smoke group，并设置 `NO_PROXY=*` / `no_proxy=*` 后，CDP 默认全组 `251/251`；WebDriver
  Classic/BiDi/Selenium/Semantics `--continue-on-failure` 为 `157/157`；两套均 `ok=true`、失败列表为空。

2026-08-05 第八十一切片实现记录（loaded navigation commit 与 Page replacement 同源 fact batch）：

- producer 审计确认，当前 production loaded-Document 成功路径只有
  `BrowserNavigationOwner::commit_loaded_page_replacement` 能同时把 exact request 从 pending 移到 committed、推进 Page generation、写入 joint
  history、提交 crash recovery navigation 并退出 initial-empty-Document 状态。Protocol 的 renderer attachment、旧 Page disposal 和 preload
  listener 都只是这次 commit 前后的 participant/projection，不能单独声称 navigation 已 committed；
- `BrowserFact` 新增 `NavigationCommitted { navigation }`。唯一 Core transaction 在全部权威状态提交后，以一个 `publish_batch` 固定发布：

  ```text
  pending request N against Page A
    -> commit request + Page B + history + recovery state
    -> sequence K:     NavigationCommitted(N), envelope Page B
    -> sequence K + 1: PageReplaced(A, N),      envelope Page B
    -> renderer lifecycle facts for Page B
  ```

  两条 fact 共享 exact Browser/Context/Target/Page B source，并保持相邻 sequence；没有第二个 commit 点、Protocol callback、frontend session id 或
  subscription 判断。`NavigationCommitted` 表达 request outcome，`PageReplaced` 表达 topology change，即使目前二者总在同一 loaded commit
  出现，也不能让 consumer 通过其中一条反推并伪造另一条；
- stale/superseded permit、stale Page generation 和 renderer attachment rollback 都在 Core commit 前失败，因此不得发布任一 commit fact。Core
  回归检查 acceptance -> committed -> replacement -> DCL 的严格顺序与同一 successor Page；Protocol 回归检查两条 fact 在 predecessor Page
  disposal participant 等待前已经可见，并检查 stale request 两条都不泄漏；现有 CDP response/event shape、session filtering 和 command-response
  barrier 本切片不变，尚未把 frontend event 改为 journal projector；
- 同轮 failure producer 审计没有把 navigation tail 的 loader-only `clear_pending...` 升格成 `NavigationFailed`：network/load failure 可能先提交
  `FailedNavigationDiscard` Page transition，Fetch cancellation 保留当前 Document，download 是无 Document commit 的成功分流，而新 request
  admission 会 supersede 旧 pending request。下一切片必须为这些路径设计 exact request token + protocol-neutral typed terminal transaction，并明确
  download 与 failure 的不同语义；不能在 renderer replay tail 完成后按 loader 猜失败，也不能把 errorText/CDP result shape 存进 Browser Core；
- 修改前 Core 成功 commit 基线 `1/1`（run `bcd8372e-4af3-48f1-9bf5-651833f5aca4`）。修改后 Core exact commit `1/1`（run
  `c188899c-a4d6-42a4-a4d7-a28d0b3c56e3`）；Core Page replacement 因果组 `8/8`（run
  `579456ba-3146-4734-ad0f-f856c70d31bc`）、lifecycle wait consumer 组 `6/6`（run
  `17b78a89-1c31-4a0c-bdf9-b6cbc849c6b7`）、Protocol replacement/rollback 组 `2/2`（run
  `115161ae-8b5e-4ea0-8deb-81a8cc6358a4`）。Core commit 用例原生 stress `20/20`（run
  `da863ef1-8c59-4ef1-bb5d-f00b458ef622`），Protocol commit + stale rollback 两条用例原生 stress 共 `40/40`（run
  `2c28f31f-d0ff-4162-93b7-140c49142907`），均使用 `--flaky-result fail`；
- workspace 全量 nextest `16210/16210`、既有 skip `17`（run `135460df-d20d-4e19-8d39-3071470cf2d8`）。
  `cargo fmt --all --check`、`git diff --check`、workspace all-target clippy `-D warnings` 与 workspace release build 均通过；固定
  `target/release/moli` SHA-256 为 `df3adc786cc10568f9312edef4e7fa94524884531c09b02fefa6e10133f938e3`。显式清除大小写
  HTTP/HTTPS/ALL/FTP proxy 和 inherited smoke group，并设置 `NO_PROXY=*` / `no_proxy=*` 后，CDP 默认全组连续两轮均
  `ok=true`，第二轮计数 `251/251`、失败列表为空；WebDriver Classic/BiDi/Selenium/Semantics
  `--continue-on-failure` 为 `157/157`、`ok=true`、失败列表为空；
- 本切片开始前按用户要求立即执行 `git pull -r origin master`，`origin/master` 没有当前分支尚待重放的新提交，Git 报告 up to date；工作树当时
  干净，未使用 autostash，也没有 content conflict 或 skip。

2026-08-05 第八十二切片实现记录（accepted navigation 的 exact non-commit terminal fact）：

- production terminal-path 审计把跨 Document request 的 Browser 不变量收敛为：

  ```text
  NavigationAccepted(N1, Page A)
    -> exactly one of:
       NavigationCommitted(N1, Page B)
       NavigationFailed(N1, reason, Page A or successor/terminal Page B)
       NavigationConvertedToDownload(N1, Page A)
  ```

  `BrowserNavigationFailure` 是 Core-owned、protocol-neutral 的 typed reason：`Network`、`Commit`、`Canceled`、
  `Superseded { replacement }`、`TargetCrashed`、`TargetClosed`；错误文本只表达浏览器失败事实，不保存 CDP command/session/result shape。
  download 是 request 成功终止但没有创建 Document，不能伪装为失败或 Page replacement；
- request registry 把 pending/committed navigation 与 trace sidecar 合并为一个 `BrowserDocumentNavigationRecord`，所有 retire/rollback 都携带 exact
  request token。新 request admission 若替换旧 pending request，会在同一个 Browser Owner batch 中先发布
  `NavigationFailed(N1, Superseded { N2 })`，再发布 `NavigationAccepted(N2)`；两条 envelope 都携带 admission 时的 current Page。旧 request 的
  delayed completion 因 request id + loader id 不再匹配，只能 stale-drop，不能终止 N2，也不能重复发布 N1 terminal；
- 保留 current Document 的 network/commit/cancel failure 通过 `fail_document_navigation_if_matches` 原子 retire exact pending request，envelope 仍是
  current Page，`previous_page = None`；response 转 download 通过独立 exact transaction retire request，Page generation 不变。需要丢弃失效
  Document 的 network failure 走 `FailedNavigationDiscard` Page transaction：先 exact-take pending authority，再 CAS 推进 Page generation；若 Page
  permit 已 stale，则同 turn 恢复原 request record 且不发布 fact；成功后清理 runtime/request residence，并以 successor Page envelope 发布
  `NavigationFailed`，payload 保存被淘汰的 `previous_page`。Page transition permit 的私有 payload 使用互斥 enum，不能表示“failure kind 但缺少
  navigation/failure”的内部无效组合；
- Target crash/close 在唯一 Core termination transaction 清理 request、runtime、history 与 Page/Target topology 后，如存在 exact pending request，
  会在同一个 batch 先发布 `NavigationFailed(TargetCrashed|TargetClosed)`，再发布 `TargetCrashed|TargetClosed`；两条使用同一 terminal Page
  envelope，failure payload 保存旧 Page。没有 pending request 时仍只发布 Target terminal fact；
- Protocol 新增的 adapter 只把 session 一次解析为 exact Browser owner，并分别提交 failure/download transaction。materialized network/load error
  分类为 `Network`，renderer candidate/config/install rejection 分类为 `Commit`，`Page.stopLoading` 等保留当前 Page 的显式中止分类为
  `Canceled`；Fetch main-Document failure 若使 Document 失效则走 Page discard。download 必须先成功 retire exact Core request，才允许启动物理
  download projection。旧的 loader-only navigation tail 已改名并限定为 renderer/resource 的物理投影清理，不再修改 Core request authority；因此
  delayed N1 tail 即使在 N2 admission 后运行，也不能清除 N2；
- 聚焦验证中，Core navigation-owner 组 `99/99`（run `a515ff6d-309f-42bc-8c55-f58840f6e91d`），Protocol exact navigation adapter
  `2/2`（run `f3344072-b39b-44aa-9c6e-ad1749af7faa`），materialized failure/download、Fetch failure 与 stop-loading 组 `6/6`
  （run `f1bf8a82-1670-493e-87cd-19b7f81af605`）。第一次 Protocol 聚焦运行唯一失败是测试把 Fetch main-Document network failure 错写成
  `Canceled`；按 production 的 Document-invalidating 行为修正期望后通过，没有为测试改变实现；
- 收尾扩大验证中，Core + Protocol 全量 `6045/6045`、既有 skip `13`（run
  `afb3ce8e-9e29-43fa-bead-f885cf648989`）；9 条 exact terminal / supersede / stale rollback / Target close / Fetch / stop-loading / download /
  delayed-tail 关键用例原生 stress `20/20`，共 `180/180` 次通过（run `d594d07f-a80f-455d-b311-0d2c9db507b2`，
  `--flaky-result fail`）。第一次 workspace 全量为 `16215/16216`（run `f11e9f39-fb87-4ecd-9dce-c8eaa994c265`），唯一失败是本轮未修改的
  renderer sandboxed-blob-iframe/OPFS 测试；该精确用例随后单次通过（run `3254aea9-0852-489a-aff3-cfd1479f0d10`）、原生 stress
  `50/50`（run `a7796e39-4e06-4f57-a539-5316147d6ed0`），相邻 structured-clone 8 条用例 stress `10/10`、共 `80/80`
  （run `c0afc462-07f6-41af-88e1-e45d3b1bae1c`）；未加 sleep/timeout/retry，也未修改该测试或 renderer。最终 workspace 全量
  复核先以 `16216/16216`、既有 skip `17` 通过（run `3fc18d78-4e3b-426b-9238-024cbbf530bc`）；补齐 pending navigation 的 crash
  batch 顺序断言后，该分支原生 stress `20/20`（run `e14b32f1-71ba-4f28-9632-40a011ac2ed5`），最后一次 workspace 全量仍为
  `16216/16216`、既有 skip `17`（run `828d7053-2188-4ac4-aaf6-0fa6094b08cf`）；
- `cargo fmt --all --check`、`git diff --check`、workspace all-target clippy `-D warnings` 与 workspace release build 均通过；固定
  `target/release/moli` SHA-256 为 `53a7e01801a18020c41a3a9c22743e2fbc27a5064a134e87d5a922bc1c1552bc`。显式清除大小写
  HTTP/HTTPS/ALL/FTP proxy、原 no-proxy 与 inherited smoke group，并设置 `NO_PROXY=*` / `no_proxy=*` 后，CDP 默认全组连续两轮均
  `ok=true`，第二轮计数 `251/251`、失败列表为空；WebDriver Classic/BiDi/Selenium/Semantics `--continue-on-failure` 连续两轮均
  `ok=true`，第二轮计数 `157/157`、失败列表为空；
- 本切片开始前按用户要求立即执行 `git pull -r origin master`；工作树当时干净，Git 报告当前分支 up to date，没有 autostash、content
  conflict 或 skip。同步完成后才开始本切片修改；
- 本切片没有新增 sleep、retry、poll、drain、pump 或 per-request watcher，也没有改变现有 CDP event shape/response barrier。fact 的通用 frontend
  projector、BiDi/Classic/high-level consumer、journal lag recovery，以及 Browser Host 与 frontend connection 的 lifetime 分离仍属于后续 Phase
  5/6，不能据此宣称 Phase 5 已完成。

2026-08-05 第八十三切片实现记录（首个 root lifecycle frontend fact projector）：

- lifecycle producer 与 projector 的边界收敛为：

  ```text
  exact renderer lifecycle record R accepted for Page A / Document D
    -> Browser Core appends DocumentLifecycleReached(K, A, D, milestone, stamp)
    -> connection-local frontend subscriber observes K and freezes the matching protocol binding
    -> existing command-response visibility barrier releases the same stamp
    -> frontend projector checks exact K/A/D/binding/stamp
    -> emit existing CDP + automation DCL/load shape
  ```

  renderer record 仍负责有序 ingress 和 response visibility release，但不再单独授权 root `Page.domContentEventFired` / `Page.loadEventFired` 或对应
  `Page.lifecycleEvent`。没有 exact Browser fact、同一 raw record 第二次 replay、旧 attachment 或已经投影到较晚 sequence 后再倒序补发的旧 milestone
  都 fail-closed，不允许回退到旧 direct emitter；`Started`（包括 `document.open()` 的 `init`）尚无 Browser fact，仍走既有 renderer-local
  projection，不在本切片伪装成已迁移；
- `CdpConnection` 新增 `CdpBrowserFactProjector` 作为迁移期 frontend-owned cursor。它订阅 Core 的 immutable bounded journal，跨所有 fact 保持严格递增
  sequence；只有等待当前 frontend exact visibility 选择或退休的 `DocumentLifecycleReached` 进入本地 pending queue，通常是被 load visibility barrier
  挡住的 load，也可能是尚未被该 frontend 选择的较早 milestone；navigation/Target/termination facts 当前只推进 cursor、不构造 wire shape。pending
  lifecycle 上限固定为 `1024`，publish 和 Browser Owner 都不等待 projector；subscriber lag/close、非单调
  sequence、pending overflow、缺失 exact fact 均成为 typed terminal error，当前 adapter 记录错误并停止继续猜测 lifecycle event。application 统一
  disconnect/resnapshot policy 仍是后续工作；
- frontend cursor 不要求每个订阅者消费同一 Page/binding 的所有较早 lifecycle fact。若较晚的 exact load 已经由当前 frontend 的 visible record 选中，
  projector 会退休这个 binding 下未被该 frontend 选择的更早 DCL fact，再投影 load；随后旧 DCL raw replay 因 exact fact 已退休而 fail-closed。这个选择不是
  放宽 Document 内 renderer 顺序：`RendererDocumentLifecycleProtocolCursor` 仍负责拒绝倒序 ingress；它只承认 frontend 可以在 DCL 与 load 之间开始观察。
  本地 Chromium 对照也明确区分了两种行为：`third_party/blink/renderer/core/inspector/inspector_page_agent.cc` 中 `Page.enable` 只注册 agent、不会回放
  `domContentEventFired`，而 `setLifecycleEventsEnabled(true)` 才按当前 Document timing 回放已经达到的 lifecycle marker。因此“任何 frontend 看到 load 前
  都必须先消费 journal 中旧 DCL”不是 Chromium/CDP 不变量；
- projector 在 authoritative lifecycle ingress 后立即以非阻塞 `try_recv` 捕获 fact，并冻结
  `{PageResidenceIdentity, Renderer Document/frame, TargetPageAttachmentId, frameId, loaderId, navigation}` binding。这样 load fact 即使先发生、后因 response
  barrier 才可见，也不需要重读 mutable Page；同 renderer Document 的已完成旧 epoch 可以复用同一冻结 wire binding，而 attachment replacement 不能
  领取旧 fact。可见 record 只作为 exact release token，最终 timestamp、Document/epoch 和 navigation correlation 均取自 fact + frozen binding；
- lifecycle projection trace 现在同时写入 renderer lifecycle sequence、frontend projection sequence 和实际 `browser_fact_sequence`，并把输入 residence
  标成 `browser-fact-journal`。fact append 与 frontend output 的完整独立 wall-clock trace 仍需随通用 scheduler-side fact ingress 补齐，不能把当前相邻
  同线程 capture 误称为 application/Host lifetime 已拆开；
- production 搜索确认 root renderer DCL/load 的两个 `BackgroundProtocolEvent` constructor 只剩这个 fact-gated emitter 调用；其余命中均为 constructor
  本身或 serialization tests。child-frame lifecycle、`Page.setLifecycleEventsEnabled` 的 current visible snapshot replay、navigation/Target events 与
  `MainDocumentLoadFactProjection` completion transport 没有在本切片扩大迁移；后续应把 subscriber wake 提到 application scheduler，并逐 fact family
  增加 projector，而不是在每个 domain 新建 watcher；
- 修改前 lifecycle 邻域 `135/135`（run `438df3ef-77a3-478f-ba3c-daee9c11ee1d`）。新增 projector 的 exact visibility/once、attachment stale、completed
  epoch、late-observer skip/no-regression 与 bounded lag 五条回归最终 `5/5`（run `76df095f-f1b8-4f76-b722-5a5003751b1a`）；既有 lifecycle 邻域扩大到
  `136/136`（run `0dae1654-50e3-454c-956c-e0930b033dd0`），Protocol 全包 `3430/3430`（run
  `5bce6dd8-2294-4c6d-81ea-61805b2adeb1`），13 条 exact/stale/barrier/replacement/Chromium-order 邻域做 `--stress-count 20
  --flaky-result fail` 共 `260/260`（run `ce4e45ad-06a1-4aa8-9b01-b85e4b7ee896`）。开发中的第一处 fixture 回归是一个 test-only
  loaded-commit fixture 直接赋值 physical
  `browser_context` 并手工传 DCL、没有注册 Core Page 或发布 Browser fact；fixture 改走 `insert_browser_context`，显式记录 exact fact 后仍证明“不回读 live
  Page”，并新增 raw record duplicate 零 DCL/load 与 consumed fact sequence 断言，没有 production fallback。随后一版过严的 projector order guard 又让 4 条
  `document.open/write/close` 回归 `4/4` 必现超时：初始 Document 的 DCL fact 未被该 frontend 选择，初始 load 因此关闭整个 subscriber，饿死后续 replacement
  lifecycle；按上述 Chromium subscription 语义改为退休未选择的旧 fact 后，原 4 条 `4/4` 通过（run
  `3980c05c-044f-456e-bd86-7f44aa851ba4`），没有加 timeout、sleep 或 retry；
- 提交/rebase 前的 workspace 全量 nextest 为 `16221/16221`、既有 skip `17`（run `1dbd4f77-a780-4531-9ce1-2ca26847db35`）。
  `cargo fmt --all --check`、`git diff --check`、workspace all-target clippy `-D warnings` 与 workspace release build 均通过；固定
  `target/release/moli` SHA-256 为 `5f335925d3b2438a79458a9c3bf92d4a354daf28d2cc835f36291fa90362a198`。显式清除大小写
  HTTP/HTTPS/ALL/FTP proxy、原 no-proxy 与 inherited smoke group，并设置 `NO_PROXY=*` / `no_proxy=*` 后，CDP 默认全组连续两轮均为
  `251/251`、`ok=true`、失败列表为空；WebDriver Classic/BiDi/Selenium/Semantics `--continue-on-failure` 连续两轮均为
  `157/157`、`ok=true`、失败列表为空；
- 本切片提交后按用户要求执行原样 `git pull -r origin master`：`origin/master` 从 `744e161dad` 前进到 `dae23ca510`，97 个分支提交全部
  重放，没有 skip。三处 conflict 均按现有责任边界合并：`moli-protocol/src/lib.rs` 同时保留 master 的 renderer command exports 与分支的
  Browser Host turn export；`conn/dispatch.rs` 保留 master 的显式 parse-error 语义与分支的 owner-lane settle；Patchright shadow fixtures 使用 master
  已提供的 `navigate_to_data_html_async`，该 helper 自身已经等待 exact renderer load，因此没有叠加第二次等待。rebase 后 workspace all-target
  `cargo check` 发现 `Target.sendMessageToTarget` 的 owner-lane adapter 仍调用 master 已删除的 infallible `ParsedCdpCommand::parse`；修复抽出统一 raw
  dispatch 入口，让顶层和 nested command 共享 `parse_str` / `-32700`，并新增“outer command 成功、nested parse error 经
  `Target.receivedMessageFromTarget` 返回”的回归，没有引入 `expect` 处理不可信 payload；
- rebase conflict、nested parse 与本轮 projector 邻域 `14/14`（run `66612363-3c50-470e-85e7-09a5fc3e5ce0`），rebased Protocol 全包
  `3431/3431`（run `778deb7d-7c16-4e09-bffd-556d80b79e61`），workspace all-target `cargo check`、`cargo fmt --all --check` 与
  `git diff --check` 通过，Protocol all-target clippy `-D warnings` 通过。rebase 前已完成 16k workspace、release 与两套 smoke；按用户要求降低重复验收
  密度，rebase 后没有再次运行 workspace 全量、release 或 smoke，不能把上面的 release SHA 当成 rebased tree 的二进制哈希；
- 第一次 rebased branch 推送后的 remote 校验窗口中，`origin/master` 又新增一个仅修改 parser 调研文档的 `d4070fec16`。因此再次执行原样
  `git pull -r origin master`，98 个分支提交全部重放，无 conflict 或 skip；该上游增量不改代码，未因此重复 Rust/protocol 验证；
- 本切片没有新增 sleep、retry、poll、drain、pump、spawn 或 per-feature watcher，也没有改变现有 event JSON/session fan-out/response visibility barrier。
  cursor 当前仍和 authoritative `BrowserNavigationOwner` 一起物理驻留在 `CdpConnection`，只是责任上只读；通用 application-owned fact wake、Target/
  navigation projector、frontend attach bootstrap/lag recovery，以及 Browser Host 独立 lifetime 仍分别属于后续 Phase 5/6，不能据此宣称 Phase 5 完成。

2026-08-05 Phase 5 frontend fact ingress/projector 模块闭环记录：

- `BrowserFactJournal` 在 retained cursor 之外新增 payload-free `BrowserFactWakeSubscriber`。每个原子 publish batch 在 fact 已进入 retained ring 与
  broadcast cursor 后，只发布一次最新 `BrowserFactSequence`；wake 使用 `watch` 合并慢 consumer，不复制 envelope、不保留第二份 fact queue，且
  `send_replace` 不等待 frontend。新订阅者能看到当前 committed tail；journal drop 前已经提交的 tail 先交付，随后才报告 typed closed terminal；
- `CdpSchedulerEventReceivers` 现在物理拥有 wake subscription。WebSocket actor 的 `SchedulerInputReceivers` 与 direct CDP/WebDriver 的
  `recv_interleaved_input` 都把 `BrowserFactWake(sequence)` 当作独立 application input；renderer navigation gate 不屏蔽它，ready Runtime response
  snapshot 也先冻结当时已经 ready 的 fact wake。没有新增 background task、per-domain watcher、sleep、poll loop 或 Browser Owner drain；
- frontend 收到 wake 后让唯一 bounded fact cursor 前进，并严格验证 cursor 至少到达 wake sequence。lag、journal close、non-monotonic、pending overflow
  或 wake/cursor 不一致均沿 typed scheduler progress failure 终止该 frontend；Browser Owner 和其它 subscriber 不受影响。原先只表达 renderer transport
  的 failure wrapper 因此改名为通用 `CdpSchedulerProgressFailure`，避免把 fact ingress terminal 伪装成 renderer 错误；
- `CdpBrowserFactProjector` 现在同时拥有 navigation、Target terminal 与 root lifecycle 的 pending projection。Page navigation admission、普通/失效 Page
  failure、download、loaded commit、Page crash/close 和 BrowserContext disposal 的 Page close 都必须 claim exact envelope；loaded commit 要求
  `NavigationCommitted -> PageReplaced` 相邻，Target terminal 会一起消费同 batch 的 pending-navigation failure。URL、initiator、security origin、session fan-out、
  discovery subscription 和 response barrier 仍是在 producing turn 冻结的 frontend binding，不反向进入 Browser fact；
- projector 观察到 `PageReplaced`、带 predecessor 的 `NavigationFailed`、`TargetCrashed` 或 `TargetClosed` 时，会退休该 predecessor Page 尚未释放的
  lifecycle projection，防止 canceled response barrier 或 slow frontend 让旧 Document 长期占用 bounded pending window。production producer audit 确认
  Core navigation/replacement/termination producer 都通过上述 claim boundary；剩余直接调用位于 `#[cfg(test)]` fixture；
- root lifecycle producer 与 CDP projection 当前仍在同一 `CdpConnection` renderer turn。该 turn 内的 exact non-blocking capture 暂时保留，用于保证同一
  publication 中 DCL/load 与其它 renderer output 的既有顺序；直接把唯一 cursor 搬出 connection 会让 fact 到下一 application turn 才到达，从而需要新的
  output residence。后续应随 Browser producer 移入 Host 或引入明确 fact/output causal join 后删除这条 same-turn 快路径，不能把本模块描述成 projector
  lifetime 已独立；
- 本模块不改变 CDP/BiDi/Classic event JSON、session fan-out 或 response visibility barrier。Core coalescing/bootstrap/closed、application idle wake、
  same-cursor fact-family、old-Page retirement、failed-Page transaction 和 Target terminal adjacency 均有聚焦回归。最终当前树 workspace 全量 nextest
  `16233/16233`、既有 skip `17`（run `2cd09b14-eff0-4fb4-9734-ae1b5522a7d9`）；`cargo fmt --all --check`、workspace all-target
  clippy `-D warnings` 与 workspace release build 均通过。固定 `target/release/moli` SHA-256 为
  `e2555ac0f7a52c3f10544efacb5fec45deb85f9c5759f32ee3e5ce3eac2f0735`；
- 显式清除大小写 HTTP/HTTPS/ALL/FTP proxy 和 inherited smoke group，并设置 `NO_PROXY=*` / `no_proxy=*` 后，CDP 默认全组两轮均为
  `251/251`、`ok=true`、失败列表为空；WebDriver Classic/BiDi/Selenium/Semantics `--continue-on-failure` 两轮均为 `157/157`、
  `ok=true`、失败列表为空。Target creation/metadata facts、attach bootstrap/resnapshot 和 Browser Host 独立 lifetime 仍属于后续 Phase 5/6，
  因此本模块闭环不等于 Phase 5 exit。

2026-08-05 Phase 5 top-level Target creation fact 模块闭环记录：

- Core `BrowserContext` topology commit、background/active Target registration 与 bootstrap placeholder replacement 现在都在完整
  Target/Target-handle/Page-slot commit 后发布唯一 `BrowserFact::TargetCreated`。envelope 冻结 Browser instance、typed
  BrowserContext/Target 与初始 exact `PageResidenceIdentity`；不携带 tab facade、session、attachment、discovery filter 或 wire event；
- `BrowserTargetRegistration` 返回同一 commit 的 Page identity。Protocol 的统一 target-registry projector 在同 turn 完成 physical payload
  投影后，从唯一 `CdpBrowserFactProjector` claim 该 occurrence；即使没有 discovery subscriber 也会 claim 并丢弃 frontend 输出，避免关闭
  subscription 改变 Browser trace 或让 bounded pending window 累积。fact 缺失会使该 frontend projector 进入 typed terminal error，不能回退为
  根据 physical Target 猜测事件；
- `Target.createTarget` 与 renderer popup 路径把 claim 后的 opaque creation projection 随既有 completion move-own。输出前再次验证 exact
  Page-slot instance：initial Document materialization 可以推进 generation，但 Target close 后复用相同 public targetId 会获得不同 slot instance，
  predecessor token 不能投影到 successor。一个 occurrence 可以授权该 frontend 对同一 browser Target 生成 page/tab CDP fan-out 与 automation
  sidecar，但只能被 cursor claim 一次；auto-attach completion 本身不被 discovery subscription 反向阻塞；
- BrowserContext/default Target bootstrap 会立即 claim 并结束 creation occurrence。随后 `Target.setDiscoverTargets` 独立枚举当前 live topology，
  因而“创建时 discovery 关闭，稍后开启”仍报告现存 tab/page；重复开启依靠 frontend reported-target state 去重，不会再次 claim 或重放旧
  occurrence。这冻结了 occurrence 与 resnapshot 的模块边界，但 resnapshot 的 authoritative snapshot API 尚待从 physical Protocol topology
  提取；worker Target 仍是 renderer lifecycle，不伪装成 top-level Browser topology fact；
- Core 与 projector 回归覆盖 Context active/background 发布顺序、拒绝时零 fact、无 discovery claim、单次 claim、Document generation 前进、
  same targetId 新实例 stale-drop、bootstrap 消费、后开启 discovery resnapshot，以及既有 createTarget/popup/auto-attach 行为。聚焦
  `cargo nextest` 为 `84/84`（run `a5121ac3-6775-4c32-bd27-adf4a47cc311`）。本模块没有新增 task、watcher、sleep、retry、poll、
  drain 或 pump；最终当前树 workspace 全量 nextest `16239/16239`、既有 skip `17`（run
  `611e1045-e5d3-4396-bafb-d82389b5c0e4`），`cargo fmt --all --check`、`git diff --check`、workspace all-target
  clippy `-D warnings` 与 workspace release build 均通过。固定 `target/release/moli` SHA-256 为
  `e1ac8225192bb5c1c95e35f702f31108f500de0393e185847ffdfcab32f476cc`；显式清除大小写 HTTP/HTTPS/ALL/FTP proxy、
  原 no-proxy 与 inherited smoke group，并设置 `NO_PROXY=*` / `no_proxy=*` 后，CDP 默认全组 `251/251`、WebDriver
  Classic/BiDi/Selenium/Semantics `--continue-on-failure` `157/157`，两套均为 `ok=true`、失败列表为空；
- 提交后执行 `git pull -r origin master`，上游从 `d4070fec16` 前进到 `780d9fe8ed`，只新增一份 benchmark 审计文档；100 个
  分支提交无 conflict 或 skip 重放。排除 `docs/**` 后 pre/post-rebase tree diff 为空，post-rebase `cargo fmt --all --check` 与
  `git diff --check` 通过；按降低重复验收密度的约定，没有为纯文档上游增量再次运行 workspace nextest、release 或 smoke。

2026-08-05 Phase 5 Browser-owned top-level Target snapshot 与 attach bootstrap 模块闭环记录：

- Core 新增 protocol-neutral `BrowserTopLevelTargetSnapshot`、`BrowserContextTargetSnapshot` 与
  `BrowserTargetStateSnapshot`。snapshot 在一个 owner turn 内按 selected Context、inactive Context，以及各 Context 的 active Target、
  background Target 顺序冻结当前 topology；每项保存 exact Browser instance、Context handle、Target handle 和 Page-slot identity，不保存
  title、URL、opener、session、attachment、filter、tab facade 或 worker metadata；
- current-state snapshot 与 `TargetCreated` occurrence journal 明确分工：journal 回答“发生过什么”，snapshot 回答“现在有哪些 exact
  top-level Target”。Document generation、active/background residence 可以前进而不使稳定 Page slot 失效；Context/Target/Page-slot 被替换，
  或 public id 在另一个 Browser instance 中复用时，旧 snapshot 必须 stale-drop。即使 snapshot 为空也保留 Browser provenance，不能跨
  Browser Host 投影；
- Protocol 新增单一 `target_snapshot_projection` 边界：先验证 physical/Core topology，再从 Core 捕获 snapshot；投影时逐项验证 exact
  Context、Target 和物理 Page-slot handle，最后才 join 当前 title/URL/opener/attached metadata。renderer-owned shared/dedicated/service
  worker 仍由各 Context 的 frontend payload 追加；Core 不接触 CDP type、session 或 wire event；
- `Target.getTargets`、`Target.getTargetInfo`、`Target.getClientWindows`、`Target.setDiscoverTargets` 的现存 Target bootstrap，以及
  `Target.attachToTarget`、`Target.setAutoAttach` 的 top-level page/tab bootstrap 已改为消费同一 Core snapshot。direct attach 与
  auto-attach 在 initial Document/runtime binding 等 await 后重新验证 exact snapshot，再投影并提交 session/event；旧 Target completion
  不能绑定或宣布复用相同 targetId 的后继实例。auto-attach 在 Context activation await 后也重新验证 exact Context，恢复先前 selected
  Context 时不按裸 public id 选择后继实例；
- 本模块没有增加 task、watcher、sleep、retry、poll、drain 或 pump。Core/projection、discovery/get、direct attach、auto-attach 与
  same-id stale completion 的聚焦回归最终为 `88/88`（run `51ca9e6f-13dc-416d-903b-054a6fbcf1ad`），focused Core/Protocol lib
  clippy `-D warnings` 通过。最终当前树 workspace 全量 nextest `16248/16248`、既有 skip `17`（run
  `df479120-aab2-49cd-87b9-d3af075204f1`）；`cargo fmt --all --check`、`git diff --check`、workspace all-target clippy
  `-D warnings` 与 workspace release build 均通过。固定 `target/release/moli` SHA-256 为
  `32f7ebcaeb48ff7d428b1d52eb988a8fb0400d0381d952c223d9b27e13b5fa4b`；显式清除大小写 HTTP/HTTPS/ALL/FTP proxy、原
  no-proxy 与 inherited smoke group，并设置 `NO_PROXY=*` / `no_proxy=*` 后，CDP 默认全组 `251/251`、WebDriver
  Classic/BiDi/Selenium/Semantics `--continue-on-failure` `157/157`，两套均为 `ok=true`、失败列表为空；
- 这个 snapshot 模块落地时仍是 Phase 5 迁移形状：authoritative owner/snapshot 物理上由 `CdpConnection` 持有的
  `BrowserNavigationOwner` 提供，metadata transition 与 attachment/subscription 当时尚未拆开；该缺口由下一个模块记录闭环，owner 的
  物理 residence 则仍留给 Phase 6。

2026-08-05 Phase 5 top-level Target metadata fact 模块闭环记录：

- Core 新增 protocol-neutral `BrowserTargetMetadataTransition` 与 `BrowserFact::TargetMetadataChanged`。一个 loaded Document commit
  在 request、Page generation 和 history 都提交后，以同一原子 batch 发布
  `NavigationCommitted -> PageReplaced -> TargetMetadataChanged`；metadata 冻结 exact navigation、successor
  `PageResidenceIdentity`、URL 与 title，不含 CDP session、attached、discovery filter、tab facade 或 wire payload；
- named target reuse 只向 Browser Host 提交 auxiliary navigation intent，不再提前改 physical Target URL，也不在导航接受时发布 metadata
  fact。后继 Document 真正 commit 后才产生唯一 metadata occurrence。初始 Target navigation 与普通/renderer navigation 使用同一 producer，
  删除了 Browser Host completion 内根据物理 Target 猜测 `targetInfoChanged` 的 special case；
- 本地 Chromium `/home/donoughliu/chromium/src/out/Default/chrome`（source revision
  `a03603fe9af6230a12f1b2fb2c18a7d003a0d937`）在显式清理代理的本地 HTTP named-popup probe 中，等待第一次导航完全 settled 后，第二次
  `window.open('/two', 'named')` 的可观察顺序是 `Runtime.evaluate` response，随后恰好一条 popup
  `Target.targetInfoChanged(url=/two)`。因此 named-target selection 不是 metadata occurrence；未等待第一次导航的 probe 中 response 前出现的
  `/one` change 是前一次 Document 的延迟 commit，不能误归因给第二次 named reuse；
- 唯一 `CdpBrowserFactProjector` 按 exact navigation/Page claim metadata fact。独立 Target projector 先验证 Core snapshot 与 exact Page，
  再用 fact 中冻结的 URL/title 覆盖物理投影，并只在 frontend 边界 join attached、discovery/reporting policy 和 page/tab fan-out；旧 fact 在
  replacement 后 stale-drop，slow frontend 尚未 claim 的 predecessor metadata 会由 successor replacement/termination 退休，不占用 bounded
  pending window；
- attach/detach 只产生 frontend-owned attached-state delta，不写 Browser fact；shared/dedicated/service worker 的 metadata 继续由各自
  renderer/frontend lifecycle 投影，没有伪装成 top-level Browser transition。本模块没有新增 task、watcher、sleep、retry、poll、drain 或
  pump；
- Core/fact/projector/Chromium-order 聚焦回归 `17/17`（run `e2d44b55-2ac0-4415-8d96-e5c38e50d150`），Target metadata、attachment、
  auto-attach 与 named-popup 扩大矩阵 `183/183`（run `5af8b929-32c6-460b-b69b-fa3468261d49`），same-command/independent named
  navigation `3/3`（run `83926ae9-e660-4d0f-b7ec-c578936c21cd`）。最终树 workspace 全量 nextest `16254/16254`、既有 skip
  `17`（run `489d2680-51c1-4945-a439-cbd72cbb4b27`）；`cargo fmt --all --check`、`git diff --check`、workspace
  all-target clippy `-D warnings` 与 workspace release build 均通过。固定 `target/release/moli` SHA-256 为
  `4ace6a0fe991dce27b038b5775f573b146cc0a82b33e091093e58783d258da7e`；显式清除大小写 HTTP/HTTPS/ALL/FTP proxy、原
  no-proxy 与 inherited smoke group，并设置 `NO_PROXY=*` / `no_proxy=*` 后，CDP 默认全组 `251/251`、WebDriver
  Classic/BiDi/Selenium/Semantics `--continue-on-failure` `157/157`，两套均为 `ok=true`、失败列表为空；
- 提交后按约定执行 `git pull -r origin master`：`origin/master` 从 `93ceb0ed1f` 前进到 `08d0340f0c`，102 个分支提交全部
  重放，无 skip；七处 content conflict 按当前边界合并。上游把 CDP ingress 收紧为 validated `ParsedCdpCommand`、私有
  renderer policy 和 typed navigation token，因此内部合成的 Runtime payload 改用 `RendererCommandDescriptor::from_synthesized_payload`
  或先 parse 再建立 `Cmd` view，test-only command 改用 `Cmd::for_test`；Debugger cleanup 使用 exact canceled navigation terminal，
  Performance fixture 则先注册 Core Context/Target topology 再安装 Page。没有恢复 raw struct literal、旧 navigation clear 或绕过 Core
  authority 的 compatibility path；
- rebased tree 的跨边界聚焦组 `14/14`（run `0ff4f788-f884-42d9-939d-642e85c3ae4c`）。首次 workspace 全量为
  `16268/16269`（run `5aa98c33-484c-40bc-8907-9c758271ea9d`），唯一失败由上述 Performance fixture 确定性违反注册顺序造成；
  修正后 exact stress `20/20`（run `96cbb916-b125-43a9-bc50-a45c061e45d0`）、Performance 模块 `55/55`（run
  `33678d29-3856-416d-a26d-c25a45d1d50b`），最终 workspace 全量 `16269/16269`、既有 skip `17`（run
  `3f1987de-fbbd-432f-95b8-bbec44c00a95`）。`cargo fmt --all --check`、`git diff --check`、workspace all-target clippy
  `-D warnings` 与 workspace release build 均通过；release SHA-256 为
  `aa677bdc412e498fca340d73cd5cd059f08561f873f418712c1890a90abe5788`。显式清理大小写 HTTP/HTTPS/ALL proxy 并设置
  `NO_PROXY=*` / `no_proxy=*` 后，CDP 默认全组 `251/251`、WebDriver 四组 `157/157`，均为 `ok=true`；
- rebase 后曾在一次 CDP 全组的 routed popup `wait_for_load_state("load")` 抓到无 DCL/load 的挂起；初始独立 core 复跑为
  `8/10`，但增加 production trace 或失败后诊断会改变调度，后续明确清代理的 fresh-process core `50/50` 与 CDP 全组均通过。
  Chromium/renderer 路径审计排除了“403 DCL 为假”、projector pending 阻塞 renderer ingress 和重复 initial-navigation trigger；成功/失败
  wire 都已完成真实 Document commit，差异发生在 post-commit lifecycle tail。当前没有足够证据归因到 response-flush continuation、engine
  handoff 或 renderer owner，因此没有提交 sleep、retry、timeout 放宽或猜测性 production 修复；临时 Python 诊断已删除。这是仍需以
  exact Document/generation trace 捕获的残余风险，单次最终 smoke 绿色不能证明其消失；
- 这仍不是 Phase 5 exit：CDP projector 与 authoritative owner/journal 仍共同物理驻留 `CdpConnection`，root lifecycle causal join 和
  BiDi/Classic 的同源消费还需审计。下一步先做严格 Phase 5 exit audit，按未满足的不变量选择一个端到端模块；不再默认逐 command/wrapper
  扩展 fact plumbing。

2026-08-05 Phase 5 Browser-owned Document lifecycle wait 模块闭环记录：

- Phase 5 audit 确认 CDP navigation completion 与 BiDi/Classic/high-level page-load wait 仍通过
  `CdpConnection -> TargetPageSlot -> RendererDocumentLifecycleWaiter` 注册、轮询和显式释放 frontend-local waiter。它虽然不再拥有
  renderer progress，却让同一 DCL/load truth 分成“Browser fact occurrence”和“Protocol Page callback”两套消费路径；也无法在 bounded
  journal 已淘汰旧 occurrence 后可靠 bootstrap 新 waiter；
- Core 新增每个 live top-level Target 至多一条的 `BrowserDocumentLifecycleRegistry` current-state index。它只保存 exact
  `PageResidenceIdentity`、renderer Document/epoch、furthest milestone 和 termination，不保存协议事件、subscriber、physical Page 或 callback。
  occurrence journal 回答“发生过什么”，current-state index 回答“当前 exact Document 已经到哪一步”；`Started` 只选择 current snapshot，
  reached/terminated 仍由唯一 bounded journal 发布。replacement、带 predecessor 的 failure、Target crash/close 会退休旧 Page snapshot；
- `BrowserNavigationOwner::capture_document_lifecycle_wait` 在一个 Core turn 内验证 exact current Page generation、pending replacement 和
  current snapshot，再从同一个 journal cut 建立双 cursor 的 move-only ticket。已达到或已终止的 Document 立即得到 typed terminal；旧
  Page 或已有 queued cross-Document navigation 得到 `Superseded`；否则后续 DCL/load/termination/replacement/Target terminal 从公共 fact
  stream 决定结果。ticket 构造器收为 Core-private，Protocol 不能自行拼 cursor 或伪造 Browser authority；
- 删除 `TargetPageSlot` 的 waiter id、registry、event callback loop、outcome polling 与 release API。`DevToolsDocumentLifecycleWaitKey`
  现在只冻结 frontend route 解析得到的 frame/loader/Document identity，并 move-own Core ticket；scheduler、CDP、BiDi 与 Classic 只读取 ticket
  state，drop 即释放 subscriber。exact CDP lifecycle event matching、response visibility barrier，以及
  `Page.setLifecycleEventsEnabled` 对 frontend-visible current snapshot 的 replay 都保留在 Protocol projection，因为它们决定 wire visibility，
  不拥有 Browser lifecycle truth；
- 本模块没有新增 task、watcher、sleep、retry、轮询、drain 或 pump。current snapshot/journal eviction、future termination、successor
  replacement、CDP same-Document wait、BiDi interactive/complete wait 与 Classic page-load strategy 的跨层矩阵 `217/217`（run
  `6415ad9c-c40b-4220-bb2a-126b1708f0a0`）；其中 7 条关键路径按 nextest 原生 stress 连续 20 轮、共 `140/140` 通过（run
  `adfb5f19-be4b-4be8-bd83-b001c6a36c70`）。最终当前树 workspace 全量 nextest `16273/16273`、既有 skip `17`（run
  `b353b357-4afc-44d7-91b8-1188b105c924`）；`cargo fmt --all --check`、`git diff --check`、workspace all-target clippy
  `-D warnings` 与 workspace release build 均通过，release SHA-256 为
  `8892c61dcbe3a0fc605c0c55093fe0b98567f453637af87c7e137f977eedc867`。显式清理大小写 HTTP/HTTPS/ALL/FTP proxy、原 smoke
  group，并设置 `NO_PROXY=*` / `no_proxy=*` 后，CDP 默认全组 `251/251`、WebDriver Classic/BiDi/Selenium/Semantics
  `--continue-on-failure` `157/157`，两套均为 `ok=true`、失败列表为空；
- 这仍不是 Phase 5 exit：root lifecycle producer/projector 的 same-turn causal join、其余 BiDi/Classic navigation/target consumer audit，
  以及 owner/journal 从 `CdpConnection` 的物理迁出仍未完成；最后一项属于 Phase 6。

2026-08-05 Phase 5 root lifecycle fact/output causal link 模块闭环记录：

- exit audit 确认上一模块仍有一处不必要耦合：renderer ingress 向 Core 发布 exact lifecycle fact 后，会在同一个
  `CdpConnection` turn 立即调用 `CdpBrowserFactProjector::capture_available(...)`。这个调用不阻塞 Browser Owner，却让 producer
  负责推进 frontend 的唯一 journal cursor；application fact wake 与 renderer visible output 因而不是两个真正独立的 consumer 时机；
- producer 现在只登记 `BrowserFactSequence -> CommittedRendererDocumentBinding` causal link。link 冻结 exact
  `PageResidenceIdentity` 对应的 attachment、renderer frame/Document、navigation token 与 loader attribution；登记时验证 fact 的 exact
  Page/Document，重复同一 link 幂等，冲突或错配使该 frontend projector 进入 typed terminal error。fact 仍是生命周期 occurrence 的唯一
  authority，link 只回答“这个 occurrence 应投影到哪个已冻结 frontend binding”，不反向修改 Browser state；
- link 登记不读取 subscriber、不推进 cursor。payload-free application wake 可以先把 fact 捕获为 pending，随后 renderer visible record
  再按同一个 sequence/binding/stamp claim；visible record 也可以先触发独立 capture。attachment replacement 后的新 binding 不能领取旧
  Document fact，`document.open()` 已完成 epoch 仍沿用当前 root-Document binding 的既有可观察语义；
- causal-link 表最多保留与 bounded journal 相同的 `1024` sequence window；超出可达窗口的 link 会被裁掉，subscriber lag 仍是 typed
  terminal error。它不复制 fact payload、不唤醒或推进 Browser，也没有新增 task、watcher、sleep、retry、poll、drain 或 pump，因此不是第二条
  fact/event queue；
- projector 聚焦回归 `12/12`（run `dfb52549-3175-4630-8846-6136662f2de4`）；覆盖 Core lifecycle、Page slot、main-Document
  commit、CDP lifecycle、BiDi interactive/complete 与 Classic page-load strategy 的跨层矩阵 `236/236`（run
  `d3b24a1a-b0ff-4fdb-a59e-cb574a746135`）；6 条 wake/replacement/epoch/commit/BiDi 关键路径按 nextest 原生 stress 连续 20 轮、共
  `120/120` 通过（run `d8743f85-5cfc-4fef-ac06-60f3b367b9ae`）；
- 最终当前树 workspace 全量 nextest `16273/16273`、既有 skip `17`（run
  `3658d588-6464-4022-aeee-92942b9f595b`）；`cargo fmt --all --check`、`git diff --check`、workspace all-target clippy
  `-D warnings` 与 workspace release build 均通过，release SHA-256 为
  `06e57c0845b4b993952c25eafde4a1bfe5fcee86c6acfee1c4e916b0d84ed6f8`。显式清理大小写 HTTP/HTTPS/ALL/FTP proxy、原
  no-proxy 与 inherited smoke group，并设置 `NO_PROXY=*` / `no_proxy=*` 后，CDP 默认全组 `251/251`、WebDriver
  Classic/BiDi/Selenium/Semantics `--continue-on-failure` `157/157`，两套均为 `ok=true`、失败列表为空；
- 这仍不是 Phase 5 exit。`moli/src/protocol_server/webdriver_bidi.rs` 的
  `push_initial_load_events_for_script_created_targets` / `initial_load_events_for_target_created_message` 仍把
  `Target.targetCreated` 直接扩写成 `AutomationEvent::NavigationFrame` 和 `AutomationEvent::Load`；该 load 没有 exact
  Document/milestone/stamp fact，关闭或改变 Target frontend projection 仍可能改变 BiDi lifecycle trace。下一模块应先为 script-created
  target 建立同源 lifecycle bootstrap/consumer，再继续审计剩余 BiDi/Classic outcome；authoritative owner/journal 从
  `CdpConnection` 的物理迁出明确留给 Phase 6。

2026-08-05 Phase 5 BiDi script-created Target lifecycle consumer 模块闭环记录：

- 本地 Chromium/ChromeDriver `147.0.7709.0` 在显式清代理的 HTTP popup probe 中输出一条
  `contextCreated(url=about:blank)`，随后按同一非空 navigation UUID 输出一条 DCL 和一条 load，二者 timestamp 非零；没有从 Target creation
  推断 load。当前 Moli 对同一场景则先输出 `load(navigation=null)`，再输出共享真实 loader navigation 的 DCL/load。首条 load 由
  `Target.targetCreated` 的 URL 合成，不对应任何 Document/milestone/stamp，造成同一 Document 两次 load，并把假 load 排在真实 DCL 前；
- 删除 `push_initial_load_events_for_script_created_targets` 与
  `initial_load_events_for_target_created_message`，不再把 Target frontend message 扩写成 `NavigationFrame + Load`。script-created popup 的
  DCL/load 只来自 `domains/page/lifecycle.rs`：renderer ingress 发布 exact `DocumentLifecycleReached`，统一 projector claim sequence 与冻结
  Document binding 后生成 `AutomationEvent::DomContentLoaded/Load`，BiDi frontend 再做 subscription/context mapping；
- 临时 Target discovery 现在只服务 `browsingContext.contextCreated` subscription。仅订阅 DCL/load 的 BiDi connection 不再改变 Target
  discovery state，也不需要 Target event 才能观察 popup lifecycle。既有 command-response 后 frontend navigation delivery wait 仍用于把已完成
  的 fact-authorized sidecar 送上该 socket；它没有新增 Browser owner action、task、watcher、sleep、retry、poll、drain 或 pump，也不把
  frontend 订阅变回 Browser progress 条件；
- 新回归先在旧实现上确定性失败：收到 synthetic load、真实 DCL、真实 load 共三条（run
  `a73761dd-c019-435f-9f67-0dfd6265cebe`）；删除推断后 exact case 通过（run
  `6c85e1e1-a882-4750-8806-713788b6d0c2`）。BiDi lifecycle/subscription/WPT viewport 与 CDP popup 扩大矩阵 `78/78`（run
  `7a20593c-5914-421b-9d1b-b8c8a6597abe`）；exact BiDi、viewport 和 CDP popup 三条路径连续 20 轮、共 `60/60` 通过（run
  `23c3c912-a8e7-4e72-9f07-3b125a2cc9c0`）；
- workspace 首轮 nextest 为 `16273/16274`、既有 skip `17`（run
  `3c867449-d670-4214-88a4-f68cf9f3e7b4`），唯一失败是本模块未修改的 renderer/OPFS 用例
  `sandboxed_blob_iframe_keeps_opaque_storage_context_for_opfs_messages`，结果停在 `"pending"`。该用例在历史全量中已有同型单次失败记录；本轮 exact
  `--stress-count 20 --flaky-result fail` 为 `20/20` 通过（run `47493d76-7bab-48c3-8440-9e36995f60ae`）。首轮执行时另一个 checkout
  也在跑 workspace nextest，存在资源争用但不足以证明根因；未修改 OPFS/structured-clone、未增加 timeout、retry 或放宽断言。无该外部争用后的
  workspace 最终 nextest 为 `16274/16274`、既有 skip `17`（run `b1f7a9e4-e13e-4ca7-ad17-284fe66d3f81`）；
- `cargo fmt --all`、`cargo fmt --all --check`、`git diff --check`、workspace all-target clippy `-D warnings` 与 workspace release build
  均通过。rebase 前固定 `target/release/moli` SHA-256 为
  `f47b60918fb24964935ad237c9729222e1430d055873bac264cf9fa7763a346a`。显式清除大小写 HTTP/HTTPS/ALL/FTP proxy、原
  no-proxy 与 inherited smoke group，并设置 `NO_PROXY=*` / `no_proxy=*` 后，CDP 默认全组 `251/251`、WebDriver
  Classic/BiDi/Selenium/Semantics `157/157`，两套均为 `ok=true`、失败列表为空；
- 提交后执行 `git pull -r origin master`，上游从 `08d0340f0c` 前进到 `815b44cbf0`，新增 protocol TCP keepalive、CDP DOM
  snapshot smoke 同步和 WebDriver smoke 失败诊断；105 个分支提交无冲突重放，本模块 rebase 前后 stable patch-id 均为
  `f80cbbbf460ff98a45fe37d4155bda4020a6262e`。当前树 exact popup lifecycle 回归 `1/1` 通过（run
  `6e4940b7-f23f-45e6-bf1f-6122750528e9`），fmt/diff check 与 workspace release build 通过，release SHA-256 更新为
  `a429aabfdf8913c90fc7bf5204ad8b037b92f0a4e8653b5329e08fd3565e2a05`；同样清代理后 CDP `251/251`、WebDriver
  `157/157` 再次通过。未在等价模块 patch 上重复 workspace 全量与 clippy；上面的全量/clippy 是 rebase 前同一模块 patch 的门禁，rebase 后以 exact
  交叉点、重建 release 和两套完整 smoke 覆盖新增 master integration；
- 这仍不是 Phase 5 exit：显式 `browsingContext.create` 成功后仍重新查询 physical TargetInfo 并构造
  `AutomationEvent::TargetCreated`，显式 close 仍用 command 前保存的 TargetInfo 与成功结果构造 `TargetDestroyed`；二者没有把 Protocol 已 claim
  的 exact Target creation/terminal fact 带到 BiDi projection。下一模块应把显式 create/close 的 Target lifecycle sidecar 收敛到同一 fact
  projection，同时检查 Chromium live `contextCreated(url=about:blank)` 与当前 Moli requested-URL payload 的独立兼容性差异；owner/journal
  从 `CdpConnection` 的物理迁出仍属于 Phase 6。

2026-08-05 Phase 5 显式 BiDi Target lifecycle consumer 模块闭环记录：

- 显式 `Target.createTarget` 的 physical Target/Page-slot commit 已由既有 `BrowserFact::TargetCreated` 唯一授权。本模块让 fact claim 后的
  projection 在 BiDi lifecycle projector 已注册时，无论 CDP discovery 是否开启都发布恰好一个 protocol-neutral
  `AutomationEvent::TargetCreated`，并携带 committed
  `DevToolsTargetInfo` 快照。`Target.targetCreated` 的 page/tab、filter 和 owner-session fan-out 仍由 CDP frontend 决定，但这些 wire
  notification 在 BiDi projector 存在时显式关闭 automation sidecar，不能再按 discovery owner 数量重复生成 BiDi occurrence；没有
  BiDi frontend 时保持既有 CDP typed event，不额外构造 neutral projection；popup 使用同一形状；
- top-level Target close 在 prepare 阶段冻结 exact page TargetInfo，Browser Host 提交 `TargetClosed` 后才允许 terminal projector 生成恰好一个
  neutral `AutomationEvent::TargetDestroyed`。CDP `Target.targetDestroyed` fan-out 同样变为 wire-only；Target attachment/detachment 和 worker
  lifecycle 仍保留各自原有 typed sidecar。BrowserContext disposal 的既有 fact-gated frozen lifecycle prefix 保留，但其 top-level CDP
  destroyed fan-out 也不再复制 automation occurrence；
- 删除 BiDi command adapter 在 create 成功后执行 `GetTargetInfo`、按 command result/fallback 合成 `TargetCreated`，以及 close 前查询并保存
  `TargetInfo`、按成功结果合成 `TargetDestroyed` 的路径。现在 Browser transition 是 occurrence authority；command result 只决定 frontend
  response，不再证明 browsing-context lifecycle 已发生；
- create fact 与 command completion 在同一 turn 可用，因此 exact `contextCreated` 先投影、随后发送 create response。close 只负责向
  Browser Owner mailbox 提交 action，真正 terminal fact 在后续 owner turn 到达；BiDi socket actor 因而只暂存带 channel 的 close response 和
  exact target id，立即返回 application loop。Browser Host 自主推进后，输出路由先按 TargetDestroyed sidecar 投影/发送
  `contextDestroyed`，再释放匹配 target 的 response。未订阅该事件时仍按 sidecar 解锁 response；不相关 Target 的 terminal fact 不能误释放，
  多条 pending close 按 exact target id 分别结算；
- 这条 frontend response residence 不拥有 Browser progress，也没有新增 task、watcher、sleep、retry、poll、drain 或 pump。socket 断开只会
  丢弃尚未发送的 frontend response；Browser Host 与 frontend/application lifetime 的物理解耦仍是 Phase 6，不能用本模块宣称 disconnect 后的
  Host lifetime 已完成。当前 projector registration 是 `CdpConnection` 上的 connection-local 迁移状态，由 BiDi actor attach/release 显式
  开关；Phase 6 应随通用 frontend handle/subscription 移出 connection，而不是把该布尔量扩散成 domain policy；
- 红灯回归先证明 discovery 关闭时显式 create/close 的 typed lifecycle 均为 `0`；修正后 Protocol exact create/close、CDP-discovery 去重、
  popup discovery/no-discovery、Context disposal 与 BiDi create/close ordering 聚焦矩阵通过。红灯 run 为
  `bbd39c49-89ce-41e8-a811-5d16bdec323b`；扩大后的 Protocol Target 矩阵 `688/688`（run
  `55a8479f-9b9a-4e3e-ac3e-9628cd58d551`），BiDi 模块与 exact close-response helper `129/129`（run
  `b2d3b0c4-2c6f-4907-b142-50e278a5b82f`）；
- 最终 workspace nextest 一轮 `16277/16277`、既有 skip `17`（run
  `42fcfcff-0de4-4dfc-bb48-01f837858f1f`）。`cargo fmt --all --check`、`git diff --check`、workspace all-target
  clippy `-D warnings` 与 workspace release build 均通过；固定 `target/release/moli` SHA-256 为
  `0aa46477cd776f7e5e6c59b9adca2f7354b10a6277d9d5ff228e82e5b49f19f3`。显式清除大小写 HTTP/HTTPS/ALL/FTP
  proxy、原 no-proxy 与 inherited smoke group，并设置 `NO_PROXY=*` / `no_proxy=*` 后，CDP 默认全组 `251/251`、WebDriver
  Classic/BiDi/Selenium/Semantics `--continue-on-failure` `157/157`，两套均为 `ok=true`、失败列表为空；
- 提交后执行 `git pull -r origin master`，上游从 `815b44cbf0` 前进到 `abdb5e8cc4`。106 个分支提交只有旧 Target-topology
  提交与上游新增 Page/Network listener 测试装配发生一处冲突；合并结果同时保留 Core Target registry 注册和主/辅助 session-owned
  Network listener。排除本条事后记录的 production/test patch 在 rebase 前后 stable patch-id 均为
  `81a07b599c17806de02c69600be87747388b8cfc`；交叉点两例与 exact
  create/close 三例合计 `5/5` 通过（run `1f63d7df-4c3e-48fa-9098-30bf2eb07e14`），fmt/diff check 和 workspace release
  build 通过，release SHA-256 更新为 `38ef645268149d02a45b7fbbcf95dc517119c354220755bbdecf1d0390a5f75a`。同样清代理后，包含上游
  新增 case 的 CDP 默认全组 `252/252`、WebDriver 四组 `157/157`，均为 `ok=true`、失败列表为空。未在等价模块 patch 上重复
  workspace 全量与 clippy；rebase 前同一 stable patch 已通过两项门禁，rebase 后以 exact 交叉点、重建 release 和两套完整 smoke 覆盖
  master integration。

2026-08-05 Phase 5 BiDi deferred navigation outcome 模块闭环记录：

- 此前 `browsingContext.navigate(wait=interactive|complete)` 遇到 Fetch request/response/auth pause 时，会暂存 BiDi response；后续
  `network.continue*` 结束暂停后，adapter 先读 `BackgroundCommandResponsePayload`，读不到再按相同 command id 扫原始 CDP
  `{"id","result"|"error"}`。这让 CDP wire JSON 事实上成为第二个 navigation completion authority；任意同 id 原始消息都可能提前
  结算 BiDi pending response，且 early success 与 paused terminal 使用两套推断路径；
- `BackgroundCommandResponseEvent` 现在可携带不参与 JSON serialization 的 exact
  `BrowserNavigateCommandOutcome` sidecar。background early-success FIFO 在投影 `Page.navigate` response 时保留同一个 neutral outcome；
  Fetch decision 的 terminal plan 则冻结原始 requested URL，先分离 neutral outcome 与 frontend projection，再把 outcome 绑定到原
  navigation command id 的 response envelope。`network.continue*` 自己的 response 仍独立使用外层 command id，不能误领该 sidecar；
- `BrowserNavigateResponseProjectionShape` 记录原 response 是否实际包含 `frameId`、`loaderId`、`errorText`、`isDownload`。BiDi
  `navigation-{loaderId}` 在分离边界恢复 neutral loader identity，但 wire projection 保留原来的 `navigation`/`url` shape，不额外泄漏
  CDP 字段。BiDi pending consumer 只按 exact command id 读取 `BrowserNavigateCommandOutcome::{Completed,Rejected}`；旧的 typed CDP
  payload parser、raw protocol-message fallback 和 result-shape猜测函数均已删除；
- 这条 sidecar 只随既有有界 output FIFO 前进，不新增 task、watcher、sleep、retry、poll、drain 或 pump，也不参与 Browser Owner
  调度。frontend 慢或断开只影响自身 pending response 是否还能发送，不能改变已经接受的 navigation；
- 本地 Chromium checkout 的 WPT
  `third_party/blink/web_tests/external/wpt/webdriver/tests/bidi/network/continue_with_auth/action.py::test_cancel`
  明确要求 auth cancel 后完成唯一 401 response。因此端到端回归保持“401 Document 正常完成 navigation”的语义，而不是把 cancel
  制造成 navigation error；provideCredentials 则继续要求 200 Document 与非空 navigation id；
- 红灯先证明旧实现会用 raw CDP error 结算 pending BiDi response（run
  `417f480c-2929-4828-bb08-89181ba2a38e`）。修正后 early success、BiDi wire-shape round-trip、paused rejection sidecar、typed-only
  consumer、raw-ignore，以及 auth cancel/provideCredentials 两条 WebSocket 路径共 `7/7` 通过（run
  `28e5a08a-7ee1-442b-bd53-fec63ba6f291`）；
- 最终 workspace nextest 一轮 `16292/16292`、既有 skip `17`（run
  `42fb1e85-967b-4d74-a468-9b558302c3c0`）。`cargo fmt --all --check`、`git diff --check`、workspace all-target
  clippy `-D warnings` 与 workspace release build 均通过；固定 `target/release/moli` SHA-256 为
  `86531502df70c3871a279c3f27e514857e8f948d00a6d8da5dc3bbb47d67709c`。显式清除大小写 HTTP/HTTPS/ALL/FTP
  proxy、原 no-proxy 与 inherited smoke group，并设置 `NO_PROXY=*` / `no_proxy=*` 后，CDP 默认全组 `252/252`、WebDriver
  Classic/BiDi/Selenium/Semantics `--continue-on-failure` `157/157`，两套均为 `ok=true`、失败列表为空；
- 提交后执行 `git pull -r origin master`，上游从 `abdb5e8cc4` 前进到 `c7baec54b8`，新增 layoutless page-side point query
  的 empty-result 语义及对应 CDP smoke；107 个分支提交无冲突重放。排除本条事后证据的 module patch stable patch-id 前后均为
  `78715685b9ca7747670af13f6ac5c477956e75f9`。上述 7 条 outcome 路径与上游 3 条 point-query/CDP hit-test 交叉点合计
  `10/10` 通过（run `9e66a3fe-e422-47ee-bbde-12b8cbcd079c`），fmt/diff check 与 workspace release build 通过，release
  SHA-256 更新为 `8bf3107f836cad88718287d83de08e9b7542e95a19c800e6698287d2e7deff8e`；同样清代理后，CDP 默认全组
  `252/252`、WebDriver 四组 `157/157` 再次通过。未在等价 module patch 上重复 workspace 全量与 clippy；rebase 前同一 stable
  patch 已通过两项门禁，rebase 后以 exact 交叉点、重建 release 和两套完整 smoke 覆盖新增 master integration；
- 最终同步时再次执行 `git pull -r origin master`，上游从 `c7baec54b8` 前进到 `031fa2c39f`，新增 base URL/小型
  parsing allocation 与 shadow-slot index 的 DOM CPU 优化及 profile 文档；107 个分支提交仍无冲突重放。排除本条事后证据的
  module patch stable patch-id 前后均为 `18f690a5461ff6cce4d12a91acbfefb49d5aaeec`。7 条 outcome 路径与 5 条上游
  DOM 交叉点合计 `12/12` 通过（run `dc757f65-9881-4576-9684-2bf1fb94f141`）；workspace release build 通过，release
  SHA-256 更新为 `3b832d3a5a438192cded7e463901bfc5ebb9bea3f77f00e64d33de98205b080c`。同样清代理后，CDP 默认全组
  `252/252`、WebDriver 四组 `157/157` 再次为 `ok=true`、失败列表为空；
- 本模块落地时尚未单独完成 Phase 5 exit audit：neutral outcome 的类型和 navigation authority 已属于 Core，但 response
  correlation/sidecar 仍物理驻留 Protocol output envelope。下述 exit audit 已确认没有其他 frontend 从 wire payload 反推
  Browser outcome；该物理 transport residence 转入 Phase 6，不把 sidecar 扩散成各 domain 的自定义 channel。

2026-08-05 Phase 5 exit audit：

- producer inventory 确认 Target、navigation、Page replacement、exact lifecycle 与 terminal facts 只从 Core authoritative
  transition 发布；Core 不依赖 `BackgroundProtocolEvent`、session subscription 或 socket flush；
- consumer inventory 确认 CDP 由单 cursor projector claim fact，BiDi/Classic/high-level wait 使用 exact ticket 或 neutral
  outcome；剩余 JSON 解析属于 renderer Runtime 或 frontend command shape，不承担 Browser 完成判断；
- journal 的 retained bound、subscriber cursor、显式 `Lagged`、stale Page retirement 与 response-visible sequence 已覆盖慢订阅、
  replacement 和 DCL/load causal join；frontend 断开只丢失自身投影；
- 因此本阶段 exit gate 已满足。journal/projector 的共同物理 residence 和 Browser Host 的应用级 lifetime 明确转入 Phase 6，
  不再用“清除 Protocol 内所有 await”扩大 Phase 5。

Exit gate：Browser Core transition 不构造 `BackgroundProtocolEvent`；CDP event 是否发出由 frontend
subscription 决定，但关闭 subscription 不改变 browser trace。

### Phase 6：Browser Host lifetime 与 browser state 完整提取

目标：Browser Host 不再从属于单个 frontend connection。

状态：已完成。profile/storage/cookie owner、Browser Host registry/engine residence、browser-global behavior policy、physical
renderer Page lifetime owner、Page command/cache payload、per-context renderer/network runtime root、Host executor authority、download
policy/registry、Host identity namespace、network request/body owner 与 Target `sessionStorage` association 已按端到端模块进入
application-owned Browser Host。actual socket frontend 已由 `CdpFrontendEndpoint/Router/Receivers` 独立表示；owner-task-resident
`DevToolsHostAdapter` 负责 renderer/DevTools projection，并不随任一 socket attach/detach 创建或销毁。

Phase 6 最终 audit 修正了一个过期前提：legacy 名为 `CdpConnection` 的值不是一个 socket frontend connection，而是共享 owner task
内的 DevToolsAgentHost 类 adapter。`BrowserContext`/Target 中剩余的 renderer channel、Inspector session、Network/Log/Runtime cursor
与 output projection 不应仅因位于 Protocol 就搬进 Browser Core。Phase 6 的 exit gate 以 authority 和 lifetime 为准，而不是要求
Protocol projection 容器归零；多 frontend 共用同一 Browser Host、以及每种 frontend 的 projection 收敛属于 Phase 7。

按 owner inventory 分批迁移：

1. BrowserContext/Target/Page authoritative registry；
2. profile/storage partition/cookie owner；
3. network condition、cache、headers、request/body stores 的 browser portion（global policy 与 request/body store 已完成）；
4. download registry/global IO 的 browser portion；
5. permission/emulation 中真正影响浏览器行为的 state（browser-global policy 已完成，context/Target runtime projection 随第 1 项迁移）。

2026-08-05 profile/storage/cookie owner 模块：

- application 在接受协议 frontend 前创建唯一 `StoragePartitionState`；default profile BrowserContext 通过
  `StoragePartitionSharedStorageHandles` 共享同一个 live cookie store。raw CDP、standalone BiDi 与 Classic 的读、写、删除立即
  观察同一真值；ephemeral BrowserContext 仍使用独立 memory partition；
- 删除 `SharedCookieProfile`、`CookieProfileCommit`、`CdpCookieSnapshot`、connection-local profile snapshot、
  `commit_cookie_delta` 及 teardown merge。frontend/owner 结束只发送 payload-free flush request，不能把旧视图写回 Browser state；
- `StoragePartitionState::flush` 在 application-owned partition 内串行化“取当前快照 + 写文件”，避免多个生命周期 checkpoint
  并发时较旧文件写入最后完成。flush 是持久化边界，不持有 Browser Owner、不推进 navigation，也没有新增 drain/pump；
- typed command 回归覆盖两个 `CdpConnection` 的 live Set/Get/Delete；服务级回归覆盖两个并发 BiDi frontend 立即互见，以及旧
  frontend 先关闭时仍只落盘较新的 Browser-owned 值；既有 profile restart、delete persistence 和 ephemeral isolation 用例保留；
- 聚焦 owner/live/persistence 回归 `6/6` 通过（runs `1d7dce32-7236-4db5-af6b-b5fcef765f6c`、
  `5982026d-1c7b-429f-bf44-50146091de12`）。workspace nextest `16289/16289`、既有 skip `17`（run
  `701a8887-d69e-4b14-8d6c-208ecab6524d`），fmt、diff check、workspace all-target clippy `-D warnings` 和 workspace
  release build 均通过；`target/release/moli` SHA-256 为
  `95fb65b3a3db319b466d03645dc323ee8fc8a4aa9360da36bfc4fd5454bc1cfc`。固定该 binary 并显式清除大小写
  HTTP/HTTPS/ALL/FTP proxy、继承 smoke group，设置 `NO_PROXY=*` / `no_proxy=*` 后，CDP 默认全组
  `252/252`、WebDriver Classic/BiDi/Selenium/Semantics `--continue-on-failure` `157/157`，两套均为
  `ok=true`、失败列表为空；
- 提交后按约定执行 `git pull -r origin master`，上游从 `031fa2c39f` 前进到 `d7fb86b60e`，新增
  awaitPromise realm-replacement 回归与 Inspector scoped-microtask 修复；108 个分支提交无冲突重放，本模块 stable
  patch-id 前后均为 `42903c7ea4240981738a5fa5649ac213cfeb3318`。本模块 6 条 owner/storage 用例与上游 3 条
  Inspector/realm 用例交叉验证 `9/9` 通过（run `ba67dcf6-58d4-449a-a2b5-cdb380b13c9a`）；fmt/diff check 与
  workspace release build 通过，最终 binary SHA-256 为
  `102b97bb2020a343afd9f2e94eef84d49ef700cb199d3db6bd4ecf9c0a61cd67`。同样清代理后，CDP 默认全组
  `252/252`、WebDriver 四组 `157/157` 再次为 `ok=true`、失败列表为空。没有在 stable module patch 上重复
  workspace 全量与 clippy；rebase 前同一 patch 已通过两项门禁，rebase 后以 exact 交叉点、release 重建和两套完整
  外部 smoke 覆盖新增 master integration；
- 这完成了 Phase 6 inventory 的第 2 项，但不是 Phase 6 exit。后续模块必须按完整 Browser Host
  lifetime/registry 闭环提取，不能退回逐字段 wrapper 迁移。

2026-08-05 Browser Host registry/engine residence 模块：

- Core 新增 `BrowserHostState`，把 `BrowserNavigationOwner`、authoritative BrowserContext/Target/Page-generation
  registry、selected/retained `NavigationEngine` 和 Browser fact journal 放进同一个 current-thread Host allocation。
  这里有意使用 `Rc<RefCell<_>>`：V8/renderer owner 当前只允许在 dedicated local runtime 上运行，使用
  `Arc<Mutex<_>>` 会虚构并不存在的跨线程能力；
- application composition root 先按实际 fetch/resource 配置创建 `BrowserHostState`，再创建
  `CdpConnection` adapter。`BrowserHostActor` 必须接收并持有这一个 state，owner queue 与 authoritative state
  因而属于同一个 Host 组件；没有 state 的第二种 actor 构造路径已经删除；
- `CdpConnection` 删除内嵌的 `BrowserNavigationOwner` 值，只保存访问同一 residence 的迁移期 capability。
  fact projector 也从该 residence 建立自己的 subscriber cursor；所有 registry/history/replacement/termination/engine
  transaction 均访问同一个 owner，没有复制 shadow registry、第二条 queue 或新的 drain/pump；
- data/inline/buffered/streaming/network-response Page 构建和 resource-runtime rebuild 不把
  `RefMut<BrowserNavigationOwner>` 带过 `await`。target/session-owned detached load 在同步 owner turn 内捕获
  move-owned `NavigationEngine`，等待完成后随 typed navigation outcome 进入既有 exact adoption boundary；active/unbound
  setup load 则克隆 exact selected engine 的 renderer capability，由 authoritative selected engine 继续保持 runtime lifetime，
  不把每次新建的 storage-input `RendererBrowserContextRuntime` 误作 renderer owner，也不返回调用者不可见的隐藏 handoff。
  无 navigation outcome 的 direct fetch/rebuild wrapper 在等待后显式归还 engine。targeted all-target clippy 以
  `clippy::await-holding-refcell-ref` 作为结构门禁通过；
- Core 回归证明 actor 持有 application 创建的 exact Host residence；Protocol 回归在注册 default
  BrowserContext/Target 后销毁整个 `CdpConnection`，随后仍可从 application-owned residence 读取同一个
  `BrowserInstanceId` 和完整 registry。聚焦用例通过（runs `63ffcb43-af28-41c9-9343-0dee4ee31402`、
  `e7470f58-7a37-4bf1-a09d-1fed28db12cc`）；
- 首轮 workspace 全量暴露 7 条 owner-route/storage Protocol 回归：临时 engine 被当作 authoritative owner，或
  buffered setup 返回隐藏 engine 后随部分 move 被 drop，导致旧 Page 的 render-runtime 关闭。修复后 7 条 exact
  回归连续 `10/10` 轮全部通过（run `d3527df6-b57c-41e5-9ba2-b0aa504f9d9a`）。同轮唯一 Renderer
  structured-clone 失败单独及 `20/20` stress 均通过（runs `201f6d3b-518e-4437-89b5-b0d79f73e072`、
  `c43c36ee-1265-46e4-9a3b-4cd71d2dc88d`），未用 retry/sleep 或削弱断言处理；最终 workspace nextest
  `16295/16295`、既有 skip `17`（run `e63f8434-26d3-4dbc-ac65-705b66029930`）；
- `cargo fmt --all --check`、`git diff --check`、workspace all-target clippy `-D warnings` 与 workspace release build
  均通过；固定 `target/release/moli` SHA-256 为
  `657278b75a92cc37df38b1171f7cc97c8fc4f46f3807fc72a7191c29a4962402`。显式清除大小写
  HTTP/HTTPS/ALL/FTP proxy、设置 `NO_PROXY=*` / `no_proxy=*` 并固定该 binary 后，CDP 默认全组
  `252/252`、WebDriver Classic/BiDi/Selenium/Semantics `--continue-on-failure` `157/157`，两套均为
  `ok=true`、失败列表为空；
- 随后按约定执行 `git pull -r origin master`，109 个分支提交重放到 `f22f23462b89`。上游 structured resource-runtime
  shutdown 要求先停止 renderer producers、再释放 Page/engine lease，最后关闭 fetch admission 并 join network owner。初版 rebase
  integration 错把“取走 navigation owner”放在 `CdpConnection::drop`，workspace run
  `10c50dcd-c853-452c-9318-37419dfe3f56` 的 exact Host-lifetime 回归证明 frontend drop 会提前终止 Host。最终边界改为：
  retiring adapter 释放 physical frontend Context projection，并把 renderer runtime roots 转交 `BrowserHostState`；它不再停止
  Browser-owned Page 的 renderer producers，最后一个 Host residence drop 时才终止 producers、释放 authoritative navigation
  owner/engines 并 join roots。WebSocket owner-registry shutdown `2/2`、frontend drop
  后 registry 可读回归均通过；`moli-protocol/test-support` 同步传递 Core test support，避免依赖构建绕过同一装配路径；
- rebase 后全量 run `028ea487-3049-4046-a6a3-2990f9f56d1e` 为 `16317/16318`，唯一失败不是 Browser/CDP 路径：
  `StreamingRawResponse` 析构时 completion receiver 可先唤醒 reaper，而 exact runtime lifetime lease 尚未按字段顺序释放。修复由 response
  自己在 `Drop` 中显式先释放 lease，并新增不依赖调度的析构顺序回归；新回归 `20/20`（run
  `3697830d-686e-408a-bdf7-ff903ed2f77c`）、原失败用例 `100/100`（run
  `75a492c2-4155-4031-b3fb-0dc5ef62261c`）。`f22f23462b89` 组合树 workspace nextest `16319/16319`、既有 skip `17`（run
  `34220ede-79d3-48a9-b0b3-3100a84bcca2`），fmt、workspace all-target clippy `-D warnings` 与 release build 通过；该树 binary
  SHA-256 为 `09559e50e21e63003a596cfe7079672684acbf429e09b93474aa4c92740f4e03`；
- 首轮 release CDP 全组暴露 `dom-shadow-outer-html` 偶发在主 Document DCL 后读取不到已提交 child Document；带相邻 DOM
  组复跑为 `1/10` 失败。`~/chromium/src/out/Default/chrome` 147 的 gated-iframe probe 证明：child request 已发出时主 DCL 可以已经发生，
  iframe 此时仍投影 `about:blank`，目标 child node 尚不存在。因此该 outerHTML 用例改为等待它实际需要的 `load`，不是放宽 Browser
  lifecycle；相邻组修正后 `20/20`。清代理后的 CDP 默认全组最终 `252/252`、WebDriver 四组 `157/157`，均为 `ok=true`、失败列表为空；
- 同一 release、无代理的知乎 answer live 复核仍显示书面边界：`done` 约 0.18 秒返回 720-byte `zh-zse-ck` challenge Document，
  `domstable` 约 10.2 秒后得到约 393 KiB successor DOM，同时记录 challenge fetch 的 CORS preflight rejection。该 navigation 由未来
  timer/network work 产生，不属于 exact DCL/load turn 的 `FollowBeforeReply`；它仍是高层 wait policy 与站点 WebAPI 兼容问题，不能据此
  把 403 DCL 判假，也不能把 Phase 6 Host residence 改造成隐式 sleep/drain；
- 最终同步时 `origin/master` 再前进到 `d74a05535409`，新增 failed-navigation error Document 的 CDP 合同，以及 runtime-created
  script 的 Document referer/initiator 修复；110 个分支提交无冲突重放。首次交叉验证 `4/5`，唯一失败是新 Protocol fixture 仍直接写
  physical `browser_context`，绕过本分支 authoritative Core registry，因而导航没有进入 owner；改用同文件既有
  `insert_browser_context` 装配后，该 runtime-script 用例连续 `20/20`（run
  `1425e321-150f-49c0-a7e2-1a20e8ed985d`）。最终 workspace nextest `16320/16320`、既有 skip `17`（run
  `74c6cf4f-d753-4bce-b5b0-bfa72adb9d35`），fmt、workspace all-target clippy `-D warnings` 和 release build 通过；binary
  SHA-256 为 `aceb60eb0a61bd96ee16ba39cee7d8cfe5d8fce196bc7db8b618b557b6a91a44`。无代理的新增 error-Document 组
  `8/8`、扩展后 CDP 默认全组 `258/258`、WebDriver 四组 `157/157` 均为 `ok=true`、失败列表为空。这继续确认 error
  Document 的 DCL/load 是事实；是否跟随未来 replacement 仍由上层 completion policy 决定；
- 验收完成后的最后一次 `git pull -r origin master` 又把 110 个分支提交无冲突重放到 `7b6328faac0a`；该 master 增量只刷新
  4 份 WPT cross-engine passed/failed/timeout/crash 基线列表，不修改 Rust、协议、smoke 或构建配置。因可执行 tree 与上述
  `d74a05535409` 验收相同，没有为纯基线数据重复 workspace/release/外部 smoke；fmt 与 diff check 仍通过；
- 这是一项物理 residence 提取，不是 Phase 6 exit。`CdpConnection` 当前仍持有 strong access capability，且
  physical `BrowserContext`/Target/Page payload、Protocol session projection 和 `BrowserHostTurnExecutor` 仍混在
  Protocol；physical Page 自身仍可能跨 renderer wait 借给 compatibility wrapper，但 Browser Host registry/engine
  residence 已不随该 wait 被借用。进入 physical payload 拆分前只允许把仍驻留 Connection 的整组 browser-global policy
  一次迁出；随后必须整体拆分 physical Browser runtime payload 与 frontend projection，并把 adapter capability 收紧为
  不会决定 Host lifetime 的 handle，而不是继续为每个调用点增加 forwarding wrapper。

2026-08-06 browser-global behavior policy 模块：

- Core 新增 `BrowserHostPolicyState`，由 `BrowserHostState` 与 navigation owner/engine 共居。基础 browser identity、HTTP
  proxy/no-proxy、TLS host verification、全局 UA/headers/cache/network/geolocation、window bounds 和 permission overrides
  现在只有这一份 application-owned 真值；这些类型不携带 session id、command id、subscription 或 CDP event；
- `CdpConnection` 删除对应 11 个 mutable 字段。Host-attached adapter 构造器也删除第二份 `FetchConfig` 参数：配置只在
  `BrowserHostState::new` 时从 authoritative `NavigationEngine` 初始化，frontend 不能再用自己的配置建立 shadow global
  state。两个 adapter 共享同一 Host 时会立即读到同一 policy，销毁其中一个 adapter 不会 reset policy；
- policy mutation 使用 closed、move-owned `BrowserHostPolicyUpdate`，在同步短临界区应用；不向 adapter 暴露任意
  re-entrant mutation closure，typed update 后续可原样进入 Host mailbox。renderer/network wait 只携带 move-owned policy
  snapshot，不跨 `await` 持有 `RefCell` guard。高频 resource-runtime/navigation 路径使用不包含 permission/window 的
  `BrowserHostNetworkPolicySnapshot`，避免每次导航克隆可增长的 permission descriptor 列表；
- Protocol 仍负责把 global policy 应用到现有 physical Page/worker，因为对应 payload 尚未移出 Protocol；
  `BrowserContext.global_*` 明确降级为 applied projection，新 Context 从 Host snapshot 初始化，frontend teardown 不会把
  projection 写回 Host。全局 command 更新 Host truth 后再启动既有 exact Page participant，因此没有新增 drain、pump、sleep
  或第二条 execution authority；
- Core policy/default/shared-residence 与 Protocol permission/window/UA/geolocation/replacement 回归 `38/38` 通过（run
  `02225502-9532-483d-bfe2-f5eeeaefa25e`）；headers/cache/proxy/TLS/network condition、background promotion 和
  existing/future Page application 回归 `26/26` 通过（run `29ce5265-33f9-4172-a18b-3f63c5732796`）；
- workspace nextest `16323/16323`、既有 skip `17`（run `b2eb0e75-429b-4e03-aa21-948f5f7f389c`），
  `cargo fmt --all --check`、`git diff --check`、workspace all-target clippy `-D warnings` 与 workspace release build
  均通过；固定 `target/release/moli` SHA-256 为
  `148c31c7d79ae29175d45e0fb3063670683dcdb37991aac33686cbd802bd467b`。显式清除大小写
  HTTP/HTTPS/ALL/FTP proxy 和继承的 smoke group、设置 `NO_PROXY=*` / `no_proxy=*` 并固定该 binary 后，CDP
  默认全组 `258/258`、WebDriver Classic/BiDi/Selenium/Semantics `--continue-on-failure` `157/157`，两套均为
  `ok=true`、失败列表为空；
- 随后按约定执行 `git pull -r origin master`，111 个分支提交无冲突重放到 `590ee6c64e`。上游增量只修改
  `moli-http-cache`，删除 disposable HTTP cache response path 的 `fsync`，与 policy residence 没有代码重叠；
  rebase 后 cache crate 全部用例与三条 Host policy 共享回归合计 `47/47` 通过（run
  `8e2cf1f4-57a4-4d44-b57b-31a7f582bb45`），workspace release build 通过，最终 binary SHA-256 为
  `a84c09f04279d3e984e0f70c2704860fcf9f8743c85d1382e7c1c0fe5d32cfd8`。同样清代理并固定最终 binary 后，
  CDP 默认全组 `258/258`、WebDriver 四组 `157/157` 再次为 `ok=true`、失败列表为空；未在无冲突、无代码交叉的
  stable module patch 上重复 workspace 全量与 clippy，前一条记录是本模块 exact patch 的完整门禁；
- 该模块完成 Phase 6 inventory 中第 3、5 项的 browser-global policy 部分，但不是 physical runtime extraction，也不是
  Phase 6 exit。下一模块必须以 `BrowserContext/Target/Page runtime payload + Host executor` 为闭环拆分 frontend projection，
  不能继续逐个 global field 或 wrapper 迁移。现有多个 frontend 对已经各自复制的 physical Context 不做跨 adapter
  反向同步；该限制只能通过移出单份 physical payload 解决，不能在 projection 上增加 watcher。

2026-08-06 physical renderer Page lifetime owner 模块：

- renderer 的单体 `RendererPageHandle` 拆成两种明确能力：唯一的 lifetime owner 负责 cancel/remove 与最终析构，cloneable
  `RendererPageClientHandle` 只提交命令、读取稳定 identity 和访问 DevTools/dialog bridge，drop client 不会移除 renderer Page。
  `Page` 对新建/standalone 调用仍暂存唯一 owner；一旦 Browser Page residence commit，它只保留 command/cache projection，因而
  frontend Page wrapper 的 `close`/drop 不再拥有 Browser Page 的生死权；
- Core `BrowserPageResidenceRegistry` 现在把 renderer Page lifetime owner 与 exact
  `{BrowserContext, Target, Page-slot instance, generation}` 放在同一 record。initial Document materialization 和 loaded replacement
  只有在 request/Target/generation 全部复验成功后才从 move-owned candidate 取走 owner；stale/reject 之前不会 `take`，候选仍可由
  caller 确定性关闭。replacement、failed-navigation discard、crash/close termination 则随 typed commit result 返回 predecessor
  owner，由 commit 之后的独立 cleanup participant 等待 remove acknowledgement；不携带 renderer owner 的 authority helper 只在
  `cfg(test)` / `test-support` 构建存在，production API 不能绕开 transfer invariant；
- Protocol 的 active/background Page slot 仍保存 `Page` command/cache projection 和 attachment/lifecycle/session state，但 projection
  不再保存已 commit renderer Page 的唯一 owner。replacement/termination cleanup 同时兼容迁移期 test fixture 中尚未被 Core
  采用的 Page owner，却以 Core 返回的 retired owner 为 production authority；没有在 Core commit 与 physical projection 之间加入
  await、callback、drain、pump、sleep、retry 或 watcher；
- owner-transfer audit 又覆盖了 legacy forget/removal 路径：Core registry 的 Target forget、staged rollback 与 BrowserContext removal
  都按 exact `BrowserPageOwnerKey` move-return renderer Page owner，调用方必须显式完成 cleanup。尤其是已经 materialize initial
  Document、随后失败的 popup rollback，不再依赖 frontend `Page::close` 的旧副作用；BrowserContext terminal removal 也会返回 typed
  termination 之后残留的 owner，作为不静默泄漏的安全边界。同 targetId 的另一 Context/实例不能授权取走 owner；
- bootstrap placeholder replacement 只允许尚未 commit renderer Page 的占位 Target；若目标已有 Core-owned Page，返回
  `TargetHasCommittedRendererPage`，要求调用方先走 typed termination，而不是覆盖 registry record 并静默 drop 唯一 owner；
- `CdpConnection::drop` 现在先从 physical Context 取出 renderer runtime roots，再只销毁 frontend Context/Target/Page projection，
  不再调用 producer termination；roots 转交 `BrowserHostState`。最后一个 Host residence 才按“终止 producers -> drop
  navigation owner/renderer Page owners -> shutdown/join network roots”的既有顺序收尾；
- 新的真实 renderer 回归会 build 并安装 initial Page，确认 Core registry 持有 exact renderer page id，drop 整个
  `CdpConnection` 后仍可从 application-owned Host capture/commit Target termination，并等待同一个 renderer Page owner 成功关闭。
  这比只检查 BrowserContext/Target registry 存活更强：它同时证明 frontend teardown 没有提前 drop Page owner 或关闭 producer
  admission。Page-residence 邻域 `16/16`（run `a2b84a19-ddc5-48db-a848-fd2f0f8eccfe`），replacement/termination/lifetime
  邻域 `40/40`（run `76864d7b-464a-4c6d-95c5-39296dcce1c8`），新增真实 teardown 回归 `1/1`（run
  `1d87edbc-104f-4bfe-9adb-8a88bc6a0c46`）通过；最终扩展邻域 `57/57`（run
  `7a3a4bbb-8748-4ef2-b43b-7dfc74b29134`）通过。加入真实 initial-Document popup rollback 后，最终 owner 边界矩阵
  `22/22`（run `4eee66fa-0036-4f94-af74-2c0e661bedef`）；四条 replacement/teardown/popup async 路径用 native
  nextest `--stress-count 20 --flaky-result fail` 做了 20 轮、共 80 次有界复跑，全部通过（run
  `e008f539-722c-4af9-ad62-2435683141b4`），没有用 retry、sleep 或放宽断言隐藏失败；
- 首次最终 workspace run 为 `16323/16324`：唯一失败是 renderer BroadcastChannel 的真实 PageVm replacement identity-collision
  回归。该用例不改代码独立复跑 `1/1`，随后 `--stress-count 50` 为 `50/50`（run
  `2331c3c8-bac5-4128-992e-70f7f6c62caf`）；再次以只报告失败的全 workspace run 验证为 `16324/16324`、既有
  skip `17`（run `cc3f7e04-8933-4ff0-8110-4f8007d609bc`）。因此保留为一次未复现的并发瞬态证据，不用重跑结果
  反向证明其根因已经修复；
- `cargo fmt --all --check`、`git diff --check`、workspace all-target clippy `-D warnings` 与 workspace release build
  均通过。固定 `target/release/moli` SHA-256 为
  `8e566274d16a23bda31916034ec745d9f226e5d36423e66d1dcee9764eaecefb`；显式清除大小写
  HTTP/HTTPS/ALL/FTP proxy 和继承的 smoke group、设置 `NO_PROXY=*` / `no_proxy=*` 后，CDP 默认全组
  `258/258`、`ok=true`；WebDriver Classic/BiDi/Selenium/Semantics
  `--continue-on-failure` 为 `157/157`、`ok=true`，两套失败列表均为空；
- 提交后按约定执行 `git pull -r origin master`，112 个分支提交无冲突重放到 `25326b1c12`。master 增量只新增
  Fetch/runtime callback teardown 的 CDP smoke 与 fixture（4 个 Python/README 文件），不修改 Rust、构建配置或本模块代码；排除
  本条事后证据的 module patch stable patch-id 前后均为 `6cf756750e2f16259f8205448278a2c4fcd83df4`。rebase 后
  workspace release build 通过，binary SHA-256 仍为
  `8e566274d16a23bda31916034ec745d9f226e5d36423e66d1dcee9764eaecefb`；新增 Fetch teardown 组 `3/3`、扩展后的
  CDP 默认全组 `259/259`、WebDriver 四组 `157/157` 均为 `ok=true`、失败列表为空。没有在无 Rust 交叉且 stable module
  patch 不变的组合树上重复 workspace 全量与 clippy；rebase 前同一 patch 已通过两项门禁，rebase 后以新增 exact smoke、release
  重建和两套完整外部 smoke 覆盖集成；
- 这完成的是 physical Page 的 lifetime authority，不是完整 Page payload 或 Host executor 提取。`CdpConnection` 仍持有 physical
  `BrowserContext`/Target、`TargetPageSlot` 中的 Page command/cache projection、attachment/session state 和 production
  `BrowserHostTurnExecutor`；per-context renderer/network root 也仍在 Context 创建时驻留 Protocol，只在 adapter teardown 时转交
  Host。下一模块必须整体提取 BrowserContext/Target runtime payload 与 Host executor，并让 CDP 只持 handle/projection，不能重新
  退回逐字段 forwarding wrapper。

2026-08-06 BrowserContext renderer/network runtime root residence 模块：

- inventory 先确认现有 `BrowserContext` 同时包含 Page/worker runtime payload、storage/network applied state 与 CDP
  session/subscription/cache；因此本模块没有把整个混合类型塞进 `Rc<RefCell<_>>` 冒充拆分，而只提取其中唯一、可独立定义生命周期的
  `RendererBrowserContextRuntimeOwner`；
- `BrowserHostState` 新增按 exact `BrowserContextHandle` 索引的 runtime-root registry。Context topology 注册与 root 注册是同一个同步
  Host transaction；Context removal 先暂存 exact root，再提交 Core topology，typed rejection 会把 root 放回原实例，成功才 move-return
  给 terminal cleanup。同一个公开 context id 的旧、新实例不能互相取走 root；
- 未注册的 physical `BrowserContext` 只暂存 candidate owner；注册成功后只留下 cloneable、non-owning runtime access。
  `CdpConnection::drop` 的“临终收集并转交 roots”整段删除：frontend teardown 不再参与 Browser runtime ownership，最后一个 Host
  residence 仍按“终止 producers -> drop navigation/Page owners -> shutdown/join network roots”收尾；
- `Target.disposeBrowserContext` 的既有 owner task 新增正常的 Context runtime cleanup participant。terminal owner turn 立即关闭 producer
  admission，随后在不借用 `CdpConnection` 的 participant 中关闭残留 exact Page owners/projections，最后 join network root；这不是
  frontend waiter、drain 或 pump，丢弃 command reply 也不能取消已接受的 cleanup；
- Core exact-root 回归覆盖 topology rejection rollback、成功 move-return 与 same-id ABA 重建；Protocol 回归覆盖 committed projection
  不再携带 owner、drop frontend 后 Host root 仍可访问，以及丢弃 disposal reply 后 network root 仍被 terminal participant 关闭。开发期
  聚焦 nextest 为 Core `1/1`（run `19580cf4-af8d-4bf0-b812-9ae9b9655a4e`）、Context disposal/Host lifetime `23/23`
  （run `63cdc1ea-40fb-490f-8d36-ee85de74a460`），加强后的 detached cleanup exact 回归 `1/1`
  （run `abf2afcb-8039-4080-a5d4-4706708b04bb`），Core state 与既有 Context projection rollback 矩阵 `11/11`
  （run `f20741c6-b36d-4bf7-a9d5-562d9323ea1f`）；
- 最终 workspace nextest `16325/16325`、既有 skip `17`（run `c38f6747-df7f-45f8-af81-c0562458cc54`），
  `cargo fmt --all --check`、`git diff --check`、workspace all-target clippy `-D warnings` 与 workspace release build 均通过。
  固定 `target/release/moli` SHA-256 为
  `5449a8dfa744417b1f8b7fec4064d5d16d54e1895c027ca2e8fa5072beeadfcf`；显式清除大小写
  HTTP/HTTPS/ALL/FTP proxy 与继承 smoke group、设置 `NO_PROXY=*` / `no_proxy=*` 后，CDP 默认全组 `259/259`、
  WebDriver Classic/BiDi/Selenium/Semantics `--continue-on-failure` `157/157`，两套均为 `ok=true`、失败列表为空；
- 这完成了 physical BrowserContext runtime root 的 lifetime authority，但不是 Phase 6 exit。`CdpConnection` 仍保存 physical
  BrowserContext/Target、Page command/cache、session/attachment projection 与 production `BrowserHostTurnExecutor`。下一模块按
  `Context/Target/Page runtime payload + Host executor` 的完整闭环继续，不再以单字段或单 wrapper 作为架构里程碑。

2026-08-06 physical Page command/cache payload residence 模块：

- Core `BrowserPageResidenceRegistry` 的 exact Target/Page-generation record 现在同时持有唯一 renderer lifetime owner 与唯一
  mutable `Page` command/cache payload owner。initial Document materialization 和 loaded replacement 在同一个 Browser owner
  transaction 中校验两者指向同一 renderer Page，再一起发布 successor；failed-navigation discard、Target termination 和
  Target forget 会在 owner turn 内撤销 payload liveness，不再等待 Protocol slot drop；
- Protocol `TargetPageSlot` 删除 `Option<Page>`，只保存 cloneable、non-owning `BrowserPageRuntimeAccess`。一次 Page 操作先
  checkout move-owned `BrowserPageRuntimeLease`：短 `RefCell` borrow 在返回前已经结束，renderer/V8 等待期间不借用
  `BrowserHostState`。lease drop 只有在同一个 Core owner 仍 live 时才归还 payload；replacement/termination 已发生时直接丢弃旧
  Page，不能把 late completion 的旧 cache 放回 successor generation；
- runtime access 单独保存 immutable physical Page id、renderer Page id 与 renderer owner id。renderer output route 和 Core owner
  配对验证不需要 checkout mutable payload，因此某条 frontend 命令持有 lease 时不会让 Browser navigation/replacement 丢失 exact
  identity；`has_loaded_page` 也表示 Core liveness，而不是 cell 此刻是否被一条命令临时 checkout；
- replacement rejection 会把 renderer lifetime owner 恢复到尚未 commit 的 Page candidate，再确定性关闭；commit 后 frontend
  projection 只安装 Core 返回的 access。renderer attachment projection 恢复既有幂等语义：prepare 已提交 attachment 时只绑定其
  id，channel closed/missing 时记录诊断并返回，不因重复 attach 或 production `expect` panic；
- exact 回归让 predecessor Page lease 跨过第二次 replacement commit，证明旧 access 在 commit turn 立即失效、late lease drop
  不能复活旧 Page、successor access 仍解析到新 payload；增强回归还明确证明 live access 在 payload 被 checkout 时保持 live，
  同期第二次 checkout 只表示 occupancy，不表示 Document 消失（run `d682b087-0f74-46d6-9ddd-51babcf375f3`）。相邻 stale
  replacement、background owner、initial build、termination 与 active/background swap `5/5`（run
  `3688ebd1-e459-4b0f-b8a4-79b56e44bcc0`），Core replacement/transition/termination 邻域 `24/24`（run
  `e480bf90-9e15-4474-911b-fac78d3a7eb7`）通过；Core/Protocol all-target check 与 all-target clippy `-D warnings`
  通过；
- 首次 workspace 全量把一个不能模糊处理的 lease 约束暴露成 `55` 条失败：旧的两阶段 Page API 在
  `start -> await -> finish` 之间保留第一次 lease，或测试把只读 lease 跨到下一条命令时，第二次 checkout 会暂时失败；这不是
  `NoDocumentLoaded`，因为 Core Page residence 仍 live。共享 preload、child-frame DOM、frame-tree、resource search、runtime
  normalization 与 fetch cleanup 入口现在只在同步 start/finish turn 内持有 lease，测试读访问也缩到实际断言作用域；需要 renderer
  owner cleanup 的 termination 回归改走真实 Core replacement，不再用裸 slot fixture 假装 production ownership。这个约束写入
  access API：`is_live` 判断 residence，checkout 失败还可能表示同一 payload 正被另一 lease 占用；同一两阶段操作再次 checkout 前
  必须先释放，不能把 occupancy 投影成 Document absence，也没有增加 retry、sleep、drain 或等待轮询；
- 修正后 Protocol 全量 `3469/3469`（run `804efa76-9c9f-4fae-9a8f-bf5f93e8f06f`），WebSocket
  CDP/BiDi/Classic 集成层 `350/350`（run `8425e5b7-e42b-46c3-8836-4e7e26fce71a`），最终 workspace
  `16325/16325`、既有 skip `17`（run `81bfc369-0fd8-427a-8954-4c1fe5574eb6`）。`cargo fmt --all --check`、
  `git diff --check`、workspace all-target clippy `-D warnings` 与 workspace release build 均通过；release binary SHA-256 为
  `5889fa1758aa748959fb0936629f2f5e53c158ee4f705bdf288c9ebee32a33ea`。显式清除大小写 HTTP/HTTPS/ALL/FTP proxy、
  继承的 smoke group 和固定端口环境后，CDP 默认全组 `259/259`、WebDriver Classic/BiDi/Selenium/Semantics
  `--continue-on-failure` `157/157`，两套均为 `ok=true`、失败列表为空；
- 这是完整 Page payload 的物理 residence cutover，但不是 Phase 6 exit。`CdpConnection` 仍持有 access capability、physical
  BrowserContext/Target、attachment/session/fact projection，production `BrowserHostTurnExecutor` 仍以整个 connection 为执行体。
  下一模块应把 Host runtime executor 与 Context/Target runtime payload 组成独立 owner-facing API，让 CDP 只提交命令并消费事实；
  不再按 Page command 调用点继续加 forwarding wrapper。

2026-08-06 Browser Host executor authority 与 application module 模块：

- inventory 先验证了一个错误方向：现有 `BrowserContext` 混合 physical Target/worker runtime、storage/network applied state 与
  CDP session/subscription projection；若把整个类型机械塞进共享 `RefCell`，`browser_context_by_id()` 等裸引用 API 会迫使数十个
  domain 理解 residence guard，且跨 projection 调用立即产生大面积临时借用与重入冲突。这满足本文“同一修复扩散到无关调用点”
  的停止条件，因此该未提交实验被完整撤回；后续必须先在 `BrowserContext` 内建立 physical payload/frontend projection 的类型边界，
  不能用一个共享容器冒充模块拆分；
- Protocol 新增独立 `browser_host_executor_residence` 模块，只定义 non-cloneable、current-thread
  `BrowserHostTurnExecutorOwner` 与 crate-private short-turn adapter。production `CdpConnection` 不再实现
  `BrowserHostTurnExecutor`，也不再公开 participant completion executor；只有持有 owner 的 application composition 才能让
  Core actor 启动 selected turn 或应用 move-owned completion。Protocol 单元测试保留 `cfg(test)` direct-actor compatibility impl，
  不进入 production library；
- application 的纯 `BrowserHostOwnerLane` 物理驻留独立 `browser_host/execution_lane.rs`，同时持有
  `BrowserHostActor`、唯一 executor owner 和 owner participant completion receiver；该模块不再引用 detached DevTools command
  completion。`cdp_scheduler/owner_inputs.rs` 只保留 frontend input mux，把 CDP navigation timeout 后的 reply future 与 Host wake
  作为两种独立来源合并给现有 adapter loop。start turn 使用一次短 adapter，renderer/network wait 被 move 到
  `PendingBrowserHostTurn` 后 adapter 立即释放；completion 到达时再绑定新 adapter，因此没有 owner/connection borrow 跨 wait；
- executor start trace 增加 application-owner turn sequence，便于区分 Core mailbox selection、短 physical projection turn 与后续
  participant completion；没有增加 queue、drain、pump、sleep、retry 或 frontend flush 依赖，也不改变 CDP/BiDi wire shape；
- Host mailbox/owner lifetime、stopped Host、frontend mux gate 与 executor residence 邻域 `21/21` 通过（run
  `29ab4958-321f-4dc8-90c2-0b4725376c29`）；slow frontend、wait:none、separate participant completion 与
  temporary Context selection 的跨等待不变量 `4/4` 通过（run `47861c78-1baf-4ef7-ba4d-06daa3823b6b`）；
- workspace nextest `16326/16326`、既有 skip `17`（首次记录 run
  `77878ca4-1ab6-4291-b8df-c7f93bb0cefb`，constructor 收口后的精确工作树复跑同样通过），
  `cargo fmt --all --check`、`git diff --check` 与 workspace all-target clippy `-D warnings` 通过；本模块不改变
  wire shape 或浏览器可观察行为，因此不机械重复 release 与外部 CDP/WebDriver smoke；
- 这完成的是 production execution authority 和 application module residence，不是 Phase 6 exit。executor 内部为完成迁移期
  Page/Target/session projection 仍短暂借用 `CdpConnection`，且 physical `BrowserContext`/Target container 仍驻留 Protocol。
  下一模块应先把该混合 container 拆成 Host-owned physical Context/Target payload 与 frontend-owned projection，再把前者交给这里已经
  独立的 executor owner；不得把 `RefCell` guard 或 forwarding wrapper 扩散到所有 domain。

2026-08-06 Browser download owner 模块：

- 模块开始前先验证并撤回了一条错误路径：把 SharedWorker/DedicatedWorker/ServiceWorker 的四组 map 从
  `BrowserContext` 搬到同一个 `CdpConnection` sidecar，会迫使 `71` 个调用点理解 `RefCell` guard，产生近 `900` 行迁移
  plumbing；这些 map 本来就是 renderer worker fact 的 CDP frontend projection，搬家既不改变 Browser Host lifetime，也不让
  多 frontend 共享真值。该实验未提交、已完整撤回。后续 inventory 必须先区分“仍在 Protocol 的 Browser owner”与“本来就应留在
  frontend 的 projection”，不能用字段 residence 代替 authority 证据；
- Core 新增 protocol-neutral `BrowserDownloadBehavior`、`BrowserDownloadPolicyState` 和
  `BrowserDownloadRegistry`。behavior/path 与 per-BrowserContext override 现在和 navigation/policy 一样由 application-owned
  `BrowserHostState` 保存；active cancel handle、terminal state 和 artifact path 则由 Host 持有的 thread-safe registry 保存，后台
  download participant 只克隆同一个 registry handle；
- `CdpConnection` 删除 `BrowserDownloadBehavior` 的 browser policy 部分和 connection-local `SharedDownloadRegistry`。两个
  frontend 连接到同一个 Host 时，后写入的 download policy 立即成为共同真值；第一个 frontend drop 后，第二个 frontend 仍可取消
  active download、读取 completed artifact。下载网络、文件写入、rename 和 terminal registry commit 不依赖 frontend response flush，
  没有增加 drain、pump、sleep 或 retry；
- CDP `Browser.setDownloadBehavior.eventsEnabled` 的 session generation、typed automation event enablement 和 WebDriver BiDi
  subscription 明确保留在 connection-local `BrowserDownloadEventSubscriptions`。raw CDP session detach 只清自身 observer；同 Host 的
  第二个 frontend 不继承第一个 frontend 的 wire/automation subscription。Browser policy 与 frontend projection 因而不再混在一个
  `BrowserDownloadBehavior` struct；
- download、response-flush、context disposal、CDP/Page alias、event route generation 与跨 frontend owner 聚焦 nextest
  `58/58` 通过（run `651afce4-1186-411f-9c08-4cca829b7ef5`），相邻 ordered typed-event drain 与 context disposal
  `2/2` 通过（run `6283652a-1095-49a8-8e3b-868b54713317`）；最终 workspace 全量 `16329/16329`、既有
  skip `17`，`cargo fmt --all --check`、`git diff --check`、workspace all-target clippy `-D warnings` 与 workspace
  release build 均通过。固定 release binary SHA-256 为
  `9c3de12998bae928fc85bc290d060e3023a430abea028021aff4699a352dc826`；显式清除大小写
  HTTP/HTTPS/ALL/FTP proxy、继承的 smoke group 和固定端口环境后，CDP 默认全组 `259/259`、WebDriver
  Classic/BiDi/Selenium/Semantics `--continue-on-failure` `157/157`，两套均为 `ok=true`、失败列表为空；
- 这完成了 Phase 6 inventory 第 4 项中的 download policy/registry owner，不是 Phase 6 exit。`global_io_streams` 是返回给请求
  frontend 的 IO handle projection，不因名字含 global 就提升为 Browser owner。下一步应做严格 owner inventory/exit audit：若
  `BrowserContext`/Target 中剩余值只是 session、attachment、subscription、applied snapshot 或 non-owning runtime access，应留在
  frontend 并重命名/分型；只有能改变无 frontend 时 browser trace 或决定 lifetime 的 payload 才迁入 Host。

2026-08-06 Browser Host identity namespace 模块：

- 严格 owner inventory 先排除了继续搬 worker/session/subscription map：这些值只改变 frontend attach/event projection，不决定无
  frontend 时的 browser trace。真正仍由每个 `CdpConnection` 各自从 `1` 开始的 owner cluster 是 BrowserContext sequence、Target
  sequence 和 Browser Owner command correlation；多个 adapter 一旦共享 Host，这三组 local counter 会产生相同 public/correlation id；
- Core 新增 `BrowserHostIdentityState`。BrowserContext 与 Browser command sequence 由 exact `BrowserHostState` 单调分配；
  `CdpConnection` 删除 `next_bc_id`、`next_target_id`、`shared_target_id_allocator` 与
  `next_browser_owner_command_id`。CDP `BID-*`、BiDi `user-context-*` 共用一个 Host context sequence，所有进入 Browser mailbox 的
  command 也共用一个 Host correlation namespace；session id、Page subscription generation 和 internal Runtime command id 仍明确留在
  frontend；
- Target id 还必须满足进程级 DevTools discovery 唯一性，因此 application 创建 cloneable `BrowserTargetIdAllocator`，注入每个
  participating Host；Host 再负责实际 allocation。原来从 application 反向安装到 connection 的 allocator setter 已删除。该 atomic
  只传递 uniqueness，使用 relaxed compare-exchange；Target payload 的发布/可见性仍由 Host mailbox 与 commit 边界同步，不能把
  identity atomic 当成 state publication；
- 两个 adapter 共享同一 Host 的 exact 回归证明 Context、Target 与 Browser command 分别得到 `1/2`，两个 Host 共享 application
  Target allocator 的回归也得到不重复 sequence。Core/Protocol owner 回归 `4/4`（run
  `4b2a5e4e-b79b-4a67-8812-8bddbeff34ca`），真实 Browser command、Context、page/worker Target 创建路径 `8/8`（run
  `0b173060-6875-409b-a021-3730374b946c`），HTTP/WebSocket live AgentHost route/teardown `4/4`（run
  `74a5222c-6323-409e-af46-f9e5bba6bc9f`）通过；最终 workspace 全量 `16333/16333`、既有 skip `17`，
  `cargo fmt --all --check`、`git diff --check`、workspace all-target clippy `-D warnings` 与 workspace release build
  均通过。固定 release binary SHA-256 为
  `fa98735b39dce491edfbc3862aa6d065609ebf10ce619ce54307e4534023e8aa`；显式清除大小写
  HTTP/HTTPS/ALL/FTP proxy、继承的 smoke group 和固定端口环境后，CDP 默认全组 `259/259`、WebDriver
  Classic/BiDi/Selenium/Semantics `--continue-on-failure` `157/157`，两套均为 `ok=true`、失败列表为空；
- 这关闭 inventory 中 BrowserContext/Target identity owner，不是 physical Context/Target payload，也不是 Phase 6 exit。
  `ConnectionNetworkRequestIdAllocator` 暂不机械并入本模块：当前 request/body artifact 同时含 Browser producer data、CDP session
  visibility、collector scope 和 IO cursor，必须在下一次完整 request/body owner 模块中先分型，不能只移动 counter 制造半套真值。

2026-08-06 Browser network request/body owner 模块：

- owner inventory 把原 `TargetNetworkArtifacts` 拆成两类状态。request id namespace、原始 request/response body、pending/failed/
  evicted response 状态会在 frontend 不存在时继续决定浏览器事实，归 Browser Host；session visibility、collector membership、
  subresource/WebSocket handle mapping、event cursor 与 IO read offset 只决定某个 frontend 如何观察，留在 Protocol；
- Core 新增 `BrowserNetworkArtifactStore` 和 protocol-neutral body/spool 类型，并由 application-owned `BrowserHostState` 保存唯一
  store。`CdpConnection` 删除 connection-local allocator；所有 document、subresource 与 WebSocket request id 都从 exact Host 的
  短生命周期 handle 分配。Protocol 中仅供测试的 target-local `REQ-*` counter 也已删除，避免结构继续暗示第二个 namespace；
- response body 保持既有 inspector budget：总计 `20,000,000` bytes、单项 `2,000,000` bytes，并保留明确 `Evicted` 状态；过去
  unbounded 的 request body 现在使用 Host lifetime budget：总计 `200,000,000` bytes、单项 `64 MiB`。body 可在 memory、secure
  tempfile spool 或 renderer subresource source 中驻留，不进入 fact journal，也不按 frontend 复制；
- 未注册 `BrowserContext` 可暂存 candidate artifact store。Context/Target projection commit 时，exact Host adoption 只复制该
  projection 已知的 request ids，同时把 candidate sequence 单调合并进 Host，防止注册后重用 `REQ-n`。active、background、parked
  Target 以及 restore projection 都走同一个 adoption 边界；
- `Network.disable`、session detach 或 frontend drop 现在只删除自身 visibility/collector/IO projection，不删除 Host raw body。
  另一个 frontend 只有在自己的 event/fact projection 建立 visibility 后才能读取同一 raw artifact；共享 Host 本身不会绕过 CDP
  session/target 授权。没有新增 watcher、drain、pump、sleep、retry 或 Network observer backpressure；
- Core network body/artifact 聚焦回归 `13/13`（run `b3b69c68-9fd8-4d68-9346-eefbeefaa8c3`），Protocol
  Network/Fetch/Target parking 与跨 frontend 聚焦矩阵 `347/347`（run
  `2a55c422-a481-4b93-9611-3952568fa9a9`）通过。第一次 workspace 全量为 `16335/16337`，稳定暴露
  `ParkedNetworkArtifacts` 的 derived equality 错把两个空 projection 中不同 Host capability residence 判成 frontend state
  不同；exact 两用例复跑 `0/2` 后，改为只比较 frontend-local artifacts/counters，并在 restore/drain 边界显式 adopt Host
  handle。修复后 exact 两用例连续 20 轮、共 `40/40`（run `f7f17139-53e2-4e7f-bf13-02752dfe6d93`），最终 workspace
  全量 `16337/16337`、既有 skip `17`（run `0ca9ef8f-a4dc-46ee-acbe-0d04b0d7a3be`）；
- `cargo fmt --all --check`、`git diff --check`、workspace all-target clippy `-D warnings` 与 workspace release build 均通过。
  固定 release binary SHA-256 为 `1dfb9b1d5c28717544f61f5a281caea4f36357778c6b11198b2e905d557170bb`；显式清除大小写
  HTTP/HTTPS/ALL/FTP proxy 与继承的 smoke group 后，CDP 默认全组 `259/259`、WebDriver
  Classic/BiDi/Selenium/Semantics `--continue-on-failure` `157/157`，两套均为 `ok=true`、失败列表为空；
- 这完成 Phase 6 inventory 第 3 项的 request/body browser portion，不是 Phase 6 exit。Protocol 仍保存 physical
  BrowserContext/Target container 和 frontend-local projection；下一模块必须继续按完整 physical payload/handle 边界收口，不能把
  raw body 再塞进 fact journal，也不能为了复用 body store 让普通 Network listener 获得 request permit。

2026-08-06 Browser Target `sessionStorage` namespace owner 模块：

- owner inventory 把 top-level Target 的 `sessionStorage` namespace 判为 Browser state：它跨 Document navigation、active/background
  residence 与 frontend detach 保持，但不同 top-level Target 必须隔离；带 opener 的 popup 在 renderer 接受 `window.open` 时冻结一份
  deep-cloned seed，后续 mutation 不与 opener 共享。CDP session、DOMStorage subscription 和 event fan-out 仍是 frontend projection；
- Core 新增独立 `target_session_storage` 模块。authoritative `BrowserTargetRecord` 现在把 exact `BrowserTargetHandle` 与
  `SharedWebStorageStore` association 放在同一 registry record；BrowserContext bootstrap 会在一个 registration transaction 中转交
  active 和所有 background Target 的 candidate namespace，普通 background/active registration、bootstrap replacement 和 renderer popup
  则通过 `BrowserTargetCreationMetadata` 原子安装 seed。Core commit 返回只读 `BrowserTargetSessionStorageAccess`，不把 mutable registry
  或 frontend route 暴露给 Protocol；
- Target registration rollback 不发布 live access；activation/demotion 只移动同一个 exact Target record；Target removal、Context disposal
  和 active replacement 与 handle retirement 同一 transaction 提交。复用相同 public targetId 会得到新的 Target instance 与新的 namespace，
  predecessor access 立即不再 live，不能授权 successor。已经被 in-flight renderer work 捕获的 store clone 可以完成旧操作，但 access 本身
  不绕过 exact Target route/liveness 授权；
- Protocol 的 `TargetSessionStorageNamespace` 降级为 registration 前的 candidate 或 Core access projection。Context/Target projector 在
  same-turn commit 后绑定 exact handle；Page storage handles 从 access 取得 store。createTarget、popup、active replacement、parking、promotion
  与 test fixture 都经过同一绑定边界；frontend teardown 不把 namespace 写回 Core，也没有新增 watcher、task、drain、pump、sleep 或 retry；
- Core session-storage owner/persistence 邻域 `6/6`（其中 exact registration/rollback/public-id reuse/Context removal 与 metadata
  rejection `4/4`，run `1410ea18-e7e5-4a7f-9b34-5a0f0e878560`），包含 active/background Context bootstrap 的全 Target handoff 回归 `1/1`
  （run `55164a08-5cf9-475a-aa83-8ebf39588a97`），跨 navigation/parking/popup/DOMStorage 与 same-id replacement 的
  `session_storage` 矩阵 `10/10`（run `8e6f5880-f6f6-42bb-98b7-514de43b7eeb`）通过；
- 最终 workspace 全量 `16343/16343`、既有 skip `17`（run `27222a88-8eb1-4990-9383-765eb7b4ab93`）；
  `cargo fmt --all --check`、`git diff --check`、workspace all-target clippy `-D warnings` 与 workspace release build 均通过。
  固定 release binary SHA-256 为 `87e1b977cea39c27f06f0d9a8a7395e146413a914deb839c94ec37154fc2ec75`；显式清除大小写
  HTTP/HTTPS/ALL/FTP proxy 与继承的 smoke group 后，CDP 默认全组 `259/259`、WebDriver
  Classic/BiDi/Selenium/Semantics `--continue-on-failure` `157/157`，两套均为 `ok=true`、失败列表为空；
- 这关闭了 mixed Target container 中唯一明确的 browsing-context storage owner，不是 Phase 6 exit。physical `BrowserContext`/Target
  仍保存 DevTools session/attachment、worker fact projection、applied runtime snapshot 与 non-owning Host access；下一模块应按完整
  Context/Target physical payload 与 frontend projection 边界拆分，不能继续把纯 frontend map 搬进 Host，也不能为每个 domain 增加 wrapper。

2026-08-06 top-level Target frontend attachment projection 模块：

- owner inventory 明确区分两件事：Target identity/residence/lifetime 属于 Browser Core；CDP primary/auxiliary `sessionId`、detach 与
  session-to-Target route 只决定某个 frontend 如何控制和观察该 Target，属于 frontend projection。后者不能为了“共享 Host”被搬进
  Core，也不应继续寄生在 physical `BackgroundTarget` 或 active Page slot 上；
- Protocol 新增 `TopLevelTargetAttachmentProjectionRegistry`，以 exact `BrowserTargetHandle` 为 key，并维护唯一 session reverse route。
  `BrowserContext.session_id`、`auxiliary_target_sessions` 与 `BackgroundTarget.session_id` 已删除。active/background promotion、demotion
  只移动 physical runtime payload，不移动或重建 attachment；primary 与 auxiliary session 都继续绑定同一个 exact Target instance；
- public `targetId` 复用不能授权旧 attachment。primary/auxiliary route 在返回 public id 前都会复验 registry handle 与当前 physical
  Target handle 完全相同；predecessor 的 late command、detach 或 held session 因而不能进入同名 successor。session id 重新绑定时，
  registry 同步清除旧 primary/auxiliary projection，避免 reverse map 与 per-Target set 形成双重 truth；
- Target registration、attach/auto-attach、session route、parking/promotion、rollback、close/crash/Context disposal 都改为通过同一个
  attachment registry。Target terminal 先冻结 primary/auxiliary cleanup，再移除 exact registry record 与 physical payload；没有新增
  task、watcher、drain、pump、sleep、retry，也没有让 frontend attachment 参与 Browser progress；
- 旧 Protocol 单元测试曾把 session 直接传给 `BackgroundTarget` 构造器，等价于绕过新 owner boundary。测试构造 metadata 现在只在
  `cfg(test)` 下短暂存在，并在 Core exact Target registration 或 direct test harness command boundary 立即消费进同一 registry；生产
  runtime 不读取该 metadata。另有测试原先构造“存在 page session、但不存在 Target”的不可能状态，现已改为显式 Target fixture，或
  按真实语义断言 `InvalidSessionId`；
- exact registry、same-public-id stale route、session-id reuse、active/background swap 与 Target attach 邻域聚焦 nextest `58/58`
  通过（run `f7d20653-cb5b-44df-a904-8de856388527`）。首次 workspace 全量确定性暴露一批 legacy test fixture 在创建 exact
  Target 前挂 session，或直接插入带 session 的 background fixture 后未消费 projection metadata；这些路径被新 registry 正确拒绝，未通过
  compatibility fallback 放宽。修正夹具装配顺序后，代表性 Worker Fetch `1/1`（run
  `d787ca12-05f8-46a5-bbed-c3529d48c614`）、剩余六条 direct fixture 路径 `6/6`（run
  `e688c17f-14ad-4c69-9e4d-eccb5d0a3a43`）与完整 Protocol 包 `3469/3469`（run
  `00f7ff3e-fed7-4339-a894-338b01587bd5`）通过；
- 最终 workspace 全量 `16347/16347`、既有 skip `17`（run `388a7470-87c5-4e93-9c3f-a84fa66ad566`）。
  `cargo fmt --all --check`、`git diff --check`、workspace all-target clippy `-D warnings` 与 workspace release build 均通过；固定
  `target/release/moli` SHA-256 为 `8e02a0a7a2d62b126fd0136510e0e8dd78a91fe54562c8cd407ff056a38a3589`。显式清除大小写
  HTTP/HTTPS/ALL/FTP proxy、原 no-proxy 与 inherited smoke group，并设置 `NO_PROXY=*` / `no_proxy=*` 后，CDP 默认全组
  `259/259`、WebDriver Classic/BiDi/Selenium/Semantics `--continue-on-failure` `157/157`，两套均为 `ok=true`、失败列表为空；
- 提交后按约定执行 `git pull -r origin master`，120 个分支提交无冲突重放到 `8f1bbd0de3`。master 增量压缩 native DOM element
  identity 并刷新 WPT baseline，不修改 Protocol attachment/session 路径；本模块 `moli-protocol` code patch 的 stable patch-id
  前后均为 `72e5935686bf08ba4d09179f54c2f2de7f1b0c9a`。rebase 后 attachment 全组与新增 DOM string boundary 交叉验证
  `59/59`（run `b2fb901c-17d2-42f3-8a07-ff2d39d4504b`），fmt/diff check 和 workspace release build 通过，最终 binary
  SHA-256 为 `8c5f85bb98cd975814719ccfeecb732ac35ef8d25515f47e532dd5d381bd7d9e`；同样清代理并固定该 binary 后，CDP
  `259/259`、WebDriver 四组 `157/157` 再次为 `ok=true`、失败列表为空。未在 stable module patch 上重复 workspace 全量与 clippy；
  rebase 前同一 code patch 已通过两项门禁，rebase 后以 exact 交叉点、release 重建和两套完整 smoke 覆盖 master integration；
- 这完成的是 mixed Target container 的 frontend attachment 分型，不是 Phase 6 exit。physical `BrowserContext`/Target 仍保存 worker
  projection、applied runtime snapshot、Page command/cache projection 与 non-owning Host access，且 executor completion 仍短暂借用
  `CdpConnection`。下一模块应以完整 physical Context/Target runtime payload 为单位进入独立 Host owner API；不能继续逐 session 或
  domain 增加 forwarding wrapper，也不能把这个 frontend registry 误搬进 Browser Core。

2026-08-06 exact Target frontend session state registry 模块：

- 前一模块只抽出了 attachment route，本模块把同一 exact `BrowserTargetHandle` 下的 primary/root
  `DevToolsSessionState`、auxiliary session state 与 session reverse route 合并为一个
  `TopLevelTargetFrontendSessionRegistry`。`BrowserContext` 不再另存 active primary/auxiliary state，
  `ParkedPageSessionState` 也不再保存 background 副本；physical active/background residence、parking、promotion 和 demotion
  因而只移动 renderer/policy payload，不再搬运或克隆 frontend session state；
- primary state 属于 exact Target，显式 primary `sessionId` 只是可 detach/rebind 的 route；auxiliary state 则与对应 attachment
  route 同生共死。session id 复用会在建立新 route 前原子删除旧 primary/auxiliary route 与旧 auxiliary state。session-scoped
  reset 只把 still-attached auxiliary value 归零，不能删除 map membership；只有 detach/Target terminal 才能做结构删除，避免
  reverse route 仍存在但 state 已消失的双重 truth；
- `unbound` 只表示尚无 active Target 时的 root candidate。新注册的 active exact Target 原子接管它；active Target terminal
  会先删除所有显式 route，再只把跨 successor 明确定义保留的 aggregate runtime bindings 写入新的 candidate。Runtime/Page
  subscription、pending inspector await、remote-object ownership、dialog 与 document-local state 都不会进入 successor；复用相同
  public `targetId` 也不会继承 predecessor state；
- Target owner command mutation、navigation renderer inputs、loaded-Document cleanup、pending inspector await drain/diagnostics、
  Runtime remote-object owner 检查与 output subscription 都改为查询同一 registry。任意 session/target route 使用 exact lookup；
  stale 或缺失的 auxiliary route 退化为 `NoLoaded`，且在确认 frontend projection 前不会取走 parked policy payload。background
  navigation 缺失 frontend projection 时使用空 frontend-derived renderer config 继续 Browser-owned action，不能让 projection
  完整性变成 Browser progress permit；
- physical `targetParking` diagnostics 不再伪装拥有 frontend pending-await state；对应计数现在从 exact registry 汇总。没有新增
  task、watcher、drain、pump、sleep、retry，也没有新增 production `expect!`/`panic!`。registry exact identity、residence
  invariance、same-public-id isolation、session-id reuse、primary detach/rebind、unbound adoption、terminal sanitized inheritance、
  background navigation 与 stale auxiliary policy preservation 均有直接回归。聚焦 terminal sanitized inheritance `1/1`
  通过（run `0b9cb2a9-ba09-4b16-9c57-892ef54a4dd8`）；完整 Protocol 包 `3475/3475` 通过。首次大范围迁移验证暴露的
  auxiliary reset/route membership、无 active Target 的 root command、无 primary session 的 background fixture registration、
  terminal root binding retention 与 diagnostics residence 都在本模块 owner boundary 收口，没有以 compatibility fallback 放宽；
- 最终 workspace nextest `16353/16353`、既有 skip `17`（run
  `f68bf84f-917e-40bd-9e45-d5556a54a1a0`）；`cargo fmt --all --check`、`git diff --check`、workspace
  all-target clippy `-D warnings` 与 workspace release build 均通过。固定 `target/release/moli` SHA-256 为
  `ca0a67549906e379ff3a52f748ac4b53ba5d33c6a413a6d493400507d51dc2ad`；显式清除大小写
  HTTP/HTTPS/ALL/FTP proxy 与 smoke group 环境覆盖、设置 `NO_PROXY=*` / `no_proxy=*` 并固定该 binary 后，CDP 默认全组
  `259/259`、WebDriver Classic/BiDi/Selenium/Semantics `--continue-on-failure` `157/157`，两套均为
  `ok=true`、失败列表为空；
- 提交后执行 `git pull -r origin master`，121 个分支提交无冲突重放到 `45c532c5eb`；本模块 stable patch-id 前后均为
  `b4cfeeb201eb5281adb48f4f4daac5682980ba94`。上游唯一增量修复 Debugger step response/event ordering，并与本模块在
  `domains/runtime/activity.rs` 的测试 fixture accessor 有代码交叉；其 wire barrier、Protocol output slot、renderer pause bridge
  和真实 WebSocket 新回归合计 `7/7` 通过（run `3696dd3e-e9f8-4163-b07b-694f51e5d597`）。rebase 后 workspace
  release build 通过，最终 binary SHA-256 为
  `889b46e530adeef89d60988d7e899544751d15c84be5237b2771de0b7ca70040`；同样清代理并固定该 binary 后，CDP
  `259/259`、WebDriver 四组 `157/157` 再次为 `ok=true`、失败列表为空。没有在 stable module patch 上重复 workspace
  全量与 clippy；rebase 前同一 patch 已通过两项门禁，rebase 后以 exact 交叉点、release 重建和两套完整 smoke 覆盖 master
  integration；
- 这关闭的是 top-level Target frontend session state 的单一 residence；当时仍把 `CdpConnection` 视为 frontend connection，因而把
  完整 physical Context/Target payload 提取列为 Phase 6 blocker。后续 final audit 以 production composition root 纠正了这个前提：
  actual socket frontend 与 owner-task-resident adapter 已是两个 residence，详见下一模块。

2026-08-06 DevTools Host adapter lifetime final audit 与 Phase 6 exit：

- audit 从 production `SharedCdpOwnerRegistry -> owner task -> CdpScheduler` 装配路径重新追踪 lifetime，而不是继续按 legacy 类型名
  推断 ownership。actual socket frontend 是 `CdpFrontendEndpoint/Receivers`、`CdpFrontendRouter` 和 per-socket sink；
  `CdpConnection` 则始终驻留共享 owner task，跨 browser/page WebSocket attach、detach 和 reconnect 存活。已有真实 WebSocket 回归
  明确证明 browser frontend 断开后 owner count、dynamic Target、renderer runtime 与独立 page frontend 均继续存活；因此把后者继续
  当作“单个 frontend connection”拆分，是过期模型；
- audit 中验证并撤回了 generic owned/borrowed `CdpConnection` residence 方案。该方案机械泛型化接近 `300` 个 Protocol 文件，all-target
  编译仍需要大量 fixture 改写，更关键的是它只是把仍含 Inspector session、worker projection 与 output cursor 的混合
  `BrowserContext` 整体搬家，没有改变任何 authority。未提交实验被完整撤回，工作树恢复到精确基线；这再次满足本文的停止条件：
  大面积 forwarding/generic plumbing 且不删除 owner，不能作为架构进展；
- Protocol 新增 non-cloneable `DevToolsHostAdapter`，显式包住 legacy `CdpConnection`。application `CdpScheduler` 字段从模糊的
  `conn` 改为 `host_adapter`；Browser Host execution lane 的 application API 只接受这个 adapter 类型。Core actor 仍独立拥有输入选择
  与 authoritative `BrowserHostState`，adapter 只在 actor 选定 turn 后提供短 renderer/DevTools projection。socket frontend
  endpoint/router 从未获得 adapter 或 executor capability；
- 该 cutover 不增加 queue、task、watcher、drain、pump、sleep、retry，也不改变 command/event wire shape。它冻结 Chromium 风格的
  三层责任：Browser Core 拥有 browser truth/progress，application-owned DevTools adapter 横跨 Browser/Renderer 做命令与事实投影，
  socket frontend 只负责输入、route 和输出 sink。剩余 worker/Inspector/Network/Log projection 可以合法留在 adapter；只有经 owner
  inventory 证明会在无 frontend 时继续决定 browser fact/runtime lifetime 的状态，才需要继续迁入 Core；
- exact adapter residence `1/1`（run `ce59391a-3623-413f-aa89-e35ecb053278`）、无 frontend 输入时的 Browser fact wake
  `1/1`（run `802fb26d-4d29-4825-9411-fcfe5b8669ce`）与真实 browser WebSocket detach 后 dynamic Target/runtime 存活
  `1/1`（run `6a640b13-be4b-40e2-aac0-bc7d335b3d60`）通过；最终 workspace nextest `16360/16360`、既有 skip
  `17`（run `ec82af25-a5e1-45d7-b976-075aed6c3f1b`）；`cargo fmt --all --check`、`git diff --check`、workspace
  all-target clippy `-D warnings` 与 workspace release build 均通过。固定 release binary SHA-256 为
  `862d5b85506c14afc0e24b55ce4e7b087c49b1a1e49e720f3a31071c4d83f355`；显式清除大小写
  HTTP/HTTPS/ALL/FTP proxy 与 inherited smoke group/port，设置 `NO_PROXY=*` / `no_proxy=*` 并固定该 binary 后，CDP 默认
  全组 `259/259`、WebDriver Classic/BiDi/Selenium/Semantics `--continue-on-failure` `157/157`，两套均为
  `ok=true`、失败列表为空；
- Phase 6 exit 以 production lifetime 和 authority 通过，不再以“Protocol projection 容器必须为空”作为伪 gate。下一阶段只收敛
  raw CDP、BiDi 与 Classic 的 actual frontend/service lifetime；standalone `Browser` 不因复用 renderer/core 语义而被强制改造成
  DevTools frontend。

`moli serve` 先创建 owner task，其中 Browser Host owner lane 与 DevTools Host adapter 是独立成员；随后接受
CDP/BiDi frontend endpoint。frontend endpoint 保存 command queue、route 与 output sink，不持有 Browser Host 或 adapter。
`moli fetch` / MCP 则创建一次调用级 standalone `Browser` Core；它们不持有 served Host receiver，也不参与 CDP
frontend queue。两种 deployment 共享 exact lifecycle/navigation contract，但 high-level wait 与 CDP observation 的返回 policy
可以不同。

Exit gate（已通过）：drop actual frontend endpoint 不会 drop Browser Host 或 DevTools Host adapter；BrowserContext/Target/Page
identity、topology、navigation、Page/engine/profile lifetime 与 browser-global policy 的 authoritative mutable state 位于 Core/
application-owned services。legacy `CdpConnection` 只作为 `DevToolsHostAdapter` 内部实现保存 renderer/DevTools projection 和
non-owning/共享 Core access，不再被解释为 frontend lifetime owner。

### Phase 7：served protocol frontend 收敛

目标：删除“actual frontend endpoint 可以持有或驱动 Browser/DevTools Host receiver”的过渡模型。

工作：

- 同一 logical browser/session 的多个 frontend clone exact 同一 owner endpoint 或 typed service handle；独立 WebDriver
  session 与 raw-CDP browser 可以拥有不同 Host instance，具体 actor 类型可以服从各自 composition；
- raw CDP、BiDi 与 Classic 都不能直接持有或驱动 Host receiver/scheduler；
- 各 frontend 只保留协议 wait/error/result/event mapping；
- WebDriver default Context 必须属于该 session 的 private Host；只有 wire API 显式创建/删除 user context 时才发对应
  create/delete command，不能为架构整齐改变 BiDi default userContext 语义；
- 删除各 adapter 的独立 page pump、pending-navigation 扫描和 browser teardown fallback。

Exit gate：给同一 Browser Host 输入相同 command/renderer intent，raw CDP、BiDi 与 Classic 观察到相同
browser trace；差异仅来自书面的等待与 wire 投影语义。关闭或放慢任一 actual frontend 不改变 Host trace/lifetime。
standalone CLI/MCP 的跨 deployment differential 属于产品兼容与 benchmark gate，不是 CDP/Browser owner 分离 gate。

状态：已完成（2026-08-06 exit audit）。第一项端到端模块已把 standalone BiDi 与 Classic 的 production Host residence 收敛到
`protocol_server/devtools_host_service.rs` 与 `protocol_server/devtools_host_service/actor.rs`：service actor
唯一拥有 `CdpScheduler`、`BrowserHostExecutionLane`、renderer/background receivers 和
`ProtocolAdapterScheduler`；cloneable service handle 只提交 typed request/attach/shutdown。Classic 保留独立串行
frontend queue 和 WebDriver frame/result 映射，attached BiDi detach 后 exact Classic Host 继续运行并允许重新 attach；
standalone BiDi socket 结束后由 session owner 显式 shutdown/join service。原 standalone BiDi loop 与
Classic-with/without-BiDi Host loop 已删除，没有新增 sleep、poll、drain、retry 或 frontend progress trigger。

最终 audit 没有继续把 raw CDP 强塞进同一个 service 类型。production `SharedCdpOwnerRegistry` application-own
一个 shared owner task；`CdpFrontendEndpoint` 只有 control/command sender 和 shutdown watch，不包含
`CdpScheduler`、`BrowserHostActor`、renderer/background receiver 或 adapter。owner actor 独立选择 Browser Host
wake、participant completion、fact wake 与 frontend input；browser/page WebSocket detach 只删除 route/sink，不能关闭
owner。raw CDP 的多 socket/Target topology 与 WebDriver 的 session-owned Host 不同，保留两个 composition actor
是有意设计，不是第二份 browser authority。

同一 audit 还确认：standalone `Browser` 的 `FollowInStandaloneAdapter + FollowBeforeReply` 是
`document-milestone-navigation-completion-design-current.md` 已冻结的 high-level completion policy；它不是 frontend
pump。把完整 `Request`、raw non-HTML、response/selector/script wait、dump/trace 与 mutable automation `Page`
改经 DevTools command，会重写 public Browser API，却不删除 served Host 中任何 owner，因此移出本阶段。

本端到端模块的验证边界是 Host service lifetime，不是“删掉所有 frontend await”：Protocol server
BiDi/Classic 聚焦组 `258/258`（run `af25a630-f095-4e7b-87b1-d602c960da22`），`moli`
crate 全量 `573/573`（run `8f63490c-6264-4139-be1b-7a1e78be15ef`）。workspace 首轮
`16361/16362` 时出现一项未被本模块修改的 renderer sandboxed Blob iframe/OPFS 调度失败（run
`20afe1dd-3a7f-48e4-97c4-f46f0844abbc`）；原测试聚焦压力 `20/20`（run
`4ec3c12a-33ab-4805-a5a7-f4cfc66d51ad`）、所属 structured-clone 模块五轮 `40/40`（run
`a8a8fa2b-0ec5-4017-b69f-1b7514725e10`），最终 workspace 全量 `16362/16362`、既有 skip `17`
（run `9aaa6193-538d-4890-bd37-ec40ee869a53`）。workspace fmt、all-target clippy `-D warnings`、
release build 与 diff check 均通过；固定 `target/release/moli` SHA-256 为
`89425f5cb6f804fcccdc48ade3d521c530ff0e6a7beaf17960bf95a2712ca795`。显式清除大小写
HTTP/HTTPS/ALL/FTP proxy、原 no-proxy 与 inherited smoke group/port，并设置 `NO_PROXY=*` /
`no_proxy=*` 后，CDP 默认全组 `259/259`，WebDriver Classic/BiDi/Selenium/Semantics
`--continue-on-failure` `157/157`；两套均为 `ok=true`、失败列表为空。

2026-08-06 exit audit 的静态 inventory 与聚焦证据：`ProtocolSchedulerWork` 只剩 protocol observation、
main-Document Browser-fact projection 和 BiDi channel continuation；renderer navigation、replacement、popup、
termination 均已从该 residence 消失。`CommandOwnerScope` 只冻结 session/output route，不授权 Browser action。
Host mailbox/fact wake、raw-CDP frontend detach/reconnect、BiDi detach 后 Classic Host 存活与 clone handle lifetime
共 `6/6` 通过（run `44580b3e-dbfb-457b-9d63-00955ec7da60`）；standalone post-load reload 与两个
Fetch exact owner 在 frontend wait 丢失后继续完成共 `3/3` 通过（run
`68648067-a21c-4fad-880d-96687f1c1455`）。本次只修正文档 gate，没有修改 Rust，因此没有机械重复
workspace/release/smoke；前一端到端模块的完整门禁和外部 smoke 即为当前代码树证据。

### Phase 8：删除迁移层并评估物理隔离

状态：已完成 exit audit；物理隔离保持可选。

删除或确认不再承担 Browser authority：

- protocol scheduler 中遗留 browser-owner work kind；
- `CommandOwnerScope` 对 browser action 的使用；
- duplicated BrowserContext/Target/Page current-state cache；
- command-followup 充当 browser progress pump 的 helper；
- 已失去用途的 generation、route override 和 adapter bridge。

2026-08-06 audit 结果：前两项已经满足；`ProtocolSchedulerWorkKind` 的三个剩余 variant 都是协议投影/
continuation，`CommandOwnerScope` 是 frontend correlation。physical `BrowserContext`/Target/Page view 仍可合法驻留
`DevToolsHostAdapter`，但 authoritative identity/topology/generation/lifetime 在 Core；因此它不是 duplicated current-state
authority。现存 command-followup snapshot 只调度 BiDi channel action，不扫描或推进 navigation。generation、exact route
和 non-cloneable `DevToolsHostAdapter` 仍分别用于 stale rejection、frontend routing 和 application residence，不能因带有
迁移期名字而删除。

这也冻结 Phase 8 的停止条件：重命名 300 个 Protocol 文件、把 `CdpConnection` 机械改名，或让 raw CDP/WebDriver/
standalone Browser 共用一个 concrete actor，都不会删除 authority 或 progress dependency，不能算本计划收益。后续只有
production trace 证明某个 adapter 字段会在没有 frontend 时决定 Browser progress/lifetime，才重开 owner migration。

随后基于证据决定是否：

- Browser Host 独立 OS thread；
- 多 Browser Host worker pool；
- 进程/IPC 隔离。

当前没有 correctness 或资源证据要求物理拆分；同进程 current-thread 模型长期保留。物理拆分不是完成条件。

## 第一批 PR 建议

不要从“移动整个 `CdpConnection`”开始。建议按以下可审查切片推进：

### PR A：identity inventory 与 trace

- 维护本文和 field ownership inventory；
- 给 renderer intent、owner action、request、commit、fact projection 增加统一 trace key；
- 不改行为。

状态：架构范围已完成。本文、initial/producer inventory、production navigation trace/JSONL、Core fact
sequence、四种 Moli surface 同源 fixture 和第一版 release short-navigation 基线均已建立；Chromium
machine differential 以及 event-heavy/多 Target/idle baseline 作为后续 benchmark 增强，不阻塞本计划 exit。

### PR B：renderer navigation 去 session 化

- action 只携带 neutral target/page/document identity；
- owner lookup 不再进入 `CommandOwnerScope`；
- 保留现有 scheduler，先证明路由不回退。

状态：已完成。`TopLevelLocationNavigationOwnerAction` 已去 session 化；下一切片进入 PR C，不再给
该 action 增加 frontend route compatibility field。

### PR C：Browser navigation owner seam

- `NavigationEngine` 和 page lookup 进入 core-owned substructure；
- protocol 通过无 session 的 owner API 调用；
- event adapter 暂时保留。

状态：已完成。BrowserContext/Target/Page generation、initial Document、request、history、replacement、
termination 与 selected/retained engine authority 均已进入 Core；physical payload adapter 只剩同 turn 投影。

### PR D：renderer navigation lane cutover

- 启动 Browser Owner queue；
- renderer intent 直接发布到该 queue；
- 删除 `TopLevelLocationNavigationOwnerAction` protocol residence；
- 复用本次 passive-progress regression 作为红绿证据。

PR C 已先迁移 `Page.navigate` 共用的 request/history/replacement/termination authority；PR D 只切换 renderer
与 command navigation 的执行 lane，不再重做这些 state owner。Browser fact journal 已在 Phase 5 完成；当前进入 Phase 6
Host lifetime 与 browser state 的应用级提取。

状态：已完成（第48切片 exit audit）。renderer action 已直接进入 Core `BrowserHostActor` mailbox，Protocol scheduler residence 与
`BrowserOwnerInputPublished` migration envelope 均已删除；`CdpConnection` 在 publication boundary 只保存
cloneable producer handle。raw-input pop/complete seam 也已删除：Core actor 签发 exact `BrowserHostTurn` 并调用
protocol-neutral executor，`CdpScheduler` 和行为 fixture 的调度路径不再拥有 selection 后的裸 input。actor
residence 也已移到 application input composition，所有 production blocking wait 直接监听 mailbox，不再用
frontend/renderer input 间接唤醒或靠 loop-top polling 推进。
actor executor 也已改成同步 short-start contract；renderer/network participant wait 现在由 application execution
lane 登记成 exact pending/completion input，不再跨等待借用 actor 或 `CdpConnection`。response-ready navigation
现在由独立 completion 模块把 load/configuration/renderer commit 分成三个 phase；production background path 也
通过 opaque participant completion 回到原 input channel，且只由 terminal disposition 释放 navigation gate。
renderer commit 后的 Inspector replay dispatch 也已拆成逐个 move-owned participant；全部 replay 共用原 gate，
target-keyed engine 在 commit apply turn 内完成 adoption。generic materialized outcome 也已在 body apply 后进入同一
tail participant seam，不再由 Browser Host completion inline drain replay。loaded Page restore/replacement/disposal、
worker retirement、lifecycle projection、BiDi preload listener 与 Runtime output normalization 的主要 navigation/Page
command 路径也已逐步迁成 exact participant；scheduler-facing `ReleaseObjects` 也已成为逐 handle participant chain。
第48切片确认 renderer top-level navigation 已无 Protocol execution fallback，slow/disconnected frontend 也不构成其
progress predecessor，因此 PR D 和 Phase 3 正式关闭。`CdpTurnOutcome`/fact journal 与 actor teardown/lifetime 分别进入
Phase 5/6；第49--75切片已把 production command navigation、renderer top-level history、popup/termination、Fetch decision、
BrowserContext disposal、download progress 与 initial-target URL 的 action authority 逐步汇入同一 Host lane；第76切片又迁移
`Page.createIsolatedWorld` 的 initial-URL prerequisite 并完成 Phase 4 exit audit。neutral fact/completion transport 和独立 Host lifetime
仍分别由 Phase 5/6 负责。

## 回归矩阵

### Navigation source

至少覆盖：

1. CDP `Page.navigate`；
2. BiDi/Classic navigate；
3. CLI initial navigation；
4. parser-blocking script `location.href`；
5. DCL handler navigation；
6. load handler navigation；
7. post-load one-shot timer reload；
8. history traversal / same-document hash navigation；
9. popup/auxiliary target；
10. `Page.stopLoading` / target close competing with navigation。

### Frontend condition

每个关键 source 至少组合：

- 无 protocol subscriber；
- passive CDP event listener；
- 插入 noop command；
- 多 in-flight command；
- slow socket writer；
- frontend disconnect；
- primary + auxiliary session；
- Page domain disabled/enabled；
- BiDi/Classic attached；
- CLI high-level wait。

### Identity 与 race

- A/B 使用不同 Page/document/loader identity；
- consecutive navigation 后旧 request completion 不能 commit；
- old renderer intent 不能修改 successor Page；
- old DCL/load wake 不能投影成新 Document；
- target park/promote 不重建错误 owner route；
- context delete/browser close 只结算一次；
- frontend detach 不影响 accepted browser action；
- interception permit disconnect 按 policy 结算；
- duplicate fact/projector retry 不重复执行 side effect。

### Protocol observable

必须检查：

- command response shape/error code；
- response 与 post-response event 顺序；
- `frameNavigated`、loader id、lifecycle event attribution；
- Target attach/detach/close sequence；
- Network request/response/body target ownership；
- pending command timeout/cancel；
- session-local domain subscription；
- slow/lagged subscriber 的显式处理。

### Differential

本地 deterministic fixture 同时跑：

- Moli standalone；
- Moli raw CDP；
- Moli BiDi/Classic；
- Xvfb Chromium raw CDP。

比较 browser-visible trace，不比较像素：

- request/redirect/status/final URL；
- navigation/commit/replacement；
- Document/loader identity；
- DCL/load sequence；
- command response timing；
- frontend disconnect/second command 是否改变结果。

### Repository gate

每一阶段使用所属边界附近的 focused `cargo nextest`。阶段合并前按仓库规则：

```bash
cargo nextest run --no-fail-fast
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
```

禁止用 `cargo test` 代替。

## 性能与资源约束

拆分不能以复制状态和事件换 correctness。

必须保持：

- 第一阶段不新增每 frontend OS process/thread；
- Browser Host mutable state 单份；
- subscriber 只保存 cursor/小型 projection state；
- DOM snapshot、network body 和下载数据不复制进 journal；
- owner/action/fact channel bounded 或有明确 backpressure policy；
- 无 subscriber 时不构造昂贵 CDP JSON；
- trace 默认关闭，开启时有界；
- 不因为 frontend 数量重复 NavigationEngine/renderer，除非明确创建独立 BrowserContext/Target。

Phase 0 先记录现有 release 的：

- CLI/CDP navigation latency；
- cold/warm RSS/PSS；
- 1/多 target 并发；
- event-heavy Network/CDP output；
- browser idle footprint。

后续每个 owner/lifetime cutover 都做前后对比。任何明显 regression 先定位状态复制、channel queue、
serialization 或多余 wake，不能通过少发协议事件提高分数。正式百分比 gate 以 Phase 0 baseline 和
benchmark noise 冻结，不在设计阶段拍脑袋写阈值。

## Diagnostics

统一 trace 至少包含：

```text
browser_instance_id
browser_context_id
target_id
page_residence_generation
navigation_request_id
renderer_agent_attachment_id
document_lifecycle_identity
browser_action_id
browser_fact_sequence
source = frontend-command | renderer-intent | network | lifecycle
owner_state_before / owner_state_after
frontend_projection_sequence
```

时间点分开记录：

- intent/command published；
- Browser Owner accepted；
- network started；
- response commit-ready；
- commit participant settled；
- Page replacement committed；
- renderer lifecycle reached；
- browser fact appended；
- frontend event projected/flushed。

这样可以区分“browser 没执行”“fact 没发布”“projector 没订阅”“socket 没 flush”，避免重新用下一条
命令做探针。trace 不记录 cookie value、authorization 或 response body 等敏感内容。

## 迁移纪律

- 一个 action 任意时刻只能有一个 execution authority；
- 新 owner 路径落地后，同一 PR 或紧邻 PR 删除旧执行路径；
- adapter 可以转换类型，不能重新扫描 mutable Page 寻找 pending action；
- 不用 feature flag 长期维护两套 browser runtime；必要时只做 test-only shadow decision trace，shadow
  不能执行 side effect；
- 不为了减少改动让 Browser Core 接受 `sessionId`、CDP event buffer 或 wire error；
- 不为了复用 `CdpScheduler` 把 Browser Owner queue 命名成另一种 protocol residence；
- 不把 frontend disconnect 的 drop side effect 当作 browser lifecycle API；
- 不在 CDP、BiDi、Classic 调用点复制同一种 navigation/termination 修复；
- 不用 sleep、drain、retry、heartbeat 或无限 pump 修调度正确性。

## 风险

### V8/thread affinity

`NavigationEngine` 和 renderer handle 仍是 current-thread owner。过早跨线程会引入 Send/Sync、local
task 和 teardown 风险。因此先在同一 dedicated runtime 内拆 actor/handle，不能为了“看起来分开”
移动 V8 到普通 Tokio worker。

### BrowserContext 中混有 protocol state

当前 BrowserContext/Target 结构可能同时保存 browser runtime 和 DevTools projection。不能机械移动
整个类型。先做 field ownership inventory，再拆 authoritative runtime state 与 frontend mirror。

### DevTools commit timing

document-start script、binding、isolated world 和 V8 session restore 要在 author JS 前就绪。Browser
Core 分离不能把它们推迟到 DCL 后；应使用 explicit commit participant，与现有
`PreparedRendererDocument/ReadyToCommit` 计划对齐。

### Network/Fetch 边界

Fetch interception 可以合法暂停 request，但 Network listener 只是 observer。迁移时最容易把两者
都做成 protocol backpressure。必须分别建模 request permit 与 network fact subscription。

### 多 frontend 与 lifetime

共享 Browser Host 后，frontend attach/detach、session-owned context 和 browser-close 权限要明确。
第一版不必支持任意多客户端写同一 target，但内部 API 不能重新把 host ownership 绑到第一个 socket。

### Event journal 内存

Network/DOM 高频事实可能撑大 journal。第一阶段只迁 navigation/lifecycle/target small facts；大 body、
DOM snapshot 和 stream 保持 owner store + handle。journal retention 和 lag policy 必须有指标。

### 双重 truth

迁移期间 frontend mirror 可能和 Browser Core current state 分叉。每个字段必须标记
authoritative/projection/cache；projection 不得反向决定 current target/page。

## 停止并重新设计的条件

出现以下任一情况，暂停扩大修改：

- Browser Core action 需要 CDP/BiDi session id 才能执行；
- Browser Host 直接构造 CDP JSON 或 `BackgroundProtocolEvent`；
- protocol actor 仍需发 noop/drive/pump 才能让 browser action 前进；
- old/new queue 都可能执行同一 navigation；
- frontend output queue 满会停止 renderer/network owner；
- 为迁移新增 sleep、retry、反复 `yield_now` 或无限 drain；
- 同一修复要复制到 CDP、BiDi、Classic 三处；
- stale completion 只能靠“当前 target”猜 owner；
- 必须先析构旧 Page 才能 commit 新 Page；
- Browser Core 和 frontend 同时保存 authoritative BrowserContext/Target/Page；
- 新 crate 造成 core/protocol/renderer dependency cycle；
- benchmark 变快是因为漏发事件、跳过 DOM/network 输出或降低正确性。

停止时记录：应成立的不变量、违反路径、当前 authority、下一步最小 differential。

## 非目标

- 不复制 Chromium 的完整多进程、Mojo、RenderFrameHost、BFCache、prerender 或 OOPIF；
- 不实现 GUI、layout、paint/compositor；
- 不在本计划中重新定义 DCL/load/Done/domstable；
- 不解决知乎 challenge、WAF、CORS 或具体站点兼容；
- 不改变 CDP/BiDi wire shape；
- 不一次重写所有 domain dispatcher；
- 不同时重构全部 Network/Fetch/body store；
- 不为了架构形式新增 OS process、thread 或 crate；
- 不承诺无证据的多客户端并发写语义。

## 待冻结决策

| 问题 | 默认方向 | 冻结阶段 |
| --- | --- | --- |
| Browser Host lifetime | raw-CDP browser 为 `serve` owner 级；WebDriver 为 session 级；CLI 为一次调用级；只有显式 attach 的 frontend 共享 exact instance | Phase 6/7 |
| frontend disconnect 后页面是否继续 | 默认继续；session-owned context 显式删除 | Phase 3/6 |
| 第一版是否跨线程 | 否，同 dedicated current-thread runtime | Phase 2 |
| 是否新增 crate | 否，先放 `moli-core` | Phase 2 |
| journal retention | bounded ring + subscriber lag 明确失败/重快照 | Phase 5 |
| 多 frontend 写同一 target | 先保证读/attach 与 owner 隔离，再定义冲突 policy | Phase 6 |
| DevTools commit participant timeout | exact request policy；断开不得 orphan | Phase 4/5 |
| Network fact 迁移粒度 | navigation/lifecycle 完成后逐 producer 迁移 | Phase 5+ |
| Browser Host 是否未来独立 process | 只按性能/隔离证据决定 | Phase 8 |

## 完成定义

长期改造完成必须同时满足：

1. Browser Host 是 BrowserContext、Target、Page 和 NavigationEngine 的唯一 mutable owner；
2. renderer navigation intent 不含 frontend/session route；
3. command navigation 与 renderer navigation 进入同一 Browser Owner request state machine；
4. `ProtocolSchedulerWork` 不含 navigation/replacement/termination browser action；
5. `CdpConnection` 不持有 strong NavigationEngine 或 authoritative BrowserContext/Target/Page state；
6. Browser Core 不依赖 CDP/BiDi wire crate，不保存 frontend command/session id；
7. raw CDP/BiDi/Classic 的 actual frontend 只通过 owner endpoint 或 typed service handle 访问 served
   Browser Host；standalone CLI/MCP 通过一次调用级 `Browser` Core facade，不能驱动 served Host receiver；
8. lifecycle/navigation/target facts 带 exact identity 和 sequence，并由 frontend 独立投影；
9. frontend command、读取、断开和 backpressure 不成为 browser progress trigger；
10. explicit interception/debug/commit hold 使用 request-scoped permit，普通 observer 没有执行权限；
11. 旧 Page/request/agent 的 late completion 不能污染 current Page/session；
12. 插入/删除任意 noop frontend command 不改变 browser trace；
13. focused、workspace nextest、fmt、clippy、raw client smoke 和 Chromium differential 通过；
14. WebFetch/CDP 成功率不因漏事件或降低正确性虚增，资源回归有前后证据；
15. 当前短期 adapter guard 只剩 attachment/投影职责，不再承担 browser progress containment。

## 相关文档

- [`cdp-current.md`](cdp-current.md)：当前 CDP stack 和剩余架构债事实。
- [`workspace-crate-map-current.md`](workspace-crate-map-current.md)：当前 crate dependency 和 ownership 约束。
- [`chromium-aligned-devtools-navigation-redesign-2026-07-27.md`](chromium-aligned-devtools-navigation-redesign-2026-07-27.md)：DevTools target/session、renderer channel、V8 session restore 和 author-JS 前 commit 设计。
- [`cdp-chromium-aligned-architecture-refactor-plan-2026-06-14.md`](cdp-chromium-aligned-architecture-refactor-plan-2026-06-14.md)：CDP actor、renderer callback 和 output ordering 的既有总计划。
- [`cdp-scheduler-owner-loop-cleanup-plan-2026-06-12.md`](cdp-scheduler-owner-loop-cleanup-plan-2026-06-12.md)：owner output channel 与删除 generic pump 的历史计划。
- [`document-milestone-navigation-completion-design-current.md`](document-milestone-navigation-completion-design-current.md)：exact DCL/load、跨 Document completion gate 和 passive progress 不变量。
- [`page-lifecycle-current.md`](page-lifecycle-current.md)：当前 wait mode 和 lifecycle 事实。
- [`webfetch-horizontal-zhihu-fix2-2026-08-01.md`](webfetch-horizontal-zhihu-fix2-2026-08-01.md)：CLI/CDP 差异、Chromium 对照、短期修复和 live evidence。
