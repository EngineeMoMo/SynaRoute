# Laya 对照实验复现（2026-11-17，最终复核 2026-11-19）

结论与限制见 `docs/40-修复与对比实验-2026-11-17.md`。生产默认继续使用 Python Laya 0.3.29；Rust/ONNX 已接入为 Windows x64 可选托管后端，带离线 bundle、7 个 SHA-256 校验、原子选择、健康检查、进程树停止、回滚和 Python 回退。本目录中的比较脚本只负责可复现 benchmark，不参与生产安装或服务启动。

托管后端的原生验收已单独完成：Windows WebView2 的 48 题推理、bundle 导入两次、回滚、运行中拒绝切换、停止监听器、Python 回退均通过；Linux WebKitGTK 验收覆盖真实会话归档/恢复。对应日志是 `output/webview-windows-rerun-2026-11-18.log` 和 `output/linux-webview-2026-11-18.log`。Rust bundle 仍不是默认后端，也没有宣称可直接作为 macOS/Linux 发布包。

## 固定输入

- 对照基线使用 v0.1.90 tag 的行为，当前工作区候选版本为 v0.1.91；参考源码 `D:\ReferenceProject\laya` 固定在提交 `b1d656952f0f9581ea691a70cb6f38abf3eb13a9`（0.4.35）。
- 实验根目录 `D:\ReferenceProject\laya-comparison-2026-11-17`。新增参考源码、依赖、缓存、模型必须放在 D 盘这一参考目录体系内。
- 既有 multilingual 模型快照 `7b928d828b7b0e022f929d9bd2e44165aa270148`；模型路径记录在 `python-0.3.29.json`，本轮没有重新下载。
- `comparison-tasks.json`：48 条、校准/保留分区 12/36，SHA-256 `96129371ac344e8697431e1abeda521807f3d5149bade1d2a626a73f91339f29`。固定生产判断问题及 0.75 阈值。
- Python 3.12.14、torch 2.14.1+cpu、numpy 2.5.5、transformers 5.19.0。0.4.35 通过 `--source` 加载参考源码，复用相同依赖环境；不是“两个版本分别全新安装”的实验。
- ONNX 导出工具来自该提交的 `laya-rust/tools/export_onnx.py`，onnx 1.23.0、onnxscript 0.7.2、onnxruntime 1.30.0、psutil 7.2.2 放在实验根的 `deps`。Rust 使用 laya-onnx 0.1.0、ort 2.0.0-rc.13 / load-dynamic，调用同一 ONNX Runtime DLL。

## 在现有实验环境重复预测

从业务仓库根目录运行 PowerShell。以下只使用已经存在的环境和模型；缺失时先在 `D:\ReferenceProject` 准备相同版本，不把实验安装到生产 Python 环境。输出使用 `replay-` 前缀，保留原始三轮数据。

```powershell
$experiment = 'D:\ReferenceProject\laya-comparison-2026-11-17'
$baseline = Get-Content -Raw -LiteralPath "$experiment\python-0.3.29.json" | ConvertFrom-Json
$environmentRoot = $baseline.source
1..4 | ForEach-Object { $environmentRoot = Split-Path -Parent $environmentRoot }
$python = Join-Path $environmentRoot 'Scripts\python.exe'
$model = $baseline.model
$env:PYTHONPATH = "$experiment\deps"
$env:PYTHONDONTWRITEBYTECODE = '1'
$env:HF_HUB_OFFLINE = '1'
$env:HF_HOME = "$experiment\cache\huggingface"
$env:PIP_CACHE_DIR = "$experiment\cache\pip"
$env:TORCH_HOME = "$experiment\cache\torch"
$env:TEMP = "$experiment\tmp"
$env:TMP = "$experiment\tmp"
$env:OMP_NUM_THREADS = '2'
$env:MKL_NUM_THREADS = '2'
$env:TOKENIZERS_PARALLELISM = 'false'
& $python scripts/laya/compare.py --model $model --tasks scripts/laya/comparison-tasks.json --threads 2 --label python-0.3.29 --output "$experiment\replay-python-0.3.29.json"
& $python scripts/laya/compare.py --source D:\ReferenceProject\laya --model $model --tasks scripts/laya/comparison-tasks.json --threads 2 --label python-0.4.35 --output "$experiment\replay-python-0.4.35.json"
$env:ORT_DYLIB_PATH = "$experiment\deps\onnxruntime\capi\onnxruntime.dll"
& "$experiment\target\release\synaroute-laya-comparison.exe" "$experiment\onnx\multilingual" scripts/laya/comparison-tasks.json "$experiment\replay-rust-onnx.json"
```

每个命令都应检查退出码。比较加载延迟需要分别开新进程，不在一个 Python 会话里先后 import 两个版本；按轮次交换两个 Python 版本的运行顺序。每题内部跑三次，热延迟采用第三次。原始三轮中位数来自 `repeat-{1,2,3}-{python-0.3.29,python-0.4.35,rust-onnx}.json`。

上述直接运行 Rust 不测进程峰值。原始 Rust 内存值由外部进程每 10 ms 读取 working set 得到；Python 记录 Windows peak working set。这两者不能当成相同精度的峰值测量。重跑时若没有采样，应只比较时间、得分和文件体积。

## 重建 Rust 对照产物

实验根的 `rust-bench/Cargo.toml`、`Cargo.lock` 和 `src/main.rs` 已保留。`src/main.rs` 来自本仓 `onnx-bench.rs`，只用于复现 benchmark；生产托管 worker 位于 `src-tauri/laya-worker`，其 bundle 与选择/回滚逻辑不依赖本节的实验工程。依赖只在实验工程使用绝对参考路径，不写进生产 Cargo.toml。

```toml
[package]
name = "synaroute-laya-comparison"
version = "0.1.0"
edition = "2024"
[dependencies]
laya-onnx = { path = "D:/ReferenceProject/laya/laya-rust", default-features = false }
serde_json = "1"
ort = { version = "=2.0.0-rc.13", default-features = false, features = ["load-dynamic"] }
```

```powershell
$env:CARGO_HOME = "$experiment\cargo-home"
$env:CARGO_TARGET_DIR = "$experiment\target"
Copy-Item -LiteralPath scripts/laya/onnx-bench.rs -Destination "$experiment\rust-bench\src\main.rs"
& "$env:USERPROFILE\.cargo\bin\cargo.exe" +1.99.0 build --manifest-path "$experiment\rust-bench\Cargo.toml" --release --locked --offline
```

如需重新导出模型，使用单独目录避免覆盖原始产物：

```powershell
& $python D:\ReferenceProject\laya\laya-rust\tools\export_onnx.py $model --out "$experiment\onnx-replay" --verify
```

原始 `export.log` 含两组 fixture 数值对照；36 种 shape/session 检查的日志明确提示没有 PyTorch 对照，不将其表述为 36 组数值一致。

## 汇总和审计

```powershell
node scripts/laya/summarize-comparison.mjs D:\ReferenceProject\laya-comparison-2026-11-17 output/laya-comparison-2026-11-17.json
```

汇总包含数据集摘要、三轮中位数、最大得分差、保留题计数、运行环境、安装计划版本，以及 Cargo.lock/ONNX 两文件 SHA-256。`install-plan-0.4.35.json` 仅为 pip dry-run 解析计划，不能当成已安装的依赖清单。原始结果不覆盖；换题集、模型、问题、阈值或依赖时建立新日期目录并重新记录摘要。

## 相关独立实验

- 会话后台分页：`cargo +1.99.0 test --manifest-path src-tauri/Cargo.toml --offline --locked --lib compare_full_list_and_paged_index -- --ignored --nocapture`。临时目录可用 `TEMP`/`TMP` 指向业务仓库的 `output`；生成的是本仓测试夹具，不是参考源码。
- Claude 热恢复：`node scripts/native-effort/claude-restore.mjs <已安装 Claude 的绝对路径>`，输出在业务仓库 `output/claude-restore-*`；假 key、隔离配置、仅 loopback。
- 原生 effort：`node scripts/native-effort/probe.mjs --codex <已安装 Codex 的绝对路径> --claude <已安装 Claude 的绝对路径> --adapter-tests`。设置 `RUSTUP_TOOLCHAIN=1.99.0`、`CARGO_NET_OFFLINE=true` 和 D 盘 `TEMP`/`TMP`。
- 安装器离线故障：`powershell -NoProfile -ExecutionPolicy Bypass -File scripts/laya/test-installer.ps1`。它使用可控的假 Python，不能替代干净机器完整安装验收。
