# rime-sync 用户手册

面向最终用户。目标：把 Windows（小狼毫 Weasel）和 Linux（fcitx5-rime / ibus-rime）的
**用户词库、自定义配置、词典源**通过**你自己的 WebDAV** 双向同步。

- 不做账号、不做服务端；你的密码只存在本机。
- **不**同步 `*.userdb` 目录本体（LevelDB 二进制）——同步的是它的文本快照 `*.userdb.txt`，由 Rime 自己合并。
- **不**同步 `installation.yaml`（每台设备私有）。

---

## 1. 安装

### Windows 10/11
1. 下载 `rime-sync_<版本>_x64-setup.exe`，双击。
2. 若出现"Windows 已保护你的电脑"：点"更多信息 → 仍要运行"（安装包未做代码签名）。
3. 安装过程如需联网下载 WebView2，请保持网络畅通（安装包已内嵌 WebView2 引导器，无需预装）。
4. 从开始菜单启动「Rime 同步」（若装了 Weasel，程序组里也会有一份快捷方式）。
5. 不想安装时用便携版 `rime-sync_<版本>_x64-portable.zip`：解压即用，不写注册表。

### Linux
deb 系（Ubuntu/Debian，推荐）：

```bash
sudo apt install ./rime-sync_<版本>_amd64.deb
# 依赖缺失时：
sudo apt --fix-broken install
```

通用 AppImage：

```bash
chmod +x rime-sync_<版本>_amd64.AppImage
./rime-sync_<版本>_amd64.AppImage
# 若提示 FUSE 相关错误：
sudo apt install libfuse2        # 或
./rime-sync_<版本>_amd64.AppImage --appimage-extract-and-run
```

> 前置要求：系统里已经装好 Rime（Windows 装小狼毫；Linux 装 `fcitx5-rime` 或 `ibus-rime`），
> 否则没有可同步的目录。

---

## 2. 首次配置（5 步）

1. **确认 Rime 用户目录**。应用会自动探测，失败时手动选：

   | 平台 | 默认路径 |
   |---|---|
   | Windows | `%APPDATA%\Rime`（若注册表 `HKCU\Software\Rime\Weasel\RimeUserDir` 有值，以它为准） |
   | Linux + fcitx5 | `~/.local/share/fcitx5/rime` |
   | Linux + ibus | `~/.config/ibus/rime` |

   判断方法：目录里应有 `installation.yaml`、`*.custom.yaml`、`build/`。

2. **设备名（device_id）**：首次启动自动生成 `<主机名>-<6位随机>`，无需手填。
   **不要**在两台机器上填同一个值，否则远端 `sync/<device_id>/` 会互相覆盖。
3. **填 WebDAV**（下一节）。
4. 点 **测试连接**：成功会显示延迟与服务器类型（207/200 都算成功）。
5. 先点 **传输（transport）**，看到远端出现 `shared/` 和 `sync/<你的设备>/` 即打通。

---

## 3. 填 WebDAV

### 3.1 需要填的四个字段

| 字段 | 说明 | 示例 |
|---|---|---|
| URL | WebDAV 根地址，**通常以 `/` 结尾** | `https://dav.jianguoyun.com/dav/` |
| 用户名 | 服务商账号（多为邮箱） | `me@example.com` |
| 密码 | **应用专用密码**，不是登录密码 | `abcd-efgh-ijkl` |
| 远端目录 | 应用自建的工作目录，别填已有目录 | `rime-sync` |

程序会在远端目录下创建：

```text
<远端目录>/
├─ sync/<device_id>/owner.json
├─ sync/<device_id>/<词典>.userdb.txt
└─ shared/manifest.json + shared/files/...
```

### 3.2 常见服务商的 URL 形态

| 服务 | URL | 用户名 | 密码 |
|---|---|---|---|
| 坚果云 | `https://dav.jianguoyun.com/dav/` | 注册邮箱 | **应用密码**（网页版 → 账户信息 → 安全选项 → 添加应用密码） |
| Nextcloud | `https://<你的域名>/remote.php/dav/files/<用户名>/` | 用户名 | **应用密码**（设置 → 安全 → 创建应用密码） |
| 群晖 Synology | `https://<NAS地址>:5006/`（HTTP 为 `:5005`） | DSM 账号 | 建议专用账号；若启用 2FA 必须用应用密码 |
| 自建（rclone/nginx） | `http://<主机>:<端口>/<路径>/` | 配置里的账号 | 配置里的密码 |

> 以上 URL 形态是各服务商的通用约定，**本仓库未逐家实测**。用下面的命令自检，返回 `207`
> （或 `200`）就说明 URL/账号/密码三者都对：

```bash
curl -s -o /dev/null -w '%{http_code}\n' -X PROPFIND \
  -H 'Depth: 0' -u '用户名:应用密码' \
  'https://dav.jianguoyun.com/dav/'
# 207 = 正常；401 = 账号或应用密码错；404 = 路径不对（少了 /dav/ 或 remote.php/dav/files/<user>/）
```

### 3.3 关于密码的三条硬建议

1. **永远用应用专用密码**，不要用主账号密码：应用密码可单独吊销，泄露损失可控。
2. 密码优先存**系统钥匙串**（service = `rime-sync`，account = WebDAV 用户名）；没有 Secret Service
   的 headless 环境会回落到 0600 文件，并在界面标明当前后端。共享电脑上仍然不要保存密码。
3. 服务端若支持，给这个应用密码限定**只读写一个目录**（坚果云/Nextcloud 均可按目录授权）。

### 3.4 本地自测（不想动真服务时）

仓库自带 `scripts/dev-dav.sh`，用 rclone 起一个本地 DAV：

```bash
./scripts/dev-dav.sh /tmp/dav-root        # 默认 127.0.0.1:18080，账号 u / p
# 另一个终端：
curl -s -o /dev/null -w '%{http_code}\n' -X PROPFIND -u u:p http://127.0.0.1:18080/
# 应用里 URL 填 http://127.0.0.1:18080/  账号 u  密码 p
```

### 3.5 AList

AList 把每个存储挂载成一个路径，WebDAV 入口固定是 `/dav`：

| 字段 | 填法 | 示例 |
|---|---|---|
| URL | `http://<主机>:5244/dav` | `http://192.168.1.10:5244/dav` |
| 用户名 / 密码 | AList 账号（不是存储本身的账号） | `admin` / 你的密码 |
| 远端目录 | **挂载路径前缀 + 工作目录** | 挂载点叫 `aliyun` → `aliyun/rime-sync` |

AList 后台 → 用户 → 编辑，权限按下表勾选（这是 403 的头号原因）：

| 权限项 | 必须 | 用在哪里 |
|---|---|---|
| Webdav Read | ✅ | PROPFIND 列目录、GET 下载快照 |
| Webdav Manage | ✅ | MKCOL 建目录、PUT 上传、DELETE 清理 |
| 存储「写入」 | ✅ | 上传 `shared/files/*` 与 `owner.json` |
| 存储「删除」 | ✅ | 删除写权限探测对象、清理过期文件 |
| 存储「重命名/移动」 | 建议 | manifest 的原子替换 |
| 离线下载 / 索引 | ❌ | 与本工具无关 |

先验证再填进应用：

```bash
# 只读探测：207/200 说明地址与账号正确
curl -s -o /dev/null -w '%{http_code}\n' -X PROPFIND -H 'Depth: 0' \
  -u 'admin:密码' 'http://192.168.1.10:5244/dav/'

# 写探测（会建目录、写 1 字节再删；失败时看状态码）
curl -s -o /dev/null -w '%{http_code}\n' -X MKCOL \
  -u 'admin:密码' 'http://192.168.1.10:5244/dav/aliyun/rime-sync-probe'
```

应用内也有写权限探测（CLI：`rime-sync-cli probe`），失败时会把状态码翻译成可执行建议。

---

## 4. 两种模式：transport 与 apply

| | **transport（传输）** | **apply（应用/合并）** |
|---|---|---|
| 做什么 | 只搬文件：拉远端快照/shared，推本地改动 | 拉取 → **调用 Rime 真正合并 userdb** → 部署 → 推送 |
| 触发 | 定时（默认 10 分钟）+ 手动 | **仅手动**（点按钮） |
| 打断输入 | 否 | **是**（Linux 会临时停 fcitx5，几秒不可输入） |
| 风险 | 低 | 中（期间输入法不可用；异常退出会尝试自动拉起） |
| 适合 | 日常挂着，多机保持文件一致 | 换机后第一次合并词库、想让新词真正生效 |

**推荐节奏**：平时只开 transport；换设备或想合并词库时点一次 apply。

apply 在两端分别做什么（了解即可）：

- Windows：`WeaselDeployer.exe /sync` → `WeaselDeployer.exe /deploy`（官方部署器，会把输入法切到维护模式再同步）。
- Linux：`fcitx5-remote -e` 停输入法 → `rime_dict_manager --sync` → `rime_deployer --build` → `fcitx5 -d` 重启。

> Linux 的 `rime_dict_manager --sync` **必须**在 fcitx5 停止后运行，否则底层的 LevelDB 被占用，
> 会报 `Resource temporarily unavailable` 并跳过该词典。所以不要关掉"apply 前停止 fcitx5"这个选项。

---

## 5. 冲突怎么处理

两台机器改了同一个文件的**同一处**时，程序不会替你猜：

1. 本地版本保留在原路径；
2. 远端版本另存为 `<文件名>.conflict-<设备名>-<时间戳>`（同目录）；
3. 界面在"本次结果"里列出所有冲突文件。

处理步骤：

```bash
# 1) 找冲突文件
ls **/*.conflict-*            # Windows PowerShell: Get-ChildItem -Recurse *conflict-*
# 2) 对比（任选）
diff -u default.custom.yaml default.custom.yaml.conflict-laptop-20261020T130000
code --diff default.custom.yaml default.custom.yaml.conflict-laptop-20261020T130000
# 3) 手工合并成一个，删掉 .conflict-* 文件
# 4) 再跑一次 transport（让远端也变成合并后的版本）
```

其他边界情况：

| 情况 | 行为 |
|---|---|
| 只有一边改 | 直接取改过的一边，不产生冲突文件 |
| 远端删除 + 本地没改 | 本地也删除 |
| 远端删除 + 本地改过 | 保留本地，界面告警（不会静默丢数据） |
| 两边都改但内容相同 | 视为无冲突 |

---

## 6. 故障排查

### 6.1 连接类

| 现象 | 可能原因 | 处理 |
|---|---|---|
| `401 Unauthorized` | 用了登录密码而非应用密码；账号写成昵称 | 重新生成应用密码；用户名填完整邮箱/账号 |
| `403 Forbidden` | 应用密码权限不含该目录；服务商禁用 WebDAV | 换到有权限的目录；确认服务商已开启 WebDAV |
| `404 Not Found` | URL 少了 `/dav/`、`/remote.php/dav/files/<user>/`；末尾斜杠缺失 | 用 §3.2 的 curl 自检 |
| `证书错误 / SSL handshake` | 自签证书、系统时间不对 | 换可信证书；校准时间；不要在生产上关闭校验 |
| 超时（30s） | 代理/防火墙、服务商限速 | 换网络；降低 transport 频率 |
| `远端被其他设备占用` | 另一台设备的 session 还握着 `owner.json` | 等心跳过期（几分钟）或在另一台正常退出；确认两台 device_id 不同 |

### 6.2 apply 类

| 现象 | 原因 | 处理 |
|---|---|---|
| Linux 报 `Resource temporarily unavailable` / `failed synchronizing 1/1` | fcitx5 还在跑，userdb 被 LevelDB 独占 | 勾选"apply 前停止 fcitx5"；或手动 `fcitx5-remote -e` 后重试 |
| apply 后输入法没起来 | `fcitx5 -d` 失败（无 DISPLAY/DBUS） | 在桌面会话里执行 `fcitx5 -d`；检查 `echo $DISPLAY` |
| apply 成功但新词不生效 | 只做了 transport，没做 apply；或没重新加载 | 点一次 apply；Linux 可 `fcitx5-remote -r` 重载配置 |
| Windows 提示"正在执行另一项部署任务" | 另一个 WeaselDeployer 在跑（单实例互斥） | 等几秒重试；关掉托盘里正在进行的部署 |
| Windows 找不到部署器 | 小狼毫未安装或装在非默认路径 | 重装小狼毫；或在应用里指定 `WeaselDeployer.exe` 路径 |
| Linux `rime_deployer` 不存在 | 没装 `librime-bin` | `sudo apt install librime-bin` |
| Windows `/sync` 返回 0 但快照没更新 | 参数被当成"打开设置"（会弹窗） | 关掉弹出的设置窗；查看日志确认实际执行的命令行 |

> 提示：Linux 上 `rime_dict_manager --sync` **即使失败也返回退出码 0**，所以本应用是按输出与
> 快照文件时间判断成功的。你看到"同步失败"就一定不是仅凭退出码得出的结论。

### 6.3 数据类

| 现象 | 处理 |
|---|---|
| 远端出现 `*.userdb/` 目录 | 不该发生（硬红线）。删掉远端对应目录并反馈 bug |
| `shared/` 下出现 `*.userdb.txt` | 不该发生。删掉；Rime 会把 `sync/` 下每个子目录当设备合并，放错位置会污染 |
| 误同步了 `installation.yaml` | 立刻删除远端该文件；它含设备私有 id，互相覆盖会导致快照目录错乱 |
| 磁盘满/权限拒绝 | 检查用户目录可写；`config.json` 需 0600 |

### 6.4 AList 权限排查

| 现象 | 原因 | 处理 |
|---|---|---|
| 测试连接就 403 | 账号没勾 **Webdav Read**；或存储本身只读 | AList 后台 → 用户 → 编辑，勾上 Webdav Read |
| 能列目录但一上传就 403/405 | 缺 **Webdav Manage** | 勾上 Webdav Manage，保存后重新登录 WebDAV |
| 报 404 / 目录不存在 | URL 少了 `/dav`；远端目录少了挂载前缀 | URL 用 `http://主机:5244/dav`；远端目录填 `<挂载点>/rime-sync` |
| 上传成功但文件是 0 字节 | 存储配额满或后端网盘限流 | 看 AList 日志；换存储或扩容 |
| 中文文件名乱码 | 后端存储编码问题（多见于 S3/WebDAV 后端） | 先在 AList 网页上传同名文件复现；与本工具无关 |
| 一会儿能写一会儿 403 | AList 重启后 WebDAV 会话失效 | 重新保存一次配置触发重新认证 |

### 6.5 收集日志

界面"日志"面板 → 复制；或直接看配置目录下的 `log` 文件
（Windows：`%APPDATA%\rime-sync\`，Linux：`~/.config/rime-sync/`）。
反馈问题时请附：操作系统、Rime 发行版（小狼毫/fcitx5-rime 版本）、`rime_dict_manager` 输出、
是否用了 apply。

---

## 7. 安全与隐私

- 只上传白名单文件（`*.custom.yaml`、`*.schema.yaml`、`*.dict.yaml`、词表 `*.txt`、`*.userdb.txt`），
  引擎产物（`build/`、`*.bin`）与 LevelDB 目录永不上传。
- 密码优先存系统钥匙串；回落时写 0600 文件。不上传、不写日志。
- 路径穿越防护：远端返回的路径若含 `..` 或绝对路径会被拒绝。

## 8. 卸载

- Windows：设置 → 应用 → rime-sync → 卸载。默认会连小狼毫一起卸载（仅当小狼毫是跟着
  rime-sync 装的；安装前已有的输入法不动）。想保留输入法：`uninstall.exe /KEEPWEASEL`
  或先设 `RIMESYNC_KEEP_WEASEL=1`。配置在 `%APPDATA%\rime-sync`，手动删。
- Linux deb：`sudo apt remove rime-sync`；AppImage 直接删文件。
- 卸载**不会**动你的 Rime 用户目录；远端数据需自行在 WebDAV 里删。
