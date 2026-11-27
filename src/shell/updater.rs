//! Native Windows auto-updater backed by WinHTTP (`winhttp.dll`) and pure-Rust
//! SHA-256 verification.
//!
//! Design goals:
//! - Zero third-party HTTP/TLS crate bloat; inherits Windows system proxy, PAC,
//!   VPN, and TLS 1.3 root trust store automatically via `WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY`.
//! - Queries GitHub Releases (`/repos/lxfight/LumenExplorer/releases/latest`)
//!   off the UI thread; startup auto-checks sleep briefly so launch latency is untouched.
//! - Verifies downloaded installers against `SHA256SUMS.txt` (when published in
//!   the release) before launching the Inno Setup upgrade handoff (`/SILENT /RELAUNCH`).

use std::ffi::c_void;
use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, anyhow, bail};
use crossbeam_channel::{Receiver, Sender, unbounded};
use serde::Deserialize;
use windows::Win32::Networking::WinHttp::{
    URL_COMPONENTS, WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY, WINHTTP_ACCESS_TYPE_DEFAULT_PROXY, WINHTTP_ADDREQ_FLAG_ADD,
    WINHTTP_ADDREQ_FLAG_REPLACE, WINHTTP_FLAG_SECURE, WINHTTP_INTERNET_SCHEME_HTTPS, WINHTTP_OPTION_REDIRECT_POLICY,
    WINHTTP_OPTION_REDIRECT_POLICY_ALWAYS, WINHTTP_QUERY_CONTENT_LENGTH, WINHTTP_QUERY_FLAG_NUMBER,
    WINHTTP_QUERY_STATUS_CODE, WinHttpAddRequestHeaders, WinHttpCloseHandle, WinHttpConnect, WinHttpCrackUrl,
    WinHttpOpen, WinHttpOpenRequest, WinHttpQueryHeaders, WinHttpReadData, WinHttpReceiveResponse, WinHttpSendRequest,
    WinHttpSetOption, WinHttpSetTimeouts,
};
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
use windows::core::{HSTRING, PCWSTR};

pub const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");
pub const RELEASES_PAGE_URL: &str = "https://github.com/lxfight/LumenExplorer/releases";
const RELEASES_API_URL: &str = "https://api.github.com/repos/lxfight/LumenExplorer/releases/latest";
/// Throttle automatic startup checks to at most once every 24 hours.
pub const AUTO_CHECK_INTERVAL_SECS: u64 = 24 * 60 * 60;
const MAX_API_BYTES: usize = 2 * 1024 * 1024;
const MAX_INSTALLER_BYTES: u64 = 256 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReleaseInfo {
    pub version: String,
    pub tag_name: String,
    pub published_at: String,
    pub notes: String,
    pub html_url: String,
    pub asset_name: String,
    pub download_url: String,
    pub asset_size: u64,
    pub sha256_url: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum UpdateState {
    #[default]
    Idle,
    Checking {
        manual: bool,
    },
    UpToDate {
        checked_at: u64,
    },
    Available(ReleaseInfo),
    Downloading {
        info: ReleaseInfo,
        downloaded: u64,
        total: u64,
    },
    Verifying {
        info: ReleaseInfo,
    },
    ReadyToInstall {
        info: ReleaseInfo,
        installer_path: PathBuf,
    },
    Error {
        message: String,
        manual: bool,
    },
}

pub enum UpdateEvent {
    UpToDate { checked_at: u64, manual: bool },
    Available { info: ReleaseInfo, checked_at: u64, manual: bool },
    DownloadProgress { info: ReleaseInfo, downloaded: u64, total: u64 },
    Verifying { info: ReleaseInfo },
    ReadyToInstall { info: ReleaseInfo, installer_path: PathBuf },
    Error { message: String, manual: bool },
}

#[derive(Deserialize)]
struct GitHubRelease {
    tag_name: String,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
    #[serde(default)]
    body: String,
    #[serde(default)]
    html_url: String,
    #[serde(default)]
    published_at: String,
    #[serde(default)]
    assets: Vec<GitHubAsset>,
}

#[derive(Deserialize, Clone)]
struct GitHubAsset {
    name: String,
    browser_download_url: String,
    #[serde(default)]
    size: u64,
}

pub fn now_unix_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

pub fn should_auto_check(enabled: bool, last_check: u64) -> bool {
    enabled && now_unix_secs().saturating_sub(last_check) >= AUTO_CHECK_INTERVAL_SECS
}

/// Spawns a background update check thread. When `delay` is non-zero (used on
/// startup), the thread sleeps before touching the network so cold-start time
/// is never affected.
pub fn spawn_check(manual: bool, delay: Duration) -> Receiver<UpdateEvent> {
    let (tx, rx) = unbounded();
    let _ = std::thread::Builder::new().name("lumen-update-check".to_string()).spawn(move || {
        if !delay.is_zero() {
            std::thread::sleep(delay);
        }
        run_check(&tx, manual);
        crate::platform::wake();
    });
    rx
}

/// Spawns a background installer download + SHA-256 verification thread.
pub fn spawn_download(info: ReleaseInfo) -> Receiver<UpdateEvent> {
    let (tx, rx) = unbounded();
    let _ = std::thread::Builder::new().name("lumen-update-dl".to_string()).spawn(move || {
        run_download(&tx, info);
        crate::platform::wake();
    });
    rx
}

fn run_check(tx: &Sender<UpdateEvent>, manual: bool) {
    let checked_at = now_unix_secs();
    let headers = "Accept: application/vnd.github+json\r\nX-GitHub-Api-Version: 2022-11-28\r\n";
    let bytes = match http_get_bytes(RELEASES_API_URL, Some(headers), MAX_API_BYTES) {
        Ok(b) => b,
        Err(err) => {
            let _ = tx.send(UpdateEvent::Error { message: err.to_string(), manual });
            return;
        }
    };
    let release: GitHubRelease = match serde_json::from_slice(&bytes) {
        Ok(r) => r,
        Err(err) => {
            let _ = tx.send(UpdateEvent::Error { message: format!("解析版本信息失败: {err}"), manual });
            return;
        }
    };
    if release.draft || release.prerelease {
        let _ = tx.send(UpdateEvent::UpToDate { checked_at, manual });
        return;
    }
    let clean_ver = release.tag_name.trim().trim_start_matches(['v', 'V']).to_string();
    if !is_newer_version(CURRENT_VERSION, &clean_ver) {
        let _ = tx.send(UpdateEvent::UpToDate { checked_at, manual });
        return;
    }
    let Some(installer) = select_installer_asset(&release.assets) else {
        let _ = tx.send(UpdateEvent::Error {
            message: format!("发现新版本 v{clean_ver}，但该发布尚未包含 Windows x64 安装包"),
            manual,
        });
        return;
    };
    if !is_allowed_github_url(&installer.browser_download_url) {
        let _ = tx
            .send(UpdateEvent::Error { message: "更新下载地址未通过 HTTPS 来源校验".to_string(), manual });
        return;
    }
    let sha256_url = select_checksum_asset(&release.assets, &installer.name)
        .filter(|a| is_allowed_github_url(&a.browser_download_url))
        .map(|a| a.browser_download_url.clone());

    let info = ReleaseInfo {
        version: clean_ver,
        tag_name: release.tag_name,
        published_at: release.published_at,
        notes: release.body,
        html_url: if release.html_url.is_empty() { RELEASES_PAGE_URL.to_string() } else { release.html_url },
        asset_name: installer.name.clone(),
        download_url: installer.browser_download_url.clone(),
        asset_size: installer.size,
        sha256_url,
    };
    let _ = tx.send(UpdateEvent::Available { info, checked_at, manual });
}

fn run_download(tx: &Sender<UpdateEvent>, info: ReleaseInfo) {
    if !is_allowed_github_url(&info.download_url) {
        let _ = tx.send(UpdateEvent::Error {
            message: "拒绝从非受信任来源下载安装包".to_string(), manual: true
        });
        return;
    }
    let dir = std::env::temp_dir().join("LumenUpdate");
    if let Err(err) = std::fs::create_dir_all(&dir) {
        let _ = tx.send(UpdateEvent::Error { message: format!("创建更新缓存目录失败: {err}"), manual: true });
        return;
    }
    // Strip any path components from remote asset name before joining.
    let safe_name = Path::new(&info.asset_name)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| format!("LumenSetup-{}-x64.exe", info.version));
    let final_path = dir.join(&safe_name);
    let part_path = dir.join(format!("{safe_name}.part"));
    let _ = std::fs::remove_file(&part_path);

    let mut hasher = Sha256::new();
    let expected_total = info.asset_size;
    let info_clone = info.clone();
    let mut last_report = std::time::Instant::now();

    let download_res = http_download_to_file(&info.download_url, &part_path, MAX_INSTALLER_BYTES, |chunk, dl, tot| {
        hasher.update(chunk);
        let total = if tot > 0 { tot } else { expected_total };
        if last_report.elapsed() >= Duration::from_millis(80) || (total > 0 && dl >= total) {
            last_report = std::time::Instant::now();
            let _ = tx.send(UpdateEvent::DownloadProgress { info: info_clone.clone(), downloaded: dl, total });
            crate::platform::wake();
        }
    });

    if let Err(err) = download_res {
        let _ = std::fs::remove_file(&part_path);
        let _ = tx.send(UpdateEvent::Error { message: format!("下载更新失败: {err}"), manual: true });
        return;
    }

    let _ = tx.send(UpdateEvent::Verifying { info: info.clone() });
    crate::platform::wake();
    let actual_sha256 = hasher.finalize_hex();

    if let Some(sha_url) = &info.sha256_url {
        match http_get_bytes(sha_url, None, 256 * 1024) {
            Ok(bytes) => {
                let text = String::from_utf8_lossy(&bytes);
                if let Some(expected) = parse_sha256_for_file(&text, &safe_name) {
                    if !expected.eq_ignore_ascii_case(&actual_sha256) {
                        let _ = std::fs::remove_file(&part_path);
                        let _ = tx.send(UpdateEvent::Error {
                            message: "安装包 SHA-256 校验未通过，已中止安装并清理临时文件".to_string(),
                            manual: true,
                        });
                        return;
                    }
                } else {
                    let _ = std::fs::remove_file(&part_path);
                    let _ = tx.send(UpdateEvent::Error {
                        message: "校验文件中未找到对应安装包的 SHA-256 摘要".to_string(),
                        manual: true,
                    });
                    return;
                }
            }
            Err(err) => {
                let _ = std::fs::remove_file(&part_path);
                let _ = tx.send(UpdateEvent::Error {
                    message: format!("无法获取 SHA-256 校验文件，已中止安装: {err}"),
                    manual: true,
                });
                return;
            }
        }
    }

    let _ = std::fs::remove_file(&final_path);
    if let Err(err) = std::fs::rename(&part_path, &final_path) {
        let _ = std::fs::remove_file(&part_path);
        let _ = tx.send(UpdateEvent::Error { message: format!("提交安装包文件失败: {err}"), manual: true });
        return;
    }

    let _ = tx.send(UpdateEvent::ReadyToInstall { info, installer_path: final_path });
}

/// Launches the verified installer. For installed copies, performs a clean
/// silent-progress Inno Setup upgrade with automatic relaunch (`/RELAUNCH`);
/// for portable copies, opens the standard interactive install wizard.
pub fn launch_installer(installer_path: &Path) -> Result<()> {
    if !installer_path.is_file() {
        bail!("找不到已下载的安装包文件");
    }
    let mut cmd = std::process::Command::new(installer_path);
    if crate::shell::integrate::is_installed() {
        cmd.args(["/SP-", "/SILENT", "/NORESTART", "/RELAUNCH"]);
    }
    cmd.spawn().with_context(|| format!("启动安装程序失败: {}", installer_path.display()))?;
    Ok(())
}

/// Opens a release URL in the default browser via `ShellExecuteW`.
pub fn open_url(url: &str) {
    let verb = HSTRING::from("open");
    let target = HSTRING::from(url);
    unsafe {
        ShellExecuteW(
            None,
            PCWSTR(verb.as_ptr()),
            PCWSTR(target.as_ptr()),
            PCWSTR::null(),
            PCWSTR::null(),
            SW_SHOWNORMAL,
        );
    }
}

// ---- Version & asset matching ----------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct SemVer {
    major: u64,
    minor: u64,
    patch: u64,
    prerelease: bool,
}

fn parse_semver(raw: &str) -> Option<SemVer> {
    let trimmed = raw.trim().trim_start_matches(['v', 'V']);
    let (core, prerelease) = match trimmed.split_once('-') {
        Some((c, _)) => (c, true),
        None => (trimmed, false),
    };
    let core = core.split('+').next().unwrap_or(core);
    let mut parts = core.split('.');
    let major = parts.next()?.parse::<u64>().ok()?;
    let minor = parts.next().and_then(|p| p.parse::<u64>().ok()).unwrap_or(0);
    let patch = parts.next().and_then(|p| p.parse::<u64>().ok()).unwrap_or(0);
    Some(SemVer { major, minor, patch, prerelease })
}

/// Returns `true` when `candidate` is strictly newer than `current`.
pub fn is_newer_version(current: &str, candidate: &str) -> bool {
    let (Some(cur), Some(cand)) = (parse_semver(current), parse_semver(candidate)) else {
        return false;
    };
    let cur_tuple = (cur.major, cur.minor, cur.patch);
    let cand_tuple = (cand.major, cand.minor, cand.patch);
    if cand_tuple != cur_tuple {
        return cand_tuple > cur_tuple;
    }
    // Stable release is newer than a prerelease of the same numeric version.
    cur.prerelease && !cand.prerelease
}

fn select_installer_asset(assets: &[GitHubAsset]) -> Option<&GitHubAsset> {
    assets
        .iter()
        .find(|a| {
            let lower = a.name.to_ascii_lowercase();
            lower.starts_with("lumensetup-") && lower.ends_with("-x64.exe")
        })
        .or_else(|| {
            assets.iter().find(|a| {
                let lower = a.name.to_ascii_lowercase();
                lower.ends_with(".exe") && lower.contains("setup")
            })
        })
}

fn select_checksum_asset<'a>(assets: &'a [GitHubAsset], installer_name: &str) -> Option<&'a GitHubAsset> {
    let sidecar = format!("{installer_name}.sha256").to_ascii_lowercase();
    assets
        .iter()
        .find(|a| a.name.eq_ignore_ascii_case("SHA256SUMS.txt"))
        .or_else(|| assets.iter().find(|a| a.name.to_ascii_lowercase() == sidecar))
}

fn is_allowed_github_url(url: &str) -> bool {
    let Some(rest) = url.strip_prefix("https://") else {
        return false;
    };
    let host = rest.split('/').next().unwrap_or("").to_ascii_lowercase();
    host == "github.com" || host == "api.github.com" || host.ends_with(".githubusercontent.com")
}

/// Parses `<64-hex-sha256>  <filename>` or `<64-hex-sha256> *<filename>` lines.
pub fn parse_sha256_for_file(text: &str, filename: &str) -> Option<String> {
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let mut parts = trimmed.split_whitespace();
        let digest = parts.next()?;
        let name = parts.next()?.trim_start_matches('*');
        if digest.len() == 64
            && digest.bytes().all(|b| b.is_ascii_hexdigit())
            && (name.eq_ignore_ascii_case(filename)
                || Path::new(name)
                    .file_name()
                    .is_some_and(|base| base.to_string_lossy().eq_ignore_ascii_case(filename)))
        {
            return Some(digest.to_ascii_lowercase());
        }
    }
    None
}

// ---- WinHTTP client --------------------------------------------------------

struct WinHttpHandle(*mut c_void);

impl WinHttpHandle {
    fn new(ptr: *mut c_void) -> Option<Self> {
        (!ptr.is_null()).then_some(Self(ptr))
    }
}

impl Drop for WinHttpHandle {
    fn drop(&mut self) {
        if !self.0.is_null() {
            let _ = unsafe { WinHttpCloseHandle(self.0) };
        }
    }
}

struct ParsedUrl {
    host: String,
    port: u16,
    path_and_query: String,
    https: bool,
}

fn crack_url(url: &str) -> Result<ParsedUrl> {
    let wide: Vec<u16> = url.encode_utf16().collect();
    let mut host_buf = vec![0u16; 256];
    let mut path_buf = vec![0u16; 2048];
    let mut extra_buf = vec![0u16; 2048];
    let mut comps = URL_COMPONENTS {
        dwStructSize: std::mem::size_of::<URL_COMPONENTS>() as u32,
        lpszHostName: windows::core::PWSTR(host_buf.as_mut_ptr()),
        dwHostNameLength: host_buf.len() as u32,
        lpszUrlPath: windows::core::PWSTR(path_buf.as_mut_ptr()),
        dwUrlPathLength: path_buf.len() as u32,
        lpszExtraInfo: windows::core::PWSTR(extra_buf.as_mut_ptr()),
        dwExtraInfoLength: extra_buf.len() as u32,
        ..Default::default()
    };
    unsafe { WinHttpCrackUrl(&wide, 0, &mut comps) }.context("解析更新 URL 失败")?;
    let https = comps.nScheme == WINHTTP_INTERNET_SCHEME_HTTPS;
    if !https {
        bail!("仅允许通过 HTTPS 获取更新");
    }
    let host = String::from_utf16_lossy(&host_buf[..comps.dwHostNameLength as usize]);
    let mut path_and_query = String::from_utf16_lossy(&path_buf[..comps.dwUrlPathLength as usize]);
    if comps.dwExtraInfoLength > 0 {
        path_and_query.push_str(&String::from_utf16_lossy(&extra_buf[..comps.dwExtraInfoLength as usize]));
    }
    if path_and_query.is_empty() {
        path_and_query.push('/');
    }
    Ok(ParsedUrl { host, port: comps.nPort, path_and_query, https })
}

fn open_get_request(
    url: &str,
    extra_headers: Option<&str>,
) -> Result<(WinHttpHandle, WinHttpHandle, WinHttpHandle, u64)> {
    let parsed = crack_url(url)?;
    let agent = HSTRING::from(format!("Lumen/{CURRENT_VERSION} (Windows NT 10.0; Win64; x64)"));

    let session_ptr = unsafe {
        let p =
            WinHttpOpen(PCWSTR(agent.as_ptr()), WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY, PCWSTR::null(), PCWSTR::null(), 0);
        if p.is_null() {
            WinHttpOpen(PCWSTR(agent.as_ptr()), WINHTTP_ACCESS_TYPE_DEFAULT_PROXY, PCWSTR::null(), PCWSTR::null(), 0)
        } else {
            p
        }
    };
    let session = WinHttpHandle::new(session_ptr).ok_or_else(|| anyhow!("初始化 WinHTTP 会话失败"))?;
    let _ = unsafe { WinHttpSetTimeouts(session.0, 10_000, 10_000, 15_000, 30_000) };

    let host_w = HSTRING::from(&parsed.host);
    let connect = WinHttpHandle::new(unsafe { WinHttpConnect(session.0, PCWSTR(host_w.as_ptr()), parsed.port, 0) })
        .ok_or_else(|| anyhow!("无法连接到服务器 {}", parsed.host))?;

    let verb = HSTRING::from("GET");
    let path_w = HSTRING::from(&parsed.path_and_query);
    let flags = if parsed.https { WINHTTP_FLAG_SECURE } else { Default::default() };
    let request = WinHttpHandle::new(unsafe {
        WinHttpOpenRequest(
            connect.0,
            PCWSTR(verb.as_ptr()),
            PCWSTR(path_w.as_ptr()),
            PCWSTR::null(),
            PCWSTR::null(),
            std::ptr::null(),
            flags,
        )
    })
    .ok_or_else(|| anyhow!("创建 HTTP 请求失败"))?;

    // Always follow GitHub release asset HTTPS redirects cleanly.
    let policy = WINHTTP_OPTION_REDIRECT_POLICY_ALWAYS.to_ne_bytes();
    let _ =
        unsafe { WinHttpSetOption(Some(request.0 as *const c_void), WINHTTP_OPTION_REDIRECT_POLICY, Some(&policy)) };

    if let Some(hdrs) = extra_headers {
        let wide: Vec<u16> = hdrs.encode_utf16().collect();
        let _ = unsafe {
            WinHttpAddRequestHeaders(request.0, &wide, WINHTTP_ADDREQ_FLAG_ADD | WINHTTP_ADDREQ_FLAG_REPLACE)
        };
    }

    unsafe { WinHttpSendRequest(request.0, None, None, 0, 0, 0) }.context("发送更新请求失败（请检查网络或代理）")?;
    unsafe { WinHttpReceiveResponse(request.0, std::ptr::null_mut()) }.context("接收服务器响应超时或中断")?;

    let mut status: u32 = 0;
    let mut status_len = std::mem::size_of::<u32>() as u32;
    unsafe {
        WinHttpQueryHeaders(
            request.0,
            WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER,
            PCWSTR::null(),
            Some(&mut status as *mut u32 as *mut c_void),
            &mut status_len,
            std::ptr::null_mut(),
        )
    }
    .context("读取 HTTP 状态码失败")?;

    match status {
        200 => {}
        404 => bail!("GitHub 仓库尚无已发布的正式 Release"),
        403 | 429 => bail!("GitHub API 请求频率受限，请稍后重试或前往 Releases 页面下载"),
        other => bail!("更新服务器返回异常状态码 HTTP {other}"),
    }

    let mut cl_buf = [0u16; 32];
    let mut cl_bytes = (cl_buf.len() * 2) as u32;
    let content_length = if unsafe {
        WinHttpQueryHeaders(
            request.0,
            WINHTTP_QUERY_CONTENT_LENGTH,
            PCWSTR::null(),
            Some(cl_buf.as_mut_ptr() as *mut c_void),
            &mut cl_bytes,
            std::ptr::null_mut(),
        )
    }
    .is_ok()
    {
        let chars = (cl_bytes as usize) / 2;
        String::from_utf16_lossy(&cl_buf[..chars]).trim().parse::<u64>().unwrap_or(0)
    } else {
        0
    };

    Ok((session, connect, request, content_length))
}

fn http_get_bytes(url: &str, extra_headers: Option<&str>, max_bytes: usize) -> Result<Vec<u8>> {
    let (_session, _connect, request, content_length) = open_get_request(url, extra_headers)?;
    if content_length > max_bytes as u64 {
        bail!("响应体积超过上限");
    }
    let mut out = Vec::with_capacity((content_length as usize).min(max_bytes));
    let mut buf = [0u8; 16 * 1024];
    loop {
        let mut read = 0u32;
        unsafe { WinHttpReadData(request.0, buf.as_mut_ptr() as *mut c_void, buf.len() as u32, &mut read) }
            .context("读取响应数据失败")?;
        if read == 0 {
            break;
        }
        let n = read as usize;
        if out.len() + n > max_bytes {
            bail!("响应数据超出安全限制");
        }
        out.extend_from_slice(&buf[..n]);
    }
    Ok(out)
}

fn http_download_to_file<F>(url: &str, dest: &Path, max_bytes: u64, mut on_chunk: F) -> Result<u64>
where
    F: FnMut(&[u8], u64, u64),
{
    let (_session, _connect, request, content_length) = open_get_request(url, None)?;
    if content_length > max_bytes {
        bail!("安装包体积异常，已中止下载");
    }
    let mut file = File::create(dest).with_context(|| format!("创建临时文件失败: {}", dest.display()))?;
    let mut buf = vec![0u8; 64 * 1024];
    let mut total_read = 0u64;
    loop {
        let mut read = 0u32;
        unsafe { WinHttpReadData(request.0, buf.as_mut_ptr() as *mut c_void, buf.len() as u32, &mut read) }
            .context("下载流中断")?;
        if read == 0 {
            break;
        }
        let chunk = &buf[..read as usize];
        total_read += chunk.len() as u64;
        if total_read > max_bytes {
            bail!("安装包大小超出限制");
        }
        file.write_all(chunk).context("写入安装包临时文件失败")?;
        on_chunk(chunk, total_read, content_length);
    }
    file.flush().context("刷新安装包文件失败")?;
    if content_length > 0 && total_read != content_length {
        bail!("下载不完整（已接收 {total_read} / {content_length} 字节）");
    }
    Ok(total_read)
}

// ---- Pure-Rust FIPS 180-4 SHA-256 (zero external crate deps) ---------------

struct Sha256 {
    state: [u32; 8],
    buf: [u8; 64],
    buf_len: usize,
    total_bits: u64,
}

impl Sha256 {
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5, 0xd807aa98,
        0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786,
        0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8,
        0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13,
        0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819,
        0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a,
        0x5b9cca4f, 0x682e6ff3, 0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
        0xc67178f2,
    ];

    fn new() -> Self {
        Self {
            state: [0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19],
            buf: [0u8; 64],
            buf_len: 0,
            total_bits: 0,
        }
    }

    fn update(&mut self, mut data: &[u8]) {
        self.total_bits = self.total_bits.wrapping_add((data.len() as u64) * 8);
        if self.buf_len > 0 {
            let take = (64 - self.buf_len).min(data.len());
            self.buf[self.buf_len..self.buf_len + take].copy_from_slice(&data[..take]);
            self.buf_len += take;
            data = &data[take..];
            if self.buf_len == 64 {
                let block = self.buf;
                self.compress(&block);
                self.buf_len = 0;
            }
        }
        while data.len() >= 64 {
            let mut block = [0u8; 64];
            block.copy_from_slice(&data[..64]);
            self.compress(&block);
            data = &data[64..];
        }
        if !data.is_empty() {
            self.buf[..data.len()].copy_from_slice(data);
            self.buf_len = data.len();
        }
    }

    fn compress(&mut self, block: &[u8; 64]) {
        let mut w = [0u32; 64];
        for (dst, &chunk) in w.iter_mut().zip(block.as_chunks::<4>().0.iter()) {
            *dst = u32::from_be_bytes(chunk);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16].wrapping_add(s0).wrapping_add(w[i - 7]).wrapping_add(s1);
        }
        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = self.state;
        for (&k, &wi) in Self::K.iter().zip(w.iter()) {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ ((!e) & g);
            let temp1 = h.wrapping_add(s1).wrapping_add(ch).wrapping_add(k).wrapping_add(wi);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let temp2 = s0.wrapping_add(maj);
            h = g;
            g = f;
            f = e;
            e = d.wrapping_add(temp1);
            d = c;
            c = b;
            b = a;
            a = temp1.wrapping_add(temp2);
        }
        for (s, v) in self.state.iter_mut().zip([a, b, c, d, e, f, g, h]) {
            *s = s.wrapping_add(v);
        }
    }

    fn finalize_hex(mut self) -> String {
        let bit_len = self.total_bits;
        self.buf[self.buf_len] = 0x80;
        self.buf_len += 1;
        if self.buf_len > 56 {
            self.buf[self.buf_len..64].fill(0);
            let block = self.buf;
            self.compress(&block);
            self.buf_len = 0;
        }
        self.buf[self.buf_len..56].fill(0);
        self.buf[56..64].copy_from_slice(&bit_len.to_be_bytes());
        let block = self.buf;
        self.compress(&block);

        let mut hex = String::with_capacity(64);
        for word in self.state {
            use std::fmt::Write as _;
            let _ = write!(hex, "{word:08x}");
        }
        hex
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn semver_comparison_handles_v_prefix_and_prerelease() {
        assert!(is_newer_version("0.2.0", "0.2.1"));
        assert!(is_newer_version("0.2.0", "v0.3.0"));
        assert!(is_newer_version("0.2.9", "0.10.0"));
        assert!(is_newer_version("0.2.0-rc.1", "0.2.0"));
        assert!(!is_newer_version("0.2.0", "0.2.0"));
        assert!(!is_newer_version("0.2.0", "0.2.0-rc.2"));
        assert!(!is_newer_version("0.3.0", "0.2.9"));
    }

    #[test]
    fn sha256_matches_nist_vectors() {
        let mut h = Sha256::new();
        assert_eq!(h.finalize_hex(), "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855");

        h = Sha256::new();
        h.update(b"abc");
        assert_eq!(h.finalize_hex(), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");

        h = Sha256::new();
        h.update(b"abcdbcdecdefdefgefghfghighijhijk");
        h.update(b"ijkljklmklmnlmnomnopnopq");
        assert_eq!(h.finalize_hex(), "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1");
    }

    #[test]
    fn parses_sha256sums_and_trusts_github_urls_only() {
        let sums = "# checksums\n\
            ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad  LumenSetup-0.3.0-x64.exe\n\
            e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855 *other.zip\n";
        assert_eq!(
            parse_sha256_for_file(sums, "LumenSetup-0.3.0-x64.exe").as_deref(),
            Some("ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad")
        );
        assert_eq!(parse_sha256_for_file(sums, "missing.exe"), None);

        assert!(is_allowed_github_url(
            "https://github.com/lxfight/LumenExplorer/releases/download/v0.3.0/LumenSetup-0.3.0-x64.exe"
        ));
        assert!(is_allowed_github_url("https://objects.githubusercontent.com/some/asset"));
        assert!(!is_allowed_github_url("http://github.com/lxfight/LumenExplorer"));
        assert!(!is_allowed_github_url("https://evil.example.com/LumenSetup.exe"));
    }
}
