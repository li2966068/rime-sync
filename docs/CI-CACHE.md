# CI 构建缓存（librime / Weasel）

release.yml 的 Windows job 从 0.6.9 起要自建 librime 1.17.0（合并 rimesync-extra 插件）+ boost 1.84
+ Weasel 全量 msbuild，v0.6.19 实测 **45m57s**。这里用 actions/cache 缓存**产物目录**，命中后跳过
这三段编译，目标 15–20 分钟。

硬约束只有一条：**缓存只允许影响耗时，不允许影响产物**。所以每一次命中都要过校验脚本，任何不符
就删掉缓存产物、照常全量构建。

---

## 1. 缓存了什么

只缓存产物目录，**不缓存 build 中间目录**（MSVC 增量目录、msbuild 中间产物都不进缓存）。

### Windows（release.yml · windows job）

| 缓存路径 | 内容 |
|---|---|
| third_party/librime/dist | librime 安装前缀：x64 / Win32 的 rime.dll、rime.lib、include/rime_api.h、LIBRIME-BUILD.json |
| third_party/weasel/output | Weasel msbuild 产物：WeaselServer.exe、WeaselDeployer.exe、WeaselSetup.exe、rime.dll、archives/weasel-0.17.4.0-installer.exe、data/ |
| third_party/weasel/{lib,lib64,include} | build-librime-win.ps1 部署进 Weasel 树的 rime.lib / rime_api.h |
| third_party/weasel/deps/boost_1_84_0/{boost,stage} | boost 头文件与 stage/lib（自建 librime 的前提） |
| packaging/windows/vendor/weasel-0.17.4.0-installer.exe | NSIS 用 !define RIMESYNC_WEASEL_EXE 内嵌的那一份 |
| build/cache-manifest.json | 清单：源码树 id / 插件哈希 / 脚本哈希 / 每个产物的 sha256 |

**不进缓存**：third_party/librime/{build,deps}、third_party/weasel/msbuild、整个 build/
（build/cache-manifest.json 是单文件例外）。

key 有两个（checkout 之后算一次并写进环境变量，0.6.23 起分开）：

~~~text
# restore：内容指纹，不带 run_id —— 同样的输入在任意一次运行都能命中
RIME_CACHE_KEY_RESTORE = rime-${{ runner.os }}-${{ hashFiles('third_party/librime/**') }}-${{ hashFiles('scripts/ime/build-librime-win.ps1') }}-${{ hashFiles('scripts/ime/build-weasel.ps1') }}
# save：指纹 + run_id —— 永远唯一，绝不与已有 key 冲突
RIME_CACHE_KEY_SAVE    = $RIME_CACHE_KEY_RESTORE-${{ github.run_id }}
~~~

restore-keys：rime-${{ runner.os }}-

> 指纹只算一次是有原因的：构建会往 third_party/librime 里写 dist\、deps\，save 时重算 hashFiles
> 会得到另一个字符串，缓存就永远存不到该存的地方。
>
> **save 的 key 带 run_id 是 0.6.23 的修复**：restore 用指纹命中、校验判 invalid 之后，这次
> 重建出来的产物必须写得回缓存；若 save 也用指纹，key 已存在就是 `unable to reserve cache`，
> 坏缓存永远盖不掉。两个 key 不同不影响命中：save 的 key 以 restore-key 前缀开头，
> 下一轮 restore-keys 一定捞得到最近一次保存的产物。
>
> `RIME_CACHE_KEY` 仍然保留为内容指纹（`verify-win-build-cache.ps1 -Mode write` 会把它写进
> 清单），restore/save 两个变量是从它派生出来的。

### Linux ime（release.yml · ime job）

容器里只编 librime（scripts/ime/container-build.sh 的 1/6 步），缓存的是 cmake --install 的安装
前缀 + 清单：

| 缓存路径 | 内容 |
|---|---|
| build/ime-cache/<distro>/root | DESTDIR 安装树（librime.so.1.*、rime.pc、rime_api.h、数据） |
| build/ime-cache/<distro>/manifest.json | 清单：librime 版本 / 源码树哈希 / 插件哈希 / 构建脚本哈希 / 基础镜像 / libdir / ENABLE_LOGGING / .so 哈希 / 安装树哈希 |

key（同样分两个，规则与 Windows 侧一致）：

~~~text
# restore：内容指纹
RIME_CACHE_KEY_RESTORE = rime-${{ runner.os }}-${{ matrix.distro }}-${{ hashFiles('third_party/librime/**') }}-${{ hashFiles('scripts/ime/build-linux.sh', 'scripts/ime/container-build.sh', 'scripts/ime/librime-build-cache.sh', 'scripts/ime/Dockerfile') }}
# save：指纹 + run_id
RIME_CACHE_KEY_SAVE    = $RIME_CACHE_KEY_RESTORE-${{ github.run_id }}
~~~

restore-keys：rime-${{ runner.os }}-${{ matrix.distro }}-

> 前缀以 `-` 结尾，所以 `<指纹>` 与 `<指纹>-<run_id>` 两种 key 都在覆盖范围内；
> 这与 0.6.23 之前的行为一致（那时存的就是指纹本身）。

> 这里不把两个 .ps1 的哈希算进去：容器构建根本不读它们，算进去只会让「改 Windows 脚本」无谓地
> 作废 Linux 缓存。系列前缀一致，gh cache delete 可以一起清。

---

## 2. 命中校验

### Windows：scripts/ime/verify-win-build-cache.ps1

~~~text
pwsh -NoProfile -File scripts/ime/verify-win-build-cache.ps1 -Mode verify
~~~

校验内容：

1. **清单**存在、JSON 可读、schema 匹配；
2. **输入指纹**与当前 checkout 一致：HEAD:third_party/librime、HEAD:third_party/weasel（git 树对象
   id）、plugins/rimesync-extra 的内容哈希、两个构建脚本与校验脚本的 sha256、librime 版本、boost
   版本、平台工具集；
3. **产物存在**且每个产物的 sha256 与清单一致（x64 + Win32 的 rime.dll / rime.lib / rime_api.h /
   WeaselServer.exe / WeaselDeployer.exe / 部署后的 lib64|lib/rime.lib）；
4. **二进制断言**：dist/x64/lib/rime.dll、dist/Win32/lib/rime.dll、output/rime.dll、
   output/Win32/rime.dll 里都必须有明文 rimesync_vnum 与 numpad（ISO-8859-1 逐字节搜索，和
   build-librime-win.ps1 同一判据）；
5. **安装包**：output/archives/weasel-*-installer.exe 唯一、>= 8 MiB、sha256 与清单一致；
   packaging/windows/vendor/ 里的副本缺失或哈希不符时从 archives/ 恢复。

结果只有两个出口：

- 全部通过 → 打 cache=hit，GITHUB_OUTPUT valid=true，跳过 bootstrap boost、build-librime-win.ps1、
  build-weasel.ps1（这两步仍然会「成功」，因为后面的 weasel_verify 用 outcome 判断 Weasel 是不是
  自编译的）；
- 任一不符 → 打 cache=invalid reason=<原因>（并逐条列出），删掉上面那些产物目录（git clean -fdx，
  只删未跟踪/被忽略的文件，third_party/weasel/output 里入库的 7z.exe 等脚手架会保留），
  GITHUB_OUTPUT valid=false，**退出码仍是 0**，job 继续全量构建。

reason 取值：

| reason | 含义 |
|---|---|
| no-manifest | 没有清单（首次运行或缓存未命中，属于正常情况） |
| manifest-unreadable / manifest-schema-N | 清单损坏或版本不认识 |
| inputs-changed:<字段> | 源码树 / 插件 / 构建脚本 / 版本 / 工具集变了 |
| artifact-missing:<路径> | 缓存缺产物 |
| artifact-changed:<路径> | 产物 sha256 与清单不符（残缺或被人动过） |
| plugin-missing:<路径>:<组件> | rime.dll 里搜不到 rimesync_vnum / numpad |
| installer-missing-in-output / installer-changed:<名称> / installer-too-small:<名称> | Weasel 安装包缺失/不符/过小 |
| verify-error | 校验脚本自己出意外（bug / IO 抖动）：按 invalid 处理，只重建、不卡发布 |

> **为什么用 git 树对象 id 做源码指纹**：构建会写 third_party/librime/env.bat、
> third_party/weasel/msbuild\、third_party/weasel/output\，用「目录内容哈希」会在构建前后得到不同
> 值，缓存就永远不命中。树对象 id 只看入库内容，与构建产物无关。

### Linux：scripts/ime/librime-build-cache.sh

容器内由 container-build.sh 调用（verify / write 两个子命令）：

1. 清单存在、schema 匹配；
2. librime 版本、源码树哈希、plugins/rimesync-extra 哈希、构建脚本 + Dockerfile 哈希、基础镜像、
   libdir、ENABLE_LOGGING 与清单一致；
3. librime.so.1.* 存在、sha256 一致、且带着 rimesync-extra 的模块符号
   （rime_require_module_rimesync_extra）与四个组件类符号（VnumTranslator / PunctProcessor /
   VselectProcessor / NumpadProcessor）；
4. 安装树整体哈希与清单一致。

> Linux 侧查的是**符号**不是字符串常量：GCC 会把 "rimesync_vnum" 这种短字面量直接编成立即数，
> 二进制里不留连续字节（实测 librime.so 里搜不到 rimesync_vnum，但符号齐全）。Windows 侧
> 仍然按明文常量断言（MSVC 把它们放在 .rdata，build-librime-win.ps1 一直在这么查）。

通过 → cache=hit（把安装前缀 cp 进 staging，跳过 configure/build）；不通过 → cache=invalid
reason=… 并删掉缓存目录重编。

> 这条路径**默认关闭**：CACHE_DIR 为空时 build-linux.sh 的行为和以前完全一致（本地不写缓存）。
> CI 里由 release.yml 传 CACHE_DIR=${{ github.workspace }}/build/ime-cache/<distro>。

---

## 3. 什么时候写缓存

只有「这次真的全量构建了 **且** 清单写入成功」才 save：

- Windows：record ime build cache manifest（-Mode write，失败即不存）→ actions/cache/save@v4，
  key = `RIME_CACHE_KEY_SAVE`（`<指纹>-<run_id>`）；save 之后紧跟一步打印
  `cache=saved key=<save key>`（没存则打 `cache=not-saved`）。
- Linux：容器里的 verify 判 hit 时 `scripts/ime/build-linux.sh` 输出 `cache_status=hit`，不 save；
  判 invalid（含首次未命中）时 `cache_status=invalid`，容器已删掉旧产物并重编 → save，
  同样用 `RIME_CACHE_KEY_SAVE`。

命中的那次（cache=hit 且校验通过）没有新产物可存，不重复 save；构建失败的那次也不 save，
所以**半成品永远进不了缓存**。save 的 key 永远唯一，同一份内容存两次也不会冲突。

---

## 4. 「缓存不影响正确性」的断言

无论这次是 cache=hit 还是全量构建，windows job 最后都会跑：

- setup.exe >= 12 MiB（内嵌的 Weasel 安装包还在）；
- setup.exe 里含 rime-sync-cli.exe 与 resources/prebuilt/**/*.schema.yaml；
- **新增**：解开 setup.exe，找到内嵌的 weasel-*-installer.exe，再解一层，里面的每个 rime.dll 都
  必须含 rimesync_vnum 与 numpad；上游回退包（mode=build 且自编译失败）本来就不带插件，那种情况
  由 weasel_verify 红字拦住发布，断言自动跳过；
- 另外 verify weasel vendor payload 之后那条断言也扩到了 4 个组件（rimesync_vnum /
  rimesync_punct / rimesync_vselect / numpad）。

---

## 5. 失效处理

自动：校验不过 → 删产物 → 全量构建 → **按新 key 存回**（日志里是 cache=invalid reason=…，
清单一并删除，构建完重新写清单，save 步打印 cache=saved key=<save key>）。坏缓存因此会被更新的
key 取代，下一轮 restore-keys 前缀会直接命中它。

手动（换了 runner 镜像、要强制重建、或怀疑缓存被污染时）：

~~~bash
gh cache list  -R li2966068/rime-sync            # 看 key / 大小 / id
gh cache delete <key> -R li2966068/rime-sync     # 删一个（<key> 就是日志里打印的整串）
gh cache delete --all -R li2966068/rime-sync     # 全删（会连带 rust / pnpm 缓存，慎用）
~~~

**不再需要**为了「exact key 命中但判 invalid」去手动 `gh cache delete`：那种情况现在会在同一次
运行里以 `<指纹>-<run_id>` 存回，下一轮直接命中新产物。

已知边界（都是刻意选的）：

1. **旧 key 会按 restore-keys 自然过期**：每次 invalid 重建都写一份新 key，旧条目不会再被 exact
   匹配到，只会排在 restore-keys 前缀里，最终被仓库级上限（10 GB，最近最少使用）淘汰。
   不需要人工清理；想立刻省空间可以 `gh cache delete <旧 key>`。
2. **restore-keys 命中旧 key**：同样要过清单，旧产物一定会被判 invalid 并删除，不会被带进新构建；
   代价只是多下载一份缓存。
3. **首次启用 / 改了 third_party/librime/**、scripts/ime/build-*.ps1、boost 版本**：必然全量构建
   一次（Windows 约 46–70 分钟），之后才是 15–20 分钟。
4. **容量**：仓库级缓存上限 10 GB，超了按最近最少使用淘汰；本方案 Windows 侧约 300–500 MB，
   Linux 侧约 5 MB（librime 安装树，实测 4.2 MB）。

---

## 6. 本地怎么验证

~~~bash
# Windows 侧校验脚本（不需要 Windows；判 invalid 时用 -KeepInvalid 保留现场）
pwsh -NoProfile -File scripts/ime/verify-win-build-cache.ps1 -Mode verify -KeepInvalid

# Linux 侧 helper（容器外可直接跑）
bash scripts/ime/librime-build-cache.sh verify <cache-dir> "$PWD" lib/x86_64-linux-gnu ON debian:13

# 带缓存的 Linux IME 构建（默认关闭，传 CACHE_DIR 才启用）
CACHE_DIR="$PWD/build/ime-cache/debian-13" bash scripts/ime/build-linux.sh

# 看 key 策略是否生效：save 出来的 key 一定以 -<run_id> 结尾，restore 命中的是内容指纹
gh cache list -R li2966068/rime-sync | grep 'rime-'
~~~

workflow 语法用 actionlint 校验：

~~~bash
actionlint -shellcheck= .github/workflows/release.yml
~~~

---

## 7. 预期耗时

| 场景 | Windows job | ime job |
|---|---|---|
| 冷缓存 / 缓存作废 | 与 0.6.19 持平或略高（46–70 分钟，一次性） | 与现在持平 |
| 命中（cache=hit） | 15–20 分钟（不再编 boost / librime / Weasel） | 少 3–5 分钟（实测 4 核：librime 199 s → 0 s） |

实际时间以 Actions 日志为准；cache=hit / cache=invalid reason=… 写在 verify ime build cache
那一步，cache=saved key=<save key> / cache=not-saved 写在 save 之后那一步，summary 里也有一行。
