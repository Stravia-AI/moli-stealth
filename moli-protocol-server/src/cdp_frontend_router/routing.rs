use std::collections::{HashMap, HashSet};

use anyhow::{Result, bail};
use moli_protocol::ParsedCdpCommand;
use serde_json::{Value, json};

use crate::cdp_writer::CdpSocketSink;

use super::CdpPreparedFrontendCommand;

struct CdpCommandFrontend {
    frontend_id: u64,
    dispatch_session_id: Option<String>,
    client_session_id: Option<String>,
}

struct PendingCommandRoute {
    frontend: CdpCommandFrontend,
    client_command_id: u64,
    method: String,
    attach_target_id: Option<String>,
}

struct CdpSessionFrontendRoute {
    kind: CdpSessionFrontendKind,
    target_id: Option<String>,
    base_session_id: String,
    sink: CdpSocketSink,
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum CdpSessionFrontendKind {
    Browser,
    Page,
}

#[derive(Clone)]
struct FrontendSessionRoute {
    frontend_id: u64,
    kind: FrontendSessionKind,
}

#[derive(Clone)]
enum FrontendSessionKind {
    /// One hidden browser-target or page-target session owned by exactly one
    /// WebSocket frontend. Commands without a public sessionId dispatch here.
    Base,
    /// A client-visible session created underneath the base session (or one
    /// of its descendants). Parent and target identity enforce Chromium's
    /// per-TargetHandler session lookup boundary.
    Child {
        parent_session_id: Option<String>,
        target_id: Option<String>,
    },
}

struct CdpTargetSessionReferenceError {
    code: i32,
    message: &'static str,
}

pub(super) struct CdpRoutedFrontend {
    frontend_id: u64,
    sink: CdpSocketSink,
}

impl CdpRoutedFrontend {
    pub(super) fn frontend_id(&self) -> u64 {
        self.frontend_id
    }

    pub(super) fn enqueue_message(self, message: Value) -> bool {
        self.sink.enqueue_owned_message(message)
    }
}

#[derive(Default)]
pub(super) struct CdpFrontendRoutingState {
    // The downstream protocol connection is shared, so client command ids and
    // session ownership must never be used as global frontend identities.
    next_internal_command_id: u64,
    pending_commands: HashMap<u64, PendingCommandRoute>,
    frontends: HashMap<u64, CdpSessionFrontendRoute>,
    sessions: HashMap<String, FrontendSessionRoute>,
    private_sessions: HashSet<String>,
}

impl Drop for CdpFrontendRoutingState {
    fn drop(&mut self) {
        for frontend in self.frontends.values() {
            frontend.sink.close_after_flush();
        }
    }
}

impl CdpFrontendRoutingState {
    pub(super) fn prepare_command_str(
        &mut self,
        frontend_id: u64,
        raw: String,
    ) -> Option<CdpPreparedFrontendCommand> {
        match ParsedCdpCommand::parse_str(raw) {
            Ok(command) => self.prepare_command(frontend_id, command),
            Err(error) => Some(CdpPreparedFrontendCommand::ImmediateResponse {
                frontend_id,
                message: cdp_error_response(
                    error.command_id(),
                    error.response_code(),
                    error.response_message(),
                ),
            }),
        }
    }

    pub(super) fn prepare_command(
        &mut self,
        frontend_id: u64,
        command: ParsedCdpCommand,
    ) -> Option<CdpPreparedFrontendCommand> {
        let request = command.request();
        let client_command_id = request.id();
        let method = request.method().to_owned();
        let client_session_id = request.session_id().map(str::to_owned);
        let attach_target_id = (method == "Target.attachToTarget")
            .then(|| {
                request
                    .params()
                    .and_then(|params| params.get("targetId"))
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            })
            .flatten();
        let base_session_id = self.frontends.get(&frontend_id)?.base_session_id.clone();
        let dispatch_session_id = if let Some(session_id) = client_session_id.as_deref() {
            let Some(session) = self.sessions.get(session_id) else {
                return Some(CdpPreparedFrontendCommand::ImmediateResponse {
                    frontend_id,
                    message: cdp_error_response(
                        Some(client_command_id),
                        -32001,
                        "Unknown sessionId",
                    ),
                });
            };
            if session.frontend_id != frontend_id
                || matches!(session.kind, FrontendSessionKind::Base)
            {
                return Some(CdpPreparedFrontendCommand::ImmediateResponse {
                    frontend_id,
                    message: cdp_error_response(
                        Some(client_command_id),
                        -32001,
                        "Unknown sessionId",
                    ),
                });
            }
            Some(session_id.to_owned())
        } else {
            Some(base_session_id)
        };
        let target_session_reference = match self.resolve_target_session_reference(
            frontend_id,
            dispatch_session_id.as_deref(),
            &method,
            request.params(),
        ) {
            Ok(session_id) => session_id,
            Err(error) => {
                return Some(CdpPreparedFrontendCommand::ImmediateResponse {
                    frontend_id,
                    message: cdp_error_response(Some(client_command_id), error.code, error.message),
                });
            }
        };
        let command = if let Some(session_id) = target_session_reference.as_deref() {
            match command.rewrite_target_session_reference(session_id) {
                Ok(command) => command,
                Err(error) => {
                    tracing::error!(
                        ?error,
                        "frontend routing could not serialize a Target session reference"
                    );
                    return Some(CdpPreparedFrontendCommand::ImmediateResponse {
                        frontend_id,
                        message: cdp_error_response(
                            Some(client_command_id),
                            -32603,
                            "Internal error",
                        ),
                    });
                }
            }
        } else {
            command
        };
        let internal_command_id = self.allocate_internal_command_id();
        let command = match command
            .rewrite_frontend_route(internal_command_id, dispatch_session_id.as_deref())
        {
            Ok(command) => command,
            Err(error) => {
                tracing::error!(
                    ?error,
                    "frontend routing could not serialize a rewritten typed CDP command"
                );
                return Some(CdpPreparedFrontendCommand::ImmediateResponse {
                    frontend_id,
                    message: cdp_error_response(Some(client_command_id), -32603, "Internal error"),
                });
            }
        };
        self.pending_commands.insert(
            internal_command_id,
            PendingCommandRoute {
                frontend: CdpCommandFrontend {
                    frontend_id,
                    dispatch_session_id,
                    client_session_id,
                },
                client_command_id,
                method,
                attach_target_id,
            },
        );
        Some(CdpPreparedFrontendCommand::Command(command))
    }

    fn resolve_target_session_reference(
        &self,
        frontend_id: u64,
        dispatch_session_id: Option<&str>,
        method: &str,
        params: Option<&serde_json::Map<String, Value>>,
    ) -> std::result::Result<Option<String>, CdpTargetSessionReferenceError> {
        if !matches!(
            method,
            "Target.detachFromTarget" | "Target.sendMessageToTarget"
        ) {
            return Ok(None);
        }
        let Some(params) = params else {
            return Ok(None);
        };
        if let Some(session_id) = params.get("sessionId") {
            if let Some(session_id) = session_id.as_str() {
                let owned_direct_child = self.sessions.get(session_id).is_some_and(|session| {
                    session.frontend_id == frontend_id
                        && matches!(
                            &session.kind,
                            FrontendSessionKind::Child {
                                parent_session_id,
                                ..
                            } if parent_session_id.as_deref() == dispatch_session_id
                        )
                });
                return if owned_direct_child {
                    Ok(None)
                } else {
                    Err(CdpTargetSessionReferenceError {
                        code: -32602,
                        message: "No session with given id",
                    })
                };
            }
            if !session_id.is_null() {
                // Preserve domain validation for a malformed optional value;
                // do not turn it into a valid command via targetId fallback.
                return Ok(None);
            }
        }
        let Some(target_id) = params.get("targetId").and_then(Value::as_str) else {
            return Ok(None);
        };
        let mut matching_sessions = self.sessions.iter().filter_map(|(session_id, session)| {
            (session.frontend_id == frontend_id
                && matches!(
                    &session.kind,
                    FrontendSessionKind::Child {
                        parent_session_id,
                        target_id: Some(session_target_id),
                    } if parent_session_id.as_deref() == dispatch_session_id
                        && session_target_id == target_id
                ))
            .then_some(session_id.as_str())
        });
        let Some(session_id) = matching_sessions.next() else {
            return Err(CdpTargetSessionReferenceError {
                code: -32602,
                message: "No session for given target id",
            });
        };
        if matching_sessions.next().is_some() {
            return Err(CdpTargetSessionReferenceError {
                code: -32000,
                message: "Multiple sessions attached, specify id.",
            });
        }
        Ok(Some(session_id.to_owned()))
    }

    fn allocate_internal_command_id(&mut self) -> u64 {
        loop {
            self.next_internal_command_id = self.next_internal_command_id.wrapping_add(1);
            if self.next_internal_command_id != 0
                && !self
                    .pending_commands
                    .contains_key(&self.next_internal_command_id)
            {
                return self.next_internal_command_id;
            }
        }
    }

    pub(super) fn register_browser_frontend(
        &mut self,
        frontend_id: u64,
        session_id: String,
        sink: CdpSocketSink,
    ) -> Result<()> {
        self.register_session_frontend(
            frontend_id,
            CdpSessionFrontendKind::Browser,
            None,
            session_id.clone(),
            sink,
        )?;
        self.private_sessions.remove(&session_id);
        Ok(())
    }

    pub(super) fn register_page_frontend(
        &mut self,
        frontend_id: u64,
        target_id: String,
        session_id: String,
        sink: CdpSocketSink,
    ) -> Result<()> {
        self.register_session_frontend(
            frontend_id,
            CdpSessionFrontendKind::Page,
            Some(target_id),
            session_id,
            sink,
        )
    }

    fn register_session_frontend(
        &mut self,
        frontend_id: u64,
        kind: CdpSessionFrontendKind,
        target_id: Option<String>,
        session_id: String,
        sink: CdpSocketSink,
    ) -> Result<()> {
        if self.frontends.contains_key(&frontend_id) {
            bail!("CDP frontend id is already registered");
        }
        if self.sessions.contains_key(&session_id) {
            bail!("CDP frontend session is already registered");
        }
        self.sessions.insert(
            session_id.clone(),
            FrontendSessionRoute {
                frontend_id,
                kind: FrontendSessionKind::Base,
            },
        );
        self.frontends.insert(
            frontend_id,
            CdpSessionFrontendRoute {
                kind,
                target_id,
                base_session_id: session_id,
                sink,
            },
        );
        Ok(())
    }

    pub(super) fn unregister_browser_frontend(&mut self, frontend_id: u64) -> Option<String> {
        self.unregister_session_frontend(frontend_id, CdpSessionFrontendKind::Browser)
            .map(|route| route.base_session_id)
    }

    pub(super) fn unregister_page_frontend(&mut self, frontend_id: u64) -> Option<String> {
        self.unregister_session_frontend(frontend_id, CdpSessionFrontendKind::Page)
            .map(|route| route.base_session_id)
    }

    fn unregister_session_frontend(
        &mut self,
        frontend_id: u64,
        expected_kind: CdpSessionFrontendKind,
    ) -> Option<CdpSessionFrontendRoute> {
        if self.frontends.get(&frontend_id)?.kind != expected_kind {
            return None;
        }
        let route = self.frontends.remove(&frontend_id)?;
        self.sessions
            .retain(|_, session| session.frontend_id != frontend_id);
        self.remove_pending_commands_for_frontend(frontend_id);
        route.sink.close_after_flush();
        Some(route)
    }

    fn remove_pending_commands_for_frontend(&mut self, frontend_id: u64) {
        self.pending_commands
            .retain(|_, pending| pending.frontend.frontend_id != frontend_id);
    }

    pub(super) fn register_private_session(&mut self, session_id: String) {
        self.private_sessions.insert(session_id);
    }

    pub(super) fn unregister_page_frontends_for_target(&mut self, target_id: &str) {
        let frontend_ids = self
            .frontends
            .iter()
            .filter_map(|(frontend_id, route)| {
                (route.kind == CdpSessionFrontendKind::Page
                    && route.target_id.as_deref() == Some(target_id))
                .then_some(*frontend_id)
            })
            .collect::<Vec<_>>();
        for frontend_id in frontend_ids {
            self.unregister_page_frontend(frontend_id);
        }
    }

    pub(super) fn frontend_by_id(&self, frontend_id: u64) -> Option<CdpRoutedFrontend> {
        self.frontends
            .get(&frontend_id)
            .map(|route| CdpRoutedFrontend {
                frontend_id,
                sink: route.sink.clone(),
            })
    }

    pub(super) fn route_message(
        &mut self,
        mut message: Value,
        wire_session_id: Option<&str>,
    ) -> Option<(CdpRoutedFrontend, Value)> {
        if let Some(internal_command_id) = message.get("id").and_then(Value::as_u64)
            && let Some(pending) = self.pending_commands.remove(&internal_command_id)
        {
            message["id"] = json!(pending.client_command_id);
            let CdpCommandFrontend {
                frontend_id,
                dispatch_session_id,
                client_session_id,
            } = pending.frontend;
            let sink = self.frontends.get(&frontend_id)?.sink.clone();
            if pending.method == "Target.attachToTarget"
                && message.get("error").is_none()
                && let Some(child_session_id) = message
                    .pointer("/result/sessionId")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            {
                self.register_child_session(
                    frontend_id,
                    dispatch_session_id.as_deref(),
                    &child_session_id,
                    pending.attach_target_id.as_deref(),
                );
            }
            set_top_level_session_id(&mut message, client_session_id.as_deref());
            return Some((CdpRoutedFrontend { frontend_id, sink }, message));
        }
        if message.get("id").and_then(Value::as_u64).is_some() {
            return None;
        }

        let method = message
            .get("method")
            .and_then(Value::as_str)
            .map(str::to_owned);
        let encoded_session_id = message
            .get("sessionId")
            .and_then(Value::as_str)
            .map(str::to_owned);
        debug_assert_eq!(
            encoded_session_id.as_deref(),
            wire_session_id,
            "the frozen delivery route must match the encoded wire session"
        );
        let parent_session_id = wire_session_id.map(str::to_owned);
        let target_event_session_id = message
            .pointer("/params/sessionId")
            .and_then(Value::as_str)
            .map(str::to_owned);
        let target_event_target_id = message
            .pointer("/params/targetInfo/targetId")
            .and_then(Value::as_str)
            .map(str::to_owned);
        if method.as_deref() == Some("Target.attachedToTarget")
            && let Some(child_session_id) = target_event_session_id.as_deref()
        {
            if self.private_sessions.contains(child_session_id) {
                return None;
            }
            if let Some(parent_session_id) = parent_session_id.as_deref()
                && let Some(parent) = self.sessions.get(parent_session_id)
            {
                self.register_child_session(
                    parent.frontend_id,
                    Some(parent_session_id),
                    child_session_id,
                    target_event_target_id.as_deref(),
                );
            }
        }

        if let Some(session_id) = parent_session_id.as_deref()
            && self.private_sessions.contains(session_id)
        {
            return None;
        }

        if let Some(session_id) = parent_session_id.as_deref()
            && let Some(session) = self.sessions.get(session_id).cloned()
            && let Some(route) = self.frontends.get(&session.frontend_id)
        {
            let sink = route.sink.clone();
            let frontend_id = session.frontend_id;
            if matches!(session.kind, FrontendSessionKind::Base) {
                remove_top_level_session_id(&mut message);
            }
            if method.as_deref() == Some("Target.detachedFromTarget")
                && let Some(detached_session_id) = target_event_session_id.as_deref()
            {
                self.remove_child_session_cascade(detached_session_id);
            }
            return Some((CdpRoutedFrontend { frontend_id, sink }, message));
        }
        if parent_session_id.is_some() {
            return None;
        }

        if method.as_deref() == Some("Target.detachedFromTarget")
            && let Some(session_id) = target_event_session_id.as_deref()
        {
            if self.private_sessions.remove(session_id) {
                return None;
            }
            if let Some(session) = self.sessions.get(session_id).cloned() {
                match session.kind {
                    FrontendSessionKind::Base => {
                        // Base sessions are private transport adapters and
                        // never surface on their frontend's wire protocol.
                        self.remove_session_descendants(session_id);
                        return None;
                    }
                    FrontendSessionKind::Child {
                        parent_session_id, ..
                    } => {
                        let route = self.frontends.get(&session.frontend_id)?;
                        let client_parent_session_id = parent_session_id
                            .as_deref()
                            .filter(|parent| route.base_session_id.as_str() != *parent);
                        let sink = route.sink.clone();
                        set_top_level_session_id(&mut message, client_parent_session_id);
                        self.remove_child_session_cascade(session_id);
                        return Some((
                            CdpRoutedFrontend {
                                frontend_id: session.frontend_id,
                                sink,
                            },
                            message,
                        ));
                    }
                }
            }
            self.remove_child_session_cascade(session_id);
        }

        None
    }

    fn register_child_session(
        &mut self,
        frontend_id: u64,
        parent_session_id: Option<&str>,
        child_session_id: &str,
        target_id: Option<&str>,
    ) {
        let frontend = self.frontends.get(&frontend_id);
        if frontend.is_none() {
            return;
        }
        if let Some(parent_session_id) = parent_session_id {
            if self
                .sessions
                .get(parent_session_id)
                .is_none_or(|parent| parent.frontend_id != frontend_id)
            {
                return;
            }
        } else if frontend.is_none_or(|route| route.kind != CdpSessionFrontendKind::Browser) {
            return;
        }
        if let Some(existing) = self.sessions.get(child_session_id)
            && (existing.frontend_id != frontend_id
                || matches!(existing.kind, FrontendSessionKind::Base))
        {
            return;
        }
        self.sessions.insert(
            child_session_id.to_owned(),
            FrontendSessionRoute {
                frontend_id,
                kind: FrontendSessionKind::Child {
                    parent_session_id: parent_session_id.map(str::to_owned),
                    target_id: target_id.map(str::to_owned),
                },
            },
        );
    }

    fn remove_child_session_cascade(&mut self, session_id: &str) {
        if self
            .sessions
            .get(session_id)
            .is_none_or(|session| matches!(session.kind, FrontendSessionKind::Base))
        {
            return;
        }
        self.remove_session_descendants(session_id);
        self.sessions.remove(session_id);
    }

    fn remove_session_descendants(&mut self, session_id: &str) {
        let mut pending = vec![session_id.to_owned()];
        let mut descendants = Vec::new();
        let mut visited = HashSet::new();
        visited.insert(session_id.to_owned());
        while let Some(parent_session_id) = pending.pop() {
            for child_session_id in self
                .sessions
                .iter()
                .filter_map(|(child_session_id, route)| match &route.kind {
                    FrontendSessionKind::Child {
                        parent_session_id: parent,
                        ..
                    } if parent.as_deref() == Some(parent_session_id.as_str()) => {
                        Some(child_session_id.clone())
                    }
                    FrontendSessionKind::Base | FrontendSessionKind::Child { .. } => None,
                })
                .collect::<Vec<_>>()
            {
                if visited.insert(child_session_id.clone()) {
                    pending.push(child_session_id.clone());
                    descendants.push(child_session_id);
                }
            }
        }
        for descendant in descendants {
            self.sessions.remove(&descendant);
        }
    }
}

fn remove_top_level_session_id(message: &mut Value) {
    if let Some(object) = message.as_object_mut() {
        object.remove("sessionId");
    }
}

fn set_top_level_session_id(message: &mut Value, session_id: Option<&str>) {
    if let Some(session_id) = session_id {
        message["sessionId"] = json!(session_id);
    } else {
        remove_top_level_session_id(message);
    }
}

fn cdp_error_response(id: Option<u64>, code: i32, message: &str) -> Value {
    json!({
        "id": id.map(Value::from).unwrap_or(Value::Null),
        "error": {
            "code": code,
            "message": message,
        },
    })
}

#[cfg(test)]
mod tests {
    use crate::{cdp_frontend_router::CdpFrontendRouter, cdp_scheduler::ProtocolOutputSequence};

    use super::*;

    fn test_sink() -> CdpSocketSink {
        CdpSocketSink::for_test()
    }

    fn parsed_command(raw: impl Into<String>) -> ParsedCdpCommand {
        ParsedCdpCommand::parse_str(raw).expect("test command must be valid CDP JSON")
    }

    fn expect_prepared_command(
        prepared: Option<CdpPreparedFrontendCommand>,
        label: &str,
    ) -> ParsedCdpCommand {
        match prepared.unwrap_or_else(|| panic!("missing prepared {label} command")) {
            CdpPreparedFrontendCommand::Command(command) => command,
            CdpPreparedFrontendCommand::ImmediateResponse { .. } => {
                panic!("{label} command unexpectedly produced an immediate response")
            }
        }
    }

    #[test]
    fn browser_and_page_client_command_ids_are_isolated_and_restored() {
        let mut routing = CdpFrontendRoutingState::default();
        routing
            .register_browser_frontend(5, "SID-browser".to_owned(), test_sink())
            .expect("register browser frontend");
        routing
            .register_page_frontend(10, "TID-1".to_owned(), "SID-page".to_owned(), test_sink())
            .expect("register page frontend");

        let browser_command = expect_prepared_command(
            routing.prepare_command(
                5,
                parsed_command(json!({ "id": 7, "method": "Browser.getVersion" }).to_string()),
            ),
            "browser",
        );
        let page_command = expect_prepared_command(
            routing.prepare_command(
                10,
                parsed_command(json!({ "id": 7, "method": "Page.getFrameTree" }).to_string()),
            ),
            "page",
        );
        let browser_internal_id = serde_json::from_str::<Value>(browser_command.json())
            .expect("browser command JSON")["id"]
            .as_u64()
            .expect("browser internal id");
        assert_eq!(
            serde_json::from_str::<Value>(browser_command.json()).expect("browser command JSON")["sessionId"],
            json!("SID-browser")
        );
        let page_internal_id = serde_json::from_str::<Value>(page_command.json())
            .expect("page command JSON")["id"]
            .as_u64()
            .expect("page internal id");
        assert_ne!(browser_internal_id, page_internal_id);

        let (browser_frontend, browser_response) = routing
            .route_message(
                json!({
                    "id": browser_internal_id,
                    "result": {},
                    "sessionId": "SID-browser",
                }),
                Some("SID-browser"),
            )
            .expect("route browser response");
        assert_eq!(browser_frontend.frontend_id, 5);
        assert_eq!(browser_response["id"], json!(7));
        assert!(browser_response.get("sessionId").is_none());

        let (page_frontend, page_response) = routing
            .route_message(
                json!({
                    "id": page_internal_id,
                    "result": {},
                    "sessionId": "SID-page",
                }),
                Some("SID-page"),
            )
            .expect("route page response");
        assert_eq!(page_frontend.frontend_id, 10);
        assert_eq!(page_response["id"], json!(7));
        assert!(page_response.get("sessionId").is_none());
    }

    #[test]
    fn browser_frontends_with_the_same_client_command_id_route_independently() {
        let mut routing = CdpFrontendRoutingState::default();
        routing
            .register_browser_frontend(5, "SID-browser-1".to_owned(), test_sink())
            .expect("register first browser frontend");
        routing
            .register_browser_frontend(6, "SID-browser-2".to_owned(), test_sink())
            .expect("register second browser frontend");

        let first = expect_prepared_command(
            routing.prepare_command(
                5,
                parsed_command(json!({ "id": 7, "method": "Browser.getVersion" }).to_string()),
            ),
            "first browser",
        );
        let second = expect_prepared_command(
            routing.prepare_command(
                6,
                parsed_command(json!({ "id": 7, "method": "Browser.getVersion" }).to_string()),
            ),
            "second browser",
        );
        let first = serde_json::from_str::<Value>(first.json()).expect("first command JSON");
        let second = serde_json::from_str::<Value>(second.json()).expect("second command JSON");
        assert_ne!(first["id"], second["id"]);
        assert_eq!(first["sessionId"], json!("SID-browser-1"));
        assert_eq!(second["sessionId"], json!("SID-browser-2"));

        let (frontend, response) = routing
            .route_message(
                json!({
                    "id": second["id"],
                    "result": { "product": "second" },
                    "sessionId": "SID-browser-2",
                }),
                Some("SID-browser-2"),
            )
            .expect("route second response");
        assert_eq!(frontend.frontend_id(), 6);
        assert_eq!(response["id"], json!(7));
        assert_eq!(response["result"]["product"], json!("second"));
        assert!(response.get("sessionId").is_none());

        let (frontend, response) = routing
            .route_message(
                json!({
                    "id": first["id"],
                    "result": { "product": "first" },
                    "sessionId": "SID-browser-1",
                }),
                Some("SID-browser-1"),
            )
            .expect("route first response");
        assert_eq!(frontend.frontend_id(), 5);
        assert_eq!(response["id"], json!(7));
        assert_eq!(response["result"]["product"], json!("first"));
        assert!(response.get("sessionId").is_none());
    }

    #[test]
    fn browser_base_session_events_are_private_and_frontend_scoped() {
        let mut routing = CdpFrontendRoutingState::default();
        routing
            .register_browser_frontend(5, "SID-browser-1".to_owned(), test_sink())
            .expect("register first browser frontend");
        routing
            .register_browser_frontend(6, "SID-browser-2".to_owned(), test_sink())
            .expect("register second browser frontend");

        let (frontend, event) = routing
            .route_message(
                json!({
                    "method": "Target.targetCreated",
                    "sessionId": "SID-browser-1",
                    "params": { "targetInfo": { "targetId": "TID-1" } },
                }),
                Some("SID-browser-1"),
            )
            .expect("route first browser event");
        assert_eq!(frontend.frontend_id(), 5);
        assert!(event.get("sessionId").is_none());

        assert!(
            routing
                .route_message(
                    json!({
                        "method": "Target.targetCreated",
                        "params": { "targetInfo": { "targetId": "TID-root" } },
                    }),
                    None,
                )
                .is_none(),
            "unowned root event must not be assigned to an arbitrary browser frontend"
        );
    }

    #[test]
    fn root_detach_with_a_known_child_routes_to_its_exact_frontend() {
        let mut routing = CdpFrontendRoutingState::default();
        routing
            .register_browser_frontend(5, "SID-browser-1".to_owned(), test_sink())
            .expect("register first browser frontend");
        routing
            .register_browser_frontend(6, "SID-browser-2".to_owned(), test_sink())
            .expect("register second browser frontend");
        routing.register_child_session(5, Some("SID-browser-1"), "SID-child-1", Some("TID-1"));
        routing.register_child_session(6, Some("SID-browser-2"), "SID-child-2", Some("TID-1"));

        let (frontend, event) = routing
            .route_message(
                json!({
                    "method": "Target.detachedFromTarget",
                    "params": {
                        "targetId": "TID-1",
                        "sessionId": "SID-child-1",
                    },
                }),
                None,
            )
            .expect("route owner-qualified root detach");
        assert_eq!(frontend.frontend_id(), 5);
        assert!(event.get("sessionId").is_none());

        assert!(matches!(
            routing.prepare_command(
                5,
                parsed_command(
                    json!({
                        "id": 1,
                        "method": "Runtime.evaluate",
                        "sessionId": "SID-child-1",
                    })
                    .to_string(),
                ),
            ),
            Some(CdpPreparedFrontendCommand::ImmediateResponse { .. })
        ));
        assert!(matches!(
            routing.prepare_command(
                6,
                parsed_command(
                    json!({
                        "id": 1,
                        "method": "Runtime.evaluate",
                        "sessionId": "SID-child-2",
                    })
                    .to_string(),
                ),
            ),
            Some(CdpPreparedFrontendCommand::Command(_))
        ));
    }

    #[test]
    fn root_detach_restores_visible_parent_for_nested_child() {
        let mut routing = CdpFrontendRoutingState::default();
        routing
            .register_browser_frontend(5, "SID-browser".to_owned(), test_sink())
            .expect("register browser frontend");
        routing.register_child_session(5, Some("SID-browser"), "SID-child", Some("TID-child"));
        routing.register_child_session(
            5,
            Some("SID-child"),
            "SID-grandchild",
            Some("TID-grandchild"),
        );

        let (_, event) = routing
            .route_message(
                json!({
                    "method": "Target.detachedFromTarget",
                    "params": {
                        "targetId": "TID-grandchild",
                        "sessionId": "SID-grandchild",
                    },
                }),
                None,
            )
            .expect("route nested root detach");
        assert_eq!(event["sessionId"], json!("SID-child"));
    }

    #[test]
    fn unregistering_one_browser_drops_only_its_pending_commands() {
        let mut routing = CdpFrontendRoutingState::default();
        routing
            .register_browser_frontend(5, "SID-browser-1".to_owned(), test_sink())
            .expect("register first browser frontend");
        routing
            .register_browser_frontend(6, "SID-browser-2".to_owned(), test_sink())
            .expect("register second browser frontend");
        let first = expect_prepared_command(
            routing.prepare_command(
                5,
                parsed_command(json!({ "id": 1, "method": "Browser.getVersion" }).to_string()),
            ),
            "first browser",
        );
        let second = expect_prepared_command(
            routing.prepare_command(
                6,
                parsed_command(json!({ "id": 1, "method": "Browser.getVersion" }).to_string()),
            ),
            "second browser",
        );
        let first_internal_id = serde_json::from_str::<Value>(first.json())
            .expect("first command JSON")["id"]
            .as_u64()
            .expect("first internal id");
        let second_internal_id = serde_json::from_str::<Value>(second.json())
            .expect("second command JSON")["id"]
            .as_u64()
            .expect("second internal id");

        assert_eq!(
            routing.unregister_browser_frontend(5).as_deref(),
            Some("SID-browser-1")
        );
        assert!(
            routing
                .route_message(
                    json!({
                        "id": first_internal_id,
                        "result": {},
                        "sessionId": "SID-browser-1",
                    }),
                    Some("SID-browser-1"),
                )
                .is_none()
        );
        let (frontend, response) = routing
            .route_message(
                json!({
                    "id": second_internal_id,
                    "result": {},
                    "sessionId": "SID-browser-2",
                }),
                Some("SID-browser-2"),
            )
            .expect("second browser response remains routable");
        assert_eq!(frontend.frontend_id(), 6);
        assert_eq!(response["id"], json!(1));
    }

    #[test]
    fn browser_child_session_is_preserved_on_browser_frontend() {
        let mut routing = CdpFrontendRoutingState::default();
        routing
            .register_browser_frontend(5, "SID-browser".to_owned(), test_sink())
            .expect("register browser frontend");
        routing.register_child_session(
            5,
            Some("SID-browser"),
            "SID-client-child",
            Some("TID-child"),
        );

        let command = expect_prepared_command(
            routing.prepare_command(
                5,
                parsed_command(
                    json!({
                        "id": 9,
                        "method": "Runtime.evaluate",
                        "sessionId": "SID-client-child",
                    })
                    .to_string(),
                ),
            ),
            "child-session",
        );
        let command_json =
            serde_json::from_str::<Value>(command.json()).expect("prepared command JSON");
        assert_eq!(command_json["sessionId"], json!("SID-client-child"));
        let internal_id = command_json["id"].as_u64().expect("internal id");

        let (_, response) = routing
            .route_message(
                json!({
                    "id": internal_id,
                    "result": {},
                    "sessionId": "SID-client-child",
                }),
                Some("SID-client-child"),
            )
            .expect("route child response");
        assert_eq!(response["sessionId"], json!("SID-client-child"));
    }

    #[test]
    fn legacy_target_session_references_cannot_cross_browser_frontends() {
        let mut routing = CdpFrontendRoutingState::default();
        routing
            .register_browser_frontend(5, "SID-browser-1".to_owned(), test_sink())
            .expect("register first browser frontend");
        routing
            .register_browser_frontend(6, "SID-browser-2".to_owned(), test_sink())
            .expect("register second browser frontend");
        routing.register_child_session(5, Some("SID-browser-1"), "SID-child-1", Some("TID-shared"));
        routing.register_child_session(6, Some("SID-browser-2"), "SID-child-2", Some("TID-shared"));

        for method in ["Target.detachFromTarget", "Target.sendMessageToTarget"] {
            let Some(CdpPreparedFrontendCommand::ImmediateResponse {
                frontend_id,
                message,
            }) = routing.prepare_command(
                6,
                parsed_command(
                    json!({
                        "id": 11,
                        "method": method,
                        "params": {
                            "sessionId": "SID-child-1",
                            "message": "{}",
                        },
                    })
                    .to_string(),
                ),
            )
            else {
                panic!("foreign {method} session reference was not rejected");
            };
            assert_eq!(frontend_id, 6);
            assert_eq!(message["id"], json!(11));
            assert_eq!(message["error"]["code"], json!(-32602));
        }

        let command = expect_prepared_command(
            routing.prepare_command(
                6,
                parsed_command(
                    json!({
                        "id": 12,
                        "method": "Target.detachFromTarget",
                        "params": { "targetId": "TID-shared" },
                    })
                    .to_string(),
                ),
            ),
            "owned target-id detach",
        );
        let command = serde_json::from_str::<Value>(command.json()).expect("detach command JSON");
        assert_eq!(command["sessionId"], json!("SID-browser-2"));
        assert_eq!(command["params"]["sessionId"], json!("SID-child-2"));
        assert_eq!(command["params"]["targetId"], json!("TID-shared"));
    }

    #[test]
    fn legacy_target_id_reference_requires_one_direct_child_session() {
        let mut routing = CdpFrontendRoutingState::default();
        routing
            .register_browser_frontend(5, "SID-browser".to_owned(), test_sink())
            .expect("register browser frontend");
        routing.register_child_session(5, Some("SID-browser"), "SID-child-1", Some("TID-shared"));
        routing.register_child_session(5, Some("SID-browser"), "SID-child-2", Some("TID-shared"));
        routing.register_child_session(
            5,
            Some("SID-child-1"),
            "SID-grandchild",
            Some("TID-grandchild"),
        );

        let Some(CdpPreparedFrontendCommand::ImmediateResponse { message, .. }) = routing
            .prepare_command(
                5,
                parsed_command(
                    json!({
                        "id": 20,
                        "method": "Target.detachFromTarget",
                        "params": { "targetId": "TID-shared" },
                    })
                    .to_string(),
                ),
            )
        else {
            panic!("ambiguous target-id detach was not rejected");
        };
        assert_eq!(message["error"]["code"], json!(-32000));

        let Some(CdpPreparedFrontendCommand::ImmediateResponse { message, .. }) = routing
            .prepare_command(
                5,
                parsed_command(
                    json!({
                        "id": 21,
                        "method": "Target.detachFromTarget",
                        "params": { "sessionId": "SID-grandchild" },
                    })
                    .to_string(),
                ),
            )
        else {
            panic!("non-direct child session was accepted by the base Target handler");
        };
        assert_eq!(message["error"]["code"], json!(-32602));

        let command = expect_prepared_command(
            routing.prepare_command(
                5,
                parsed_command(
                    json!({
                        "id": 22,
                        "method": "Target.detachFromTarget",
                        "sessionId": "SID-child-1",
                        "params": { "sessionId": "SID-grandchild" },
                    })
                    .to_string(),
                ),
            ),
            "direct grandchild detach",
        );
        let command = serde_json::from_str::<Value>(command.json()).expect("detach command JSON");
        assert_eq!(command["sessionId"], json!("SID-child-1"));
        assert_eq!(command["params"]["sessionId"], json!("SID-grandchild"));
    }

    #[test]
    fn attached_event_registers_child_before_attach_response() {
        let mut routing = CdpFrontendRoutingState::default();
        routing
            .register_browser_frontend(5, "SID-browser".to_owned(), test_sink())
            .expect("register browser frontend");
        let attach = expect_prepared_command(
            routing.prepare_command(
                5,
                parsed_command(
                    json!({
                        "id": 30,
                        "method": "Target.attachToTarget",
                        "params": { "targetId": "TID-child", "flatten": true },
                    })
                    .to_string(),
                ),
            ),
            "attach",
        );
        let attach_internal_id = serde_json::from_str::<Value>(attach.json())
            .expect("attach command JSON")["id"]
            .as_u64()
            .expect("attach internal id");

        let (frontend, event) = routing
            .route_message(
                json!({
                    "method": "Target.attachedToTarget",
                    "sessionId": "SID-browser",
                    "params": {
                        "sessionId": "SID-child",
                        "targetInfo": { "targetId": "TID-child", "type": "page" },
                        "waitingForDebugger": false,
                    },
                }),
                Some("SID-browser"),
            )
            .expect("route attached event");
        assert_eq!(frontend.frontend_id(), 5);
        assert!(event.get("sessionId").is_none());
        assert_eq!(event["params"]["sessionId"], json!("SID-child"));

        let child_command = expect_prepared_command(
            routing.prepare_command(
                5,
                parsed_command(
                    json!({
                        "id": 31,
                        "method": "Runtime.evaluate",
                        "sessionId": "SID-child",
                        "params": { "expression": "1" },
                    })
                    .to_string(),
                ),
            ),
            "event-registered child",
        );
        assert_eq!(
            serde_json::from_str::<Value>(child_command.json()).expect("child command JSON")["sessionId"],
            json!("SID-child")
        );

        let (_, response) = routing
            .route_message(
                json!({
                    "id": attach_internal_id,
                    "result": { "sessionId": "SID-child" },
                    "sessionId": "SID-browser",
                }),
                Some("SID-browser"),
            )
            .expect("route attach response");
        assert_eq!(response["id"], json!(30));
        assert!(response.get("sessionId").is_none());
    }

    #[test]
    fn browser_frontend_hides_base_session_on_wire() {
        let mut routing = CdpFrontendRoutingState::default();
        routing
            .register_browser_frontend(5, "SID-browser".to_owned(), test_sink())
            .expect("register browser frontend");

        let command = expect_prepared_command(
            routing.prepare_command(
                5,
                parsed_command(json!({ "id": 9, "method": "Page.getFrameTree" }).to_string()),
            ),
            "browser",
        );
        let command_json =
            serde_json::from_str::<Value>(command.json()).expect("prepared command JSON");
        assert_eq!(command_json["sessionId"], json!("SID-browser"));
        let internal_id = command_json["id"].as_u64().expect("internal id");

        let (_, response) = routing
            .route_message(
                json!({
                    "id": internal_id,
                    "result": {},
                    "sessionId": "SID-browser",
                }),
                Some("SID-browser"),
            )
            .expect("route browser response");
        assert_eq!(response["id"], json!(9));
        assert!(response.get("sessionId").is_none());
    }

    #[test]
    fn frontend_route_rewrite_preserves_unknown_command_fields() {
        let mut routing = CdpFrontendRoutingState::default();
        routing
            .register_page_frontend(10, "TID-1".to_owned(), "SID-page".to_owned(), test_sink())
            .expect("register page frontend");

        let command = expect_prepared_command(
            routing.prepare_command(
                10,
                parsed_command(
                    r#"{"id":9,"method":"Runtime.getIsolateId","params":null,"futureExtension":{"enabled":true}}"#,
                ),
            ),
            "extension-field",
        );
        let command_json =
            serde_json::from_str::<Value>(command.json()).expect("prepared command JSON");
        assert_ne!(command_json["id"], json!(9));
        assert_eq!(command_json["sessionId"], json!("SID-page"));
        assert!(command_json.get("params").is_none());
        assert_eq!(command_json["futureExtension"], json!({"enabled": true}));
    }

    #[test]
    fn malformed_command_keeps_its_originating_frontend() {
        let mut routing = CdpFrontendRoutingState::default();
        routing
            .register_browser_frontend(5, "SID-browser".to_owned(), test_sink())
            .expect("register browser frontend");
        routing
            .register_page_frontend(10, "TID-1".to_owned(), "SID-page".to_owned(), test_sink())
            .expect("register page frontend");

        assert!(matches!(
            routing.prepare_command_str(5, "{".to_owned()),
            Some(CdpPreparedFrontendCommand::ImmediateResponse { frontend_id: 5, .. })
        ));
        assert!(matches!(
            routing.prepare_command_str(10, "{".to_owned()),
            Some(CdpPreparedFrontendCommand::ImmediateResponse {
                frontend_id: 10,
                ..
            })
        ));
    }

    #[test]
    fn structurally_invalid_command_preserves_frontend_id_in_invalid_request() {
        let mut routing = CdpFrontendRoutingState::default();
        routing
            .register_browser_frontend(5, "SID-browser".to_owned(), test_sink())
            .expect("register browser frontend");

        let Some(CdpPreparedFrontendCommand::ImmediateResponse {
            frontend_id,
            message,
        }) = routing.prepare_command_str(
            5,
            r#"{"id":42,"method":"Runtime.evaluate","params":[]}"#.to_owned(),
        )
        else {
            panic!("invalid command must produce an immediate response")
        };

        assert_eq!(frontend_id, 5);
        assert_eq!(
            message,
            json!({
                "id": 42,
                "error": {"code": -32600, "message": "Invalid Request"}
            })
        );
    }

    #[test]
    fn private_page_session_detach_does_not_fall_back_to_browser_frontend() {
        let mut routing = CdpFrontendRoutingState::default();
        routing
            .register_browser_frontend(5, "SID-browser".to_owned(), test_sink())
            .expect("register browser frontend");
        routing
            .register_page_frontend(10, "TID-1".to_owned(), "SID-page".to_owned(), test_sink())
            .expect("register page frontend");
        routing.register_child_session(
            5,
            Some("SID-browser"),
            "SID-browser-child",
            Some("TID-root"),
        );

        assert!(
            routing
                .route_message(
                    json!({
                        "method": "Target.detachedFromTarget",
                        "params": {
                            "targetId": "TID-1",
                            "sessionId": "SID-page",
                            "reason": "Render process gone.",
                        },
                    }),
                    None
                )
                .is_none()
        );

        let (frontend, message) = routing
            .route_message(
                json!({
                    "method": "Target.detachedFromTarget",
                    "sessionId": "SID-browser",
                    "params": {
                        "targetId": "TID-root",
                        "sessionId": "SID-browser-child",
                    },
                }),
                Some("SID-browser"),
            )
            .expect("route browser-owned target detach");
        assert_eq!(frontend.frontend_id, 5);
        assert_eq!(message["params"]["sessionId"], json!("SID-browser-child"));
    }

    #[test]
    fn page_child_sessions_are_scoped_to_their_frontend_and_removed_on_detach() {
        let mut routing = CdpFrontendRoutingState::default();
        routing
            .register_page_frontend(10, "TID-1".to_owned(), "SID-page-1".to_owned(), test_sink())
            .expect("register first page frontend");
        routing
            .register_page_frontend(20, "TID-2".to_owned(), "SID-page-2".to_owned(), test_sink())
            .expect("register second page frontend");

        let attach = expect_prepared_command(
            routing.prepare_command(
                10,
                parsed_command(
                    json!({
                        "id": 1,
                        "method": "Target.attachToTarget",
                        "params": { "targetId": "TID-2", "flatten": true }
                    })
                    .to_string(),
                ),
            ),
            "attach",
        );
        let attach_internal_id = serde_json::from_str::<Value>(attach.json())
            .expect("prepared attach JSON")["id"]
            .as_u64()
            .expect("attach internal id");
        routing
            .route_message(
                json!({
                    "id": attach_internal_id,
                    "result": { "sessionId": "SID-child" },
                    "sessionId": "SID-page-1",
                }),
                Some("SID-page-1"),
            )
            .expect("route attach response");

        let child = expect_prepared_command(
            routing.prepare_command(
                10,
                parsed_command(
                    json!({
                        "id": 2,
                        "method": "Runtime.evaluate",
                        "sessionId": "SID-child",
                        "params": { "expression": "1" }
                    })
                    .to_string(),
                ),
            ),
            "owned child",
        );
        assert_eq!(
            serde_json::from_str::<Value>(child.json()).expect("prepared child JSON")["sessionId"],
            json!("SID-child")
        );

        let Some(CdpPreparedFrontendCommand::ImmediateResponse { message, .. }) = routing
            .prepare_command(
                20,
                parsed_command(
                    json!({
                        "id": 3,
                        "method": "Runtime.evaluate",
                        "sessionId": "SID-child",
                        "params": { "expression": "2" }
                    })
                    .to_string(),
                ),
            )
        else {
            panic!("foreign child session command was not rejected");
        };
        assert_eq!(message["error"]["code"], json!(-32001));

        routing
            .route_message(
                json!({
                    "method": "Target.detachedFromTarget",
                    "sessionId": "SID-page-1",
                    "params": {
                        "targetId": "TID-2",
                        "sessionId": "SID-child",
                    },
                }),
                Some("SID-page-1"),
            )
            .expect("route child detach");
        assert!(matches!(
            routing.prepare_command(
                10,
                parsed_command(
                    json!({
                        "id": 4,
                        "method": "Runtime.evaluate",
                        "sessionId": "SID-child",
                        "params": { "expression": "3" }
                    })
                    .to_string(),
                ),
            ),
            Some(CdpPreparedFrontendCommand::ImmediateResponse { .. })
        ));
    }

    #[test]
    fn stalled_browser_writer_does_not_block_page_frontend_enqueue() {
        let router = CdpFrontendRouter::new();
        let (root_sink, mut root_writer) = CdpSocketSink::with_stalled_writer_for_test(2);
        let (page_sink, mut page_writer) = CdpSocketSink::with_stalled_writer_for_test(2);
        router
            .register_browser_frontend(5, "SID-browser".to_owned(), root_sink)
            .expect("register browser frontend");
        router
            .register_page_frontend(10, "TID-page".to_owned(), "SID-page".to_owned(), page_sink)
            .expect("register page frontend");

        assert!(
            router.enqueue_protocol_output_sequence(ProtocolOutputSequence::from_messages(vec![
                json!({
                    "method": "Target.targetCreated",
                    "sessionId": "SID-browser",
                    "params": { "targetInfo": { "targetId": "TID-root" } },
                }),
                json!({
                    "method": "Runtime.consoleAPICalled",
                    "params": { "type": "log" },
                    "sessionId": "SID-page",
                }),
            ]))
        );

        assert_eq!(
            root_writer.take_message()["method"],
            json!("Target.targetCreated")
        );
        let page_message = page_writer.take_message();
        assert_eq!(page_message["method"], json!("Runtime.consoleAPICalled"));
        assert!(page_message.get("sessionId").is_none());
        assert!(root_writer.is_open());
        assert!(page_writer.is_open());
    }

    #[test]
    fn browser_frontends_register_with_independent_base_sessions() {
        let mut routing = CdpFrontendRoutingState::default();
        routing
            .register_browser_frontend(5, "SID-browser-1".to_owned(), test_sink())
            .expect("register first browser frontend");
        routing
            .register_browser_frontend(6, "SID-browser-2".to_owned(), test_sink())
            .expect("register second browser frontend");
        assert!(routing.frontend_by_id(5).is_some());
        assert!(routing.frontend_by_id(6).is_some());
        assert_eq!(
            routing.unregister_browser_frontend(5).as_deref(),
            Some("SID-browser-1")
        );
        assert!(routing.frontend_by_id(5).is_none());
        assert!(routing.frontend_by_id(6).is_some());
    }

    #[test]
    fn private_control_session_events_do_not_reach_browser_frontend() {
        let mut routing = CdpFrontendRoutingState::default();
        routing
            .register_browser_frontend(5, "SID-browser".to_owned(), test_sink())
            .expect("register browser frontend");
        routing.register_private_session("SID-control".to_owned());

        assert!(
            routing
                .route_message(
                    json!({
                        "method": "Target.attachedToTarget",
                        "params": {
                            "sessionId": "SID-control",
                            "targetInfo": { "targetId": "browser" },
                        },
                    }),
                    None
                )
                .is_none()
        );
        assert!(
            routing
                .route_message(
                    json!({
                        "method": "Target.targetCreated",
                        "sessionId": "SID-control",
                        "params": { "targetInfo": { "targetId": "TID-private" } },
                    }),
                    Some("SID-control")
                )
                .is_none()
        );
        assert!(
            routing
                .route_message(
                    json!({
                        "method": "Target.detachedFromTarget",
                        "params": { "sessionId": "SID-control" },
                    }),
                    None
                )
                .is_none()
        );
    }

    #[test]
    fn orphaned_responses_and_unknown_session_events_do_not_fall_back_to_browser() {
        let mut routing = CdpFrontendRoutingState::default();
        routing
            .register_browser_frontend(5, "SID-browser".to_owned(), test_sink())
            .expect("register browser frontend");

        assert!(
            routing
                .route_message(json!({ "id": 999, "result": {} }), None)
                .is_none()
        );
        assert!(
            routing
                .route_message(
                    json!({
                        "method": "Runtime.consoleAPICalled",
                        "sessionId": "SID-stale",
                        "params": { "type": "log" },
                    }),
                    Some("SID-stale")
                )
                .is_none()
        );
    }
}
