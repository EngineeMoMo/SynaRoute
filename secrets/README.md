# secrets/ —— 加密后可入库的凭据

本目录只放**加密后的密文**。明文私钥被 [`.gitignore:37`](../.gitignore) 的 `*.key` 拦着，
永远不会进仓库 —— 别去掉那条规则。

## 里面是什么

| 文件 | 内容 | 加密方式 |
|---|---|---|
| `synaroute.key.gpg` | Tauri 在线更新签名私钥（明文对应 `~/.tauri/synaroute.key`） | GnuPG 对称加密，AES-256，口令保护 |

## 为什么要加密后才入库

本仓库是**公开仓库**（且必须公开：私有仓会让 updater 报
`Could not fetch a valid release JSON`）。而这把私钥**没有口令保护** —— 拿到文件即可签名。

明文推上去的后果：任何人都能签出一个「验签通过」的更新包，而所有 SynaRoute 客户端的
updater 会把它当作正品安装。签名是用户与伪造更新之间唯一的一道关卡。

更麻烦的是**这把钥匙泄露后没法悄悄换**：已发布客户端的二进制里嵌着当前公钥
（指纹 `1F6689C022960775`），换私钥等于**所有老用户的自动更新全部失效**，只能手动重装。

所以：密文可以公开，明文绝对不行。密文的强度 = 你的口令强度。

## 重装系统后还原

```bash
gpg -d secrets/synaroute.key.gpg > ~/.tauri/synaroute.key
```

还原后验证公钥指纹与 `src-tauri/tauri.conf.json` 的 `plugins.updater.pubkey` 配对，
再按 [docs/18](../docs/18-重装系统开发环境清单.md) §3.4 设 `TAURI_SIGNING_PRIVATE_KEY`。

## 重新生成密文（换口令 / 换钥匙时）

```bash
gpg --symmetric --cipher-algo AES256 --pinentry-mode loopback \
    -o secrets/synaroute.key.gpg ~/.tauri/synaroute.key
```

`-o` 目标已存在时 gpg 会问是否覆盖；要直接覆盖加 `--yes`。

## ⚠️ 口令丢了 = 钥匙丢了

密文谁都解不开，包括你自己。**口令必须另存**（密码管理器），
且**仍要留一份明文私钥的离线备份**（U 盘 / 外部盘）——
git 里这份是「换机自助恢复」用的，不是唯一副本。
