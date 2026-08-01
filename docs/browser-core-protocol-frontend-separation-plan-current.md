# Browser Core 与协议前端分离改造计划

最后更新：2026-08-02

状态：长期架构计划。本文冻结目标边界、迁移顺序、验收条件和停止条件；除“当前实现基线”与
明确标记为已完成的项目外，不表示目标模块或类型已经实现。类型名是工作名，所有权边界比命名
优先。

## 结论

Lightmount 长期要把当前混在 `CdpConnection` / `CdpScheduler` 里的两种责任拆开：

1. **Browser Core / Browser Owner** 自主拥有 BrowserContext、Target、Page、NavigationEngine、
   navigation/replacement/termination、history、网络和 profile/storage 运行时；
2. **协议前端** 只拥有 transport、frontend session、domain enable/subscription、command correlation、
   wire shape、DevTools renderer channel 和 browser fact 的协议投影。

这里的“拆开”首先指 authority、state、lifetime 和 scheduler lane 分开，不要求立即拆 OS
process。第一阶段仍使用同一进程、同一 dedicated current-thread runtime / local executor，避免
增加 V8 thread-affinity 风险和进程级资源成本。只有 typed command/fact 边界稳定、且性能或隔离
证据支持时，才重新评估 IPC 或多进程。

这不是把当前的跨线程 CDP 和 Browser Core 搬到一起。当前 CDP scheduler 与承担 Browser Core
职责的 `CdpConnection` / `NavigationEngine` 控制状态本来就在同一个 `lightmount-cdp-owner`
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
- 现有 `lightmount-core`、`lightmount-protocol`、renderer 和应用层怎样分阶段迁移；
- 如何避免一次性重写 `CdpConnection`，以及每一阶段用什么证据验收。

本文不重新设计 V8 Inspector session restore，也不重新定义 DCL/Load/Done。相关语义分别由：

- [`chromium-aligned-devtools-navigation-redesign-2026-07-27.md`](chromium-aligned-devtools-navigation-redesign-2026-07-27.md)；
- [`document-milestone-navigation-completion-design-current.md`](document-milestone-navigation-completion-design-current.md)；
- [`page-lifecycle-current.md`](page-lifecycle-current.md)

继续负责。本文只定义它们与 Browser Core / protocol frontend 的交界。

## 当前线程拓扑与问题定性

截至 2026-08-02，`serve` 路径的相关执行拓扑是：

```text
application Tokio multi-thread runtime
  -> protocol transport / registry / service tasks

lightmount-cdp-owner dedicated OS thread
  -> Tokio current-thread runtime + LocalSet
     -> CdpScheduler
        -> CdpConnection
           -> BrowserContext / Target state
           -> active/background NavigationEngine control

render_runtime dedicated OS thread
  -> renderer owner loop
     -> V8 / DOM / parser / timer / microtask / Document lifecycle
```

也就是说：

- CDP transport 不必和 owner 在同一线程；输入通过 channel 进入 owner；
- `CdpScheduler`、`CdpConnection` 以及当前承担 browser navigation authority 的控制状态在同一个
  `lightmount-cdp-owner` 线程；
- renderer 已经是另一个专用线程，通过 command、publication 和 completion channel 与 owner 通信；
- 当前还没有独立于 protocol scheduler 的 Browser Owner queue，`CdpConnection` 同时混合协议状态
  和 authoritative browser state。

所以 403 Document 的 DCL 后 successor navigation 曾经不推进，不是 CDP 与 Browser Core 跨线程
造成的 mutex/thread deadlock，而是同一个 owner thread 内的**逻辑 progress starvation**：renderer
已经发布 navigation intent，但它被表示为 `ProtocolSchedulerWork`；protocol residence 没有选择该
work 时，Browser navigation 就没有获得 execution authority。

2026-08-02 的短期修复已经消除已知的 `pending_load` 错误全局阻塞。长期改造不再重复修这个具体
guard，而是让普通 CDP observation 从类型和所有权上失去阻塞 Browser Owner progress 的能力。

## 术语

### Browser Core

协议无关的浏览器运行时和所有权边界。它不是 GUI、paint/compositor 或完整 Chromium browser
process 的同义词。对 Lightmount 而言，它至少包含：

- browser instance 和 BrowserContext lifetime；
- Target/Page registry；
- active/background `NavigationEngine`；
- top-level navigation、history traversal、reload、popup 和 termination owner；
- profile、cookie、storage partition、network policy 和 download 的浏览器级状态；
- renderer intent 的消费与 exact browser fact 的发布。

初始实现放在 `lightmount-core` 的 browser/page orchestration 边界，不先新增 crate。

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
- WebDriver Classic；
- standalone CLI / MCP 的高层 fetch、wait 和 automation surface。

CDP frontend 额外拥有 DevToolsSession、Target domain subscription、V8 Inspector command
correlation、CDP error/event shape 和 socket flush ordering。这些状态不能搬进 Browser Core。

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

当前每个 CDP WebSocket 创建一套独立的：

```text
CdpScheduler
  -> CdpConnection
       -> BrowserContext / Target / session route
       -> NavigationEngine
       -> retained background engines
       -> protocol/domain state
       -> scheduler-visible state
  -> ProtocolAdapterScheduler
  -> renderer/V8 runtime
```

这个形状保证了 V8 thread affinity，但也把 browser instance lifetime 绑定到 frontend socket。

### `CdpConnection` 当前混合的状态

`lightmount-protocol/src/conn.rs` 当前同时持有：

- browser/session routing、Target control 和 auto-attach policy；
- BrowserContext、active/inactive context 和 Target/Page state；
- profile、permission、download、global IO、headers、network condition、cache policy；
- network collectors、cookie/storage owner 和 download registry；
- scheduler hooks、scheduler-visible queues；
- active `NavigationEngine` 和 retained background engines；
- pending Runtime/Inspector command 与 frontend session state。

这些字段目前位于一个结构里不等于它们属于同一个长期 owner。

### `CdpConnection` 初始字段归属清单

下面是基于当前 struct 的第一轮 owner inventory。标记为“拆分”的字段不能整体搬迁，需要先把
authoritative browser state 与 frontend projection 分开。

| 当前字段/字段组 | 最终方向 | 预计阶段 | 备注 |
| --- | --- | --- | --- |
| `browser_context` / `inactive_browser_contexts` | 拆分后 Browser Core | Phase 2/6 | context/target/page runtime 进 core；DevTools projection 留 frontend |
| `browser_session_ids` / `next_session_id` | CDP frontend | Phase 6 | 纯 frontend session identity |
| `auto_attach*` / discovery/filter/listener state | CDP frontend | Phase 5/6 | policy 可以向 host 注册，但 subscription truth 留 frontend |
| `target_control` | 拆分 | Phase 1/6 | BrowserTarget registry 进 core；session/agent-host route 留 protocol |
| ServiceWorker auto-attach related owner state | protocol frontend | Phase 6/7 | service worker runtime identity 来自 browser facts，attach policy 属于 frontend |
| `next_bc_id` / `next_target_id` / shared target allocator | Browser Core | Phase 6 | 外部 wire id 可保持兼容映射 |
| Page subscription generation | CDP frontend | Phase 5 | 只影响事件投影 |
| internal Runtime command id / pending Inspector await state | protocol/renderer channel | 保持 | 不进入 Browser Core command identity |
| no-session route override | 删除或收敛为 frontend typed route | Phase 1/8 | 不能被 renderer intent 依赖 |
| network request id allocator | Browser Core/network runtime | Phase 6 | protocol 使用只读 request identity |
| `window_bounds` / permission / geolocation / network condition | Browser Core policy state | Phase 6 | protocol command 只更新 policy |
| user agent / headers / cache / proxy / TLS config | Browser Core/network runtime | Phase 6 | 不是 frontend connection-local truth |
| initial storage partition / cookie/profile state | Browser Core/storage services | Phase 6 | 删除 connection-close 反向 merge owner 语义 |
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
  -> capture CommandOwnerScope/session route
  -> publish TopLevelLocationNavigationOwnerAction
  -> ProtocolSchedulerWork residence
  -> ProtocolAdapterScheduler selects residence
  -> CdpConnection executes Page-owned navigation
```

发布阶段明确不执行 navigation，protocol scheduler 中的 action 是唯一执行权。因此只要 protocol
adapter 错误地停止 residence selection，网络请求和 Page replacement 就不会开始，即使 renderer
已经产生 intent。

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

Lightmount 的长期映射是：

| Chromium 角色 | Lightmount 目标边界 |
| --- | --- |
| Browser owner | `lightmount-core` Browser Host/Page owner + fetch/storage/profile services |
| Renderer owner | `lightmount-renderer-v8` + DOM/parser/WebAPI runtime |
| DevTools control/observation | `lightmount` frontend actors + `lightmount-protocol` + renderer inspector endpoint |

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

判断：采用。这一方案直接修正 authority 和 lifetime，同时保留 Lightmount 的轻量资源目标，并为
未来 IPC 留出边界。

### 方案 D：立即复制 Chromium 多进程

做法：Browser、Renderer、DevTools 立即拆成多个 OS process 和 IPC channel。

判断：暂不采用。进程数量不会自动修复 owner 错位，反而先引入 serialization、crash recovery、
shared profile/service 和性能成本。等逻辑边界稳定后再按证据评估。

### 方案 E：每个协议各自持有 browser runtime

做法：CDP、BiDi、Classic、CLI 各保留一套 Page/navigation owner，只共享 renderer 或低层 helper。

判断：拒绝。它会固化当前 CLI/CDP 语义漂移，同一种 lifecycle/navigation 修复需要复制多次。

## 目标架构

### 进程内逻辑结构

第一版目标保持同进程：

```text
lightmount application runtime
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
`-- CLI/MCP high-level adapter
```

Browser Host 通过 typed handle 接收命令。frontend 不获得 `&mut NavigationEngine`、`&mut Page` 或
可执行 owner action 的引用。

### 两条逻辑队列的职责

`queue` 在这里首先是 execution lane / ownership mailbox，不承诺独立 OS thread。第一版可以让两者
在同一个 `lightmount-cdp-owner` current-thread runtime 上交替运行，但不得共享执行权限：

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
| Browser instance lifetime | CDP socket stack / CLI call | application + Browser Host | frontend detach 不隐式销毁 host |
| BrowserContext / Target / Page | `CdpConnection` | Browser Core | protocol 只保留投影和 subscription |
| `NavigationEngine` / retained engines | `CdpConnection` | Browser Core | 仍保持 thread affine |
| 页面 JS top-level navigation | renderer -> protocol work | Browser Owner queue | renderer intent 不含 session id |
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
PageResidenceIdentity { target, loaded_page_generation }
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
`lightmount-core`，只把最小 carrier 放进 `lightmount-page-types`。不能为了迁移让 renderer 反向依赖
`lightmount-core`。

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

协议适配器负责映射 CDP error code、BiDi error 和 CLI status。

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
  | TargetInfoChanged
  | NavigationAccepted
  | NavigationStarted
  | NavigationCommitted
  | NavigationFailed
  | PageReplaced
  | DocumentLifecycleReached { document, milestone }
  | TargetClosed
```

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

BiDi 的 `wait`、Classic page-load strategy 和 CLI wait 可以在 command accepted 后附加各自 observer，
但 observer 不能持有 navigation execution authority。

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
- CLI/high-level wait；
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

Browser Core 分离不改变 DCL/load 的 exact-Document 语义。高层 wait 通过 browser facts 实现：

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

分离后的收益是 high-level policy 可以读取统一 browser trace，不再分别给 CLI/CDP/Classic 实现
pending-navigation 扫描。

## Crate 与模块边界

### `lightmount-core`

新增或逐步形成以下工作模块：

```text
lightmount-core/src/browser_host/
  mod.rs
  actor.rs
  handle.rs
  command.rs
  outcome.rs
  fact.rs
  journal.rs
  commit_participant.rs
  context_registry.rs
  target_page_registry.rs
  navigation_owner.rs
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

### `lightmount-page-types`

只有 renderer、core 和 protocol 都必须共享的最小 neutral identity/carrier 才放这里。禁止加入：

- CDP session/command id；
- protocol event/error shape；
- Browser Host mutable registry；
- V8 handle 或 executor/channel。

### `lightmount-renderer-v8`

继续拥有：

- V8/DOM/parser/task/microtask/timer；
- exact Document lifecycle；
- renderer DevTools agent endpoint；
- immutable renderer browser intent producer。

不新增对 `lightmount-core` 的 production dependency，不直接获取 Browser Host mutable state。

### `lightmount-protocol`

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

### `lightmount`

应用层负责：

- 启动和关闭 Browser Host runtime；
- 为 CDP/BiDi/Classic/CLI 提供 handle；
- 选择 host lifetime/profile；
- 启动各 protocol frontend actor；
- 不重新实现 Page owner loop。

raw CDP `CdpScheduler` 长期只负责 frontend command/pending response/output ordering 和 fact projection
drain，不再负责 Browser Owner action selection。

### 暂不新增 crate

现有 dependency direction 已允许 `lightmount-protocol -> lightmount-core`。第一轮不要新增
`lightmount-browser-core` crate。只有满足以下条件才重新评估：

- `lightmount-core` 因 Browser Host API 产生明确、持续的依赖膨胀；
- neutral runtime 可以在不依赖 renderer/protocol 的情况下独立编译；
- 新 crate 能删除依赖环或显著缩短 build，而不是只移动文件。

## 分阶段实施

每个 phase 可以拆成多个小 PR，但一个 PR 只能有一个 execution authority。迁移期允许 adapter，
不允许 old/new owner 同时可能执行同一 action。

### Phase 0：冻结基线和名词

状态：部分完成。

已完成：

- exact Document lifecycle identity 和 typed replacement terminal；
- renderer navigation intent 在 producing turn 冻结/移动；
- passive CDP navigation progress 回归；
- pending load global guard 的短期 containment；
- Chromium browser/renderer/DevTools ownership 调研；
- `CdpConnection` 第一轮字段组 owner inventory。

仍需：

- 为 owner action/fact 增加统一 trace 字段；
- 把 inventory 中标记为“拆分”的字段细化到 authoritative/projection/cache 成员；
- 固定 CLI/CDP/BiDi/Classic 的同源 navigation trace fixture；
- 记录当前 release 的 latency/RSS/CPU 基线。

Exit gate：文档和 trace 能回答一个 navigation 的 source、owner、request、Page generation、Document、
执行 turn 和 frontend projection sequence。

### Phase 1：neutral identity 与 renderer intent 去 session 化

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

### Phase 2：提取 Browser navigation owner state

目标：先形成可以被独立 actor 拥有的 state seam。

工作：

1. 在 `lightmount-core` 形成 `BrowserPageOwner` / `BrowserNavigationOwner` 工作结构；
2. 移入 active/background `NavigationEngine` 的 ownership 和 exact Page lookup；
3. 移入 navigation request、replacement、history 和 termination 的 owner-level API；
4. `CdpConnection` 只通过 facade 调用，不直接访问 engine；
5. 现有 protocol event 先通过 adapter 返回，行为不变。

这一阶段不急于改 WebSocket lifetime，也不创建第二条 actor。目的是先让 state 边界可编译、可测试。

Exit gate：`CdpConnection` 不再直接持有或替换 active/background `NavigationEngine`，owner-level API
不接受 protocol event buffer、session id 或 CDP command shape。

### Phase 3：建立独立 Browser Owner lane

目标：让 renderer navigation 完全离开 protocol residence。

工作：

1. application/local runtime 启动 `BrowserHostActor` 和 `BrowserHostHandle`；
2. renderer intent 直接进入 Browser Owner input；
3. `TopLevelLocationNavigationOwnerAction` 从 `ProtocolSchedulerWork` 删除；
4. Browser Owner 自主 schedule next turn；
5. navigation outcome/facts 通过 typed channel 返回 protocol projector；
6. 保持同进程、同 current-thread runtime，不先引入跨线程 V8 handle。

必须新增的 actor regressions：

- `Page.navigate` response 后 parser script 导航，不发下一命令也进入 B；
- frontend 完全不 enable Page domain，renderer navigation 仍执行；
- 插入/删除 noop command，browser trace 完全相同；
- slow protocol output queue 不延迟 replacement HTTP request start；
- old Page intent 在 B commit 后被 stale-drop。

Exit gate：protocol scheduler 中不存在 renderer-sourced top-level navigation execution authority。

### Phase 4：command navigation 与 renderer navigation 汇合

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

Exit gate：Browser Core transition 不构造 `BackgroundProtocolEvent`；CDP event 是否发出由 frontend
subscription 决定，但关闭 subscription 不改变 browser trace。

### Phase 6：Browser Host lifetime 与 browser state 完整提取

目标：Browser Host 不再从属于单个 frontend connection。

按 owner inventory 分批迁移：

1. BrowserContext/Target/Page authoritative registry；
2. profile/storage partition/cookie owner；
3. network condition、cache、headers、request/body stores 的 browser portion；
4. download registry/global IO 的 browser portion；
5. permission/emulation 中真正影响浏览器行为的 state。

`lightmount serve` 先创建 Browser Host，再接受 CDP/BiDi frontend。frontend connection 保存 handle 和
projection state。CLI 创建 ephemeral Browser Host，并通过同一 command/fact API 工作。

Exit gate：drop frontend 不会 drop Browser Host；`CdpConnection` 不持有 authoritative
BrowserContext/Target/Page/engine/profile mutable state。

### Phase 7：所有 frontend 收敛

目标：删除“共享 DevTools facade 等于共享 browser owner”的过渡模型。

工作：

- raw CDP、BiDi、Classic 和 CLI/MCP 使用同一个 BrowserHostHandle；
- 各 frontend 只保留协议 wait/error/result/event mapping；
- WebDriver session-owned context 用显式 create/delete command；
- CLI fetch 的 FollowBeforeReply/domstable/selector policy 消费同一 facts；
- 删除各 adapter 的独立 page pump、pending-navigation 扫描和 browser teardown fallback。

Exit gate：给同一 Browser Host 输入相同 command/renderer intent，四类 frontend 观察到相同
browser trace；差异仅来自书面的等待与投影语义。

### Phase 8：删除迁移层并评估物理隔离

删除：

- protocol scheduler 中遗留 browser-owner work kind；
- `CommandOwnerScope` 对 browser action 的使用；
- connection close profile merge 的临时 owner 语义；
- duplicated BrowserContext/Target/Page current-state cache；
- command-followup 充当 browser progress pump 的 helper；
- 已失去用途的 generation、route override 和 adapter bridge。

随后基于证据决定是否：

- Browser Host 独立 OS thread；
- 多 Browser Host worker pool；
- 进程/IPC 隔离。

物理拆分不是完成条件。若同进程模型已满足 correctness 和资源目标，可以长期保留。

## 第一批 PR 建议

不要从“移动整个 `CdpConnection`”开始。建议按以下可审查切片推进：

### PR A：identity inventory 与 trace

- 维护本文和 field ownership inventory；
- 给 renderer intent、owner action、request、commit、fact projection 增加统一 trace key；
- 不改行为。

状态：本文和初始 inventory 已完成；production trace 尚未开始。

### PR B：renderer navigation 去 session 化

- action 只携带 neutral target/page/document identity；
- owner lookup 不再进入 `CommandOwnerScope`；
- 保留现有 scheduler，先证明路由不回退。

### PR C：Browser navigation owner seam

- `NavigationEngine` 和 page lookup 进入 core-owned substructure；
- protocol 通过无 session 的 owner API 调用；
- event adapter 暂时保留。

### PR D：renderer navigation lane cutover

- 启动 Browser Owner queue；
- renderer intent 直接发布到该 queue；
- 删除 `TopLevelLocationNavigationOwnerAction` protocol residence；
- 复用本次 passive-progress regression 作为红绿证据。

只有 PR D 稳定后，再迁 `Page.navigate`、history、termination 和 fact journal。

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

- Lightmount standalone；
- Lightmount raw CDP；
- Lightmount BiDi/Classic；
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
| Browser Host lifetime | `serve` 进程级；CLI 为一次调用级 ephemeral host | Phase 6 |
| frontend disconnect 后页面是否继续 | 默认继续；session-owned context 显式删除 | Phase 3/6 |
| 第一版是否跨线程 | 否，同 dedicated current-thread runtime | Phase 2 |
| 是否新增 crate | 否，先放 `lightmount-core` | Phase 2 |
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
7. CDP/BiDi/Classic/CLI 通过同一 handle 访问 Browser Host；
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
