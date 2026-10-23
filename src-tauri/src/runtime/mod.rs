//! 首启运行时装配：来源解析、版本基线、托管安装与状态机。
//!
//! 这一层只处理「用哪个 Node + DSH、是否需要在首启装配、装配到什么阶段」，
//! 不负责启动 `dsh web` 进程；进程生命周期仍由 `sidecar` 模块管理。
//! 所有路径都由调用方注入（AppData/Resources/开发目录），便于在隔离目录
//! 中测试，避免触碰真实的用户 `DSH_HOME`。

pub mod install;
pub mod manifest;
pub mod source;
pub mod version;

pub use manifest::LoadedManifest;
pub use source::{CoreCandidate, CoreSource, RejectedCandidate, Resolution, RuntimeEnvironment};

use std::{fmt, path::PathBuf};

use serde::Serialize;

/// 运行时装配过程中可能出现的结构化错误。
///
/// 用户可见文案与诊断细节分离：`user_message` 只给等待页一句能行动的话，
/// `detail` 保留路径、版本、期望摘要等排障信息，写入 sidecar 日志。
#[derive(Debug)]
pub enum RuntimeError {
    /// 环境/清单条件不满足，属于「本轮无法完成」而非代码错误。
    BlockedByEnv {
        /// 稳定错误码，便于日志检索与测试断言。
        code: &'static str,
        /// 面向排障的细节说明。
        detail: String,
    },
    /// 完整性校验失败（SHA-256 不符、摘要格式非法、归档路径逃逸等）。
    Integrity {
        /// 稳定错误码。
        code: &'static str,
        /// 包含期望值与实际值的细节。
        detail: String,
    },
    /// 文件系统读写失败。
    Io {
        /// 正在执行的动作，例如「复制 node 目录」。
        context: String,
        /// 底层 IO 错误。
        source: std::io::Error,
    },
    /// 运行时目录布局不完整（缺少 node.exe、lib/bin.js 等）。
    Layout {
        /// 被检查的根目录。
        path: PathBuf,
        /// 缺失项的可读描述。
        missing: String,
    },
    /// 版本无法满足清单基线。
    Version {
        /// 包含实际版本与最低版本的中文细节。
        detail: String,
    },
}

impl RuntimeError {
    /// 构造一个 blocked-by-env 错误。
    ///
    /// 参数：
    /// - `code`：稳定错误码，建议全大写下划线。
    /// - `detail`：排障细节，不得包含凭据。
    pub fn blocked(code: &'static str, detail: impl Into<String>) -> Self {
        // 1. 统一格式，避免各调用点自行拼错误码。
        Self::BlockedByEnv {
            code,
            detail: detail.into(),
        }
    }

    /// 构造一个完整性错误。
    ///
    /// 参数：
    /// - `code`：稳定错误码。
    /// - `detail`：期望值与实际值的差异说明。
    pub fn integrity(code: &'static str, detail: impl Into<String>) -> Self {
        // 1. 与 blocked 分开，便于用户区分「环境未就绪」和「内容被篡改」。
        Self::Integrity {
            code,
            detail: detail.into(),
        }
    }

    /// 生成稳定的错误码。
    ///
    /// 返回值：全大写下划线错误码；UI 与日志都不依赖中文文案做匹配。
    pub fn code(&self) -> &'static str {
        match self {
            Self::BlockedByEnv { code, .. } | Self::Integrity { code, .. } => code,
            Self::Io { .. } => "RUNTIME_IO_FAILED",
            Self::Layout { .. } => "RUNTIME_LAYOUT_INCOMPLETE",
            Self::Version { .. } => "RUNTIME_VERSION_UNSUPPORTED",
        }
    }

    /// 生成等待页展示的一句话。
    ///
    /// 返回值：不包含路径与底层错误的用户文案；具体原因由 `detail` 提供。
    pub fn user_message(&self) -> String {
        match self {
            Self::BlockedByEnv { .. } => {
                "运行时装配暂不可用：核心包来源尚未就绪，请反馈给维护者".to_string()
            }
            Self::Integrity { .. } => "运行时文件校验失败，请重试或重新安装".to_string(),
            Self::Io { .. } => "读写运行时文件失败，请重试".to_string(),
            Self::Layout { .. } => "运行时文件不完整，请重试或重新安装".to_string(),
            Self::Version { .. } => "运行时版本低于最低要求，请升级后重试".to_string(),
        }
    }

    /// 生成写入日志的详细说明。
    ///
    /// 返回值：包含错误码、路径或底层错误的中文文本，仅用于日志与状态详情。
    pub fn detail(&self) -> String {
        match self {
            Self::BlockedByEnv { code, detail } | Self::Integrity { code, detail } => {
                format!("{code}: {detail}")
            }
            Self::Io { context, source } => format!("{context}：{source}"),
            Self::Layout { path, missing } => {
                format!("运行时目录 {} 缺少 {missing}", path.display())
            }
            Self::Version { detail } => detail.clone(),
        }
    }
}

impl fmt::Display for RuntimeError {
    /// 输出用户可见文案，避免 `?` 转换后把路径暴露到界面。
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        // 1. 复用统一的用户文案。
        formatter.write_str(&self.user_message())
    }
}

impl std::error::Error for RuntimeError {
    /// 返回底层 IO 错误（存在时），供日志链展示。
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            _ => None,
        }
    }
}

/// 托管运行时装配的阶段，序列化后供等待页展示。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum InstallStage {
    /// 校验下载/本地制品的完整性。
    Verify,
    /// 解压归档或复制目录到 staging。
    Extract,
    /// 校验布局并原子切换到托管根目录。
    Activate,
}

impl InstallStage {
    /// 生成等待页可读的阶段文案。
    ///
    /// 返回值：固定的中文短语，不包含路径或进度百分比。
    pub fn label(self) -> &'static str {
        match self {
            Self::Verify => "校验运行时",
            Self::Extract => "解压运行时",
            Self::Activate => "启用运行时",
        }
    }
}

/// 首启装配状态机对外的状态快照。
///
/// 序列化由 `phase` 字段区分（detect / installing / ready / failed），
/// 等待页据此切换文案与重试入口。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "phase", rename_all = "snake_case")]
pub enum RuntimeStatus {
    /// 正在探测来源与版本。
    Detect,
    /// 正在装配托管运行时。
    Installing {
        /// 正在装配的来源，目前只可能是 Managed。
        source: CoreSource,
        /// 当前装配阶段。
        stage: InstallStage,
        /// 阶段细节，例如「校验 SHA-256」。
        detail: String,
    },
    /// 来源已确定，DSH 入口可用；进程启动由 sidecar 状态继续跟踪。
    Ready {
        /// 最终选中的来源。
        source: CoreSource,
        /// DSH 版本；读不到时为 `None`。
        version: Option<String>,
    },
    /// 装配或来源解析失败；`reasons` 是逐来源的拒绝原因。
    Failed {
        /// 面向用户的一句话。
        message: String,
        /// 可选的修复建议。
        hint: Option<String>,
        /// 逐来源诊断，按优先级顺序排列。
        reasons: Vec<String>,
    },
}

/// 可独立测试的装配状态机。
///
/// 只保存状态，不执行 IO；`sidecar` 在真实流程中调用它，测试可以直接驱动
/// 状态迁移，避免为状态机引入文件系统夹具。
#[derive(Debug, Default)]
pub struct BootstrapState {
    /// 当前状态快照。
    status: Option<RuntimeStatus>,
}

impl BootstrapState {
    /// 读取当前状态。
    ///
    /// 返回值：尚未显式初始化时按 `Detect` 处理，从不让 UI 看到空状态。
    pub fn status(&self) -> RuntimeStatus {
        // 1. 默认状态是「正在探测」，与窗口创建后的第一帧一致。
        self.status.clone().unwrap_or(RuntimeStatus::Detect)
    }

    /// 回到探测阶段；重试与首次启动共用。
    pub fn detect(&mut self) {
        // 1. 覆盖上一轮的 ready/failed/installing。
        self.status = Some(RuntimeStatus::Detect);
    }

    /// 进入装配阶段。
    ///
    /// 参数：
    /// - `source`：正在装配的来源。
    /// - `stage`：当前阶段。
    /// - `detail`：阶段细节，供等待页展示。
    pub fn begin_install(&mut self, source: CoreSource, stage: InstallStage, detail: String) {
        // 1. 允许从探测态进入装配，也允许装配过程中更新阶段与细节。
        if matches!(
            self.status,
            None | Some(RuntimeStatus::Detect) | Some(RuntimeStatus::Installing { .. })
        ) {
            self.status = Some(RuntimeStatus::Installing {
                source,
                stage,
                detail,
            });
        }
    }

    /// 标记来源可用。
    ///
    /// 参数：
    /// - `source`：最终来源。
    /// - `version`：DSH 版本文本；读取失败时为 `None`。
    pub fn ready(&mut self, source: CoreSource, version: Option<String>) {
        // 1. 探测与装配阶段都可以直接成功（本地/全局来源不需要装配）。
        self.status = Some(RuntimeStatus::Ready { source, version });
    }

    /// 标记失败并保留逐来源原因。
    ///
    /// 参数：
    /// - `message`：用户可见的一句话。
    /// - `hint`：可选修复建议。
    /// - `reasons`：逐来源拒绝原因，顺序与来源优先级一致。
    pub fn fail(&mut self, message: String, hint: Option<String>, reasons: Vec<String>) {
        // 1. 失败状态保留逐来源原因，供等待页与重试诊断。
        self.status = Some(RuntimeStatus::Failed {
            message,
            hint,
            reasons,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 验证状态机按 detect → installing → failed → retry → ready 迁移。
    #[test]
    fn bootstrap_state_machine_transitions() {
        // 1. 默认是探测态，且不允许重试。
        let mut state = BootstrapState::default();
        assert_eq!(state.status(), RuntimeStatus::Detect);
        assert!(!matches!(state.status(), RuntimeStatus::Failed { .. }));

        // 2. 进入装配后持续上报阶段。
        state.begin_install(
            CoreSource::Managed,
            InstallStage::Extract,
            "复制运行时".into(),
        );
        assert!(matches!(
            state.status(),
            RuntimeStatus::Installing {
                stage: InstallStage::Extract,
                ..
            }
        ));

        // 3. 失败后可重试，重试从探测重新开始。
        state.fail(
            "装配失败".into(),
            Some("检查网络".into()),
            vec!["原因".into()],
        );
        assert!(matches!(state.status(), RuntimeStatus::Failed { .. }));
        state.detect();
        assert_eq!(state.status(), RuntimeStatus::Detect);

        // 4. 成功进入 Ready，且不再允许重复装配。
        state.ready(CoreSource::Global, Some("0.2.1-rc.2".into()));
        assert_eq!(
            state.status(),
            RuntimeStatus::Ready {
                source: CoreSource::Global,
                version: Some("0.2.1-rc.2".into())
            }
        );
        assert!(!matches!(state.status(), RuntimeStatus::Failed { .. }));
    }

    /// 验证错误对象把用户文案与排障细节分开。
    #[test]
    fn runtime_error_separates_user_and_detail_messages() {
        // 1. blocked-by-env 的用户文案不暴露 URL/路径。
        let error = RuntimeError::blocked("RUNTIME_ARTIFACT_UNCONFIGURED", "清单缺少 url");
        assert_eq!(error.code(), "RUNTIME_ARTIFACT_UNCONFIGURED");
        assert!(!error.user_message().contains("url"));
        assert!(error.detail().contains("清单缺少 url"));

        // 2. 布局错误在细节里带路径，用户文案保持克制。
        let error = RuntimeError::Layout {
            path: PathBuf::from("C:/tmp/runtime"),
            missing: "dsh/lib/bin.js".into(),
        };
        assert!(error.detail().contains("bin.js"));
        assert!(!error.user_message().contains("C:/tmp/runtime"));
    }
}
