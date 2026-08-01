# Akeep 产品说明

状态：核心备份与恢复流程已经实现，可直接用于本地或 S3-compatible 存储

产品关系：Akeep 是完全独立的产品、代码库和品牌

## 结论

Akeep 不是“又一个 Agent 对话查看器”，也不把
“压缩 JSONL”当成独立品类。

它的定位是：

> **Akeep — coding-agent 工作记录的本地版本历史。**

英文主描述：

> **Privacy-first, Git-like backup, recovery, migration, and sharing for AI agent session history.**

产品心智不是“给聊天文件套 Git”，而是把用户已经在产生的 Agent 状态自动
保存成 commit：

```text
init -> status -> commit -> work normally -> commit
                           -> log / diff / fsck / checkout / clone
```

最重要的产品体验是“对 Agent 工作流零侵入，但历史真的能取回”：

- 不要求修改 Claude Code、Codex 或其他 Agent；
- 不要求先 `add`，Provider adapter 自动发现窄而明确的 durable-state allowlist；
- 日常只需 `akeep commit`，需要时再 `log`、`diff` 或 `checkout`；
- manifest parent 和 `HEAD~N` 提供真正的线性版本语义；
- 哈希、压缩、去重、一致性 SQLite snapshot 是实现可靠性的机制，不是让用户
  先理解的产品口号。

上传成功不等于可恢复。长期价值仍然是“不丢、看得懂版本、能取回”；压缩和
去重让它在 50+ GB 的真实数据上可负担。

## 名字：保留 Akeep，不改成 akgit

不建议改名为 `akgit`：

- `Akeep` 直接表达“替用户保存 Agent work”，可以覆盖本地、S3、未来托管同步；
- `akgit` 会暗示 Git object/protocol 兼容、working tree、staging、branch、merge、
  remote 和冲突语义；Akeep 有意不实现这些；
- 用户真正需要的是熟悉的少数动作，不是第二套 Git；
- `akeep commit/log/diff/checkout/clone` 已经借用了足够的认知，无需把整个品牌
  绑在实现类比上。

只有未来真的需要 `branch/merge/push/pull`，并且格式可以由普通 Git 工具读取
时，才应重新讨论 `akgit`。当前产品名和二进制都保持 `akeep`。

本轮公开 Web/GitHub 检索没有发现一个在 coding-agent history 类别中占据
`Akeep` 或 `akgit` 的主导产品；这不是商标许可结论。进行品牌扩展或商业推广
时仍应单独检查 GitHub、crates.io、Homebrew、主要域名和目标销售地区商标。
即使 `akgit` 可用，上述产品预期问题仍然使 `Akeep` 更合适。

## 我们自己能不能用

能，而且应该先服务我们自己。

我们的真实数据超过 50 GB，原来已有一个每周运行、覆盖五个 Agent 的 S3
备份服务。这给 Akeep 提供了真实压力测试和清楚的最低功能线：

- 不能少于现有五个 Provider 的原始备份覆盖；
- 不能丢失现有的 SQLite 一致性快照、定时运行、增量上传和不传播删除；
- 必须新增可选客户端加密、压缩/去重、内容哈希和恢复验证；
- 必须能与旧服务并行，不修改旧备份；
- 必须通过两次真实恢复演练后，才允许替换旧服务。

因此，Akeep 的第一位用户不是一个虚构 persona，而是我们正在使用的机器。
任何在 50+ GB 数据上成本失控、内存爆炸、恢复过慢或误报成功的设计，都应
在真实使用中尽早暴露。

首个真实 commit 的数字已经说明压缩是有效机制，但不是唯一卖点：
55,206,535,333 logical bytes（51.42 GiB）变为 10,690,998,971 stored bytes
（9.96 GiB），即 5.16:1、减少 80.6%。首次 commit 内的重复 chunk 只有
188,747,292 bytes（0.34%），因此首轮节省主要来自 zstd；跨 commit 去重的价值
体现在后续未变化内容不再上传，而不是把首轮压缩率全部归功于 dedup。

## 能否不再使用之前的备份项目

可以。迁移备份系统时，先并行运行一段时间再停旧服务：

正确顺序是：

1. Akeep 使用独立目录和独立 S3 prefix 进行 shadow backup；
2. 连续运行至少 14 天并完成至少三次自动备份；
3. checkout 最新和至少一周前的两个 commit；
4. 对恢复结果做逐文件哈希比对；
5. 在隔离的 Provider home 中确认 Claude Code 和 Codex 能识别恢复记录；
6. 人为破坏一份复制出来的 archive，确认 `fsck` 必须失败；
7. 通过后再停旧 timer，同时保留旧远端数据作为回退。

通过这组 gate 后，Akeep 可以替代现有“Agent history 到 S3”的专用服务。
它不会替代 Git、系统备份、Provider 原生 resume 或工作区 artifact 备份。

## 核心功能

### 版本历史 + 现有备份能力等价

当前支持：

- Claude Code、Codex、Grok、Kimi Code、OpenCode 原始数据发现；
- live SQLite 一致性快照；
- 内容寻址的增量 archive；
- commit message、parent、`HEAD~N` 与跨 commit 压缩去重；
- 可选的上传前客户端加密，`none` 是完整支持的模式；
- 本地目录与 S3-compatible 两个 target；
- `init`、`status`、`commit`、`log`、`diff`、`fsck`、`checkout`、`clone`；
- Linux systemd 定时器；
- 默认不改 Provider 文件、不传播删除、不覆盖恢复目标；
- 人类可读和 JSON 两种报告。

`add` 不属于核心流程。自动发现比强迫用户维护 staging set 更符合“对现有 Agent
体验最小改动”的目标。将来若用户需要备份 adapter allowlist 之外的自定义目录，
可以增加显式 source 配置或可选 `add`，但不能让它成为普通 commit 的前置步骤。

暂不做：

- GUI 和 Web dashboard；
- 托管账号、付费和团队权限；
- 二十个 Provider；
- embedding、token/cost 图表；
- 自动清理本机冷 session；
- 跨 Provider 原生 session 注入。

### 语义交接能力

以下能力已经实现，但仍是 versioned backup 之上的派生层：

- Claude ↔ Codex semantic handoff bundle；
- 当前目标、关键决策、失败方案、文件改动、命令结果、测试状态、Git 状态、
  artifact 和 TODO 的结构化提取；
- handoff 完整性报告。

跨 Agent 应宣传为“continue the task”，不要宣传成“无损转换原生 session”。
不同 Provider 的工具调用和索引格式并不等价，强行写入内部格式会形成脆弱且
危险的兼容负担。

## 多 Agent 和多 Backend 怎么接

要区分两个维度：

- **Provider adapter** 负责发现和一致性快照：Claude、Codex、Grok、Kimi、
  OpenCode。
- **Storage target** 负责保存 opaque objects：本地 filesystem、S3-compatible。

当前不做庞大的插件系统，只保留清晰的内部边界。S3-compatible 已经覆盖
AWS S3、R2、MinIO、Backblaze B2 等大量服务；本地 filesystem 又能覆盖外接
硬盘、NAS mount 和由 Syncthing/rclone 管理的目录。等真实用户要求 WebDAV
或其他 target 时再新增。

Provider adapter 不应决定 archive 格式，storage target 也不应看懂 transcript。
这样新增 Agent 不会修改 archive/恢复核心；开启客户端加密时，新增 backend
也不会接触明文。

### “sync” 到底能承诺什么

Akeep 当前可以承诺：

- commit 直接写入一个用户选择的本地或 S3-compatible repository；
- `clone` 能把 filesystem 或 S3 repository 精确复制成可独立使用的本地 bundle；
- 用户可把 filesystem target 放在 NAS、Syncthing 或 rclone 管理的目录。

它还不能宣传成实时多设备双向 sync。当前没有 device identity、并发 writer
协调、分支/冲突、增量 pull 或 key pairing。首页应使用 “local or your own
storage” 和 “clone” 等准确表达；“managed encrypted sync” 留给有协议和冲突
语义的后续产品。

## 隐私承诺

“Privacy first” 不能只是一句宣传语，也不等于强制用户承担丢失密钥的风险。
它应变成可测试的产品约束：

- 当前 CLI 默认离线，未配置远端 target 时没有网络请求；
- 未来托管服务可以作为可选 target，但不能取代本地和自有存储路径；
- `encryption = none` 是正式支持和测试的模式；
- 加密模式在创建 vault 时确定，不能每次 commit 临时切换；
- 本地 target 可以默认不开加密；
- 远端 target 明确推荐加密，但说明风险后允许用户不开；
- `status` 永远显示当前加密模式和 storage operator 是否能读取内容；
- 开启加密时，远端只看到客户端加密后的对象和最少元数据；
- 开启加密时，原始文件和敏感路径不出现在远端明文对象名里；
- auth、credential、cache、临时目录默认排除；
- 本地 staging 目录权限为 0700；
- archive format、threat model 和 key recovery 行为公开；
- 开启加密时必须提供 recovery key；全部密钥丢失的后果必须直说；
- 公开 corruption、crash-recovery 和 restore 测试。

注意：Agent session 经常已经包含意外打印出来的 secret。Akeep 不能保证源数据
没有 secret。未开启客户端加密时，Akeep 必须明确提示远端存储方能够读取这些
内容，不能用 server-side encryption 冒充零知识加密。

因此“不强制加密”是更好的默认：可信、已全盘加密的本地磁盘通常不需要 Akeep
再加一层密钥风险；远端则强烈推荐 age，但用户确认披露后仍可选择 plaintext。
加密不是一个可随 commit 切换的 flag，而是 `init --encryption age` 确定的
repository 属性。私钥丢失没有后门；`clone` 也故意不复制它，用户必须把 identity
单独放进密码管理器、加密移动介质或另一份离线备份。

## 市场与差异化

截至 2026-07 的复核结论是：方向有真实需求，但“Agent 历史版本化”本身也已经
出现直接竞品，不能再把 Git-like 命令当成唯一创新：

- [ctx](https://www.ctx.rs/) 已经把跨 coding-agent 的本地 SQLite 索引与搜索
  做成开源 CLI；
- [coding-agent-search / cass](https://github.com/Dicklesworthstone/coding_agent_session_search)
  已经覆盖大量 Provider 的本地 TUI/CLI 检索；
- [Agent Sessions](https://github.com/jazzyalex/agent-sessions) 已经提供多 Agent
  的本地 macOS 浏览、搜索和 resume；
- [Claude Code History Viewer](https://github.com/jhlee0409/claude-code-history-viewer)
  已经能离线浏览多个 coding assistant；
- [Contextify](https://contextify.sh/) 已经覆盖本地历史、搜索和云同步；
- [Claude Sync](https://claude-sync.com/) 已经强调 Claude Code 的端到端加密同步；
- [stift](https://stift.sh/) 已经提供多 Agent 的 hosted/self-hosted background
  push/pull；
- [Entire](https://github.com/entireio/cli) 已经用 Git hook 捕获 Agent session，
  把 checkpoint/session metadata 放到独立 Git branch，并提供 resume/rewind；
- [Spool](https://spool.pro/) 已经主打分享和继续 Agent session。

因此 Akeep 不应主打：

- “我们也能看 JSONL”；
- “我们支持的 Agent 数量最多”；
- “我们能画 token 图”；
- “我们把一点文本压小了”。

Akeep 的楔子应该是：

> **Your agent work deserves version history.**

但真正的产品差异不能只写成 `commit/log/diff`。与最接近的 Entire 相比，Akeep
不依赖每个代码仓库的 Git hook 或用户代码 commit，而是跨项目、跨 Provider
备份完整的 provider-native durable state；与 stift/Claude Sync 相比，Akeep
不要求账号或 server，支持本地/S3、自选 plaintext/age，并包含 live SQLite
一致性 snapshot 和完整 scratch checkout。这个差异必须由兼容矩阵、50+ GB
真实大规模备份、故障注入和恢复演练证明，不能只靠命令名字。

需求也不是抽象假设：Claude Code
[官方 sessions 文档](https://code.claude.com/docs/en/sessions)说明本地 transcript
默认 30 天清理，并允许通过 `cleanupPeriodDays` 调整。Akeep 不应攻击 Provider
的产品选择，而应清楚告诉用户：可 resume 的本地工作状态仍有 retention 和格式
生命周期；如果它值得保留，就应拥有独立、能被 checkout 和 fsck 的 commit。

组合差异是：

> 自动发现的原始状态 + Git-like commit/diff/checkout + 用户可选客户端加密
> + 压缩去重 + Provider-aware 一致性快照 + semantic handoff。

## 宣传方式

### 首页

主标题：

> **Your agent work deserves version history.**

副标题：

> **Commit, diff, check out, and clone Claude Code, Codex, and other coding-agent
> history—locally or in your own storage.**

第三句应直接给证据：

> **Akeep auto-discovers provider-native state, compresses and deduplicates each
> commit, and offers optional client-side encryption when you want it.**

不要把 “AI history compression tool” 当主定位。用户不会为了压缩 JSONL 安装
一个高权限工具，但会为了得到不依赖单个 Provider 的版本历史安装它。

### 核心演示

用隔离 fixture 做 90 秒、可复现的恢复演示：

1. `akeep status` 自动发现五个 Provider；
2. `akeep commit -m "before migration"`；
3. 修改 fixture，再次 `commit`；
4. `akeep log` 和 `akeep diff HEAD~1 HEAD`；
5. `akeep checkout HEAD`，显示逐文件 hash 完全一致；
6. `akeep clone`，用 clone config 运行 `fsck HEAD`；
7. 破坏一个 archive object，`akeep fsck HEAD` 明确拒绝。

这比先做漂亮 dashboard 更能建立备份产品最需要的信任。

### 发布标题

可使用的发布标题：

> **Show HN: Akeep – Git-like local version history for coding agents**

其他可测试标题：

- “Your agent work deserves version history.”
- “Akeep: Git-like commits for Claude Code, Codex, and OpenCode history.”
- “Commit and diff your coding-agent history without changing your agent.”
- “We corrupted our own Agent archive. Akeep caught it before checkout.”

### 推广渠道

1. GitHub README + 可复现 demo；
2. Show HN；
3. Claude Code、Codex、OpenCode 相关社区；
4. 一篇工程文章：如何在 Agent 正在运行时一致地备份 JSONL 和 SQLite；
5. 一篇安全文章：Agent session 中到底会泄漏什么，客户端加密解决什么；
6. 恢复演练和兼容矩阵持续公开更新。

### 信任材料

Akeep 公开维护以下材料：

- archive format 文档；
- threat model；
- provider compatibility matrix；
- key recovery 说明；
- corruption 和 crash-recovery tests；
- 从零开始的离线 restore runbook；
- 清楚的 telemetry/network 行为表。

## 商业化顺序

先把本地 CLI 和自带存储做成可信的开源产品，再考虑托管同步。

未来可以收费的是：

- 多设备 E2EE 同步；
- device pairing 和 recovery-key 管理；
- encrypted version history；
- 可靠的跨设备恢复；
- 团队共享和保留策略。

不要按“存了多少 JSON”作为核心价值收费。真正可付费的是恢复可靠性、密钥管理
和免维护的连续性服务。

## 可靠性的唯一北极星

必须能回答：

> 如果这台机器今天损坏，我们是否能只依赖 Akeep，在一台干净机器上恢复出
> Provider 可识别、哈希一致的历史？

答案不是经过演练的“是”，就不能算可靠备份。
