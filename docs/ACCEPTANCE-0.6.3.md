# v0.6.3 验收清单（按 key 共享配置）

发布：2026-10-25，流水线 40164359441（10/10 job 绿，`verify-artifacts` 16 项断言 0 失败，16 个资产）

## 这一版新增

| 功能 | 默认行为 | 在哪里看 |
|---|---|---|
| 设备本地键 | `schema_list` 只存本机，上传前剥离、拉取不覆盖 | 设置 →「设备本地键」卡片 |
| 共享键 | `menu/page_size` 等其余键照常全设备同步 | 同上 |
| 双语键名 | `patch.menu/page_size` → 「每页候选数（Candidates per page）」 | 冲突页、同步结果、方案说明 |

## 配置作用域验收（两端各一台）

- [ ] A 机设置 →「设备本地键」里默认有 `schema_list`（若为空说明配置被改过）
- [ ] A 机输入方案只勾「五笔86」，B 机只勾「朙月拼音」
- [ ] 两台各改 `menu/page_size` 为相同值（例如 8），同步后双方都是 8
- [ ] 同步后 A 机仍是五笔、B 机仍是拼音（互不覆盖）
- [ ] 远端 `shared/files/default.custom.yaml` 里**没有** `schema_list`
- [ ] 再跑一次同步：`pulled=0 pushed=0`（收敛，不反复上传）
- [ ] 把 `schema_list` 从「设备本地键」移除后再同步：两边方案列表变为并集（回到旧行为）
- [ ] 把 `menu/page_size` 加进「设备本地键」，改值后同步：两边各自保留自己的值

## 界面文案

- [ ] 冲突页键名显示为「中文（English）」，鼠标悬停能看到原始 `patch.*` 键
- [ ] 同步结果里的冲突行同样是双语，不再是裸英文
- [ ] 方案管理说明里 `schema_list` 显示为「schema_list（输入方案列表 / Schema list）」
- [ ] 中英切换后上述文案随之切换

## 回归项

- [ ] Windows：托盘「Rime 同步…」、设置窗口按钮仍可用
- [ ] Windows：方案列表含五笔86，`znihao` 反查得到「你好」
- [ ] Linux：fcitx5 配置页三个按钮可点，「立即同步」产生 history 记录
- [ ] 备份区保留份数/删除/清理仍可用
- [ ] 「预览同步」列出的动作与随后执行一致

## 已知限制（不是 bug）

- 写回与上传的 `*.custom.yaml` 会经 YAML 重新序列化，**注释与原始排版可能丢失**；
- 与 0.6.2 及更早客户端混用时会来回覆盖远端文件，直到两端都升级；
- 更深的嵌套键（如 `ascii_composer/switch_key/Caps_Lock`）按原样显示英文。

## 排障

```bash
tail -5 ~/.config/com.rimesync.app/history.jsonl     # 最近同步结果
rime-sync-cli status                                 # 设备/远端状态
cat ~/.config/com.rimesync.app/config.json | grep -A2 device_local_keys
```

如果误把某键设为设备本地：设置页移除即可；配置在 `config.json` 的 `device_local_keys` 字段。
