# Laya 本地一键启动

Windows 桌面版推荐在 **大脑聚合 → 动态思考 → 判断服务选择 Laya → 一键安装并运行**。界面显示环境检查、依赖安装、模型加载和运行状态，可展开运行记录；失败后可重试。模型就绪后自动保存本地接口及多语言模型，清除之前的判断服务密钥，保留所选运行方式。端口 8000 被占用时自动选其他空闲端口。可点击“停止”，退出 SynaRoute 也会清理它启动的进程。安装环境保留供下次使用。当前用户系统代理会在未设置 HTTPS_PROXY 时用于下载。

以下脚本仍可独立使用：

Windows 双击 **启动Laya.cmd**。脚本自动查找 Python 3.10–3.13；缺失时通过 winget 安装当前用户的 Python 3.12。没有 winget 时会提示先安装 Python。安装失败不会删除已有环境，修复网络后可重试。

首次联网安装 CPU PyTorch、固定版本 `laya[serve]==0.3.29`，并由 Laya 下载多语言模型。依赖装在 `%LOCALAPPDATA%\SynaRoute\laya\env-<标识>`（兼容旧版 `venv`），`environment.txt` 指向通过校验的环境，模型由 Hugging Face 默认缓存管理。脚本不修改系统 Python、不创建后台服务。依赖和权重仍需要磁盘及内存，不能把它当作零资源开销；具体占用取决于机器和模型版本。

等终端显示服务启动完成，在 SynaRoute → 大脑聚合 → 动态思考中填写：

- 服务：Laya (laya-serve)
- 地址：`http://127.0.0.1:8000/v1/systemone`
- 模型：`multilingual`
- API Key：默认留空；若启动前设置了 `LAYA_API_KEY`，填写相同值。
- 运行方式：先「仅建议」，查看运行日志后再选择「自动」。

逐个授权支持相应思考参数的会诊模型，然后保存。即使 Laya 没启动，会诊也会回退原配置。保持终端打开，Ctrl+C 停止；再次双击复用安装。模型保持驻留以避免每轮冷启动，默认 CPU 两线程、最多驻留一个模型，只监听本机。

用于 Codex / Claude 原生档位时，在同页“原生自动思考”启动独立会话，再打开“自动选择原生档位”。它复用这里保存的服务，但开关独立于会诊模式，无需授权会诊模型；按原生客户端目录选择档位。详见 [原生会话说明](../native-effort/README.md)。

```powershell
# 只检查环境，不安装、不启动
powershell -NoProfile -ExecutionPolicy Bypass -File .\scripts\laya\start.ps1 -CheckOnly
# 端口冲突时换端口，并同步修改 SynaRoute 接口地址
powershell -NoProfile -ExecutionPolicy Bypass -File .\scripts\laya\start.ps1 -Port 8010 -Threads 4
```

首次权重下载失败请检查 Hugging Face 连通性；启动失败请阅读窗口错误。首次推理超过应用的四秒判断预算会安全回退，可待模型预热后重试。此脚本使用原项目 `laya-serve`，不是第三方 `laya-server` Docker 项目。包版本固定，间接依赖和上游权重未锁定哈希。

接口依据：[Laya 原项目](https://github.com/NandhaKishorM/laya)、[服务实现](https://github.com/NandhaKishorM/laya/blob/main/laya/serve.py)。应用也支持 [Jev](https://docs.typesafe.ai/api) 和 OpenAI-compatible Chat Completions 判断服务。判断质量需要在实际任务上评估，不能把模型分数当作正确率。

## 故障恢复与验证

- 安装、临时目录与模型缓存所在磁盘检查可写性和剩余空间；新装/修复至少预留 6 GB，可用环境复用至少预留 128 MB。此值是预检门槛，不是峰值磁盘占用承诺。
- 安装锁覆盖整个服务生命周期，避免不同程序实例或独立脚本同时修改环境。
- 网络连接/读取设有限时，pip 网络重试 3 次；桌面管理器另设阶段总时限：检查 2 分钟、Python 15 分钟、依赖 30 分钟、模型加载 20 分钟。运行后持续不健康 60 秒则停止并报告。
- “修复环境并运行”创建独立环境，安装并导入校验通过后原子切换指针；失败或中断不覆盖旧环境。旧环境及未完成目录保留，便于排错；修复需要额外空间。不会删除共享模型缓存。
- 界面按磁盘、权限、网络/证书、Python、端口、内存、依赖、超时分类给出处理建议，并可复制最近 40 行经过 URL 脱敏的日志。记录保存在本地 `last-status.json`，重启可看到上次失败或中断。分享前仍应检查日志中的本机路径等信息。
- 服务健康后才保存本地接口；若中途切换分类/关闭面板，可在返回后点击“使用此本地服务”。安装失败不覆盖当前接口配置。

离线故障测试：`powershell -NoProfile -ExecutionPolicy Bypass -File scripts/laya/test-installer.ps1`。使用可控的假 Python 执行真实安装脚本，不访问网络。真实安装与推理冒烟（会下载依赖和权重）：`cargo test --manifest-path src-tauri/Cargo.toml real_install_and_inference_smoke --lib -- --ignored --nocapture`。测试结束自动停止服务，环境保留。
