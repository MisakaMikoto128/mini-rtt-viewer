//! GitHub Release 更新规划与自动检查节流。

use anyhow::{bail, Context, Result};
use semver::Version;
use std::path::PathBuf;

const AUTOMATIC_CHECK_INTERVAL_SECS: u64 = 86_400;
const CHECKSUM_ASSET_NAME: &str = "SHA256SUMS";

/// 固定仓库与安装目标的更新配置。
#[derive(Clone, Debug)]
pub struct ReleaseConfig {
    pub owner: String,
    pub repo: String,
    pub api_base_url: String,
    pub current_version: String,
    pub install_path: PathBuf,
}

/// 已选择 Release 的界面展示信息。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReleaseInfo {
    pub version: String,
    pub notes_url: String,
}

/// GitHub Release 规划所需元数据。
#[derive(Clone, Debug)]
pub struct Release {
    pub tag_name: String,
    pub draft: bool,
    pub prerelease: bool,
    pub html_url: String,
    pub assets: Vec<String>,
}

/// 一个可安装的 Release 及其固定资产布局。
#[derive(Clone, Debug)]
pub struct ReleasePlan {
    pub info: ReleaseInfo,
    pub asset_name: String,
    pub bin_path: String,
}

pub struct UpdateManager {
    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "后续网络查询与安装任务读取固定配置")
    )]
    config: ReleaseConfig,
}

impl UpdateManager {
    /// 使用正式 GitHub 仓库和当前可执行文件构造更新器。
    pub fn official() -> Result<Self> {
        Ok(Self {
            config: ReleaseConfig {
                owner: "MisakaMikoto128".into(),
                repo: "mini-rtt-viewer".into(),
                api_base_url: "https://api.github.com".into(),
                current_version: env!("CARGO_PKG_VERSION").into(),
                install_path: std::env::current_exe().context("无法定位当前可执行文件")?,
            },
        })
    }
}

/// 选择版本最高的新版正式 Release，并锁定唯一 zip 与同 Release 校验文件。
pub fn plan_release(current_version: &str, releases: &[Release]) -> Result<Option<ReleasePlan>> {
    let current = parse_version(current_version)?;
    let mut candidates = Vec::new();
    for release in releases {
        if release.draft || release.prerelease {
            continue;
        }
        let version = parse_version(&release.tag_name)?;
        if version <= current || !version.pre.is_empty() {
            continue;
        }
        candidates.push((version, release));
    }

    let Some((version, release)) = candidates.into_iter().max_by(|(a, _), (b, _)| a.cmp(b)) else {
        return Ok(None);
    };
    let version = version.to_string();
    let asset_name = format!("mini-rtt-viewer-{version}-windows-x86_64.zip");
    let mut matching_assets = release.assets.iter().filter(|asset| **asset == asset_name);
    if matching_assets.next().is_none() {
        bail!("Release {version} 缺少资产 {asset_name}");
    }
    if matching_assets.next().is_some() {
        bail!("Release {version} 包含重复资产 {asset_name}");
    }
    if !release
        .assets
        .iter()
        .any(|asset| asset == CHECKSUM_ASSET_NAME)
    {
        bail!("Release {version} 缺少资产 {CHECKSUM_ASSET_NAME}");
    }

    Ok(Some(ReleasePlan {
        info: ReleaseInfo {
            notes_url: release.html_url.clone(),
            version: version.clone(),
        },
        asset_name,
        bin_path: format!("mini-rtt-viewer-{version}-windows-x86_64/mini-rtt-viewer.exe"),
    }))
}

/// 首次检查始终到期，后续检查间隔至少 24 小时；时钟回拨时继续等待。
pub fn automatic_check_due(last: u64, now: u64) -> bool {
    last == 0 || now.saturating_sub(last) >= AUTOMATIC_CHECK_INTERVAL_SECS
}

fn parse_version(value: &str) -> Result<Version> {
    Version::parse(value.strip_prefix('v').unwrap_or(value))
        .with_context(|| format!("无效 Release 版本 {value}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn release(tag: &str, draft: bool, prerelease: bool, assets: &[&str]) -> Release {
        Release {
            tag_name: tag.into(),
            draft,
            prerelease,
            html_url: format!(
                "https://github.com/MisakaMikoto128/mini-rtt-viewer/releases/tag/{tag}"
            ),
            assets: assets.iter().map(|asset| (*asset).to_owned()).collect(),
        }
    }

    #[test]
    fn release_plan_accepts_only_unique_newer_stable_asset() {
        let plan = plan_release(
            "0.5.2",
            &[
                release(
                    "v0.5.3",
                    false,
                    false,
                    &["mini-rtt-viewer-0.5.3-windows-x86_64.zip", "SHA256SUMS"],
                ),
                release(
                    "v0.5.4-rc.1",
                    true,
                    false,
                    &["mini-rtt-viewer-0.5.4-windows-x86_64.zip"],
                ),
                release(
                    "v0.5.1",
                    false,
                    false,
                    &["mini-rtt-viewer-0.5.1-windows-x86_64.zip"],
                ),
            ],
        )
        .unwrap()
        .unwrap();
        assert_eq!(plan.info.version, "0.5.3");
        assert_eq!(plan.asset_name, "mini-rtt-viewer-0.5.3-windows-x86_64.zip");
        assert_eq!(
            plan.bin_path,
            "mini-rtt-viewer-0.5.3-windows-x86_64/mini-rtt-viewer.exe"
        );
        assert!(
            plan_release("0.5.2", &[release("v0.5.2", false, false, &[])])
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn release_plan_rejects_duplicate_or_unchecksummed_assets() {
        let duplicate = release(
            "v0.5.3",
            false,
            false,
            &[
                "mini-rtt-viewer-0.5.3-windows-x86_64.zip",
                "mini-rtt-viewer-0.5.3-windows-x86_64.zip",
                "SHA256SUMS",
            ],
        );
        assert!(plan_release("0.5.2", &[duplicate]).is_err());

        let missing_checksums = release(
            "v0.5.3",
            false,
            false,
            &["mini-rtt-viewer-0.5.3-windows-x86_64.zip"],
        );
        assert!(plan_release("0.5.2", &[missing_checksums]).is_err());
    }

    #[test]
    fn automatic_check_is_throttled_for_twenty_four_hours() {
        assert!(automatic_check_due(0, 100));
        assert!(!automatic_check_due(100, 100 + 86_399));
        assert!(automatic_check_due(100, 100 + 86_400));
        assert!(!automatic_check_due(200, 100)); // 时钟回拨不形成请求风暴
    }

    #[test]
    fn official_config_uses_fixed_repository_and_current_executable() {
        let manager = UpdateManager::official().unwrap();
        assert_eq!(manager.config.owner, "MisakaMikoto128");
        assert_eq!(manager.config.repo, "mini-rtt-viewer");
        assert_eq!(manager.config.api_base_url, "https://api.github.com");
        assert_eq!(manager.config.current_version, env!("CARGO_PKG_VERSION"));
        assert_eq!(
            manager.config.install_path,
            std::env::current_exe().unwrap()
        );
    }
}
