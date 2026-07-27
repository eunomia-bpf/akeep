# Akeep 产品与发布计划

状态：MVP 决策稿

产品关系：Akeep 是完全独立的产品、代码库和品牌

## 结论

Akeep 值得做，但不能把自己定义成“又一个 Agent 对话查看器”。

最合适的定位是：

> **Akeep — 面向 coding-agent 工作记录的隐私优先、可验证备份与恢复工具。**

英文主描述：

> **Private, verified backup and recovery for coding-agent work.**

第一版最重要的功能不是搜索、图表或支持多少 Agent，而是：

> **从真实备份中完成一次经过哈希验证、Provider 能识别的恢复。**

备份上传成功不等于可恢复。Akeep 应该卖的长期价值是“不丢、可验证、
能取回”，压缩和去重只是让这件事更便宜、更快。

## 我们自己能不能用

能，而且应该先服务我们自己。

初始 dogfood 数据超过 50 GB，已经有一个每周运行、覆盖五个 Agent 的 S3
备份服务。这给 Akeep 提供了真实压力测试和清楚的最低功能线：

- 不能少于现有五个 Provider 的原始备份覆盖；
- 不能丢失现有的 SQLite 一致性快照、定时运行、增量上传和不传播删除；
- 必须新增可选客户端加密、压缩/去重、内容哈希和恢复验证；
- 必须能与旧服务并行，不修改旧备份；
- 必须通过两次真实恢复演练后，才允许替换旧服务。

因此，Akeep 的第一位用户不是一个虚构 persona，而是我们正在使用的机器。
任何在 50+ GB 数据上成本失控、内存爆炸、恢复过慢或误报成功的设计，都应
在公开发布前暴露。

## 能否不再使用之前的备份项目

可以，但不是现在立刻停。

正确顺序是：

1. Akeep 使用独立目录和独立 S3 prefix 进行 shadow backup；
2. 连续运行至少 14 天并完成至少三次自动备份；
3. 恢复最新和至少一周前的两个 recovery point；
4. 对恢复结果做逐文件哈希比对；
5. 在隔离的 Provider home 中确认 Claude Code 和 Codex 能识别恢复记录；
6. 人为破坏一份复制出来的 archive，确认 `verify` 必须失败；
7. 通过后再停旧 timer，同时保留旧远端数据作为回退。

通过这组 gate 后，Akeep 可以替代现有“Agent history 到 S3”的专用服务。
它不会替代 Git、系统备份、Provider 原生 resume 或工作区 artifact 备份。

## MVP 范围

### v0.1：先替掉现有备份服务

必须具备：

- Claude Code、Codex、Grok、Kimi Code、OpenCode 原始数据发现；
- live SQLite 一致性快照；
- 内容寻址的增量 archive；
- 压缩与跨 recovery point 去重；
- 可选的上传前客户端加密，`none` 是完整支持的模式；
- 本地目录与 S3-compatible 两个 target；
- `doctor`、`backup`、`snapshots`、`verify`、`recover`；
- Linux systemd 定时器；
- 默认不改 Provider 文件、不传播删除、不覆盖恢复目标；
- 人类可读和 JSON 两种报告。

暂不做：

- GUI 和 Web dashboard；
- 托管账号、付费和团队权限；
- 二十个 Provider；
- embedding、token/cost 图表；
- 自动清理本机冷 session；
- 跨 Provider 原生 session 注入。

### v0.2：让历史真正可继续

备份替换 gate 通过后，再加入：

- Claude/Codex 本地全文搜索；
- Markdown/JSON 导出；
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

v0.1 不做庞大的插件系统，只保留清晰的内部边界。S3-compatible 已经覆盖
AWS S3、R2、MinIO、Backblaze B2 等大量服务；本地 filesystem 又能覆盖外接
硬盘、NAS mount 和由 Syncthing/rclone 管理的目录。等真实用户要求 WebDAV
或其他 target 时再新增。

Provider adapter 不应决定 archive 格式，storage target 也不应看懂 transcript。
这样新增 Agent 不会修改 archive/恢复核心；开启客户端加密时，新增 backend
也不会接触明文。

## 隐私承诺

“Privacy first” 不能只是一句宣传语，也不等于强制用户承担丢失密钥的风险。
它应变成可测试的产品约束：

- 默认离线、无账号、无 telemetry；
- 未配置远端时没有网络请求；
- `encryption = none` 是正式支持和测试的模式；
- 加密模式在创建 vault 时确定，不能每次 backup 临时切换；
- 本地 target 可以默认不开加密；
- 远端 target 明确推荐加密，但说明风险后允许用户不开；
- `doctor` 永远显示当前加密模式和 storage operator 是否能读取内容；
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

## 市场与差异化

当前市场已经证明“历史管理”是真需求，同时也说明普通 viewer 很拥挤：

- [ctx](https://www.ctx.rs/) 已经把跨 coding-agent 的本地 SQLite 索引与搜索
  做成开源 CLI；
- [Agent Sessions](https://github.com/jazzyalex/agent-sessions) 已经提供多 Agent
  的本地 macOS 浏览、搜索和 resume；
- [Claude Code History Viewer](https://github.com/jhlee0409/claude-code-history-viewer)
  已经能离线浏览多个 coding assistant；
- [Contextify](https://contextify.sh/) 已经覆盖本地历史、搜索和云同步；
- [Claude Sync](https://claude-sync.com/) 已经强调 Claude Code 的端到端加密同步；
- [Spool](https://spool.pro/) 已经主打分享和继续 Agent session。

因此 Akeep 不应主打：

- “我们也能看 JSONL”；
- “我们支持的 Agent 数量最多”；
- “我们能画 token 图”；
- “我们把一点文本压小了”。

Akeep 的楔子应该是：

> **Not just backed up. Proven recoverable.**

需求也不是抽象假设：Claude Code
[官方 sessions 文档](https://code.claude.com/docs/en/sessions)说明本地 transcript
默认 30 天清理，并允许通过 `cleanupPeriodDays` 调整。Akeep 不应攻击 Provider
的产品选择，而应清楚告诉用户：可 resume 的本地工作状态仍有 retention 和格式
生命周期；如果它值得保留，就应拥有独立、可验证的 recovery point。

组合差异是：

> 原始状态保真 + 用户可选客户端加密 + 压缩去重 + Provider-aware
> 一致性快照 + 可验证恢复 + 后续 semantic handoff。

## 宣传方式

### 首页

主标题：

> **Your agent history is not a cache.**

副标题：

> **Back up, verify, and recover Claude Code, Codex, and other coding-agent
> work—without giving a storage provider the plaintext.**

第三句应直接给证据：

> **Every recovery point is content-addressed and tested before Akeep calls it
> complete. Client-side encryption is available when you want it.**

不要把 “AI history compression tool” 当主定位。用户不会为了压缩 JSONL 安装
一个高权限工具，但会为了避免丢掉数百小时工作安装可靠恢复工具。

### 第一支演示

用隔离 fixture 做 90 秒、可复现的恢复演示：

1. `akeep doctor` 发现五个 Provider；
2. `akeep backup` 创建 recovery point；
3. 删除临时 fixture，不碰真实用户数据；
4. `akeep recover`；
5. 显示逐文件 hash 完全一致；
6. 破坏一个 archive object；
7. `akeep verify` 明确拒绝。

这比先做漂亮 dashboard 更能建立备份产品最需要的信任。

### 发布标题

完成真实 dogfood restore 之后：

> **Show HN: Akeep – I had 50GB of coding-agent history, so I built a backup I
> could actually verify**

其他可测试标题：

- “Your coding-agent history is plaintext, local, and not a backup.”
- “Akeep: client-encrypted recovery points for Claude Code and Codex.”
- “We corrupted our own Agent backup. Akeep caught it before restore.”

### 首发渠道

1. GitHub README + 可复现 demo；
2. Show HN；
3. Claude Code、Codex、OpenCode 相关社区；
4. 一篇工程文章：如何在 Agent 正在运行时一致地备份 JSONL 和 SQLite；
5. 一篇安全文章：Agent session 中到底会泄漏什么，客户端加密解决什么；
6. 恢复演练和兼容矩阵持续公开更新。

### 信任材料

公开发布前至少具备：

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

## MVP 的唯一北极星

首发前必须能回答：

> 如果这台机器今天损坏，我们是否能只依赖 Akeep，在一台干净机器上恢复出
> Provider 可识别、哈希一致的历史？

答案不是经过演练的“是”，就还没有 MVP。
