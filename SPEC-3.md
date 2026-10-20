# SPEC-3 — OpenList 适配与网络策略修正

## 背景（真机实测发现）
对真实 OpenList（http://192.168.111.1:5244）实测结果：
- 认证成功、写权限正常（MKCOL/PUT/GET/DELETE 全通）
- **用户 `rime` 的 WebDAV 根 `/dav/` 就是挂载点 `/14NAS/rime`**；访问 `/dav/14NAS/rime/` 反而 404
- `test-conn` 对不存在的 remote_dir 返回 404 + dav_class=missing-dir，语义误导
- `probe` 在全新服务器上**必失败**（MKCOL 409，因为没先 ensure_base）
- 需要支持「远端目录留空 = 服务器根」

## 需求
### A. OpenList 适配
1. presets 增加 **OpenList**（AList 的开源分支），保留 AList 作为 legacy 别名；默认路径 `/dav`、默认端口 5244；识别依据：页面 `<title>OpenList` 或 `Server` 头。
2. **probe 先 ensure_base**：逐级 MKCOL remote_dir 后再建探测目录；失败才报错。探测结束必须删除所有残留。
3. **test-conn 语义修正**：远端目录不存在时返回 `ok=true` + `remote_dir_missing=true` + 中文提示「服务器连通，远端目录将在首次同步时自动创建」；只有连接/认证失败才 `ok=false`。
4. **remote_dir 允许为空**（= 服务器根）。配置校验、URL 拼接、ensure_base、manifest 路径全部正确处理空值；UI 占位提示「留空 = 服务器根」。
5. **URL 归一化**：输入 `192.168.111.1:5244` → 自动补 `http://` 与 `/dav`；已带 scheme 或路径则不重复追加。

### B. 网络策略（用户明确纠正）
> 有线网络必须能同步，不允许做成"仅 Wi-Fi"。

6. 规则：**只要存在默认路由且不是「计量/漫游」网络，就允许同步**。有线与无线完全同等。
7. `NetworkPolicy`：
   - `available()`：有默认路由即可用
   - `is_metered()`：尽力而为——Windows 用 NetworkInformation 的 ConnectionCost；Linux 读 NetworkManager D-Bus 的 Metered 属性；**探测不到一律视为非计量**（绝不因此阻止同步）
   - 计量网络下：定时 transport 跳过并说明原因；**手动同步仍允许**
8. **失败退避**：连续失败间隔 1→2→4→8→16→30 分钟（上限 30），成功即复位；状态里暴露 next_retry_at。

### C. UI
9. 连接页预设下拉加 OpenList；选中后提示需在后台开启 `Webdav Read` + `Webdav Manage` 及 上传/删除/重命名/移动 权限。
10. 展示网络状态（可用 / 计量 / 离线）与下次重试时间。

## 验收（必须用真实环境）
真实 OpenList：`http://192.168.111.1:5244/dav`，账号 `rime`，密码 `<REDACTED>`。
1. `test-conn`：remote_dir 填不存在的路径 → `ok=true` 且 `remote_dir_missing=true`；填根（空）→ `ok=true`
2. `probe`：在全新服务器上**成功**（会自动建再删），远端无残留
3. `transport`：remote_dir 为空时直接写服务器根；两轮幂等（第二轮 pulled=pushed=merged=0）
4. 测试完成后远端必须清理干净（回到只有 `/dav/`）
5. `cargo test --workspace` ≥ 86 passed；fmt/clippy 干净
6. 新增单测：URL 归一化 5 种输入、空 remote_dir 路径拼接、退避序列、metered 判定在"探测失败"时返回非计量

## 边界
可改：crates/**、crates/cli/**、src-tauri/src/**、src/**
禁止：.github/**、packaging/**、docs/（除 architecture.md）、tauri.conf.json、LICENSE、README、CHANGELOG
不要 commit/push；不要把密码写进任何文件或提交
