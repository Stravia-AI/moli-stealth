use chromiumoxide_cdp::cdp::browser_protocol::browser::{
    CancelDownloadParams, SetWindowBoundsParams,
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::str::FromStr;

use crate::conn::{BrowserWindowBounds, CdpConnection, Cmd};
use crate::devtools_runtime::{
    DevToolsCommand, DevToolsCommandResult, DevToolsError, DevToolsErrorKind,
    DevToolsSetDownloadBehaviorCommand, DevToolsSetPermissionCommand,
};
use crate::domains::actions::BrowserAction;
use crate::domains::command_output::CommandOutputPlan;
use crate::version;
use moli_core::{
    browser_host::{BrowserDownloadBehavior, BrowserDownloadPolicyUpdate},
    page::{CompletedPageCommand, PendingPageCommand},
};

const DEV_TOOLS_WINDOW_ID: u32 = 1_923_710_101;
pub(crate) struct PendingBrowserCommandDispatch {
    command_id: Option<u64>,
    response_session_id: Option<String>,
    kind: PendingBrowserCommandKind,
}

pub(crate) struct CompletedBrowserCommandDispatch {
    command_id: Option<u64>,
    response_session_id: Option<String>,
    kind: CompletedBrowserCommandKind,
}

pub(crate) enum BrowserCommandTaskStep {
    Pending(PendingBrowserCommandDispatch),
    Complete(CommandOutputPlan),
}

enum PendingBrowserCommandKind {
    OpenDownloadAsStream {
        pending: tokio::task::JoinHandle<Result<Vec<u8>, String>>,
    },
    ApplyPermissionOverrides {
        pending: Vec<PendingBrowserPageCommand>,
    },
}

enum CompletedBrowserCommandKind {
    OpenDownloadAsStream {
        completed: Result<Vec<u8>, String>,
    },
    ApplyPermissionOverrides {
        completed: Vec<CompletedBrowserPageCommand>,
    },
}

struct PendingBrowserPageCommand {
    target: PendingBrowserPageTarget,
    pending: PendingPageCommand,
}

struct CompletedBrowserPageCommand {
    target: PendingBrowserPageTarget,
    completed: Result<CompletedPageCommand, String>,
}

#[derive(Clone)]
enum PendingBrowserPageTarget {
    BrowserContextActive {
        browser_context_id: String,
    },
    BrowserContextBackground {
        browser_context_id: String,
        target_id: String,
    },
}

impl PendingBrowserCommandDispatch {
    pub(crate) async fn wait(self) -> CompletedBrowserCommandDispatch {
        let kind = match self.kind {
            PendingBrowserCommandKind::OpenDownloadAsStream { pending } => {
                CompletedBrowserCommandKind::OpenDownloadAsStream {
                    completed: pending
                        .await
                        .map_err(|error| error.to_string())
                        .and_then(|result| result),
                }
            }
            PendingBrowserCommandKind::ApplyPermissionOverrides { pending } => {
                let mut completed = Vec::with_capacity(pending.len());
                for page_command in pending {
                    completed.push(CompletedBrowserPageCommand {
                        target: page_command.target,
                        completed: page_command
                            .pending
                            .wait()
                            .await
                            .map_err(|error| error.to_string()),
                    });
                }
                CompletedBrowserCommandKind::ApplyPermissionOverrides { completed }
            }
        };
        CompletedBrowserCommandDispatch {
            command_id: self.command_id,
            response_session_id: self.response_session_id,
            kind,
        }
    }
}

impl CompletedBrowserCommandDispatch {
    pub(crate) fn command_id(&self) -> Option<u64> {
        self.command_id
    }

    pub(crate) fn session_id(&self) -> Option<&str> {
        self.response_session_id.as_deref()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, strum::EnumString, strum::IntoStaticStr)]
#[strum(serialize_all = "lowercase")]
enum PermissionSetting {
    Granted,
    Denied,
    Prompt,
}

impl PermissionSetting {
    fn parse(value: &str) -> Option<Self> {
        Self::from_str(value).ok()
    }

    fn label(self) -> &'static str {
        self.into()
    }
}

fn normalize_permission_setting(value: String) -> String {
    PermissionSetting::parse(&value)
        .map(PermissionSetting::label)
        .unwrap_or(value.as_str())
        .to_owned()
}

pub(crate) fn try_start_browser_command_dispatch(
    conn: &mut CdpConnection,
    cmd: &Cmd<'_>,
) -> BrowserCommandTaskStep {
    let Some(action) = cmd.parse_action::<BrowserAction>() else {
        return BrowserCommandTaskStep::Complete(CommandOutputPlan::error(-32601, "UnknownMethod"));
    };
    match action {
        BrowserAction::GetVersion => BrowserCommandTaskStep::Complete(get_version(conn)),
        BrowserAction::GetWindowForTarget => {
            BrowserCommandTaskStep::Complete(get_window_for_target(conn))
        }
        BrowserAction::SetWindowBounds => {
            BrowserCommandTaskStep::Complete(set_window_bounds(conn, cmd))
        }
        BrowserAction::SetDownloadBehavior => {
            BrowserCommandTaskStep::Complete(set_download_behavior_command_output_plan(conn, cmd))
        }
        BrowserAction::CancelDownload => {
            BrowserCommandTaskStep::Complete(cancel_download(conn, cmd))
        }
        BrowserAction::OpenDownloadAsStream => start_open_download_as_stream_command(conn, cmd),
        BrowserAction::SetPermission => start_set_permission_command(conn, cmd),
        BrowserAction::GrantPermissions => start_grant_permissions_command(conn, cmd),
        BrowserAction::ResetPermissions => start_reset_permissions_command(conn, cmd),
    }
}

fn get_version(conn: &CdpConnection) -> CommandOutputPlan {
    CommandOutputPlan::result(json!({
        "protocolVersion": version::PROTOCOL_VERSION,
        "product": version::PRODUCT,
        "revision": version::REVISION,
        "userAgent": conn.user_agent(),
        "jsVersion": version::js_version(),
    }))
}

fn bounds_json(bounds: &BrowserWindowBounds) -> Value {
    let mut value = json!({
        "windowState": bounds.window_state,
    });
    let object = value
        .as_object_mut()
        .expect("browser bounds json must be an object");
    if let Some(left) = bounds.left {
        object.insert("left".to_owned(), json!(left));
    }
    if let Some(top) = bounds.top {
        object.insert("top".to_owned(), json!(top));
    }
    if let Some(width) = bounds.width {
        object.insert("width".to_owned(), json!(width));
    }
    if let Some(height) = bounds.height {
        object.insert("height".to_owned(), json!(height));
    }
    value
}

fn get_window_for_target(conn: &CdpConnection) -> CommandOutputPlan {
    let policy = conn.browser_host_policy_snapshot();
    CommandOutputPlan::result(json!({
        "windowId": DEV_TOOLS_WINDOW_ID,
        "bounds": bounds_json(policy.window_bounds())
    }))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SetDownloadBehaviorParams {
    behavior: String,
    #[serde(default)]
    download_path: Option<String>,
    #[serde(default)]
    events_enabled: bool,
    #[serde(default)]
    browser_context_id: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SetPermissionParams {
    permission: Value,
    setting: String,
    #[serde(default)]
    origin: Option<String>,
    #[serde(default)]
    embedded_origin: Option<String>,
    #[serde(default)]
    browser_context_id: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GrantPermissionsParams {
    permissions: Vec<Value>,
    #[serde(default)]
    origin: Option<String>,
    #[serde(default)]
    browser_context_id: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ResetPermissionsParams {
    #[serde(default)]
    browser_context_id: Option<String>,
}

fn optional_i64_to_i32(value: Option<i64>) -> Result<Option<i32>, ()> {
    value.map(i32::try_from).transpose().map_err(|_| ())
}

fn optional_i64_to_u32(value: Option<i64>) -> Result<Option<u32>, ()> {
    value.map(u32::try_from).transpose().map_err(|_| ())
}

fn set_window_bounds(conn: &mut CdpConnection, cmd: &Cmd<'_>) -> CommandOutputPlan {
    let params: SetWindowBoundsParams = match cmd.get_params() {
        Ok(Some(params)) => params,
        _ => {
            return CommandOutputPlan::error(-32602, "InvalidParams");
        }
    };

    if *params.window_id.inner() != i64::from(DEV_TOOLS_WINDOW_ID) {
        return CommandOutputPlan::error(-32602, "InvalidParams");
    }

    let left = match optional_i64_to_i32(params.bounds.left) {
        Ok(left) => left,
        Err(()) => {
            return CommandOutputPlan::error(-32602, "InvalidParams");
        }
    };
    let top = match optional_i64_to_i32(params.bounds.top) {
        Ok(top) => top,
        Err(()) => {
            return CommandOutputPlan::error(-32602, "InvalidParams");
        }
    };
    let width = match optional_i64_to_u32(params.bounds.width) {
        Ok(width) => width,
        Err(()) => {
            return CommandOutputPlan::error(-32602, "InvalidParams");
        }
    };
    let height = match optional_i64_to_u32(params.bounds.height) {
        Ok(height) => height,
        Err(()) => {
            return CommandOutputPlan::error(-32602, "InvalidParams");
        }
    };
    let mut bounds = conn.browser_host_policy_snapshot().window_bounds().clone();
    bounds.left = left;
    bounds.top = top;
    bounds.width = width;
    bounds.height = height;
    if let Some(window_state) = params.bounds.window_state {
        bounds.window_state = window_state.as_ref().to_owned();
    }
    conn.apply_browser_host_policy_update(
        moli_core::browser_host::BrowserHostPolicyUpdate::ReplaceWindowBounds(bounds),
    );

    CommandOutputPlan::success()
}

pub(crate) fn set_download_behavior_command_output_plan(
    conn: &mut CdpConnection,
    cmd: &Cmd<'_>,
) -> CommandOutputPlan {
    let params: SetDownloadBehaviorParams = match cmd.get_params() {
        Ok(Some(params)) => params,
        _ => {
            return CommandOutputPlan::error(-32602, "InvalidParams");
        }
    };

    if let Some(wanted_id) = params.browser_context_id.as_deref()
        && !conn.has_browser_context_id(wanted_id)
    {
        return CommandOutputPlan::error(-31998, "UnknownBrowserContextId");
    }
    let Some(behavior) = BrowserDownloadBehavior::parse(params.behavior.as_str()) else {
        return CommandOutputPlan::error(-32602, "InvalidParams");
    };

    match params.browser_context_id {
        Some(browser_context_id) => {
            conn.apply_browser_download_policy_update(
                BrowserDownloadPolicyUpdate::SetBrowserContext {
                    browser_context_id: browser_context_id.clone(),
                    behavior,
                    download_path: params.download_path,
                },
            );
            conn.ensure_automation_download_event_override_for_browser_context(&browser_context_id);
        }
        None => conn.apply_browser_download_policy_update(BrowserDownloadPolicyUpdate::SetGlobal {
            behavior,
            download_path: params.download_path,
        }),
    }
    conn.set_browser_download_events_enabled_for_session(cmd.session_id, params.events_enabled);

    CommandOutputPlan::success()
}

pub(crate) fn execute_devtools_browser_command(
    conn: &mut CdpConnection,
    command: DevToolsCommand,
) -> Result<DevToolsCommandResult, DevToolsError> {
    match command {
        DevToolsCommand::SetDownloadBehavior(command) => {
            execute_devtools_set_download_behavior(conn, command)
        }
        _ => Err(DevToolsError::new(
            DevToolsErrorKind::Unsupported,
            "UnsupportedDevToolsCommand",
        )),
    }
}

pub(crate) async fn execute_devtools_browser_command_async(
    conn: &mut CdpConnection,
    command: DevToolsCommand,
) -> Result<DevToolsCommandResult, DevToolsError> {
    match command {
        DevToolsCommand::SetPermission(command) => {
            execute_devtools_set_permission_command_async(conn, command).await
        }
        _ => Err(DevToolsError::new(
            DevToolsErrorKind::Unsupported,
            "UnsupportedDevToolsCommand",
        )),
    }
}

fn execute_devtools_set_download_behavior(
    conn: &mut CdpConnection,
    command: DevToolsSetDownloadBehaviorCommand,
) -> Result<DevToolsCommandResult, DevToolsError> {
    let target_contexts = match command.user_contexts {
        Some(user_contexts) => {
            if user_contexts.is_empty() {
                return Err(DevToolsError::new(
                    DevToolsErrorKind::InvalidArgument,
                    "user contexts must not be empty",
                ));
            }
            for browser_context_id in &user_contexts {
                if !conn.has_browser_context_id(browser_context_id.as_str()) {
                    return Err(DevToolsError::new(
                        DevToolsErrorKind::NoSuchTarget,
                        "UnknownBrowserContextId",
                    ));
                }
            }
            Some(user_contexts)
        }
        None => None,
    };

    let Some(behavior) = command.behavior else {
        match target_contexts {
            Some(user_contexts) => {
                for browser_context_id in user_contexts {
                    let browser_context_id = browser_context_id.into_string();
                    conn.apply_browser_download_policy_update(
                        BrowserDownloadPolicyUpdate::RemoveBrowserContext {
                            browser_context_id: browser_context_id.clone(),
                        },
                    );
                    conn.clear_automation_download_events_for_browser_context(&browser_context_id);
                }
            }
            None => {
                conn.apply_browser_download_policy_update(BrowserDownloadPolicyUpdate::ResetGlobal);
                conn.set_automation_download_events_enabled_for_browser_context(None, false);
            }
        }
        return Ok(DevToolsCommandResult::Empty);
    };

    let Some(parsed_behavior) = BrowserDownloadBehavior::parse(behavior.behavior.as_str()) else {
        return Err(DevToolsError::new(
            DevToolsErrorKind::InvalidArgument,
            "download behavior is invalid",
        ));
    };

    match target_contexts {
        Some(user_contexts) => {
            for browser_context_id in user_contexts {
                let browser_context_id = browser_context_id.into_string();
                conn.apply_browser_download_policy_update(
                    BrowserDownloadPolicyUpdate::SetBrowserContext {
                        browser_context_id: browser_context_id.clone(),
                        behavior: parsed_behavior,
                        download_path: behavior.download_path.clone(),
                    },
                );
                conn.set_automation_download_events_enabled_for_browser_context(
                    Some(&browser_context_id),
                    behavior.events_enabled,
                );
            }
        }
        None => {
            conn.apply_browser_download_policy_update(BrowserDownloadPolicyUpdate::SetGlobal {
                behavior: parsed_behavior,
                download_path: behavior.download_path,
            });
            conn.set_automation_download_events_enabled_for_browser_context(
                None,
                behavior.events_enabled,
            );
        }
    }
    Ok(DevToolsCommandResult::Empty)
}

async fn execute_devtools_set_permission_command_async(
    conn: &mut CdpConnection,
    command: DevToolsSetPermissionCommand,
) -> Result<DevToolsCommandResult, DevToolsError> {
    let browser_context_id = command.browser_context_id.as_ref().map(|id| id.as_str());
    if validate_browser_context_id(conn, browser_context_id).is_err() {
        return Err(DevToolsError::new(
            DevToolsErrorKind::NoSuchTarget,
            "UnknownBrowserContextId",
        ));
    }
    let browser_context_id = command
        .browser_context_id
        .map(|browser_context_id| browser_context_id.into_string());

    let permission = command.permission;
    let setting = normalize_permission_setting(command.setting);
    let origin = command.origin;
    let embedded_origin = command.embedded_origin;
    let mut overrides = conn
        .browser_host_policy_snapshot()
        .permission_overrides()
        .to_vec();
    overrides.retain(|override_entry| {
        override_entry.permission != permission
            || override_entry.origin.as_deref() != Some(origin.as_str())
            || override_entry.embedded_origin != embedded_origin
            || override_entry.browser_context_id != browser_context_id
    });
    overrides.push(crate::conn::PermissionOverride {
        permission,
        setting,
        origin: Some(origin),
        embedded_origin,
        browser_context_id,
    });
    conn.apply_browser_host_policy_update(
        moli_core::browser_host::BrowserHostPolicyUpdate::ReplacePermissionOverrides(overrides),
    );

    let pending = start_loaded_page_permission_override_commands(conn)
        .map_err(|error| DevToolsError::new(DevToolsErrorKind::Internal, error))?;
    if pending.is_empty() {
        return Ok(DevToolsCommandResult::Empty);
    }
    let completed = PendingBrowserCommandDispatch {
        command_id: None,
        response_session_id: command
            .context
            .session_id
            .map(|session_id| session_id.into_string()),
        kind: PendingBrowserCommandKind::ApplyPermissionOverrides { pending },
    }
    .wait()
    .await;
    let CompletedBrowserCommandKind::ApplyPermissionOverrides {
        completed: commands,
    } = completed.kind
    else {
        unreachable!("set permission can only wait for permission override commands")
    };
    for command in commands {
        let completion = command
            .completed
            .map_err(|error| DevToolsError::new(DevToolsErrorKind::Internal, error))?;
        finish_pending_permission_override_command(conn, command.target, completion)
            .map_err(|error| DevToolsError::new(DevToolsErrorKind::Internal, error))?;
    }
    Ok(DevToolsCommandResult::Empty)
}

fn cancel_download(conn: &mut CdpConnection, cmd: &Cmd<'_>) -> CommandOutputPlan {
    let params: CancelDownloadParams = match cmd.get_params() {
        Ok(Some(params)) => params,
        _ => {
            return CommandOutputPlan::error(-32602, "InvalidParams");
        }
    };

    match conn.cancel_download(&params.guid) {
        Ok(()) => CommandOutputPlan::success(),
        Err(message) => CommandOutputPlan::error(-32602, message),
    }
}

fn start_open_download_as_stream_command(
    conn: &mut CdpConnection,
    cmd: &Cmd<'_>,
) -> BrowserCommandTaskStep {
    let params: CancelDownloadParams = match cmd.get_params() {
        Ok(Some(params)) => params,
        _ => {
            return browser_error_step(-32602, "InvalidParams");
        }
    };

    match conn.start_open_download_as_stream(&params.guid) {
        Ok(pending) => BrowserCommandTaskStep::Pending(PendingBrowserCommandDispatch {
            command_id: cmd.id,
            response_session_id: cmd.session_id.map(str::to_owned),
            kind: PendingBrowserCommandKind::OpenDownloadAsStream { pending },
        }),
        Err(message) => browser_error_step(-32602, message),
    }
}

fn validate_browser_context_id(
    conn: &CdpConnection,
    browser_context_id: Option<&str>,
) -> Result<(), ()> {
    if let Some(wanted_id) = browser_context_id
        && !conn.has_browser_context_id(wanted_id)
    {
        return Err(());
    }
    Ok(())
}

fn start_set_permission_command(conn: &mut CdpConnection, cmd: &Cmd<'_>) -> BrowserCommandTaskStep {
    let params: SetPermissionParams = match cmd.get_params() {
        Ok(Some(params)) => params,
        _ => {
            return browser_error_step(-32602, "InvalidParams");
        }
    };
    if validate_browser_context_id(conn, params.browser_context_id.as_deref()).is_err() {
        return browser_error_step(-31998, "UnknownBrowserContextId");
    }

    let mut overrides = conn
        .browser_host_policy_snapshot()
        .permission_overrides()
        .to_vec();
    overrides.retain(|override_entry| {
        override_entry.permission != params.permission
            || override_entry.origin != params.origin
            || override_entry.embedded_origin != params.embedded_origin
            || override_entry.browser_context_id != params.browser_context_id
    });
    overrides.push(crate::conn::PermissionOverride {
        permission: params.permission,
        setting: normalize_permission_setting(params.setting),
        origin: params.origin,
        embedded_origin: params.embedded_origin,
        browser_context_id: params.browser_context_id,
    });
    conn.apply_browser_host_policy_update(
        moli_core::browser_host::BrowserHostPolicyUpdate::ReplacePermissionOverrides(overrides),
    );
    start_apply_permission_overrides_command(conn, cmd)
}

fn start_grant_permissions_command(
    conn: &mut CdpConnection,
    cmd: &Cmd<'_>,
) -> BrowserCommandTaskStep {
    let params: GrantPermissionsParams = match cmd.get_params() {
        Ok(Some(params)) => params,
        _ => {
            return browser_error_step(-32602, "InvalidParams");
        }
    };
    if validate_browser_context_id(conn, params.browser_context_id.as_deref()).is_err() {
        return browser_error_step(-31998, "UnknownBrowserContextId");
    }

    let mut overrides = conn
        .browser_host_policy_snapshot()
        .permission_overrides()
        .to_vec();
    for permission in params.permissions {
        overrides.retain(|override_entry| {
            override_entry.permission != permission
                || override_entry.origin != params.origin
                || override_entry.embedded_origin.is_some()
                || override_entry.browser_context_id != params.browser_context_id
        });
        overrides.push(crate::conn::PermissionOverride {
            permission,
            setting: PermissionSetting::Granted.label().to_owned(),
            origin: params.origin.clone(),
            embedded_origin: None,
            browser_context_id: params.browser_context_id.clone(),
        });
    }
    conn.apply_browser_host_policy_update(
        moli_core::browser_host::BrowserHostPolicyUpdate::ReplacePermissionOverrides(overrides),
    );

    start_apply_permission_overrides_command(conn, cmd)
}

fn start_reset_permissions_command(
    conn: &mut CdpConnection,
    cmd: &Cmd<'_>,
) -> BrowserCommandTaskStep {
    let params: ResetPermissionsParams = match cmd.get_params() {
        Ok(Some(params)) => params,
        Ok(None) => ResetPermissionsParams {
            browser_context_id: None,
        },
        Err(_) => {
            return browser_error_step(-32602, "InvalidParams");
        }
    };
    if validate_browser_context_id(conn, params.browser_context_id.as_deref()).is_err() {
        return browser_error_step(-31998, "UnknownBrowserContextId");
    }

    if let Some(browser_context_id) = params.browser_context_id {
        let mut overrides = conn
            .browser_host_policy_snapshot()
            .permission_overrides()
            .to_vec();
        overrides.retain(|entry| {
            entry.browser_context_id.as_deref() != Some(browser_context_id.as_str())
        });
        conn.apply_browser_host_policy_update(
            moli_core::browser_host::BrowserHostPolicyUpdate::ReplacePermissionOverrides(overrides),
        );
    } else {
        conn.apply_browser_host_policy_update(
            moli_core::browser_host::BrowserHostPolicyUpdate::ReplacePermissionOverrides(
                Vec::new(),
            ),
        );
    }

    start_apply_permission_overrides_command(conn, cmd)
}

fn browser_error_step(code: i32, message: impl Into<String>) -> BrowserCommandTaskStep {
    BrowserCommandTaskStep::Complete(CommandOutputPlan::error(code, message))
}

fn browser_success_step() -> BrowserCommandTaskStep {
    BrowserCommandTaskStep::Complete(CommandOutputPlan::success())
}

fn start_apply_permission_overrides_command(
    conn: &mut CdpConnection,
    cmd: &Cmd<'_>,
) -> BrowserCommandTaskStep {
    let pending = match start_loaded_page_permission_override_commands(conn) {
        Ok(pending) => pending,
        Err(message) => return browser_error_step(-32000, message),
    };
    if pending.is_empty() {
        return browser_success_step();
    }
    BrowserCommandTaskStep::Pending(PendingBrowserCommandDispatch {
        command_id: cmd.id,
        response_session_id: cmd.session_id.map(str::to_owned),
        kind: PendingBrowserCommandKind::ApplyPermissionOverrides { pending },
    })
}

fn start_loaded_page_permission_override_commands(
    conn: &mut CdpConnection,
) -> Result<Vec<PendingBrowserPageCommand>, String> {
    let all_overrides = conn
        .browser_host_policy_snapshot()
        .permission_overrides()
        .to_vec();
    let mut pending = Vec::new();
    for browser_context in conn
        .browser_context
        .iter_mut()
        .chain(conn.inactive_browser_contexts.iter_mut())
    {
        let browser_context_id = browser_context.id.clone();
        let effective_overrides = all_overrides
            .iter()
            .filter(|entry| {
                entry.browser_context_id.is_none()
                    || entry.browser_context_id.as_deref() == Some(browser_context_id.as_str())
            })
            .map(|entry| moli_core::page::PermissionOverrideRegistration {
                permission: entry.permission.clone(),
                setting: entry.setting.clone(),
                origin: entry.origin.clone(),
                embedded_origin: entry.embedded_origin.clone(),
            })
            .collect::<Vec<_>>();
        if let Some(page) = browser_context.active_target.runtime_slot.loaded_page_mut() {
            pending.push(PendingBrowserPageCommand {
                target: PendingBrowserPageTarget::BrowserContextActive {
                    browser_context_id: browser_context_id.clone(),
                },
                pending: page
                    .start_set_permission_overrides(&effective_overrides)
                    .map_err(|error| {
                        format!("failed to update page permission overrides: {error}")
                    })?,
            });
        }
        for target in &mut browser_context.background_targets {
            let target_id = target.target_id().to_owned();
            let Some(page) = target.loaded_page_mut() else {
                continue;
            };
            pending.push(PendingBrowserPageCommand {
                target: PendingBrowserPageTarget::BrowserContextBackground {
                    browser_context_id: browser_context_id.clone(),
                    target_id,
                },
                pending: page
                    .start_set_permission_overrides(&effective_overrides)
                    .map_err(|error| {
                        format!("failed to update page permission overrides: {error}")
                    })?,
            });
        }
    }
    Ok(pending)
}

pub(crate) fn complete_pending_browser_command(
    conn: &mut CdpConnection,
    completed: CompletedBrowserCommandDispatch,
) -> CommandOutputPlan {
    match completed.kind {
        CompletedBrowserCommandKind::OpenDownloadAsStream { completed: bytes } => match bytes {
            Ok(bytes) => {
                let stream = conn.finish_open_download_as_stream(bytes);
                CommandOutputPlan::result(json!({ "stream": stream }))
            }
            Err(message) => CommandOutputPlan::error(-32602, message),
        },
        CompletedBrowserCommandKind::ApplyPermissionOverrides {
            completed: commands,
        } => {
            for command in commands {
                let completion = match command.completed {
                    Ok(completion) => completion,
                    Err(error) => {
                        return CommandOutputPlan::error(-32000, error);
                    }
                };
                if let Err(error) =
                    finish_pending_permission_override_command(conn, command.target, completion)
                {
                    return CommandOutputPlan::error(-32000, error);
                }
            }
            CommandOutputPlan::success()
        }
    }
}

fn finish_pending_permission_override_command(
    conn: &mut CdpConnection,
    target: PendingBrowserPageTarget,
    completion: CompletedPageCommand,
) -> Result<(), String> {
    let mut page = match target {
        PendingBrowserPageTarget::BrowserContextActive { browser_context_id } => conn
            .browser_context_by_id_mut(&browser_context_id)
            .and_then(|browser_context| {
                browser_context.active_target.runtime_slot.loaded_page_mut()
            }),
        PendingBrowserPageTarget::BrowserContextBackground {
            browser_context_id,
            target_id,
        } => conn
            .browser_context_by_id_mut(&browser_context_id)
            .and_then(|browser_context| browser_context.background_target_mut(&target_id))
            .and_then(|target| target.loaded_page_mut()),
    }
    .ok_or_else(|| "NoDocumentLoaded".to_owned())?;
    page.finish_set_permission_overrides(completion)
        .map_err(|error| error.to_string())
}

// ────────────────────────────────────────────────────────────────────────────
// Tests – ported from lightpanda/src/cdp/domains/browser.zig
// ────────────────────────────────────────────────────────────────────────────
#[cfg(test)]
mod tests {
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;
    use crate::conn::CdpCommandTaskStep;
    use crate::testing::TestContext;
    use axum::{Router, response::IntoResponse, routing::get};
    use tokio::net::TcpListener;

    #[test]
    fn permission_setting_parses_standard_cdp_tokens() {
        assert_eq!(
            PermissionSetting::parse("granted"),
            Some(PermissionSetting::Granted)
        );
        assert_eq!(
            PermissionSetting::parse("denied"),
            Some(PermissionSetting::Denied)
        );
        assert_eq!(
            PermissionSetting::parse("prompt"),
            Some(PermissionSetting::Prompt)
        );
        assert_eq!(PermissionSetting::parse("Granted"), None);
        assert_eq!(PermissionSetting::parse("unknown"), None);
    }

    #[test]
    fn normalize_permission_setting_preserves_unknown_tokens() {
        assert_eq!(
            normalize_permission_setting("granted".to_owned()),
            "granted"
        );
        assert_eq!(normalize_permission_setting("prompt".to_owned()), "prompt");
        assert_eq!(
            normalize_permission_setting("experimental".to_owned()),
            "experimental"
        );
    }

    fn take_response_by_id(ctx: &mut TestContext, id: u64) -> Value {
        let pos = ctx
            .sent
            .iter()
            .position(|message| message["id"] == json!(id))
            .expect("expected response with matching id");
        ctx.sent.remove(pos)
    }

    async fn with_loaded_document_async(ctx: &mut TestContext, url: &str) {
        let mut browser_context = crate::conn::BrowserContext::new("BID-1".into());
        browser_context.set_active_target_id("TID-1");
        browser_context.attach_active_session("SID-1");
        ctx.conn.insert_browser_context(browser_context);
        let page = ctx
            .conn
            .load_page_via_runtime_async(url)
            .await
            .expect("must load document");
        let _ = ctx
            .conn
            .browser_context
            .as_mut()
            .expect("inserted browser context must be selected")
            .active_target
            .runtime_slot
            .replace_loaded_page(Some(page));
    }

    async fn current_permission_state_async(
        ctx: &mut TestContext,
        permission_name: &str,
    ) -> String {
        current_permission_state_for_session_async(ctx, "SID-1", permission_name).await
    }

    async fn current_permission_state_for_session_async(
        ctx: &mut TestContext,
        session_id: &str,
        permission_name: &str,
    ) -> String {
        ctx.process_async(json!({
            "id": 9_000,
            "method": "Runtime.evaluate",
            "sessionId": session_id,
            "params": {
                "expression": format!(
                    "(() => {{ globalThis.__permissionState = 'pending'; navigator.permissions.query({{ name: '{}' }}).then(status => {{ globalThis.__permissionState = status.state; }}); return 'scheduled'; }})()",
                    permission_name
                )
            }
        }))
        .await;
        let response = take_response_by_id(ctx, 9_000);
        assert_eq!(response["result"]["result"]["value"], json!("scheduled"));

        ctx.process_async(json!({
            "id": 9_001,
            "method": "Runtime.evaluate",
            "sessionId": session_id,
            "params": { "expression": "globalThis.__permissionState" }
        }))
        .await;
        take_response_by_id(ctx, 9_001)["result"]["result"]["value"]
            .as_str()
            .expect("permission state should be a string")
            .to_owned()
    }

    /// cdp.browser: getVersion
    #[tokio::test(flavor = "multi_thread")]
    async fn get_version_returns_chromium_compatible_metadata() {
        let mut ctx = TestContext::new();
        ctx.process_async(json!({"id": 32, "method": "Browser.getVersion"}))
            .await;
        ctx.expect_result(
            32,
            json!({
                "protocolVersion": version::PROTOCOL_VERSION,
                "product": version::PRODUCT,
                "revision": version::REVISION,
                "userAgent": moli_fetch::FetchConfig::DEFAULT_USER_AGENT,
                "jsVersion": version::js_version(),
            }),
            None,
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn get_version_preserves_target_session_route() {
        let mut ctx = TestContext::new();
        let mut browser_context = crate::conn::BrowserContext::new("BID-1".into());
        browser_context.set_active_target_id("TID-1");
        browser_context.attach_active_session("SID-1");
        ctx.conn.browser_context = Some(browser_context);
        ctx.process_async(json!({
            "id": 32_001,
            "method": "Browser.getVersion",
            "sessionId": "SID-1"
        }))
        .await;
        ctx.expect_result(
            32_001,
            json!({
                "protocolVersion": version::PROTOCOL_VERSION,
                "product": version::PRODUCT,
                "revision": version::REVISION,
                "userAgent": moli_fetch::FetchConfig::DEFAULT_USER_AGENT,
                "jsVersion": version::js_version(),
            }),
            Some("SID-1"),
        );
    }

    /// cdp.browser: getWindowForTarget
    #[tokio::test(flavor = "multi_thread")]
    async fn get_window_for_target_returns_window_info() {
        let mut ctx = TestContext::new();
        ctx.process_async(json!({"id": 33, "method": "Browser.getWindowForTarget"}))
            .await;
        ctx.expect_result(
            33,
            json!({
                "windowId": DEV_TOOLS_WINDOW_ID,
                "bounds": { "windowState": "normal" }
            }),
            None,
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn set_window_bounds_updates_get_window_for_target() {
        let mut ctx = TestContext::new();
        ctx.process_async(json!({
            "id": 34,
            "method": "Browser.setWindowBounds",
            "params": {
                "windowId": DEV_TOOLS_WINDOW_ID,
                "bounds": {
                    "left": 10,
                    "top": 20,
                    "width": 1280,
                    "height": 720,
                    "windowState": "minimized"
                }
            }
        }))
        .await;
        ctx.expect_result(34, json!({}), None);

        ctx.process_async(json!({"id": 35, "method": "Browser.getWindowForTarget"}))
            .await;
        ctx.expect_result(
            35,
            json!({
                "windowId": DEV_TOOLS_WINDOW_ID,
                "bounds": {
                    "left": 10,
                    "top": 20,
                    "width": 1280,
                    "height": 720,
                    "windowState": "minimized"
                }
            }),
            None,
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn set_window_bounds_rejects_unknown_window_id() {
        let mut ctx = TestContext::new();
        ctx.process_async(json!({
            "id": 36,
            "method": "Browser.setWindowBounds",
            "params": {
                "windowId": 999,
                "bounds": { "windowState": "normal" }
            }
        }))
        .await;
        ctx.expect_error(36, -32602, "InvalidParams");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn set_download_behavior_updates_connection_state_and_context_override() {
        let mut ctx = TestContext::new();
        ctx.conn
            .insert_browser_context(crate::conn::BrowserContext::new("BID-1".into()));

        ctx.process_async(json!({
            "id": 36_901,
            "method": "Browser.setDownloadBehavior",
            "params": {
                "behavior": "allowAndName",
                "downloadPath": "/tmp/global-downloads",
                "eventsEnabled": true
            }
        }))
        .await;
        ctx.expect_result(36_901, json!({}), None);

        ctx.process_async(json!({
            "id": 37,
            "method": "Browser.setDownloadBehavior",
            "params": {
                "behavior": "allow",
                "downloadPath": "/tmp/downloads",
                "eventsEnabled": true,
                "browserContextId": "BID-1"
            }
        }))
        .await;
        ctx.expect_result(37, json!({}), None);

        let policy = ctx.conn.browser_download_policy_snapshot();
        assert_eq!(
            policy.global().behavior(),
            BrowserDownloadBehavior::AllowAndName
        );
        assert_eq!(
            policy.global().download_path(),
            Some("/tmp/global-downloads")
        );
        assert!(
            !ctx.conn
                .automation_download_events_enabled_for_browser_context(None)
        );
        assert_eq!(
            ctx.conn.browser_download_event_session_ids_for_test(),
            vec![None]
        );
        assert!(policy.browser_context_override("BID-1").is_some());

        let context_settings = policy.effective_for_browser_context(Some("BID-1"));
        assert_eq!(context_settings.behavior(), BrowserDownloadBehavior::Allow);
        assert_eq!(context_settings.download_path(), Some("/tmp/downloads"));
        assert!(
            !ctx.conn
                .automation_download_events_enabled_for_browser_context(Some("BID-1"))
        );

        let global_settings = policy.effective_for_browser_context(None);
        assert_eq!(
            global_settings.behavior(),
            BrowserDownloadBehavior::AllowAndName
        );
        assert_eq!(
            global_settings.download_path(),
            Some("/tmp/global-downloads")
        );
        assert!(
            !ctx.conn
                .automation_download_events_enabled_for_browser_context(None)
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn set_download_behavior_tracks_event_observers_per_session_and_detach() {
        let mut ctx = TestContext::new();
        ctx.conn
            .register_browser_session("SID-browser-a".to_owned());
        ctx.conn
            .register_browser_session("SID-browser-b".to_owned());

        ctx.process_async(json!({
            "id": 37_001,
            "method": "Browser.setDownloadBehavior",
            "sessionId": "SID-browser-a",
            "params": {
                "behavior": "allowAndName",
                "downloadPath": "/tmp/downloads-a",
                "eventsEnabled": true
            }
        }))
        .await;
        ctx.expect_result(37_001, json!({}), Some("SID-browser-a"));

        ctx.process_async(json!({
            "id": 37_002,
            "method": "Browser.setDownloadBehavior",
            "sessionId": "SID-browser-b",
            "params": {
                "behavior": "allowAndName",
                "downloadPath": "/tmp/downloads-b",
                "eventsEnabled": false
            }
        }))
        .await;
        ctx.expect_result(37_002, json!({}), Some("SID-browser-b"));
        assert_eq!(
            ctx.conn.browser_download_event_session_ids_for_test(),
            vec![Some("SID-browser-a".to_owned())]
        );

        assert!(
            ctx.conn
                .detach_browser_session_owner_without_event("SID-browser-a")
                .is_some()
        );
        assert!(
            ctx.conn
                .browser_download_event_session_ids_for_test()
                .is_empty()
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn set_download_behavior_rejects_unknown_browser_context_id() {
        let mut ctx = TestContext::new();
        ctx.process_async(json!({
            "id": 38,
            "method": "Browser.setDownloadBehavior",
            "params": {
                "behavior": "allow",
                "browserContextId": "BID-missing"
            }
        }))
        .await;
        ctx.expect_error(38, -31998, "UnknownBrowserContextId");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn set_download_behavior_rejects_invalid_params() {
        let mut ctx = TestContext::new();
        ctx.process_async(json!({
            "id": 39,
            "method": "Browser.setDownloadBehavior",
            "params": {}
        }))
        .await;
        ctx.expect_error(39, -32602, "InvalidParams");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn set_download_behavior_rejects_unknown_behavior() {
        let mut ctx = TestContext::new();
        ctx.process_async(json!({
            "id": 390,
            "method": "Browser.setDownloadBehavior",
            "params": {
                "behavior": "allowandname",
                "downloadPath": "/tmp/downloads"
            }
        }))
        .await;
        ctx.expect_error(390, -32602, "InvalidParams");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn cancel_download_rejects_unknown_guid() {
        let mut ctx = TestContext::new();
        ctx.process_async(json!({
            "id": 391,
            "method": "Browser.cancelDownload",
            "params": { "guid": "missing-guid" }
        }))
        .await;
        ctx.expect_error(391, -32602, "No download item found for the given GUID");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn open_download_as_stream_reads_completed_artifact_via_io_domain() {
        let mut ctx = TestContext::new();
        let artifact_path = std::env::temp_dir().join(format!(
            "moli-cdp-download-stream-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        std::fs::write(&artifact_path, [0_u8, 255_u8, b'a'])
            .expect("test download artifact should be written");
        ctx.conn
            .test_insert_completed_download("DOWNLOAD-STREAM-1", artifact_path.clone());

        ctx.process_async(json!({
            "id": 392,
            "method": "Browser.openDownloadAsStream",
            "params": { "guid": "DOWNLOAD-STREAM-1" }
        }))
        .await;
        let stream = take_response_by_id(&mut ctx, 392)["result"]["stream"]
            .as_str()
            .expect("stream handle")
            .to_owned();
        assert!(stream.starts_with("BROWSER-STREAM-"));

        ctx.process_async(json!({
            "id": 393,
            "method": "IO.read",
            "params": { "handle": stream }
        }))
        .await;
        ctx.expect_result(
            393,
            json!({ "base64Encoded": true, "data": "AP9h", "eof": true }),
            None,
        );

        let _ = std::fs::remove_file(artifact_path);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn set_permission_updates_connection_state() {
        let mut ctx = TestContext::new();
        ctx.conn
            .insert_browser_context(crate::conn::BrowserContext::new("BID-1".into()));

        ctx.process_async(json!({
            "id": 40,
            "method": "Browser.setPermission",
            "params": {
                "permission": { "name": "clipboard-read" },
                "setting": "denied",
                "origin": "https://example.com",
                "browserContextId": "BID-1"
            }
        }))
        .await;
        ctx.expect_result(40, json!({}), None);
        let policy = ctx.conn.browser_host_policy_snapshot();
        let permission_overrides = policy.permission_overrides();
        assert_eq!(permission_overrides.len(), 1);
        assert_eq!(
            permission_overrides[0].permission,
            json!({ "name": "clipboard-read" })
        );
        assert_eq!(permission_overrides[0].setting, "denied");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn grant_permissions_records_granted_entries() {
        let mut ctx = TestContext::new();
        ctx.conn
            .insert_browser_context(crate::conn::BrowserContext::new("BID-1".into()));

        ctx.process_async(json!({
            "id": 41,
            "method": "Browser.grantPermissions",
            "params": {
                "permissions": [
                    { "name": "geolocation" },
                    { "name": "clipboard-read" }
                ],
                "origin": "https://example.com",
                "browserContextId": "BID-1"
            }
        }))
        .await;
        ctx.expect_result(41, json!({}), None);
        let policy = ctx.conn.browser_host_policy_snapshot();
        assert_eq!(policy.permission_overrides().len(), 2);
        assert!(
            policy
                .permission_overrides()
                .iter()
                .all(|entry| entry.setting == "granted")
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn reset_permissions_clears_matching_browser_context_scope() {
        let mut ctx = TestContext::new();
        ctx.conn
            .insert_browser_context(crate::conn::BrowserContext::new("BID-1".into()));
        ctx.conn.apply_browser_host_policy_update(
            moli_core::browser_host::BrowserHostPolicyUpdate::ReplacePermissionOverrides(vec![
                crate::conn::PermissionOverride {
                    permission: json!({ "name": "clipboard-read" }),
                    setting: "granted".into(),
                    origin: Some("https://example.com".into()),
                    embedded_origin: None,
                    browser_context_id: Some("BID-1".into()),
                },
                crate::conn::PermissionOverride {
                    permission: json!({ "name": "clipboard-write" }),
                    setting: "granted".into(),
                    origin: Some("https://example.com".into()),
                    embedded_origin: None,
                    browser_context_id: None,
                },
            ]),
        );

        ctx.process_async(json!({
            "id": 42,
            "method": "Browser.resetPermissions",
            "params": { "browserContextId": "BID-1" }
        }))
        .await;
        ctx.expect_result(42, json!({}), None);
        let policy = ctx.conn.browser_host_policy_snapshot();
        assert_eq!(policy.permission_overrides().len(), 1);
        assert_eq!(policy.permission_overrides()[0].browser_context_id, None);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn permission_methods_reject_unknown_browser_context_id() {
        let mut ctx = TestContext::new();
        ctx.process_async(json!({
            "id": 43,
            "method": "Browser.setPermission",
            "params": {
                "permission": { "name": "clipboard-read" },
                "setting": "granted",
                "browserContextId": "BID-missing"
            }
        }))
        .await;
        ctx.expect_error(43, -31998, "UnknownBrowserContextId");

        ctx.process_async(json!({
            "id": 44,
            "method": "Browser.grantPermissions",
            "params": {
                "permissions": [{ "name": "geolocation" }],
                "browserContextId": "BID-missing"
            }
        }))
        .await;
        ctx.expect_error(44, -31998, "UnknownBrowserContextId");

        ctx.process_async(json!({
            "id": 45,
            "method": "Browser.resetPermissions",
            "params": { "browserContextId": "BID-missing" }
        }))
        .await;
        ctx.expect_error(45, -31998, "UnknownBrowserContextId");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn grant_and_reset_permissions_update_loaded_page_query_state() {
        let mut ctx = TestContext::new();
        with_loaded_document_async(&mut ctx, "data:text/html,<body></body>").await;

        ctx.process_async(json!({ "id": 46, "method": "Runtime.enable", "sessionId": "SID-1" }))
            .await;
        let response = take_response_by_id(&mut ctx, 46);
        assert_eq!(response["result"], json!({}));
        ctx.sent.clear();

        ctx.process_async(json!({
            "id": 47,
            "method": "Browser.grantPermissions",
            "params": {
                "permissions": [{ "name": "geolocation" }],
                "browserContextId": "BID-1"
            }
        }))
        .await;
        ctx.expect_result(47, json!({}), None);
        assert_eq!(
            current_permission_state_async(&mut ctx, "geolocation").await,
            "granted"
        );

        ctx.process_async(json!({
            "id": 48,
            "method": "Browser.resetPermissions",
            "params": { "browserContextId": "BID-1" }
        }))
        .await;
        ctx.expect_result(48, json!({}), None);
        assert_eq!(
            current_permission_state_async(&mut ctx, "geolocation").await,
            "prompt"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn set_permission_origin_updates_matching_loaded_page_query_state() {
        async fn handler() -> impl IntoResponse {
            "<!doctype html><html><body>ok</body></html>"
        }

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(listener, Router::new().route("/page", get(handler)))
                .await
                .unwrap();
        });

        let mut ctx = TestContext::new();
        let url = format!("http://{addr}/page");
        with_loaded_document_async(&mut ctx, &url).await;

        ctx.process_async(json!({ "id": 49, "method": "Runtime.enable", "sessionId": "SID-1" }))
            .await;
        let response = take_response_by_id(&mut ctx, 49);
        assert_eq!(response["result"], json!({}));
        ctx.sent.clear();

        ctx.process_async(json!({
            "id": 50,
            "method": "Browser.setPermission",
            "params": {
                "permission": { "name": "geolocation" },
                "setting": "denied",
                "origin": format!("http://{addr}"),
                "browserContextId": "BID-1"
            }
        }))
        .await;
        ctx.expect_result(50, json!({}), None);
        assert_eq!(
            current_permission_state_async(&mut ctx, "geolocation").await,
            "denied"
        );

        server.abort();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn async_dispatch_set_permission_updates_matching_loaded_page_query_state() {
        async fn handler() -> impl IntoResponse {
            "<!doctype html><html><body>ok</body></html>"
        }

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(listener, Router::new().route("/page", get(handler)))
                .await
                .unwrap();
        });

        let mut ctx = TestContext::new();
        let url = format!("http://{addr}/page");
        with_loaded_document_async(&mut ctx, &url).await;

        ctx.process_async(json!({
            "id": 51,
            "method": "Browser.setPermission",
            "params": {
                "permission": { "name": "geolocation" },
                "setting": "denied",
                "origin": format!("http://{addr}"),
                "browserContextId": "BID-1"
            }
        }))
        .await;
        ctx.expect_result(51, json!({}), None);
        assert_eq!(
            current_permission_state_async(&mut ctx, "geolocation").await,
            "denied"
        );

        server.abort();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn pending_permission_overrides_use_concrete_page_targets_across_ambient_owner_change() {
        let mut ctx = TestContext::new();
        let mut browser_context =
            crate::conn::BrowserContext::new("BID-browser-permission".to_owned());
        browser_context.set_active_target_id("TID-browser-permission-active".to_owned());
        browser_context.attach_active_session("SID-browser-permission-active".to_owned());
        let background = crate::conn::BackgroundTarget::with_url(
            "TID-browser-permission-background".to_owned(),
            Some("SID-browser-permission-background".to_owned()),
            "data:text/html,<title>background permissions</title>".to_owned(),
        );
        browser_context.background_targets.push(background);
        browser_context.adopt_background_target_fixture_attachments();
        ctx.conn.insert_browser_context(browser_context);
        ctx.install_navigation_fixture_for_session_owner(
            "data:text/html,<title>active permissions</title>",
            Some("SID-browser-permission-active"),
        )
        .await;
        ctx.install_navigation_fixture_for_session_owner(
            "data:text/html,<title>background permissions</title>",
            Some("SID-browser-permission-background"),
        )
        .await;

        for (id, session_id) in [
            (52, "SID-browser-permission-active"),
            (53, "SID-browser-permission-background"),
        ] {
            ctx.process_async(json!({
                "id": id,
                "method": "Runtime.enable",
                "sessionId": session_id
            }))
            .await;
            ctx.expect_result(id, json!({}), Some(session_id));
        }
        ctx.sent.clear();

        let background_route = ctx
            .conn
            .target_session_route_for_target_id("TID-browser-permission-background")
            .expect("background target route");
        let raw = serde_json::to_string(&json!({
            "id": 54,
            "method": "Browser.grantPermissions",
            "params": {
                "permissions": [{ "name": "geolocation" }],
                "browserContextId": "BID-browser-permission"
            }
        }))
        .expect("Browser.grantPermissions command should serialize");
        let pending = {
            let previous_route = ctx
                .conn
                .replace_none_session_owner_route_override(Some(background_route));
            let step = ctx.conn.start_command_dispatch(&raw);
            ctx.conn
                .replace_none_session_owner_route_override(previous_route);
            match step {
                CdpCommandTaskStep::Pending(pending) => pending,
                CdpCommandTaskStep::Complete(outcome) => {
                    panic!(
                        "Browser.grantPermissions should update loaded page permission overrides: {:?}",
                        outcome.into_parts().0
                    )
                }
            }
        };

        let active_route = ctx
            .conn
            .target_session_route_for_target_id("TID-browser-permission-active")
            .expect("active target route");
        let previous_route = ctx
            .conn
            .replace_none_session_owner_route_override(Some(active_route));
        let (messages, scheduler_events) = ctx
            .complete_command_task_step_for_test(CdpCommandTaskStep::Pending(pending))
            .await;
        ctx.conn
            .replace_none_session_owner_route_override(previous_route);

        assert!(scheduler_events.is_empty());
        assert_eq!(messages, vec![json!({ "id": 54, "result": {} })]);
        assert_eq!(
            ctx.conn
                .browser_context
                .as_ref()
                .and_then(crate::conn::BrowserContext::active_target_id),
            Some("TID-browser-permission-active")
        );

        assert_eq!(
            current_permission_state_for_session_async(
                &mut ctx,
                "SID-browser-permission-active",
                "geolocation"
            )
            .await,
            "granted"
        );
        assert_eq!(
            current_permission_state_for_session_async(
                &mut ctx,
                "SID-browser-permission-background",
                "geolocation"
            )
            .await,
            "granted"
        );
    }

    /// Noop methods should return an empty result without error.
    #[tokio::test(flavor = "multi_thread")]
    async fn noop_methods_return_empty_result() {
        for method in &["Browser.setPermission", "Browser.grantPermissions"] {
            let mut ctx = TestContext::new();
            ctx.process_async(json!({"id": 1, "method": method, "params": {}}))
                .await;
            ctx.expect_error(1, -32602, "InvalidParams");
        }
    }
}
