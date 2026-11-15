//! processors 列表的目标状态：**唯一真相源**。
//!
//! rime-sync 往 Rime 的 `engine/processors` 里塞三个自研处理器
//! （`rimesync_vselect` / `numpad` / `rimesync_punct`），而「它们该排在哪里」
//! 有三条互相独立的写入路径：
//!
//! 1. 共享方案补丁（`data_patch::Doc::ensure_processors`）—— 改 distro 的
//!    `schema.yaml`，按行改写，用户文件里的注释与格式原样保留；
//! 2. 用户目录降级补丁（`data_patch::processor_target`，共享目录不可写时的
//!    Linux 兜底）—— 写 `<schema>.custom.yaml`，`engine/processors` 是**整段替换**，
//!    所以要写全量列表；
//! 3. 用户已有 custom.yaml 的增量修复（`managed_custom::repair_processors`）——
//!    只补/挪缺的东西，用户摆好的顺序不动。
//!
//! 三条路径以前各写一份「插到哪、要不要补」的判断，久了必然漂移：某一处改了
//! 锚点，另一处还按老规矩插，补丁互相打架；验证（verify）也只能按其中一处的
//! 口径去查。这里把规则收成一份，三条路径都从这里取。
//!
//! 规则本身（为什么这么排）：
//!
//! * `rimesync_vselect` 紧跟 `recognizer` —— 它要在识别器切出片段之后才接手
//!   选词；`recognizer` 不在时才退到输入处理环节（`key_binder` / `speller`）
//!   之前，都不在就追加到末尾。
//! * `numpad` 紧跟 `rimesync_vselect` —— 它依赖 vselect 建好的上下文。
//! * `rimesync_punct` 必须排在 `punctuator` **之前** —— 它命中按键就直接
//!   CommitText 并返回 `kAccepted`，后面的 punctuator 就没机会把半角标点转成
//!   Rime 自带的那套（见 third_party/librime/plugins/rimesync-extra/punct_processor.cc）。
//!
//! 策略（`Policy`）表达的是**来源不同、允许做的事也不同**：
//!
//! * `Authoritative` —— 文件由 rime-sync 自己生成/写就的，列表顺序由我们说了算：
//!   强制重排 vselect/numpad，缺什么补什么，位置不对的 `rimesync_punct` 也挪正。
//! * `Conservative` —— 用户可能自己大改过：不动用户摆好的 `rimesync_vselect`
//!   （只保证 `numpad` 紧跟它且只有一份），`rimesync_punct` 缺了才补、已有的不挪。

/// rimesync 的 v 选择处理器（`rimesync_vselect`）。
pub const VSELECT_PROCESSOR: &str = "rimesync_vselect";
/// 中文标点直出处理器（`rimesync_punct`）。
///
/// 它必须排在 `punctuator` **之前**：它先处理按键、直接上屏中文标点，
/// 后面的 punctuator 就不会再把半角标点转成 Rime 自带的那一套。
pub const PUNCT_PROCESSOR: &str = "rimesync_punct";
/// 小键盘归一化处理器（`numpad`）。
pub const NUMPAD_PROCESSOR: &str = "numpad";

/// 识别器：vselect 的首选锚点。
const RECOGNIZER: &str = "recognizer";
/// 输入处理环节：没有识别器时，vselect 退到它们之前。
const INPUT_STAGES: [&str; 2] = ["key_binder", "speller"];
/// 标点处理器：punct 必须排在它前面。
const PUNCTUATOR: &str = "punctuator";

/// 修复策略：同一份 processors 列表，不同来源允许做的事不一样。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Policy {
    /// 权威：文件由 rime-sync 生成或写就的，顺序由我们说了算 ——
    /// 强制重排 `rimesync_vselect` / `numpad`，补齐缺失的 `rimesync_punct`，
    /// 位置不对的 `rimesync_punct` 也挪到 `punctuator` 前面。
    Authoritative,
    /// 保守：用户可能自己大改过 —— 保留用户摆好的 `rimesync_vselect`，
    /// 只保证 `numpad` 紧跟它且只有一份；`rimesync_punct` 缺了才补，已有的不挪。
    Conservative,
}

/// 插入锚点。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Anchor {
    /// 插到同名项**之后**。
    After(&'static str),
    /// 插到同名项**之前**。
    Before(&'static str),
    /// 追加到列表末尾。
    End,
}

/// 对 processors 列表做的一步编辑。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// 摘掉列表里所有同名项。
    Remove(&'static str),
    /// 插入一项到锚点处。
    Insert {
        /// 要插入的处理器名。
        item: &'static str,
        /// 插到哪。
        anchor: Anchor,
    },
}

/// 列表里是否已有某个名字。
pub fn contains<S: AsRef<str>>(base: &[S], name: &str) -> bool {
    base.iter().any(|item| item.as_ref() == name)
}

/// `rimesync_vselect` 在列表里的位置（取第一个）。
pub fn vselect_index<S: AsRef<str>>(names: &[S]) -> Option<usize> {
    names
        .iter()
        .position(|name| name.as_ref() == VSELECT_PROCESSOR)
}

/// `rimesync_punct` 在列表里的位置（取第一个）。
pub fn punct_index<S: AsRef<str>>(names: &[S]) -> Option<usize> {
    names
        .iter()
        .position(|name| name.as_ref() == PUNCT_PROCESSOR)
}

/// `punctuator` 在列表里的位置（取第一个）。
pub fn punctuator_index<S: AsRef<str>>(names: &[S]) -> Option<usize> {
    names.iter().position(|name| name.as_ref() == PUNCTUATOR)
}

/// vselect 的插入锚点：`recognizer` 之后优先；没有识别器就插到输入处理环节
/// （`key_binder` / `speller` 里**排在列表前面**的那个）之前；都没有就追加到末尾。
///
/// 之所以看「排在前面那个」而不是固定 `key_binder`：这两个是同一环节的两半
/// （按键绑定 → 拼写），vselect 要插在它们**之前**才能先于输入处理生效，
/// 而用户完全可能把两者的顺序换过 —— 那就以更靠前的那个为准。
pub fn vselect_anchor<S: AsRef<str>>(base: &[S]) -> Anchor {
    if contains(base, RECOGNIZER) {
        return Anchor::After(RECOGNIZER);
    }
    let mut earliest: Option<(usize, &'static str)> = None;
    for stage in INPUT_STAGES {
        let Some(index) = base.iter().position(|item| item.as_ref() == stage) else {
            continue;
        };
        if earliest.is_none_or(|(best, _)| index < best) {
            earliest = Some((index, stage));
        }
    }
    match earliest {
        Some((_, stage)) => Anchor::Before(stage),
        None => Anchor::End,
    }
}

/// punct 的插入锚点：必须在 `punctuator` 之前；列表里没有 punctuator 就追加到末尾。
pub fn punct_anchor<S: AsRef<str>>(base: &[S]) -> Anchor {
    if contains(base, PUNCTUATOR) {
        Anchor::Before(PUNCTUATOR)
    } else {
        Anchor::End
    }
}

/// `numpad` 是否已就位：vselect 紧跟一份 numpad，且列表里只有一份 numpad。
pub fn numpad_ok<S: AsRef<str>>(base: &[S]) -> bool {
    let Some(index) = vselect_index(base) else {
        return false;
    };
    let adjacent = base
        .get(index + 1)
        .map(|item| item.as_ref() == NUMPAD_PROCESSOR)
        .unwrap_or(false);
    adjacent
        && base
            .iter()
            .filter(|item| item.as_ref() == NUMPAD_PROCESSOR)
            .count()
            == 1
}

/// `rimesync_punct` 的位置是否不对（排在 `punctuator` 后面 = 拿不到按键）。
///
/// 缺 punctuator 就无从比较（没冲突）；缺 punct 由「补齐」负责，不算错位。
pub fn punct_misplaced<S: AsRef<str>>(base: &[S]) -> bool {
    let (Some(punct), Some(punctuator)) = (punct_index(base), punctuator_index(base)) else {
        return false;
    };
    punct > punctuator
}

/// 按策略算出对 processors 列表要做的编辑动作（名字级，与怎么落地无关）。
///
/// 路径 1 / 2 用它驱动文本级或值级编辑，路径 3 用它驱动行级编辑 ——
/// 规则只有这一份，三条路径不会各自漂移。
pub fn plan<S: AsRef<str>>(base: &[S], policy: Policy) -> Vec<Action> {
    match policy {
        Policy::Authoritative => authoritative_plan(base),
        Policy::Conservative => conservative_plan(base),
    }
}

/// 权威策略：顺序由我们说了算。
fn authoritative_plan<S: AsRef<str>>(base: &[S]) -> Vec<Action> {
    // 先摘掉再插回去：用户把顺序改乱了也能纠正回来。
    let mut actions = vec![
        Action::Remove(VSELECT_PROCESSOR),
        Action::Remove(NUMPAD_PROCESSOR),
    ];
    actions.push(Action::Insert {
        item: VSELECT_PROCESSOR,
        anchor: vselect_anchor(base),
    });
    actions.push(Action::Insert {
        item: NUMPAD_PROCESSOR,
        anchor: Anchor::After(VSELECT_PROCESSOR),
    });
    // punct：缺就补；位置不对（排在 punctuator 后面）也挪正 ——
    // 留着错位等于「中文标点直出没开」，还不如直接修好。
    if !contains(base, PUNCT_PROCESSOR) {
        actions.push(Action::Insert {
            item: PUNCT_PROCESSOR,
            anchor: punct_anchor(base),
        });
    } else if punct_misplaced(base) {
        actions.push(Action::Remove(PUNCT_PROCESSOR));
        actions.push(Action::Insert {
            item: PUNCT_PROCESSOR,
            anchor: punct_anchor(base),
        });
    }
    actions
}

/// 保守策略：只动「必须动」的，且前提是文件里确实有 rimesync 的东西。
fn conservative_plan<S: AsRef<str>>(base: &[S]) -> Vec<Action> {
    // 没有 rimesync_vselect ⇒ 这份文件被用户大改过（或根本不是 rime-sync 管的），
    // 一个字节都不动 —— 替用户加处理器比不修更危险。
    if !contains(base, VSELECT_PROCESSOR) {
        return Vec::new();
    }
    let mut actions = Vec::new();
    if !numpad_ok(base) {
        actions.push(Action::Remove(NUMPAD_PROCESSOR));
        actions.push(Action::Insert {
            item: NUMPAD_PROCESSOR,
            anchor: Anchor::After(VSELECT_PROCESSOR),
        });
    }
    // punct 缺了才补（用户自己摆的位置一律保留）。
    if !contains(base, PUNCT_PROCESSOR) {
        actions.push(Action::Insert {
            item: PUNCT_PROCESSOR,
            anchor: punct_anchor(base),
        });
    }
    actions
}

/// 按策略算出目标 processors 列表（名字级）。
///
/// 这是「目标状态」的字面定义：给定现状，按策略应变成什么样。
/// 路径 1 直接用它算全量列表；路径 2 / 3 用 `plan` 落到自己的编辑机制上
/// （它们要保留用户文件里的注释与格式，不能整段替换）。
pub fn target_names<S: AsRef<str>>(base: &[S], policy: Policy) -> Vec<String> {
    let mut out: Vec<String> = base.iter().map(|item| item.as_ref().to_string()).collect();
    for action in plan(base, policy) {
        match action {
            Action::Remove(name) => out.retain(|item| item != name),
            Action::Insert { item, anchor } => {
                let at = match anchor {
                    Anchor::After(anchor) => {
                        out.iter().position(|name| name == anchor).map(|i| i + 1)
                    }
                    Anchor::Before(anchor) => out.iter().position(|name| name == anchor),
                    Anchor::End => None,
                };
                match at {
                    Some(index) => out.insert(index, item.to_string()),
                    None => out.push(item.to_string()),
                }
            }
        }
    }
    out
}

/// 严格校验：列表是否达到 rime-sync 自己写就的目标状态。
///
/// 校验的是「结构对不对」，不管这份文件是谁写的：vselect 在不在、numpad 紧不紧
/// 跟它、vselect 是不是紧跟识别器、punct 在不在、punct 是不是排在 punctuator 前面。
///
/// 共享方案补丁（路径 1 / 2）写完必须过这一关 —— 它写的是整段替换的列表，
/// 顺序错了等于把用户的输入法改坏了。
pub fn validate<S: AsRef<str>>(names: &[S]) -> Result<(), String> {
    let vselect = vselect_index(names)
        .ok_or_else(|| format!("engine/processors 里没有 {VSELECT_PROCESSOR}"))?;
    if names
        .get(vselect + 1)
        .map(|name| name.as_ref() != NUMPAD_PROCESSOR)
        .unwrap_or(true)
    {
        return Err(format!(
            "engine/processors 里 {NUMPAD_PROCESSOR} 没有紧跟 {VSELECT_PROCESSOR}"
        ));
    }
    if let Some(recognizer) = names.iter().position(|name| name.as_ref() == RECOGNIZER) {
        if vselect != recognizer + 1 {
            return Err(format!("{VSELECT_PROCESSOR} 没有紧跟 recognizer"));
        }
    }
    if punct_index(names).is_none() {
        return Err(format!("engine/processors 里没有 {PUNCT_PROCESSOR}"));
    }
    if punct_misplaced(names) {
        return Err(format!(
            "engine/processors 里 {PUNCT_PROCESSOR} 必须排在 {PUNCTUATOR} 之前，否则拿不到按键"
        ));
    }
    Ok(())
}

/// 保守校验：只查「rimesync 的定制还在不在、还成不成立」。
///
/// 用户自己的 custom.yaml 里 `rimesync_vselect` 不必紧跟 `recognizer`
/// （用户可能自己插了别的处理器），所以这里不查那条；`rimesync_punct` 的**位置**
/// 也不查 —— 保守策略承诺「用户摆的位置一律保留」，既然不挪，就不能反过来
/// 因为它没排在 punctuator 之前而拒绝写入（那连顺带的 numpad 修复也做不成）。
/// 这里只查：vselect 在不在、numpad 紧不紧跟它、punct 在不在。
pub fn validate_conservative<S: AsRef<str>>(names: &[S]) -> Result<(), String> {
    let vselect = vselect_index(names)
        .ok_or_else(|| format!("engine/processors 里没有 {VSELECT_PROCESSOR}"))?;
    if names
        .get(vselect + 1)
        .map(|name| name.as_ref() != NUMPAD_PROCESSOR)
        .unwrap_or(true)
    {
        return Err(format!(
            "engine/processors 里 {NUMPAD_PROCESSOR} 没有紧跟 {VSELECT_PROCESSOR}"
        ));
    }
    if punct_index(names).is_none() {
        return Err(format!("engine/processors 里没有 {PUNCT_PROCESSOR}"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn list(names: &[&str]) -> Vec<String> {
        names.iter().map(|name| name.to_string()).collect()
    }

    #[test]
    fn authoritative_plan_reorders_vselect_and_numpad() {
        let base = list(&[
            "ascii_composer",
            "recognizer",
            "key_binder",
            "speller",
            "punctuator",
        ]);
        let plan = plan(&base, Policy::Authoritative);
        assert_eq!(
            plan,
            vec![
                Action::Remove(VSELECT_PROCESSOR),
                Action::Remove(NUMPAD_PROCESSOR),
                Action::Insert {
                    item: VSELECT_PROCESSOR,
                    anchor: Anchor::After("recognizer")
                },
                Action::Insert {
                    item: NUMPAD_PROCESSOR,
                    anchor: Anchor::After(VSELECT_PROCESSOR)
                },
                Action::Insert {
                    item: PUNCT_PROCESSOR,
                    anchor: Anchor::Before("punctuator")
                },
            ]
        );
    }

    #[test]
    fn authoritative_target_moves_scrambled_order_back() {
        let base = list(&[
            "numpad",
            "punctuator",
            "recognizer",
            "rimesync_vselect",
            "speller",
        ]);
        let target = target_names(&base, Policy::Authoritative);
        assert_eq!(
            target,
            list(&[
                // punct 必须排在 punctuator 之前：punctuator 在列表最前，
                // punct 只能插到它前面。
                PUNCT_PROCESSOR,
                "punctuator",
                "recognizer",
                VSELECT_PROCESSOR,
                NUMPAD_PROCESSOR,
                "speller",
            ])
        );
    }

    #[test]
    fn authoritative_target_keeps_existing_punct_in_place() {
        // 已有 punct 且位置正确：不重复添加、不挪动。
        let base = list(&["recognizer", PUNCT_PROCESSOR, "punctuator"]);
        let target = target_names(&base, Policy::Authoritative);
        assert_eq!(
            target
                .iter()
                .filter(|item| *item == PUNCT_PROCESSOR)
                .count(),
            1
        );
        let punct = target
            .iter()
            .position(|item| item == PUNCT_PROCESSOR)
            .unwrap();
        let punctuator = target.iter().position(|item| item == "punctuator").unwrap();
        assert!(punct < punctuator, "{target:?}");
    }

    #[test]
    fn authoritative_target_fixes_misplaced_punct() {
        let base = list(&["recognizer", "punctuator", PUNCT_PROCESSOR]);
        let target = target_names(&base, Policy::Authoritative);
        let punct = target
            .iter()
            .position(|item| item == PUNCT_PROCESSOR)
            .unwrap();
        let punctuator = target.iter().position(|item| item == "punctuator").unwrap();
        assert!(punct < punctuator, "{target:?}");
    }

    #[test]
    fn conservative_plan_does_nothing_without_vselect() {
        let base = list(&["ascii_composer", "recognizer", "key_binder"]);
        assert!(plan(&base, Policy::Conservative).is_empty());
        assert_eq!(target_names(&base, Policy::Conservative), base);
    }

    #[test]
    fn conservative_plan_keeps_vselect_where_user_put_it() {
        // vselect 在 recognizer 之后但中间隔着别的 —— 保守策略不许挪它。
        let base = list(&["recognizer", "speller", VSELECT_PROCESSOR, "punctuator"]);
        let target = target_names(&base, Policy::Conservative);
        assert_eq!(
            target,
            list(&[
                "recognizer",
                "speller",
                VSELECT_PROCESSOR,
                NUMPAD_PROCESSOR,
                // 缺的 punct 补在 punctuator 前面（保守策略只补缺，不挪已有的）。
                PUNCT_PROCESSOR,
                "punctuator"
            ])
        );
    }

    #[test]
    fn conservative_plan_adds_missing_punct() {
        let base = list(&[
            "recognizer",
            VSELECT_PROCESSOR,
            NUMPAD_PROCESSOR,
            "punctuator",
        ]);
        let target = target_names(&base, Policy::Conservative);
        let punct = target
            .iter()
            .position(|item| item == PUNCT_PROCESSOR)
            .unwrap();
        let punctuator = target.iter().position(|item| item == "punctuator").unwrap();
        assert!(punct < punctuator, "{target:?}");
    }

    #[test]
    fn conservative_plan_keeps_misplaced_punct_where_it_is() {
        // 用户自己摆的位置，保守策略不动它（只是别再往上加）。
        let base = list(&[
            "recognizer",
            VSELECT_PROCESSOR,
            NUMPAD_PROCESSOR,
            "punctuator",
            PUNCT_PROCESSOR,
        ]);
        let target = target_names(&base, Policy::Conservative);
        assert_eq!(target, base);
    }

    #[test]
    fn validate_catches_missing_vselect_and_broken_numpad() {
        assert!(validate(&list(&["recognizer", "numpad"])).is_err());
        assert!(validate(&list(&["recognizer", VSELECT_PROCESSOR, "speller"])).is_err());
        assert!(validate(&list(&[
            "recognizer",
            VSELECT_PROCESSOR,
            NUMPAD_PROCESSOR,
            PUNCT_PROCESSOR
        ]))
        .is_ok());
    }

    #[test]
    fn validate_catches_missing_punct() {
        let base = list(&["recognizer", VSELECT_PROCESSOR, NUMPAD_PROCESSOR]);
        let error = validate(&base).expect_err("缺 punct 必须报错");
        assert!(error.contains(PUNCT_PROCESSOR), "{error}");
    }

    #[test]
    fn validate_catches_punct_after_punctuator() {
        let base = list(&[
            "recognizer",
            VSELECT_PROCESSOR,
            NUMPAD_PROCESSOR,
            "punctuator",
            PUNCT_PROCESSOR,
        ]);
        assert!(validate(&base).is_err());
        let good = list(&[
            "recognizer",
            VSELECT_PROCESSOR,
            NUMPAD_PROCESSOR,
            PUNCT_PROCESSOR,
            "punctuator",
        ]);
        assert!(validate(&good).is_ok());
    }

    #[test]
    fn validate_conservative_does_not_require_vselect_after_recognizer() {
        // 用户自己插了别的处理器在中间 —— 保守校验不报。
        let base = list(&[
            "recognizer",
            "speller",
            VSELECT_PROCESSOR,
            NUMPAD_PROCESSOR,
            PUNCT_PROCESSOR,
        ]);
        assert!(validate(&base).is_err());
        assert!(validate_conservative(&base).is_ok());
    }

    #[test]
    fn vselect_anchor_prefers_the_earlier_input_stage() {
        let base = list(&["speller", "key_binder"]);
        assert_eq!(vselect_anchor(&base), Anchor::Before("speller"));
        let base = list(&["key_binder", "speller"]);
        assert_eq!(vselect_anchor(&base), Anchor::Before("key_binder"));
        let base = list(&["selector"]);
        assert_eq!(vselect_anchor(&base), Anchor::End);
    }
}
