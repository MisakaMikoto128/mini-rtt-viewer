//! GitHub Release 更新规划与自动检查节流。

use anyhow::{anyhow, bail, Context, Result};
use self_update::{ReleaseAsset, UpdateStrategy};
use semver::Version;
use serde::Serialize;
use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard};
use std::time::Duration;

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

/// 更新操作当前所处阶段。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum UpdatePhase {
    Idle,
    Checking,
    Available,
    UpToDate,
    Installing,
    ReadyToRestart,
    Error,
}

/// 面向本机管理接口的更新状态。
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateStatus {
    pub current_version: String,
    pub latest_version: Option<String>,
    pub phase: UpdatePhase,
    pub notes_url: Option<String>,
    pub message: Option<String>,
}

#[derive(Debug)]
struct UpdateState {
    status: UpdateStatus,
}

pub struct UpdateManager {
    config: ReleaseConfig,
    state: Mutex<UpdateState>,
}

impl UpdateManager {
    pub fn new(config: ReleaseConfig) -> Self {
        let status = UpdateStatus {
            current_version: config.current_version.clone(),
            latest_version: None,
            phase: UpdatePhase::Idle,
            notes_url: None,
            message: None,
        };
        Self {
            config,
            state: Mutex::new(UpdateState { status }),
        }
    }

    /// 使用正式 GitHub 仓库和当前可执行文件构造更新器。
    pub fn official() -> Result<Self> {
        Ok(Self::new(ReleaseConfig {
            owner: "MisakaMikoto128".into(),
            repo: "mini-rtt-viewer".into(),
            api_base_url: "https://api.github.com".into(),
            current_version: env!("CARGO_PKG_VERSION").into(),
            install_path: std::env::current_exe().context("无法定位当前可执行文件")?,
        }))
    }

    pub fn status(&self) -> UpdateStatus {
        self.lock_state().status.clone()
    }

    pub fn check_now(&self) -> Result<UpdateStatus> {
        self.begin(UpdatePhase::Checking)?;
        let result = (|| {
            let release = self.updater()?.is_update_available()?;
            let mut status = self.status();
            if let Some(release) = release {
                let info = ReleaseInfo {
                    version: release.version().to_owned(),
                    notes_url: release.release_notes_url().unwrap_or_default().to_owned(),
                };
                status.phase = UpdatePhase::Available;
                status.latest_version = Some(info.version);
                status.notes_url = Some(info.notes_url).filter(|url| !url.is_empty());
            } else {
                status.phase = UpdatePhase::UpToDate;
                status.latest_version = None;
                status.notes_url = None;
            }
            Ok(status)
        })();
        self.finish(result)
    }

    pub fn install(&self) -> Result<UpdateStatus> {
        self.begin(UpdatePhase::Installing)?;
        let result = (|| {
            let installed = self.updater()?.update()?;
            if !installed.is_updated() {
                bail!("Release 未返回已更新结果");
            }
            let mut status = self.status();
            if status.latest_version.as_deref() != Some(installed.version()) {
                status.notes_url = None;
            }
            status.phase = UpdatePhase::ReadyToRestart;
            status.latest_version = Some(installed.version().to_owned());
            Ok(status)
        })();
        self.finish(result)
    }

    fn updater(&self) -> Result<self_update::backends::github::Update> {
        let client = reqwest::blocking::Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(60))
            .user_agent(concat!("mini-rtt-viewer/", env!("CARGO_PKG_VERSION")))
            .build()?;
        let updater = self_update::backends::github::Update::configure()
            .repo_owner(&self.config.owner)
            .repo_name(&self.config.repo)
            .bin_name("mini-rtt-viewer")
            .target("windows-x86_64")
            .current_version(&self.config.current_version)
            .bin_install_path(&self.config.install_path)
            .bin_path_in_archive("mini-rtt-viewer-{{ version }}-windows-x86_64/mini-rtt-viewer.exe")
            .asset_matcher(select_unique_release_asset)
            .checksum_from_asset(CHECKSUM_ASSET_NAME)
            .api_base_url(&self.config.api_base_url)
            .reqwest_client(client)
            .update_strategy(UpdateStrategy::Latest)
            .show_output(false)
            .no_confirm(true)
            .build()?;
        Ok(updater)
    }

    fn begin(&self, phase: UpdatePhase) -> Result<()> {
        let mut state = self.lock_state();
        if matches!(
            state.status.phase,
            UpdatePhase::Checking | UpdatePhase::Installing
        ) {
            bail!("更新操作冲突 (409)：已有检查或安装正在进行");
        }
        state.status.phase = phase;
        state.status.message = None;
        if phase == UpdatePhase::Checking {
            state.status.latest_version = None;
            state.status.notes_url = None;
        }
        Ok(())
    }

    fn finish(&self, result: Result<UpdateStatus>) -> Result<UpdateStatus> {
        match result {
            Ok(status) => {
                self.lock_state().status = status.clone();
                Ok(status)
            }
            Err(error) => {
                let message = sanitize_error_message(&format!("{error:#}"));
                let mut state = self.lock_state();
                state.status.phase = UpdatePhase::Error;
                state.status.message = Some(message.clone());
                drop(state);
                Err(anyhow!(message))
            }
        }
    }

    fn lock_state(&self) -> MutexGuard<'_, UpdateState> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

fn select_unique_release_asset(assets: &[ReleaseAsset]) -> Option<ReleaseAsset> {
    let mut matching = assets.iter().filter(|asset| {
        asset
            .name()
            .strip_prefix("mini-rtt-viewer-")
            .and_then(|name| name.strip_suffix("-windows-x86_64.zip"))
            .and_then(|version| Version::parse(version).ok())
            .is_some_and(|version| version.pre.is_empty())
    });
    let selected = matching.next()?.clone();
    if matching.next().is_some() {
        None
    } else {
        Some(selected)
    }
}

fn sanitize_error_message(message: &str) -> String {
    message
        .split_whitespace()
        .map(|token| {
            let Some(start) = token.find("http://").or_else(|| token.find("https://")) else {
                return token.to_owned();
            };
            let url = &token[start..];
            let Some(scheme_end) = url.find("://").map(|index| index + 3) else {
                return token.to_owned();
            };
            let url = url.split(['?', '#']).next().unwrap_or_default();
            let authority_end = url[scheme_end..]
                .find('/')
                .map(|index| scheme_end + index)
                .unwrap_or(url.len());
            let sanitized = if let Some(userinfo_end) = url[scheme_end..authority_end].rfind('@') {
                format!(
                    "{}{}",
                    &url[..scheme_end],
                    &url[scheme_end + userinfo_end + 1..]
                )
            } else {
                url.to_owned()
            };
            format!("{}{sanitized}", &token[..start])
        })
        .collect::<Vec<_>>()
        .join(" ")
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
    use sha2::{Digest, Sha256};
    use std::fs;
    use std::io::{Cursor, Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Condvar, Mutex};
    use std::thread::{self, JoinHandle};
    use std::time::Duration;
    use tempfile::TempDir;

    #[derive(Clone, Copy)]
    enum ChecksumMode {
        Correct,
        Wrong,
        Missing,
    }

    #[derive(Default)]
    struct DownloadGate {
        state: Mutex<(bool, bool)>,
        changed: Condvar,
    }

    impl DownloadGate {
        fn pause(&self) {
            let mut state = self.state.lock().unwrap();
            state.0 = true;
            self.changed.notify_all();
            while !state.1 {
                state = self.changed.wait(state).unwrap();
            }
        }

        fn wait_until_started(&self) {
            let mut state = self.state.lock().unwrap();
            while !state.0 {
                state = self.changed.wait(state).unwrap();
            }
        }

        fn release(&self) {
            let mut state = self.state.lock().unwrap();
            state.1 = true;
            self.changed.notify_all();
        }
    }

    struct FixtureResponses {
        releases: Vec<u8>,
        archive: Vec<u8>,
        sums: Vec<u8>,
        checksum_mode: ChecksumMode,
        disconnect_download: AtomicBool,
        gate_download: AtomicBool,
        gate: Arc<DownloadGate>,
    }

    struct ReleaseFixture {
        base_url: String,
        current_version: String,
        responses: Arc<FixtureResponses>,
        stop: Arc<AtomicBool>,
        server: Option<JoinHandle<()>>,
    }

    impl ReleaseFixture {
        fn new(current: &str, latest: &str, checksum_mode: ChecksumMode) -> Self {
            Self::with_archive_exe(current, latest, checksum_mode, true)
        }

        fn with_archive_exe(
            current: &str,
            latest: &str,
            checksum_mode: ChecksumMode,
            include_archive_exe: bool,
        ) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            listener.set_nonblocking(true).unwrap();
            let base_url = format!("http://{}", listener.local_addr().unwrap());
            let version = latest.strip_prefix('v').unwrap_or(latest);
            let asset_name = format!("mini-rtt-viewer-{version}-windows-x86_64.zip");
            let archive_path = if include_archive_exe {
                format!("mini-rtt-viewer-{version}-windows-x86_64/mini-rtt-viewer.exe")
            } else {
                format!("mini-rtt-viewer-{version}-windows-x86_64/other.exe")
            };
            let mut archive_writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
            archive_writer
                .start_file(
                    archive_path,
                    zip::write::SimpleFileOptions::default()
                        .compression_method(zip::CompressionMethod::Deflated),
                )
                .unwrap();
            archive_writer.write_all(b"new-binary").unwrap();
            let archive = archive_writer.finish().unwrap().into_inner();
            let digest = match checksum_mode {
                ChecksumMode::Wrong => "0".repeat(64),
                _ => Sha256::digest(&archive)
                    .iter()
                    .map(|byte| format!("{byte:02x}"))
                    .collect(),
            };
            let sums = format!("{digest}  {asset_name}\n").into_bytes();
            let mut assets = vec![serde_json::json!({
                "id": 1,
                "name": asset_name,
                "url": format!("{base_url}/assets/release.zip"),
                "browser_download_url": format!("{base_url}/assets/release.zip"),
                "content_type": "application/zip",
                "size": archive.len(),
                "digest": null
            })];
            if !matches!(checksum_mode, ChecksumMode::Missing) {
                assets.push(serde_json::json!({
                    "id": 2,
                    "name": "SHA256SUMS",
                    "url": format!("{base_url}/assets/SHA256SUMS"),
                    "browser_download_url": format!("{base_url}/assets/SHA256SUMS"),
                    "content_type": "text/plain",
                    "size": sums.len(),
                    "digest": null
                }));
            }
            let releases = serde_json::to_vec(&serde_json::json!([{
                "id": 1,
                "tag_name": format!("v{version}"),
                "created_at": "2026-10-30T00:00:00Z",
                "published_at": "2026-10-30T00:00:00Z",
                "name": version,
                "body": "local fixture",
                "html_url": format!("{base_url}/releases/v{version}"),
                "draft": false,
                "prerelease": false,
                "assets": assets
            }]))
            .unwrap();
            let responses = Arc::new(FixtureResponses {
                releases,
                archive,
                sums,
                checksum_mode,
                disconnect_download: AtomicBool::new(false),
                gate_download: AtomicBool::new(false),
                gate: Arc::new(DownloadGate::default()),
            });
            let stop = Arc::new(AtomicBool::new(false));
            let server_responses = Arc::clone(&responses);
            let server_stop = Arc::clone(&stop);
            let server = thread::spawn(move || {
                while !server_stop.load(Ordering::SeqCst) {
                    match listener.accept() {
                        Ok((stream, _)) => {
                            stream.set_nonblocking(false).unwrap();
                            serve_fixture(stream, &server_responses);
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            thread::sleep(Duration::from_millis(2));
                        }
                        Err(error) => panic!("fixture accept failed: {error}"),
                    }
                }
            });
            Self {
                base_url,
                current_version: current.to_owned(),
                responses,
                stop,
                server: Some(server),
            }
        }

        fn config(&self, install_path: &Path) -> ReleaseConfig {
            ReleaseConfig {
                owner: "MisakaMikoto128".into(),
                repo: "mini-rtt-viewer".into(),
                api_base_url: self.base_url.clone(),
                current_version: self.current_version.clone(),
                install_path: install_path.to_owned(),
            }
        }

        fn disconnect_next_download(&self) {
            self.responses
                .disconnect_download
                .store(true, Ordering::SeqCst);
        }

        fn pause_next_download(&self) {
            self.responses.gate_download.store(true, Ordering::SeqCst);
        }

        fn wait_until_download_started(&self) {
            self.responses.gate.wait_until_started();
        }

        fn release_download(&self) {
            self.responses.gate.release();
        }
    }

    impl Drop for ReleaseFixture {
        fn drop(&mut self) {
            self.responses.gate.release();
            self.stop.store(true, Ordering::SeqCst);
            if let Some(server) = self.server.take() {
                server.join().unwrap();
            }
        }
    }

    fn serve_fixture(mut stream: TcpStream, responses: &FixtureResponses) {
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut request = Vec::new();
        let mut buffer = [0; 1024];
        while !request.windows(4).any(|bytes| bytes == b"\r\n\r\n") {
            let read = stream.read(&mut buffer).unwrap();
            if read == 0 {
                return;
            }
            request.extend_from_slice(&buffer[..read]);
            assert!(request.len() <= 16 * 1024, "fixture request too large");
        }
        let request = String::from_utf8(request).unwrap();
        let path = request.split_whitespace().nth(1).unwrap();
        if path.starts_with("/repos/MisakaMikoto128/mini-rtt-viewer/releases") {
            write_response(&mut stream, 200, "application/json", &responses.releases);
        } else if path == "/assets/SHA256SUMS"
            && !matches!(responses.checksum_mode, ChecksumMode::Missing)
        {
            write_response(&mut stream, 200, "text/plain", &responses.sums);
        } else if path == "/assets/release.zip" {
            if responses.gate_download.swap(false, Ordering::SeqCst) {
                responses.gate.pause();
            }
            if responses.disconnect_download.swap(false, Ordering::SeqCst) {
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Type: application/zip\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    responses.archive.len()
                )
                .unwrap();
                stream
                    .write_all(&responses.archive[..responses.archive.len() / 2])
                    .unwrap();
            } else {
                write_response(&mut stream, 200, "application/zip", &responses.archive);
            }
        } else {
            write_response(&mut stream, 404, "text/plain", b"not found");
        }
    }

    fn write_response(stream: &mut TcpStream, status: u16, content_type: &str, body: &[u8]) {
        write!(
            stream,
            "HTTP/1.1 {status} {}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            if status == 200 { "OK" } else { "Not Found" },
            body.len()
        )
        .unwrap();
        stream.write_all(body).unwrap();
    }

    struct TempExecutable {
        _directory: TempDir,
        path: PathBuf,
    }

    impl TempExecutable {
        fn new(contents: &str) -> Self {
            let directory = TempDir::new().unwrap();
            let path = directory.path().join("old.exe");
            fs::write(&path, contents).unwrap();
            Self {
                _directory: directory,
                path,
            }
        }

        fn path(&self) -> &Path {
            &self.path
        }

        fn contents(&self) -> Vec<u8> {
            fs::read(&self.path).unwrap()
        }
    }

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
    fn verified_release_replaces_only_the_temporary_executable() {
        let fixture = ReleaseFixture::new("0.5.2", "9.0.0", ChecksumMode::Correct);
        let old = TempExecutable::new("old-binary");
        let manager = UpdateManager::new(fixture.config(old.path()));
        let available = manager.check_now().unwrap();
        assert_eq!(available.phase, UpdatePhase::Available);
        assert_eq!(available.latest_version.as_deref(), Some("9.0.0"));
        assert_eq!(
            available.notes_url.as_deref(),
            Some(format!("{}/releases/v9.0.0", fixture.base_url).as_str())
        );
        assert_eq!(
            manager.install().unwrap().phase,
            UpdatePhase::ReadyToRestart
        );
        assert_eq!(old.contents(), b"new-binary");
    }

    #[test]
    fn bad_or_missing_checksum_preserves_temporary_executable() {
        for mode in [ChecksumMode::Wrong, ChecksumMode::Missing] {
            let fixture = ReleaseFixture::new("0.5.2", "9.0.0", mode);
            let old = TempExecutable::new("old-binary");
            let manager = UpdateManager::new(fixture.config(old.path()));
            manager.check_now().unwrap();
            assert!(manager.install().is_err());
            assert_eq!(old.contents(), b"old-binary");
            assert_eq!(manager.status().phase, UpdatePhase::Error);
        }
    }

    #[test]
    fn missing_archive_exe_preserves_temporary_executable() {
        let fixture =
            ReleaseFixture::with_archive_exe("0.5.2", "9.0.0", ChecksumMode::Correct, false);
        let old = TempExecutable::new("old-binary");
        let manager = UpdateManager::new(fixture.config(old.path()));
        manager.check_now().unwrap();
        assert!(manager.install().is_err());
        assert_eq!(old.contents(), b"old-binary");
        assert_eq!(manager.status().phase, UpdatePhase::Error);
    }

    #[test]
    fn interrupted_download_preserves_temporary_executable() {
        let fixture = ReleaseFixture::new("0.5.2", "9.0.0", ChecksumMode::Correct);
        fixture.disconnect_next_download();
        let old = TempExecutable::new("old-binary");
        let manager = UpdateManager::new(fixture.config(old.path()));
        manager.check_now().unwrap();
        assert!(manager.install().is_err());
        assert_eq!(old.contents(), b"old-binary");
        assert_eq!(manager.status().phase, UpdatePhase::Error);
    }

    #[test]
    fn concurrent_install_returns_conflict() {
        let fixture = ReleaseFixture::new("0.5.2", "9.0.0", ChecksumMode::Correct);
        let old = TempExecutable::new("old-binary");
        let manager = Arc::new(UpdateManager::new(fixture.config(old.path())));
        manager.check_now().unwrap();
        fixture.pause_next_download();
        let installing = Arc::clone(&manager);
        let first = thread::spawn(move || installing.install());
        fixture.wait_until_download_started();

        let error = manager.install().unwrap_err();
        assert!(
            error.to_string().contains("409"),
            "unexpected error: {error}"
        );
        assert_eq!(manager.status().phase, UpdatePhase::Installing);

        fixture.release_download();
        assert_eq!(
            first.join().unwrap().unwrap().phase,
            UpdatePhase::ReadyToRestart
        );
    }

    #[test]
    fn up_to_date_release_does_not_offer_an_install() {
        let fixture = ReleaseFixture::new("0.5.2", "0.5.2", ChecksumMode::Correct);
        let old = TempExecutable::new("old-binary");
        let manager = UpdateManager::new(fixture.config(old.path()));
        let status = manager.check_now().unwrap();
        assert_eq!(status.phase, UpdatePhase::UpToDate);
        assert_eq!(status.latest_version, None);
    }

    #[test]
    fn status_uses_camel_case_json_fields_and_phases() {
        let fixture = ReleaseFixture::new("0.5.2", "9.0.0", ChecksumMode::Correct);
        let old = TempExecutable::new("old-binary");
        let manager = UpdateManager::new(fixture.config(old.path()));
        let status = serde_json::to_value(manager.status()).unwrap();
        assert_eq!(status["currentVersion"], "0.5.2");
        assert_eq!(status["phase"], "idle");
        assert_eq!(
            serde_json::to_value(UpdatePhase::ReadyToRestart).unwrap(),
            "readyToRestart"
        );
    }

    #[test]
    fn error_summary_removes_url_credentials_and_query() {
        let message = sanitize_error_message(
            "download https://user:secret@example.test/file.zip?token=also-secret failed",
        );
        assert!(!message.contains("secret"), "unsafe message: {message}");
        assert!(message.contains("https://example.test/file.zip"));
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
