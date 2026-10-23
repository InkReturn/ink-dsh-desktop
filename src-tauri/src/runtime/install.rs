//! 托管运行时（AppData）的路径约定、安装器与原子切换。
//!
//! 安装器只认两种已实现制品：本地目录（开发/测试 fixture）和带 SHA-256 的
//! zip 归档。正式的 HTTPS 下载地址与摘要尚未确定，因此远程制品会明确返回
//! blocked-by-env，不会伪造下载结果或跳过完整性校验。

use std::{
    fs::{self, File},
    io::{self, Read},
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use semver::Version;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::{
    source::{read_package_version, runtime_layout, CoreCandidate, CoreSource},
    version::evaluate,
    InstallStage, RuntimeError,
};

/// 托管运行时在应用数据目录下的子目录名。
pub const MANAGED_DIR_NAME: &str = "runtime";

/// 运行时标记文件名，记录本次装配的版本与摘要。
pub const RUNTIME_MARKER: &str = "runtime.json";

/// 装配 staging 目录前缀，每次安装唯一。
const STAGING_PREFIX: &str = ".staging-";

/// 原子切换时的备份目录前缀。
const BACKUP_PREFIX: &str = ".backup-";

/// 解压条目上限，防止损坏/恶意归档耗尽 AppData 目录项。
const MAX_ARCHIVE_ENTRIES: usize = 200_000;

/// 解压后总字节上限（8 GiB），远高于 Node + DSH 的实际体积。
const MAX_EXTRACTED_BYTES: u64 = 8 * 1024 * 1024 * 1024;

/// 开发/测试夹具指向运行时目录或 zip 的环境变量（仅调试构建读取）。
pub const ENV_RUNTIME_ARTIFACT: &str = "INKDSH_RUNTIME_ARTIFACT";

/// 开发/测试夹具 zip 的 SHA-256 环境变量（仅调试构建读取）。
pub const ENV_RUNTIME_ARTIFACT_SHA256: &str = "INKDSH_RUNTIME_ARTIFACT_SHA256";

/// 托管运行时标记的内容。
#[derive(Debug, Clone, Serialize, Deserialize)]
struct RuntimeMarker {
    /// 标记结构版本，未来迁移时用于识别旧格式。
    schema: u32,
    /// 本次装配的 DSH 版本。
    dsh: Option<String>,
    /// 装配完成时间（Unix 秒），仅用于诊断。
    installed_at_unix: u64,
    /// 制品 SHA-256；目录 fixture 没有摘要时为 `None`。
    sha256: Option<String>,
}

/// 托管运行时的根目录包装。
#[derive(Debug, Clone)]
pub struct ManagedRuntime {
    /// `<AppData>/runtime`。
    root: PathBuf,
}

impl ManagedRuntime {
    /// 基于应用数据目录构造托管运行时。
    ///
    /// 参数：
    /// - `app_data_dir`：Tauri 的 `app_data_dir`；测试传入临时目录即可完全
    ///   隔离，不会触碰真实用户目录。
    pub fn new(app_data_dir: &Path) -> Self {
        // 1. 固定使用 `runtime/` 子目录，与 resources 的捆绑布局保持同名。
        Self {
            root: app_data_dir.join(MANAGED_DIR_NAME),
        }
    }

    /// 返回托管根目录。
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// 探测托管运行时是否已可用。
    ///
    /// 返回值：布局完整时返回 `Managed` 来源候选（版本可能为 `None`）；目录
    /// 不完整返回 `None`。该函数只读文件系统，不启动任何进程。
    pub fn probe(&self) -> Option<CoreCandidate> {
        // 1. 复用统一的布局检查，保证与捆绑运行时同一套判定。
        CoreCandidate::from_runtime_root(CoreSource::Managed, &self.root)
    }

    /// 安装或替换托管运行时。
    ///
    /// 参数：
    /// - `artifact`：本地目录、带摘要的 zip 或远程制品描述。
    /// - `minimum`：清单声明的最低 DSH 版本，用于提交前拒绝旧核心。
    /// - `progress`：阶段回调，参数为阶段与细节；调用方负责写日志/UI 状态。
    ///
    /// 返回值：切换完成后重新探测到的 `Managed` 候选。
    ///
    /// 副作用：写 staging 目录、读取制品、最终替换 `root`；失败时清理本轮
    /// staging，保留切换前的旧运行时。
    pub fn install(
        &self,
        artifact: &ArtifactSource,
        minimum: Option<&Version>,
        progress: &mut dyn FnMut(InstallStage, String),
    ) -> Result<CoreCandidate, RuntimeError> {
        // 1. 远程制品尚未接入传输层，明确返回 blocked-by-env。
        if let ArtifactSource::Remote {
            url,
            size_bytes,
            file_name,
            ..
        } = artifact
        {
            let size = size_bytes
                .map(|value| format!("{value} 字节"))
                .unwrap_or_else(|| "大小未知".to_string());
            return Err(RuntimeError::blocked(
                "RUNTIME_DOWNLOAD_BLOCKED",
                format!(
                    "正式核心包 {file_name}（{url}，{size}）的下载传输层尚未接入\
                     （P0-2 blocked-by-env），本轮只提供清单、fixture 与安装状态机"
                ),
            ));
        }

        // 2. 准备父目录、清扫上次残留并处理中断的切换。
        self.prepare_root()?;

        // 3. 在根目录旁边创建唯一 staging。必须与根目录同级：激活时会把
        //    旧根目录整体改名为备份，staging 若在根目录内部会随之消失。
        let parent = self.root.parent().unwrap_or_else(|| Path::new("."));
        let staging = parent.join(format!("{STAGING_PREFIX}{}", unique_suffix()));
        let result = self.stage_and_activate(&staging, artifact, minimum, progress);
        if result.is_err() {
            let _ = fs::remove_dir_all(&staging);
        }
        result
    }

    /// 在来源探测前恢复中断的托管切换。
    ///
    /// 返回值：执行了恢复或清理时返回一句诊断；无需处理时返回 `None`。
    ///
    /// 副作用：清理 `.staging-*`；根目录缺失且存在 `.backup-*` 时，把最新
    /// 备份 rename 回根目录；根目录存在时清理陈旧备份。该方法是幂等的，
    /// 由调用方在 `probe` 之前执行，避免崩溃窗口把旧运行时永久搁置。
    pub fn recover_interrupted_activation(&self) -> Option<String> {
        // 1. 父目录不存在说明从未安装过，直接返回。
        let parent = self.root.parent()?;
        let entries = fs::read_dir(parent).ok()?;

        // 2. 收集 staging 与备份（备份带修改时间，用于选择最新一份）。
        let mut staging: Vec<PathBuf> = Vec::new();
        let mut backups: Vec<(SystemTime, PathBuf)> = Vec::new();
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            let path = entry.path();
            if name.starts_with(STAGING_PREFIX) {
                staging.push(path);
            } else if name.starts_with(BACKUP_PREFIX) {
                let modified = entry
                    .metadata()
                    .and_then(|metadata| metadata.modified())
                    .unwrap_or(UNIX_EPOCH);
                backups.push((modified, path));
            }
        }

        // 3. 清理所有 staging；它们只可能是上次失败/中断的临时产物。
        let mut acted = false;
        for path in staging {
            if fs::remove_dir_all(&path).is_ok() {
                acted = true;
            }
        }

        // 4. 根目录缺失：按修改时间从新到旧恢复一份备份；恢复失败时保留备份
        //    等下一次启动重试，绝不删除唯一可恢复的副本。
        backups.sort_by_key(|left| left.0);
        let mut message = None;
        let mut preserve_backups = false;
        if !self.root.exists() {
            if let Some((_, latest)) = backups.pop() {
                match fs::rename(&latest, &self.root) {
                    Ok(()) => {
                        acted = true;
                        message = Some(format!(
                            "检测到中断的运行时切换，已从备份恢复：{}",
                            self.root.display()
                        ));
                    }
                    Err(error) => {
                        preserve_backups = true;
                        message = Some(format!("检测到中断的运行时切换，但恢复备份失败：{error}"));
                        backups.push((UNIX_EPOCH, latest));
                    }
                }
            }
        }

        // 5. 清理多余/陈旧备份，仅保留恢复失败时需要重试的那一份。
        if !preserve_backups {
            for (_, stale) in &backups {
                let _ = fs::remove_dir_all(stale);
            }
        }

        // 6. 只清理未恢复时也给出一句日志，便于确认状态。
        if message.is_none() && acted {
            message = Some(format!("已清理托管运行时残留目录：{}", parent.display()));
        }
        message
    }

    /// 准备托管根目录的父目录并处理中断切换。
    ///
    /// 返回值：创建父目录失败时返回 IO 错误；恢复/清理失败只记诊断，由
    /// 调用方通过日志观察，不阻断安装。
    fn prepare_root(&self) -> Result<(), RuntimeError> {
        // 1. 只创建父目录；根目录由激活步骤原子改名产生。
        let parent = self.root.parent().unwrap_or_else(|| Path::new("."));
        fs::create_dir_all(parent).map_err(|source| RuntimeError::Io {
            context: format!("创建应用数据目录 {}", parent.display()),
            source,
        })?;

        // 2. 复用启动期的恢复逻辑，保证安装前不会覆盖可恢复的旧运行时。
        let _ = self.recover_interrupted_activation();
        Ok(())
    }

    /// 执行 staging 的填充、校验与原子切换。
    ///
    /// 参数：
    /// - `staging`：本轮唯一 staging 路径。
    /// - `artifact`：安装来源。
    /// - `minimum`：最低版本基线。
    /// - `progress`：阶段回调。
    fn stage_and_activate(
        &self,
        staging: &Path,
        artifact: &ArtifactSource,
        minimum: Option<&Version>,
        progress: &mut dyn FnMut(InstallStage, String),
    ) -> Result<CoreCandidate, RuntimeError> {
        // 1. 按来源落地文件：目录复制或 zip 解压。
        match artifact {
            ArtifactSource::Directory { path } => {
                progress(
                    InstallStage::Verify,
                    format!("校验本地运行时目录 {}", path.display()),
                );
                if runtime_layout(path).is_none() {
                    return Err(RuntimeError::Layout {
                        path: path.clone(),
                        missing: "node 可执行文件或 dsh/lib/bin.js".to_string(),
                    });
                }
                progress(
                    InstallStage::Extract,
                    "复制运行时文件到托管目录".to_string(),
                );
                copy_tree(path, staging).map_err(|source| RuntimeError::Io {
                    context: format!("复制运行时目录到 {}", staging.display()),
                    source,
                })?;
            }
            ArtifactSource::Archive {
                path,
                sha256,
                file_name,
            } => {
                progress(InstallStage::Verify, format!("校验 {file_name} 的 SHA-256"));
                verify_sha256(path, sha256)?;
                progress(InstallStage::Extract, format!("解压 {file_name}"));
                extract_zip(path, staging)?;
            }
            ArtifactSource::Remote { .. } => {
                return Err(RuntimeError::blocked(
                    "RUNTIME_DOWNLOAD_BLOCKED",
                    "远程制品路径不应进入 staging".to_string(),
                ));
            }
        }

        // 2. 校验 staging 布局与 DSH 版本，不满足基线就放弃本轮安装。
        let layout = runtime_layout(staging).ok_or_else(|| RuntimeError::Layout {
            path: staging.to_path_buf(),
            missing: "node 可执行文件或 dsh/lib/bin.js".to_string(),
        })?;
        let version = read_package_version(&layout.dsh_root);
        let gate = evaluate(version.as_deref(), minimum);
        if let Some(reason) = gate.reason() {
            return Err(RuntimeError::Version {
                detail: format!("待安装的托管运行时不可用：{reason}"),
            });
        }

        // 3. 写标记，记录版本与制品摘要，便于下次诊断。
        let marker = RuntimeMarker {
            schema: 1,
            dsh: version.clone(),
            installed_at_unix: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|duration| duration.as_secs())
                .unwrap_or_default(),
            sha256: artifact.sha256().map(ToOwned::to_owned),
        };
        write_marker(staging, &marker)?;

        // 4. 原子切换到托管根目录。
        progress(InstallStage::Activate, "切换到托管运行时目录".to_string());
        self.activate(staging)?;

        // 5. 以最终目录重新探测，确保返回的候选与磁盘一致。
        self.probe().ok_or_else(|| RuntimeError::Layout {
            path: self.root.clone(),
            missing: "切换后的 runtime/node 或 runtime/dsh/lib/bin.js".to_string(),
        })
    }

    /// 把 staging 原子切换为托管根目录。
    ///
    /// 参数：
    /// - `staging`：已校验完成的 staging 目录。
    ///
    /// 副作用：可能先把旧根目录重命名为备份；切换失败时回滚备份。成功后
    /// 尽力删除备份，删除失败不影响结果。
    fn activate(&self, staging: &Path) -> Result<(), RuntimeError> {
        // 1. 旧目录不存在时直接改名。
        if !self.root.exists() {
            return fs::rename(staging, &self.root).map_err(|source| RuntimeError::Io {
                context: format!("把 {} 切换为 {}", staging.display(), self.root.display()),
                source,
            });
        }

        // 2. 旧目录存在：先移到备份，再切换；失败时把备份改回原名。
        let backup = self
            .root
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join(format!("{BACKUP_PREFIX}{}", unique_suffix()));
        fs::rename(&self.root, &backup).map_err(|source| RuntimeError::Io {
            context: format!("备份旧托管运行时 {}", self.root.display()),
            source,
        })?;
        if let Err(source) = fs::rename(staging, &self.root) {
            let _ = fs::rename(&backup, &self.root);
            return Err(RuntimeError::Io {
                context: format!("启用新托管运行时 {}", self.root.display()),
                source,
            });
        }

        // 3. 成功后尽力清理备份；失败只留下可被下次启动恢复的目录。
        let _ = fs::remove_dir_all(&backup);
        Ok(())
    }
}

/// 安装来源。
#[derive(Debug, Clone)]
pub enum ArtifactSource {
    /// 本地运行时目录（开发/测试 fixture，要求布局与捆绑运行时一致）。
    Directory {
        /// 已就绪的运行时根目录。
        path: PathBuf,
    },
    /// 带 SHA-256 的 zip 归档。
    Archive {
        /// 归档文件路径。
        path: PathBuf,
        /// 期望摘要（规范化后的 64 位小写十六进制）。
        sha256: String,
        /// 文件名，仅用于日志与进度文案。
        file_name: String,
    },
    /// 远程制品；地址与摘要齐备，但下载传输层尚未接入。
    Remote {
        /// HTTPS 下载地址。
        url: String,
        /// 期望摘要。
        sha256: String,
        /// 期望字节数，仅用于诊断。
        size_bytes: Option<u64>,
        /// 文件名。
        file_name: String,
    },
}

impl ArtifactSource {
    /// 依据清单与调试夹具选择安装来源。
    ///
    /// 参数：
    /// - `descriptor`：清单中的 `core` 描述。
    /// - `dev_fixture`：调试构建读取到的 `INKDSH_RUNTIME_ARTIFACT`；测试可
    ///   直接传入临时目录或 zip 路径。
    /// - `dev_fixture_sha256`：调试 zip 夹具的期望摘要；目录夹具不需要。
    ///
    /// 返回值：可用来源；正式 URL/摘要缺失、格式非法或 zip 夹具缺摘要时
    /// 返回结构化错误（多为 blocked-by-env / integrity），不会猜测摘要。
    pub fn from_descriptor(
        descriptor: &super::manifest::CoreDescriptor,
        dev_fixture: Option<PathBuf>,
        dev_fixture_sha256: Option<String>,
    ) -> Result<Self, RuntimeError> {
        // 1. 调试夹具优先：目录直接使用，zip 必须提供摘要。
        if let Some(path) = dev_fixture {
            if path.is_dir() {
                return Ok(Self::Directory { path });
            }
            if !path.is_file() {
                return Err(RuntimeError::blocked(
                    "RUNTIME_FIXTURE_MISSING",
                    format!(
                        "INKDSH_RUNTIME_ARTIFACT 指向的路径不存在：{}",
                        path.display()
                    ),
                ));
            }
            let Some(sha) = dev_fixture_sha256 else {
                return Err(RuntimeError::integrity(
                    "RUNTIME_FIXTURE_SHA_MISSING",
                    "zip 夹具必须同时设置 INKDSH_RUNTIME_ARTIFACT_SHA256",
                ));
            };
            return Ok(Self::Archive {
                path: path.clone(),
                sha256: normalize_sha256(&sha)?,
                file_name: file_name_of(&path),
            });
        }

        // 2. 正式清单：URL 与摘要必须同时存在。
        let url = descriptor.url.as_deref().unwrap_or("").trim();
        let sha = descriptor.sha256.as_deref().unwrap_or("").trim();
        if url.is_empty() {
            return Err(RuntimeError::blocked(
                "RUNTIME_ARTIFACT_UNCONFIGURED",
                "清单 core.url 未配置：正式核心包发布地址尚未确定（P0-2 blocked-by-env）",
            ));
        }
        if sha.is_empty() {
            return Err(RuntimeError::blocked(
                "RUNTIME_INTEGRITY_UNCONFIGURED",
                "清单 core.sha256 未配置：不能在没有摘要的情况下安装核心包",
            ));
        }
        if !url.starts_with("https://") {
            return Err(RuntimeError::blocked(
                "RUNTIME_ARTIFACT_UNTRUSTED_URL",
                format!("核心包地址必须是 https：{url}"),
            ));
        }

        // 3. 文件名优先取清单字段，否则从 URL 末段推导；当前只支持 zip。
        let file_name = descriptor
            .file_name
            .as_deref()
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .map(ToOwned::to_owned)
            .unwrap_or_else(|| file_name_of(Path::new(url)));
        if !file_name.to_ascii_lowercase().ends_with(".zip") {
            return Err(RuntimeError::blocked(
                "RUNTIME_ARCHIVE_UNSUPPORTED",
                format!("当前只支持 zip 核心包：{file_name}"),
            ));
        }

        Ok(Self::Remote {
            url: url.to_string(),
            sha256: normalize_sha256(sha)?,
            size_bytes: descriptor.size_bytes,
            file_name,
        })
    }

    /// 返回制品摘要（仅归档/远程来源）。
    fn sha256(&self) -> Option<&str> {
        match self {
            Self::Directory { .. } => None,
            Self::Archive { sha256, .. } | Self::Remote { sha256, .. } => Some(sha256),
        }
    }
}

/// 规范化 SHA-256 文本。
///
/// 参数：
/// - `raw`：期望摘要，允许前后空白与 `sha256:` 前缀，大小写不敏感。
///
/// 返回值：64 位小写十六进制；长度或字符不合法时返回 integrity 错误，避免
/// 把空摘要或短摘要当成有效校验。
pub fn normalize_sha256(raw: &str) -> Result<String, RuntimeError> {
    // 1. 去空白与可选前缀。
    let trimmed = raw.trim();
    let without_prefix = trimmed
        .strip_prefix("sha256:")
        .or_else(|| trimmed.strip_prefix("SHA256:"))
        .unwrap_or(trimmed);
    let normalized = without_prefix.trim().to_ascii_lowercase();

    // 2. 必须是 64 位十六进制。
    if normalized.len() != 64 || !normalized.chars().all(|ch| ch.is_ascii_hexdigit()) {
        return Err(RuntimeError::integrity(
            "RUNTIME_INTEGRITY_METADATA_INVALID",
            format!("SHA-256 必须是 64 位十六进制，实际为 {raw:?}"),
        ));
    }
    Ok(normalized)
}

/// 计算文件的 SHA-256。
///
/// 参数：
/// - `path`：待计算文件；不存在或不可读时返回 IO 错误。
///
/// 返回值：64 位小写十六进制摘要。按 64 KiB 分块读取，避免大制品整文件
/// 载入内存。
pub fn sha256_file(path: &Path) -> Result<String, RuntimeError> {
    // 1. 打开文件并准备哈希器。
    let mut file = File::open(path).map_err(|source| RuntimeError::Io {
        context: format!("打开待校验文件 {}", path.display()),
        source,
    })?;
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];

    // 2. 分块读取并更新摘要，直到 EOF。
    loop {
        let read = file.read(&mut buffer).map_err(|source| RuntimeError::Io {
            context: format!("读取待校验文件 {}", path.display()),
            source,
        })?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }

    // 3. 输出小写十六进制。
    let digest = hasher.finalize();
    let mut hex = String::with_capacity(64);
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(hex, "{byte:02x}");
    }
    Ok(hex)
}

/// 校验文件摘要。
///
/// 参数：
/// - `path`：待校验文件。
/// - `expected`：期望摘要，允许 `sha256:` 前缀。
///
/// 返回值：一致时 `Ok`；不一致返回 integrity 错误，包含期望值与实际值。
pub fn verify_sha256(path: &Path, expected: &str) -> Result<(), RuntimeError> {
    // 1. 先规范化期望值，再计算实际值。
    let expected = normalize_sha256(expected)?;
    let actual = sha256_file(path)?;
    if actual != expected {
        return Err(RuntimeError::integrity(
            "RUNTIME_INTEGRITY_MISMATCH",
            format!(
                "{} 的 SHA-256 不一致：期望 {expected}，实际 {actual}",
                path.display()
            ),
        ));
    }
    // 2. 一致时返回成功。
    Ok(())
}

/// 递归复制目录内容到目标目录。
///
/// 参数：
/// - `from`：源目录，必须存在。
/// - `to`：目标目录，不存在时创建。
///
/// 返回值：底层 IO 结果。符号链接直接拒绝，避免把链接目标或用户目录带进
/// 托管区；Windows 备用数据流名（含 `:`）同样拒绝。
fn copy_tree(from: &Path, to: &Path) -> io::Result<()> {
    // 1. 确保目标目录存在。
    fs::create_dir_all(to)?;

    // 2. 遍历源条目，目录递归、文件复制。
    for entry in fs::read_dir(from)? {
        let entry = entry?;
        let source = entry.path();
        let name = entry.file_name();
        let name_text = name.to_string_lossy();
        let file_type = entry.file_type()?;

        // 3. 拒绝符号链接与 ADS 名称，防止复制出 staging 之外的内容。
        if file_type.is_symlink() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("拒绝复制符号链接：{}", source.display()),
            ));
        }
        if name_text.contains(':') {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("拒绝复制包含备用数据流的文件：{}", source.display()),
            ));
        }

        let destination = to.join(&name);
        if file_type.is_dir() {
            copy_tree(&source, &destination)?;
        } else {
            fs::copy(&source, &destination)?;
        }
    }
    Ok(())
}

/// 把 zip 归档解压到目标目录。
///
/// 参数：
/// - `archive`：zip 文件路径。
/// - `destination`：解压目标；允许不存在。
///
/// 返回值：解压成功返回 `Ok`；路径穿越、损坏归档或 IO 失败返回结构化错误。
/// 副作用：写 `destination` 下的文件与目录。
fn extract_zip(archive: &Path, destination: &Path) -> Result<(), RuntimeError> {
    // 1. 打开归档并创建目标目录。
    let file = File::open(archive).map_err(|source| RuntimeError::Io {
        context: format!("打开核心包 {}", archive.display()),
        source,
    })?;
    let mut zip = zip::ZipArchive::new(file).map_err(|error| {
        RuntimeError::integrity(
            "RUNTIME_ARCHIVE_INVALID",
            format!("{} 不是合法 zip：{error}", archive.display()),
        )
    })?;
    fs::create_dir_all(destination).map_err(|source| RuntimeError::Io {
        context: format!("创建解压目录 {}", destination.display()),
        source,
    })?;

    // 2. 先做条目数上限检查，避免每个条目单独建文件耗尽目录。
    if zip.len() > MAX_ARCHIVE_ENTRIES {
        return Err(RuntimeError::integrity(
            "RUNTIME_ARCHIVE_TOO_LARGE",
            format!(
                "{} 条目数 {} 超过上限 {MAX_ARCHIVE_ENTRIES}",
                archive.display(),
                zip.len()
            ),
        ));
    }

    // 3. 逐条目解压；`enclosed_name` 拒绝绝对路径与 `..` 穿越。
    let mut extracted_bytes = 0u64;
    for index in 0..zip.len() {
        let mut entry = zip.by_index(index).map_err(|error| {
            RuntimeError::integrity(
                "RUNTIME_ARCHIVE_INVALID",
                format!("读取第 {index} 个归档条目失败：{error}"),
            )
        })?;
        let Some(relative) = entry.enclosed_name() else {
            return Err(RuntimeError::integrity(
                "RUNTIME_ARCHIVE_PATH_ESCAPE",
                format!("归档条目路径逃逸：{}", entry.name()),
            ));
        };
        let target = destination.join(&relative);

        // 4. 目录条目只建目录；文件条目建父目录后流式写入。
        if entry.is_dir() {
            fs::create_dir_all(&target).map_err(|source| RuntimeError::Io {
                context: format!("创建解压子目录 {}", target.display()),
                source,
            })?;
            continue;
        }
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent).map_err(|source| RuntimeError::Io {
                context: format!("创建解压父目录 {}", parent.display()),
                source,
            })?;
        }
        let mut output = File::create(&target).map_err(|source| RuntimeError::Io {
            context: format!("创建解压文件 {}", target.display()),
            source,
        })?;
        let written = io::copy(&mut entry, &mut output).map_err(|source| RuntimeError::Io {
            context: format!("写出解压文件 {}", target.display()),
            source,
        })?;

        // 5. 累计实际写出字节数，防止声明大小造假的 zip bomb。
        extracted_bytes = extracted_bytes.saturating_add(written);
        if extracted_bytes > MAX_EXTRACTED_BYTES {
            return Err(RuntimeError::integrity(
                "RUNTIME_ARCHIVE_TOO_LARGE",
                format!(
                    "{} 解压总字节超过上限 {MAX_EXTRACTED_BYTES}",
                    archive.display()
                ),
            ));
        }
    }

    // 4. Unix 上补齐 node 可执行权限，否则解压后的运行时无法 spawn。
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let node = destination.join("node").join("node");
        if node.is_file() {
            let _ = fs::set_permissions(&node, fs::Permissions::from_mode(0o755));
        }
    }

    Ok(())
}

/// 写入运行时标记文件。
///
/// 参数：
/// - `staging`：正在装配的目录。
/// - `marker`：要记录的版本与摘要。
///
/// 副作用：创建 `<staging>/runtime.json`；失败返回 IO 错误并交由调用方清理。
fn write_marker(staging: &Path, marker: &RuntimeMarker) -> Result<(), RuntimeError> {
    // 1. 序列化为带缩进的 JSON，便于人工读取。
    let text = serde_json::to_string_pretty(marker).map_err(|error| RuntimeError::Io {
        context: "序列化运行时标记".to_string(),
        source: io::Error::other(error),
    })?;

    // 2. 写入 staging；切换成功后随目录一起生效。
    let path = staging.join(RUNTIME_MARKER);
    fs::write(&path, text).map_err(|source| RuntimeError::Io {
        context: format!("写入运行时标记 {}", path.display()),
        source,
    })
}

/// 从路径末尾取文件名，供日志与清单缺省值使用。
///
/// 参数：
/// - `path`：可能来自 URL 或本地路径。
///
/// 返回值：最后一个非空路径段；无法提取时返回 `core.zip`。
fn file_name_of(path: &Path) -> String {
    // 1. URL 与 Windows 路径都用 `/` 或 `\` 分隔，先统一再取末段。
    let text = path.to_string_lossy();
    let name = text
        .split(['/', '\\'])
        .rfind(|segment| !segment.is_empty())
        .unwrap_or("core.zip");
    name.to_string()
}

/// 生成 staging/备份目录的唯一后缀。
///
/// 返回值：`<进程号>-<纳秒>`；时间获取失败时退化为进程号。
fn unique_suffix() -> String {
    // 1. 纳秒时间戳足以避免同一进程内的并发冲突。
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    format!("{}-{nanos}", std::process::id())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::manifest::CoreDescriptor;
    use std::io::Write as _;

    /// 创建唯一临时目录。
    fn temp_dir(name: &str) -> PathBuf {
        // 1. 时间纳秒 + 名称避免并行测试冲突。
        let dir = std::env::temp_dir().join(format!(
            "inkdsh-install-{name}-{}-{}",
            std::process::id(),
            unique_suffix()
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// 在目录中写入一份可识别的假运行时布局。
    fn write_runtime_tree(root: &Path, version: &str) {
        // 1. node 可执行文件占位（测试不实际执行）。
        fs::create_dir_all(root.join("node")).unwrap();
        fs::write(
            root.join("node")
                .join(if cfg!(windows) { "node.exe" } else { "node" }),
            b"",
        )
        .unwrap();

        // 2. DSH 入口与 package.json 版本。
        let dsh = root.join("dsh");
        fs::create_dir_all(dsh.join("lib")).unwrap();
        fs::write(dsh.join("lib").join("bin.js"), b"").unwrap();
        fs::write(
            dsh.join("package.json"),
            format!(r#"{{"name":"@deepseek-ai/dsh","version":"{version}"}}"#),
        )
        .unwrap();
    }

    /// 创建包含假运行时的 zip 归档，返回 (路径, 摘要)。
    fn write_runtime_zip(root: &Path, version: &str) -> (PathBuf, String) {
        // 1. 写入两个必需文件与 package.json。
        let path = root.join("core.zip");
        let file = File::create(&path).unwrap();
        let mut writer = zip::ZipWriter::new(file);
        let options = zip::write::SimpleFileOptions::default();
        let node_name = if cfg!(windows) {
            "node/node.exe"
        } else {
            "node/node"
        };
        for (name, content) in [
            (node_name.to_string(), String::new()),
            ("dsh/lib/bin.js".to_string(), String::new()),
            (
                "dsh/package.json".to_string(),
                format!(r#"{{"name":"@deepseek-ai/dsh","version":"{version}"}}"#),
            ),
        ] {
            writer.start_file(name, options).unwrap();
            writer.write_all(content.as_bytes()).unwrap();
        }
        writer.finish().unwrap();

        // 2. 计算归档摘要。
        let sha = sha256_file(&path).unwrap();
        (path, sha)
    }

    /// 验证目录 fixture 能完成装配、写标记并返回 Managed 候选。
    #[test]
    fn installs_directory_fixture_and_probes() {
        // 1. 准备来源目录与隔离 AppData。
        let source = temp_dir("source");
        let appdata = temp_dir("appdata");
        write_runtime_tree(&source, "0.2.1-rc.2");
        let runtime = ManagedRuntime::new(&appdata);
        let mut stages = Vec::new();
        let mut progress = |stage: InstallStage, _detail: String| stages.push(stage);

        // 2. 装配并校验布局、版本与标记。
        let candidate = runtime
            .install(
                &ArtifactSource::Directory {
                    path: source.clone(),
                },
                Some(&"0.2.0".parse().unwrap()),
                &mut progress,
            )
            .expect("目录 fixture 应装配成功");
        assert_eq!(candidate.source, CoreSource::Managed);
        assert_eq!(candidate.version_text.as_deref(), Some("0.2.1-rc.2"));
        assert!(runtime
            .root()
            .join("node")
            .join(if cfg!(windows) { "node.exe" } else { "node" })
            .is_file());
        assert!(runtime
            .root()
            .join("dsh")
            .join("lib")
            .join("bin.js")
            .is_file());
        assert!(runtime.root().join(RUNTIME_MARKER).is_file());
        assert_eq!(runtime.probe().unwrap().source, CoreSource::Managed);

        // 3. 阶段顺序应为 校验 → 复制 → 启用。
        assert_eq!(
            stages,
            vec![
                InstallStage::Verify,
                InstallStage::Extract,
                InstallStage::Activate
            ]
        );

        let _ = fs::remove_dir_all(source);
        let _ = fs::remove_dir_all(appdata);
    }

    /// 验证第二次装配会替换旧运行时，而不是叠加目录。
    #[test]
    fn replaces_previous_managed_runtime() {
        // 1. 先装 0.2.0，再装 0.2.1-rc.2。
        let source = temp_dir("replace-source");
        let appdata = temp_dir("replace-appdata");
        write_runtime_tree(&source, "0.2.0");
        let runtime = ManagedRuntime::new(&appdata);
        runtime
            .install(
                &ArtifactSource::Directory {
                    path: source.clone(),
                },
                None,
                &mut |_, _| {},
            )
            .unwrap();
        write_runtime_tree(&source, "0.2.1-rc.2");
        let candidate = runtime
            .install(
                &ArtifactSource::Directory {
                    path: source.clone(),
                },
                None,
                &mut |_, _| {},
            )
            .unwrap();

        // 2. 断言旧版本被替换，且没有遗留 staging/备份目录。
        assert_eq!(candidate.version_text.as_deref(), Some("0.2.1-rc.2"));
        let leftovers = fs::read_dir(runtime.root().parent().unwrap())
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| {
                let name = entry.file_name().to_string_lossy().to_string();
                name.starts_with(STAGING_PREFIX) || name.starts_with(BACKUP_PREFIX)
            })
            .count();
        assert_eq!(leftovers, 0);

        let _ = fs::remove_dir_all(source);
        let _ = fs::remove_dir_all(appdata);
    }

    /// 验证低于最低版本的 fixture 被拒绝，且不会破坏已安装运行时。
    #[test]
    fn rejects_too_old_fixture_without_touching_existing() {
        // 1. 先安装一个达标版本。
        let source = temp_dir("old-source");
        let appdata = temp_dir("old-appdata");
        write_runtime_tree(&source, "0.2.1");
        let runtime = ManagedRuntime::new(&appdata);
        runtime
            .install(
                &ArtifactSource::Directory {
                    path: source.clone(),
                },
                Some(&"0.2.0".parse().unwrap()),
                &mut |_, _| {},
            )
            .unwrap();

        // 2. 用 0.1.9 重装应失败，原版本仍在。
        write_runtime_tree(&source, "0.1.9");
        let error = runtime
            .install(
                &ArtifactSource::Directory {
                    path: source.clone(),
                },
                Some(&"0.2.0".parse().unwrap()),
                &mut |_, _| {},
            )
            .unwrap_err();
        assert_eq!(error.code(), "RUNTIME_VERSION_UNSUPPORTED");
        assert_eq!(
            runtime.probe().unwrap().version_text.as_deref(),
            Some("0.2.1")
        );

        let _ = fs::remove_dir_all(source);
        let _ = fs::remove_dir_all(appdata);
    }

    /// 验证带摘要的 zip 归档能校验、解压并启用。
    #[test]
    fn installs_verified_archive() {
        // 1. 生成 zip 与正确摘要。
        let source = temp_dir("zip-source");
        let appdata = temp_dir("zip-appdata");
        let (archive, sha) = write_runtime_zip(&source, "0.2.1-rc.2");
        let runtime = ManagedRuntime::new(&appdata);

        // 2. 安装并断言来源与摘要写入标记。
        let candidate = runtime
            .install(
                &ArtifactSource::Archive {
                    path: archive.clone(),
                    sha256: sha,
                    file_name: "core.zip".to_string(),
                },
                Some(&"0.2.0".parse().unwrap()),
                &mut |_, _| {},
            )
            .unwrap();
        assert_eq!(candidate.version_text.as_deref(), Some("0.2.1-rc.2"));
        let marker: RuntimeMarker =
            serde_json::from_str(&fs::read_to_string(runtime.root().join(RUNTIME_MARKER)).unwrap())
                .unwrap();
        assert_eq!(marker.sha256.as_deref().map(str::len), Some(64));

        let _ = fs::remove_dir_all(source);
        let _ = fs::remove_dir_all(appdata);
    }

    /// 验证摘要不匹配时拒绝安装，不产生托管目录。
    #[test]
    fn rejects_archive_with_wrong_digest() {
        // 1. 用全零摘要触发 mismatch。
        let source = temp_dir("badsha-source");
        let appdata = temp_dir("badsha-appdata");
        let (archive, _sha) = write_runtime_zip(&source, "0.2.1");
        let runtime = ManagedRuntime::new(&appdata);

        // 2. 断言错误码与目录状态。
        let error = runtime
            .install(
                &ArtifactSource::Archive {
                    path: archive,
                    sha256: "0".repeat(64),
                    file_name: "core.zip".to_string(),
                },
                None,
                &mut |_, _| {},
            )
            .unwrap_err();
        assert_eq!(error.code(), "RUNTIME_INTEGRITY_MISMATCH");
        assert!(runtime.probe().is_none());

        let _ = fs::remove_dir_all(source);
        let _ = fs::remove_dir_all(appdata);
    }

    /// 验证归档路径穿越会被拒绝，且不会在目标之外写文件。
    #[test]
    fn rejects_zip_path_traversal() {
        // 1. 构造含 `../escape.txt` 条目的 zip。
        let source = temp_dir("escape-source");
        let appdata = temp_dir("escape-appdata");
        let archive = source.join("evil.zip");
        let file = File::create(&archive).unwrap();
        let mut writer = zip::ZipWriter::new(file);
        let options = zip::write::SimpleFileOptions::default();
        writer.start_file("../escape.txt", options).unwrap();
        writer.write_all(b"escape").unwrap();
        writer.finish().unwrap();
        let sha = sha256_file(&archive).unwrap();
        let runtime = ManagedRuntime::new(&appdata);

        // 2. 断言拒绝且目标之外没有文件；staging 位于 AppData 根下，
        //    `../escape.txt` 若被放行会落到 <AppData>/escape.txt。
        let error = runtime
            .install(
                &ArtifactSource::Archive {
                    path: archive,
                    sha256: sha,
                    file_name: "evil.zip".to_string(),
                },
                None,
                &mut |_, _| {},
            )
            .unwrap_err();
        assert_eq!(error.code(), "RUNTIME_ARCHIVE_PATH_ESCAPE");
        assert!(!appdata.join("escape.txt").exists());
        assert!(!appdata.parent().unwrap().join("escape.txt").exists());
        assert!(
            !fs::read_dir(&appdata)
                .map(|entries| entries.flatten().any(|entry| entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with(STAGING_PREFIX)))
                .unwrap_or(false),
            "失败安装不应留下 staging 目录"
        );

        let _ = fs::remove_dir_all(source);
        let _ = fs::remove_dir_all(appdata);
    }

    /// 验证启动期恢复会把中断切换留下的最新备份还原为托管根目录。
    #[test]
    fn recovers_interrupted_activation_before_probe() {
        // 1. 模拟「根目录缺失 + 备份 + 残留 staging」的崩溃现场。
        let appdata = temp_dir("recover-appdata");
        let runtime = ManagedRuntime::new(&appdata);
        let backup = appdata.join(format!("{BACKUP_PREFIX}interrupted"));
        write_runtime_tree(&backup, "0.2.1");
        let staging = appdata.join(format!("{STAGING_PREFIX}interrupted"));
        fs::create_dir_all(&staging).unwrap();
        assert!(runtime.probe().is_none());

        // 2. 恢复后应能探测到托管运行时，备份与 staging 都被清理。
        let message = runtime
            .recover_interrupted_activation()
            .expect("应执行恢复并返回诊断");
        assert!(message.contains("恢复"), "{message}");
        let candidate = runtime.probe().expect("恢复后应探测到托管运行时");
        assert_eq!(candidate.version_text.as_deref(), Some("0.2.1"));
        assert!(!backup.exists());
        assert!(!staging.exists());

        let _ = fs::remove_dir_all(appdata);
    }

    /// 验证清单缺 URL / 缺摘要时返回 blocked-by-env，而不是伪造下载。
    #[test]
    fn classifies_unconfigured_artifact_as_blocked_by_env() {
        // 1. 空描述。
        let error =
            ArtifactSource::from_descriptor(&CoreDescriptor::default(), None, None).unwrap_err();
        assert_eq!(error.code(), "RUNTIME_ARTIFACT_UNCONFIGURED");

        // 2. 有 URL 无摘要。
        let descriptor = CoreDescriptor {
            url: Some("https://example.invalid/core.zip".to_string()),
            ..CoreDescriptor::default()
        };
        let error = ArtifactSource::from_descriptor(&descriptor, None, None).unwrap_err();
        assert_eq!(error.code(), "RUNTIME_INTEGRITY_UNCONFIGURED");

        // 3. 非 https 地址。
        let descriptor = CoreDescriptor {
            url: Some("http://example.invalid/core.zip".to_string()),
            sha256: Some("0".repeat(64)),
            ..CoreDescriptor::default()
        };
        let error = ArtifactSource::from_descriptor(&descriptor, None, None).unwrap_err();
        assert_eq!(error.code(), "RUNTIME_ARTIFACT_UNTRUSTED_URL");
    }

    /// 验证远程制品明确返回下载传输层 blocked-by-env。
    #[test]
    fn remote_artifact_reports_blocked_download_transport() {
        // 1. 构造合法远程描述。
        let appdata = temp_dir("remote-appdata");
        let runtime = ManagedRuntime::new(&appdata);
        let artifact = ArtifactSource::Remote {
            url: "https://example.invalid/core.zip".to_string(),
            sha256: "0".repeat(64),
            size_bytes: None,
            file_name: "core.zip".to_string(),
        };

        // 2. 断言未创建目录且错误码稳定。
        let error = runtime
            .install(&artifact, None, &mut |_, _| {})
            .unwrap_err();
        assert_eq!(error.code(), "RUNTIME_DOWNLOAD_BLOCKED");
        assert!(runtime.probe().is_none());

        let _ = fs::remove_dir_all(appdata);
    }

    /// 验证摘要规范化与 64 位十六进制约束。
    #[test]
    fn normalizes_and_validates_sha256_text() {
        // 1. 大小写与前缀都应被归一化。
        let digest = "ABCDEF0123456789ABCDEF0123456789ABCDEF0123456789ABCDEF0123456789";
        assert_eq!(
            normalize_sha256(&format!("sha256:{digest}")).unwrap(),
            digest.to_ascii_lowercase()
        );

        // 2. 非 64 位或非十六进制必须拒绝。
        assert!(normalize_sha256("abcd").is_err());
        assert!(normalize_sha256(&format!("sha256:{}", "z".repeat(64))).is_err());
    }
}
