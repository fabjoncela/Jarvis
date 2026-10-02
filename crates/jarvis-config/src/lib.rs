#![forbid(unsafe_code)]

//! Strict, all-or-nothing loading for Jarvis source-controlled configuration.
//!
//! Configuration is untrusted input until this crate has parsed the complete
//! fixed bundle and enforced immutable policy. A successful parse does not
//! certify FACEIT compatibility or a local FACEIT identity.

use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::fmt;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use serde::Deserialize;

const MAX_CONFIG_FILE_BYTES: u64 = 256 * 1024;
const LOCAL_APP_DATA_ROOT: &str = "%LOCALAPPDATA%/Jarvis/";
const EXPECTED_DATABASE: &str = "%LOCALAPPDATA%/Jarvis/jarvis.db";
const EXPECTED_RECIPE_DIRECTORY: &str = "%LOCALAPPDATA%/Jarvis/learned_commands";
const FACEIT_ANTICHEAT_APPLICATION_ID: &str = "faceit_anticheat";
const CHROME_APPLICATION_ID: &str = "chrome";
const CS2_APPLICATION_ID: &str = "cs2";

/// Configuration cannot claim that FACEIT is ready until a future detector
/// verifies the install-specific component identities.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FaceitSetupStatus {
    VerificationRequired,
}

/// A catalog that contains identifiers and aliases only; it never exposes an
/// executable path, browser profile, credential, or cleanup target.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ApplicationCatalog {
    application_ids: BTreeSet<String>,
    protected_close_targets: BTreeSet<String>,
    launch_aliases: BTreeMap<String, String>,
}

impl ApplicationCatalog {
    pub fn application_ids(&self) -> impl Iterator<Item = &str> {
        self.application_ids.iter().map(String::as_str)
    }

    pub fn protected_close_targets(&self) -> impl Iterator<Item = &str> {
        self.protected_close_targets.iter().map(String::as_str)
    }

    pub fn launch_aliases(&self) -> impl Iterator<Item = (&str, &str)> {
        self.launch_aliases
            .iter()
            .map(|(alias, application_id)| (alias.as_str(), application_id.as_str()))
    }
}

/// A complete configuration snapshot which passed all parser and policy checks.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ValidatedConfiguration {
    applications: ApplicationCatalog,
    faceit_setup_status: FaceitSetupStatus,
    exit_cooldown_seconds: u16,
}

impl ValidatedConfiguration {
    /// Loads the entire fixed configuration bundle. Callers must reject the
    /// startup/reload if any file fails; there is deliberately no fallback.
    pub fn load_from_dir(directory: impl AsRef<Path>) -> Result<Self, ConfigError> {
        let documents = ConfigDocuments::read_from(directory.as_ref())?;
        Self::from_documents(documents)
    }

    #[must_use]
    pub const fn applications(&self) -> &ApplicationCatalog {
        &self.applications
    }

    #[must_use]
    pub const fn faceit_setup_status(&self) -> FaceitSetupStatus {
        self.faceit_setup_status
    }

    #[must_use]
    pub const fn exit_cooldown_seconds(&self) -> u16 {
        self.exit_cooldown_seconds
    }

    fn from_documents(documents: ConfigDocuments) -> Result<Self, ConfigError> {
        let raw = RawConfiguration {
            jarvis: parse_document(ConfigFile::Jarvis, &documents.jarvis)?,
            god_mode: parse_document(ConfigFile::GodMode, &documents.god_mode)?,
            competitive: parse_document(ConfigFile::Competitive, &documents.competitive)?,
            applications: parse_document(ConfigFile::Applications, &documents.applications)?,
            cleanup: parse_document(ConfigFile::Cleanup, &documents.cleanup)?,
            memory: parse_document(ConfigFile::Memory, &documents.memory)?,
            learning: parse_document(ConfigFile::Learning, &documents.learning)?,
        };
        validate(raw)
    }
}

#[derive(Debug)]
pub enum ConfigError {
    Read {
        file: ConfigFile,
        path: PathBuf,
        source: std::io::Error,
    },
    TooLarge {
        file: ConfigFile,
        path: PathBuf,
        bytes: u64,
    },
    NotUtf8 {
        file: ConfigFile,
        path: PathBuf,
    },
    Parse {
        file: ConfigFile,
        message: String,
    },
    Validation(Vec<ConfigViolation>),
}

impl fmt::Display for ConfigError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Read { file, path, source } => {
                write!(
                    formatter,
                    "could not read {} at {}: {source}",
                    file.name(),
                    path.display()
                )
            }
            Self::TooLarge { file, path, bytes } => write!(
                formatter,
                "{} at {} is {bytes} bytes; limit is {MAX_CONFIG_FILE_BYTES}",
                file.name(),
                path.display()
            ),
            Self::NotUtf8 { file, path } => {
                write!(
                    formatter,
                    "{} at {} is not UTF-8",
                    file.name(),
                    path.display()
                )
            }
            Self::Parse { file, message } => {
                write!(formatter, "{} is invalid TOML: {message}", file.name())
            }
            Self::Validation(violations) => {
                write!(
                    formatter,
                    "configuration violates {} policy rule(s)",
                    violations.len()
                )
            }
        }
    }
}

impl Error for ConfigError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Read { source, .. } => Some(source),
            Self::TooLarge { .. }
            | Self::NotUtf8 { .. }
            | Self::Parse { .. }
            | Self::Validation(_) => None,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConfigViolation {
    pub location: &'static str,
    pub message: String,
}

impl fmt::Display for ConfigViolation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.location, self.message)
    }
}

#[derive(Clone, Copy, Debug)]
pub enum ConfigFile {
    Jarvis,
    GodMode,
    Competitive,
    Applications,
    Cleanup,
    Memory,
    Learning,
}

impl ConfigFile {
    #[must_use]
    pub const fn relative_path(self) -> &'static str {
        match self {
            Self::Jarvis => "jarvis.toml",
            Self::GodMode => "profiles/god-mode.toml",
            Self::Competitive => "profiles/competitive.toml",
            Self::Applications => "applications.toml",
            Self::Cleanup => "cleanup.toml",
            Self::Memory => "memory.toml",
            Self::Learning => "learning.toml",
        }
    }

    #[must_use]
    pub const fn name(self) -> &'static str {
        self.relative_path()
    }
}

#[derive(Clone)]
struct ConfigDocuments {
    jarvis: String,
    god_mode: String,
    competitive: String,
    applications: String,
    cleanup: String,
    memory: String,
    learning: String,
}

impl ConfigDocuments {
    fn read_from(directory: &Path) -> Result<Self, ConfigError> {
        Ok(Self {
            jarvis: read_document(directory, ConfigFile::Jarvis)?,
            god_mode: read_document(directory, ConfigFile::GodMode)?,
            competitive: read_document(directory, ConfigFile::Competitive)?,
            applications: read_document(directory, ConfigFile::Applications)?,
            cleanup: read_document(directory, ConfigFile::Cleanup)?,
            memory: read_document(directory, ConfigFile::Memory)?,
            learning: read_document(directory, ConfigFile::Learning)?,
        })
    }
}

fn read_document(directory: &Path, file: ConfigFile) -> Result<String, ConfigError> {
    let path = directory.join(file.relative_path());
    let file_handle = fs::File::open(&path).map_err(|source| ConfigError::Read {
        file,
        path: path.clone(),
        source,
    })?;
    let metadata = file_handle.metadata().map_err(|source| ConfigError::Read {
        file,
        path: path.clone(),
        source,
    })?;
    if metadata.len() > MAX_CONFIG_FILE_BYTES {
        return Err(ConfigError::TooLarge {
            file,
            path,
            bytes: metadata.len(),
        });
    }

    // The metadata check avoids reading a known-oversized file. The bounded
    // read is still required because a file can grow or be replaced between
    // the metadata query and the read.
    let capacity = usize::try_from(metadata.len()).map_err(|_| ConfigError::TooLarge {
        file,
        path: path.clone(),
        bytes: metadata.len(),
    })?;
    let mut bytes = Vec::with_capacity(capacity);
    file_handle
        .take(MAX_CONFIG_FILE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|source| ConfigError::Read {
            file,
            path: path.clone(),
            source,
        })?;
    let byte_count = u64::try_from(bytes.len()).expect("configuration byte count fits in u64");
    if byte_count > MAX_CONFIG_FILE_BYTES {
        return Err(ConfigError::TooLarge {
            file,
            path,
            bytes: byte_count,
        });
    }
    String::from_utf8(bytes).map_err(|_| ConfigError::NotUtf8 { file, path })
}

fn parse_document<T: for<'de> Deserialize<'de>>(
    file: ConfigFile,
    document: &str,
) -> Result<T, ConfigError> {
    toml::from_str(document).map_err(|error| ConfigError::Parse {
        file,
        message: error.to_string(),
    })
}

struct RawConfiguration {
    jarvis: JarvisFile,
    god_mode: GodModeFile,
    competitive: CompetitiveFile,
    applications: ApplicationsFile,
    cleanup: CleanupFile,
    memory: MemoryFile,
    learning: LearningFile,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct JarvisFile {
    general: General,
    activation: Activation,
    storage: Storage,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct General {
    default_profile: String,
    offline_first: bool,
    run_as_administrator: bool,
    raw_audio_persistence: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Activation {
    wake_word: String,
    push_to_talk: String,
    emergency_stop: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Storage {
    database: String,
    recipe_directory: String,
    log_retention_days: u16,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct GodModeFile {
    id: String,
    display_name: String,
    voice: GodVoice,
    ai: GodAi,
    security: GodSecurity,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct GodVoice {
    enabled: bool,
    transcribe_only_after_activation: bool,
    max_utterance_seconds: u16,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct GodAi {
    enabled: bool,
    spawn_on_unknown_command: bool,
    idle_timeout_seconds: u16,
    arbitrary_shell: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct GodSecurity {
    dangerous_actions_require_confirmation: bool,
    confirmation_expiry_seconds: u16,
    revalidate_recipes_on_every_use: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CompetitiveFile {
    id: String,
    display_name: String,
    overrides: Vec<String>,
    activation: CompetitiveActivation,
    shutdown: CompetitiveShutdown,
    resident: CompetitiveResident,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CompetitiveActivation {
    manual_entry: bool,
    automatic_entry: bool,
    trigger_logic: String,
    process_names: Vec<String>,
    require_verified_faceit_setup: bool,
    exit_cooldown_seconds: u16,
    detection_failure_policy: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CompetitiveShutdown {
    terminate_all_nonessential_owned_workers: bool,
    close_microphone: bool,
    cancel_tts: bool,
    unregister_command_hotkeys: bool,
    deny_worker_restart: bool,
    deny_general_actions: bool,
    deny_scripts_and_extensions: bool,
    deny_learning_and_memory_maintenance: bool,
    deny_embedding_generation_and_retrieval: bool,
    deny_codegen_compilation_and_sandbox: bool,
    grace_period_milliseconds: u16,
    verify_process_exit: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CompetitiveResident {
    supervisor: bool,
    event_monitor: bool,
    minimal_tray: bool,
    priority: String,
    gpu_inference_allowed: bool,
    continuous_telemetry: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ApplicationsFile {
    apps: BTreeMap<String, Application>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Application {
    #[serde(rename = "type")]
    kind: String,
    appid: Option<u32>,
    path_id: Option<String>,
    aliases: Vec<String>,
    protected_from_cleanup: Option<bool>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CleanupFile {
    cleanup: Cleanup,
    #[serde(default)]
    targets: Vec<toml::Value>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Cleanup {
    enabled: bool,
    policy: String,
    protect_windows_security_and_game_dependencies: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MemoryFile {
    session: MemorySession,
    durable: DurableMemory,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MemorySession {
    max_records: u16,
    max_bytes: u32,
    ttl_seconds: u32,
    persist_transcripts: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DurableMemory {
    store: String,
    explicit_preferences: bool,
    explicit_corrections: bool,
    inferred_preferences_stage: String,
    credentials_as_references_only: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LearningFile {
    learning: Learning,
    promotion: Promotion,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Learning {
    enabled_profiles: Vec<String>,
    source: String,
    prefer_skills_before_llm: bool,
    auto_generate_recipe_candidates: bool,
    auto_activate_generated_code: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Promotion {
    low_risk_recipe_min_verified_live_successes: u16,
    require_static_checks: bool,
    require_fixture_tests: bool,
    require_independent_verifier: bool,
    expanded_permissions_require_grant: bool,
}

fn validate(raw: RawConfiguration) -> Result<ValidatedConfiguration, ConfigError> {
    let mut violations = Vec::new();

    validate_general(&raw.jarvis, &mut violations);
    validate_god_mode(&raw.god_mode, &mut violations);
    validate_competitive(&raw.competitive, &mut violations);
    let applications = validate_applications(&raw.applications, &mut violations);
    validate_cleanup(&raw.cleanup, &mut violations);
    validate_memory(&raw.memory, &raw.jarvis.storage, &mut violations);
    validate_learning(&raw.learning, &mut violations);

    if violations.is_empty() {
        Ok(ValidatedConfiguration {
            applications,
            faceit_setup_status: FaceitSetupStatus::VerificationRequired,
            exit_cooldown_seconds: raw.competitive.activation.exit_cooldown_seconds,
        })
    } else {
        Err(ConfigError::Validation(violations))
    }
}

fn validate_general(file: &JarvisFile, violations: &mut Vec<ConfigViolation>) {
    require(
        violations,
        "general.default_profile",
        file.general.default_profile == "god-mode",
        "only god-mode may be the startup profile",
    );
    require(
        violations,
        "general.offline_first",
        file.general.offline_first,
        "must remain true",
    );
    require(
        violations,
        "general.run_as_administrator",
        !file.general.run_as_administrator,
        "must remain false",
    );
    require(
        violations,
        "general.raw_audio_persistence",
        !file.general.raw_audio_persistence,
        "must remain false",
    );
    require(
        violations,
        "activation.wake_word",
        is_safe_text(&file.activation.wake_word, 1, 64),
        "must be a short, non-control string",
    );
    require(
        violations,
        "activation.push_to_talk",
        is_safe_hotkey(&file.activation.push_to_talk),
        "must use a bounded modifier+key form",
    );
    require(
        violations,
        "activation.emergency_stop",
        is_safe_hotkey(&file.activation.emergency_stop),
        "must use a bounded modifier+key form",
    );
    require(
        violations,
        "activation",
        normalize(&file.activation.push_to_talk) != normalize(&file.activation.emergency_stop),
        "push-to-talk and emergency-stop must differ",
    );
    require(
        violations,
        "storage.database",
        file.storage.database == EXPECTED_DATABASE,
        "must be the fixed %LOCALAPPDATA%/Jarvis/jarvis.db template",
    );
    require(
        violations,
        "storage.recipe_directory",
        file.storage.recipe_directory == EXPECTED_RECIPE_DIRECTORY,
        "must be the fixed %LOCALAPPDATA%/Jarvis/learned_commands template",
    );
    require(
        violations,
        "storage.log_retention_days",
        (1..=365).contains(&file.storage.log_retention_days),
        "must be between 1 and 365 days",
    );
}

fn validate_god_mode(file: &GodModeFile, violations: &mut Vec<ConfigViolation>) {
    require(
        violations,
        "god-mode.id",
        file.id == "god-mode",
        "must equal god-mode",
    );
    require(
        violations,
        "god-mode.display_name",
        is_safe_text(&file.display_name, 1, 64),
        "must be a short, non-control string",
    );
    require(
        violations,
        "god-mode.voice.transcribe_only_after_activation",
        file.voice.transcribe_only_after_activation,
        "must remain true",
    );
    require(
        violations,
        "god-mode.voice.enabled",
        file.voice.enabled,
        "must remain true while this V1 profile is enabled",
    );
    require(
        violations,
        "god-mode.voice.max_utterance_seconds",
        (1..=60).contains(&file.voice.max_utterance_seconds),
        "must be between 1 and 60 seconds",
    );
    require(
        violations,
        "god-mode.ai.idle_timeout_seconds",
        file.ai.idle_timeout_seconds <= 600,
        "must not exceed 600 seconds",
    );
    require(
        violations,
        "god-mode.ai.enabled",
        file.ai.enabled,
        "must remain true while this V1 profile is enabled",
    );
    require(
        violations,
        "god-mode.ai.spawn_on_unknown_command",
        file.ai.spawn_on_unknown_command,
        "must remain true; availability is still profile-gated",
    );
    require(
        violations,
        "god-mode.ai.arbitrary_shell",
        !file.ai.arbitrary_shell,
        "must remain false",
    );
    require(
        violations,
        "god-mode.security.dangerous_actions_require_confirmation",
        file.security.dangerous_actions_require_confirmation,
        "must remain true",
    );
    require(
        violations,
        "god-mode.security.confirmation_expiry_seconds",
        (1..=60).contains(&file.security.confirmation_expiry_seconds),
        "must be between 1 and 60 seconds",
    );
    require(
        violations,
        "god-mode.security.revalidate_recipes_on_every_use",
        file.security.revalidate_recipes_on_every_use,
        "must remain true",
    );
}

fn validate_competitive(file: &CompetitiveFile, violations: &mut Vec<ConfigViolation>) {
    require(
        violations,
        "competitive.id",
        file.id == "competitive",
        "must equal competitive",
    );
    require(
        violations,
        "competitive.display_name",
        is_safe_text(&file.display_name, 1, 64),
        "must be a short, non-control string",
    );
    require(
        violations,
        "competitive.overrides",
        file.overrides == ["god-mode"],
        "must override exactly god-mode",
    );
    require(
        violations,
        "competitive.activation.manual_entry",
        file.activation.manual_entry,
        "must remain true",
    );
    require(
        violations,
        "competitive.activation.automatic_entry",
        file.activation.automatic_entry,
        "must remain true",
    );
    require(
        violations,
        "competitive.activation.trigger_logic",
        file.activation.trigger_logic == "any",
        "must remain any",
    );
    let mut normalized_processes = BTreeSet::new();
    for process in &file.activation.process_names {
        let valid = is_safe_process_name(process);
        require(
            violations,
            "competitive.activation.process_names",
            valid,
            "must contain only normalized executable leaf names",
        );
        if valid {
            require(
                violations,
                "competitive.activation.process_names",
                normalized_processes.insert(normalize(process)),
                "must not contain duplicate names",
            );
        }
    }
    require(
        violations,
        "competitive.activation.process_names",
        normalized_processes.contains("cs2.exe"),
        "must contain cs2.exe",
    );
    require(
        violations,
        "competitive.activation.require_verified_faceit_setup",
        file.activation.require_verified_faceit_setup,
        "must remain true",
    );
    require(
        violations,
        "competitive.activation.exit_cooldown_seconds",
        (1..=60).contains(&file.activation.exit_cooldown_seconds),
        "must be between 1 and 60 seconds",
    );
    require(
        violations,
        "competitive.activation.detection_failure_policy",
        file.activation.detection_failure_policy == "restricted",
        "must remain restricted",
    );

    let shutdown = &file.shutdown;
    for (location, value) in [
        (
            "competitive.shutdown.terminate_all_nonessential_owned_workers",
            shutdown.terminate_all_nonessential_owned_workers,
        ),
        (
            "competitive.shutdown.close_microphone",
            shutdown.close_microphone,
        ),
        ("competitive.shutdown.cancel_tts", shutdown.cancel_tts),
        (
            "competitive.shutdown.unregister_command_hotkeys",
            shutdown.unregister_command_hotkeys,
        ),
        (
            "competitive.shutdown.deny_worker_restart",
            shutdown.deny_worker_restart,
        ),
        (
            "competitive.shutdown.deny_general_actions",
            shutdown.deny_general_actions,
        ),
        (
            "competitive.shutdown.deny_scripts_and_extensions",
            shutdown.deny_scripts_and_extensions,
        ),
        (
            "competitive.shutdown.deny_learning_and_memory_maintenance",
            shutdown.deny_learning_and_memory_maintenance,
        ),
        (
            "competitive.shutdown.deny_embedding_generation_and_retrieval",
            shutdown.deny_embedding_generation_and_retrieval,
        ),
        (
            "competitive.shutdown.deny_codegen_compilation_and_sandbox",
            shutdown.deny_codegen_compilation_and_sandbox,
        ),
        (
            "competitive.shutdown.verify_process_exit",
            shutdown.verify_process_exit,
        ),
    ] {
        require(violations, location, value, "must remain true");
    }
    require(
        violations,
        "competitive.shutdown.grace_period_milliseconds",
        (100..=5_000).contains(&shutdown.grace_period_milliseconds),
        "must be between 100 and 5000 milliseconds",
    );

    let resident = &file.resident;
    for (location, value) in [
        ("competitive.resident.supervisor", resident.supervisor),
        ("competitive.resident.event_monitor", resident.event_monitor),
        ("competitive.resident.minimal_tray", resident.minimal_tray),
    ] {
        require(violations, location, value, "must remain true");
    }
    require(
        violations,
        "competitive.resident.priority",
        resident.priority == "below_normal",
        "must remain below_normal",
    );
    require(
        violations,
        "competitive.resident.gpu_inference_allowed",
        !resident.gpu_inference_allowed,
        "must remain false",
    );
    require(
        violations,
        "competitive.resident.continuous_telemetry",
        !resident.continuous_telemetry,
        "must remain false",
    );
}

fn validate_applications(
    file: &ApplicationsFile,
    violations: &mut Vec<ConfigViolation>,
) -> ApplicationCatalog {
    let mut application_ids = BTreeSet::new();
    let mut protected_close_targets = BTreeSet::new();
    let mut launch_aliases = BTreeMap::new();

    for (id, application) in &file.apps {
        require(
            violations,
            "applications.apps.<id>",
            is_safe_identifier(id),
            "must be a lowercase identifier",
        );
        if !is_safe_identifier(id) {
            continue;
        }
        application_ids.insert(id.clone());
        match application.kind.as_str() {
            "steam" => {
                require(
                    violations,
                    "applications.apps.<id>.appid",
                    application.appid.is_some(),
                    "is required for Steam applications",
                );
                require(
                    violations,
                    "applications.apps.<id>.path_id",
                    application.path_id.is_none(),
                    "is not allowed for Steam applications",
                );
            }
            "configured_executable" => {
                require(
                    violations,
                    "applications.apps.<id>.path_id",
                    application
                        .path_id
                        .as_deref()
                        .is_some_and(is_safe_identifier),
                    "must be an identifier, never an executable path",
                );
                require(
                    violations,
                    "applications.apps.<id>.appid",
                    application.appid.is_none(),
                    "is not allowed for configured executables",
                );
            }
            _ => violation(
                violations,
                "applications.apps.<id>.type",
                "must be steam or configured_executable in this milestone",
            ),
        }
        if application.protected_from_cleanup.unwrap_or(false) {
            protected_close_targets.insert(id.clone());
        }
        for alias in &application.aliases {
            let normalized = normalize(alias);
            require(
                violations,
                "applications.apps.<id>.aliases",
                is_safe_alias(alias),
                "must be a short, non-control phrase",
            );
            if is_safe_alias(alias) {
                require(
                    violations,
                    "applications.apps.<id>.aliases",
                    !launch_aliases.contains_key(&normalized),
                    "must not collide after normalization",
                );
                launch_aliases
                    .entry(normalized)
                    .or_insert_with(|| id.clone());
            }
        }
    }

    let cs2 = file.apps.get("cs2");
    require(
        violations,
        "applications.apps.cs2",
        cs2.is_some_and(|application| {
            application.kind == "steam"
                && application.appid == Some(730)
                && application.protected_from_cleanup == Some(true)
        }),
        "must remain protected Steam app 730",
    );
    require(
        violations,
        "applications.apps.faceit_anticheat",
        file.apps
            .get(FACEIT_ANTICHEAT_APPLICATION_ID)
            .is_some_and(|application| {
                application.kind == "configured_executable"
                    && application.path_id.as_deref() == Some("verified_faceit_anticheat_install")
                    && application.protected_from_cleanup == Some(true)
            }),
        "must remain the protected fixed FACEIT Anti-Cheat handoff identifier",
    );
    require(
        violations,
        "applications.apps.chrome",
        file.apps
            .get(CHROME_APPLICATION_ID)
            .is_some_and(|application| {
                application.kind == "configured_executable"
                    && application.path_id.as_deref() == Some("verified_chrome_install")
            }),
        "must remain the fixed Chrome handoff identifier",
    );
    require(
        violations,
        "applications.apps.cs2",
        file.apps
            .get(CS2_APPLICATION_ID)
            .is_some_and(|application| {
                application.kind == "steam"
                    && application.appid == Some(730)
                    && application.protected_from_cleanup == Some(true)
            }),
        "must remain the fixed CS2 Steam handoff identifier",
    );
    ApplicationCatalog {
        application_ids,
        protected_close_targets,
        launch_aliases,
    }
}

fn validate_cleanup(file: &CleanupFile, violations: &mut Vec<ConfigViolation>) {
    require(
        violations,
        "cleanup.enabled",
        !file.cleanup.enabled,
        "must remain false until external identities and cleanup execution are implemented",
    );
    require(
        violations,
        "cleanup.policy",
        file.cleanup.policy == "explicit_targets_only",
        "must remain explicit_targets_only",
    );
    require(
        violations,
        "cleanup.protect_windows_security_and_game_dependencies",
        file.cleanup.protect_windows_security_and_game_dependencies,
        "must remain true",
    );
    require(
        violations,
        "cleanup.targets",
        file.targets.is_empty(),
        "cannot configure external cleanup targets before identity verification exists",
    );
}

fn validate_memory(file: &MemoryFile, storage: &Storage, violations: &mut Vec<ConfigViolation>) {
    require(
        violations,
        "memory.session.max_records",
        (1..=1_000).contains(&file.session.max_records),
        "must be between 1 and 1000",
    );
    require(
        violations,
        "memory.session.max_bytes",
        (1_024..=4_194_304).contains(&file.session.max_bytes),
        "must be between 1024 and 4194304",
    );
    require(
        violations,
        "memory.session.ttl_seconds",
        (60..=86_400).contains(&file.session.ttl_seconds),
        "must be between 60 and 86400",
    );
    require(
        violations,
        "memory.session.persist_transcripts",
        !file.session.persist_transcripts,
        "must remain false",
    );
    require(
        violations,
        "memory.durable.store",
        file.durable.store == "jarvis.db"
            && storage.database == format!("{LOCAL_APP_DATA_ROOT}{}", file.durable.store),
        "must remain the database filename under the fixed local-app-data root",
    );
    require(
        violations,
        "memory.durable.explicit_preferences",
        file.durable.explicit_preferences,
        "must remain true",
    );
    require(
        violations,
        "memory.durable.explicit_corrections",
        file.durable.explicit_corrections,
        "must remain true",
    );
    require(
        violations,
        "memory.durable.inferred_preferences_stage",
        file.durable.inferred_preferences_stage == "v2",
        "must remain v2",
    );
    require(
        violations,
        "memory.durable.credentials_as_references_only",
        file.durable.credentials_as_references_only,
        "must remain true",
    );
}

fn validate_learning(file: &LearningFile, violations: &mut Vec<ConfigViolation>) {
    require(
        violations,
        "learning.enabled_profiles",
        file.learning.enabled_profiles == ["god-mode"],
        "must be limited to god-mode",
    );
    require(
        violations,
        "learning.source",
        file.learning.source == "authorized_actions_and_allowed_events",
        "must remain restricted to authorized inputs",
    );
    require(
        violations,
        "learning.prefer_skills_before_llm",
        file.learning.prefer_skills_before_llm,
        "must remain true",
    );
    require(
        violations,
        "learning.auto_activate_generated_code",
        !file.learning.auto_activate_generated_code,
        "must remain false",
    );
    require(
        violations,
        "learning.auto_generate_recipe_candidates",
        file.learning.auto_generate_recipe_candidates,
        "must remain true; candidates still require independent verification",
    );
    let promotion = &file.promotion;
    require(
        violations,
        "promotion.low_risk_recipe_min_verified_live_successes",
        (1..=100).contains(&promotion.low_risk_recipe_min_verified_live_successes),
        "must be between 1 and 100",
    );
    for (location, value) in [
        (
            "promotion.require_static_checks",
            promotion.require_static_checks,
        ),
        (
            "promotion.require_fixture_tests",
            promotion.require_fixture_tests,
        ),
        (
            "promotion.require_independent_verifier",
            promotion.require_independent_verifier,
        ),
        (
            "promotion.expanded_permissions_require_grant",
            promotion.expanded_permissions_require_grant,
        ),
    ] {
        require(violations, location, value, "must remain true");
    }
}

fn require(
    violations: &mut Vec<ConfigViolation>,
    location: &'static str,
    condition: bool,
    message: &'static str,
) {
    if !condition {
        violation(violations, location, message);
    }
}

fn violation(violations: &mut Vec<ConfigViolation>, location: &'static str, message: &'static str) {
    violations.push(ConfigViolation {
        location,
        message: message.to_owned(),
    });
}

fn normalize(value: &str) -> String {
    value
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_lowercase()
}

fn is_safe_text(value: &str, minimum: usize, maximum: usize) -> bool {
    let length = value.chars().count();
    (minimum..=maximum).contains(&length) && !value.chars().any(char::is_control)
}

fn is_safe_hotkey(value: &str) -> bool {
    let parts = value.split('+').collect::<Vec<_>>();
    if !(2..=5).contains(&parts.len()) || parts.iter().any(|part| part.is_empty()) {
        return false;
    }
    let modifiers = &parts[..parts.len() - 1];
    let mut seen = BTreeSet::new();
    if modifiers.iter().any(|modifier| {
        !matches!(*modifier, "Ctrl" | "Alt" | "Shift" | "Win") || !seen.insert(*modifier)
    }) {
        return false;
    }
    let key = parts[parts.len() - 1];
    key.len() <= 20
        && key
            .chars()
            .all(|character| character.is_ascii_alphanumeric())
}

fn is_safe_process_name(value: &str) -> bool {
    let normalized = normalize(value);
    normalized == value
        && normalized.ends_with(".exe")
        && normalized.len() <= 128
        && normalized.chars().all(|character| {
            character.is_ascii_lowercase()
                || character.is_ascii_digit()
                || matches!(character, '.' | '_' | '-')
        })
}

fn is_safe_identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .chars()
            .enumerate()
            .all(|(index, character)| match index {
                0 => character.is_ascii_lowercase(),
                _ => {
                    character.is_ascii_lowercase()
                        || character.is_ascii_digit()
                        || matches!(character, '_' | '-')
                }
            })
}

fn is_safe_alias(value: &str) -> bool {
    is_safe_text(value, 1, 80) && !value.contains(['\\', '/', '*'])
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn fixture_documents() -> ConfigDocuments {
        ConfigDocuments {
            jarvis: include_str!("../../../config/jarvis.toml").to_owned(),
            god_mode: include_str!("../../../config/profiles/god-mode.toml").to_owned(),
            competitive: include_str!("../../../config/profiles/competitive.toml").to_owned(),
            applications: include_str!("../../../config/applications.toml").to_owned(),
            cleanup: include_str!("../../../config/cleanup.toml").to_owned(),
            memory: include_str!("../../../config/memory.toml").to_owned(),
            learning: include_str!("../../../config/learning.toml").to_owned(),
        }
    }

    fn violations(error: ConfigError) -> Vec<ConfigViolation> {
        match error {
            ConfigError::Validation(violations) => violations,
            unexpected => panic!("expected validation error, got {unexpected}"),
        }
    }

    fn temporary_directory() -> PathBuf {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let sequence = NEXT.fetch_add(1, Ordering::Relaxed);
        let directory = std::env::temp_dir().join(format!(
            "jarvis-config-test-{}-{sequence}",
            std::process::id()
        ));
        fs::create_dir_all(&directory).expect("create isolated config test directory");
        directory
    }

    #[test]
    fn checked_in_bundle_validates_without_certifying_faceit() {
        let configuration = ValidatedConfiguration::from_documents(fixture_documents())
            .expect("checked-in config should validate");
        assert!(configuration
            .applications()
            .application_ids()
            .any(|identifier| identifier == "discord"));
        assert!(configuration
            .applications()
            .application_ids()
            .any(|identifier| identifier == FACEIT_ANTICHEAT_APPLICATION_ID));
        assert_eq!(
            configuration.faceit_setup_status(),
            FaceitSetupStatus::VerificationRequired
        );
        assert_eq!(configuration.exit_cooldown_seconds(), 5);
    }

    #[test]
    fn pre_match_handoff_requires_the_fixed_faceit_identifier() {
        let mut documents = fixture_documents();
        documents.applications = documents
            .applications
            .replace("[apps.faceit_anticheat]", "[apps.faceit_disabled]");

        let violations = violations(
            ValidatedConfiguration::from_documents(documents)
                .expect_err("pre-match handoff identifiers must remain fixed"),
        );
        assert!(violations
            .iter()
            .any(|violation| violation.location == "applications.apps.faceit_anticheat"));
    }

    #[test]
    fn oversized_document_is_rejected_before_the_bundle_is_parsed() {
        let directory = temporary_directory();
        let path = directory.join(ConfigFile::Jarvis.relative_path());
        fs::write(&path, vec![b'x'; (MAX_CONFIG_FILE_BYTES + 1) as usize])
            .expect("write oversized fixture");

        let result = read_document(&directory, ConfigFile::Jarvis);
        fs::remove_dir_all(&directory).expect("remove isolated config test directory");

        assert!(matches!(
            result,
            Err(ConfigError::TooLarge { bytes, .. }) if bytes > MAX_CONFIG_FILE_BYTES
        ));
    }

    #[test]
    fn unknown_fields_are_rejected() {
        let mut documents = fixture_documents();
        documents.jarvis.push_str("\nunreviewed_option = true\n");
        assert!(matches!(
            ValidatedConfiguration::from_documents(documents),
            Err(ConfigError::Parse { .. })
        ));
    }

    #[test]
    fn weakened_competitive_shutdown_is_rejected() {
        let mut documents = fixture_documents();
        documents.competitive = documents.competitive.replace(
            "deny_general_actions = true",
            "deny_general_actions = false",
        );
        let violations = violations(
            ValidatedConfiguration::from_documents(documents)
                .expect_err("weakened competitive policy must fail"),
        );
        assert!(violations
            .iter()
            .any(|violation| violation.location == "competitive.shutdown.deny_general_actions"));
    }

    #[test]
    fn raw_executable_path_is_rejected() {
        let mut documents = fixture_documents();
        documents.applications = documents.applications.replace(
            "path_id = \"verified_discord_install\"",
            "path_id = \"C:/Discord/Discord.exe\"",
        );
        let violations = violations(
            ValidatedConfiguration::from_documents(documents)
                .expect_err("raw executable path must fail"),
        );
        assert!(violations
            .iter()
            .any(|violation| violation.location == "applications.apps.<id>.path_id"));
    }

    #[test]
    fn alias_collisions_are_rejected_after_normalization() {
        let mut documents = fixture_documents();
        documents.applications = documents.applications.replace(
            "aliases = [\"cs\", \"cs2\", \"counter\", \"counter strike\", \"counter strike 2\"]",
            "aliases = [\"discord\"]",
        );
        let violations = violations(
            ValidatedConfiguration::from_documents(documents)
                .expect_err("colliding alias must fail"),
        );
        assert!(violations
            .iter()
            .any(|violation| violation.location == "applications.apps.<id>.aliases"));
    }
}
