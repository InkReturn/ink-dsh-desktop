//! 运行时版本解析与最低版本基线判定。
//!
//! 版本字符串统一交给 `semver` 解析，本模块只负责剥掉 npm/pnpm 常见的 `v`
//! 前缀，并把判定结果收敛成「未配置 / 达标 / 过低 / 未知」四种状态，避免各
//! 调用点自行比较字符串或静默放行。

use semver::Version;

/// 实际版本与最低基线之间的判定结果。
///
/// `Unconfigured` 只在清单没有声明最低版本时出现；`Unknown` 表示基线已经
/// 配置、但候选版本读不到或解析失败，此时不能证明兼容。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VersionGate {
    /// 清单未配置最低版本，任何版本都放行。
    Unconfigured,
    /// 版本可解析且不低于基线。
    Compatible,
    /// 版本可解析但低于基线，必须回退到下一个来源。
    TooOld {
        /// 候选自身的版本文本（已归一化）。
        actual: String,
        /// 清单声明的最低版本文本。
        minimum: String,
    },
    /// 版本缺失或无法解析，且基线已配置，无法证明兼容。
    Unknown {
        /// 候选提供的原始版本文本；缺失时为 `None`。
        actual: Option<String>,
        /// 清单声明的最低版本文本。
        minimum: String,
    },
}

impl VersionGate {
    /// 是否允许使用该候选。
    ///
    /// 返回值：`Unconfigured` 与 `Compatible` 为 `true`；`TooOld` 与
    /// `Unknown` 为 `false`，调用方应记录原因并继续尝试下一来源。
    pub fn is_compatible(&self) -> bool {
        matches!(self, Self::Unconfigured | Self::Compatible)
    }

    /// 生成面向上层日志/UI 的不兼容原因。
    ///
    /// 返回值：兼容或未配置时返回 `None`；不兼容时返回中文原因，包含实际
    /// 版本与最低版本，便于用户判断该升级哪个 DSH。
    pub fn reason(&self) -> Option<String> {
        match self {
            Self::Unconfigured | Self::Compatible => None,
            Self::TooOld { actual, minimum } => Some(format!(
                "版本 {actual} 低于最低要求 {minimum}，已跳过该来源"
            )),
            Self::Unknown { actual, minimum } => Some(match actual {
                Some(actual) => format!("版本 {actual} 无法解析，不能校验最低要求 {minimum}"),
                None => format!("无法读取版本，不能校验最低要求 {minimum}"),
            }),
        }
    }
}

/// 解析版本文本。
///
/// 参数：
/// - `text`：来自 `package.json`、运行时清单或命令行输出的版本字符串；
///   允许前后空白和单个 `v`/`V` 前缀（如 `v0.2.1-rc.2`）。
///
/// 返回值：解析成功时返回 `semver::Version`；空串、非 semver 文本（例如
/// 时间戳或 `unknown`）返回 `None`。该函数不读取任何文件。
pub fn parse_version(text: &str) -> Option<Version> {
    // 1. 去掉常见包装：空白与 `v` 前缀。
    // 2. 交给 semver 解析；预发布段（rc/beta）保持原有语义。
    let trimmed = text.trim();
    let without_prefix = trimmed
        .strip_prefix(['v', 'V'])
        .unwrap_or(trimmed)
        .trim_start();
    Version::parse(without_prefix).ok()
}

/// 对照最低基线评估一个候选版本。
///
/// 参数：
/// - `actual`：候选版本原始文本；读不到版本时传 `None`。
/// - `minimum`：清单声明的最低版本；`None` 表示清单未配置基线。
///
/// 返回值：见 [`VersionGate`]。语义与参考实现不同：基线已配置时，
/// 版本读不到按 `Unknown` 拒绝，而不是静默放行，避免把无法证明兼容的
/// 入口当成可用核心。
pub fn evaluate(actual: Option<&str>, minimum: Option<&Version>) -> VersionGate {
    // 1. 没有基线时不做限制。
    let Some(minimum) = minimum else {
        return VersionGate::Unconfigured;
    };
    let minimum_text = minimum.to_string();

    // 2. 候选版本缺失或无法解析 → Unknown。
    let Some(raw) = actual else {
        return VersionGate::Unknown {
            actual: None,
            minimum: minimum_text,
        };
    };
    let Some(parsed) = parse_version(raw) else {
        return VersionGate::Unknown {
            actual: Some(raw.trim().to_string()),
            minimum: minimum_text,
        };
    };

    // 3. 可解析版本按 semver 顺序比较；预发布版本低于同版本正式版。
    if parsed >= *minimum {
        VersionGate::Compatible
    } else {
        VersionGate::TooOld {
            actual: parsed.to_string(),
            minimum: minimum_text,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 验证 `v` 前缀、空白与预发布段都能被正确解析。
    #[test]
    fn parses_prefixed_and_prerelease_versions() {
        // 1. 覆盖常见 npm 版本写法。
        // 2. 断言前缀被剥离、预发布段保留。
        assert_eq!(
            parse_version("v0.2.1-rc.2").unwrap().to_string(),
            "0.2.1-rc.2"
        );
        assert_eq!(parse_version(" 0.2.1 ").unwrap().to_string(), "0.2.1");
        assert_eq!(parse_version("V0.2.0").unwrap().to_string(), "0.2.0");
        assert_eq!(parse_version("unknown"), None);
        assert_eq!(parse_version("0.2"), None);
        assert_eq!(parse_version(""), None);
    }

    /// 验证 semver 顺序：rc.2 < rc.10 < 正式版。
    #[test]
    fn orders_prerelease_numerically() {
        // 1. 解析三个版本。
        // 2. 断言预发布标识按数值而非字符串比较。
        let rc2 = parse_version("0.2.1-rc.2").unwrap();
        let rc10 = parse_version("0.2.1-rc.10").unwrap();
        let release = parse_version("0.2.1").unwrap();
        assert!(rc2 < rc10);
        assert!(rc10 < release);
        assert!(parse_version("0.2.0").unwrap() < rc2);
    }

    /// 验证最低版本判定覆盖达标、过低、未知和未配置。
    #[test]
    fn evaluates_minimum_version_gate() {
        // 1. 基线 0.2.0：0.2.1-rc.2 达标，0.1.9 过低。
        // 2. 版本缺失或损坏时按未知拒绝。
        // 3. 没有基线时不做限制。
        let minimum = parse_version("0.2.0").unwrap();
        assert_eq!(
            evaluate(Some("0.2.1-rc.2"), Some(&minimum)),
            VersionGate::Compatible
        );
        assert!(matches!(
            evaluate(Some("0.1.9"), Some(&minimum)),
            VersionGate::TooOld { .. }
        ));
        assert!(matches!(
            evaluate(None, Some(&minimum)),
            VersionGate::Unknown { .. }
        ));
        assert!(matches!(
            evaluate(Some("not-a-version"), Some(&minimum)),
            VersionGate::Unknown { .. }
        ));
        assert_eq!(evaluate(None, None), VersionGate::Unconfigured);
        assert_eq!(evaluate(Some("0.0.1"), None), VersionGate::Unconfigured);
    }

    /// 验证不兼容原因包含两侧版本，且兼容时不产生噪声日志。
    #[test]
    fn explains_incompatible_versions() {
        // 1. 过低与未知各生成一条原因。
        // 2. 兼容状态不返回原因。
        let minimum = parse_version("0.2.0").unwrap();
        let too_old = evaluate(Some("0.1.9"), Some(&minimum));
        assert!(too_old.reason().unwrap().contains("0.2.0"));
        assert!(too_old.reason().unwrap().contains("0.1.9"));
        let unknown = evaluate(None, Some(&minimum));
        assert!(unknown.reason().unwrap().contains("无法读取版本"));
        assert_eq!(evaluate(Some("0.2.1"), Some(&minimum)).reason(), None);
    }
}
