//! 运行时来源解析：DSH_BIN 覆盖、本地、托管、捆绑、全局安装。
//!
//! 解析顺序固定为「Override → Local → Global → Managed → Bundled」。每个
//! 候选都先做版本基线校验：低于清单 `engines.dsh.minimum` 或版本读不到时
//! 记录拒绝原因并继续回退，绝不静默使用。解析过程只读文件系统，不启动进程。
//! 用户已有的 DSH（本地、全局）版本达标时直接复用，不触发下载/装配。

use std::{
    env, fs,
    path::{Path, PathBuf},
};

use semver::Version;
use serde::Serialize;

use super::{
    install::{ArtifactSource, ManagedRuntime},
    manifest::{CoreDescriptor, LoadedManifest},
    version::{evaluate, parse_version},
    RuntimeError,
};

/// npm 包内 DSH CLI 入口的相对路径（相对于安装前缀）。
pub const DSH_PACKAGE_BIN: &str = "node_modules/@deepseek-ai/dsh/lib/bin.js";

/// 捆绑/托管运行时内 DSH 入口的相对路径。
pub const RUNTIME_DSH_BIN: &str = "dsh/lib/bin.js";

/// 捆绑/托管运行时内 Node 可执行文件相对路径（Windows）。
pub const RUNTIME_NODE_EXE: &str = "node/node.exe";

/// 捆绑/托管运行时内 Node 可执行文件相对路径（Unix）。
pub const RUNTIME_NODE_POSIX: &str = "node/node";

/// npm 包名；用于拒绝把任意脚本当作 DSH 入口。
pub const DSH_PACKAGE_NAME: &str = "@deepseek-ai/dsh";

/// 显式指定 DSH 入口的环境变量。
pub const ENV_DSH_BIN: &str = "DSH_BIN";

/// 显式指定 Node 可执行文件的环境变量。
pub const ENV_DSH_NODE: &str = "DSH_NODE";

/// 显式指定捆绑运行时根目录的环境变量。
pub const ENV_RUNTIME_DIR: &str = "INKDSH_RUNTIME_DIR";

/// 运行时来源，顺序即优先级。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CoreSource {
    /// `DSH_BIN` 显式覆盖，跳过自动回退。
    Override,
    /// 项目本地（或开发目录）安装的 DSH。
    Local,
    /// 首启装配到 AppData 的托管运行时。
    Managed,
    /// 安装包 Resources 内的捆绑运行时。
    Bundled,
    /// PATH / npm 全局安装的 DSH。
    Global,
}

impl CoreSource {
    /// 面向日志与等待页的中文名称。
    pub fn label(self) -> &'static str {
        match self {
            Self::Override => "DSH_BIN 覆盖",
            Self::Local => "项目本地 DSH",
            Self::Managed => "托管运行时",
            Self::Bundled => "捆绑运行时",
            Self::Global => "全局 DSH",
        }
    }
}

/// 运行时目录布局检查结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeLayout {
    /// Node 可执行文件绝对路径。
    pub node: PathBuf,
    /// DSH `lib/bin.js` 绝对路径。
    pub bin: PathBuf,
    /// DSH 包目录（含 package.json）。
    pub dsh_root: PathBuf,
}

/// 检查 `node/` + `dsh/lib/bin.js` 布局。
///
/// 参数：
/// - `root`：捆绑运行时或托管运行时的根目录。
///
/// 返回值：两个入口文件都存在时返回路径集合；任一缺失返回 `None`。该函数
/// 不读取版本清单，也不执行文件。
pub fn runtime_layout(root: &Path) -> Option<RuntimeLayout> {
    // 1. 按平台选择 node 文件名。
    let node = root.join(if cfg!(windows) {
        RUNTIME_NODE_EXE
    } else {
        RUNTIME_NODE_POSIX
    });
    let bin = root.join(RUNTIME_DSH_BIN);

    // 2. 两个入口必须是普通文件。
    if !node.is_file() || !bin.is_file() {
        return None;
    }
    Some(RuntimeLayout {
        node: normalize_existing_path(&node),
        bin: normalize_existing_path(&bin),
        dsh_root: normalize_existing_path(&root.join("dsh")),
    })
}

/// 读取 npm 包的 `package.json` 版本。
///
/// 参数：
/// - `package_dir`：包根目录；读取 `<package_dir>/package.json`。
///
/// 返回值：`version` 字段存在且为字符串时返回原文（可能带 `v` 前缀）；
/// 文件缺失、JSON 损坏或字段缺失时返回 `None`，由调用方按基线决定是否拒绝。
pub fn read_package_version(package_dir: &Path) -> Option<String> {
    // 1. 读取并解析 package.json。
    let value = read_package_json(package_dir)?;

    // 2. 只接受字符串版本，避免把数字或对象当成版本。
    value
        .get("version")?
        .as_str()
        .map(|version| version.trim().to_string())
        .filter(|version| !version.is_empty())
}

/// 读取 npm 包的包名。
///
/// 参数：
/// - `package_dir`：包根目录；读取 `<package_dir>/package.json`。
///
/// 返回值：`name` 字段为字符串时返回原文；文件缺失、JSON 损坏或字段缺失
/// 时返回 `None`。该函数只读文件系统。
fn read_package_name(package_dir: &Path) -> Option<String> {
    // 1. 复用统一的 package.json 读取逻辑。
    let value = read_package_json(package_dir)?;

    // 2. 只接受字符串包名。
    value
        .get("name")?
        .as_str()
        .map(|name| name.trim().to_string())
        .filter(|name| !name.is_empty())
}

/// 读取并解析包目录下的 `package.json`。
///
/// 参数：
/// - `package_dir`：包根目录。
///
/// 返回值：JSON 对象；文件缺失或内容损坏时返回 `None`。
fn read_package_json(package_dir: &Path) -> Option<serde_json::Value> {
    // 1. 读取文本并解析；任何一步失败都视为不可用候选。
    let text = fs::read_to_string(package_dir.join("package.json")).ok()?;
    serde_json::from_str(&text).ok()
}

/// 一个可启动的 DSH 候选。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CoreCandidate {
    /// 候选来源。
    pub source: CoreSource,
    /// Node 可执行文件；非捆绑来源为 `DSH_NODE` 或 PATH 中的 `node`。
    pub node: PathBuf,
    /// DSH `lib/bin.js` 绝对路径。
    pub bin: PathBuf,
    /// 解析成功时的 semver 版本。
    pub version: Option<Version>,
    /// 原始版本文本，用于日志与错误提示。
    pub version_text: Option<String>,
    /// DSH 包目录。
    pub package_dir: PathBuf,
}

impl CoreCandidate {
    /// 从 npm 布局的 `lib/bin.js` 构造候选。
    ///
    /// 参数：
    /// - `source`：来源标签。
    /// - `bin`：`.../@deepseek-ai/dsh/lib/bin.js`。
    /// - `node`：用于启动该入口的 Node 路径。
    ///
    /// 返回值：入口存在时返回候选；路径不存在、无法推导包目录，或 Override
    /// 指向的包名不是 `@deepseek-ai/dsh` 时返回 `None`。该函数只读
    /// `package.json`。
    pub fn from_package_bin(source: CoreSource, bin: &Path, node: PathBuf) -> Option<Self> {
        // 1. 入口必须是文件。
        if !bin.is_file() {
            return None;
        }

        // 2. bin 位于 `<package>/lib/bin.js`，回溯两级得到包目录。
        let package_dir = bin.parent()?.parent()?.to_path_buf();

        // 3. 显式覆盖必须指向真正的 DSH 包，避免把任意 .js/.exe 当内核执行。
        if source == CoreSource::Override
            && read_package_name(&package_dir).as_deref() != Some(DSH_PACKAGE_NAME)
        {
            return None;
        }
        let version_text = read_package_version(&package_dir);
        Some(Self {
            source,
            node,
            bin: normalize_existing_path(bin),
            version: version_text.as_deref().and_then(parse_version),
            version_text,
            package_dir: normalize_existing_path(&package_dir),
        })
    }

    /// 从 `node/` + `dsh/lib/bin.js` 运行时布局构造候选。
    ///
    /// 参数：
    /// - `source`：来源标签（Bundled 或 Managed）。
    /// - `root`：运行时根目录。
    ///
    /// 返回值：布局完整时返回候选；否则返回 `None`。
    pub fn from_runtime_root(source: CoreSource, root: &Path) -> Option<Self> {
        // 1. 先检查入口文件。
        let layout = runtime_layout(root)?;

        // 2. 版本以 DSH 包内的 package.json 为准，不信任外部标记。
        let version_text = read_package_version(&layout.dsh_root);
        Some(Self {
            source,
            node: layout.node,
            bin: layout.bin,
            version: version_text.as_deref().and_then(parse_version),
            version_text,
            package_dir: layout.dsh_root,
        })
    }

    /// 生成日志用的版本描述。
    ///
    /// 返回值：有版本时返回规范文本，否则返回「版本未知」。
    pub fn version_label(&self) -> String {
        // 1. 优先使用规范化后的 semver，保留原始文本作为兜底。
        self.version
            .as_ref()
            .map(ToString::to_string)
            .or_else(|| self.version_text.clone())
            .unwrap_or_else(|| "版本未知".to_string())
    }
}

/// 被拒绝的候选及原因。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RejectedCandidate {
    /// 候选来源。
    pub source: CoreSource,
    /// 拒绝原因，包含路径或版本差异。
    pub detail: String,
}

impl RejectedCandidate {
    /// 生成单行诊断。
    ///
    /// 返回值：`来源：原因`，用于日志与等待页的失败详情。
    pub fn describe(&self) -> String {
        // 1. 统一格式，避免调用点各自拼接。
        format!("{}：{}", self.source.label(), self.detail)
    }
}

/// 来源解析结果。
#[derive(Debug, Clone)]
pub enum Resolution {
    /// 找到可用候选，可以直接启动。
    Use {
        /// 选中的候选。
        candidate: CoreCandidate,
        /// 优先级更高但被拒绝的候选。
        rejected: Vec<RejectedCandidate>,
        /// 解析阶段的附加诊断。
        diagnostics: Vec<String>,
    },
    /// 需要先把核心包装配到托管目录。
    InstallManaged {
        /// 已就绪的安装来源。
        artifact: ArtifactSource,
        /// 已拒绝的候选。
        rejected: Vec<RejectedCandidate>,
        /// 附加诊断。
        diagnostics: Vec<String>,
    },
    /// 没有任何可用来源。
    Unavailable {
        /// 逐来源原因（含修复建议）。
        reasons: Vec<String>,
        /// 附加诊断。
        diagnostics: Vec<String>,
    },
}

/// 托管安装来源的规划结果。
#[derive(Debug, Clone)]
pub enum ArtifactPlan {
    /// 来源可用，可以进入安装状态机。
    Ready(ArtifactSource),
    /// 来源不可用（清单未配置、摘要非法等）。
    Blocked {
        /// 稳定错误码。
        code: &'static str,
        /// 排障细节。
        detail: String,
    },
}

impl ArtifactPlan {
    /// 依据清单描述与调试夹具规划安装来源。
    ///
    /// 参数：
    /// - `descriptor`：清单 `core` 字段。
    /// - `fixture`：调试夹具路径；`None` 时走正式清单。
    /// - `fixture_sha256`：调试 zip 夹具的摘要。
    ///
    /// 返回值：成功时 `Ready`；失败时把结构化错误摊平成 `Blocked`，避免
    /// 解析阶段因为错误类型不可克隆而中断。
    pub fn plan(
        descriptor: &CoreDescriptor,
        fixture: Option<PathBuf>,
        fixture_sha256: Option<String>,
    ) -> Self {
        // 1. 复用安装器的来源判定。
        match ArtifactSource::from_descriptor(descriptor, fixture, fixture_sha256) {
            Ok(source) => Self::Ready(source),
            Err(RuntimeError::BlockedByEnv { code, detail })
            | Err(RuntimeError::Integrity { code, detail }) => Self::Blocked { code, detail },
            Err(error) => Self::Blocked {
                code: error.code(),
                detail: error.detail(),
            },
        }
    }
}

/// 一次启动所需的全部来源信息。
///
/// 所有路径都在构造时确定，`resolve` 只做只读探测；测试可以直接构造该
/// 结构，指向临时目录，不需要改环境变量。
#[derive(Debug, Clone)]
pub struct RuntimeEnvironment {
    /// 清单声明的最低版本。
    pub minimum: Option<Version>,
    /// `DSH_BIN` 显式覆盖。
    pub override_bin: Option<PathBuf>,
    /// `DSH_NODE` 显式覆盖。
    pub node_override: Option<PathBuf>,
    /// 本地安装前缀候选（每个前缀下找 `node_modules/@deepseek-ai/dsh`）。
    pub local_roots: Vec<PathBuf>,
    /// 托管运行时。
    pub managed: ManagedRuntime,
    /// 捆绑运行时根目录。
    pub bundled_root: Option<PathBuf>,
    /// 全局安装前缀候选。
    pub global_roots: Vec<PathBuf>,
    /// 托管安装来源规划。
    pub artifact: ArtifactPlan,
    /// 构造阶段的诊断信息。
    pub diagnostics: Vec<String>,
}

impl RuntimeEnvironment {
    /// 从环境变量、Tauri 路径和清单构造运行时环境。
    ///
    /// 参数：
    /// - `app_data_dir`：应用数据目录；托管运行时固定在其下。
    /// - `resource_dir`：Tauri 安装资源根；捆绑运行时为 `<资源根>/runtime`。
    /// - `crate_dir`：编译期 `src-tauri` 目录；仅开发构建用于定位仓库内运行时
    ///   和清单，打包后不会用到。
    /// - `manifest`：已加载的运行时清单。
    ///
    /// 返回值：可直接用于解析的 [`RuntimeEnvironment`]。该函数只读环境变量
    /// 与文件系统，不写任何路径。
    pub fn discover(
        app_data_dir: &Path,
        resource_dir: Option<&Path>,
        crate_dir: Option<&Path>,
        manifest: &LoadedManifest,
    ) -> Self {
        let mut diagnostics = Vec::new();

        // 1. 显式覆盖只读取非空环境变量。
        let override_bin = env_path(ENV_DSH_BIN);
        let node_override = env_path(ENV_DSH_NODE);
        if override_bin.is_some() {
            diagnostics.push("检测到 DSH_BIN 显式覆盖，将跳过自动来源回退".to_string());
        }

        // 2. 捆绑运行时：环境变量 → 安装资源 → 仅开发构建回退仓库目录。
        let mut bundled_root = None;
        if let Some(dir) = env_path(ENV_RUNTIME_DIR) {
            if dir.is_dir() {
                bundled_root = Some(dir);
            } else {
                diagnostics.push(format!(
                    "INKDSH_RUNTIME_DIR 指向的目录不存在，已忽略并继续查找安装资源：{}",
                    dir.display()
                ));
            }
        }
        if bundled_root.is_none() {
            bundled_root = resource_dir
                .map(|dir| dir.join("runtime"))
                .filter(|path| path.is_dir())
                .or_else(|| {
                    crate_dir
                        .map(|dir| dir.join("resources").join("runtime"))
                        .filter(|_| cfg!(debug_assertions))
                        .filter(|path| path.is_dir())
                });
        }

        // 3. 调试夹具只在调试构建读取，生产包永远不会走 fixture 路径。
        let (fixture, fixture_sha256) = if cfg!(debug_assertions) {
            (
                env_path(super::install::ENV_RUNTIME_ARTIFACT),
                env::var(super::install::ENV_RUNTIME_ARTIFACT_SHA256)
                    .ok()
                    .filter(|value| !value.trim().is_empty()),
            )
        } else {
            (None, None)
        };
        if let Some(path) = &fixture {
            diagnostics.push(format!(
                "调试装配夹具已启用：{}（仅用于开发验证，生产构建不读取）",
                path.display()
            ));
        }

        // 4. 组装来源候选与安装规划。
        Self {
            minimum: manifest.minimum.clone(),
            override_bin,
            node_override,
            local_roots: local_install_roots(),
            managed: ManagedRuntime::new(app_data_dir),
            bundled_root,
            global_roots: global_install_roots(),
            artifact: ArtifactPlan::plan(&manifest.manifest.core, fixture, fixture_sha256),
            diagnostics,
        }
    }

    /// 解析当前应使用的运行时来源。
    ///
    /// 返回值：可用候选、需要托管安装，或所有来源都不可用。
    pub fn resolve(&self) -> Resolution {
        // 1. 首次解析时托管运行时参与评估。
        self.resolve_internal(false, Vec::new())
    }

    /// 托管装配失败后继续解析更低优先级的来源。
    ///
    /// 参数：
    /// - `seed`：已经失败的候选，按原顺序保留给诊断。
    ///
    /// 返回值：Bundled/Global 中第一个可用候选；否则返回 Unavailable。
    pub fn resolve_after_managed_failure(&self, seed: Vec<RejectedCandidate>) -> Resolution {
        // 1. 跳过 Managed，避免在同一个失败制品上死循环。
        self.resolve_internal(true, seed)
    }

    /// 执行实际的来源解析。
    ///
    /// 参数：
    /// - `skip_managed`：是否跳过托管运行时（装配失败后的回退）。
    /// - `seed`：已拒绝候选的前缀。
    fn resolve_internal(&self, skip_managed: bool, seed: Vec<RejectedCandidate>) -> Resolution {
        let mut rejected = seed;
        let mut diagnostics = Vec::new();

        // 1. DSH_BIN 是显式覆盖：路径无效时硬失败，不悄悄换来源。
        if let Some(bin) = &self.override_bin {
            if !bin.is_file() {
                return Resolution::Unavailable {
                    reasons: vec![format!(
                        "DSH_BIN 指向的文件不存在：{}；请修正该环境变量或取消覆盖",
                        bin.display()
                    )],
                    diagnostics,
                };
            }
            let Some(candidate) =
                CoreCandidate::from_package_bin(CoreSource::Override, bin, self.system_node())
            else {
                return Resolution::Unavailable {
                    reasons: vec![format!(
                        "DSH_BIN 不是可识别的 DSH 入口（需为 .../@deepseek-ai/dsh/lib/bin.js）：{}",
                        bin.display()
                    )],
                    diagnostics,
                };
            };

            // 1.1 显式覆盖是用户意图，低版本只告警；日志必须写清豁免关系。
            let gate = evaluate(candidate.version_text.as_deref(), self.minimum.as_ref());
            if let Some(reason) = gate.reason() {
                diagnostics.push(format!("DSH_BIN 显式覆盖仍使用该版本，但{reason}"));
            }
            return Resolution::Use {
                candidate,
                rejected,
                diagnostics,
            };
        }

        // 2. 项目本地安装（开发机常见）。
        for root in &self.local_roots {
            let bin = root.join(DSH_PACKAGE_BIN);
            if !bin.is_file() {
                continue;
            }
            let Some(candidate) =
                CoreCandidate::from_package_bin(CoreSource::Local, &bin, self.system_node())
            else {
                continue;
            };
            match self.gate_candidate(&candidate) {
                Ok(()) => {
                    return Resolution::Use {
                        candidate,
                        rejected,
                        diagnostics,
                    }
                }
                Err(reason) => rejected.push(RejectedCandidate {
                    source: CoreSource::Local,
                    detail: reason,
                }),
            }
        }

        // 3. 全局安装：用户已有且版本达标的 DSH 优先于下载/捆绑运行时。
        let mut seen: Vec<PathBuf> = Vec::new();
        for prefix in &self.global_roots {
            for bin in global_package_candidates(prefix) {
                if seen.contains(&bin) || !bin.is_file() {
                    continue;
                }
                seen.push(bin.clone());
                let Some(candidate) =
                    CoreCandidate::from_package_bin(CoreSource::Global, &bin, self.system_node())
                else {
                    continue;
                };
                match self.gate_candidate(&candidate) {
                    Ok(()) => {
                        return Resolution::Use {
                            candidate,
                            rejected,
                            diagnostics,
                        }
                    }
                    Err(reason) => rejected.push(RejectedCandidate {
                        source: CoreSource::Global,
                        detail: reason,
                    }),
                }
            }
        }

        // 4. 托管运行时：本地与全局都不可用时，已安装则校验版本，未安装则
        //    按清单决定是否装配。
        if !skip_managed {
            if let Some(candidate) = self.managed.probe() {
                match self.gate_candidate(&candidate) {
                    Ok(()) => {
                        return Resolution::Use {
                            candidate,
                            rejected,
                            diagnostics,
                        }
                    }
                    Err(reason) => rejected.push(RejectedCandidate {
                        source: CoreSource::Managed,
                        detail: reason,
                    }),
                }
            }
            match &self.artifact {
                ArtifactPlan::Ready(artifact) => {
                    return Resolution::InstallManaged {
                        artifact: artifact.clone(),
                        rejected,
                        diagnostics,
                    }
                }
                ArtifactPlan::Blocked { code, detail } => rejected.push(RejectedCandidate {
                    source: CoreSource::Managed,
                    detail: format!("{code}: {detail}"),
                }),
            }
        }

        // 5. 捆绑运行时（离线包）。
        if let Some(root) = &self.bundled_root {
            match CoreCandidate::from_runtime_root(CoreSource::Bundled, root) {
                Some(candidate) => match self.gate_candidate(&candidate) {
                    Ok(()) => {
                        return Resolution::Use {
                            candidate,
                            rejected,
                            diagnostics,
                        }
                    }
                    Err(reason) => rejected.push(RejectedCandidate {
                        source: CoreSource::Bundled,
                        detail: reason,
                    }),
                },
                None => rejected.push(RejectedCandidate {
                    source: CoreSource::Bundled,
                    detail: format!(
                        "运行时目录 {} 不完整（缺少 node 可执行文件或 dsh/lib/bin.js）",
                        root.display()
                    ),
                }),
            }
        }

        // 6. 全部失败：保留逐来源原因，补充可行动建议。
        let mut reasons: Vec<String> = rejected.iter().map(RejectedCandidate::describe).collect();
        if reasons.is_empty() {
            reasons.push(
                "未发现任何 DSH 入口；请全局安装 @deepseek-ai/dsh、执行 pnpm install，\
                 或使用带离线运行时的安装包"
                    .to_string(),
            );
        } else {
            reasons.push(
                "以上来源都不可用；请升级 DSH 到清单要求的最低版本、执行 pnpm install，\
                 或使用带离线运行时的安装包"
                    .to_string(),
            );
        }
        Resolution::Unavailable {
            reasons,
            diagnostics,
        }
    }

    /// 按最低版本基线校验候选。
    ///
    /// 参数：
    /// - `candidate`：待校验候选。
    ///
    /// 返回值：兼容时 `Ok`；否则返回包含原因与入口路径的 `Err`。
    fn gate_candidate(&self, candidate: &CoreCandidate) -> Result<(), String> {
        // 1. 复用统一的版本判定。
        let gate = evaluate(candidate.version_text.as_deref(), self.minimum.as_ref());
        if gate.is_compatible() {
            return Ok(());
        }

        // 2. 拒绝原因带上入口路径，便于定位是哪个安装。
        Err(format!(
            "{}（入口：{}）",
            gate.reason().unwrap_or_else(|| "版本不可用".to_string()),
            candidate.bin.display()
        ))
    }

    /// 返回非捆绑来源使用的 Node 路径。
    ///
    /// 返回值：`DSH_NODE` 显式路径，或交给 PATH 解析的 `node`。
    fn system_node(&self) -> PathBuf {
        // 1. 显式覆盖必须存在，否则保持原样让 spawn 报出可读错误。
        self.node_override
            .clone()
            .map(|path| normalize_existing_path(&path))
            .unwrap_or_else(|| PathBuf::from("node"))
    }
}

/// 读取非空环境变量为路径。
///
/// 参数：
/// - `name`：环境变量名。
///
/// 返回值：变量存在且去空白后非空时返回 `PathBuf`；否则 `None`。
fn env_path(name: &str) -> Option<PathBuf> {
    // 1. 空字符串等价于未设置，避免命令行误传空值。
    env::var_os(name)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

/// 把已存在的路径归一化为不带 Windows verbatim 前缀的绝对路径。
///
/// 参数：
/// - `path`：已存在的文件或目录路径。
///
/// 返回值：`dunce::canonicalize` 成功时返回归一化路径；失败时保留原路径。
/// Node 不接受 `\\?\D:\...` 形式作为脚本参数，因此必须提前去掉前缀。
pub fn normalize_existing_path(path: &Path) -> PathBuf {
    // 1. 使用 dunce 兼容 UNC 与超长路径，同时去掉 verbatim 前缀。
    dunce::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

/// 收集项目本地可能的 DSH 安装前缀。
///
/// 返回值：当前工作目录的 3 级祖先 + 编译期仓库根；调用方按顺序探测
/// `node_modules/@deepseek-ai/dsh`，不保证目录真的存在。
fn local_install_roots() -> Vec<PathBuf> {
    // 1. 兼容从仓库根或任意子目录启动。
    let mut roots = Vec::new();
    if let Ok(current_dir) = env::current_dir() {
        roots.extend(current_dir.ancestors().take(3).map(Path::to_path_buf));
    }

    // 2. 仅开发构建补充编译期仓库根，避免发布包探测开发机路径。
    if cfg!(debug_assertions) {
        if let Some(project_root) = Path::new(env!("CARGO_MANIFEST_DIR")).parent() {
            roots.push(project_root.to_path_buf());
        }
    }
    roots
}

/// 收集全局 DSH 可能的安装前缀。
///
/// 返回值：PATH 中存在 dsh shim 的目录 + npm/pnpm 常见全局目录；只生成
/// 候选，不保证包存在。
fn global_install_roots() -> Vec<PathBuf> {
    // 1. 只把实际存在 dsh shim 的 PATH 目录作为候选。
    let mut roots = Vec::new();
    if let Some(path) = env::var_os("PATH") {
        for directory in env::split_paths(&path) {
            let has_shim = ["dsh", "dsh.exe", "dsh.cmd", "dsh.ps1", "dsh.bat"]
                .iter()
                .any(|name| directory.join(name).is_file());
            if has_shim {
                roots.push(directory);
            }
        }
    }

    // 2. 补充 npm/pnpm 默认全局目录，覆盖 GUI 进程 PATH 不完整的情况。
    if let Some(app_data) = env::var_os("APPDATA") {
        roots.push(PathBuf::from(app_data).join("npm"));
    }
    if let Some(home) = env::var_os("HOME") {
        let home = PathBuf::from(home);
        roots.push(home.join(".npm-global").join("lib"));
        roots.push(home.join(".local").join("share").join("pnpm"));
    }
    if let Some(profile) = env::var_os("USERPROFILE") {
        roots.push(
            PathBuf::from(profile)
                .join("AppData")
                .join("Roaming")
                .join("npm"),
        );
    }
    roots
}

/// 根据全局前缀推导 DSH 的 `lib/bin.js` 候选。
///
/// 参数：
/// - `prefix`：PATH shim 目录或 npm 全局 prefix。
///
/// 返回值：Windows 风格 `prefix/node_modules/...` 与 Unix 风格
/// `prefix/../lib/node_modules/...` 两个候选。
fn global_package_candidates(prefix: &Path) -> Vec<PathBuf> {
    // 1. npm prefix 直接把 node_modules 放在 prefix 下。
    // 2. Unix shim 目录是 prefix/bin，模块在 prefix/lib/node_modules 下。
    let mut candidates = vec![prefix.join(DSH_PACKAGE_BIN)];
    if let Some(parent) = prefix.parent() {
        candidates.push(parent.join("lib").join(DSH_PACKAGE_BIN));
    }
    candidates
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 创建唯一临时目录。
    fn temp_dir(name: &str) -> PathBuf {
        // 1. 时间纳秒 + 进程号避免并行测试冲突。
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "inkdsh-source-{name}-{}-{unique}",
            std::process::id()
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// 在 npm 前缀下写入假 DSH 包。
    fn write_npm_prefix(prefix: &Path, version: &str) -> PathBuf {
        // 1. 构造 node_modules/@deepseek-ai/dsh/lib/bin.js。
        let package = prefix.join("node_modules/@deepseek-ai/dsh");
        fs::create_dir_all(package.join("lib")).unwrap();
        fs::write(package.join("lib").join("bin.js"), b"").unwrap();

        // 2. 写入 package.json，版本可为非法文本以覆盖 Unknown 分支。
        fs::write(
            package.join("package.json"),
            format!(r#"{{"name":"@deepseek-ai/dsh","version":"{version}"}}"#),
        )
        .unwrap();
        package.join("lib").join("bin.js")
    }

    /// 在运行时根目录写入假 node/ + dsh/ 布局。
    fn write_runtime_root(root: &Path, version: &str) {
        // 1. node 占位。
        fs::create_dir_all(root.join("node")).unwrap();
        fs::write(
            root.join(if cfg!(windows) {
                "node/node.exe"
            } else {
                "node/node"
            }),
            b"",
        )
        .unwrap();

        // 2. DSH 入口与版本。
        let dsh = root.join("dsh");
        fs::create_dir_all(dsh.join("lib")).unwrap();
        fs::write(dsh.join("lib").join("bin.js"), b"").unwrap();
        fs::write(
            dsh.join("package.json"),
            format!(r#"{{"name":"@deepseek-ai/dsh","version":"{version}"}}"#),
        )
        .unwrap();
    }

    /// 构造最小环境；默认没有任何来源。
    fn environment(appdata: &Path, minimum: Option<&str>) -> RuntimeEnvironment {
        // 1. 托管目录为空，安装来源 blocked。
        RuntimeEnvironment {
            minimum: minimum.map(|text| parse_version(text).unwrap()),
            override_bin: None,
            node_override: None,
            local_roots: Vec::new(),
            managed: ManagedRuntime::new(appdata),
            bundled_root: None,
            global_roots: Vec::new(),
            artifact: ArtifactPlan::Blocked {
                code: "RUNTIME_ARTIFACT_UNCONFIGURED",
                detail: "测试默认未配置正式核心包".to_string(),
            },
            diagnostics: Vec::new(),
        }
    }

    /// 验证 override 最高优先级，且低版本只告警不切换来源。
    #[test]
    fn override_wins_and_only_warns_on_low_version() {
        // 1. 本地与全局都有达标版本，override 指向 0.1.0。
        let appdata = temp_dir("override-appdata");
        let local = temp_dir("override-local");
        write_npm_prefix(&local, "0.2.1");
        let global = temp_dir("override-global");
        write_npm_prefix(&global, "0.2.1");
        let override_dir = temp_dir("override-bin");
        let override_bin = write_npm_prefix(&override_dir, "0.1.0");

        // 2. 断言仍选 Override，并带显式豁免基线告警。
        let mut env = environment(&appdata, Some("0.2.0"));
        env.override_bin = Some(override_bin.clone());
        env.local_roots = vec![local.clone()];
        env.global_roots = vec![global.clone()];
        match env.resolve() {
            Resolution::Use {
                candidate,
                diagnostics,
                ..
            } => {
                assert_eq!(candidate.source, CoreSource::Override);
                assert_eq!(candidate.bin, normalize_existing_path(&override_bin));
                assert!(diagnostics
                    .iter()
                    .any(|line| line.contains("显式覆盖仍使用")));
            }
            other => panic!("应选中 Override，实际 {other:?}"),
        }

        let _ = fs::remove_dir_all(appdata);
        let _ = fs::remove_dir_all(local);
        let _ = fs::remove_dir_all(global);
        let _ = fs::remove_dir_all(override_dir);
    }

    /// 验证 DSH_BIN 指向非 DSH 包时硬失败，而不是把任意脚本当内核。
    #[test]
    fn override_rejects_non_dsh_package() {
        // 1. 构造一个 name 不是 @deepseek-ai/dsh 的普通包。
        let appdata = temp_dir("override-invalid-appdata");
        let package_root = temp_dir("override-invalid-package");
        let package = package_root.join("node_modules/other-tool");
        fs::create_dir_all(package.join("lib")).unwrap();
        let bin = package.join("lib").join("bin.js");
        fs::write(&bin, b"console.log('not dsh')").unwrap();
        fs::write(
            package.join("package.json"),
            r#"{"name":"other-tool","version":"1.0.0"}"#,
        )
        .unwrap();

        // 2. 断言解析硬失败，且原因明确指向入口识别。
        let mut env = environment(&appdata, None);
        env.override_bin = Some(bin);
        match env.resolve() {
            Resolution::Unavailable { reasons, .. } => {
                assert!(reasons
                    .iter()
                    .any(|reason| reason.contains("不是可识别的 DSH 入口")))
            }
            other => panic!("应拒绝非 DSH 包，实际 {other:?}"),
        }

        let _ = fs::remove_dir_all(appdata);
        let _ = fs::remove_dir_all(package_root);
    }

    /// 验证本地低版本被拒绝后回退到全局，并保留拒绝原因。
    #[test]
    fn too_old_local_falls_back_to_global() {
        // 1. 本地 0.1.9，全局 0.2.1，基线 0.2.0。
        let appdata = temp_dir("fallback-appdata");
        let local = temp_dir("fallback-local");
        write_npm_prefix(&local, "0.1.9");
        let global = temp_dir("fallback-global");
        write_npm_prefix(&global, "0.2.1");

        // 2. 断言选 Global，且原因包含最低版本。
        let mut env = environment(&appdata, Some("0.2.0"));
        env.local_roots = vec![local.clone()];
        env.global_roots = vec![global.clone()];
        match env.resolve() {
            Resolution::Use {
                candidate,
                rejected,
                ..
            } => {
                assert_eq!(candidate.source, CoreSource::Global);
                assert_eq!(candidate.version_text.as_deref(), Some("0.2.1"));
                assert!(rejected.iter().any(|item| {
                    item.source == CoreSource::Local && item.detail.contains("低于最低要求")
                }));
            }
            other => panic!("应回退到全局，实际 {other:?}"),
        }

        let _ = fs::remove_dir_all(appdata);
        let _ = fs::remove_dir_all(local);
        let _ = fs::remove_dir_all(global);
    }

    /// 验证版本缺失在配置基线时被拒绝，而不是静默使用。
    #[test]
    fn unknown_version_rejected_when_baseline_configured() {
        // 1. 本地 package.json 缺 version，全局版本合法。
        let appdata = temp_dir("unknown-appdata");
        let local = temp_dir("unknown-local");
        let package = local.join("node_modules/@deepseek-ai/dsh");
        fs::create_dir_all(package.join("lib")).unwrap();
        fs::write(package.join("lib").join("bin.js"), b"").unwrap();
        fs::write(
            package.join("package.json"),
            r#"{"name":"@deepseek-ai/dsh"}"#,
        )
        .unwrap();
        let global = temp_dir("unknown-global");
        write_npm_prefix(&global, "0.2.1");

        // 2. 断言拒绝原因含「无法读取版本」，最终使用全局。
        let mut env = environment(&appdata, Some("0.2.0"));
        env.local_roots = vec![local.clone()];
        env.global_roots = vec![global.clone()];
        match env.resolve() {
            Resolution::Use {
                candidate,
                rejected,
                ..
            } => {
                assert_eq!(candidate.source, CoreSource::Global);
                assert!(rejected
                    .iter()
                    .any(|item| item.detail.contains("无法读取版本")));
            }
            other => panic!("应回退到全局，实际 {other:?}"),
        }

        let _ = fs::remove_dir_all(appdata);
        let _ = fs::remove_dir_all(local);
        let _ = fs::remove_dir_all(global);
    }

    /// 验证全局安装优先于捆绑运行时。
    #[test]
    fn global_precedes_bundled() {
        // 1. 捆绑 0.2.1，全局 0.2.2。
        let appdata = temp_dir("bundled-appdata");
        let bundled = temp_dir("bundled-root");
        write_runtime_root(&bundled, "0.2.1");
        let global = temp_dir("bundled-global");
        write_npm_prefix(&global, "0.2.2");

        // 2. 断言选 Global：用户已有安装优先于随包运行时。
        let mut env = environment(&appdata, Some("0.2.0"));
        env.bundled_root = Some(bundled.clone());
        env.global_roots = vec![global.clone()];
        match env.resolve() {
            Resolution::Use { candidate, .. } => {
                assert_eq!(candidate.source, CoreSource::Global)
            }
            other => panic!("应选中全局 DSH，实际 {other:?}"),
        }

        let _ = fs::remove_dir_all(appdata);
        let _ = fs::remove_dir_all(bundled);
        let _ = fs::remove_dir_all(global);
    }

    /// 验证全局安装优先于托管装配：有可用全局 DSH 时不应触发下载。
    #[test]
    fn global_precedes_managed_install() {
        // 1. 空托管目录 + 可装配 fixture + 达标全局安装。
        let appdata = temp_dir("global-managed-appdata");
        let fixture = temp_dir("global-managed-fixture");
        fs::create_dir_all(fixture.join("node")).unwrap();
        fs::create_dir_all(fixture.join("dsh/lib")).unwrap();
        fs::write(fixture.join("dsh/lib/bin.js"), b"").unwrap();
        fs::write(fixture.join("dsh/package.json"), r#"{"version":"0.2.1"}"#).unwrap();
        let global = temp_dir("global-managed-global");
        write_npm_prefix(&global, "0.2.1");

        // 2. 断言直接使用 Global，不返回 InstallManaged。
        let mut env = environment(&appdata, Some("0.2.0"));
        env.global_roots = vec![global.clone()];
        env.artifact = ArtifactPlan::Ready(ArtifactSource::Directory {
            path: fixture.clone(),
        });
        match env.resolve() {
            Resolution::Use { candidate, .. } => {
                assert_eq!(candidate.source, CoreSource::Global)
            }
            other => panic!("应优先使用全局 DSH，实际 {other:?}"),
        }

        let _ = fs::remove_dir_all(appdata);
        let _ = fs::remove_dir_all(fixture);
        let _ = fs::remove_dir_all(global);
    }

    /// 验证托管缺失且安装来源可用时请求装配。
    #[test]
    fn requests_managed_install_when_absent_and_artifact_ready() {
        // 1. 空 AppData + 目录夹具。
        let appdata = temp_dir("install-appdata");
        let fixture = temp_dir("install-fixture");
        let mut env = environment(&appdata, Some("0.2.0"));
        env.artifact = ArtifactPlan::Ready(ArtifactSource::Directory {
            path: fixture.clone(),
        });

        // 2. 断言返回 InstallManaged，且未偷偷使用夹具目录。
        match env.resolve() {
            Resolution::InstallManaged { artifact, .. } => {
                assert!(matches!(artifact, ArtifactSource::Directory { .. }))
            }
            other => panic!("应请求安装，实际 {other:?}"),
        }
        assert!(env.managed.probe().is_none());

        let _ = fs::remove_dir_all(appdata);
        let _ = fs::remove_dir_all(fixture);
    }

    /// 验证 DSH_BIN 指向不存在的文件时硬失败，不回退。
    #[test]
    fn missing_override_fails_without_fallback() {
        // 1. 全局可用，但 DSH_BIN 指向不存在的路径。
        let appdata = temp_dir("missing-override-appdata");
        let global = temp_dir("missing-override-global");
        write_npm_prefix(&global, "0.2.1");
        let mut env = environment(&appdata, None);
        env.override_bin = Some(appdata.join("does-not-exist").join("lib").join("bin.js"));
        env.global_roots = vec![global.clone()];

        // 2. 断言 Unavailable 且原因明确指向 DSH_BIN。
        match env.resolve() {
            Resolution::Unavailable { reasons, .. } => {
                assert!(reasons.iter().any(|reason| reason.contains("DSH_BIN")))
            }
            other => panic!("应硬失败，实际 {other:?}"),
        }

        let _ = fs::remove_dir_all(appdata);
        let _ = fs::remove_dir_all(global);
    }

    /// 验证所有来源缺失时给出可行动建议。
    #[test]
    fn unavailable_reports_actionable_hint() {
        // 1. 空环境、未配置安装来源。
        let appdata = temp_dir("empty-appdata");
        let env = environment(&appdata, Some("0.2.0"));
        match env.resolve() {
            Resolution::Unavailable { reasons, .. } => {
                assert!(reasons.iter().any(|reason| reason.contains("pnpm install")));
                assert!(reasons
                    .iter()
                    .any(|reason| reason.contains("RUNTIME_ARTIFACT_UNCONFIGURED")));
            }
            other => panic!("应返回不可用，实际 {other:?}"),
        }

        let _ = fs::remove_dir_all(appdata);
    }

    /// 验证托管路径一律位于注入的 AppData 下，便于隔离测试。
    #[test]
    fn managed_paths_stay_under_injected_appdata() {
        // 1. 只注入临时 AppData。
        let appdata = temp_dir("paths-appdata");
        let env = environment(&appdata, None);
        assert_eq!(
            env.managed.root(),
            appdata.join(super::super::install::MANAGED_DIR_NAME)
        );
        assert!(env.managed.root().join("node").starts_with(&appdata));
        assert!(env
            .managed
            .root()
            .join("dsh/lib/bin.js")
            .starts_with(&appdata));

        // 2. 与真实用户目录无关（不读取 HOME/DSH_HOME）。
        let home = env::var_os("USERPROFILE").map(PathBuf::from);
        if let Some(home) = home {
            assert!(!env.managed.root().starts_with(home.join(".dsh")));
        }

        let _ = fs::remove_dir_all(appdata);
    }

    /// 验证 `runtime_layout` 对缺失文件的判定。
    #[test]
    fn runtime_layout_requires_both_entrypoints() {
        // 1. 空目录不完整。
        let root = temp_dir("layout-root");
        assert!(runtime_layout(&root).is_none());

        // 2. 只补 node 仍不完整。
        fs::create_dir_all(root.join("node")).unwrap();
        fs::write(
            root.join(if cfg!(windows) {
                "node/node.exe"
            } else {
                "node/node"
            }),
            b"",
        )
        .unwrap();
        assert!(runtime_layout(&root).is_none());

        // 3. 补齐 bin.js 后通过。
        fs::create_dir_all(root.join("dsh/lib")).unwrap();
        fs::write(root.join("dsh/lib/bin.js"), b"").unwrap();
        assert!(runtime_layout(&root).is_some());

        let _ = fs::remove_dir_all(root);
    }

    /// 验证 package.json 版本读取容忍损坏与缺失。
    #[test]
    fn reads_package_version_safely() {
        // 1. 正常版本与 v 前缀。
        let dir = temp_dir("version-read");
        fs::write(dir.join("package.json"), r#"{"version":"v0.2.1-rc.2"}"#).unwrap();
        assert_eq!(read_package_version(&dir).as_deref(), Some("v0.2.1-rc.2"));

        // 2. JSON 损坏或字段缺失返回 None。
        fs::write(dir.join("package.json"), "{broken").unwrap();
        assert_eq!(read_package_version(&dir), None);
        fs::write(dir.join("package.json"), r#"{"name":"x"}"#).unwrap();
        assert_eq!(read_package_version(&dir), None);

        let _ = fs::remove_dir_all(dir);
    }
}
