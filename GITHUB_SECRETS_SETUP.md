# GitHub Secrets 配置说明（更新签名）

Release 工作流靠两个仓库 Secret 给安装包签名，客户端的 updater 用嵌在二进制里的公钥验签。
没配 → 产物无签名 → **updater 验签失败，而这个失败不会在构建时报错**。

| Secret 名 | 内容 |
|---|---|
| `TAURI_SIGNING_PRIVATE_KEY` | `~/.tauri/synaroute.key` 的**全文**（`export TAURI_SIGNING_PRIVATE_KEY="$(cat ~/.tauri/synaroute.key)"` 取到的那串） |
| `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` | 该私钥的口令。**当前这把钥没有口令**（见 [docs/18 §3.4](docs/18-重装系统开发环境清单.md)），故填空值 |

配置位置：仓库 → Settings → Secrets and variables → Actions → New repository secret。
消费点是 [`.github/workflows/release.yml`](.github/workflows/release.yml) 的 `tauri-action` 一步。

配好之后推 tag，Release 会带上 `latest.json` 与各安装包的 `.sig`。

## 🔴 这个文件曾经泄露过签名私钥 —— 别再把值写回来

2026-08-14 的 `29b9257` 把私钥（`DE14C6EC68286277`）与它的解密口令**一起**写进了本文件正文，
推到了公开仓库，直到 2026-08-23 才发现。

- 那把钥刚好在 08-16 被换掉，所以只有 **v0.1.23** 那一批客户端嵌着它。是运气，不是防线。
- 而 v0.1.23 的用户处境最坏：他们**收不到**你之后的正版更新（0.1.26+ 用新钥签，
  他们的客户端不认），却**能收到**任何人用泄露钥签的伪造更新。
- 删掉值**不等于修好了**：它在 git 历史里、在已 fork 的副本里。已公开的密钥只能
  当作永久失效来处置。

当时本文件的「安全说明」写着「私钥已加密，即使泄露也需要密码才能使用」——
而密码就在同一个文件的下面几行。**把口令和它保护的东西放在一起，等于没有口令。**

现在有一道机械判据盯着这件事：[`scripts/lib/secret-scan.mjs`](scripts/lib/secret-scan.mjs)，
由 `npm run check:forbidden`（每次 `npm run gates` 与 CI 都跑）与 `npm run audit:release` 共同消费。
它按**解码后的内容特征**判 —— 因为这次的密钥是包了一层 base64 存的，
按文件名判和按明文 grep 判**都抓不到**。
