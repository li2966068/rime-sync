# 输入增强：v 数值转中文 + 中文标点直出（计划随 0.6.8 发布）

随包方案（16 个）多了两个输入增强，由 librime 插件提供、由本仓库生成的
`<schema>.custom.yaml` 打开：

* v 数值转中文：输入 v123、v12.5，候选里给出中文数字（大写/小写由插件实现决定）；
  组件是 translators 首位的 rimesync_vnum。
* 中文标点直出：反斜杠、/、<、>、[、]、{、}、竖线、%、$、*、~ 不按 Shift 直接上屏主字符
  （见 §1），按 Shift + 该键 弹备选；组件是 processors 里 punctuator 之前的 rimesync_punct。

两个插件（组件名 `rimesync_vnum` / `rimesync_punct`）通过 `BUILD_MERGED_PLUGINS`
编进 librime，**不需要单独放 .so**。用户侧只需要配置文件。

## 1. 标点主字符表

主字符（不按 Shift 直接上屏；装了插件时 Shift 弹备选）：

* 反斜杠 → 、    / → 、    < → 《    > → 》
* [ → 「    ] → 」    { → 『    } → 』
* 竖线 → ·    % → ％    $ → ￥    * → ×    ~ → ～

逗号、句号、感叹号、问号、分号、冒号本来就是 {commit: …}，不需要改。

例外：注音方案（bopomofo / bopomofo_tw / bopomofo_express）用 < > 打 ，。，~ 是它自定的
ASCII 键，这三处保持原样（改了会破坏注音自己的键盘布局）。

## 2. 生成的文件

```
packaging/rime/schemas/<schema>.custom.yaml   # 16 个，随包方案各一份
```

`<schema>` 的 16 个：`luna_pinyin`、`luna_pinyin_simp`、`luna_pinyin_fluency`、
`luna_pinyin_tw`、`luna_quanpin`、`wubi86`、`wubi_pinyin`、`wubi_trad`、
`cangjie5`、`cangjie5_express`、`stroke`、`terra_pinyin`、`terra_pinyin_12345`、
`bopomofo`、`bopomofo_tw`、`bopomofo_express`（= 随包 `default.yaml` 的 schema_list）。

内容结构：

```yaml
patch:
  engine/processors:        # 原顺序 + 在 key_binder/punctuator/selector 之前插入 rimesync_punct
  engine/translators:       # 原顺序 + 首位 rimesync_vnum
  recognizer/patterns/vnum: '^[vV][0-9.]*$'
  punctuator:               # 整段内联（见 §5），half_shape 主字符改成 {commit: X}
```

为什么这么排：

* `rimesync_punct` 排在 `key_binder` / `punctuator` / `selector` 里**最先出现**的那个之前——只挡在 `punctuator` 前面的话，数字选词会先被 `selector` 吃掉；
* `v1234` 需要 `recognizer/patterns/vnum`：朙月拼音的拼写代数会把 `v` 当无效音节切开，translator 就收不到完整片段；模式 `^[vV][0-9.]*$` 同时允许中间态（先打 `v`、再补 `12.`）。

引擎列表是从 vendored 方案（`third_party/rime-data/**/<schema>.schema.yaml`）里读出来的
真实顺序，包括 `__include:` 继承方案（`luna_pinyin_simp` 等）——生成器
`scripts/ime/gen-schema-custom.py` 会自己展开 `__include` 链，不写死。

## 3. 安装 / 升级行为

模板装到 `/usr/share/rime-sync/defaults/schemas/`（随 `rime-inputmethod` deb），
安装器按下面的规则复制到用户 Rime 目录
（Linux `~/.local/share/fcitx5/rime`，Windows `%APPDATA%\Rime`）：

* **首次安装**（用户目录还没有 `default.custom.yaml`）：写入 `default.custom.yaml` 与 16 个
  `<schema>.custom.yaml`；
* **升级**：默认**不写**，避免默默改变老用户行为；已有同名文件永远跳过，绝不覆盖；
* 想主动打开（升级场景）：安装器参数 `--with-schema-tweaks`，或
  `sudo RIMESYNC_WITH_SCHEMA_TWEAKS=1 dpkg --configure rime-sync`，或手工

  ```bash
  cp /usr/share/rime-sync/defaults/schemas/*.custom.yaml ~/.local/share/fcitx5/rime/
  fcitx5-remote -r
  ```

写入了哪些文件会打印在安装输出里，同时记到 `/var/log/rime-sync/schema-tweaks.log`
（每次安装一行 `wrote=/kept=/no_data=` 汇总）。

Windows：NSIS 钩子（`packaging/nsis-hooks.nsh`）已经会从
`$INSTDIR\resources\schemas\*.custom.yaml` 复制到 `%APPDATA%\Rime\`（存在即跳过，
并且要求 Weasel 的 data 里有 `wubi86`，否则跳过以免部署失败）。**当前 Windows 安装包还没有
这批模板**：`src-tauri/tauri.conf.json` 的 `resources` 还没映射
`../packaging/rime/schemas`，补上映射后这段钩子自动生效。

## 4. 怎么关闭

| 想关掉 | 做法 |
| --- | --- |
| 某个方案的全部增强 | 删掉用户目录里的 `<schema>.custom.yaml`，再 `fcitx5-remote -r` |
| 只关标点直出、保留 v 数值 | 在对应 `*.custom.yaml` 里删掉 `punctuator:` 整段（保留 `engine/*`），重新部署 |
| 只关 v 数值 | 删掉 `engine/translators` 段里的 `rimesync_vnum` 一行 |
| 关掉插件本身 | 需要重新编 librime（去掉 `BUILD_MERGED_PLUGINS` 里的插件），不属于用户配置 |

删文件后重新部署即可，不需要卸载。

## 5. 为什么 punctuator 是整段内联（不是只写 half_shape）

方案里写的是：

```yaml
punctuator:
  import_preset: symbols      # 或 default
```

librime 在 link 阶段把 preset 整个 include 进来，再把方案自己的 punctuator 当 overrides
逐键合并（`legacy_preset_config_plugin.cc` + `IncludeReference::Resolve`）。合并要求节点类型
一致：preset 里 `<` 是 list（候选菜单），而我们要写 `{commit: 《}`（map）。实测（librime 1.17.0
+ `rime_deployer --build`）会直接失败：

```
copy on write failed; incompatible node type: commit
error merging branch commit
error merging branch <
error merging branch half_shape
failed to merge tree: symbols:punctuator
failed to include section symbols:punctuator
error building config: luna_pinyin.schema
```

所以生成器把**生效的 punctuator 解析成完整映射后内联**，并去掉 `import_preset`。
代价是文件大一些（luna_pinyin 约 26 KB，其余 1.4–1.6 KB），好处是自足：
不依赖 data 目录里的额外 preset，`custom.yaml` 跨设备同步也不会缺依赖；
`punctuator/symbols`（`/fh` 这类符号输入）也原样保留。

同理，`default.custom.yaml` 里**不能**写 `punctuator/half_shape`：有 6 个随包方案是
`import_preset: default`，实测那样写会整体顶掉 preset 的键（编译后的 `half_shape` 只剩
写进去的键，`,` `?` 直接不响应）。模板里只在 `rimesync/punctuator_primary` 下留一份
参考表（不生效），真正生效的副本在每个方案的 custom 文件里。

## 6. 自检

```bash
python3 scripts/ime/gen-schema-custom.py --check          # 产物是否与方案一致（缺失/过期/多余）
python3 scripts/ime/test_gen_schema_custom.py             # 静态检查（引擎顺序、标点取值、可复现）
python3 scripts/ime/test_gen_schema_custom.py --deploy    # 再用真实 librime 部署一遍并核对生效配置
```

`--deploy` 会：把 vendored rime-data 拷到临时目录 → 用生成的 custom 文件跑
`rime_deployer --build` → 断言日志里没有 `failed to merge` / `error building config`，
并解析 deployer 写出的 `<schema>.schema.yaml`（编译后的生效配置）核对
`translators[0]`、`rimesync_punct` 的位置、`punctuator/half_shape` 的取值、
`full_shape` 与 `symbols` 未改动。

## 7. 还没验证的部分

需要在**真机**（fcitx5 + 启用插件的 librime，Windows 用 Weasel）上确认：

1. `v123` / `v12.5` 的候选内容与上屏结果（插件实现侧，本仓库只负责打开组件）；
2. `rimesync_punct` 的 Shift 备选交互（哪些键有备选、备选顺序）；
3. 插件接管 `/` 之后 `/fh` 这类符号输入是否还可用（processor 排在 punctuator 之前，
   直出 `、` 后可能再拼不出 `/fh`）；
4. Windows 侧还没有随包模板（见 §3），需要在加上 `tauri.conf.json` resources 映射后验证。
- `/` 有意不直出：它同时是符号前缀（`/fh` 等），插件与随包配置都保留原来的候选菜单；需要直出时把 gen-schema-custom.py 的 PUNCT_PRIMARY 加回并重新生成。

- v 模式选词：键盘上的**数字键不能用**（会被 recognizer 并进输入），改用 a–h 选第 1–8 个候选（空格/回车同样可用）。键位可用 `rimesync_vselect/keys` 覆盖。
