//! GitHub Release 更新规划与自动检查节流。

use anyhow::{anyhow, bail, Context, Result};
use self_update::ReleaseAsset;
use semver::Version;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard};
use std::time::Duration;

const AUTOMATIC_CHECK_INTERVAL_SECS: u64 = 86_400;
const CHECKSUM_ASSET_NAME: &str = "SHA256SUMS";
const MAX_RELEASE_PAGES: usize = 32;

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

#[derive(Clone, Debug)]
struct SelectedRelease {
    tag_name: String,
    plan: ReleasePlan,
    assets: Vec<String>,
}

#[derive(Deserialize)]
struct GitHubRelease {
    tag_name: String,
    draft: bool,
    prerelease: bool,
    html_url: String,
    assets: Vec<GitHubAsset>,
}

#[derive(Deserialize)]
struct GitHubAsset {
    name: String,
}

impl From<GitHubRelease> for Release {
    fn from(release: GitHubRelease) -> Self {
        Self {
            tag_name: release.tag_name,
            draft: release.draft,
            prerelease: release.prerelease,
            html_url: release.html_url,
            assets: release.assets.into_iter().map(|asset| asset.name).collect(),
        }
    }
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
            let selected = self.selected_release()?;
            let mut status = self.status();
            if let Some(selected) = selected {
                status.phase = UpdatePhase::Available;
                status.latest_version = Some(selected.plan.info.version);
                status.notes_url = Some(selected.plan.info.notes_url).filter(|url| !url.is_empty());
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
            let Some(selected) = self.selected_release()? else {
                bail!("没有可安装的新版正式 Release");
            };
            validate_release_assets(&selected)?;
            let expected_version = selected.plan.info.version.clone();
            let installed = self.updater(&selected)?.update()?;
            if !installed.is_updated() || installed.version() != expected_version {
                bail!("Release 未返回预期的已更新结果");
            }
            let mut status = self.status();
            status.phase = UpdatePhase::ReadyToRestart;
            status.latest_version = Some(expected_version);
            status.notes_url = Some(selected.plan.info.notes_url).filter(|url| !url.is_empty());
            Ok(status)
        })();
        self.finish(result)
    }

    fn selected_release(&self) -> Result<Option<SelectedRelease>> {
        let releases_url = reqwest::Url::parse(&format!(
            "{}/repos/{}/{}/releases?per_page=100",
            self.config.api_base_url.trim_end_matches('/'),
            self.config.owner,
            self.config.repo
        ))?;
        let client = http_client()?;
        let mut next_url = Some(releases_url.clone());
        let mut visited = HashSet::new();
        let mut releases = Vec::new();
        for _ in 0..MAX_RELEASE_PAGES {
            let url = next_url
                .take()
                .ok_or_else(|| anyhow!("缺少 Release 分页 URL"))?;
            if !visited.insert(url.to_string()) {
                bail!("Release 分页链接形成循环");
            }
            let response = client
                .get(url)
                .header(reqwest::header::ACCEPT, "application/vnd.github+json")
                .send()?
                .error_for_status()?;
            next_url = next_release_page(response.headers())?
                .map(|next| validate_next_release_url(&next, &releases_url))
                .transpose()?;
            releases.extend(
                response
                    .json::<Vec<GitHubRelease>>()?
                    .into_iter()
                    .map(Release::from),
            );
            if next_url.is_none() {
                return select_release(&self.config.current_version, &releases);
            }
        }
        bail!("Release 分页超过 {MAX_RELEASE_PAGES} 页上限")
    }

    fn updater(&self, selected: &SelectedRelease) -> Result<self_update::backends::github::Update> {
        let asset_name = selected.plan.asset_name.clone();
        let updater = self_update::backends::github::Update::configure()
            .repo_owner(&self.config.owner)
            .repo_name(&self.config.repo)
            .bin_name("mini-rtt-viewer")
            .target("windows-x86_64")
            .current_version(&self.config.current_version)
            .release_tag(&selected.tag_name)
            .bin_install_path(&self.config.install_path)
            .bin_path_in_archive(selected.plan.bin_path.clone())
            .asset_matcher(move |assets| select_unique_release_asset(assets, &asset_name))
            .checksum_from_asset(CHECKSUM_ASSET_NAME)
            .api_base_url(&self.config.api_base_url)
            .reqwest_client(http_client()?)
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
        if phase == UpdatePhase::Installing && state.status.phase != UpdatePhase::Available {
            bail!("更新安装冲突 (409)：当前状态不是已确认的新版");
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

fn next_release_page(headers: &reqwest::header::HeaderMap) -> Result<Option<String>> {
    let Some(link) = headers.get(reqwest::header::LINK) else {
        return Ok(None);
    };
    let link = link.to_str().context("Release Link 响应头不是有效文本")?;
    let mut next = None;
    for entry in link.split(',') {
        let mut parts = entry.trim().split(';');
        let target = parts.next().unwrap_or_default().trim();
        let is_next = parts.any(|part| {
            let part = part.trim();
            part.eq_ignore_ascii_case("rel=\"next\"") || part.eq_ignore_ascii_case("rel=next")
        });
        if !is_next {
            continue;
        }
        if next.is_some() {
            bail!("Release Link 包含多个 next 链接");
        }
        let url = target
            .strip_prefix('<')
            .and_then(|target| target.strip_suffix('>'))
            .filter(|target| !target.is_empty())
            .context("Release next 分页链接格式无效")?;
        next = Some(url.to_owned());
    }
    Ok(next)
}

fn validate_next_release_url(next: &str, releases_url: &reqwest::Url) -> Result<reqwest::Url> {
    let url = reqwest::Url::parse(next).context("Release next 分页 URL 无效")?;
    if url.scheme() != releases_url.scheme()
        || url.host_str() != releases_url.host_str()
        || url.port_or_known_default() != releases_url.port_or_known_default()
        || url.path() != releases_url.path()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
    {
        bail!("Release next 分页链接超出固定仓库或包含无效部分");
    }
    Ok(url)
}

fn http_client() -> Result<reqwest::blocking::Client> {
    Ok(reqwest::blocking::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(60))
        .user_agent(concat!("mini-rtt-viewer/", env!("CARGO_PKG_VERSION")))
        .build()?)
}

fn select_unique_release_asset(
    assets: &[ReleaseAsset],
    expected_asset_name: &str,
) -> Option<ReleaseAsset> {
    let mut matching = assets
        .iter()
        .filter(|asset| asset.name() == expected_asset_name);
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
    let Some(selected) = select_release(current_version, releases)? else {
        return Ok(None);
    };
    validate_release_assets(&selected)?;
    Ok(Some(selected.plan))
}

fn select_release(current_version: &str, releases: &[Release]) -> Result<Option<SelectedRelease>> {
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
    Ok(Some(SelectedRelease {
        tag_name: release.tag_name.clone(),
        plan: ReleasePlan {
            info: ReleaseInfo {
                notes_url: release.html_url.clone(),
                version: version.clone(),
            },
            asset_name,
            bin_path: format!("mini-rtt-viewer-{version}-windows-x86_64/mini-rtt-viewer.exe"),
        },
        assets: release.assets.clone(),
    }))
}

fn validate_release_assets(selected: &SelectedRelease) -> Result<()> {
    let version = &selected.plan.info.version;
    let asset_name = &selected.plan.asset_name;
    let mut matching_assets = selected.assets.iter().filter(|asset| *asset == asset_name);
    if matching_assets.next().is_none() {
        bail!("Release {version} 缺少资产 {asset_name}");
    }
    if matching_assets.next().is_some() {
        bail!("Release {version} 包含重复资产 {asset_name}");
    }
    if !selected
        .assets
        .iter()
        .any(|asset| asset == CHECKSUM_ASSET_NAME)
    {
        bail!("Release {version} 缺少资产 {CHECKSUM_ASSET_NAME}");
    }
    Ok(())
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
    use std::collections::HashMap;
    use std::fs;
    use std::io::{Cursor, Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Barrier, Condvar, Mutex};
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

    struct FixtureRelease {
        version: String,
        asset_version: String,
        draft: bool,
        prerelease: bool,
        include_archive_exe: bool,
    }

    impl FixtureRelease {
        fn new(version: &str) -> Self {
            Self {
                version: version.into(),
                asset_version: version.into(),
                draft: false,
                prerelease: false,
                include_archive_exe: true,
            }
        }

        fn draft(mut self) -> Self {
            self.draft = true;
            self
        }

        fn prerelease(mut self) -> Self {
            self.prerelease = true;
            self
        }

        fn with_asset_version(mut self, version: &str) -> Self {
            self.asset_version = version.into();
            self
        }
    }

    struct FixturePage {
        body: Vec<u8>,
        link: Option<String>,
    }

    struct FixtureResponses {
        release_pages: Mutex<Vec<FixturePage>>,
        release_objects: HashMap<String, Vec<u8>>,
        archives: HashMap<String, Vec<u8>>,
        sums: HashMap<String, Vec<u8>>,
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
            Self::from_releases(current, vec![FixtureRelease::new(latest)], checksum_mode)
        }

        fn with_archive_exe(
            current: &str,
            latest: &str,
            checksum_mode: ChecksumMode,
            include_archive_exe: bool,
        ) -> Self {
            let mut release = FixtureRelease::new(latest);
            release.include_archive_exe = include_archive_exe;
            Self::from_releases(current, vec![release], checksum_mode)
        }

        fn from_releases(
            current: &str,
            specs: Vec<FixtureRelease>,
            checksum_mode: ChecksumMode,
        ) -> Self {
            Self::from_pages(current, vec![specs], checksum_mode)
        }

        fn from_pages(
            current: &str,
            pages: Vec<Vec<FixtureRelease>>,
            checksum_mode: ChecksumMode,
        ) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            listener.set_nonblocking(true).unwrap();
            let base_url = format!("http://{}", listener.local_addr().unwrap());
            let mut release_values = Vec::new();
            let mut release_objects = HashMap::new();
            let mut archives = HashMap::new();
            let mut sums_by_path = HashMap::new();
            let mut index = 0;
            for specs in pages {
                let mut page_values = Vec::new();
                for spec in specs {
                    let version = spec.version.strip_prefix('v').unwrap_or(&spec.version);
                    let asset_name =
                        format!("mini-rtt-viewer-{}-windows-x86_64.zip", spec.asset_version);
                    let archive_path = if spec.include_archive_exe {
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
                    let archive_url = format!("/assets/{version}/release.zip");
                    let sums_url = format!("/assets/{version}/SHA256SUMS");
                    let mut assets = vec![serde_json::json!({
                        "id": index * 2 + 1,
                        "name": asset_name,
                        "url": format!("{base_url}{archive_url}"),
                        "browser_download_url": format!("{base_url}{archive_url}"),
                        "content_type": "application/zip",
                        "size": archive.len(),
                        "digest": null
                    })];
                    if !matches!(checksum_mode, ChecksumMode::Missing) {
                        assets.push(serde_json::json!({
                            "id": index * 2 + 2,
                            "name": "SHA256SUMS",
                            "url": format!("{base_url}{sums_url}"),
                            "browser_download_url": format!("{base_url}{sums_url}"),
                            "content_type": "text/plain",
                            "size": sums.len(),
                            "digest": null
                        }));
                    }
                    let tag_name = format!("v{version}");
                    let value = serde_json::json!({
                        "id": index + 1,
                        "tag_name": tag_name,
                        "created_at": "2026-10-30T00:00:00Z",
                        "published_at": "2026-10-30T00:00:00Z",
                        "name": version,
                        "body": "local fixture",
                        "html_url": format!("{base_url}/releases/v{version}"),
                        "draft": spec.draft,
                        "prerelease": spec.prerelease,
                        "assets": assets
                    });
                    release_objects.insert(tag_name, serde_json::to_vec(&value).unwrap());
                    page_values.push(value);
                    archives.insert(archive_url, archive);
                    sums_by_path.insert(sums_url, sums);
                    index += 1;
                }
                release_values.push(page_values);
            }
            let page_count = release_values.len();
            let release_pages = release_values
                .into_iter()
                .enumerate()
                .map(|(page, values)| FixturePage {
                    body: serde_json::to_vec(&values).unwrap(),
                    link: (page + 1 < page_count).then(|| {
                        format!(
                            "<{base_url}/repos/MisakaMikoto128/mini-rtt-viewer/releases?per_page=100&page={}>; rel=\"next\"",
                            page + 2
                        )
                    }),
                })
                .collect();
            let responses = Arc::new(FixtureResponses {
                release_pages: Mutex::new(release_pages),
                release_objects,
                archives,
                sums: sums_by_path,
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

        fn set_release_link(&self, page: usize, link: Option<String>) {
            self.responses.release_pages.lock().unwrap()[page - 1].link = link;
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
        if let Some(tag) =
            path.strip_prefix("/repos/MisakaMikoto128/mini-rtt-viewer/releases/tags/")
        {
            if let Some(release) = responses.release_objects.get(tag) {
                write_response(&mut stream, 200, "application/json", release);
            } else {
                write_response(&mut stream, 404, "text/plain", b"not found");
            }
        } else if path.starts_with("/repos/MisakaMikoto128/mini-rtt-viewer/releases") {
            let page = path
                .split_once('?')
                .and_then(|(_, query)| query.split('&').find_map(|part| part.strip_prefix("page=")))
                .and_then(|page| page.parse::<usize>().ok())
                .unwrap_or(1);
            let page = responses
                .release_pages
                .lock()
                .unwrap()
                .get(page - 1)
                .map(|page| (page.body.clone(), page.link.clone()));
            if let Some((body, link)) = page {
                write_response_with_link(&mut stream, &body, link.as_deref());
            } else {
                write_response(&mut stream, 404, "text/plain", b"not found");
            }
        } else if let Some(sums) = responses.sums.get(path) {
            if matches!(responses.checksum_mode, ChecksumMode::Missing) {
                write_response(&mut stream, 404, "text/plain", b"not found");
            } else {
                write_response(&mut stream, 200, "text/plain", sums);
            }
        } else if let Some(archive) = responses.archives.get(path) {
            if responses.gate_download.swap(false, Ordering::SeqCst) {
                responses.gate.pause();
            }
            if responses.disconnect_download.swap(false, Ordering::SeqCst) {
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Type: application/zip\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    archive.len()
                )
                .unwrap();
                stream.write_all(&archive[..archive.len() / 2]).unwrap();
            } else {
                write_response(&mut stream, 200, "application/zip", archive);
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

    fn write_response_with_link(stream: &mut TcpStream, body: &[u8], link: Option<&str>) {
        write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n",
            body.len()
        )
        .unwrap();
        if let Some(link) = link {
            write!(stream, "Link: {link}\r\n").unwrap();
        }
        stream.write_all(b"\r\n").unwrap();
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
    fn ready_to_restart_cannot_be_installed_again() {
        let fixture = ReleaseFixture::new("0.5.2", "9.0.0", ChecksumMode::Correct);
        let old = TempExecutable::new("old-binary");
        let manager = UpdateManager::new(fixture.config(old.path()));
        manager.check_now().unwrap();
        manager.install().unwrap();

        let error = manager.install().unwrap_err();
        assert!(
            error.to_string().contains("409"),
            "unexpected error: {error}"
        );
        assert_eq!(manager.status().phase, UpdatePhase::ReadyToRestart);
        assert_eq!(old.contents(), b"new-binary");
    }

    #[test]
    fn stale_available_admission_yields_one_install_and_one_conflict() {
        let fixture = ReleaseFixture::new("0.5.2", "9.0.0", ChecksumMode::Correct);
        let old = TempExecutable::new("old-binary");
        let manager = Arc::new(UpdateManager::new(fixture.config(old.path())));
        manager.check_now().unwrap();
        let admitted = Arc::new(Barrier::new(2));
        let first_done = Arc::new(Barrier::new(2));

        let first_manager = Arc::clone(&manager);
        let first_admitted = Arc::clone(&admitted);
        let first_finished = Arc::clone(&first_done);
        let first = thread::spawn(move || {
            assert_eq!(first_manager.status().phase, UpdatePhase::Available);
            first_admitted.wait();
            let result = first_manager.install();
            first_finished.wait();
            result
        });
        let second_manager = Arc::clone(&manager);
        let second = thread::spawn(move || {
            assert_eq!(second_manager.status().phase, UpdatePhase::Available);
            admitted.wait();
            first_done.wait();
            second_manager.install()
        });

        let results = [first.join().unwrap(), second.join().unwrap()];
        assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
        let conflict = results
            .into_iter()
            .find_map(Result::err)
            .expect("one stale install must conflict");
        assert!(
            conflict.to_string().contains("409"),
            "unexpected error: {conflict}"
        );
        assert_eq!(manager.status().phase, UpdatePhase::ReadyToRestart);
    }

    #[test]
    fn draft_and_prerelease_releases_are_not_offered_or_installed() {
        for release in [
            FixtureRelease::new("10.0.0").draft(),
            FixtureRelease::new("10.0.0-rc.1").prerelease(),
        ] {
            let fixture =
                ReleaseFixture::from_releases("0.5.2", vec![release], ChecksumMode::Correct);
            let old = TempExecutable::new("old-binary");
            let manager = UpdateManager::new(fixture.config(old.path()));
            assert_eq!(manager.check_now().unwrap().phase, UpdatePhase::UpToDate);
            assert!(manager.install().is_err());
            assert_eq!(old.contents(), b"old-binary");
        }
    }

    #[test]
    fn out_of_order_releases_use_highest_stable_for_check_and_install() {
        let fixture = ReleaseFixture::from_releases(
            "0.5.2",
            vec![
                FixtureRelease::new("9.0.0"),
                FixtureRelease::new("10.0.0"),
                FixtureRelease::new("8.0.0"),
            ],
            ChecksumMode::Correct,
        );
        let old = TempExecutable::new("old-binary");
        let manager = UpdateManager::new(fixture.config(old.path()));
        let available = manager.check_now().unwrap();
        assert_eq!(available.latest_version.as_deref(), Some("10.0.0"));
        assert_eq!(
            available.notes_url.as_deref(),
            Some(format!("{}/releases/v10.0.0", fixture.base_url).as_str())
        );
        let installed = manager.install().unwrap();
        assert_eq!(installed.phase, UpdatePhase::ReadyToRestart);
        assert_eq!(installed.latest_version.as_deref(), Some("10.0.0"));
    }

    #[test]
    fn highest_stable_release_on_later_page_is_checked_and_installed() {
        let fixture = ReleaseFixture::from_pages(
            "0.5.2",
            vec![
                vec![FixtureRelease::new("9.0.0")],
                vec![FixtureRelease::new("10.0.0")],
            ],
            ChecksumMode::Correct,
        );
        let old = TempExecutable::new("old-binary");
        let manager = UpdateManager::new(fixture.config(old.path()));
        assert_eq!(
            manager.check_now().unwrap().latest_version.as_deref(),
            Some("10.0.0")
        );
        let installed = manager.install().unwrap();
        assert_eq!(installed.phase, UpdatePhase::ReadyToRestart);
        assert_eq!(installed.latest_version.as_deref(), Some("10.0.0"));
    }

    #[test]
    fn looping_release_pages_return_error() {
        let fixture = ReleaseFixture::from_pages(
            "0.5.2",
            vec![
                vec![FixtureRelease::new("9.0.0")],
                vec![FixtureRelease::new("10.0.0")],
            ],
            ChecksumMode::Correct,
        );
        fixture.set_release_link(
            2,
            Some(format!(
                "<{}/repos/MisakaMikoto128/mini-rtt-viewer/releases?per_page=100&page=1>; rel=\"next\"",
                fixture.base_url
            )),
        );
        let old = TempExecutable::new("old-binary");
        let manager = UpdateManager::new(fixture.config(old.path()));
        assert!(manager.check_now().is_err());
        assert_eq!(manager.status().phase, UpdatePhase::Error);
    }

    #[test]
    fn malformed_next_release_page_returns_error() {
        let fixture = ReleaseFixture::from_releases(
            "0.5.2",
            vec![FixtureRelease::new("9.0.0")],
            ChecksumMode::Correct,
        );
        fixture.set_release_link(1, Some("broken; rel=\"next\"".into()));
        let old = TempExecutable::new("old-binary");
        let manager = UpdateManager::new(fixture.config(old.path()));
        assert!(manager.check_now().is_err());
        assert_eq!(manager.status().phase, UpdatePhase::Error);
    }

    #[test]
    fn release_pagination_is_bounded() {
        let fixture = ReleaseFixture::from_pages(
            "0.5.2",
            (0..33)
                .map(|_| vec![FixtureRelease::new("9.0.0")])
                .collect(),
            ChecksumMode::Correct,
        );
        let old = TempExecutable::new("old-binary");
        let manager = UpdateManager::new(fixture.config(old.path()));
        assert!(manager.check_now().is_err());
        assert_eq!(manager.status().phase, UpdatePhase::Error);
    }

    #[test]
    fn mismatched_asset_version_is_rejected_before_replacing_exe() {
        let fixture = ReleaseFixture::from_releases(
            "0.5.2",
            vec![FixtureRelease::new("9.0.0").with_asset_version("8.0.0")],
            ChecksumMode::Correct,
        );
        let old = TempExecutable::new("old-binary");
        let manager = UpdateManager::new(fixture.config(old.path()));
        manager.check_now().unwrap();
        assert!(manager.install().is_err());
        assert_eq!(old.contents(), b"old-binary");
        assert_eq!(manager.status().phase, UpdatePhase::Error);
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
