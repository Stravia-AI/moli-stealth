# Moli 与 Lightpanda 内存差距动态归因报告

日期：2026-08-30

分支：`webfetch-inspection`

Moli 源码：`5f3817b00aea55a48404b051b2de9f9e3aea9c0d`

Moli release SHA-256：`13f77711fed5a185eb5d9cff178275d5172aa0c64df28684a59bcea47d0a68c1`

Lightpanda SHA-256：`9255a9f5f4c0cc3381bd8c42db1937ea997aab6a5426be8a96203b9a4e4291b9`

## 2026-08-31 深挖复核：当前结论

> 本节是对下文 2026-08-30 初版的增量复核，使用当前分支
> Rust 源码状态 `e3e59a449`。下文旧数字保留用于追溯，但涉及“当前差距”和构建参数的
> 判断以本节为准。已经完成 A/B 的结果与待验证推断分开记录。

此前默认 CGU=16 的调查二进制 SHA-256 为
`32734bdb97fa5222c0d3f35efadcf466a9c5df3d352f85c158af0ba98423025c`；最终源码
`e3e59a449` 的 CGU=1/no-LTO 候选 SHA-256 为
`d767d374e8557aa6d18a180106fabb7ebfbd35daa91e64b7f8ed962c1b478583`；
Lightpanda 仍为 `1.0.0-nightly.6240+37391687`，SHA-256 与文首一致。

### 2026-08-31 最终动态闭环：大 DOM 峰值已消除，PGO 不进入范围

本小节记录本轮最后一组复核。PGO 按项目决定完全排除：没有用 PGO 数据解释任何
收益，也不把 PGO 留作待完成优化。allocator 同样维持 jemalloc，不以更换 allocator
换取冷启动数字。下面所有 Lightpanda 结果均来自官方二进制
`9255a9f5f4c0cc3381bd8c42db1937ea997aab6a5426be8a96203b9a4e4291b9`，不使用调查中
曾为隔离实验修改过的 Lightpanda 构建。

除非小节另行注明，下面“当前候选”的 Moli 都是同一源码状态以
`CARGO_PROFILE_RELEASE_CODEGEN_UNITS=1` 构建的 no-LTO release，并继续使用 jemalloc；
它不是仓库默认 `codegen-units=16` 构建。这样写是为了把已经动态验证的 CGU=1
构建候选与尚未修改的产品默认配置分开，不能把下列 28.983 MiB 基线误报成当前默认
release 的数字。PGO 没有参与构建或数据采集。

#### 公平空页面基线是 28.983 MiB，不是笼统的 45 MiB

在双方使用相同 parser/count=0 页面、DOMContentLoaded 后先完成内容校验、任何
引擎专用 GC/诊断 probe 之前取样、双方关闭 executable THP、交替运行 10 轮且每个
样本新建进程的口径下：

| 指标 | Moli 中位数 | 官方 Lightpanda 中位数 | 差值 |
|---|---:|---:|---:|
| DCL PSS | 53.714 MiB | 24.731 MiB | **+28.983 MiB** |
| 主二进制执行页 | 36.115 MiB | 15.275 MiB | **+20.840 MiB** |
| unnamed anonymous | 8.748 MiB | 2.273 MiB | +6.475 MiB |
| brk heap | 0 | 2.811 MiB | -2.811 MiB |
| 主二进制非执行页 | 6.857 MiB | 4.174 MiB | +2.684 MiB |
| 其中 relocation COW/anonymous | 2.426 MiB | 0.105 MiB | +2.320 MiB |
| 共享库 | 1.818 MiB | 0.120 MiB | +1.698 MiB |
| 线程栈 | — | — | +0.008 MiB |

匿名映射与 brk 合并后的净差为 +3.664 MiB；主二进制非执行页扣除 COW 后只剩
+0.363 MiB file-backed 差。上述互斥物理桶合计约 28.9 MiB，余量来自分别取中位数
和舍入。下文旧的 31.944 MiB 来自更宽的 Page/Runtime/Network 生命周期和不同 settle
边界，适合保留作历史生命周期实验，但不能与这个 parser/count=0 基线直接相减后
归因给源码改动。证据文件为
`results/memory-deep-20260831/moli-current-vs-lightpanda-official-parser-count0-preprobe-thp-off-paired-10r.json`。

jemalloc 的无 GC、无 V8 probe 独立运行也把匿名页说明白了。10 轮有效样本的中位数为：

| jemalloc 指标 | 中位数 |
|---|---:|
| allocated | 3.882 MiB |
| active | 4.471 MiB |
| metadata | 4.743 MiB |
| resident | 9.721 MiB |
| mapped | 17.393 MiB |
| retained | 2.107 MiB |
| tcache bytes | 1.186 MiB |
| dirty pages | 0.959 MiB |

`allocated` 是 `active` 的子集，`resident` 又覆盖 active extent 与 metadata，不能把
这些数相加。物理上约 8.775 MiB unnamed anonymous 与约 4.471 MiB active、
4.743 MiB metadata 的量级一致；这不是尚未定位的 45 MiB live-object 泄漏。该数据
来自 `moli-current-cgu1-stats-parser-count0-no-gc-thp-off-10r-valid.json`。一次早期运行
因为 harness 在错误目录等待固定输出而 10/10 超时，页面实际已到 DCL；它被明确记为
harness 失败，没有混入有效样本。

#### 20 万节点峰值的真实根因与修复收益

直接写入 live `DocumentFragment` 的 parser 改造首先消除了 clone/staging 双树峰值。
同一页面 20 万节点、内核 `VmHWM`、1 秒低干扰采样、交替 10 轮时，direct parser
相对旧 clone parser 的成对 HWM 中位减少 **11.621 MiB**，10/10 轮都更低。它不是
稳态优化：显式 purge 后 5 万节点的 retained PSS 反而约高 2.21 MiB，因此只按“降低
解析峰值”接受，不把它包装成所有口径都下降。

随后在 20 万节点导航达到约 145 MiB RSS 时触发 sampled jemalloc live profile。
估算的 65.06 MiB live allocation 中，`StyleMutationEffect::from_dom_mutation_effects`
一条 owner stack 占约 **27.375 MiB**。原因是一次 fragment child-list 已经携带完整
20 万 `added_nodes`，tree effects 又把相同节点逐个展开为 20 万个
`ConnectedSubtree`；它们被放进以最大枚举 variant 为 entry 大小的 `IndexSet`，形成
巨大的短命 hash table。其他显著 owner 包括 `NativeDom::create_node` 18.340 MiB、
`TreeAdoptionPlan::before_adoption` 2.625 MiB、query-index subtree 2.500 MiB、V8 string
转 Rust 2.500 MiB 和 child-list payload 1.750 MiB。sampled profile 是归因证据，不把
其估算字节当成精确 PSS。

修复没有删除样式 fallback 语义，而是把 connected/disconnected subtree roots 改为
一个 `Arc<[DomHandle]>` 批次；当 roots 与 child-list 的 added/removed payload 完全
相同时直接共享同一块数组。跨 owner Document 时才拆批，runtime fallback 仍展开并
保留每个 root。与修复前 direct-parser 二进制做 10 轮成对 A/B：

| 20 万节点指标 | 批量 roots | 修复前逐 root | 成对差中位数 |
|---|---:|---:|---:|
| 内核 `VmHWM` | 105.779 MiB | 152.105 MiB | **-46.818 MiB** |
| DCL PSS | 96.327 MiB | 98.200 MiB | -3.247 MiB |
| DCL anonymous PSS | 53.971 MiB | 55.459 MiB | -3.441 MiB |
| DCL elapsed | 5077.1 ms | 5154.4 ms | -65.9 ms |

`VmHWM` 10/10 轮下降，范围 -48.812 至 -43.836 MiB；DCL PSS 只在 7/10 轮下降，
符合“释放前峰值被消掉、稳态主要不变”的设计目标。空 parser/count=0 的另一个 10 轮
A/B 中，HWM 成对中位差 +0.104 MiB、PSS +0.086 MiB，范围均跨零，没有空页基线
回归。两组均为零产品失败。证据文件分别为
`moli-batched-style-vs-direct-silent-parser-vmhwm-200k-paired-10r.json` 和
`moli-batched-style-vs-direct-silent-parser-count0-paired-10r.json`。

#### 修复后与官方 Lightpanda 的动态斜率

最终候选与官方 Lightpanda 使用同一 fixture、0/20 万节点、THP-off、每样本新进程、
交替 10 轮，共 40 个样本，零失败：

| 指标 | Moli count=0 | Moli 20 万 | LP count=0 | LP 20 万 |
|---|---:|---:|---:|---:|
| `VmHWM` | 56.102 MiB | 105.018 MiB | 22.953 MiB | 85.539 MiB |
| DCL PSS | 53.673 MiB | 94.072 MiB | 20.838 MiB | 72.502 MiB |
| DCL anonymous PSS | 11.416 MiB | 51.328 MiB | 2.859 MiB | 53.277 MiB |
| DCL 主二进制执行页 | 36.088 MiB | 36.525 MiB | 14.025 MiB | 15.213 MiB |

从 0 到 20 万节点，Moli 的 HWM 增长中位为 **48.916 MiB**，Lightpanda 为
62.594 MiB；Moli 少增长 **11.871 MiB**。PSS 增长分别为 40.478 与 51.669 MiB，
Moli 少增长 **11.078 MiB**。20 万节点时总 PSS 仍多 21.577 MiB，但主二进制执行页
正好多 21.250 MiB，anonymous PSS 中位反而少 2.121 MiB。换言之，当前大 DOM 的
额外动态内存增长已经不比 Lightpanda 差；剩余总量差基本回到静态代码 working set、
重定位 COW 和共享依赖，而不是另一个未解释的 per-node heap 放大器。证据文件为
`moli-batched-style-vs-lightpanda-official-parser-vmhwm-slope-paired-10r.json`。

上述 10 轮候选之后又修正了低基数 Window wrapper 的跨 realm brand 标记；高基数
Node wrapper 仍是单字段。为避免用修复前二进制代替最终交付物，使用上面列出的最终
SHA 另做了 5 轮 exact-final confirmation：双方仍为 THP-off、同一 parser fixture、
0/20 万节点、交替顺序、每样本新进程，并且在任何引擎专用 GC 或 heap diagnostic
之前取自然 DCL 样本。20 个进程零失败：

| exact-final 指标 | Moli count=0 | Moli 20 万 | LP count=0 | LP 20 万 |
|---|---:|---:|---:|---:|
| `VmHWM` | 56.191 MiB | 106.813 MiB | 22.223 MiB | 84.977 MiB |
| 自然 DCL PSS | 53.633 MiB | 93.604 MiB | 20.114 MiB | 71.761 MiB |
| unnamed anonymous PSS | 8.879 MiB | 48.660 MiB | 2.266 MiB | 51.418 MiB |
| 主二进制执行页 | 36.029 MiB | 36.217 MiB | 13.529 MiB | 14.697 MiB |
| DCL elapsed | 8.17 ms | 5136.90 ms | 1.28 ms | 184.77 ms |

最终 Moli 的 HWM 增长成对中位为 50.539 MiB，Lightpanda 为 62.824 MiB；逐轮
“Moli 增长减 Lightpanda 增长”的中位是 **-11.965 MiB**，5/5 轮均为负。自然
DCL 不做 GC，因 allocator decay 波动更大：20 万节点总 PSS 中位差为 21.844 MiB，
其中执行页差 21.520 MiB，而 unnamed anonymous 中位反而少 2.758 MiB。这一轮不替换样本
更多的 10 轮主结果，只证明最终修复后的确切二进制仍保持同一结论。证据文件为
`moli-final-clean-vs-lightpanda-official-parser-vmhwm-slope-preprobe-paired-5r.json`。

这不代表两边已经等价：20 万节点 DCL 中位约为 Moli 5095 ms、Lightpanda 169 ms，
仍有独立的解析/DOM 吞吐问题；本节只闭环内存，不用吞吐差异掩盖它。对内存而言，
剩余约 20–29 MiB 已能按实际驻留页逐桶解释，其中最大项是 Moli 更完整的 renderer、
CDP、Rust 泛型/async 与 Web API 功能面所触达的执行代码。它仍可通过更细的页面功能
延迟加载继续优化，但不再是“45 MiB 不知道在哪里”的状态。

### 45 MiB 应当怎样陈述

固定最小页面、单进程、每次使用新 inode、双方交替启动、DOMContentLoaded
后冻结进程再读取 `/proc` 的结果如下：

| 口径 | Moli PSS 中位数 | Lightpanda PSS 中位数 | 配对差中位数 |
|---|---:|---:|---:|
| 当前默认运行环境 | 66.377 MiB | 20.887 MiB | **+45.487 MiB** |
| 双方进程级关闭 executable THP | 61.475 MiB | 20.896 MiB | **+40.584 MiB** |
| Moli 单 CGU、无 LTO；双方关闭 THP | 52.843 MiB | 20.899 MiB | **+31.944 MiB** |

因此，“Moli 固定比 Lightpanda 多 45 MiB”不是足够严谨的陈述。45.487 MiB
是默认环境的一次稳定基线，但其中约 4.9 MiB 是本机 executable THP 是否形成所
放大的观测差；THP 形成具有时序性，同一 Moli 的 THP-off/default 直接配对中位
只差 -0.311 MiB，不能把 4.9 MiB 写成关闭 THP 后必得的产品收益。更适合讨论
实现差异的是双方同样关闭 THP 后的 **40.584 MiB 结构性对照**。

更重要的是，当前 release profile 的默认 `codegen-units=16` 本身贡献了一块很大
的可消除代码 working set。只改成 `codegen-units=1` 且不开 LTO 后，结构性差距
已经下降到 **31.944 MiB**。这不是静态 ELF 尺寸推断，而是 10 轮真实进程配对结果。

### 单 CGU 后剩余 31.944 MiB 的物理拆分

| 互斥物理类别 | Moli - Lightpanda 配对差中位数 | 占剩余差距 |
|---|---:|---:|
| 主二进制已驻留执行页 | **+22.689 MiB** | 71.03% |
| 匿名映射与 brk | **+5.230 MiB** | 16.37% |
| 主二进制重定位后 COW 页 | **+2.316 MiB** | 7.25% |
| 其他文件映射 | **+1.030 MiB** | 3.22% |
| 主二进制非执行 file-backed 页 | **+0.438 MiB** | 1.37% |

五项合计 31.703 MiB；剩余约 0.24 MiB 来自 kernel/special、共享页分摊以及对各桶
分别取中位数造成的舍入差。这里没有把 JS heap、allocator 或线程栈重复相加：它们
都包含在匿名桶中。

10 轮中，单 CGU Moli 的 DCL PSS 范围为 52.423–53.383 MiB，Lightpanda 为
20.894–20.909 MiB；配对差范围为 31.518–32.475 MiB。Lightpanda 的稳定性也说明
31.944 MiB 不是由对照组波动碰巧得到的。

### 构建参数 A/B：目前应选单 CGU、无 LTO

八个构建都来自同一源码，双方都经相同 THP wrapper 启动，每项内存结论均来自 10 轮
交替顺序配对。表中明确写出各自 reference，避免把不同实验的中位数直接相加：

| Moli release 参数 | DCL PSS A/B 结果 | 执行页 A/B 结果 | `.text` | 结论 |
|---|---:|---:|---:|---|
| 默认：CGU=16、无 LTO | 基线 | 基线 | 80.702 MiB | 当前正式构建 |
| **CGU=1、无 LTO** | **-8.564 MiB** | **-7.746 MiB** | 72.769 MiB | 当前最优；10/10 轮降低 |
| CGU=1、无 LTO、lld safe ICF | **相对 CGU=1 -0.583 MiB** | **-0.391 MiB** | 69.347 MiB | 内存小幅降低，但 DOM 热路径慢 1.1%–2.6%，排除 |
| safe ICF + machine outliner r0 | **相对 safe ICF -1.046 MiB** | **-1.041 MiB** | 65.997 MiB | 10/10 轮降低，但 10k DOM 导航中位慢 55%，排除 |
| safe ICF + machine outliner r2 | **相对 r0 -0.063 MiB** | **-0.074 MiB** | 65.986 MiB | 范围跨零，额外 rerun 无有效收益 |
| CGU=1、ThinLTO | -6.364 MiB | -5.643 MiB | 77.099 MiB | 比单 CGU 回吐约 2.2 MiB |
| CGU=1、Fat LTO | **相对 CGU=1 无 LTO +1.249 MiB** | **+1.002 MiB** | 75.713 MiB | 文件更小，但运行 working set 更大 |
| CGU=16、ThinLTO | **+2.722 MiB** | **+2.582 MiB** | 84.673 MiB | 当前组合下反而扩大 working set |

单 CGU 的 DCL 配对差范围为 -9.336 至 -8.179 MiB；其中 COW 页固定减少
0.328 MiB，匿名页中位减少 0.223 MiB。收益在 `Target.createTarget` 阶段已经达到
-7.950 MiB，证明主要作用是减少首次页面/runtime 路径所触达的重复泛型实例和较差
代码布局，而不是页面正文恰好少分配了对象。

单 CGU 也通过了独立性能闸门。双方关闭 executable THP，每个二进制运行 3 个独立
进程；每个进程先丢弃 5 次 warmup，再在页面内直接用 `performance.now()` 测量 35
次“创建、挂载并移除 10,000 个 DOM 元素”。相对默认 CGU=16，CGU=1 三组内部耗时
中位数分别降低 **4.7%、1.7%、2.3%**，均值分别降低 5.1%、1.1%、2.0%；GC 后 PSS
逐组降低 6.9–8.1 MiB。因此它不是用吞吐交换内存，当前证据同时支持其内存和热路径
性能方向。

这个 A/B 修正了此前“ThinLTO 可省约 6.36 MiB”的不完整判断：组合实验里的收益
来自 `codegen-units=1`，ThinLTO 在 CGU=1 基础上反而增加约 2.2 MiB DCL PSS。
Fat LTO 也没有扭转方向：其磁盘文件从 152.403 MB 降到 151.018 MB，`.data.rel.ro`
从 2.159 MiB 降到 1.913 MiB，但 `.text` 增加 2.944 MiB；真实 DCL 配对差中位数
为 **+1.249 MiB**，范围 +0.311 至 +1.605 MiB。Fat LTO 构建耗时 7 分 46 秒，
最终链接 rustc 峰值 RSS 约 4 GiB。这里再次证明文件大小不能代替 present-page 测量。

safe ICF 同样说明了这一点：它让 `.text` 静态减少 3.422 MiB、整个 ELF 减少
4.551 MB，但最小文档 DCL 实际只少 0.391 MiB executable PSS；重定位 COW 页固定
再少 0.109 MiB，总 PSS 配对中位数少 0.583 MiB，范围 -1.136 至 +0.437 MiB。
收益在 target-created 阶段已经出现，总 PSS 中位数为 -0.426 MiB。

重复 target 验收进一步限定了它的取舍。关闭 executable THP 后，3 个独立进程各
连续创建/关闭 30 个含 10,000 个元素的页面，取第 6–30 个 target：safe ICF 的 DCL
PSS 逐组降低 1.6/0.8/2.0 MiB，关闭后降低 2.5/0.4/2.0 MiB。未关闭 THP 时曾观测到
候选高出约 26 MiB；匿名页相同、差值全部来自主 ELF，而相同实验经
`PR_SET_THP_DISABLE` 后方向反转，证明那是 executable THP fault-in 放大的观测伪影，
不是 ICF 引入的 heap 回归。

性能不能同样通过。外层 DCL wall time 呈约 68/96 ms 双峰，会被事件调度边界放大，
所以另做页面内直接计时：3 个独立进程、每个 35 个有效样本，共 105 次 10,000 元素
创建/挂载/移除。safe ICF 相对单 CGU 的三组内部中位数分别慢 **1.1%、1.3%、2.6%**，
均值分别慢 1.5%、0.7%、1.8%，方向逐组一致。以冷页仅 0.583 MiB 的收益，不接受约
1%–3% 的 DOM 热路径回归；因此 **safe ICF 不进入产品 release 配置**。

machine outliner 也做了单独的动态验收。本机 LLVM 的
`machine-outliner-reruns=N` 表示初始运行一次后再运行 N 次，默认 N=0；默认
`outliner-benefit-threshold` 只有 1 byte。r0 将 `.text` 再缩小 3.350 MiB，DCL
执行页与总 PSS 分别少 1.041/1.046 MiB，配对总差范围 -1.517 至 -0.868 MiB；但它
同时生成 **77,318 个** outlined helper，helper 中位只有 11 bytes，最终二进制中
有 **764,871 个**对这些 helper 的调用。unwind 信息随之膨胀：`.eh_frame` 增加
1.468 MiB，整个 ELF 反而比 safe ICF 大 2.245 MB。server-ready 执行页还增加
0.646 MiB，说明它主要改变页面热路径布局，不是所有生命周期点都降低。

更关键的是性能反例。3 个独立进程各连续创建、导航、关闭 30 个含 10,000 个 DOM
元素的 target，取 target 6–30 稳定段中位数；顺序为 outliner/ICF、ICF/outliner、
outliner/ICF。outliner 三组导航中位为 139/144/145 ms，safe ICF 为 71/93/103 ms，
逐组慢 42–69 ms，中位回归 **51.2 ms / 54.9%**。关闭 target 逐组都更慢，中位
增加 0.812 ms / 14.6%。因此 r0 不能进入 release。

增加到 r2 也没有改善取舍：`.text` 相对 r0 只再少 12,416 bytes，却新增 11,811
个 helper 和 15,488 个 outlined call，`.eh_frame` 再增加 236,096 bytes，ELF
再增大 883,368 bytes。10 轮 DCL 总 PSS 边际只有 -0.063 MiB，范围 -0.457 至
+0.705 MiB。另一个构建保持 LLVM 默认的 `linkonce_odr=false`；其 helper 数、
call 数和 section 尺寸与显式打开该开关完全一致，排除了额外 linkonce 开关是性能
回归来源的可能。结论是：**更多 machine-outliner rerun 并不会更好，当前默认
1-byte 收益阈值的 outliner 整体排除**。本轮已明确不再测试 PGO，因此不保留
conservative-PGO outliner 作为待办。

是否把 CGU=1 作为产品默认值，还需要完成启动延迟、WebFetch 吞吐和 CI 构建时长
验证；内存方向已经有稳定证据。

### Startup snapshot 的真实量级

先区分两层 snapshot。Moli 当前使用的 V8 二进制已经内嵌 V8 自身的默认 startup
snapshot；`CreateParams::default()` 创建 isolate 时会走
`v8::internal::Snapshot::Initialize`，二进制中也存在
`Snapshot::DefaultSnapshotBlob`。Moli 真正缺少的不是这一层，而是由 embedder 生成、
额外包含 Web API 模板和页面 Context 的 **application snapshot**。源码里的
`"cold, no snapshot"` timing 文案只能理解为“没有 Moli application snapshot”，
不能理解为 V8 从完全空白 heap 启动。

为了避免用 Lightpanda 的设计静态推测 snapshot 收益，使用 Lightpanda 官方二进制
对应的精确源码提交 `373916873f46316527f8de5a67a0bc6497b1087c` 和 V8
`14.0.365.4`，构建了“内嵌 startup snapshot”和“不内嵌、启动时生成 snapshot”
两个版本，并做 10 轮交替配对。内嵌 snapshot 在 DCL 时稳定节省：

| 类别 | 内嵌 snapshot - 启动时生成 |
|---|---:|
| 总 PSS | **-2.641 MiB** |
| 执行页 | -2.094 MiB |
| 匿名页 | -0.914 MiB |
| snapshot/file-backed 数据页 | +0.367 MiB |

总差范围只有 -2.647 至 -2.625 MiB。这证明 snapshot 对 Lightpanda 有价值，但至少
在同一 V8、同一 Lightpanda 实现里，它解释不了 40.6 MiB 结构性差距。该对照的
“无内嵌”版本仍会在进程启动时创建 snapshot，再用它创建 context，因此不能直接
把 2.641 MiB 搬到 Moli 名下。

两边 application 层的接线差异已经核到精确版本源码。Lightpanda 在构建 snapshot
时创建全部 `FunctionTemplate`，把模板作为 isolate data 写入，并把 Page 与 Worker
两个完整 Context 都加入 snapshot；运行时用 `Context::FromSnapshot` 恢复。Moli 则在
每个 document isolate 中重新构造 420 条 constructor metadata、`Window`/global
template 和 native bridge，再用该 template 调 `Context::new`。Moli 的 callback
数量和功能面都更大，不能直接复用 Lightpanda 的 blob。

Moli 自身的等价 A/B 已经完成。隔离原型把 Window/global、cross-origin global、
实际 ready 的 `FunctionTemplate`、全部 NativeBridge wrapper `ObjectTemplate` 和一个
Page Context 写入 660,768-byte blob；页面 host pointer、DOM、存储、安全 token、
inspector 与 microtask policy 仍在恢复后安装。实验从同一个最终 PIE ELF 提取并按
符号名固定 3,568 个 callback trampoline/raw callback external references；生成器与
最终 ELF 的符号序列逐项相同，最终 binary SHA-256 为
`714acdee7bfa569473b7a63ae255583e7b1807eabe5053a6102a83e76200d123`。开关两边使用
同一个 ELF，并关闭 executable THP，因此磁盘大小、代码布局和大页不会污染配对。

10 轮交替配对的最小文档结果如下：

| GC 后指标 | snapshot - direct bootstrap |
|---|---:|
| 总 PSS | 中位 **-0.258 MiB**，均值 -0.178 MiB，范围 -0.664～+0.294 MiB |
| unnamed anonymous PSS | 中位 -0.127 MiB |
| binary PSS | 中位 -0.045 MiB，范围跨零 |
| V8 used heap | 稳定 -0.040 MiB |

GC 前 DCL 总 PSS 中位反而为 +0.091 MiB；GC 后 10 组中仍有 4 组增加。也就是说，
完整 application snapshot 的真实内存作用只有约 0.1～0.3 MiB，低于本机进程级
PSS 波动，不能记作稳定收益，更不可能解释此前估计的 2.5 MiB cage 差。

30 轮同二进制内部 timing 给出了为什么它不适合 Moli 的确定解释：snapshot
反序列化使 `Isolate::new` 中位慢 0.602 ms；恢复 Rust registry/handle 使
`IsolateBootstrapCache` 中位慢 0.406 ms；虽然 NativeBridge 模板恢复比重建快
0.087 ms，isolate 初始化总计仍中位慢 **0.898 ms**。完整 CLI wall time均值近零、
中位差 +0.438 ms，已被其他阶段噪声淹没。Moli direct bootstrap 本来只花约
0.30 ms 构建高度 lazy 的模板状态，而 Lightpanda 的“无内嵌”对照会在启动时创建
整份 snapshot；两边 2.641 MiB 与约 0.2 MiB 的不同量级符合各自架构，而不是 Moli
漏接一个现成 2.6 MiB 优化。

生产实现仍应由 binding declaration 同源生成 external references，不能依赖实验的
ELF symbol sidecar；但由于完整原型同时没有稳定内存收益并增加 isolate 初始化时间，
本轮明确**不产品化 Moli application snapshot**。

### 差距在什么时候形成

单 CGU Moli 与 Lightpanda 的结构性配对差中位数：

| 生命周期点 | PSS 差 |
|---|---:|
| server ready | +1.201 MiB |
| browser WebSocket connected | +0.239 MiB |
| `Target.createTarget` | **+27.865 MiB** |
| attached | +28.211 MiB |
| Page/Runtime/Network enabled | +29.263 MiB |
| 最小文档 DOMContentLoaded | **+31.944 MiB** |

这把优化边界限定得很清楚：约 27.6 MiB 的相对差是在首次 materialize page/V8/DOM
Web API 环境时一次性产生的；启用三个 CDP domain 再增加约 1.05 MiB，最小正文导航
再增加约 2.68 MiB。网络响应正文、复杂 DOM 或 JS heap 不是剩余差距的主因。

### 剩余执行页：不是一个模糊的“Rust/V8 太大”

对同一轮 DCL 时 present 的每个 4 KiB executable page，以页中点所在符号归类。
该方法的物理页总数是精确的；混合承载多个函数的页面只能归到一个符号，因此逻辑
分类是近似归因，不能当作静态 section 大小。单 CGU Moli 为 9,127 页
（35.652 MiB），Lightpanda 为 3,287 页（12.840 MiB）：

| 逻辑代码组 | Moli 驻留页 | Lightpanda 驻留页 | 解释 |
|---|---:|---:|---|
| Moli renderer | 8.316 MiB | — | page runtime、native bridge、context host 与绑定 |
| Moli protocol/CDP | 4.926 MiB | — | command dispatch、session/target/domain 状态 |
| Rust runtime/泛型容器/async/serde | 5.992 MiB | 0.176 MiB | `core`/`alloc`/HashMap/Tokio 等被 Moli 路径单态化触达的代码 |
| V8/inspector/cppgc | 5.449 MiB | 4.523 MiB | 同属 V8，但 Moli 首页多触达约 0.926 MiB |
| DOM/style/parser | 1.527 MiB | 0.172 MiB | 两边实现广度和解析/样式架构不同 |
| Lightpanda Zig browser/CDP/network 等 | — | 4.148 MiB | Lightpanda 自身浏览器实现 |

Moli 在 server ready 和 browser WebSocket connected 时的执行页反而分别比
Lightpanda 少约 0.64 MiB 和 0.76 MiB。创建首个 target 后，Moli 执行页从
8.465 MiB 跳到 32.215 MiB，Lightpanda 只从 9.227 MiB 到 11.215 MiB；因此
代码页差不是 CLI/server 常驻开销，而是首次页面 bootstrap 的 eager working set。

源码路径与逐页结果相互印证，但后者才是判断依据：Moli 已把除 `Window` 外的全局
构造器设为 lazy；然而构建唯一 eager 的 `Window` 模板时，统一
`install_constructor_template_bindings()` 仍依次进入 SVG、IndexedDB、WebAudio、
WebRTC、OPFS、Streams 等所有 installer 做名称判断。空白页的 target-created
驻留页中已经出现这些模块。

已经为此做了隔离构建：`Window` 直接跳过该统一 fan-out，其 own bindings 仍由既有
`install_window_own_template_bindings()` 安装。10 轮配对中，target-created 执行页
只减少 0.246 MiB、总 PSS 减少 0.153 MiB；到 DCL 执行页减少 0.152 MiB，但总
PSS 配对中位数为 **+0.057 MiB**，范围 -0.441 至 +0.672 MiB。结论是它可以作为
代码整洁性小改进，却不是内存主解；逐页符号落入多个模块还受 4 KiB 同页混排影响，
不能把所有这些页面都归因给 no-op fan-out。

功能面也不是等价的。冻结内存快照完成后再读取 API（因此不会污染上述 PSS），Moli
的 `globalThis` 有 674 个 own property，Lightpanda 有 290 个；共同 284 个，Moli
独有 390 个（其中 28 个为 `__moli`/`__lm` 内部桥接名），Lightpanda 独有 6 个。
Moli 的 `Document` 原型链 own-property 总数为 255，Lightpanda 为 146；body
原型链为 331 vs 247。属性数量不能线性换算成 MiB，但它证明 renderer/V8 代码与
heap 差中有一部分来自 Web API 完成度，而不只是实现低效。优化目标应是延迟物化或
snapshot 化这些绑定，不能为了追平数字直接删除对外能力。

### 匿名页与 allocator：为什么不切换系统分配器

单 CGU 最小页的 Moli 匿名 PSS 约 9.08 MiB，Lightpanda 约 3.85 MiB。Moli 的
jemalloc 全采样 live heap dump 在 DCL 只记录到 2.271 MiB 请求字节；其中 isolate
创建路径约 0.625 MiB、protocol/router 相关路径约 0.466 MiB、page bootstrap
约 0.325 MiB、scheduler 路径约 0.275 MiB、context bootstrap 约 0.160 MiB、
OwnerWakeQueue 约 0.098 MiB。live bytes 与 PSS 不能直接相减：jemalloc 的 size-class
舍入、active extent、metadata 和保留页均已包含在匿名 PSS 中。

线程栈不是 5.230 MiB 匿名差的主因。DCL 时 Moli 有 18 个线程、Lightpanda 有
19 个线程；通过各线程 stack pointer 找到实际 VMA 后，栈 PSS 中位数分别为
0.621 MiB 和 0.262 MiB，差 **0.359 MiB**。

进一步保存每个匿名 VMA 后，可以把整个桶近似拆平。两边 V8 pointer-compression
cage 内的 committed 映射都具有低地址、页对齐的小块形态；Moli 中位 3.340 MiB，
Lightpanda 0.848 MiB，差 **2.492 MiB**。高地址 unnamed allocator 映射加
Lightpanda 的 brk 分别为 5.234 MiB 和 2.785 MiB，差 **2.449 MiB**；再加上述
栈差 0.359 MiB，合计约 5.30 MiB。与总桶 5.230 MiB 的约 0.07 MiB 差来自只做
3 轮以及分别取中位数。`low-address V8 cage` 是依据 VMA 地址、对齐、尺寸和随
isolate materialization 增长作出的归类；它不是内核提供的映射名称。

生命周期也支持该归因：Moli 的 cage-shaped PSS 从 server ready 0.043 MiB 增到
target-created 2.043 MiB，再到 DCL 3.340 MiB；Lightpanda 则为 0.043、0.848、
0.848 MiB。Moli 的 `Runtime.getHeapUsage` 同时报告 2 MiB V8 heap、约 1.75 MiB
used；Lightpanda 未实现该 CDP 命令，故不伪造一个 heap 数字与它比较。这里说明
application-level snapshot/lazy binding 的潜在收益主要落在约 2.5 MiB cage 差和
部分代码页，而不是 30–40 MiB。

系统 allocator 的 10 轮冷启动配对确实让 Moli DCL 总 PSS 中位数少 1.298 MiB，
其中匿名页少 0.713 MiB；但 3 个独立进程、每个连续创建/导航/关闭 30 个含 10,000
DOM 元素页面的复验中，稳定阶段系统 allocator 的活跃 DCL PSS 三组中位数分别为
103.2、101.2、114.4 MiB，jemalloc 为 98.2、97.1、98.6 MiB。关闭 target 的耗时
系统 allocator 三组都更慢，中位增量约 1.19 ms（约 20%）；导航耗时受机器噪声
影响较大，三组方向不一致。结论是：全局切 allocator 只优化冷启动表象，却恶化长寿
命页面循环的保留内存，当前明确排除。

### 其他文件映射：1 MiB 已全部落到具体依赖

保存完整 mapping group 的 3 轮复验中，Moli 其他 file-backed PSS 中位数为
1.061 MiB，Lightpanda 为 0.062 MiB。约 0.999 MiB 差可以完整解释：

| 来源 | Moli | Lightpanda | 差/含义 |
|---|---:|---:|---|
| `libstdc++.so.6` | 0.530 MiB | 0 | CED 的 `cxx -> link-cplusplus` 引入；Moli 同时还有 V8 静态 libc++ 代码 |
| font/image/XML/compression 链 | 0.415 MiB | 0 | fontconfig、freetype、png、expat、bz2、brotli、zlib |
| libc/libm/loader/libgcc | 0.115 MiB | 0.046 MiB | 系统 runtime 差约 0.069 MiB |
| NSS | 0 | 0.016 MiB | Lightpanda 独有，抵消少量差距 |

`readelf` 与 Cargo 反向依赖确认，直接的 `libstdc++` NEEDED 来自
`moli-encoding-detector -> compact-enc-det -> cxx -> link-cplusplus`。这提供了
一个比“静态链接所有库”更窄的候选：把 CED/字体图片栈延迟加载到实际需要编码检测、
字体或图片时。静态链接本身只会把页移入主二进制，不能视作已得收益；延迟加载仍需
单独 A/B 和发布打包评估。

CED 的静态 `libstdc++` 隔离构建已经实际尝试过，但不能作为产品方案：V8 的
`rusty_v8` archive 已静态包含 Chromium libc++/libc++abi，而把 GCC 的
`libstdc++.a` 再链接进同一个 ELF 会在 `std::exception`、`std::logic_error`、
`std::runtime_error` 等 ABI 符号上产生重复定义，最终链接失败。这不是 linker wrapper
写法造成的测量失败，而是同一进程混入两套静态 C++ runtime 的结构冲突。可行边界只剩
两类：让 CED 使用与 V8 完全一致的 Chromium libc++ ABI，或把 CED 做成首次检测时
`dlopen` 的独立 DSO；前者受 V8 私有 libc++ revision/ABI 约束，后者增加发布制品和
加载失败面。它们的收益上限仅是当前最小页约 0.530 MiB 的 DSO PSS，不能在没有完整
编码回归与真实 A/B 前列为可合入收益。

### 当前证据文件与后续状态

本节数字均可从以下原始数据复算：

- `results/memory-gap-20260831-0116/current-vs-lightpanda-prctl-thp-off.json`：当前正式构建结构基线；
- `results/memory-deep-20260831/moli-cgu1-ab-thp-off.json`：单 CGU vs 默认构建；
- `results/memory-deep-20260831/moli-thinlto-ab-thp-off.json`：ThinLTO + CGU=1 vs 默认；
- `results/memory-deep-20260831/moli-thinlto-cgu16-ab-thp-off.json`：ThinLTO + CGU=16 vs 默认；
- `results/memory-deep-20260831/moli-fatlto-cgu1-ab-thp-off.json`：Fat LTO + CGU=1 vs 无 LTO + CGU=1；
- `results/memory-deep-20260831/moli-cgu1-safe-icf-ab-thp-off.json`：safe ICF vs 无 ICF；
- `results/memory-deep-20260831/safeicf-thpoff-sequence-summary.json`：safe ICF 三组 30 target 稳态复验；
- `results/memory-deep-20260831/safeicf-dom-hotpath.json`：safe ICF 三进程、每进程 35 次 10k DOM 热路径；
- `results/memory-deep-20260831/cgu1-dom-hotpath.json`：单 CGU 三进程、每进程 35 次 10k DOM 热路径；
- `results/memory-deep-20260831/moli-cgu1-safe-icf-outliner-r0-ab-thp-off.json`：outliner r0 vs safe ICF；
- `results/memory-deep-20260831/moli-cgu1-safe-icf-outliner-r2-vs-r0-thp-off.json`：outliner r2 vs r0；
- `results/memory-deep-20260831/outliner-sequence-summary.json`：三组 30 target 的 outliner 性能与稳态内存；
- `results/memory-deep-20260831/cgu1-vs-lightpanda-thp-off.json`：优化后 Moli vs Lightpanda；
- `results/memory-deep-20260831/cgu1-vs-lightpanda-attribution.json`：逐页 executable attribution；
- `results/memory-deep-20260831/cgu1-code-supercategories.json`：逐页代码超级分类；
- `results/memory-deep-20260831/moli-window-installer-fastpath-ablation-thp-off.json`：installer fan-out 隔离实验；
- `results/memory-deep-20260831/cgu1-vs-lightpanda-api-surface.json`：API surface 动态对照；
- `results/memory-deep-20260831/cgu1-lightpanda-anonymous-vma-summary.json`：匿名 VMA 细分；
- `results/memory-deep-20260831/cgu1-lightpanda-file-mapping-summary.json`：完整 DSO 归因；
- `results/memory-deep-20260831/lightpanda-snapshot-ab.json`：Lightpanda 精确版本 snapshot A/B；
- `results/memory-deep-20260831/moli-cgu1-bootstrap-timing.json`：Moli application bootstrap 分段 timing；
- `results/memory-deep-20260831/app-snapshot-full-pairs/summary.json`：Moli 完整 application snapshot 10 轮同 ELF、THP-off 配对；
- `results/memory-deep-20260831/app-snapshot-full-bootstrap-timing.json`：Moli 完整 application snapshot 30 轮内部 bootstrap timing；
- `results/memory-deep-20260831/moli-cgu1-live-dcl.heap`：全采样 jemalloc live heap；
- `results/memory-deep-20260831/allocator-sequence-summary.json`：三组 30 target allocator 循环；
- `results/memory-deep-20260831/moli-current-vs-lightpanda-official-parser-count0-preprobe-thp-off-paired-10r.json`：公平空 parser 基线与互斥物理桶；
- `results/memory-deep-20260831/moli-direct-silent-vs-clone-parser-vmhwm-200k-paired-10r.json`：live fragment parser 对 clone parser 的峰值 A/B；
- `results/memory-deep-20260831/jemalloc-peak-direct-sampled/`：20 万节点峰值 sampled live allocations；
- `results/memory-deep-20260831/moli-batched-style-vs-direct-silent-parser-vmhwm-200k-paired-10r.json`：批量 style roots 对逐 root 实现的峰值 A/B；
- `results/memory-deep-20260831/moli-batched-style-vs-lightpanda-official-parser-vmhwm-slope-paired-10r.json`：修复后 0/20 万节点主斜率对照；
- `results/memory-deep-20260831/moli-final-clean-vs-lightpanda-official-parser-vmhwm-slope-preprobe-paired-5r.json`：最终 SHA 的零诊断扰动确认。

PGO 已按本轮范围决定明确排除，不再作为待完成项或候选收益；Moli application
snapshot A/B 也已完成并因收益不稳定、初始化变慢而排除。尚未完成、不得写成确定
收益的部分：CED/字体栈延迟加载 A/B，以及单 CGU 对真实 WebFetch 延迟/吞吐和
CI clean-build 时长的影响。parser DOM 动态内存斜率已经由上面的 10 轮主实验和
5 轮 exact-final 实验闭环，不再列为未知项。

## 结论

在“新进程、单实例、相同 CDP 命令、相同最小文档、DOMContentLoaded 后冻结进程”的可比基线中：

| 指标 | Moli | Lightpanda | Moli - Lightpanda |
|---|---:|---:|---:|
| PSS 中位数 | 74.520 MiB | 21.714 MiB | **+52.807 MiB** |
| PSS 范围 | 73.749–76.528 MiB | 21.708–22.298 MiB | 配对差 51.451–54.801 MiB |
| RSS 中位数 | 78.281 MiB | 23.844 MiB | **+54.438 MiB** |
| 线程数 | 19 | 19 | 0 |

Moli 的 PSS 是 Lightpanda 的 3.43 倍。这个结果不是由二进制文件大小、VSS、静态依赖树或主观代码阅读推断出来的，而是来自 5 轮交替顺序运行、冻结后的 `/proc` 实测。

最终差距的物理 Top 5 是：

| 排名 | 互斥物理类别 | Moli | Lightpanda | 差距 | 占 52.807 MiB |
|---:|---|---:|---:|---:|---:|
| 1 | 主二进制已驻留可执行页 | 41.871 MiB | 13.715 MiB | **+28.156 MiB** | 53.32% |
| 2 | 匿名映射与 brk heap | 20.648 MiB | 3.832 MiB | **+16.816 MiB** | 31.85% |
| 3 | 主二进制非执行 private-clean 页 | 8.652 MiB | 3.883 MiB | **+4.770 MiB** | 9.03% |
| 4 | 主二进制 private-dirty 页 | 2.742 MiB | 0.105 MiB | **+2.637 MiB** | 4.99% |
| 5 | 其他文件映射的 PSS | 0.496 MiB | 0.104 MiB | **+0.392 MiB** | 0.74% |

这五项的配对中位数之和是 52.771 MiB，覆盖总差距的约 **99.93%**。剩余约 0.04 MiB 来自显式 `[stack]` 映射和页级统计/中位数舍入；后文用每线程 stack pointer 识别出的 0.828 MiB 栈差绝大部分在 `/proc/smaps` 中没有 `[stack:<tid>]` 名字，已包含在 Top 2 的匿名映射里，不能再相加。因此，这份 Top 5 没有把同一块内存重复计算。

最重要的判断是：

1. 差距的第一来源是**实际执行过而驻留的代码页**，不是磁盘上的二进制尺寸。
2. 第二来源是**实际提交的匿名物理页**，但它不能简单等同于 JS heap。allocator A/B 已证明 jemalloc 的进程级覆盖是其中一个重要可控变量；公共后缀表、空任务源 channel 和已触达线程栈也已得到调用栈或页级归因。
3. 最终 52.8 MiB 中，有 **5.50 MiB PSS** 已通过“复用 Moli 默认 target”动态 A/B 证明是当前 CDP 流程中的重复 target/isolate 成本。
4. 主二进制 non-exec clean 差的主要成因是 Moli PIE 的 3.879 MiB `.rela.dyn`；保持 PIE/ASLR 并启用 packed RELR 的 A/B 实测可减少约 **3.8 MiB PSS**。
5. 主二进制 dirty 差主要是 loader 修补后受 RELRO 保护的 `.data.rel.ro`。它不是泄漏；改成 non-PIE 只会改变页的 clean/dirty 归类并牺牲可执行文件 ASLR，不是合理的单进程 PSS 优化。
6. 浏览器 WebSocket 建立时就产生了 39.31 MiB 的相对差距；这里包含首个 renderer/V8 runtime 的代码热集和匿名提交。它主要说明初始化时机，不代表 39.31 MiB 全部可以从最终活跃态删除。
7. 真实网页并不保证 Moli 永远更大。7 个双方成功站点中，6 个 Moli 更大；Wired 上 Lightpanda 反而比 Moli 高 29.04 MiB，原因是 Lightpanda 的内容相关匿名页更大。

## 五个物理桶对应的逻辑、用途和可优化性

下面的逻辑项位于前述五个互斥物理桶之内，但逻辑项彼此可能重叠。例如“默认 target 5.50 MiB”同时包含代码页、allocator、V8 和线程栈，不能再与这些子项相加。

| 物理桶 | 已动态确认的主要逻辑 | 为什么存在 | 当前可操作结论 |
|---|---|---|---|
| 可执行页差 28.156 MiB | 首次 CDP 连接触达 `moli_renderer_v8` 9.305 MiB、V8 5.484 MiB、Rust `core` 3.797 MiB、`moli_protocol` 2.547 MiB、`alloc` 2.168 MiB | Web API/V8 bridge、JS engine、CDP dispatch，以及被调用方单态化进调用模块的泛型代码 | 默认 target 延迟实例化已有 5.50 MiB 总 PSS A/B；单 CGU 又稳定降低 8.56 MiB；LTO、ICF 和 outliner 已动态排除，后续只看 lazy cold path 与无训练代码布局，并继续以 present executable pages 验收 |
| 匿名与 brk 差 16.816 MiB | jemalloc active/metadata/THP、PSL 哈希表、两个 isolate 的空任务源、V8 committed heap、已触达线程栈 | 通用分配、Cookie 安全校验、HTML task-source 调度、JS heap、线程执行上下文 | allocator 不能盲换：system allocator 的交替配对最小页中位省 2.56 MiB，但 10k DOM 导航慢约 38%；优先做 PSL 单表示和任务队列去双 channel；THP 只解释波动，交替复验未证明 `thp:never` 有稳定收益 |
| non-exec clean 差 4.770 MiB | `.rela.dyn`、按需触达的 `.rodata`/snapshot/静态表 | PIE 启动重定位元数据和只读运行时数据 | packed RELR 保留 PIE，实测约省 3.8 MiB；10.57 MiB ICU 原始 blob 在最小页只驻留约 11 KiB，不应按文件尺寸误优化 |
| private-dirty 差 2.637 MiB | Moli `.data.rel.ro` 驻留 2.454 MiB，加少量 `.data`/GOT | loader 修补指针、虚表和静态 dispatch 表后用 RELRO 锁成只读 | 这是稳定的每进程 COW 成本。减少它需要减少指针型静态表/feature/单态化；non-PIE 不降低这些页的单进程物理需求且损失 ASLR |
| 其他 file-backed 差 0.392 MiB | `libstdc++`、fontconfig/freetype、png/brotli、libc 等已触达页 | V8 C++ runtime、字体、图片、压缩和系统运行时 | 仅占 0.74%。静态链接只会把页移到主二进制并失去系统共享，不应作为当前优化方向 |

当前有三类数字，必须分开理解：

- **最终 PSS A/B**：packed RELR 约 3.8 MiB、复用默认 target 5.50 MiB、system allocator 首个最小页配对中位 2.56 MiB；这些是实际进程差，但 allocator 同时有重 DOM 性能反例。
- **live allocation 归因**：PSL 1.417 MiB、OwnerReady task sources 982 KiB、V8 isolate init 635 KiB；这些说明具体调用者，但不等于最终 PSS 收益。
- **结构性上限/方案**：合并 task source 队列、压缩静态表、代码布局；没有完成 Moli 全进程 A/B 前不把它们写成已实现收益。
- **被复验否定的候选**：分组 10+10 次一度显示 `thp:never` 少 1.77 MiB；交替顺序的 10 组配对只剩 0.234 MiB 中位差且正负波动很大，因此不把它算作收益。

## 为什么不能直接相信原 WebFetch 报告里的内存中位数

触发本次调查的全量 WebFetch 报告显示：

| target | 成功结果 PSS 中位数 |
|---|---:|
| lightpanda | 19 MiB |
| lightpanda-cdp | 15 MiB |
| moli | 32 MiB |
| moli-cdp | 44 MiB |

这些数字适合描述“该次并发 benchmark 中被采样到的成功任务”，不适合解释单个引擎的绝对 footprint，原因有三类。

第一，任务以 `parallelism=20` 并发执行。Linux PSS 会把同一 inode 的共享代码页按当前映射进程数分摊。并发越高，单进程 PSS 看起来越小，但机器上所有进程的总物理成本没有因此消失。

第二，成功集合不同。Moli、Lightpanda 对网页的成功率和失败种类不同，内存中位数对应的网页内容不是同一 cohort。

第三，即使限制到 50 个共同成功 attempt，PSS 也不是每个 attempt 都采到：

| target | 共同成功 attempt | 有 PSS 样本 |
|---|---:|---:|
| lightpanda | 50 | 29 |
| lightpanda-cdp | 50 | 27 |
| moli | 50 | 26 |
| moli-cdp | 50 | 26 |

因此本报告把原报告当作“现象入口”，把结论建立在独立、串行、固定文档的冻结进程实验上。

## 测量口径

### 主基线

每轮均启动全新的 release 进程，Moli 和 Lightpanda 的执行顺序按轮次交替：

1. 等待 CDP server ready；
2. 建立 browser WebSocket；
3. `Target.createTarget({url: "about:blank"})`；
4. `Target.attachToTarget(flatten=true)`；
5. 启用 `Page`、`Runtime`、`Network` 和 lifecycle events；
6. 导航到 `data:text/html,<title>x</title><p>x</p>`；
7. 等待 DOMContentLoaded，再稳定 50 ms；
8. 向进程发送 `SIGSTOP`，确保测量期间页面和 GC 不再变化；
9. 读取进程树的 `/proc/<pid>/smaps`、`smaps_rollup`、线程数和 fd 数；
10. 通过 `/proc/<pid>/pagemap` 统计主二进制 `r-x` 映射中 present 的 4 KiB 页；
11. `SIGCONT` 后关闭 target 和进程。

两边在该实验中都是单进程，因此没有遗漏 renderer 子进程。主结果使用 PSS；RSS 用于交叉验证。

### 为什么用 PSS，同时保留 RSS

- RSS 计算该进程映射的全部驻留页；共享页会被每个进程重复计数。
- PSS 把共享页按映射者数量均分，更适合汇总多个进程。
- 本次主基线是单引擎、单进程、没有同 inode 的并行副本，所以 PSS 与 RSS 的结论方向一致。
- VSS 只表示地址空间，不表示物理提交。Lightpanda/V8 可保留数十 GiB cage，但大部分页的 RSS/PSS 为 0，不能据此判断内存占用。

### 环境

| 项目 | 值 |
|---|---|
| OS | Debian，Linux `6.12.73+deb13-amd64` |
| 架构 | x86-64，4 KiB page |
| CPU | Intel Core Ultra 7 270K Plus，24 logical CPUs |
| 物理内存 | 55 GiB |
| Moli | `moli 1.0.6`，release build |
| Lightpanda | `1.0.0-nightly.6240+37391687` |

## Top 5 的计算方式与稳定性

五个类别按下列公式计算：

- 主二进制可执行驻留页：`present r-x pages × 4096`；
- 匿名与 brk：`Pss(anonymous) + Pss([heap])`；
- 主二进制非执行 clean：`main_binary.Private_Clean - executable resident bytes`；
- 主二进制 dirty：`main_binary.Private_Dirty`；
- 其他文件映射：除主二进制以外文件映射的 PSS。

每轮先计算 `Moli - Lightpanda`，再取 5 轮配对差的中位数。配对差范围如下：

| 类别 | 配对差中位数 | 5 轮范围 |
|---|---:|---:|
| 主二进制可执行驻留页 | 28.156 MiB | 27.777–28.406 MiB |
| 匿名与 brk | 16.816 MiB | 15.949–18.766 MiB |
| 主二进制非执行 clean | 4.770 MiB | 4.664–4.836 MiB |
| 主二进制 dirty | 2.637 MiB | 2.637–2.637 MiB |
| 其他文件映射 | 0.392 MiB | 0.386–0.433 MiB |

可执行页、非执行 clean、dirty 三项都非常稳定；主要波动来自匿名页。这也说明 52.8 MiB 不是一次 GC 时机或偶然 page fault 造成的离群点。

## 差距在哪个生命周期阶段产生

下面是另一套互斥拆分：它回答“最终差距在什么时候出现”，不能与前面的物理 Top 5 相加。

| 排名 | 阶段 | Moli 本阶段增量 | Lightpanda 本阶段增量 | 相对差增量 | 占最终差距 |
|---:|---|---:|---:|---:|---:|
| 1 | 建立 browser WebSocket | +40.536 MiB | +1.227 MiB | **+39.310 MiB** | 74.44% |
| 2 | server idle 基线 | 21.966 MiB | 14.764 MiB | **+7.016 MiB** | 13.29% |
| 3 | `Target.createTarget` | +5.790 MiB | +2.449 MiB | **+3.341 MiB** | 6.33% |
| 4 | 最小文档导航 | +5.634 MiB | +3.000 MiB | **+2.634 MiB** | 4.99% |
| 5 | 启用 Page/Runtime/Network | +1.020 MiB | +0.219 MiB | **+0.800 MiB** | 1.51% |
| 修正 | attach target | -0.238 MiB | +0.055 MiB | **-0.293 MiB** | -0.55% |

这些阶段增量合计为 52.807 MiB。最大的转折点非常明确：Moli 在 browser WebSocket 刚连接时就启动了共享 CDP owner、renderer/V8 runtime 和默认页面；Lightpanda 此时几乎没有创建页面执行环境。

这 39.31 MiB 大部分是“初始化提前发生”，不是全部都能从最终态消除。真正已通过 A/B 证明重复的部分是后文的 5.50 MiB 默认 target 成本。

## Top 1：已驻留可执行代码页，差 28.156 MiB

### 动态证据

最小页面完成后，主二进制 `r-x` 映射中实际 present 的页为：

| target | 驻留可执行页 |
|---|---:|
| Moli | 41.871 MiB |
| Lightpanda | 13.715 MiB |
| 差值 | **28.156 MiB** |

这不是用 `ls -l` 得出的二进制尺寸差，而是对运行中进程的 pagemap 逐页检查。磁盘文件大小只作为上下文：Moli 165.7 MiB，Lightpanda 123.3 MiB；文件大不代表页面会驻留，报告没有用这个静态值归因。

### 冷/热 page-cache 对照

另做 3 轮 binary page-cache cold/hot 对照：

| target | 模式 | 活跃 PSS | 驻留可执行页 |
|---|---|---:|---:|
| Moli | cold | 74.801 MiB | 41.320 MiB |
| Moli | hot | 74.784 MiB | 41.926 MiB |
| Lightpanda | cold | 21.643 MiB | 13.605 MiB |
| Lightpanda | hot | 21.799 MiB | 13.715 MiB |

冷热启动没有改变结论，因此不是上一次运行残留的 page cache 把 Moli 算大了。

### 哪些代码在 browser WebSocket 阶段被真正触页

对 Moli release 的符号区间与新驻留页做动态映射。建立 browser WebSocket 时，Moli 新驻留可执行页中最大的五个命名空间是：

| 排名 | 命名空间 | 新驻留页 |
|---:|---|---:|
| 1 | `moli_renderer_v8` | 9.305 MiB |
| 2 | `v8` | 5.484 MiB |
| 3 | Rust `core` 泛型实例 | 3.797 MiB |
| 4 | `moli_protocol` | 2.547 MiB |
| 5 | Rust `alloc` 泛型实例 | 2.168 MiB |

五项合计 23.30 MiB，占 Moli 在该阶段新增 33.04 MiB 可执行页的约 70.5%。这里的 `core`/`alloc` 不是独立运行时常驻库，而是被 Moli 各模块单态化并链接进主二进制的代码。

按具体符号区间看，最大的新增代码页包括 `hashbrown` reserve/rehash 约 0.652 MiB、`Vec::from_iter` 约 0.398 MiB、V8 C function mapping 约 0.344 MiB、`HashMap::insert` 约 0.195 MiB 和 Future poll 实例约 0.188 MiB。它们的作用分别是协议/DOM registry 扩容、集合构造、Web API 到 V8 的 native callback 映射，以及异步状态机执行；“删掉 Rust `core`/`alloc`”不是有效方案，真正要减少的是调用点的类型数量、冷 dispatcher 的首次触达和大泛型实例的重复单态化。

后续阶段也不是零代码成本：WebSocket → target 新增约 0.484 MiB executable pages，target → domains 新增约 1.055 MiB，domains → 最小文档新增约 2.027 MiB。后两段主要仍是 `moli_protocol`、renderer bridge、V8 和 `moli_core`，所以 domain handler 和首次文档路径都适合单独做 working-set 门禁。

Lightpanda release 已 strip，无法可靠地做同等级命名空间归属；报告只比较它的实际页数，不伪造符号级模块对照。

### 含义

这项优化不能靠“释放 heap”完成。方向应是：

- 把 CDP 冷命令、错误序列化和不常用 Web API 从首次连接热路径移走；
- 检查大泛型函数和协议 dispatcher 的单态化膨胀；
- 单 codegen unit 已得到稳定正向结果；ThinLTO、Fat LTO、链接器 ICF 和 machine outliner 已用内存及性能 A/B 排除，剩余只评估无需训练数据的冷代码布局；
- 每次改动继续用 pagemap 比较“实际触页”，不能只比较 ELF 尺寸。

## Top 2：匿名与 brk 物理页，差 16.816 MiB

### 不是 JS heap 单项造成的

最小文档时，Moli 的 `Runtime.getHeapUsage` 返回：

| 指标 | 值 |
|---|---:|
| used JS heap | 1.742 MiB |
| total JS heap | 2.000 MiB |
| embedder heap | 0.123 MiB |
| backing storage | 0 MiB |

Moli 与 Lightpanda 的匿名/brk 差是 16.816 MiB，明显不能用 1.742 MiB 的目标页面 JS heap 解释。Lightpanda 当前 build 不实现 `Runtime.getHeapUsage`、`Performance.getMetrics` 和 HeapProfiler 诊断命令，所以报告没有猜测它的 JS heap 数字。

### 匿名差距的阶段拆分

只观察匿名与 brk 页，最终差距主要按以下阶段形成：

| 阶段 | Moli 增量 | Lightpanda 增量 | 相对差增量 |
|---|---:|---:|---:|
| server idle 基线 | 7.086 MiB | 2.559 MiB | +4.523 MiB |
| browser WebSocket | +4.969 MiB | +0.734 MiB | +4.234 MiB |
| `Target.createTarget` | +5.500 MiB | +0.328 MiB | +5.172 MiB |
| attach target | -0.578 MiB | 0 MiB | -0.578 MiB |
| enable domains | +0.008 MiB | 0 MiB | +0.008 MiB |
| 最小文档导航 | +3.672 MiB | +0.211 MiB | +3.461 MiB |

也就是说，匿名差距不是只在解析 DOM 后产生；大头在 server/runtime 基线和两个页面执行环境的建立阶段已经出现。

### jemalloc 全采样 call stack

原 release 的 jemalloc 没有启用 profile/stats。为避免把 heaptrack 的不完整结果当事实，另构建了仅用于诊断的 profiling release，设置 `lg_prof_sample=0`，在同一进程的连续阶段做 heap dump 相减。绝对 PSS 仍以上面的原 release 为准。

| 阶段 | 新增 live malloc request bytes | renderer/V8 路径 |
|---|---:|---:|
| idle → browser WebSocket | 2.000 MiB | 1.569 MiB |
| browser WebSocket → target created | 1.557 MiB | 1.461 MiB |
| target → domains | 0.009 MiB | 0.008 MiB |
| domains → navigation | 0.021 MiB | 0.011 MiB |
| idle → navigation 合计 | **3.587 MiB** | **3.049 MiB** |

合计中其余已归类 live request 为 `moli_protocol/navigation` 0.205 MiB、async/server 0.191 MiB、fetch/storage 0.017 MiB、其他 0.125 MiB。

两个值得直接处理的子项是：

- 两个 isolate 的 `OwnerReadyTaskSource` 及其 wake channel 共 **982.219 KiB** live request，即每个 isolate 精确为 502,896 bytes；
- 两次 V8 `Isolate::Init` 调用栈共 **635 KiB** live request。

这两个数是 renderer/V8 分类的子集，不能再次与 3.049 MiB 相加。

### 空任务源为什么接近 1 MiB

每个 `RendererPageOwnedTaskSources` 会立即建立 28 个 scheduler source（`moli-renderer-v8/src/page_task_queue/owner_sources.rs:613-670`）；其中 27 个通过 `OwnerReadyTaskSource<T, S>` 实现，WebSocket 使用自己的有界/无界组合。当前基准同时保留默认 target 和显式 target，因此有两套 source。

`OwnerReadyTaskSource` 的实际结构不是一个轻量 flag：它包装的 `OwnerTaskSource<T>` 同时创建 `parser_boundary_wake` 和普通 `wake` 两个 Tokio unbounded channel（`moli-owner-queue/src/owner_task_source.rs:6-24`），再额外持有 readiness mutex/Arc 和 ready signal（`moli-owner-queue/src/owner_ready_task_source.rs:110-140`）。Tokio 1.52.1 的 MPSC 在创建空 channel 时就分配首个 32-slot `Block<T>`，所以 payload 越大，**队列里一个任务都没有也会占内存**。

全采样 heap 的原始 backtrace 地址可区分 27 个泛型实例。两个 isolate 中最大的五项是：

| task source | 空 source live request |
|---|---:|
| child module dependency fetch start | 162.156 KiB |
| modulepreload start | 98.156 KiB |
| ServiceWorker internal | 82.156 KiB |
| ServiceWorker client message | 66.156 KiB |
| dynamic import owner action | 58.156 KiB |

五项合计 466.781 KiB。child module dependency 的 queue payload 内嵌 `FrameDocumentModuleDependencyFetchTask`，而 scheduler dequeue 后本来就会把它装进 `Box`；它在进入 channel 前仍是大值，导致两条空 channel 各预留 32 个大 slot。

这些 source 本身有用途：HTML 事件循环需要区分 DOM、用户交互、networking、worker、IndexedDB、WebCrypto、module reaction 等 task source，scheduler 也需要保持各 source FIFO、可取消身份和 ready-time 仲裁。不能把 27 类任务不加标签地塞成一个 FIFO。

但是当前实现有两处可以明确优化：

1. `OwnerReadyTaskSource::route()` 只暴露普通 `sender()`；它没有暴露 `parser_boundary_sender()`。因此它内部第二条 parser-boundary channel 对这 27 个 source 是不可达的，虽然通用 `OwnerTaskSource` 在 parser lifecycle 的其他调用者中确实需要它。按本次全采样分配精确折半计算，给 OwnerReady 提供 single-wake variant 的结构性上限约为 **500,736 bytes（489 KiB）**；这还是 profile 推导，完成进程 A/B 前不写成 PSS 收益。
2. producer 在 send 前已经持有 readiness mutex，consumer dequeue/rearm 也持有同一把锁，随后再使用 Tokio MPSC 是双重同步。可以把 queue、ready、closed 放进一个共享 state，route 直接在锁内 push，consumer 仍保持唯一所有权；这样可去掉空 channel 首块的大部分 982 KiB。实现必须保留 sender 关闭错误、empty→nonempty 只唤醒一次、无 lost wake、FIFO 和批量连续性，并用现有 race tests 加并发模型测试验收。

如果先做风险更低的小改，可以只把上述几个大 payload 在进入 channel 前 `Box` 化；它会减少 32-slot 首块，但会增加真实任务到来时的一次小分配，收益应再做全进程 A/B。

V8 `Isolate::Init` 的 635 KiB 则用于 snapshot 初始化、heap/CppHeap 和 isolate 内部表。它不能像空 channel 一样直接删除；当前最有效的动作是避免默认 target 和显式 target 同时初始化两次 isolate。

### 公共后缀表：1.417 MiB live request

server idle 的全采样快照只有 2.079 MiB live malloc request，其中约 **1.417 MiB** 来自：

`StoragePartitionState::open → new_shared_browser_cookie_store → public_suffix_list → publicsuffix::List::append → hashbrown reserve`

它不是网页数据。CookieStore 需要 Public Suffix List 拒绝 `Domain=co.uk`、`Domain=github.io` 一类越权 cookie；删除校验会造成安全与兼容性回归。

当前 `moli-site/src/public_suffix.rs:9-35` 对同一份 `public_domains.txt` 有两种运行时表示：CookieStore 使用动态解析的 `publicsuffix::List`，site/eTLD+1 逻辑另有三个 `HashSet<String>`；`moli-cookie-jar/src/jar.rs:61-75` 又在每个 shared browser cookie store 初始化时请求前者。最小 data URL 只触发 `publicsuffix::List`，所以“合并两份”不会自动在该基线省掉全部 1.417 MiB；合理方向是让 cookie 与 site 逻辑共同查询一个静态压缩 trie/DAWG，或者让 CookieStore 到首次需要校验 Domain cookie 时才初始化 PSL。验收必须保留 ICANN/private suffix、wildcard、exception 和 IDNA cookie cases。

### jemalloc 自身到底占了什么

另构建启用 jemalloc `stats` 的诊断 binary，通过 mallctl 在同一进程读取 allocator 内部计数。该 binary 的总 PSS/BSS 不能与原 release 横比，但 allocator 各计数的阶段差可以回答匿名页去向：

| jemalloc 指标 | server idle | 活跃最小页 | 增量 |
|---|---:|---:|---:|
| allocated（调用者仍持有） | 2.492 MiB | 7.230 MiB | +4.738 MiB |
| active（run 已提交页） | 2.855 MiB | 8.230 MiB | +5.375 MiB |
| metadata | 2.422 MiB | 6.796 MiB | +4.374 MiB |
| resident | 5.285 MiB | 14.625 MiB | +9.340 MiB |
| mapped | 8.973 MiB | 20.266 MiB | +11.293 MiB |
| retained | 1.527 MiB | 2.734 MiB | +1.207 MiB |

因此匿名页差中的“大 extent”不能都叫作 JS heap：最小页 target 可见 used JS heap 只有 1.742 MiB，而 jemalloc active pages 与 metadata 在页面建立过程中都显著增长。jemalloc 的 metadata 不是应用对象，但服务 arena、size class、extent radix tree 和线程缓存；mapped/retained 是地址空间或待复用 extent，只有 resident/active 对当前物理页最相关。

### THP：解释了波动，但 `thp:never` 没有稳定收益

本机 Transparent Huge Page 策略是 `always`。实测到 jemalloc 的 8 MiB extent 中有一块 `AnonHugePages: 8192 kB`：即使只需要部分 4 KiB 页，也可能以完整 2 MiB huge page 计入物理内存。

最初按配置分组各跑 10 个新进程时得到：

| 配置 | DCL PSS 中位数 | 匿名+brk 中位数 |
|---|---:|---:|
| 当前 jemalloc 配置 | 82.507 MiB | 21.664 MiB |
| `MALLOC_CONF=thp:never` | 80.737 MiB | 20.027 MiB |
| 差值 | **-1.770 MiB** | **-1.637 MiB** |

但这两组不是交替运行，且分布明显双峰。为排除时间顺序和 huge-page 随机提交，随后补了 10 组配对：每组都跑 default/never，两种先后顺序按组交替，binary、data URL、CDP 命令和 DCL 采样点完全相同。

| 交替配对指标（default - `thp:never`） | 中位数 | 10 组范围 |
|---|---:|---:|
| DCL 总 PSS | **+0.234 MiB** | -5.438–+1.892 MiB |
| DCL 匿名+brk | **+0.168 MiB** | -5.707–+1.801 MiB |
| navigate ACK 总 PSS | +0.182 MiB | -4.860–+2.004 MiB |
| DCL 时间 | -0.185 ms | -10.114–+5.704 ms |

正值才表示 `thp:never` 更省。纠正实验的效应很小、正负范围跨越多个 MiB，无法复现原先 1.77 MiB 的稳定收益。可靠结论只有两点：本机 `THP=always` 确实会使 jemalloc extent 以 2 MiB 粒度跳变；当前证据**不支持修改 allocator 的 THP 配置**。若以后重试，应先通过 mallctl 确认 `opt.thp` 实际生效，并用更多交替配对、固定 CPU/host 状态和 workload 吞吐共同验收。

### 为什么不能直接换回 system allocator

用同一源码构建 `--no-default-features` 的 system-allocator binary。最初按 allocator 分组各跑 10 次曾得到 jemalloc DCL 82.507 MiB、system 73.769 MiB；结合 THP 复验可知，这个 8.738 MiB 差混入了 jemalloc huge-page 双峰，不能作为最终收益。

随后改成 10 组配对，每组各启动一个 jemalloc 和 system 进程，并交替两者先后顺序。各自样本中位数为：

| 阶段 | jemalloc PSS | system PSS | jemalloc 匿名+brk | system 匿名+brk |
|---|---:|---:|---:|---:|
| server idle | 20.589 MiB | 17.930 MiB | 4.838 MiB | 2.777 MiB |
| domains enabled | 66.926 MiB | 64.574 MiB | 13.303 MiB | 10.932 MiB |
| navigate ACK | 71.555 MiB | 69.201 MiB | 15.994 MiB | 13.482 MiB |
| DCL | 76.893 MiB | 74.463 MiB | 16.020 MiB | 13.527 MiB |

逐组计算 `jemalloc - system` 后，DCL 总 PSS 配对差中位数为 **2.558 MiB**，范围 0.801–8.096 MiB；匿名+brk 中位差为 **2.494 MiB**，范围 0.672–7.973 MiB。10/10 组方向都表明 system 更低，所以 allocator 效应是真实的；跨度同时证明不能把某次 jemalloc huge-page 状态下的 8.7 MiB 当固定收益。最小页 DCL 时间配对中位是 jemalloc 快 1.680 ms，范围 -26.041–+3.801 ms，也没有 system 稳定更快的证据。

压力形态改变后结论进一步不同：

| workload | jemalloc | system | 结论 |
|---|---:|---:|---|
| 100 次最小 target，最后 10 次 active PSS | 87.769 MiB | 83.625 MiB | system 仍低 4.144 MiB |
| 100 次最小 target，最后 10 次 close 后 PSS | 86.684 MiB | 83.056 MiB | 两边都形成平台，无 system 线性泄漏 |
| 100 次最小导航 DCL 时间中位数 | 45.450 ms | 43.003 ms | 该单次长序列里 system 略快；交替首屏未复现稳定方向 |
| 30 次 10k DOM，最后 5 次 active PSS | 121.570 MiB | 121.513 MiB | 活跃态优势消失 |
| 30 次 10k DOM，最后 5 次 close 后 PSS | 114.137 MiB | 107.221 MiB | system 回收后低 6.916 MiB |
| 30 次 10k DOM 导航 DCL 时间中位数 | 72.452 ms | 100.198 ms | system **慢约 38.3%** |

当前 jemalloc 通过未前缀 C symbols 覆盖整个进程，不只分配 Rust 对象，也覆盖 V8 和 native 库。结论是“allocator 是匿名差的重要可控变量”，不是“马上删 jemalloc”。下一轮应把三种方案放进完整 benchmark：当前 process-wide jemalloc、只作为 Rust global allocator 而不覆盖 C/V8、system allocator；同时看 CPU、p95、组总 PSS、100+ target 平台和真实重页。

### 线程栈不是 VSS 看起来那么大

冻结活跃态两边都是 19 个线程。逐线程用 syscall register 的 stack pointer 反查实际 VMA 后：

| target | 所有线程 stack VMA PSS |
|---|---:|
| Moli | 1.078 MiB |
| Lightpanda | 0.250 MiB |
| 差值 | **0.828 MiB** |

Moli 的两个 `render_runtime` 线程各触达约 0.309 MiB；protocol sequence 线程约 0.133 MiB；8 个 V8 DefaultWorker 合计约 0.096 MiB；4 个 Tokio worker 合计约 0.082 MiB。它们分别负责串行拥有 Page/V8 event loop、串行 CDP owner state、V8 background work 和异步 I/O。8/16 MiB 的 stack reservation 是 VSS，不是当前物理成本；单纯降低 reservation 不会自动省这 1.078 MiB。复用默认 target 会少一个 renderer thread，已包含在 5.50 MiB A/B 中。

这 0.828 MiB 差大部分是匿名 VMA 的逻辑再归因，已经包含在 Top 2 的 16.816 MiB 里，不是第六个可相加的物理桶。

### 剩余匿名页如何理解

profiling 解释的是“仍存活的 malloc 请求字节”，mallctl 又补出了 allocator active/metadata/resident，V8 API 给出目标页面 heap，线程 register 给出已触达 stack。它们口径不同且有父子关系，不能相加成一个伪精确拆分。代表性 domains 快照中，Moli 匿名 PSS 的 VMA 大小分布为：

| VMA 大小桶 | 匿名 PSS |
|---|---:|
| 4–16 MiB | 10.734 MiB |
| 1–4 MiB | 4.945 MiB |
| 256 KiB–1 MiB | 3.492 MiB |

这些页集中在少量中大型 `rw-p` extent。现在已经通过 mallctl 与 THP A/B 确认其中相当一部分是 jemalloc active/metadata/huge-page residency，但尚不能把每一个 4 KiB 页无歧义地分给 V8 direct mmap 或 jemalloc extent。若要做到逐页 owner，仍需给 V8 page allocator commit/decommit 和 jemalloc extent hooks 增加运行时标签；仅靠 smaps 的匿名路径名做不到。

## Top 3：主二进制非执行 clean 页，差 4.770 MiB

这部分主要是运行时实际触达的动态重定位元数据、只读数据、常量表和其他 non-executable file-backed pages：

| target | 非执行 private-clean |
|---|---:|
| Moli | 8.652 MiB |
| Lightpanda | 3.883 MiB |
| 差值 | **4.770 MiB** |

它与 Top 1 的 `r-x` 页互斥。对冻结进程把 present page 映射回 ELF section 后，主要 section 为：

| section | Moli 驻留 | Lightpanda 驻留 | 含义 |
|---|---:|---:|---|
| `.rela.dyn` | 3.879 MiB | 0.002 MiB | loader 启动时读取的动态 relocation entries |
| `.rodata` | 4.517 MiB | 2.955 MiB | snapshot、字符串、解析/编码/ICU/协议常量表 |

这是一轮独立 section probe；总量会随代码路径触页略变，因此 canonical 差仍使用前面的 5 轮 4.770 MiB，不能把该表各差值机械相加替代主结果。

### 为什么 Moli 有 3.879 MiB relocation 表

Moli 是 PIE，ELF 中 `.rela.dyn` 为 4,067,232 bytes / 169,468 entries，另有 271 个 PLT relocation；Lightpanda 当前 binary 是固定地址 `EXEC`，总 relocation 只有约 357 个。PIE 让可执行文件基址随机化，但大量 Rust/C++ 静态指针需要 loader 重定位，传统 RELA 每条记录占 24 bytes，于是启动时必须触达近 4 MiB 表。

使用 `-Wl,-z,pack-relative-relocs` 构建的诊断 binary 仍是 PIE：

- `.rela.dyn` 从 4,067,232 bytes 降到 4,704 bytes；
- 新 `.relr.dyn` 只有 44,256 bytes；
- binary 文件约缩小 4 MiB；
- 第一轮 5+5 活跃最小页中，主 binary file-backed PSS 中位数从 59.707 MiB 降到 55.961 MiB，即 **-3.746 MiB**；
- 纠偏实验又做了 10 组 baseline/RELR 配对并交替先后顺序：DCL 主 binary PSS 中位下降 **3.854 MiB**，10 组各下降 3.383–4.297 MiB，方向 10/10 一致；
- 同一交替实验的 server idle 总 PSS 配对中位下降 **3.860 MiB**（范围 3.581–4.083 MiB），主 binary PSS 下降 3.906 MiB；DCL 匿名页差中位仅 -0.033 MiB，说明收益确实来自 file-backed relocation pages；
- DCL 总 PSS 配对中位下降 3.650 MiB，范围 1.440–5.016 MiB；范围比主 binary 页宽是因为前述 jemalloc/THP 匿名双峰，不能反过来否定稳定的 section 收益。

这是当前风险/收益最清晰的优化：保留 PIE/ASLR，只压缩相对 relocation 的表示。合入前需要在最老支持的发布镜像/loader 上启动验证，并跑 release/CDP/WPT；验收还应检查 ELF 仍为 `DYN`、存在 `DT_RELR`，且 5 轮冻结主 binary PSS 至少下降 3.5 MiB。

### `.rodata` 不等于都常驻

Moli 磁盘上的 `.rodata` 约 17.10 MiB，其中最大的单符号是约 10.568 MiB 的 ICU raw data；但最小页只让该 blob 驻留约 11 KiB。Linux demand paging 已经替我们延迟了绝大多数 ICU 数据，删除或拆 ICU blob 对当前基线 PSS 几乎没有 10 MiB 级收益。

已触达的代表性数据包括约 409 KiB 的 V8 startup blob（基本全驻留）、ICU property trie/index、encoding detector 表、Brotli/crypto 小表和协议字符串。优化这些项应看逐页 residency：把冷 registry/错误字符串从首次 CDP 路径移开有意义，按 ELF section 总尺寸砍功能没有意义。

## Top 4：主二进制 private-dirty 页，差 2.637 MiB

| target | 主二进制 private-dirty |
|---|---:|
| Moli | 2.742 MiB |
| Lightpanda | 0.105 MiB |
| 差值 | **2.637 MiB** |

该差值在 5 轮中完全一致，并且从 idle 到 active 几乎不变化。因此它是主二进制映射中稳定的 COW/RELRO/可写数据成本，不是页面内容 heap，也不是泄漏。它值得在链接布局和全局可变数据层面单独审计，但优先级低于执行代码和匿名页。

进一步的 section probe 显示 Moli `.data.rel.ro` 共约 2.469 MiB，活跃态驻留约 **2.454 MiB**。这里放的是编译期不可变、但含地址的对象：vtable、函数指针、V8 builtin/intrinsic metadata、静态 dispatch/属性表等。PIE loader 先把其中地址修补成实际基址，所以物理页变成 private-dirty；随后 RELRO 把权限改成只读。也就是说，“dirty”描述页经历过 COW，不表示代码还在写它。

能命名的较大符号包括约 68.9 KiB LLVM 静态表、56.6 KiB V8 builtin metadata、52.9 KiB encoding detector unigram table、39 KiB V8 object table、22.2 KiB intrinsic function table，以及若干 style property map。符号有大量 anonymous/zero-sized 合并项，当前只能覆盖约 1.135 MiB，报告不把剩余部分伪装成完整 owner 拆分。

packed RELR 会减少 Top 3 的 relocation **记录**，不会让这些最终需要修补的指针页不再 COW。non-PIE 诊断 build 会把更多页表现为 file-clean，但代价是失去 executable ASLR；单进程仍需驻留这些指针/表内容。因此不建议以 non-PIE 作为 Top 4 的优化。合理方向是减少不使用 feature 带入的静态 registry、指针型大表和重复单态化，再用 section residency 验证。

## Top 5：其他文件映射，差 0.392 MiB

| target | 其他 file-backed PSS |
|---|---:|
| Moli | 0.496 MiB |
| Lightpanda | 0.104 MiB |
| 差值 | **0.392 MiB** |

这包括共享库的按份 PSS 和少量 file-backed dirty pages。它只占总差距 0.74%，不是当前优化重点。

代表性冻结快照中，Moli 已触达的外部库主要是 `libstdc++`（V8/C++ runtime），其次是小量 `libpng`、`libbz2`、`libm`、expat、fontconfig、freetype 和 Brotli；Lightpanda 主要只有 libc、loader、libm 和 NSS 的少量页。单次 `libstdc++` PSS 可在约 0.7 MiB 左右，但共享库 PSS 会受机器上其他映射者影响，所以 canonical 结论仍采用交替 5 轮的 0.392 MiB 差。

这些库分别提供 C++ ABI、图片、字体、压缩和系统解析能力。为了让类别“消失”而静态链接，会把代码移进主 binary 并失去跨进程共享，通常不降低组总物理内存。这里最多检查真正从未使用却在启动期触页的初始化函数，不值得先重构依赖体系。

## 已证明可消除的 5.50 MiB：默认 target 重复实例化

### 动态现象

刚建立 browser WebSocket、尚未发送 `Target.createTarget` 时：

- Moli 的 `Target.getTargets` 已返回一个 page target：`moli-default`；
- Lightpanda 返回 0 个 target。

而 benchmark 随后按 Chromium 常用流程显式调用 `Target.createTarget`。这使 Moli 同时保留默认 target 和新 target 两套页面 runtime。

### A/B 结果

用同一个 Moli release、相同 data URL、相同 domains、相同 DCL 等待和冻结方式，分别运行 5 个新进程：

| 路径 | PSS 中位数 | RSS 中位数 | 线程 | 目标页面 JS heap |
|---|---:|---:|---:|---:|
| 正常 `Target.createTarget` | 77.947 MiB | 81.633 MiB | 19 | used 1.739 / total 2.000 MiB |
| 复用 `moli-default` | 72.444 MiB | 76.145 MiB | 18 | used 1.739 / total 2.000 MiB |
| 差值 | **5.503 MiB** | **5.488 MiB** | **1** | 0 |

这不是静态推断：仅改变是否创建第二 target，就稳定少了约 5.5 MiB 和一个线程，目标页面可见 JS heap 完全相同。

### 当前代码链路

动态结果对应到当前实现的链路如下：

1. browser WebSocket 在 `moli-protocol-server/src/protocol_server/cdp.rs:302` 进入共享 frontend；
2. `moli-protocol-server/src/protocol_server/cdp_owner.rs:88` 首次创建 shared owner；
3. `moli-protocol-server/src/protocol_server/cdp_owner.rs:198` 创建 scheduler；
4. `moli-protocol-server/src/cdp_scheduler.rs:713` 构造 `CdpConnection`，并在 `:727` 安装默认 target；
5. `moli-protocol/src/conn.rs:1327` 在连接构造时创建 `NavigationEngine`；
6. `moli-protocol/src/conn.rs:3779` 安装默认 browser target，`:3788` 立即建立 initial empty document；
7. `moli-core/src/runtime/navigation_engine.rs:546` 在 `:561` 立即初始化 `JsRuntime`。

这段源码只用于解释已观察到的动态行为，不是结论的唯一证据。

### 建议设计

保留默认 target 的 CDP 元数据和 endpoint 兼容性，但把 initial empty document、`NavigationEngine` 和 `JsRuntime` 延迟到首次 attach、navigation 或需要执行页面命令时。这样既不需要让 benchmark 走私有路径，也能避免“客户端马上 createTarget”时常驻两套 isolate。

这里应以 **约 5.5 MiB 最终 PSS** 作为已验证收益目标。browser WebSocket 阶段的约 40.5 MiB 大部分会在第一个真正页面启动时重新出现，只是从连接阶段后移，不能把它都写成节省量。

## 页面内容放大后的差距

### 冻结前后同进程 workload

在同一个已加载最小页面中记录 before，执行 workload，稳定后冻结并记录 after；每项 5 轮：

| workload | Moli PSS 增量 | Lightpanda PSS 增量 | 原始差值 |
|---|---:|---:|---:|
| no-op | -0.273 MiB | +1.867 MiB | -2.140 MiB |
| 64 MiB ArrayBuffer | +69.018 MiB | +66.367 MiB | **+2.650 MiB** |
| 250k JS objects | +46.570 MiB | +42.161 MiB | **+4.409 MiB** |
| 10k full DOM nodes | +19.721 MiB | +18.231 MiB | **+1.489 MiB** |
| 5k attached CSS rules | +13.258 MiB | +8.742 MiB | **+4.516 MiB** |

Lightpanda 的 no-op 第一次 evaluate 自身会触发约 1.87 MiB warm-up，而 Moli 同期略有回落。因此上表保留未经 no-op 校正的原始差值；不把校正后的更大差距伪装成精确结果。

这些数据说明：

- 64 MiB backing store 两边都接近请求大小，Moli 不是按倍数复制 ArrayBuffer；
- 大量 JS objects、CSS rules 的 Moli 增量更高，值得在基线问题之后继续优化；
- 10k DOM 节点的额外差距只有约 1.5 MiB，不能解释最小页的 52.8 MiB 基线差。

### DOM/CSS 因子拆分

另一组 3 轮实验把创建内容与 JS wrapper/序列化动作拆开。最大的后续热点是：

| workload | Moli 增量 | Lightpanda 增量 | 差值 |
|---|---:|---:|---:|
| 5k CSSOM wrappers | 22.817 MiB | 9.739 MiB | **+13.078 MiB** |
| 10k nodes 的完整 innerHTML | 31.052 MiB | 19.676 MiB | **+11.376 MiB** |
| 5k attached CSS rules | 17.192 MiB | 8.421 MiB | **+8.771 MiB** |
| 清空 innerHTML | 11.926 MiB | 4.527 MiB | **+7.398 MiB** |

这部分属于内容相关增量，不应与最小页 Top 5 相加。它表明在基线收敛后，CSSOM wrapper 保留和 innerHTML materialization 是优先级最高的页面级热点。

## 真实站点交叉验证

每个站点每个引擎启动新进程，DOMContentLoaded 后冻结，运行 2 次。外部网络内容会变化，因此同时记录元素数，只把两边内容规模接近的结果视为强证据。

| 站点 | Moli PSS | Lightpanda PSS | 差值 | 元素数 Moli / LP |
|---|---:|---:|---:|---:|
| Slack | 157.415 MiB | 56.954 MiB | +100.460 MiB | 2210 / 2216 |
| White House | 161.131 MiB | 66.226 MiB | +94.905 MiB | 1014.5 / 1016.5 |
| Anthropic | 135.138 MiB | 56.827 MiB | +78.311 MiB | 1160 / 1160 |
| Canada traffic | 101.140 MiB | 31.063 MiB | +70.077 MiB | 123 / 123 |
| NASA | 131.599 MiB | 71.173 MiB | +60.426 MiB | 2518.5 / 2525 |
| Microsoft | 162.657 MiB | 146.003 MiB | +16.654 MiB | 628 / 804 |
| Wired | 190.625 MiB | 219.667 MiB | **-29.041 MiB** | 1933.5 / 1886 |

CNN 的 Lightpanda 两轮都在 `Page.navigate` 超时，无法形成配对，未纳入比较。

Wired 是重要的反例：Moli 主二进制 PSS 比 Lightpanda 高约 37.25 MiB，但 Lightpanda 匿名/brk 达 183.15 MiB，Moli 为 116.39 MiB，最终 Lightpanda 反而高 29.04 MiB。这证明内容相关匿名内存能盖过 Moli 的固定代码成本，也说明“Lightpanda 永远更省”或“Moli 永远更大”都不是可靠结论。

## 并发时为什么每进程 PSS 会变小

用 CLI 最小页面做 1、4、8 个相同 release 进程的受控并发，每档 3 轮：

| 并发 | target | 每进程 PSS 中位数 | 组总 PSS 中位数 | 每进程 RSS 中位数 |
|---:|---|---:|---:|---:|
| 1 | Moli | 56.593 MiB | 56.593 MiB | 60.207 MiB |
| 1 | Lightpanda | 21.641 MiB | 21.641 MiB | 23.785 MiB |
| 4 | Moli | 26.991 MiB | 109.257 MiB | 60.162 MiB |
| 4 | Lightpanda | 9.508 MiB | 38.029 MiB | 23.738 MiB |
| 8 | Moli | 21.532 MiB | 174.712 MiB | 60.127 MiB |
| 8 | Lightpanda | 7.484 MiB | 59.857 MiB | 23.752 MiB |

并发从 1 增到 8 时，RSS 基本不变，单进程 PSS 大幅下降，正是共享主二进制 clean pages 被均摊。机器真正承担的 8 进程组总 PSS 仍是 Moli 174.7 MiB、Lightpanda 59.9 MiB，Moli 约 2.92 倍。

因此 benchmark 应同时报告：

- 串行隔离进程的 PSS 与 RSS；
- 固定并发下的**组总 PSS**；
- 不再把并发运行中的“每 case PSS 中位数”描述成一个引擎的绝对 footprint。

## 重复 target 是否存在持续线性泄漏

对 ArrayBuffer、10k DOM、5k CSS 各连续创建/关闭 5 个 target：

- Moli 的大块 ArrayBuffer 在关闭后明显释放，但 allocator/code working set 留下平台；
- Lightpanda 的关闭后 PSS 经常不立即回落，后续 cycle 会复用已经提交的页；
- 两边在 5 轮内都表现为平台或复用，没有证据支持“每关闭一次 target 就持续线性增长”的泄漏结论。

这项结果只排除了短序列中的明显线性泄漏，不能替代数百次导航的长期斜率测试。

## 优化优先级

### P0：启用 packed RELR

这是最独立的一项：保留 PIE/ASLR，DCL 与 server PSS A/B 均约 **-3.8 MiB**，不改浏览器语义。建议单独一个 build commit：

- release linker 增加 packed relative relocations；
- CI 检查产物仍为 PIE 且具有 `DT_RELR`；
- 在最老支持的 runtime/container 做启动 smoke；
- 跑完整 release、CDP/WPT 和至少 5 轮 frozen PSS；
- 若部署环境 loader 不支持，则不能静默回退到无法启动的产物，应明确限定平台或暂缓。

### P0：默认 target 只保留元数据，延迟页面 runtime

这是当前唯一已经有最终 PSS A/B 收益的架构改动：约 **5.5 MiB PSS、1 线程**。验收应至少包含：

- browser WebSocket 建立后，不因默认 target 元数据而初始化完整 `JsRuntime`；
- `Target.getTargets`、target WebSocket endpoint、auto-attach 行为保持兼容；
- 显式 `Target.createTarget` 的最小页 PSS 在 5 轮配对测试中下降至少 4 MiB；
- CDP/WPT/benchmark 不允许改成 Moli 专用复用路径来掩盖问题。

### P1：收敛 OwnerReady task-source queue

这项已有 **982.219 KiB live request** 的逐 source 调用栈，不再是泛泛的“channel 可能多”。建议分两步，便于验证并发语义：

1. 让 `OwnerReadyTaskSource` 不再构造不可达的 parser-boundary channel；profile 推导的分配上限约 489 KiB，但以全进程 A/B 为准。
2. 再评估 readiness state 与 incoming queue 合并，或先 box 最大 payload；第二步必须保留 close、FIFO、batch contiguous 和 lost-wake race tests。

验收同时看空 page source live allocation、最终 PSS、任务压力吞吐和相关 scheduler/WPT，不以减少类型数量为目标破坏 HTML task-source 仲裁。

### P1：公共后缀表使用一个紧凑查询表示

目标不是关闭 Cookie PSL 校验，而是替换 server idle 时 1.417 MiB 的动态 hash/list 构建，并避免同一文本以后再生成三组 HashSet。优先评估生成期构建的静态 trie/DAWG；若先做 lazy，必须证明首次 Domain cookie 的延迟与并发初始化可接受。用 PSL 全规则测试、Cookie domain rejection、same-site/site-key 和 IDNA cases 验收。

### 暂不推进：`thp:never`

初始分组实验显示 -1.77 MiB，但 10 组交替配对只得到 -0.234 MiB 中位收益，且范围跨越 -1.892 到 +5.438 MiB（以 `thp:never - default` 表示）。它目前是解释匿名 PSS 双峰的线索，不是可合入优化。

### 已关闭：不切换 allocator

system allocator 在交替首个最小页配对中位省 2.56 MiB、10/10 组方向一致，却让
10k DOM 导航慢 38.3%，并在重 workload 活跃态失去内存优势。结合本轮明确的产品
范围，allocator 方案到此关闭：保留当前 jemalloc，不再测试 system、Rust-only
jemalloc 或其他 allocator，也不把冷启动的 2.56 MiB 外推成可获得收益。

### P1：缩小首次 CDP 连接的代码 working set

优先看 `moli_renderer_v8`、V8、`moli_protocol` 及泛型 `core`/`alloc` 热路径。目标指标不是 ELF 文件缩小，而是 browser WebSocket 和最小导航后的 present executable pages。

建议一次只改变一个 release/link/layout 参数，并保留 CDP 阶段 executable-page
attribution。ThinLTO、Fat LTO、ICF 和 machine outliner 已被动态排除；后续参数的
磁盘缩小仍不是验收条件，browser WebSocket 或活跃最小页的 present code pages 必须
实际下降，且不能明显损伤启动与网页吞吐。PGO 不在本轮范围内。

### P2：补齐 V8 与 jemalloc 的逐 extent 标签

mallctl 已把 allocator 总体边界缩小，但 V8 direct page commit 与 jemalloc extent 在匿名 smaps 中仍可能相邻/合并。只有在 allocator 方案仍无法解释真实站点差距时，再给 commit/decommit/extent hook 加 owner tag；它是诊断能力建设，不直接节省内存。

### P2：CSSOM 与 innerHTML

基线问题之后，优先检查 CSS rule wrapper 生命周期、样式对象复制，以及 innerHTML materialization 中的中间字符串/DOM snapshot。动态差值分别达到 13.08 MiB 和 11.38 MiB。

## 不能从本报告推出的结论

- 不能说 Moli 的 52.8 MiB 都是“V8 开销”；Lightpanda 也使用 V8，且当前符号/诊断能力不对称。
- 不能说 browser WebSocket 的 39.31 MiB 都可删除；其中大部分是首个页面迟早需要的代码热集。
- 不能把 3.587 MiB jemalloc live request 当成全部匿名页；它不覆盖 direct mmap、allocator metadata、extent slack 和所有 committed V8 pages。
- 不能把 system allocator 的首个最小页配对中位 -2.56 MiB 外推为真实网页的免费收益；样本范围是 -0.80 到 -8.10 MiB，且 10k DOM 已测到约 38.3% DCL 时间回归。
- 不能把 `.data.rel.ro` 的 dirty 页叫作仍在修改的全局状态；RELRO 已将其锁成只读，它反映的是 PIE relocation 后的 COW。
- 不能因 non-PIE 看起来有更多 clean 页就关闭 ASLR；packed RELR 才是保持安全属性的 relocation 元数据优化。
- 不能把 10.568 MiB ICU blob 的文件尺寸当作 10.568 MiB PSS；最小页只触达约 11 KiB。
- 不能用二进制磁盘尺寸、VSS 或静态依赖数量代替 PSS/RSS。
- 不能把外部真实站点的两轮数据当成稳定排名；它用于验证方向和发现反例。
- 不能把各层指标相加：默认 target A/B、OwnerReady channel、PSL、allocator、线程栈、匿名页、renderer/V8 分类存在父子/重叠关系。

## 原始证据索引

主原始数据位于 `moli-benchmark/results/memory-breakdown-20260830-183530/`：

| 文件 | 内容 |
|---|---|
| `cdp-connection-phases.json` | 5 轮主基线、各 CDP 阶段、smaps 和 executable pages |
| `code-pages-by-cdp-phase.json` | 3 轮各阶段新增可执行页与 Moli 符号命名空间归属 |
| `cold-hot-code-pages.json` | binary page-cache 冷热对照 |
| `default-target-reuse.json` | `createTarget` 与复用 `moli-default` 的 5×5 A/B |
| `allocation-profile-phases.json` | jemalloc 全采样阶段差与调用栈分类摘要 |
| `moli-profile-active.smaps` | profiling build 的原始 active smaps |
| `frozen-key-workloads.json` | no-op、ArrayBuffer、JS objects、DOM、CSS 的 5 轮冻结增量 |
| `dom-css-breakdown.json` | DOM/CSS/innerHTML 因子拆分 |
| `real-site-frozen-smaps.json` | 8 个真实站点的冻结 smaps 与内容计数 |
| `concurrency-pss-scaling.json` | 1/4/8 并发的单进程与组总内存 |
| `repeated-target-retention.json` | 5 cycle target 创建/关闭保留曲线 |
| `cli-active-baseline.json` | CLI 单实例 7 轮对照 |
| `logical-owner-followup/moli-section-probe.json` | Moli ELF section 到实际驻留页的映射 |
| `logical-owner-followup/{moli,lightpanda}-logical-owner-probe.json` | 逻辑 owner、共享库和匿名 mapping 补充快照 |
| `logical-owner-followup/{moli,lightpanda}-thread-stack-probe.json` | 每线程 syscall SP、线程名和实际 stack VMA PSS |
| `logical-owner-followup/moli-link-ablation-{baseline,relr,nopie}.json` | PIE、packed RELR、non-PIE 的 page-runtime A/B |
| `logical-owner-followup/moli-relr-interleaved-*-{baseline,relr}.json` | 10 组交替顺序 packed RELR 纠正实验 |
| `logical-owner-followup/moli-thp-{default,never}.json` | 最初按配置分组的 10+10 THP 结果（保留用于说明顺序混杂） |
| `logical-owner-followup/moli-thp-interleaved-*-{default,never}.json` | 10 组交替顺序 THP 纠正实验 |
| `logical-owner-followup/moli-allocator-interleaved-*-{jemalloc,system}.json` | 10 组交替顺序 allocator 首个最小页实验 |
| `logical-owner-followup/moli-sequence100-{jemalloc,system}.json` | 同进程 100 次最小 target 的 active/close 平台 |
| `logical-owner-followup/moli-sequence-dom-{jemalloc,system}.json` | 同进程 30 次 10k DOM 的 PSS 与导航耗时 |

原全量 WebFetch 数据位于 `moli-benchmark/results/webfetch-mix-benchmark-20260830-172356/`，其中 `summary.json` 保存共同成功 cohort 和 PSS 样本覆盖率，`run-summary.md` 保存并发配置和汇总。

`moli-benchmark/results/` 默认被 gitignore；本报告保留了关键数值、二进制 SHA 和口径，但若需要在 CI 或另一台机器上独立复核，应先归档这些 JSON，或把冻结阶段探针产品化为仓库脚本。
