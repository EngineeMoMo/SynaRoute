# Laya 本地一键启动

Windows 桌面版推荐在 **大脑聚合 → 动态思考 → 判断服务选择 Laya → 一键安装并运行**。界面显示环境检查、依赖安装、模型加载和运行状态，可展开运行记录；失败后可重试。模型就绪后自动保存本地接口及多语言模型，清除之前的判断服务密钥，保留所选运行方式；若这会替换当前填写的远端接口或已保存的密钥，会先弹确认。其他分类若仍指向本服务以前用过的端口，会一并改到新端口；指向别处（包括你自己运行的 Laya）或带有自己密钥的分类不动。端口 8000 已有程序应答（包括监听 `0.0.0.0` / `[::]` 的开发服务器）时自动选其他空闲端口，不会抢占它。可点击“停止”，退出 SynaRoute 也会清理它启动的进程；正在安装 Python 时点停止会等安装程序结束再停（最多 10 分钟，超时强制），避免留下装了一半的 Python。安装环境保留供下次使用；环境已装好时按钮变为“启动本地服务”，直接启动、不再弹下载确认，且这条路径只启动、绝不安装。

面板里两个开关都默认关闭，是本机设置（存于数据目录的 `laya-local.json`，不随配置导出）：

- 「随 SynaRoute 自动启动本地服务」：每次启动应用约 5 秒后自动启动**已安装**的环境，只用上次健康检查通过的端口；环境未安装或该端口被其他程序占用时不启动、也不下载或安装任何内容，面板显示原因，点「重试」会另选端口。
- 「通过镜像 hf-mirror.com 下载模型」：无法访问 Hugging Face 时开启，之后的模型下载改从这个第三方镜像站进行（它会看到下载请求）；下载的文件仍逐个核对已验证的 SHA-256。Python 与依赖的来源不变，已下载的模型不会重新下载。

未启动时运行日志会写明「本地判断服务未运行」。未设置 HTTPS_PROXY 时使用当前用户的系统代理下载，包括 PAC 自动配置脚本（pip 与 huggingface_hub 自己不认 PAC）。

以下脚本仍可独立使用：

Windows 双击 **启动Laya.cmd**。需要安装时先请求确认，桌面版也会先显示确认窗口。新建或修复环境需要 Python 3.12（x64），已有的 3.10–3.13 环境照常运行。缺少 Python 时从 python.org 下载 Python 3.12.10 x64 官方安装包（约 26 MB），核对 python.org 公布的 SHA-256 与 Python Software Foundation 发布者签名后，以当前用户静默安装，不需要 winget、不修改 PATH、不自动重启。下载最多尝试 3 次，安装完成后自动校验并继续安装依赖和模型。安装失败不会删除已有环境，可在界面点击“重试”。Arm 设备在 Windows 11 上通过系统的 x64 模拟运行；32 位 Windows 与 Windows 10 的 Arm 设备不支持，会明确提示。

依赖只从 [`requirements.lock.txt`](requirements.lock.txt) 安装：45 个包的确切版本与 SHA-256，与 Laya 0.3.29 一起验证过（CPython 3.12 win_amd64，CPU 版 PyTorch），以 `--require-hashes --only-binary :all:` 安装，任何不符的文件都会被拒绝；锁定文件缺失时不安装。依赖装在 `%LOCALAPPDATA%\SynaRoute\laya\env-<标识>`（兼容旧版 `venv`），`environment.txt` 指向通过校验的环境，模型由 Hugging Face 默认缓存管理。脚本不改动已有的 Python、不修改 PATH、不创建后台服务；但自动安装的 Python 3.12.10 是一份标准的当前用户安装，会出现在「应用和功能」里，卸载 SynaRoute 不会移除它。卸载时勾选「删除应用数据」会删除 `%LOCALAPPDATA%\SynaRoute\laya`；共享的 Hugging Face 模型缓存与该 Python 需在各自位置手动清理。本机实测：环境约 0.91 GB、模型约 0.65 GB、pip 缓存约 0.18 GB、Python 约 70 MB，首次安装约需 3.5 GB 空间；运行时常驻内存约 1.6 GB。

等终端显示服务启动完成，在 SynaRoute → 大脑聚合 → 动态思考中填写：

- 服务：Laya (laya-serve)
- 地址：`http://127.0.0.1:8000/v1/systemone`
- 模型：`multilingual`
- API Key：默认留空；若启动前设置了 `LAYA_API_KEY`，填写相同值。
- 运行方式：先「仅建议」，查看运行日志后再选择「自动」。

逐个授权支持相应思考参数的会诊模型，然后保存。即使 Laya 没启动，会诊也会回退原配置。保持终端打开，Ctrl+C 停止；再次双击复用安装。模型保持驻留以避免每轮冷启动，默认 CPU 两线程、最多驻留一个模型，只监听本机。

用于 Codex / Claude 原生档位时，在同页“原生自动思考”启动独立会话，再打开“自动选择原生档位”。它复用这里保存的服务，但开关独立于会诊模式，无需授权会诊模型；判断方式相同，只会把手动档位提高到 high。详见 [原生会话说明](../native-effort/README.md)。

## 判断方式与实测

Laya 只回答一个是非题：「这个任务是否需要深入、多步的推理（如系统架构、含糊的调试、形式化证明）？」。得分（0–1）不低于 0.75 且输入未被截断时，把思考强度提高到 high；否则保持原有设置。**它从不降低思考强度**。在 36 条中英任务的留出测试中（high / medium / low 各 12 条），需要深入推理的 12 条里提高了 4 条，其余 24 条里误提高 1 条，没有降档；此前的分级问法在同一组数据上只被采用 3 条，其中 1 条是有害的降档。阈值在另一组 18 条任务上标定，没有拿测试集调参。约 1000 token 以上的输入会截断，此时保持原有设置。延迟不是瓶颈（短句约 0.2 秒，截断长文约 1.8 秒）。判断质量仍需在实际任务上评估，不能把模型分数当作正确率。

```powershell
# 只检查环境，不安装、不启动
powershell -NoProfile -ExecutionPolicy Bypass -File .\scripts\laya\start.ps1 -CheckOnly
# 端口冲突时换端口，并同步修改 SynaRoute 接口地址
powershell -NoProfile -ExecutionPolicy Bypass -File .\scripts\laya\start.ps1 -Port 8010 -Threads 4
# 经镜像 hf-mirror.com 下载模型（仍核对 SHA-256）
powershell -NoProfile -ExecutionPolicy Bypass -File .\scripts\laya\start.ps1 -ModelMirror
```

首次权重下载失败请检查 Hugging Face 连通性，或改用镜像下载；启动失败请阅读窗口错误。首次推理超过应用的四秒判断预算会安全回退，可待模型预热后重试。此脚本使用原项目 `laya-serve`，不是第三方 `laya-server` Docker 项目。包版本锁定；模型权重固定到已缓存的快照，没有缓存时固定到已验证的提交（升级 Laya 版本时一并更新），因此已装好的环境启动时不需要访问 Hugging Face（无法访问时冷启动实测从约 33 秒降到约 12 秒）。模型版本为已验证的提交时，脚本设置 `LAYA_SHA256_DIGESTS`，laya 会在解析任何权重前拒绝 SHA-256 不符的文件（每次启动约多 0.4 秒）；`tokenizer_config.json` 不在其中，因为 laya 加载前会就地改写它。可用 `LAYA_REVISION` / `LAYA_SHA256_DIGESTS` 自行覆盖；改用其他版本时没有可核对的摘要。

接口依据：[Laya 原项目](https://github.com/NandhaKishorM/laya)、[服务实现](https://github.com/NandhaKishorM/laya/blob/main/laya/serve.py)。应用也支持 [Jev](https://docs.typesafe.ai/api) 和 OpenAI-compatible Chat Completions 判断服务。

## 故障恢复与验证

- 安装目录、模型缓存、pip 缓存与临时目录按所在磁盘合计后与剩余空间比较：新装/修复时安装目录约 1.5 GB、可用环境复用至少 128 MB；模型未缓存时再加约 1 GB，新装/修复时临时文件与 pip 缓存各约 0.5 GB。网络路径（UNC）读不到剩余空间时跳过预检并写入日志。
- 安装锁覆盖整个服务生命周期，避免不同程序实例或独立脚本同时修改环境。
- 网络连接/读取设有限时，桌面版下 pip 重试 5 次、单次超时 60 秒；桌面管理器另设阶段总时限：检查 2 分钟、Python 15 分钟、依赖 30 分钟、模型加载 20 分钟，等待停止 10 分钟后强制。运行后持续不健康 60 秒则停止并报告。加载与恢复期间每秒检查一次健康，正常运行时每 5 秒一次。
- “修复环境并运行”创建独立环境，安装并导入校验通过后原子切换指针；失败或中断不覆盖旧环境。下一次安装或修复开始前，会清理未被指针引用的 `env-<标识>` 目录（失败或中断留下的、以及被替换下来的旧环境），只保留当前指针指向的那个；`venv`、名字不符和链接目录一律不动。修复需要额外空间。不会删除共享模型缓存。
- 界面按磁盘、权限、网络/证书、Python、平台、模型完整性、端口、内存、依赖、需要安装、超时分类给出处理建议，并可复制最近 40 行经过 URL 脱敏的日志。记录以原子写入保存在本地 `last-status.json`，重启可看到上次失败或中断。分享前仍应检查日志中的本机路径等信息。
- 服务健康后才保存本地接口；若中途切换分类/关闭面板，可在返回后点击“使用此本地服务”。安装失败不覆盖当前接口配置。

离线故障测试：`powershell -NoProfile -ExecutionPolicy Bypass -File scripts/laya/test-installer.ps1`。使用可控的假 Python 执行真实安装脚本，不访问网络；覆盖安装包摘要与签名、关键区标记、哈希锁定安装、锁定文件缺失、非 ASCII 路径、按磁盘合计的空间、PAC 与手动代理、平台检查等。真实安装与推理冒烟（缺环境时会下载依赖和权重）：`cargo test --manifest-path src-tauri/Cargo.toml real_install_and_inference_smoke --lib -- --ignored --nocapture`；它核对就绪接口的记录并用生产判断问题跑一次真实推理，结束自动停止服务，环境保留。
