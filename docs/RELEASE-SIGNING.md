# 更新包签名（updater signing）

> 适用范围：Tauri v2 自动更新（`tauri-plugin-updater` + `bundle.createUpdaterArtifacts`）。
> 目标：发布流水线用私钥签出 `.sig`，客户端用 `tauri.conf.json` 里的公钥验签；
> 公钥与私钥不匹配、或产物没签名时，客户端会拒绝安装。

## 1. 密钥存放位置

| 文件 | 位置 | 是否入库 |
|---|---|---|
| 私钥 | `.secrets/updater.key`（本机，权限 0600） | **否**（`.gitignore` 已忽略 `.secrets/`） |
| 公钥 | `.secrets/updater.key.pub` | 否（公钥正文已写进 `src-tauri/tauri.conf.json`） |
| 公钥（运行时用） | `src-tauri/tauri.conf.json` → `plugins.updater.pubkey` | 是 |

验证忽略规则：

```bash
git check-ignore -v .secrets/updater.key
# .gitignore:29:.secrets/	.secrets/updater.key
```

## 2. 生成 / 重新生成密钥对

仓库内已带 Tauri CLI（`node_modules/.bin/tauri`，2.12.x），不需要全局安装：

```bash
cd /home/ljj/rime-sync
mkdir -p .secrets && chmod 700 .secrets
./node_modules/.bin/tauri signer generate -w .secrets/updater.key
# 交互式输入口令（推荐）；加 --ci 则跳过提示（无口令，仅供本地验证）
```

生成后：

1. `cat .secrets/updater.key.pub` 的**整行**内容（base64，约 152 字符）粘到
   `src-tauri/tauri.conf.json` 的 `plugins.updater.pubkey`。
2. 用下面的检查确认配置仍是合法 JSON 且不再是占位符：

```bash
python3 -c "import json;d=json.load(open('src-tauri/tauri.conf.json'));p=d['plugins']['updater']['pubkey'];print(len(p), p[:20], 'REPLACE' in p)"
```

> 公钥可以公开（它就在仓库里）；**私钥一旦泄露，任何人都能签出你的更新包**。

## 3. 配到 GitHub Secrets

仓库 Settings → Secrets and variables → Actions → New repository secret：

| Secret | 值 | 说明 |
|---|---|---|
| `TAURI_SIGNING_PRIVATE_KEY` | `.secrets/updater.key` 的**文件全文**（含 `untrusted comment:` 那一行之后的 base64 内容，一字不改） | 必填 |
| `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` | 生成密钥时输入的口令 | 无口令密钥可留空字符串 |

命令行写入（不要留在 shell history 里，注意先 `history -d` 或用编辑器粘贴）：

```bash
gh secret set TAURI_SIGNING_PRIVATE_KEY < .secrets/updater.key
gh secret set TAURI_SIGNING_PRIVATE_KEY_PASSWORD   # 交互式粘贴
```

工作流 `.github/workflows/release.yml` 已在 `pnpm tauri build` 步骤注入这两个变量；
`createUpdaterArtifacts = true` 时缺私钥会直接构建失败（不是静默产出未签名包）。

## 4. 轮换步骤

1. 生成新密钥对（§2），**先不要**覆盖旧文件：`tauri signer generate -w .secrets/updater-2026.key`。
2. 把新公钥写进 `tauri.conf.json`，发布一个**常规版本**（例如 0.3.1）。
   - 这一步很关键：旧版本客户端只认旧公钥，所以新公钥必须先随一个旧私钥签名的版本发出去。
3. 确认新版本能被旧版本升级到之后，再更新 GitHub Secrets 为新私钥。
4. 删除本地旧私钥（`shred -u`），并在发布说明里记录轮换日期。
5. 旧版本客户端若已停止维护，才可以在下一次发布时同时替换公钥与 Secrets。

## 5. 本地验证

```bash
# 用私钥给任意产物签名，确认密钥可用
./node_modules/.bin/tauri signer sign -f .secrets/updater.key -p '' dist/资产名.tar.gz
ls dist/资产名.tar.gz.sig

# 只想看公钥指纹（不打印私钥）
head -c 20 .secrets/updater.key.pub; echo
```

安装包的 `.sig` 由 `tauri build` 自动生成，与公钥同目录；手工验证时可以拿
`plugins.updater.pubkey` 与 `.pub` 文件比对（要完全一致）。

## 6. 已知未闭环项

- `plugins.updater.endpoints` 仍是占位符 `https://REPLACE-ME.example.com/...`，
  仓库还没有 `latest.json` 的发布地址；填真实地址前自动更新不会真正工作（与签名无关）。
- 私钥当前为**无口令**生成（`--ci`），便于本地冒烟；正式发布前建议按 §2 重生成带口令的密钥，
  并同步更新 `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`。
