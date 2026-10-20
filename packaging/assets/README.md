# packaging/assets — 占位资源说明

此目录**故意只放文档**，不放二进制。原因：图标属于品牌资源，需要人工确认来源与许可，
不能让 CI/脚本凭空生成后入库。

## 需要补齐的文件

| 文件 | 用途 | 要求 |
|---|---|---|
| `source-icon.png` | `tauri icon` 的源图 | 1024×1024，透明背景，居中方图 |
| `icon.ico` | Windows exe/安装器 | 由 `pnpm tauri icon` 自动生成到 `src-tauri/icons/` |
| `icon.icns` | macOS（当前不发布，可略） | 同上 |
| `nsis-header.bmp` / `nsis-sidebar.bmp` | NSIS 安装器美化（可选） | 150×57 / 164×314，24bpp BMP |
| `LICENSE-fonts.txt` | 若打包内置字体 | 必须附授权文本 |

## 生成步骤

```bash
# 1) 放入 1024x1024 源图
cp <你的图标>.png packaging/assets/source-icon.png
# 2) 生成全套图标（写入 src-tauri/icons/）
pnpm tauri icon packaging/assets/source-icon.png
# 3) 确认 tauri.conf.json 的 bundle.icon 指向生成结果
```

## 约束

- 不要提交来源不明的图标（版权风险）。
- 不要把 `*.userdb`、`installation.yaml`、任何真实用户词库放进打包资源。
- 安装器资源只在 Windows 上生效；Linux 用 `.desktop` + PNG，由 bundler 自动生成。
