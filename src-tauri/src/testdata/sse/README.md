# SSE 黄金样本夹具

六个流式转换方向（`SseDirection` 的全部变体）的回归安全网。测试代码在
`src-tauri/src/upstream/sse_golden.rs`。

## 为什么有这套东西

流式协议转换是与非流式中枢**并行的第二套矩阵**：非流式走 hub-and-spoke（3 个协议只需 6 个
单边函数），而流式是 6 个**手写的有向方法**。两套矩阵已经出现过能力漂移，而在这套测试之前
**没有任何一条测试覆盖全部 6 个方向**。

它的直接用途是给 `upstream.rs` 的拆分（docs/15 P2-1）当安全网：拆分不该改变任何行为，
而「行为没变」需要一个能自动判定的判据，不能靠人读 diff。

## 文件

### 输入夹具（上游流）

上游形态只有 3 种，每种喂给 2 个下游即可覆盖全部 6 个方向。

| 文件 | 来源等级 | 上游 | 说明 |
|---|---|---|---|
| `upstream_anthropic.sse` | **真实抓包** | api.anthropic.com / claude-opus-4.8 | 从本机运行日志 `%APPDATA%/SynaRoute/logs/2026-07-17.jsonl` 的 `request` 事件 `responseBody` 回捞。仅做两类编辑：① 正文替换（原文是用户真实对话且含本机绝对路径，按项目硬规则不得入库）；② message id 改写。**不含 thinking 与 tool_use** —— 那次请求没开思考、也没调工具 |
| `upstream_chat.sse` | 按规范构造 | 形如 glm-4.6 | 本机日志里没有 Chat 上游的响应体，抓新的需要真实厂商 Key，此处不具备条件 |
| `upstream_responses.sse` | 按规范构造 | 形如 gpt-5.5 | 同上 |

**手写夹具能守住「重构前后行为一致」，但守不住「我们对上游形态的理解本身就错了」。**
日后若抓到真流，应整份替换并把本表对应行改成真实来源。

每份夹具首部有 `:` 开头的 SSE 注释行，记录来源、上游、编辑内容与事件谱 —— 出处跟着文件走，
不依赖本 README。`process_line` 只认 `data:` 前缀，注释行天然被忽略
（有测试 `comment_and_bare_event_lines_produce_no_output` 钉住这一点）。

夹具刻意都**含中文**：多字节字符是分块不变性测试的关键，纯 ASCII 夹具测不出 UTF-8 边界问题。

### 黄金输出（`golden_*.sse`）

六个方向各一份，由测试自动生成。**它们不判断对错，只判断有没有变。**

## 重新生成

```bash
cd src-tauri && SYNAROUTE_UPDATE_GOLDEN=1 cargo test --lib sse_golden
cd src-tauri && cargo test --lib sse_golden
```

必须跑第二遍：`include_str!` 是编译期嵌入，重写后要重新编译才生效。这个「跑两遍」的动作
也正好是一道人工复核闸口。

> ⚠️ **黄金文件变了就是行为变了。** 改动 PR 必须在描述里逐条说明每一处 diff 的成因。
> 禁止无脑 `UPDATE_GOLDEN` —— 那等于把回归当成新基线固化下来，安全网反而成了缺陷的护身符。

## 测试构成

| 测试 | 作用 |
|---|---|
| `golden_snapshots_match_for_all_six_directions` | 输出逐行快照。拆分代码时任何搬错都会被精确到行地指出 |
| `output_is_independent_of_chunk_boundaries` | 四档切分（整份/按行/7 字节窗/逐字节）输出必须一致。**最抗重构的一条**——不关心事件长什么样，只关心行缓冲状态机对任意字节切分等价 |
| `capability_matrix_is_pinned` | 六方向 × 六类信号逐格钉死。见下 |
| `every_sse_direction_has_a_golden_case` | 穷举守卫：加第 7 个方向时先红 |
| `sse_direction_mapping_agrees_with_cases` | 防止两处对「谁是上游谁是下游」的理解分叉 |
| `fixtures_contain_no_generated_style_ids` | 防止重新抓包时上游 id 撞进规范化规则被静默改写 |
| `comment_and_bare_event_lines_produce_no_output` | 钉住夹具注释行不污染快照这个前提 |

## 能力矩阵记录到的已知缺口

`capability_matrix_is_pinned` 里逐格标注。当前钉住的三类缺口（都不是本次引入的，是本次
**第一次被显式记录**）：

1. **推理内容在所有「转出」方向都丢失**。`chat→anthropic` / `chat→responses` /
   `responses→anthropic` / `responses→chat` 四个方向，上游的 `reasoning_content` 与
   `reasoning_summary_text.delta` 都没有转成下游对应形态。
   用户感受：接 DeepSeek/GLM 思考档时「模型好像不思考了」。
2. **Chat/Responses → Anthropic 方向输入用量恒为 0**。Anthropic 语义要求 `input_tokens`
   出现在 `message_start`，而 Chat/Responses 的 usage 要到流尾才到，那时又没有回填。
   后果：按 Anthropic 记账时成本统计偏低。
3. `upstream_anthropic.sse` 是纯对话抓包，thinking 与 tool_use 两格属于**夹具所限**
   而非缺口 —— 表里已区分标注，别把两者混为一谈。

矩阵里每个 `false` 都必须写清是「有意为之」「已知缺口」还是「夹具所限」。空着的格子等于没钉。
