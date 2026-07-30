# OpenNARS repository map

调查日期：2026-07-12

组织主页：https://github.com/opennars

## 结论

OpenNARS 组织里没有一个仓库能在所有语境下都称为唯一的“核心”：

- **经典、研究型 OpenNARS 核心**是
  [`opennars/opennars`](https://github.com/opennars/opennars)。它是 Java 实现，README
  明确把 `src/main/java/org/opennars` 标为 reasoning engine，并由 GUI、Lab 和
  Applications 等外围仓库调用。
- **面向实际 Agent、在线学习和实时控制的核心**是
  [`opennars/OpenNARS-for-Applications`](https://github.com/opennars/OpenNARS-for-Applications)，
  简称 **ONA**。它是独立重写的 C 实现，不是经典 Java 代码库的分支；官方说明强调
  reasoning performance、procedure learning、实时应用以及 x86/ARM 部署。

如果目标是学习 OpenNARS 的经典体系和研究架构，先看 `opennars/opennars`；如果目标是
实现可嵌入的实时 Agent，或选择 Rust 重写的行为基线，应选择 ONA。

## 主要仓库

| 仓库 | 定位 | 语言 | 最近代码推送 | 判断 |
| --- | --- | --- | --- | --- |
| [`opennars/opennars`](https://github.com/opennars/opennars) | OpenNARS for Research 3.0+；经典完整推理器 | Java | 2021-03-31 | 历史主干、经典研究核心 |
| [`OpenNARS-for-Applications`](https://github.com/opennars/OpenNARS-for-Applications) | 为应用设计的通用推理组件，简称 ONA | C | 2025-05-30 | 实际应用与实时 Agent 的首选核心 |
| [`OpenNARS-for-Research`](https://github.com/opennars/OpenNARS-for-Research) | 基于 v1.5.8 重建的 v3.1.1 研究版 | Java | 2021-10-10 | 3.1.x 研究分支，不是 ONA 的上游 |
| [`opennars_v3.1.2`](https://github.com/opennars/opennars_v3.1.2) | 上述研究线的 v3.1.2 快照 | Java | 2021-10-14 | 版本化研究快照 |
| [`opennars_core`](https://github.com/opennars/opennars_core) | 基于 v1.5.8 的 v3.1.x 尝试 | Java | 2021-05-25 | 名称容易误导；不是当前活跃核心 |
| [`OpenNARS-4`](https://github.com/opennars/OpenNARS-4) | README 实际称 PyNARS，参考 3.0.4/3.1.0 设计的 Python 实现 | Python | 2024-11-27 | 语言移植/实验路线，不宜当官方基准核心 |
| [`NARS-GPT`](https://github.com/opennars/NARS-GPT) | ONA 的 GPT 自然语言通道 | Python | 2025-01-22 | 外围通道，不是推理核心 |
| [`Channels`](https://github.com/opennars/Channels) | 感知、知识、传感运动通道 | Python | 2021-10-01 | 集成层 |

## 经典 Java 项目的仓库关系

`opennars/opennars` 的 README 给出的关系是：

- `opennars-parent`：Maven 父项目；
- `opennars`：推理核心，含 memory、inference engine、control mechanism；
- `opennars-gui`：GUI；
- `opennars-lab`：实验环境和演示；
- `opennars-applications`：基于经典 Java 核心的应用；
- `opennars-middleware`、`opennars-web`：中间件和 Web 外围层。

因此，寻找经典代码时不要从 `opennars-lab` 或 `opennars-applications` 开始，它们都以
`opennars/opennars` 为底层推理器。

## ONA 为什么更适合作为实时 Agent 基线

ONA 的 README 明确说明它是一个全新平台，综合了：

- OpenNARS 的逻辑和概念；
- ANSNA/20NAR1 的事件处理与过程学习；
- ALANN 的控制模型。

其代码组织也更接近一个可移植的推理内核：

- `Term`、`Truth`、`Stamp`、`Event`：基本语义对象；
- `Narsese`、`Variable`、`NAL`、`Inference`、`RuleTable`：语言与推理规则；
- `Concept`、`Memory`、各类索引/队列/表：有界资源下的记忆结构；
- `Decision`、`Cycle`、`NAR`：目标、操作和工作循环；
- `Shell`、`NetworkNAR`：交互及 UDP 集成；
- `src/unit_tests`、`src/system_tests`、`examples`、`evaluation.py`：从单元测试到完整 Agent 场景的验证层。

官方还列出过程学习、Pong、Space Invaders、Cartpole、机器人、UDP NAR 等场景，并强调
x64、x86、ARM 和实时应用。这与低延迟、持续循环、资源受限的 Agent 系统方向更一致。

## 建议的阅读顺序

若目标是理解理论和经典架构：

1. `opennars/opennars` README 与 Narsese 示例；
2. memory / inference / control 三部分；
3. `OpenNARS-for-Research` 的 v3.1.x 差异；
4. ONA 的架构论文和实现取舍。

若目标是重写或构建实时 Agent：

1. ONA 的 `Term`、`Truth`、`Stamp`、`Narsese`；
2. `NAL.h`、`Inference`、`RuleTable`；
3. `Concept`、`Memory` 和各类有界容器；
4. `Cycle`、`Decision`、操作注册及反馈；
5. 单元测试、系统测试、示例和 `evaluation.py`；
6. 最后再对照经典 Java 实现，确认 ONA 有意做出的理论/控制差异。

## 当前工作区采用的上游

本工作区已选择 ONA 作为 Rust 实现的兼容基线：

- 仓库：`opennars/OpenNARS-for-Applications`
- 固定提交：`dc4efd0a`
- 提交日期：2025-05-16
- 本地快照：`upstream/ona`

固定提交而不是直接追踪 `master`，可以让规则、输出和测试结果具有可复现性。
