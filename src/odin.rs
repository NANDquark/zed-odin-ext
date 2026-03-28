use base64::{engine::general_purpose, Engine as _};
use std::{fs, time::SystemTime};
use zed::{
    BuildTaskDefinition, BuildTaskDefinitionTemplatePayload, BuildTaskTemplate, DebugRequest,
    DebugScenario, LanguageServerId, LaunchRequest, TaskTemplate, Worktree,
};
use zed_extension_api::{
    self as zed,
    lsp::{Completion, CompletionKind, Symbol, SymbolKind},
    serde_json,
    settings::LspSettings,
    Architecture, CodeLabel, CodeLabelSpan, DebugConfig, Os, Result,
};

struct OdinExtension {
    cached_binary_path: Option<String>,
}

const GITHUB_REPO: &str = "DanielGavin/ols";
const CODELLDB_REPO: &str = "vadimcn/codelldb";
const ODIN_CODELLDB_ADAPTER_NAME: &str = "OdinCodeLLDB";

const ODIN_SCRIPT: &str = include_str!("../resources/lldb/odin.py");

impl OdinExtension {
    fn odin_formatter_command() -> String {
        let encoded_script = general_purpose::STANDARD.encode(ODIN_SCRIPT);
        format!(
            "script import base64, types; odin = types.SimpleNamespace(); exec(base64.b64decode('{}').decode(), odin.__dict__); odin.__dict__['__lldb_init_module'](lldb.debugger, {{}})",
            encoded_script
        )
    }

    fn exe_suffix(platform: Os) -> &'static str {
        match platform {
            Os::Windows => ".exe",
            _ => "",
        }
    }

    fn path_separator(platform: Os) -> &'static str {
        match platform {
            Os::Windows => "\\",
            _ => "/",
        }
    }

    fn codelldb_binary_name(platform: Os) -> &'static str {
        match platform {
            Os::Windows => "codelldb.exe",
            _ => "codelldb",
        }
    }

    fn codelldb_asset_name(platform: Os, arch: Architecture) -> Option<String> {
        let arch = match arch {
            Architecture::Aarch64 => "arm64",
            Architecture::X8664 => "x64",
            Architecture::X86 => return None,
        };

        let platform = match platform {
            Os::Mac => "darwin",
            Os::Linux => "linux",
            Os::Windows => "win32",
        };

        Some(format!("codelldb-{platform}-{arch}.vsix"))
    }

    fn merge_init_command(config_map: &mut serde_json::Map<String, serde_json::Value>) {
        let formatter_command = Self::odin_formatter_command();

        match config_map.get_mut("initCommands") {
            Some(serde_json::Value::Array(commands)) => {
                let already_present = commands
                    .iter()
                    .any(|command| command.as_str() == Some(formatter_command.as_str()));
                if !already_present {
                    commands.insert(0, serde_json::Value::String(formatter_command));
                }
            }
            Some(_) => {}
            None => {
                config_map.insert(
                    "initCommands".to_string(),
                    serde_json::json!(vec![formatter_command]),
                );
            }
        }
    }

    fn ols_binary_name(&self, platform: Os, arch: Architecture) -> Option<String> {
        let arch: &str = match arch {
            zed::Architecture::Aarch64 => "arm64",
            zed::Architecture::X8664 => "x86_64",
            zed::Architecture::X86 => return None, // Not supported
        };

        let os: &str = match platform {
            zed::Os::Mac => "darwin",
            zed::Os::Linux => "unknown-linux-gnu",
            zed::Os::Windows => "pc-windows-msvc",
        };

        let binary_name = format!("ols-{arch}-{os}");
        Some(binary_name)
    }

    fn find_existing_ols_binary(&self) -> Option<String> {
        let entries = fs::read_dir(".").ok()?;
        let (platform, arch) = zed::current_platform();
        let binary_name = self.ols_binary_name(platform, arch)?;
        let executable_name = format!("{}{}", binary_name, Self::exe_suffix(platform));
        let separator = Self::path_separator(platform);

        for entry in entries.flatten() {
            let file_name = entry.file_name();
            let name_str = file_name.to_str()?;
            if name_str.starts_with("ols-") && entry.path().is_dir() {
                let binary_path = entry.path().join(&executable_name);
                if binary_path.is_file() {
                    let full_path = format!("{}{}{}", name_str, separator, executable_name);
                    return Some(full_path);
                }
            }
        }

        None
    }

    fn language_server_binary_path(
        &mut self,
        language_server_id: &LanguageServerId,
        worktree: &Worktree,
    ) -> Result<String> {
        let language_server = language_server_id.as_ref();
        if let Some(path) = LspSettings::for_worktree(language_server, worktree)
            .ok()
            .and_then(|settings| settings.binary)
            .and_then(|binary| binary.path)
        {
            return Ok(path);
        }

        if let Some(path) = worktree.which(language_server) {
            return Ok(path);
        }

        if let Some(path) = &self.cached_binary_path {
            if fs::metadata(path).is_ok_and(|stat| stat.is_file()) {
                return Ok(path.to_string());
            }
        }

        zed::set_language_server_installation_status(
            language_server_id,
            &zed::LanguageServerInstallationStatus::CheckingForUpdate,
        );

        let release = match zed::latest_github_release(
            GITHUB_REPO,
            zed::GithubReleaseOptions {
                require_assets: true,
                pre_release: false,
            },
        ) {
            Ok(release) => release,
            Err(e) => {
                if let Some(path) = self.find_existing_ols_binary() {
                    self.cached_binary_path = Some(path.clone());
                    return Ok(path);
                }

                return Err(format!(
                    "Failed to download OLS language server: {}\n\n\
                    To resolve this issue, you can connect to the internet and restart Zed or Manually install OLS.",
                    e
                ));
            }
        };

        let (platform, arch) = zed::current_platform();
        let file_name = self
            .ols_binary_name(platform, arch)
            .ok_or_else(|| format!("Unsupported platform {:?}", arch))?;
        let asset_name = format!("{}.zip", file_name);

        let asset = release
            .assets
            .iter()
            .find(|asset| asset.name == asset_name)
            .ok_or_else(|| format!("no asset found matching {:?}", asset_name))?;

        let version_dir = format!("ols-{}", release.version);
        let separator = Self::path_separator(platform);
        let binary_path = format!(
            "{}{}{}{}",
            version_dir,
            separator,
            file_name,
            Self::exe_suffix(platform)
        );

        if !fs::metadata(&binary_path).is_ok_and(|stat| stat.is_file()) {
            zed::set_language_server_installation_status(
                language_server_id,
                &zed::LanguageServerInstallationStatus::Downloading,
            );

            zed::download_file(
                &asset.download_url,
                &version_dir,
                zed::DownloadedFileType::Zip,
            )
            .map_err(|e| format!("failed to download file: {e}"))?;

            zed::make_file_executable(&binary_path)?;

            // Cleanup older versions
            let entries =
                fs::read_dir(".").map_err(|e| format!("failed to list working directory {e}"))?;
            for entry in entries {
                let entry = entry.map_err(|e| format!("failed to load directory entry {e}"))?;
                if entry.file_name().to_str() != Some(&version_dir) {
                    fs::remove_dir_all(entry.path()).ok();
                }
            }
        }

        self.cached_binary_path = Some(binary_path.clone());
        Ok(binary_path)
    }

    fn find_existing_codelldb_binary(&self) -> Option<String> {
        let entries = fs::read_dir(".").ok()?;
        let (platform, _) = zed::current_platform();
        let separator = Self::path_separator(platform);
        let binary_name = Self::codelldb_binary_name(platform);

        let mut newest: Option<(SystemTime, String)> = None;

        for entry in entries.flatten() {
            let file_name = entry.file_name();
            let name_str = match file_name.to_str() {
                Some(name) => name,
                None => continue,
            };

            if !name_str.starts_with("codelldb-") || !entry.path().is_dir() {
                continue;
            }

            let binary_path = entry.path().join("extension").join("adapter").join(binary_name);
            if !binary_path.is_file() {
                continue;
            }

            let modified = fs::metadata(&binary_path)
                .and_then(|metadata| metadata.modified())
                .unwrap_or(SystemTime::UNIX_EPOCH);
            let full_path = format!(
                "{}{}extension{}adapter{}{}",
                name_str, separator, separator, separator, binary_name
            );

            match &newest {
                Some((best_modified, _)) if modified <= *best_modified => {}
                _ => newest = Some((modified, full_path)),
            }
        }

        newest.map(|(_, path)| path)
    }

    fn codelldb_binary_path(
        &mut self,
        user_provided_debug_adapter_path: Option<String>,
        worktree: &Worktree,
    ) -> Result<String> {
        if let Some(path) = user_provided_debug_adapter_path {
            return Ok(path);
        }

        if let Some(path) = worktree.which("codelldb") {
            return Ok(path);
        }

        if let Some(path) = self.find_existing_codelldb_binary() {
            return Ok(path);
        }

        let release = zed::latest_github_release(
            CODELLDB_REPO,
            zed::GithubReleaseOptions {
                require_assets: true,
                pre_release: false,
            },
        )?;

        let (platform, arch) = zed::current_platform();
        let asset_name = Self::codelldb_asset_name(platform, arch)
            .ok_or_else(|| format!("Unsupported CodeLLDB platform: {platform:?}/{arch:?}"))?;
        let asset = release
            .assets
            .iter()
            .find(|asset| asset.name == asset_name)
            .ok_or_else(|| format!("No CodeLLDB asset found matching {asset_name:?}"))?;

        let version_dir = format!("codelldb-{}", release.version);
        let separator = Self::path_separator(platform);
        let binary_name = Self::codelldb_binary_name(platform);
        let binary_path = format!(
            "{}{}extension{}adapter{}{}",
            version_dir, separator, separator, separator, binary_name
        );

        if !fs::metadata(&binary_path).is_ok_and(|stat| stat.is_file()) {
            zed::download_file(
                &asset.download_url,
                &version_dir,
                zed::DownloadedFileType::Zip,
            )
            .map_err(|e| format!("failed to download CodeLLDB: {e}"))?;

            zed::make_file_executable(&binary_path)
                .map_err(|e| format!("failed to mark CodeLLDB as executable: {e}"))?;

            let entries =
                fs::read_dir(".").map_err(|e| format!("failed to list working directory: {e}"))?;
            for entry in entries.flatten() {
                let file_name = entry.file_name();
                let name = match file_name.to_str() {
                    Some(name) => name,
                    None => continue,
                };
                if name != version_dir && name.starts_with("codelldb-") {
                    fs::remove_dir_all(entry.path()).ok();
                }
            }
        }

        Ok(binary_path)
    }

    fn request_kind_from_config(
        &self,
        config: &serde_json::Value,
    ) -> Result<zed::StartDebuggingRequestArgumentsRequest> {
        let request = config
            .get("request")
            .and_then(|request| request.as_str())
            .ok_or_else(|| "Debug config is missing a string `request` field".to_string())?;

        match request {
            "launch" => Ok(zed::StartDebuggingRequestArgumentsRequest::Launch),
            "attach" => Ok(zed::StartDebuggingRequestArgumentsRequest::Attach),
            other => Err(format!("Unsupported debug request kind: {other}")),
        }
    }
}

impl OdinExtension {
    fn is_integer_type(type_str: &str) -> bool {
        matches!(
            type_str,
            // Basic signed integers
            "int" | "i8" | "i16" | "i32" | "i64" | "i128" |
            // Basic unsigned integers
            "uint" | "u8" | "u16" | "u32" | "u64" | "u128" | "uintptr" |
            // Integer aliases
            "byte" | "rune" |
            // Little-endian integers
            "i16le" | "i32le" | "i64le" | "i128le" |
            "u16le" | "u32le" | "u64le" | "u128le" |
            // Big-endian integers
            "i16be" | "i32be" | "i64be" | "i128be" |
            "u16be" | "u32be" | "u64be" | "u128be"
        )
    }

    fn create_label(code: String, filter_len: usize) -> CodeLabel {
        let code_len = code.len();
        CodeLabel {
            code,
            spans: vec![CodeLabelSpan::code_range(0..code_len)],
            filter_range: (0..filter_len).into(),
        }
    }

    fn create_label_with_span(
        code: String,
        span_range: std::ops::Range<usize>,
        filter_len: usize,
    ) -> CodeLabel {
        CodeLabel {
            code,
            spans: vec![CodeLabelSpan::code_range(span_range)],
            filter_range: (0..filter_len).into(),
        }
    }
}

impl zed::Extension for OdinExtension {
    fn new() -> Self {
        Self {
            cached_binary_path: None,
        }
    }

    fn language_server_command(
        &mut self,
        language_server_id: &LanguageServerId,
        worktree: &Worktree,
    ) -> Result<zed::Command> {
        let ols_binary_path = self.language_server_binary_path(language_server_id, worktree)?;
        Ok(zed::Command {
            command: ols_binary_path,
            args: Default::default(),
            env: Default::default(),
        })
    }

    fn language_server_initialization_options(
        &mut self,
        language_server_id: &LanguageServerId,
        worktree: &Worktree,
    ) -> Result<Option<serde_json::Value>> {
        let settings = LspSettings::for_worktree(language_server_id.as_ref(), worktree)
            .ok()
            .and_then(|lsp_settings| lsp_settings.initialization_options.clone());
        Ok(settings)
    }

    fn language_server_workspace_configuration(
        &mut self,
        language_server_id: &LanguageServerId,
        worktree: &Worktree,
    ) -> Result<Option<serde_json::Value>> {
        let settings = LspSettings::for_worktree(language_server_id.as_ref(), worktree)
            .ok()
            .and_then(|lsp_settings| lsp_settings.settings.clone())
            .unwrap_or_default();
        Ok(Some(settings))
    }

    fn label_for_completion(
        &self,
        _language_server_id: &LanguageServerId,
        completion: Completion,
    ) -> Option<CodeLabel> {
        use CompletionKind::*;

        let kind = completion.kind?;
        let label = &completion.label;
        let filter_len = label.len();

        match kind {
            Struct => {
                let code = match &completion.detail {
                    Some(detail) if detail.starts_with('[') || detail.starts_with("distinct") => {
                        format!("{} :: {}", label, detail)
                    }
                    _ => format!("{} :: struct", label),
                };
                Some(Self::create_label(code, filter_len))
            }

            Enum => {
                let code = match &completion.detail {
                    // OLS sends union type info in detail field (e.g., "union { int, f32 }")
                    // We can detect and display it correctly here
                    Some(detail) if detail.contains("union") => {
                        format!("{} :: union", label)
                    }
                    Some(detail) if Self::is_integer_type(detail) => {
                        format!("{} :: enum {}", label, detail)
                    }
                    _ => format!("{} :: enum", label),
                };
                Some(Self::create_label(code, filter_len))
            }

            Variable | Field => {
                let type_name = completion.detail.unwrap_or_else(|| "type".to_string());
                Some(Self::create_label(
                    format!("{}: {}", label, type_name),
                    filter_len,
                ))
            }

            Constant => {
                let value = completion.detail.unwrap_or_else(|| "value".to_string());
                Some(Self::create_label(
                    format!("{} :: {}", label, value),
                    filter_len,
                ))
            }

            EnumMember => {
                let code = format!(".{}", label);
                Some(Self::create_label_with_span(
                    code,
                    1..label.len() + 1,
                    filter_len,
                ))
            }

            Property => {
                let code = format!(".{}", label);
                Some(Self::create_label_with_span(
                    code,
                    1..label.len() + 1,
                    filter_len,
                ))
            }

            Keyword => Some(CodeLabel {
                code: label.clone(),
                spans: vec![CodeLabelSpan::literal(
                    label.clone(),
                    Some("keyword".to_string()),
                )],
                filter_range: (0..filter_len).into(),
            }),

            Module => {
                let code = format!("package {}", label);
                Some(Self::create_label_with_span(
                    code,
                    8..label.len() + 8,
                    filter_len,
                ))
            }

            _ => None,
        }
    }

    fn label_for_symbol(
        &self,
        _language_server_id: &LanguageServerId,
        symbol: Symbol,
    ) -> Option<CodeLabel> {
        // NOTE: Symbol navigation has limited type information compared to completions.
        // The LSP Symbol type only provides 'name' and 'kind', without detailed type info.

        use SymbolKind::*;

        let name = &symbol.name;
        let filter_len = name.len();

        match symbol.kind {
            Function => Some(Self::create_label(format!("{} :: proc", name), filter_len)),
            Variable => Some(Self::create_label(format!("{}: type", name), filter_len)),
            Struct => Some(Self::create_label(
                format!("{} :: struct", name),
                filter_len,
            )),
            // OLS sends both enums and unions as Enum kind (cannot distinguish in symbols)
            Enum => Some(Self::create_label(format!("{} :: enum", name), filter_len)),
            // Struct and union fields
            Field => Some(Self::create_label(format!("{}: type", name), filter_len)),
            _ => None,
        }
    }

    fn dap_config_to_scenario(&mut self, config: DebugConfig) -> Result<DebugScenario, String> {
        let mut config_map = serde_json::Map::new();
        match &config.request {
            DebugRequest::Launch(launch) => {
                config_map.insert("request".to_string(), serde_json::json!("launch"));
                config_map.insert("program".to_string(), serde_json::json!(&launch.program));

                if let Some(ref cwd) = launch.cwd {
                    config_map.insert("cwd".to_string(), serde_json::json!(cwd));
                }

                if !launch.args.is_empty() {
                    config_map.insert("args".to_string(), serde_json::json!(&launch.args));
                }

                if !launch.envs.is_empty() {
                    config_map.insert("env".to_string(), serde_json::json!(&launch.envs));
                }
            }
            DebugRequest::Attach(attach) => {
                config_map.insert("request".to_string(), serde_json::json!("attach"));
                config_map.insert("pid".to_string(), serde_json::json!(&attach.process_id));
            }
        }

        if let Some(stop_on_entry) = config.stop_on_entry {
            config_map.insert("stopOnEntry".to_string(), serde_json::json!(stop_on_entry));
        }

        // Register Odin type formatters in the adapter-specific phase. Zed's
        // locator build phase may ignore adapter config attached earlier.
        Self::merge_init_command(&mut config_map);

        let config_value = serde_json::Value::Object(config_map);
        let config_json = serde_json::to_string(&config_value)
            .map_err(|e| format!("Failed to serialize debug config: {}", e))?;

        Ok(DebugScenario {
            adapter: config.adapter,
            label: config.label,
            config: config_json,
            tcp_connection: None,
            build: None,
        })
    }

    fn dap_locator_create_scenario(
        &mut self,
        locator_name: String,
        build_task: TaskTemplate,
        resolved_label: String,
        debug_adapter_name: String,
    ) -> Option<DebugScenario> {
        let is_run = build_task.command == "odin" && build_task.args.first() == Some(&"run".into());
        let is_test =
            build_task.command == "odin" && build_task.args.first() == Some(&"test".into());

        if !is_run && !is_test {
            return None;
        }

        // Convert "odin run" to "odin build" with -debug flag
        let mut build_args = build_task.args.clone();
        build_args[0] = "build".to_string();

        // Add -out flag to control output name
        let (platform, _) = zed::current_platform();
        let out_name = format!("debug_build{}", Self::exe_suffix(platform));
        build_args.push(format!("-out:{}", out_name));

        // Add -debug flag if not present
        if !build_args.contains(&"-debug".into()) {
            build_args.push("-debug".into());
        }

        if is_test {
            build_args.push("-build-mode:test".into())
        }

        // Create the build task template
        let build_template = BuildTaskTemplate {
            label: if is_test {
                "odin debug test".into()
            } else {
                "odin debug build".into()
            },
            command: build_task.command.clone(),
            args: build_args,
            env: build_task.env.clone(),
            cwd: build_task.cwd.clone(),
        };

        // Build-step scenarios should not rely on adapter-specific config here.
        // The final adapter config is produced later in `dap_config_to_scenario`.
        let config = serde_json::to_string(&serde_json::json!({})).ok()?;

        // Update the task labels. The resulting label will be displayed as-is in
        // the F4 Debug menu and will have "Debug: " prepended to the label when
        // shown in the test gutter.
        let label = if is_run {
            resolved_label
                .strip_prefix("run: ")
                .unwrap_or(&resolved_label)
                .to_string()
        } else {
            resolved_label
                .strip_prefix("test: ")
                .map(|suffix| format!("test {}", suffix))
                .unwrap_or_else(|| resolved_label.clone())
        };

        Some(DebugScenario {
            adapter: debug_adapter_name,
            label,
            config,
            tcp_connection: None,
            build: Some(BuildTaskDefinition::Template(
                BuildTaskDefinitionTemplatePayload {
                    template: build_template,
                    locator_name: Some(locator_name),
                },
            )),
        })
    }

    fn run_dap_locator(
        &mut self,
        _locator_name: String,
        build_task: TaskTemplate,
    ) -> Result<DebugRequest, String> {
        // Only handle Odin build and test tasks
        if build_task.command != "odin"
            || build_task.args.is_empty()
            || !(build_task.args[0] == "build" || build_task.args[0] == "test")
        {
            return Err("Not an Odin build or test task".to_string());
        }

        // Extract the binary name from the -out: flag
        let output_name = build_task
            .args
            .iter()
            .find_map(|arg| arg.strip_prefix("-out:"))
            .ok_or_else(|| "Failed to extract output binary name from build task".to_string())?
            .to_string();

        // Construct absolute path to the binary, since lldb-dap requires absolute paths
        let cwd = build_task.cwd.as_ref().ok_or("No cwd in build task")?;
        let (platform, _) = zed::current_platform();
        let separator = Self::path_separator(platform);
        let program = format!("{}{}{}", cwd, separator, output_name);

        let request = LaunchRequest {
            program,
            cwd: build_task.cwd,
            args: vec![],
            envs: build_task.env.into_iter().collect(),
        };

        Ok(DebugRequest::Launch(request))
    }

    fn get_dap_binary(
        &mut self,
        adapter_name: String,
        config: zed::DebugTaskDefinition,
        user_provided_debug_adapter_path: Option<String>,
        worktree: &Worktree,
    ) -> Result<zed::DebugAdapterBinary> {
        if adapter_name != ODIN_CODELLDB_ADAPTER_NAME {
            return Err(format!("Unsupported Odin debug adapter: {adapter_name}"));
        }

        let command =
            self.codelldb_binary_path(user_provided_debug_adapter_path, worktree)?;
        let mut config_value: serde_json::Value = serde_json::from_str(&config.config)
            .map_err(|e| format!("Failed to parse CodeLLDB config: {e}"))?;
        let config_map = config_value
            .as_object_mut()
            .ok_or_else(|| "CodeLLDB config must be a JSON object".to_string())?;

        config_map
            .entry("name".to_string())
            .or_insert_with(|| serde_json::json!(&config.label));
        config_map
            .entry("cwd".to_string())
            .or_insert_with(|| serde_json::json!(worktree.root_path()));
        Self::merge_init_command(config_map);

        let request = self.request_kind_from_config(&config_value)?;
        let configuration = serde_json::to_string(&config_value)
            .map_err(|e| format!("Failed to serialize CodeLLDB config: {e}"))?;
        let connection = config
            .tcp_connection
            .map(zed::resolve_tcp_template)
            .transpose()?;

        Ok(zed::DebugAdapterBinary {
            command: Some(command),
            arguments: Vec::new(),
            envs: Vec::new(),
            cwd: Some(worktree.root_path()),
            connection,
            request_args: zed::StartDebuggingRequestArguments {
                configuration,
                request,
            },
        })
    }

    fn dap_request_kind(
        &mut self,
        _adapter_name: String,
        config: serde_json::Value,
    ) -> Result<zed::StartDebuggingRequestArgumentsRequest> {
        self.request_kind_from_config(&config)
    }
}

zed::register_extension!(OdinExtension);
