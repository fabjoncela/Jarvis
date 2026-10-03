//! Narrow, reviewed Win32 FFI boundary for worker containment, fixed
//! pre-match/Discord launches, and read-only competitive trigger observation.
//!
//! Target processes are opened only for limited identity queries; arbitrary
//! process assignment, termination, and service-control APIs are not exposed.
//! The Job Object API can terminate only workers created and contained by
//! Jarvis itself.

#![allow(unsafe_code)]
#![deny(unsafe_op_in_unsafe_fn)]

mod audio_endpoint;
mod control_pipe;
mod lifecycle_notifications;

pub use audio_endpoint::{
    read_default_master_volume, set_default_master_volume, AudioEndpointError,
};
pub use control_pipe::{
    send_supervisor_control_request, DiscordFeedback, PreferredApp, SupervisorControlCommand,
    SupervisorControlPipeError, SupervisorControlPipeServer,
};
pub use lifecycle_notifications::{
    CompetitiveLifecycleNotifications, LifecycleNotificationFailure,
};

use std::collections::BTreeSet;
use std::error::Error;
use std::ffi::c_void;
use std::fmt;
use std::fs;
use std::mem::size_of;
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};
use std::ptr::{null, null_mut};
use std::time::Duration;

use windows_sys::Win32::Foundation::{
    CloseHandle, GetLastError, ERROR_ALREADY_EXISTS, ERROR_INSUFFICIENT_BUFFER,
    ERROR_NO_MORE_FILES, FILETIME, HANDLE, HWND, INVALID_HANDLE_VALUE, LPARAM, WAIT_FAILED,
    WAIT_OBJECT_0, WAIT_TIMEOUT,
};
use windows_sys::Win32::Security::WinTrust::{
    WinVerifyTrust, WINTRUST_ACTION_GENERIC_VERIFY_V2, WINTRUST_DATA, WINTRUST_DATA_0,
    WINTRUST_FILE_INFO, WTD_CHOICE_FILE, WTD_DISABLE_MD2_MD4,
    WTD_REVOCATION_CHECK_CHAIN_EXCLUDE_ROOT, WTD_REVOKE_NONE, WTD_STATEACTION_IGNORE, WTD_UI_NONE,
};
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS,
};
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, IsProcessInJob,
    JobObjectBasicAccountingInformation, JobObjectExtendedLimitInformation,
    QueryInformationJobObject, SetInformationJobObject, TerminateJobObject,
    JOBOBJECT_BASIC_ACCOUNTING_INFORMATION, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
    JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
};
use windows_sys::Win32::System::Services::{
    CloseServiceHandle, OpenSCManagerW, OpenServiceW, QueryServiceConfigW, QueryServiceStatusEx,
    QUERY_SERVICE_CONFIGW, SC_HANDLE, SC_MANAGER_CONNECT, SC_STATUS_PROCESS_INFO,
    SERVICE_QUERY_CONFIG, SERVICE_QUERY_STATUS, SERVICE_RUNNING, SERVICE_STATUS_PROCESS,
    SERVICE_STOPPED,
};
use windows_sys::Win32::System::Threading::{
    CreateEventW, CreateProcessW, GetCurrentProcess, GetCurrentProcessId, GetExitCodeProcess,
    GetProcessTimes, OpenEventW, OpenProcess, QueryFullProcessImageNameW, ResumeThread, SetEvent,
    TerminateProcess, WaitForSingleObject, CREATE_SUSPENDED, INFINITE, PROCESS_INFORMATION,
    PROCESS_QUERY_LIMITED_INFORMATION, STARTUPINFOW, SYNCHRONIZATION_SYNCHRONIZE,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetClassNameW, GetWindowTextW, GetWindowThreadProcessId, IsWindowVisible,
};

/// A Win32 call failed. Error codes are retained for local diagnostics without
/// logging process arguments or other sensitive runtime data.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Win32Error {
    operation: &'static str,
    code: u32,
}

impl Win32Error {
    #[must_use]
    pub const fn operation(&self) -> &'static str {
        self.operation
    }

    #[must_use]
    pub const fn code(&self) -> u32 {
        self.code
    }
}

impl fmt::Display for Win32Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{} failed with Win32 error {}",
            self.operation, self.code
        )
    }
}

impl Error for Win32Error {}

/// The complete hard-coded set of binaries the reviewed FFI layer may create.
/// This is deliberately an enum rather than an executable path or command-line
/// parameter.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FixedInternalWorker {
    Session,
    Model,
}

/// The only external process identity this FFI boundary observes during the
/// current detection milestone. It cannot be used to target arbitrary apps.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FixedCompetitiveProcess {
    Cs2,
    FaceitAcLauncher,
    FaceitService,
}

impl FixedCompetitiveProcess {
    const fn executable_name(self) -> &'static str {
        match self {
            Self::Cs2 => "cs2.exe",
            Self::FaceitAcLauncher => "faceitclient.exe",
            Self::FaceitService => "faceitservice.exe",
        }
    }
}

/// PID plus creation time from a process opened only with limited query access.
/// This lets a later observer distinguish PID reuse without reading memory,
/// attaching a debugger, changing priority, or modifying the target.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ObservedProcessIdentity {
    process_id: u32,
    creation_time_filetime: u64,
}

impl ObservedProcessIdentity {
    #[must_use]
    pub const fn process_id(self) -> u32 {
        self.process_id
    }

    #[must_use]
    pub const fn creation_time_filetime(self) -> u64 {
        self.creation_time_filetime
    }
}

#[derive(Debug)]
pub enum ProcessObservationError {
    Win32(Win32Error),
    CandidateIdentityUnavailable { process: FixedCompetitiveProcess },
    CandidateImagePathUnavailable { process: FixedCompetitiveProcess },
    CandidateImagePathMismatch { process: FixedCompetitiveProcess },
}

impl fmt::Display for ProcessObservationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Win32(error) => error.fmt(formatter),
            Self::CandidateIdentityUnavailable { process } => write!(
                formatter,
                "could not verify PID and creation-time identity for observed {process:?}"
            ),
            Self::CandidateImagePathUnavailable { process } => {
                write!(
                    formatter,
                    "could not verify the image path for observed {process:?}"
                )
            }
            Self::CandidateImagePathMismatch { process } => {
                write!(
                    formatter,
                    "observed {process:?} did not run from its fixed path"
                )
            }
        }
    }
}

impl Error for ProcessObservationError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Win32(error) => Some(error),
            Self::CandidateIdentityUnavailable { .. }
            | Self::CandidateImagePathUnavailable { .. }
            | Self::CandidateImagePathMismatch { .. } => None,
        }
    }
}

/// The outcome of one read-only local pre-match setup verification. This
/// carries no paths, credentials, or authority to launch an application.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VerifiedPreMatchSetup {
    faceit_ac_launcher: PathBuf,
    faceit_service: PathBuf,
    chrome: PathBuf,
    steam: PathBuf,
}

/// One read-only snapshot of the fixed competitive triggers.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct CompetitiveActivitySnapshot {
    pub cs2_running: bool,
    pub verified_faceit_process_running: bool,
    pub verified_faceit_service_active: bool,
}

#[derive(Debug)]
pub enum CompetitiveActivityObservationError {
    Process(ProcessObservationError),
    Service(PreMatchSetupError),
}

impl fmt::Display for CompetitiveActivityObservationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Process(error) => error.fmt(formatter),
            Self::Service(error) => error.fmt(formatter),
        }
    }
}

impl Error for CompetitiveActivityObservationError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Process(error) => Some(error),
            Self::Service(error) => Some(error),
        }
    }
}

/// The only external applications this FACEIT setup flow can start. All
/// targets and arguments are fixed in code and none can be supplied by a user
/// or an LLM.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PreMatchLaunchStep {
    FaceitAcLauncher,
    ChromeFaceitWebsite,
    SteamCs2,
}

/// Describes whether a fixed pre-match target was started or a FACEIT-like
/// title was detected in a verified Chrome window.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PreMatchLaunchOutcome {
    Started,
    FaceitTitleMatchDetected,
}

impl PreMatchLaunchStep {
    const fn command_line(self) -> &'static str {
        match self {
            Self::FaceitAcLauncher => "faceitclient.exe",
            Self::ChromeFaceitWebsite => "chrome.exe https://www.faceit.com/",
            Self::SteamCs2 => "steam.exe -applaunch 730",
        }
    }
}

#[derive(Debug)]
pub enum PreMatchLaunchError {
    Setup(PreMatchSetupError),
    ProcessStart {
        step: PreMatchLaunchStep,
        source: Win32Error,
    },
}

impl fmt::Display for PreMatchLaunchError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Setup(error) => error.fmt(formatter),
            Self::ProcessStart { step, source } => {
                write!(formatter, "could not start {step:?}: {source}")
            }
        }
    }
}

impl Error for PreMatchLaunchError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Setup(error) => Some(error),
            Self::ProcessStart { source, .. } => Some(source),
        }
    }
}

/// A failure to establish the fixed local identities required for the narrow
/// pre-match handoff. Every error is a denial condition; callers must not
/// substitute a similarly named executable or service.
#[derive(Debug)]
pub enum PreMatchSetupError {
    EnvironmentVariableMissing(&'static str),
    Resolve {
        component: &'static str,
        source: std::io::Error,
    },
    NotRegularFile(&'static str),
    SignatureRejected {
        component: &'static str,
        status: i32,
    },
    Service(Win32Error),
    ServiceBinaryMismatch,
    InvalidServiceCommandLine,
    ServiceRunningWithoutProcessIdentity,
    Cs2ManifestMissing,
    Cs2ManifestUnreadable(std::io::Error),
    Cs2ManifestDoesNotDescribeApp730,
}

impl fmt::Display for PreMatchSetupError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EnvironmentVariableMissing(variable) => {
                write!(
                    formatter,
                    "required Windows environment variable {variable} is unavailable"
                )
            }
            Self::Resolve { component, source } => {
                write!(
                    formatter,
                    "could not resolve fixed {component} location: {source}"
                )
            }
            Self::NotRegularFile(component) => {
                write!(
                    formatter,
                    "fixed {component} location is not a regular file"
                )
            }
            Self::SignatureRejected { component, status } => {
                write!(
                    formatter,
                    "Windows trust verification rejected {component} (status {status})"
                )
            }
            Self::Service(error) => error.fmt(formatter),
            Self::ServiceBinaryMismatch => write!(
                formatter,
                "FACEITService is not registered for the verified fixed service binary"
            ),
            Self::InvalidServiceCommandLine => write!(
                formatter,
                "FACEITService has an unsupported service command line"
            ),
            Self::ServiceRunningWithoutProcessIdentity => write!(
                formatter,
                "FACEITService is running without a verifiable process identity"
            ),
            Self::Cs2ManifestMissing => write!(formatter, "Steam appmanifest_730.acf is missing"),
            Self::Cs2ManifestUnreadable(error) => {
                write!(
                    formatter,
                    "could not read Steam appmanifest_730.acf: {error}"
                )
            }
            Self::Cs2ManifestDoesNotDescribeApp730 => {
                write!(
                    formatter,
                    "Steam appmanifest_730.acf does not describe app 730"
                )
            }
        }
    }
}

impl Error for PreMatchSetupError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Resolve { source, .. } | Self::Cs2ManifestUnreadable(source) => Some(source),
            Self::EnvironmentVariableMissing(_)
            | Self::NotRegularFile(_)
            | Self::SignatureRejected { .. }
            | Self::ServiceBinaryMismatch
            | Self::InvalidServiceCommandLine
            | Self::ServiceRunningWithoutProcessIdentity
            | Self::Cs2ManifestMissing
            | Self::Cs2ManifestDoesNotDescribeApp730 => None,
            Self::Service(error) => Some(error),
        }
    }
}

/// Verifies the only local installations the pre-match launcher may use:
/// FACEIT's Anti-Cheat Launcher and registered service, Google Chrome, Steam, and Steam app
/// 730. This function is read-only. It never starts a service/process, opens a
/// browser, reads credentials, or accepts a caller-provided target.
pub fn verify_default_pre_match_setup() -> Result<VerifiedPreMatchSetup, PreMatchSetupError> {
    let program_files = required_directory("ProgramFiles")?;
    let program_files_x86 = required_directory("ProgramFiles(x86)")?;

    let faceit_root = program_files.join("FACEIT AC");
    let faceit_ac_launcher = fixed_regular_file(
        &faceit_root,
        "faceitclient.exe",
        "FACEIT Anti-Cheat Launcher",
    )?;
    let faceit_service = fixed_regular_file(&faceit_root, "faceitservice.exe", "FACEIT service")?;
    let chrome = fixed_regular_file(
        &program_files
            .join("Google")
            .join("Chrome")
            .join("Application"),
        "chrome.exe",
        "Chrome",
    )?;
    let steam_root = program_files_x86.join("Steam");
    let steam = fixed_regular_file(&steam_root, "steam.exe", "Steam")?;

    verify_trusted_signature(&faceit_ac_launcher, "FACEIT Anti-Cheat Launcher")?;
    verify_trusted_signature(&faceit_service, "FACEIT service")?;
    verify_trusted_signature(&chrome, "Chrome")?;
    verify_trusted_signature(&steam, "Steam")?;

    let configured_service = query_fixed_faceit_service_binary()?;
    let configured_service = canonical_regular_file(&configured_service, "FACEITService binary")?;
    if configured_service != faceit_service {
        return Err(PreMatchSetupError::ServiceBinaryMismatch);
    }

    verify_cs2_manifest(&steam_root)?;
    Ok(VerifiedPreMatchSetup {
        faceit_ac_launcher,
        faceit_service,
        chrome,
        steam,
    })
}

/// Reconciles the currently active fixed triggers against an already verified
/// local FACEIT/Steam installation. The setup value carries identity only; it
/// is not authority to launch applications or services.
pub fn observe_competitive_activity(
    setup: &VerifiedPreMatchSetup,
) -> Result<CompetitiveActivitySnapshot, CompetitiveActivityObservationError> {
    let processes = observe_competitive_processes(setup)
        .map_err(CompetitiveActivityObservationError::Process)?;
    let (service_active, service_process_id) = query_faceit_service_activity(setup)
        .map_err(CompetitiveActivityObservationError::Service)?;
    if service_active && service_process_id != 0 {
        observed_process_identity(
            FixedCompetitiveProcess::FaceitService,
            service_process_id,
            Some(&setup.faceit_service),
        )
        .map_err(CompetitiveActivityObservationError::Process)?;
    }

    Ok(CompetitiveActivitySnapshot {
        cs2_running: processes.cs2_running,
        verified_faceit_process_running: processes.faceit_ac_launcher_running,
        verified_faceit_service_active: service_active || processes.faceit_service_process_running,
    })
}

/// Launches one of the three fixed pre-match targets after repeating the
/// complete local setup verification. This does not start `FACEITService`
/// directly, handle authentication, or accept any dynamic command line.
pub fn launch_verified_pre_match_step(
    step: PreMatchLaunchStep,
) -> Result<PreMatchLaunchOutcome, PreMatchLaunchError> {
    let setup = verify_default_pre_match_setup().map_err(PreMatchLaunchError::Setup)?;
    let executable = match step {
        PreMatchLaunchStep::FaceitAcLauncher => &setup.faceit_ac_launcher,
        PreMatchLaunchStep::ChromeFaceitWebsite => &setup.chrome,
        PreMatchLaunchStep::SteamCs2 => &setup.steam,
    };
    if step == PreMatchLaunchStep::ChromeFaceitWebsite
        && should_skip_chrome_launch(detect_faceit_title_in_verified_chrome(executable))
    {
        return Ok(PreMatchLaunchOutcome::FaceitTitleMatchDetected);
    }
    start_fixed_pre_match_process(executable, step).map(|()| PreMatchLaunchOutcome::Started)
}

/// One-use verified identity for the fixed Discord launcher. This does not
/// override the supervisor's current profile or generation gate.
pub struct VerifiedDiscordLauncher {
    executable: PathBuf,
}

/// One-use identity for the fixed signed Chrome launcher, without a URL or
/// caller-selected arguments. Profile admission remains the supervisor's job.
pub struct VerifiedChromeLauncher {
    executable: PathBuf,
}

#[derive(Debug)]
pub enum DiscordProcessObservationError {
    Setup(PreMatchSetupError),
    Win32(Win32Error),
    CandidateIdentityUnavailable,
    CandidateImageUnavailable,
    CandidateImageOutsideInstall,
    TooManyCandidates,
    TooManyDistinctImages,
}

impl fmt::Display for DiscordProcessObservationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Setup(error) => error.fmt(formatter),
            Self::Win32(error) => error.fmt(formatter),
            Self::CandidateIdentityUnavailable => {
                formatter.write_str("Discord process identity unavailable")
            }
            Self::CandidateImageUnavailable => {
                formatter.write_str("Discord process image unavailable")
            }
            Self::CandidateImageOutsideInstall => {
                formatter.write_str("Discord-named process outside the fixed installation")
            }
            Self::TooManyCandidates => formatter.write_str("too many Discord process candidates"),
            Self::TooManyDistinctImages => {
                formatter.write_str("too many distinct Discord process images")
            }
        }
    }
}

impl Error for DiscordProcessObservationError {}

impl VerifiedDiscordLauncher {
    /// Successful return means CreateProcess accepted the request, not that
    /// Discord completed startup. Call only after fresh supervisor admission.
    pub fn start(self) -> Result<(), Win32Error> {
        start_fixed_process(&self.executable, "Discord.exe")
    }
}

impl VerifiedChromeLauncher {
    /// Successful return means CreateProcess accepted the request, not that
    /// a Chrome window or page is ready.
    pub fn start(self) -> Result<(), Win32Error> {
        start_fixed_process(&self.executable, "chrome.exe")
    }
}

/// Resolves the exact Program Files Chrome installation and checks Windows
/// trust on each request. Does not depend on FACEIT setup being present.
pub fn verify_fixed_chrome_launcher() -> Result<VerifiedChromeLauncher, PreMatchSetupError> {
    let program_files = required_directory("ProgramFiles")?;
    let executable = fixed_regular_file(
        &program_files
            .join("Google")
            .join("Chrome")
            .join("Application"),
        "chrome.exe",
        "Chrome",
    )?;
    verify_trusted_signature(&executable, "Chrome")?;
    Ok(VerifiedChromeLauncher { executable })
}

/// Resolves and trust-checks only the fixed Discord launcher under this
/// account's Local AppData. No caller path or arguments are accepted.
pub fn verify_fixed_discord_launcher() -> Result<VerifiedDiscordLauncher, PreMatchSetupError> {
    let local_app_data = required_directory("LOCALAPPDATA")?;
    let executable = fixed_regular_file(&local_app_data.join("Discord"), "Discord.exe", "Discord")?;
    verify_trusted_signature(&executable, "Discord")?;
    Ok(VerifiedDiscordLauncher { executable })
}

/// Counts only signed Discord processes in the fixed versioned installation
/// layout. This is a current-state observation, not proof that a prior launch
/// request caused the process or that a window or login is ready.
pub fn observe_fixed_discord_process_count() -> Result<u32, DiscordProcessObservationError> {
    let local_app_data =
        required_directory("LOCALAPPDATA").map_err(DiscordProcessObservationError::Setup)?;
    let root = local_app_data
        .join("Discord")
        .canonicalize()
        .map_err(|source| {
            DiscordProcessObservationError::Setup(PreMatchSetupError::Resolve {
                component: "Discord",
                source,
            })
        })?;
    // SAFETY: This is a read-only process snapshot; no process is modified.
    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
    if snapshot == INVALID_HANDLE_VALUE {
        return Err(DiscordProcessObservationError::Win32(last_error(
            "CreateToolhelp32Snapshot",
        )));
    }
    let result = observe_discord_processes_in_snapshot(snapshot, &root);
    // SAFETY: The snapshot handle is owned by this function.
    unsafe {
        let _ = CloseHandle(snapshot);
    }
    result
}

fn observe_discord_processes_in_snapshot(
    snapshot: HANDLE,
    root: &Path,
) -> Result<u32, DiscordProcessObservationError> {
    let mut entry = PROCESSENTRY32W {
        dwSize: size_of::<PROCESSENTRY32W>() as u32,
        ..Default::default()
    };
    // SAFETY: The snapshot is live and the entry structure is initialized.
    let first = unsafe { Process32FirstW(snapshot, &mut entry) };
    if first == 0 {
        let error = last_error("Process32FirstW");
        return if error.code() == ERROR_NO_MORE_FILES {
            Ok(0)
        } else {
            Err(DiscordProcessObservationError::Win32(error))
        };
    }
    let mut count = 0_u32;
    let mut trusted_images = BTreeSet::new();
    loop {
        if process_entry_name(&entry) == "discord.exe" {
            if count >= 64 {
                return Err(DiscordProcessObservationError::TooManyCandidates);
            }
            let image = verify_discord_process_candidate(entry.th32ProcessID, root)?;
            if !trusted_images.contains(&image) {
                if trusted_images.len() >= 2 {
                    return Err(DiscordProcessObservationError::TooManyDistinctImages);
                }
                verify_trusted_signature(&image, "Discord process")
                    .map_err(DiscordProcessObservationError::Setup)?;
                trusted_images.insert(image);
            }
            count += 1;
        }
        // SAFETY: The snapshot remains live and `entry` is the documented
        // in/out structure for this bounded synchronous walk.
        let next = unsafe { Process32NextW(snapshot, &mut entry) };
        if next != 0 {
            continue;
        }
        let error = last_error("Process32NextW");
        return if error.code() == ERROR_NO_MORE_FILES {
            Ok(count)
        } else {
            Err(DiscordProcessObservationError::Win32(error))
        };
    }
}

fn verify_discord_process_candidate(
    process_id: u32,
    root: &Path,
) -> Result<PathBuf, DiscordProcessObservationError> {
    // SAFETY: Query-limited access reads identity and image path only.
    let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, process_id) };
    if handle.is_null() {
        return Err(DiscordProcessObservationError::CandidateIdentityUnavailable);
    }
    let result = (|| {
        process_identity(handle, process_id).map_err(DiscordProcessObservationError::Win32)?;
        let mut buffer = vec![0_u16; 32_768];
        let mut length = buffer.len() as u32;
        // SAFETY: The handle has query-limited access and the buffer is writable.
        let queried =
            unsafe { QueryFullProcessImageNameW(handle, 0, buffer.as_mut_ptr(), &mut length) };
        if queried == 0 || length as usize > buffer.len() {
            return Err(DiscordProcessObservationError::CandidateImageUnavailable);
        }
        let image = PathBuf::from(std::ffi::OsString::from_wide(&buffer[..length as usize]))
            .canonicalize()
            .map_err(|_| DiscordProcessObservationError::CandidateImageUnavailable)?;
        if !discord_app_image_belongs_to_root(&image, root) {
            return Err(DiscordProcessObservationError::CandidateImageOutsideInstall);
        }
        Ok(image)
    })();
    // SAFETY: This function owns the process handle and no longer uses it.
    unsafe {
        let _ = CloseHandle(handle);
    }
    result
}

fn discord_app_image_belongs_to_root(image: &Path, root: &Path) -> bool {
    let Some(filename) = image.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    let Some(app_directory) = image.parent() else {
        return false;
    };
    let Some(app_name) = app_directory.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    let Some(parent) = app_directory.parent() else {
        return false;
    };
    let Some(version) = app_name.strip_prefix("app-") else {
        return false;
    };
    filename.eq_ignore_ascii_case("Discord.exe")
        && version.split('.').all(|segment| {
            !segment.is_empty() && segment.chars().all(|character| character.is_ascii_digit())
        })
        && same_windows_path(parent, root)
}

struct ChromeWindowScan<'a> {
    expected_image: &'a Path,
    faceit_title_match_found: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum FaceitTitleDetection {
    Match,
    NoMatch,
    Unknown,
}

fn detect_faceit_title_in_verified_chrome(expected_image: &Path) -> FaceitTitleDetection {
    let mut scan = ChromeWindowScan {
        expected_image,
        faceit_title_match_found: false,
    };
    // SAFETY: `scan` remains live for the synchronous EnumWindows call. The
    // callback only reads its fixed verified image path and writes its result.
    let enumeration_succeeded = unsafe {
        EnumWindows(
            Some(inspect_visible_chrome_window),
            (&mut scan as *mut ChromeWindowScan<'_>) as LPARAM,
        )
    };
    if scan.faceit_title_match_found {
        FaceitTitleDetection::Match
    } else if enumeration_succeeded != 0 {
        FaceitTitleDetection::NoMatch
    } else {
        // Enumeration failure is not evidence of an existing page. Individual
        // query failures likewise never create a positive match in the callback.
        FaceitTitleDetection::Unknown
    }
}

fn should_skip_chrome_launch(detection: FaceitTitleDetection) -> bool {
    detection == FaceitTitleDetection::Match
}

unsafe extern "system" fn inspect_visible_chrome_window(hwnd: HWND, context: LPARAM) -> i32 {
    if context == 0 {
        return 1;
    }
    // SAFETY: EnumWindows supplied this HWND for the current synchronous
    // callback; IsWindowVisible only queries its visibility state.
    if unsafe { IsWindowVisible(hwnd) } == 0 {
        return 1;
    }

    // SAFETY: EnumWindows synchronously returns the exact non-null pointer
    // passed as LPARAM by `detect_faceit_title_in_verified_chrome`.
    let scan = unsafe { &mut *(context as *mut ChromeWindowScan<'_>) };
    let mut class_name = [0_u16; 256];
    // SAFETY: `class_name` is writable for the entire bounded buffer length.
    let class_length =
        unsafe { GetClassNameW(hwnd, class_name.as_mut_ptr(), class_name.len() as i32) };
    if class_length <= 0
        || String::from_utf16_lossy(&class_name[..class_length as usize]) != "Chrome_WidgetWin_1"
    {
        return 1;
    }

    let mut process_id = 0;
    // SAFETY: `process_id` is a valid writable output for this synchronous
    // window-owner query.
    if unsafe { GetWindowThreadProcessId(hwnd, &mut process_id) } == 0
        || process_id == 0
        || !window_process_matches_image(process_id, scan.expected_image)
    {
        return 1;
    }

    let mut title = [0_u16; 2048];
    // SAFETY: `title` is writable for the entire bounded buffer length.
    let title_length = unsafe { GetWindowTextW(hwnd, title.as_mut_ptr(), title.len() as i32) };
    if title_length > 0
        && title_has_faceit_site_marker(&String::from_utf16_lossy(&title[..title_length as usize]))
    {
        scan.faceit_title_match_found = true;
        return 0;
    }
    1
}

fn window_process_matches_image(process_id: u32, expected_image: &Path) -> bool {
    // SAFETY: This requests only limited query access to the process that owns
    // a visible Chrome-class window; it does not read memory or alter it.
    let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, process_id) };
    if process.is_null() {
        return false;
    }

    let mut image = vec![0_u16; 32_768];
    let mut length = image.len() as u32;
    // SAFETY: `process` is a live limited-query handle and `image` is a
    // writable UTF-16 buffer for this synchronous API call.
    let queried =
        unsafe { QueryFullProcessImageNameW(process, 0, image.as_mut_ptr(), &mut length) };
    // SAFETY: This function owns the process handle and no longer uses it.
    unsafe {
        let _ = CloseHandle(process);
    }
    if queried == 0 || length as usize > image.len() {
        return false;
    }

    let image = PathBuf::from(std::ffi::OsString::from_wide(&image[..length as usize]));
    image
        .canonicalize()
        .is_ok_and(|actual| same_windows_path(expected_image, &actual))
}

fn title_has_faceit_site_marker(title: &str) -> bool {
    let title = title.trim().to_ascii_lowercase();
    title == "faceit"
        || ["faceit -", "faceit |", "faceit:", "faceit –", "faceit —"]
            .iter()
            .any(|prefix| title.starts_with(prefix))
}

fn start_fixed_pre_match_process(
    executable: &Path,
    step: PreMatchLaunchStep,
) -> Result<(), PreMatchLaunchError> {
    start_fixed_process(executable, step.command_line())
        .map_err(|source| PreMatchLaunchError::ProcessStart { step, source })
}

fn start_fixed_process(executable: &Path, command_line: &str) -> Result<(), Win32Error> {
    let application_name = wide_null(executable);
    let mut command_line: Vec<u16> = command_line
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    let startup = STARTUPINFOW {
        cb: size_of::<STARTUPINFOW>() as u32,
        ..Default::default()
    };
    let mut process_information = PROCESS_INFORMATION::default();

    // SAFETY: `application_name` resolves to one signature-verified fixed
    // installation executable; `command_line` is a constant argument string
    // for the selected enum variant. No handles are inherited, no elevation
    // verb is used, and all output structures are writable for this call.
    let created = unsafe {
        CreateProcessW(
            application_name.as_ptr(),
            command_line.as_mut_ptr(),
            null(),
            null(),
            0,
            0,
            null(),
            null(),
            &startup,
            &mut process_information,
        )
    };
    if created == 0 {
        return Err(last_error("CreateProcessW"));
    }

    // SAFETY: Both handles are owned by this successful CreateProcessW call.
    // Closing them relinquishes Jarvis's references without terminating the
    // newly started user applications.
    unsafe {
        let _ = CloseHandle(process_information.hThread);
        let _ = CloseHandle(process_information.hProcess);
    }
    Ok(())
}

fn required_directory(variable: &'static str) -> Result<PathBuf, PreMatchSetupError> {
    std::env::var_os(variable)
        .map(PathBuf::from)
        .ok_or(PreMatchSetupError::EnvironmentVariableMissing(variable))
}

fn fixed_regular_file(
    root: &Path,
    filename: &str,
    component: &'static str,
) -> Result<PathBuf, PreMatchSetupError> {
    let canonical_root = root
        .canonicalize()
        .map_err(|source| PreMatchSetupError::Resolve { component, source })?;
    let candidate = canonical_regular_file(&root.join(filename), component)?;
    if !candidate.starts_with(&canonical_root) {
        return Err(PreMatchSetupError::Resolve {
            component,
            source: std::io::Error::other("resolved outside its fixed installation directory"),
        });
    }
    Ok(candidate)
}

fn canonical_regular_file(
    path: &Path,
    component: &'static str,
) -> Result<PathBuf, PreMatchSetupError> {
    let metadata =
        fs::metadata(path).map_err(|source| PreMatchSetupError::Resolve { component, source })?;
    if !metadata.is_file() {
        return Err(PreMatchSetupError::NotRegularFile(component));
    }
    path.canonicalize()
        .map_err(|source| PreMatchSetupError::Resolve { component, source })
}

fn verify_trusted_signature(
    path: &Path,
    component: &'static str,
) -> Result<(), PreMatchSetupError> {
    let mut path_wide = wide_null(path);
    let mut file_info = WINTRUST_FILE_INFO {
        cbStruct: size_of::<WINTRUST_FILE_INFO>() as u32,
        pcwszFilePath: path_wide.as_mut_ptr(),
        hFile: null_mut(),
        pgKnownSubject: null_mut(),
    };
    let mut trust_data = WINTRUST_DATA {
        cbStruct: size_of::<WINTRUST_DATA>() as u32,
        pPolicyCallbackData: null_mut(),
        pSIPClientData: null_mut(),
        dwUIChoice: WTD_UI_NONE,
        fdwRevocationChecks: WTD_REVOKE_NONE,
        dwUnionChoice: WTD_CHOICE_FILE,
        Anonymous: WINTRUST_DATA_0 {
            pFile: &mut file_info,
        },
        dwStateAction: WTD_STATEACTION_IGNORE,
        hWVTStateData: null_mut(),
        pwszURLReference: null_mut(),
        dwProvFlags: WTD_REVOCATION_CHECK_CHAIN_EXCLUDE_ROOT | WTD_DISABLE_MD2_MD4,
        dwUIContext: 0,
        pSignatureSettings: null_mut(),
    };
    let mut action = WINTRUST_ACTION_GENERIC_VERIFY_V2;
    // SAFETY: The structures are initialized according to WinVerifyTrust's
    // file-subject contract and retain their pointed-to UTF-16/path storage for
    // the synchronous call. WTD_UI_NONE prevents UI and no state handle is
    // requested, so this performs verification only.
    let status = unsafe {
        WinVerifyTrust(
            null_mut(),
            &mut action,
            (&mut trust_data as *mut WINTRUST_DATA).cast::<c_void>(),
        )
    };
    if status == 0 {
        Ok(())
    } else {
        Err(PreMatchSetupError::SignatureRejected { component, status })
    }
}

fn query_fixed_faceit_service_binary() -> Result<PathBuf, PreMatchSetupError> {
    // SAFETY: Null machine and database names select the local active SCM;
    // connect access is read-only and the returned handle is closed below.
    let manager = unsafe { OpenSCManagerW(null(), null(), SC_MANAGER_CONNECT) };
    if manager.is_null() {
        return Err(PreMatchSetupError::Service(last_error("OpenSCManagerW")));
    }
    let service_name = wide_null_os(std::ffi::OsStr::new("FACEITService"));
    // SAFETY: manager is live, name is null-terminated and retained for this
    // synchronous read-only query; the returned handle is closed below.
    let service = unsafe { OpenServiceW(manager, service_name.as_ptr(), SERVICE_QUERY_CONFIG) };
    if service.is_null() {
        // SAFETY: manager is an owned handle that is not used after close.
        unsafe {
            let _ = CloseServiceHandle(manager);
        }
        return Err(PreMatchSetupError::Service(last_error("OpenServiceW")));
    }
    let result = query_service_binary_path(service);
    // SAFETY: service and manager are owned successful open handles and no
    // longer used after these close operations.
    unsafe {
        let _ = CloseServiceHandle(service);
        let _ = CloseServiceHandle(manager);
    }
    result
}

fn query_faceit_service_activity(
    setup: &VerifiedPreMatchSetup,
) -> Result<(bool, u32), PreMatchSetupError> {
    // SAFETY: Null machine and database names select the local active SCM;
    // connect access is read-only and the returned handle is closed below.
    let manager = unsafe { OpenSCManagerW(null(), null(), SC_MANAGER_CONNECT) };
    if manager.is_null() {
        return Err(PreMatchSetupError::Service(last_error("OpenSCManagerW")));
    }
    let service_name = wide_null_os(std::ffi::OsStr::new("FACEITService"));
    // SAFETY: The service name is fixed and null-terminated. Query access is
    // read-only; this handle cannot start, stop, or reconfigure the service.
    let service = unsafe {
        OpenServiceW(
            manager,
            service_name.as_ptr(),
            SERVICE_QUERY_CONFIG | SERVICE_QUERY_STATUS,
        )
    };
    if service.is_null() {
        let error = last_error("OpenServiceW");
        // SAFETY: `manager` is the owned handle returned above and is no longer
        // used after this close.
        unsafe {
            let _ = CloseServiceHandle(manager);
        }
        return Err(PreMatchSetupError::Service(error));
    }

    let result = (|| {
        let configured = query_service_binary_path(service)?;
        let configured = canonical_regular_file(&configured, "FACEITService binary")?;
        if configured != setup.faceit_service {
            return Err(PreMatchSetupError::ServiceBinaryMismatch);
        }

        let mut status = SERVICE_STATUS_PROCESS::default();
        let mut bytes_needed = 0_u32;
        // SAFETY: `service` is open for query access, `status` is a writable
        // correctly sized output buffer, and `bytes_needed` is writable.
        let queried = unsafe {
            QueryServiceStatusEx(
                service,
                SC_STATUS_PROCESS_INFO,
                (&mut status as *mut SERVICE_STATUS_PROCESS).cast::<u8>(),
                size_of::<SERVICE_STATUS_PROCESS>() as u32,
                &mut bytes_needed,
            )
        };
        if queried == 0 {
            return Err(PreMatchSetupError::Service(last_error(
                "QueryServiceStatusEx",
            )));
        }
        if status.dwCurrentState == SERVICE_RUNNING && status.dwProcessId == 0 {
            return Err(PreMatchSetupError::ServiceRunningWithoutProcessIdentity);
        }
        Ok((
            service_state_is_active(status.dwCurrentState),
            status.dwProcessId,
        ))
    })();

    // SAFETY: Both service and manager are owned successful opens and neither
    // is used after these close operations.
    unsafe {
        let _ = CloseServiceHandle(service);
        let _ = CloseServiceHandle(manager);
    }
    result
}

const fn service_state_is_active(current_state: u32) -> bool {
    current_state != SERVICE_STOPPED
}

fn query_service_binary_path(service: SC_HANDLE) -> Result<PathBuf, PreMatchSetupError> {
    let mut required = 0_u32;
    // SAFETY: A null buffer and zero length is the documented sizing query;
    // `required` is valid writable storage.
    let sized = unsafe { QueryServiceConfigW(service, null_mut(), 0, &mut required) };
    // SAFETY: This reads the thread-local error set by the immediately
    // preceding sizing call, before any other Win32 call can replace it.
    let sizing_error = unsafe { GetLastError() };
    if sized != 0 || sizing_error != ERROR_INSUFFICIENT_BUFFER || required == 0 {
        return Err(PreMatchSetupError::Service(last_error(
            "QueryServiceConfigW",
        )));
    }
    let word_count = (required as usize).div_ceil(size_of::<usize>());
    let mut buffer = vec![0_usize; word_count];
    // SAFETY: The usize buffer is sufficiently aligned for the service
    // structure and has at least the requested byte length.
    let config = buffer.as_mut_ptr().cast::<QUERY_SERVICE_CONFIGW>();
    // SAFETY: `service` remains open, `config` points to the aligned writable
    // buffer, and `required` is both its documented byte limit and output size.
    let read = unsafe { QueryServiceConfigW(service, config, required, &mut required) };
    if read == 0 {
        return Err(PreMatchSetupError::Service(last_error(
            "QueryServiceConfigW",
        )));
    }
    // SAFETY: QueryServiceConfigW succeeded and initialized the fixed header
    // at the beginning of `buffer`; its string pointer addresses this live
    // buffer for the duration of this conversion.
    let binary_path = unsafe { (*config).lpBinaryPathName };
    if binary_path.is_null() {
        return Err(PreMatchSetupError::InvalidServiceCommandLine);
    }
    // SAFETY: A successful service query returned a non-null, null-terminated
    // string pointer whose backing buffer remains live in this function.
    let command_line = unsafe { wide_pointer_to_string(binary_path) };
    parse_quoted_service_binary(&command_line)
        .map(PathBuf::from)
        .ok_or(PreMatchSetupError::InvalidServiceCommandLine)
}

fn verify_cs2_manifest(steam_root: &Path) -> Result<(), PreMatchSetupError> {
    let manifest = steam_root.join("steamapps").join("appmanifest_730.acf");
    let metadata = fs::metadata(&manifest).map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            PreMatchSetupError::Cs2ManifestMissing
        } else {
            PreMatchSetupError::Cs2ManifestUnreadable(error)
        }
    })?;
    if metadata.len() > 64 * 1024 {
        return Err(PreMatchSetupError::Cs2ManifestDoesNotDescribeApp730);
    }
    let content =
        fs::read_to_string(&manifest).map_err(PreMatchSetupError::Cs2ManifestUnreadable)?;
    if is_cs2_app_manifest(&content) {
        Ok(())
    } else {
        Err(PreMatchSetupError::Cs2ManifestDoesNotDescribeApp730)
    }
}

fn is_cs2_app_manifest(content: &str) -> bool {
    content.contains("\"appid\"\t\t\"730\"")
}

fn parse_quoted_service_binary(command_line: &str) -> Option<&str> {
    let trimmed = command_line.trim();
    let path = trimmed.strip_prefix('"')?.strip_suffix('"')?;
    (!path.is_empty()).then_some(path)
}

unsafe fn wide_pointer_to_string(pointer: *mut u16) -> String {
    let mut length = 0_usize;
    // SAFETY: The caller supplies a non-null Windows UTF-16 string pointer
    // returned by QueryServiceConfigW, which is documented as null-terminated.
    while unsafe { *pointer.add(length) } != 0 {
        length += 1;
    }
    // SAFETY: The loop above found the terminator within the same documented
    // service-owned UTF-16 string, so this exact non-terminator range is live.
    String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(pointer, length) })
}

impl FixedInternalWorker {
    #[must_use]
    pub const fn executable_name(self) -> &'static str {
        match self {
            Self::Session => "jarvis-session-worker.exe",
            Self::Model => "jarvis-model-worker.exe",
        }
    }
}

#[derive(Debug)]
pub enum WorkerLaunchError {
    Resolve {
        operation: &'static str,
        source: std::io::Error,
    },
    NotRegularFile,
    OutsideInstallationDirectory,
    ShutdownSignalAlreadyExists,
    Win32(Win32Error),
    NotAssignedToExpectedJob,
}

impl fmt::Display for WorkerLaunchError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Resolve { operation, source } => {
                write!(
                    formatter,
                    "could not {operation} the fixed internal worker: {source}"
                )
            }
            Self::NotRegularFile => {
                write!(formatter, "the fixed internal worker is not a regular file")
            }
            Self::OutsideInstallationDirectory => {
                write!(
                    formatter,
                    "the fixed internal worker must resolve inside its installation directory"
                )
            }
            Self::ShutdownSignalAlreadyExists => {
                write!(
                    formatter,
                    "the fixed worker shutdown signal name already exists"
                )
            }
            Self::Win32(error) => error.fmt(formatter),
            Self::NotAssignedToExpectedJob => {
                write!(
                    formatter,
                    "the suspended worker was not verified in its expected Job Object"
                )
            }
        }
    }
}

impl Error for WorkerLaunchError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Resolve { source, .. } => Some(source),
            Self::Win32(error) => Some(error),
            Self::NotRegularFile
            | Self::OutsideInstallationDirectory
            | Self::ShutdownSignalAlreadyExists
            | Self::NotAssignedToExpectedJob => None,
        }
    }
}

impl From<Win32Error> for WorkerLaunchError {
    fn from(error: Win32Error) -> Self {
        Self::Win32(error)
    }
}

/// Immutable identity collected from the process handle while the process is
/// still suspended. The creation time prevents later PID reuse from being
/// mistaken for this worker.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InternalWorkerIdentity {
    process_id: u32,
    creation_time_filetime: u64,
}

impl InternalWorkerIdentity {
    #[must_use]
    pub const fn process_id(self) -> u32 {
        self.process_id
    }

    #[must_use]
    pub const fn creation_time_filetime(self) -> u64 {
        self.creation_time_filetime
    }
}

/// An unnamed, non-inheritable Job Object configured to terminate all of its
/// members when the final handle closes.
///
/// The raw handle is never exposed, preventing this safe API from accidentally
/// attaching or terminating a user application.
pub struct KillOnCloseJob {
    handle: HANDLE,
}

impl fmt::Debug for KillOnCloseJob {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("KillOnCloseJob")
            .finish_non_exhaustive()
    }
}

impl KillOnCloseJob {
    /// Creates a fresh unnamed job. It has no members until trusted worker
    /// startup assigns a suspended child process in a later milestone.
    pub fn create() -> Result<Self, Win32Error> {
        // SAFETY: Both optional parameters are documented nullable pointers.
        // A null security descriptor requests the current-user default ACL and
        // a null name creates an unnamed, non-shared Job Object.
        let handle = unsafe { CreateJobObjectW(null(), null()) };
        if handle.is_null() {
            return Err(last_error("CreateJobObjectW"));
        }

        let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        // SAFETY: `handle` was returned by CreateJobObjectW above and remains
        // owned by this function. `limits` is initialized, has the documented
        // layout, and its pointer/byte length are valid for this synchronous
        // call.
        let configured = unsafe {
            SetInformationJobObject(
                handle,
                JobObjectExtendedLimitInformation,
                (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast::<c_void>(),
                size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            )
        };
        if configured == 0 {
            let error = last_error("SetInformationJobObject");
            // SAFETY: `handle` is a valid owned Job Object handle and has not
            // been transferred. Closing it releases the failed setup object.
            unsafe {
                let _ = CloseHandle(handle);
            }
            return Err(error);
        }
        Ok(Self { handle })
    }

    /// Creates the selected fixed Jarvis worker suspended. No caller can pass
    /// an executable path, command line, inherited handle list, or elevation
    /// request through this API.
    pub fn create_suspended_fixed_worker(
        &self,
        worker: FixedInternalWorker,
        installation_directory: &Path,
    ) -> Result<SuspendedInternalWorker, WorkerLaunchError> {
        let installation_directory =
            fs::canonicalize(installation_directory).map_err(|source| {
                WorkerLaunchError::Resolve {
                    operation: "resolve the installation directory",
                    source,
                }
            })?;
        let worker_path = installation_directory.join(worker.executable_name());
        let worker_path =
            fs::canonicalize(worker_path).map_err(|source| WorkerLaunchError::Resolve {
                operation: "resolve the fixed internal worker",
                source,
            })?;
        let metadata = fs::metadata(&worker_path).map_err(|source| WorkerLaunchError::Resolve {
            operation: "inspect the fixed internal worker",
            source,
        })?;
        if !metadata.is_file() {
            return Err(WorkerLaunchError::NotRegularFile);
        }
        if worker_path.parent() != Some(installation_directory.as_path()) {
            return Err(WorkerLaunchError::OutsideInstallationDirectory);
        }

        let application_name = wide_null(&worker_path);
        let current_directory = wide_null(&installation_directory);
        let startup = STARTUPINFOW {
            cb: size_of::<STARTUPINFOW>() as u32,
            ..Default::default()
        };
        let mut process_information = PROCESS_INFORMATION::default();
        // SAFETY: Both UTF-16 vectors are null-terminated and remain live for
        // this synchronous call. The application path is an exact canonical
        // fixed-manifest path, not a caller command line. Process/thread
        // security attributes and inherited handles are null/false, the
        // environment is inherited, and the initialized output structures have
        // the documented layouts. CREATE_SUSPENDED prevents the primary thread
        // from running before Job Object assignment succeeds.
        let created = unsafe {
            CreateProcessW(
                application_name.as_ptr(),
                null_mut(),
                null(),
                null(),
                0,
                CREATE_SUSPENDED,
                null(),
                current_directory.as_ptr(),
                &startup,
                &mut process_information,
            )
        };
        if created == 0 {
            return Err(WorkerLaunchError::Win32(last_error("CreateProcessW")));
        }

        let identity = match process_identity(
            process_information.hProcess,
            process_information.dwProcessId,
        ) {
            Ok(identity) => identity,
            Err(error) => {
                // SAFETY: CreateProcessW returned valid owned handles. The
                // child is still suspended, so terminating and closing it here
                // prevents an unassigned process from surviving this failure.
                unsafe {
                    let _ = TerminateProcess(process_information.hProcess, 1);
                    let _ = CloseHandle(process_information.hThread);
                    let _ = CloseHandle(process_information.hProcess);
                }
                return Err(WorkerLaunchError::Win32(error));
            }
        };

        let shutdown_signal = match CooperativeShutdownSignal::create(worker, identity) {
            Ok(signal) => signal,
            Err(error) => {
                // SAFETY: CreateProcessW returned valid owned handles. The
                // child is still suspended, so terminating and closing it here
                // prevents an unassigned process from surviving setup failure.
                unsafe {
                    let _ = TerminateProcess(process_information.hProcess, 1);
                    let _ = CloseHandle(process_information.hThread);
                    let _ = CloseHandle(process_information.hProcess);
                }
                return Err(error);
            }
        };

        Ok(SuspendedInternalWorker {
            process: process_information.hProcess,
            primary_thread: process_information.hThread,
            identity,
            shutdown_signal: Some(shutdown_signal),
        })
    }

    /// Assigns a still-suspended worker to this Job Object and verifies that
    /// membership before the worker's primary thread is resumed.
    pub fn assign_and_verify(
        &self,
        worker: &SuspendedInternalWorker,
    ) -> Result<(), WorkerLaunchError> {
        // SAFETY: Both handles are valid and owned for the duration of this
        // call. `worker` was created suspended by this module and has not been
        // resumed; this Job Object is the opaque object owned by `self`.
        let assigned = unsafe { AssignProcessToJobObject(self.handle, worker.process) };
        if assigned == 0 {
            return Err(WorkerLaunchError::Win32(last_error(
                "AssignProcessToJobObject",
            )));
        }

        let mut is_member = 0;
        // SAFETY: Both handles remain valid, and `is_member` is a writable
        // BOOL-sized output location for the synchronous API call.
        let verified = unsafe { IsProcessInJob(worker.process, self.handle, &mut is_member) };
        if verified == 0 {
            return Err(WorkerLaunchError::Win32(last_error("IsProcessInJob")));
        }
        if is_member == 0 {
            return Err(WorkerLaunchError::NotAssignedToExpectedJob);
        }
        Ok(())
    }

    /// Returns the kernel-reported number of active processes in this Job.
    pub fn active_process_count(&self) -> Result<u32, Win32Error> {
        let mut accounting = JOBOBJECT_BASIC_ACCOUNTING_INFORMATION::default();
        // SAFETY: `self.handle` is live for `&self`; `accounting` has the
        // correct layout and stays valid for the synchronous query; the output
        // byte count is intentionally unused and may be null by API contract.
        let queried = unsafe {
            QueryInformationJobObject(
                self.handle,
                JobObjectBasicAccountingInformation,
                (&mut accounting as *mut JOBOBJECT_BASIC_ACCOUNTING_INFORMATION).cast::<c_void>(),
                size_of::<JOBOBJECT_BASIC_ACCOUNTING_INFORMATION>() as u32,
                null_mut(),
            )
        };
        if queried == 0 {
            return Err(last_error("QueryInformationJobObject"));
        }
        Ok(accounting.ActiveProcesses)
    }

    /// Terminates only processes previously assigned to this specific Job.
    /// This does not enumerate, open, or affect other desktop applications.
    pub fn terminate_all(&self, exit_code: u32) -> Result<(), Win32Error> {
        // SAFETY: `self.handle` is a valid Job Object owned by this wrapper;
        // TerminateJobObject is synchronous with respect to accepting the
        // request, while higher-level code independently verifies emptiness.
        let terminated = unsafe { TerminateJobObject(self.handle, exit_code) };
        if terminated == 0 {
            return Err(last_error("TerminateJobObject"));
        }
        Ok(())
    }
}

/// A fixed internal worker whose primary thread is still suspended. Dropping
/// this value always terminates the child, so a failed assignment or resume
/// cannot leave an unmanaged worker alive.
pub struct SuspendedInternalWorker {
    process: HANDLE,
    primary_thread: HANDLE,
    identity: InternalWorkerIdentity,
    shutdown_signal: Option<CooperativeShutdownSignal>,
}

impl SuspendedInternalWorker {
    #[must_use]
    pub const fn identity(&self) -> InternalWorkerIdentity {
        self.identity
    }

    /// Resumes the primary thread only after `assign_and_verify` has completed
    /// successfully. The caller cannot construct this type directly.
    pub fn resume(mut self) -> Result<RunningInternalWorker, WorkerLaunchError> {
        // SAFETY: `primary_thread` is the valid initial thread handle returned
        // by CreateProcessW. It is still owned by `self` and was created with
        // CREATE_SUSPENDED, so this call is the first release of that suspend
        // state after the higher-level Job Object verification.
        let previous_suspend_count = unsafe { ResumeThread(self.primary_thread) };
        if previous_suspend_count == u32::MAX {
            return Err(WorkerLaunchError::Win32(last_error("ResumeThread")));
        }

        // SAFETY: The primary thread handle is owned exactly once by `self`.
        // It is no longer needed after the thread has been resumed.
        unsafe {
            let _ = CloseHandle(self.primary_thread);
        }
        self.primary_thread = null_mut();
        let process = self.process;
        self.process = null_mut();
        Ok(RunningInternalWorker {
            process,
            identity: self.identity,
            shutdown_signal: self
                .shutdown_signal
                .take()
                .expect("suspended worker always owns its shutdown signal"),
        })
    }
}

impl Drop for SuspendedInternalWorker {
    fn drop(&mut self) {
        // SAFETY: Any remaining handles are owned by this value. The process
        // is either still suspended or failed to resume, so terminating it is
        // required to uphold the no-unmanaged-worker guarantee. Closing null
        // handles is avoided after ownership has moved into RunningInternalWorker.
        unsafe {
            if !self.process.is_null() {
                let _ = TerminateProcess(self.process, 1);
            }
            if !self.primary_thread.is_null() {
                let _ = CloseHandle(self.primary_thread);
            }
            if !self.process.is_null() {
                let _ = CloseHandle(self.process);
            }
        }
    }
}

/// A verified, resumed fixed internal worker. Its raw process handle remains
/// private; the owner can retain it only through the trusted worker manager.
pub struct RunningInternalWorker {
    process: HANDLE,
    identity: InternalWorkerIdentity,
    shutdown_signal: CooperativeShutdownSignal,
}

impl RunningInternalWorker {
    #[must_use]
    pub const fn identity(&self) -> InternalWorkerIdentity {
        self.identity
    }

    /// Signals the fixed worker's sole cooperative operation: exit. It cannot
    /// carry a command, argument, executable path, or arbitrary IPC payload.
    pub fn request_cooperative_shutdown(&self) -> Result<(), Win32Error> {
        self.shutdown_signal.signal()
    }

    /// Observes whether the exact process handle has exited. The identity was
    /// captured before resume, so this never follows a reused PID.
    pub fn wait_for_exit(&self, timeout: Duration) -> Result<bool, Win32Error> {
        match wait_for_handle(self.process, timeout)? {
            WaitState::Signaled => Ok(true),
            WaitState::TimedOut => Ok(false),
        }
    }

    /// Returns the worker exit code only after the exact process handle is
    /// observed signaled. It never queries a PID that could have been reused.
    pub fn exited_with_code(&self) -> Result<Option<u32>, Win32Error> {
        if !self.wait_for_exit(Duration::ZERO)? {
            return Ok(None);
        }
        let mut exit_code = 0;
        // SAFETY: `process` is the still-owned process handle returned by
        // CreateProcessW, and `exit_code` is a valid writable output location.
        let queried = unsafe { GetExitCodeProcess(self.process, &mut exit_code) };
        if queried == 0 {
            return Err(last_error("GetExitCodeProcess"));
        }
        Ok(Some(exit_code))
    }
}

impl Drop for RunningInternalWorker {
    fn drop(&mut self) {
        // SAFETY: The process handle is owned exactly once by this wrapper.
        // The Job Object, rather than closing this observation handle, owns
        // the worker lifetime and applies KILL_ON_JOB_CLOSE when appropriate.
        unsafe {
            let _ = CloseHandle(self.process);
        }
    }
}

impl Drop for KillOnCloseJob {
    fn drop(&mut self) {
        // SAFETY: `handle` is owned exactly once by this wrapper. Closing the
        // final handle intentionally applies KILL_ON_JOB_CLOSE only to
        // processes previously assigned to this unnamed Jarvis-owned job.
        unsafe {
            let _ = CloseHandle(self.handle);
        }
    }
}

/// Takes a read-only Tool Help process snapshot and returns verified identities
/// only for the fixed `cs2.exe` trigger. If a matching candidate cannot be
/// opened with limited query access, the entire observation fails so the caller
/// can remain restricted rather than guessing.
pub fn observe_cs2_processes() -> Result<Vec<ObservedProcessIdentity>, ProcessObservationError> {
    // SAFETY: TH32CS_SNAPPROCESS requests a read-only process snapshot for all
    // processes, and process ID zero is the documented value for this flag.
    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
    if snapshot == INVALID_HANDLE_VALUE {
        return Err(ProcessObservationError::Win32(last_error(
            "CreateToolhelp32Snapshot",
        )));
    }

    let result = observe_cs2_processes_in_snapshot(snapshot);
    // SAFETY: `snapshot` is the owned handle returned by CreateToolhelp32Snapshot.
    unsafe {
        let _ = CloseHandle(snapshot);
    }
    result
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct CompetitiveProcessActivity {
    cs2_running: bool,
    faceit_ac_launcher_running: bool,
    faceit_service_process_running: bool,
}

fn observe_competitive_processes(
    setup: &VerifiedPreMatchSetup,
) -> Result<CompetitiveProcessActivity, ProcessObservationError> {
    // SAFETY: TH32CS_SNAPPROCESS requests a read-only process snapshot for all
    // processes, and process ID zero is the documented value for this flag.
    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
    if snapshot == INVALID_HANDLE_VALUE {
        return Err(ProcessObservationError::Win32(last_error(
            "CreateToolhelp32Snapshot",
        )));
    }

    let result = observe_competitive_processes_in_snapshot(snapshot, setup);
    // SAFETY: `snapshot` is the owned handle returned by CreateToolhelp32Snapshot.
    unsafe {
        let _ = CloseHandle(snapshot);
    }
    result
}

fn observe_competitive_processes_in_snapshot(
    snapshot: HANDLE,
    setup: &VerifiedPreMatchSetup,
) -> Result<CompetitiveProcessActivity, ProcessObservationError> {
    let mut entry = PROCESSENTRY32W {
        dwSize: size_of::<PROCESSENTRY32W>() as u32,
        ..Default::default()
    };
    // SAFETY: `snapshot` is a live Tool Help snapshot and `entry` has its
    // required size field initialized with writable storage for this call.
    let first = unsafe { Process32FirstW(snapshot, &mut entry) };
    if first == 0 {
        let error = last_error("Process32FirstW");
        return if error.code() == ERROR_NO_MORE_FILES {
            Ok(CompetitiveProcessActivity::default())
        } else {
            Err(ProcessObservationError::Win32(error))
        };
    }

    let mut activity = CompetitiveProcessActivity::default();
    loop {
        let process = match process_entry_name(&entry).as_str() {
            name if name == FixedCompetitiveProcess::Cs2.executable_name() => {
                Some(FixedCompetitiveProcess::Cs2)
            }
            name if name == FixedCompetitiveProcess::FaceitAcLauncher.executable_name() => {
                Some(FixedCompetitiveProcess::FaceitAcLauncher)
            }
            name if name == FixedCompetitiveProcess::FaceitService.executable_name() => {
                Some(FixedCompetitiveProcess::FaceitService)
            }
            _ => None,
        };
        if let Some(process) = process {
            let expected_path = match process {
                FixedCompetitiveProcess::Cs2 => None,
                FixedCompetitiveProcess::FaceitAcLauncher => {
                    Some(setup.faceit_ac_launcher.as_path())
                }
                FixedCompetitiveProcess::FaceitService => Some(setup.faceit_service.as_path()),
            };
            observed_process_identity(process, entry.th32ProcessID, expected_path)?;
            match process {
                FixedCompetitiveProcess::Cs2 => activity.cs2_running = true,
                FixedCompetitiveProcess::FaceitAcLauncher => {
                    activity.faceit_ac_launcher_running = true
                }
                FixedCompetitiveProcess::FaceitService => {
                    activity.faceit_service_process_running = true
                }
            }
        }

        // SAFETY: `snapshot` remains live and `entry` is the documented
        // in/out structure for advancing this synchronous snapshot walk.
        let next = unsafe { Process32NextW(snapshot, &mut entry) };
        if next != 0 {
            continue;
        }
        let error = last_error("Process32NextW");
        if error.code() == ERROR_NO_MORE_FILES {
            return Ok(activity);
        }
        return Err(ProcessObservationError::Win32(error));
    }
}

fn observe_cs2_processes_in_snapshot(
    snapshot: HANDLE,
) -> Result<Vec<ObservedProcessIdentity>, ProcessObservationError> {
    let mut entry = PROCESSENTRY32W {
        dwSize: size_of::<PROCESSENTRY32W>() as u32,
        ..Default::default()
    };
    // SAFETY: `snapshot` is a live Tool Help snapshot and `entry` has its
    // required size field initialized with writable storage for this call.
    let first = unsafe { Process32FirstW(snapshot, &mut entry) };
    if first == 0 {
        // An empty process snapshot is a valid no-match result. Any other
        // failure is propagated so the higher-level detector can fail closed.
        let error = last_error("Process32FirstW");
        return if error.code() == ERROR_NO_MORE_FILES {
            Ok(Vec::new())
        } else {
            Err(ProcessObservationError::Win32(error))
        };
    }

    let mut identities = Vec::new();
    loop {
        if process_entry_name(&entry) == FixedCompetitiveProcess::Cs2.executable_name() {
            identities.push(observed_process_identity(
                FixedCompetitiveProcess::Cs2,
                entry.th32ProcessID,
                None,
            )?);
        }

        // SAFETY: `snapshot` remains live and `entry` is the documented
        // in/out structure for advancing this synchronous snapshot walk.
        let next = unsafe { Process32NextW(snapshot, &mut entry) };
        if next != 0 {
            continue;
        }
        let error = last_error("Process32NextW");
        if error.code() == ERROR_NO_MORE_FILES {
            return Ok(identities);
        }
        return Err(ProcessObservationError::Win32(error));
    }
}

fn observed_process_identity(
    process: FixedCompetitiveProcess,
    process_id: u32,
    expected_image_path: Option<&Path>,
) -> Result<ObservedProcessIdentity, ProcessObservationError> {
    // SAFETY: This requests only documented limited query access, does not
    // inherit the handle, and neither reads nor writes target process memory.
    let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, process_id) };
    if handle.is_null() {
        return Err(ProcessObservationError::CandidateIdentityUnavailable { process });
    }
    let identity = (|| {
        let identity =
            process_identity(handle, process_id).map_err(ProcessObservationError::Win32)?;
        if let Some(expected_path) = expected_image_path {
            let actual_path = process_image_path(handle, process)?;
            if !same_windows_path(expected_path, &actual_path) {
                return Err(ProcessObservationError::CandidateImagePathMismatch { process });
            }
        }
        Ok(ObservedProcessIdentity {
            process_id: identity.process_id(),
            creation_time_filetime: identity.creation_time_filetime(),
        })
    })();
    // SAFETY: `handle` is owned by this function and is not used after close.
    unsafe {
        let _ = CloseHandle(handle);
    }
    identity
}

fn process_image_path(
    process_handle: HANDLE,
    process: FixedCompetitiveProcess,
) -> Result<PathBuf, ProcessObservationError> {
    let mut buffer = vec![0_u16; 32_768];
    let mut length = buffer.len() as u32;
    // SAFETY: `process_handle` was opened only with limited query access and
    // the writable UTF-16 buffer remains live for this synchronous call.
    let queried =
        unsafe { QueryFullProcessImageNameW(process_handle, 0, buffer.as_mut_ptr(), &mut length) };
    if queried == 0 || length as usize > buffer.len() {
        return Err(ProcessObservationError::CandidateImagePathUnavailable { process });
    }
    let image_path = PathBuf::from(std::ffi::OsString::from_wide(&buffer[..length as usize]));
    image_path
        .canonicalize()
        .map_err(|_| ProcessObservationError::CandidateImagePathUnavailable { process })
}

fn same_windows_path(expected: &Path, actual: &Path) -> bool {
    expected
        .to_string_lossy()
        .eq_ignore_ascii_case(&actual.to_string_lossy())
}

fn process_entry_name(entry: &PROCESSENTRY32W) -> String {
    let end = entry
        .szExeFile
        .iter()
        .position(|character| *character == 0)
        .unwrap_or(entry.szExeFile.len());
    String::from_utf16_lossy(&entry.szExeFile[..end]).to_ascii_lowercase()
}

/// Waits for the current fixed worker's manager-created shutdown event. This
/// is the only worker-side control operation currently supported.
pub fn wait_for_current_fixed_worker_shutdown(
    worker: FixedInternalWorker,
) -> Result<(), Win32Error> {
    // SAFETY: GetCurrentProcess returns the documented pseudo-handle for this
    // process, and GetCurrentProcessId has no pointer requirements.
    let identity = process_identity(unsafe { GetCurrentProcess() }, unsafe {
        GetCurrentProcessId()
    })?;
    let name = shutdown_signal_name(worker, identity);
    // SAFETY: `name` is a null-terminated UTF-16 value that remains live for
    // this synchronous call. The worker asks only for wait/synchronize access
    // to its own manager-created event and does not inherit the handle.
    let event = unsafe { OpenEventW(SYNCHRONIZATION_SYNCHRONIZE, 0, name.as_ptr()) };
    if event.is_null() {
        return Err(last_error("OpenEventW"));
    }
    let result = wait_for_handle(event, Duration::from_millis(u64::from(INFINITE)));
    // SAFETY: `event` was returned as an owned handle by OpenEventW and is not
    // used after this close.
    unsafe {
        let _ = CloseHandle(event);
    }
    match result? {
        WaitState::Signaled => Ok(()),
        WaitState::TimedOut => Err(Win32Error {
            operation: "WaitForSingleObject unexpectedly timed out",
            code: WAIT_TIMEOUT,
        }),
    }
}

/// An owned, per-worker named event created before the worker is resumed.
/// Its name includes both PID and creation time, preventing PID reuse from
/// addressing a later worker. A pre-existing name is rejected rather than
/// reused.
struct CooperativeShutdownSignal {
    handle: HANDLE,
}

impl CooperativeShutdownSignal {
    fn create(
        worker: FixedInternalWorker,
        identity: InternalWorkerIdentity,
    ) -> Result<Self, WorkerLaunchError> {
        let name = shutdown_signal_name(worker, identity);
        // SAFETY: The event is non-inheritable by passing null security
        // attributes, starts nonsignaled, and uses a null-terminated UTF-16
        // name that remains live for this synchronous call.
        let handle = unsafe { CreateEventW(null(), 1, 0, name.as_ptr()) };
        if handle.is_null() {
            return Err(WorkerLaunchError::Win32(last_error("CreateEventW")));
        }
        // SAFETY: CreateEventW documents GetLastError as ERROR_ALREADY_EXISTS
        // when it opens an existing named event. This read is immediately
        // after the call, before any other Win32 operation.
        let already_exists = unsafe { GetLastError() } == ERROR_ALREADY_EXISTS;
        if already_exists {
            // SAFETY: `handle` is the just-opened event handle and is owned by
            // this function until it is returned.
            unsafe {
                let _ = CloseHandle(handle);
            }
            return Err(WorkerLaunchError::ShutdownSignalAlreadyExists);
        }
        Ok(Self { handle })
    }

    fn signal(&self) -> Result<(), Win32Error> {
        // SAFETY: `handle` is a live event handle owned by this wrapper. The
        // event carries no payload; signaling it can only request worker exit.
        let signaled = unsafe { SetEvent(self.handle) };
        if signaled == 0 {
            return Err(last_error("SetEvent"));
        }
        Ok(())
    }
}

impl Drop for CooperativeShutdownSignal {
    fn drop(&mut self) {
        // SAFETY: The named event handle is owned exactly once by this
        // wrapper. Closing it cannot affect any desktop process.
        unsafe {
            let _ = CloseHandle(self.handle);
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum WaitState {
    Signaled,
    TimedOut,
}

fn wait_for_handle(handle: HANDLE, timeout: Duration) -> Result<WaitState, Win32Error> {
    let timeout_millis = timeout.as_millis().min(u128::from(u32::MAX)) as u32;
    // SAFETY: `handle` is a live process or event handle owned by the caller,
    // and the bounded timeout is passed by value to this synchronous wait.
    let result = unsafe { WaitForSingleObject(handle, timeout_millis) };
    match result {
        WAIT_OBJECT_0 => Ok(WaitState::Signaled),
        WAIT_TIMEOUT => Ok(WaitState::TimedOut),
        WAIT_FAILED => Err(last_error("WaitForSingleObject")),
        unexpected => Err(Win32Error {
            operation: "WaitForSingleObject returned an unexpected state",
            code: unexpected,
        }),
    }
}

fn last_error(operation: &'static str) -> Win32Error {
    // SAFETY: GetLastError reads thread-local state and requires no pointers.
    let code = unsafe { GetLastError() };
    Win32Error { operation, code }
}

fn process_identity(
    process: HANDLE,
    process_id: u32,
) -> Result<InternalWorkerIdentity, Win32Error> {
    let mut created = FILETIME::default();
    let mut exited = FILETIME::default();
    let mut kernel = FILETIME::default();
    let mut user = FILETIME::default();
    // SAFETY: `process` is a live handle returned by CreateProcessW, and the
    // four FILETIME outputs are initialized writable values for this synchronous
    // query. The process is still suspended while the identity is captured.
    let queried =
        unsafe { GetProcessTimes(process, &mut created, &mut exited, &mut kernel, &mut user) };
    if queried == 0 {
        return Err(last_error("GetProcessTimes"));
    }
    Ok(InternalWorkerIdentity {
        process_id,
        creation_time_filetime: filetime_value(created),
    })
}

fn shutdown_signal_name(worker: FixedInternalWorker, identity: InternalWorkerIdentity) -> Vec<u16> {
    let name = format!(
        "Local\\Jarvis.{}.Shutdown.{}.{}",
        worker.executable_name(),
        identity.process_id(),
        identity.creation_time_filetime()
    );
    name.encode_utf16().chain(std::iter::once(0)).collect()
}

fn wide_null(path: &Path) -> Vec<u16> {
    wide_null_os(path.as_os_str())
}

fn wide_null_os(value: &std::ffi::OsStr) -> Vec<u16> {
    value.encode_wide().chain(std::iter::once(0)).collect()
}

fn filetime_value(value: FILETIME) -> u64 {
    (u64::from(value.dwHighDateTime) << 32) | u64::from(value.dwLowDateTime)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_new_job_has_no_active_processes() {
        let job = KillOnCloseJob::create().expect("create an empty Job Object");
        assert_eq!(job.active_process_count().expect("query Job Object"), 0);
    }

    #[test]
    fn fixed_faceit_service_command_line_allows_only_one_quoted_binary() {
        assert_eq!(
            parse_quoted_service_binary("\"C:\\Program Files\\FACEIT AC\\faceitservice.exe\""),
            Some("C:\\Program Files\\FACEIT AC\\faceitservice.exe")
        );
        assert_eq!(
            parse_quoted_service_binary(
                "\"C:\\Program Files\\FACEIT AC\\faceitservice.exe\" --extra"
            ),
            None
        );
        assert_eq!(
            parse_quoted_service_binary("C:\\Program Files\\FACEIT AC\\faceitservice.exe"),
            None
        );
    }

    #[test]
    fn cs2_manifest_must_name_only_the_expected_steam_appid_field() {
        assert!(is_cs2_app_manifest(
            "\"AppState\"\n{\n\"appid\"\t\t\"730\"\n}"
        ));
        assert!(!is_cs2_app_manifest(
            "\"AppState\"\n{\n\"appid\"\t\t\"570\"\n}"
        ));
    }

    #[test]
    fn pre_match_launch_steps_have_only_the_fixed_apps_and_arguments() {
        assert_eq!(
            PreMatchLaunchStep::FaceitAcLauncher.command_line(),
            "faceitclient.exe"
        );
        assert_eq!(
            PreMatchLaunchStep::ChromeFaceitWebsite.command_line(),
            "chrome.exe https://www.faceit.com/"
        );
        assert_eq!(
            PreMatchLaunchStep::SteamCs2.command_line(),
            "steam.exe -applaunch 730"
        );
    }

    #[test]
    fn faceit_window_title_match_is_case_insensitive_and_unknown_titles_do_not_match() {
        assert!(title_has_faceit_site_marker("FACEIT - Google Chrome"));
        assert!(title_has_faceit_site_marker("faceit"));
        assert!(!title_has_faceit_site_marker("Steam Community"));
        assert!(!title_has_faceit_site_marker(
            "How FACEIT matchmaking works"
        ));
        assert!(!title_has_faceit_site_marker(""));
    }

    #[test]
    fn chrome_launch_is_skipped_only_for_a_clear_faceit_window_detection() {
        assert!(should_skip_chrome_launch(FaceitTitleDetection::Match));
        assert!(!should_skip_chrome_launch(FaceitTitleDetection::NoMatch));
        assert!(!should_skip_chrome_launch(FaceitTitleDetection::Unknown));
    }

    #[test]
    fn competitive_process_names_are_a_closed_fixed_set() {
        assert_eq!(FixedCompetitiveProcess::Cs2.executable_name(), "cs2.exe");
        assert_eq!(
            FixedCompetitiveProcess::FaceitAcLauncher.executable_name(),
            "faceitclient.exe"
        );
        assert_eq!(
            FixedCompetitiveProcess::FaceitService.executable_name(),
            "faceitservice.exe"
        );
    }

    #[test]
    fn faceit_service_state_is_a_trigger_until_stopped() {
        assert!(!service_state_is_active(SERVICE_STOPPED));
        assert!(service_state_is_active(2)); // Start pending.
        assert!(service_state_is_active(3)); // Stop pending.
        assert!(service_state_is_active(SERVICE_RUNNING));
    }

    #[test]
    fn fixed_process_identity_comparison_is_case_insensitive_but_exact() {
        assert!(same_windows_path(
            Path::new("C:\\Program Files\\FACEIT AC\\faceitclient.exe"),
            Path::new("c:\\program files\\faceit ac\\FACEITCLIENT.EXE")
        ));
        assert!(!same_windows_path(
            Path::new("C:\\Program Files\\FACEIT AC\\faceitclient.exe"),
            Path::new("C:\\Temp\\faceitclient.exe")
        ));
    }

    #[test]
    fn discord_process_identity_accepts_only_versioned_fixed_install_layout() {
        let root = Path::new(r"C:\Users\Admin\AppData\Local\Discord");
        assert!(discord_app_image_belongs_to_root(
            Path::new(r"C:\Users\Admin\AppData\Local\Discord\app-1.0.9260\Discord.exe"),
            root,
        ));
        for image in [
            r"C:\Users\Admin\AppData\Local\Discord\Discord.exe",
            r"C:\Users\Admin\AppData\Local\Discord\app-1..0\Discord.exe",
            r"C:\Users\Admin\AppData\Local\Discord\app-1.0.9260\other.exe",
            r"C:\Temp\Discord\app-1.0.9260\Discord.exe",
        ] {
            assert!(!discord_app_image_belongs_to_root(Path::new(image), root));
        }
    }
}
