# Rime 方案数据（vendored）

这些文件被打包进 `rime-inputmethod` deb，安装到 `/usr/share/rime-data`，使输入法在安装后无需联网即可 deploy 并输入中文。

| 目录 | 上游 | 提交 | 许可证 |
| --- | --- | --- | --- |
| rime-prelude | https://github.com/rime/rime-prelude | 082425ea0684bca36474415d4a0e8db9b016487e | LGPL-3.0 |
| rime-essay | https://github.com/rime/rime-essay | 054920de4f54c9e5994276a96a4fc2a35cb51aa3 | LGPL-3.0 |
| rime-luna-pinyin | https://github.com/rime/rime-luna-pinyin | 56b934b099dfbeab842320f13aa8b461a6ab3e42 | LGPL-3.0 |

打包时取三个仓库顶层的 `*.yaml` 与 `essay.txt` 平铺到 `/usr/share/rime-data`，保留各自的 `LICENSE`（打包到 `/usr/share/doc/rime-inputmethod/`）。

默认方案为 luna_pinyin（拼音）。上游 rime-prelude 的 `default.yaml` 还列出了 bopomofo/cangjie5/quick5/stroke/terra_pinyin；librime 的 deployer 把每个缺失 schema 记为一次 failure 并让整次部署失败，因此这里的 `default.yaml` 只保留随包的 luna_pinyin。需要其他方案时，把对应仓库一并 vendor 到这里并补回 `schema_list`。
