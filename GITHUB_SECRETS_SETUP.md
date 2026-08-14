# GitHub Secrets 配置说明

## 需要配置的 Secrets

请访问：https://github.com/EngineeMoMo/SynaRoute/settings/secrets/actions

点击 "New repository secret" 添加以下两个密钥：

### 1. TAURI_SIGNING_PRIVATE_KEY

**Name**: `TAURI_SIGNING_PRIVATE_KEY`

**Value**: 
```
dW50cnVzdGVkIGNvbW1lbnQ6IHJzaWduIGVuY3J5cHRlZCBzZWNyZXQga2V5ClJXUlRZMEl5clJLUTJzN2FHVGxMK0wwV01BWHp4dlJHSHZoSEw5eGlBbDdYdUhxYU50Z0FBQkFBQUFBQUFBQUFBQUlBQUFBQVljRWpicG52bjFscTIyTG5sTFNRZnRwWHZaMFJnY3E2VmdwcldHYmNYaE5aQWhvOVZGOS82WDZUbExBTEQ2aDU2eWFpcTY5SlBSQnRwWUdLVTFQOVhLTkxrdytPdWVNNGtXSStjdEFjMit1ZndsenlJWWQwUEllcHJsNUM4cU9BajMvNmZsNGxxRFU9Cg==
```

### 2. TAURI_SIGNING_PRIVATE_KEY_PASSWORD

**Name**: `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`

**Value**: 
```
mhm292117.
```

---

## 配置步骤

1. 访问 https://github.com/EngineeMoMo/SynaRoute/settings/secrets/actions
2. 点击 "New repository secret"
3. 输入 Name: `TAURI_SIGNING_PRIVATE_KEY`
4. 粘贴上面的 Value（完整的私钥字符串）
5. 点击 "Add secret"
6. 重复步骤 2-5，添加 `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`

---

## 验证

配置完成后：
1. 推送代码和标签
2. GitHub Actions 将能够正确签名发布包
3. 用户可以通过应用内更新器自动更新

---

## 安全说明

- ⚠️ **私钥已加密**：密钥文件使用密码加密，即使泄露也需要密码才能使用
- ⚠️ **GitHub Secrets 安全**：Secrets 在 GitHub 中加密存储，日志中自动隐藏
- ⚠️ **本地密钥文件**：`synaroute-signing.key` 不要提交到 Git，已在 .gitignore 中

---

## 密钥文件备份

本地密钥文件位置：
- 私钥: `C:/Users/Administrator/Desktop/temp/demo/synaroute-signing.key`
- 公钥: `C:/Users/Administrator/Desktop/temp/demo/synaroute-signing.key.pub`

**请妥善备份这两个文件！丢失后将无法签名更新包。**
