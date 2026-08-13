use std::collections::{HashMap, HashSet};

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
}

struct CdpSessionFrontendRoute {
    kind: CdpSessionFrontendKind,
    target_id: Option<String>,
    base_session_id: Option<String>,
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
    Base,
    Child { parent_session_id: Option<String> },
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
        let route = self.frontends.get(&frontend_id)?;
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
            route.base_session_id.clone()
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
            },
        );
        Some(CdpPreparedFrontendCommand::Command(command))
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
        sink: CdpSocketSink,
    ) -> Result<(), String> {
        if self
            .frontends
            .values()
            .any(|route| route.kind == CdpSessionFrontendKind::Browser)
        {
            return Err("CDP owner already has a browser frontend".to_owned());
        }
        if self.frontends.contains_key(&frontend_id) {
            return Err("CDP frontend id is already registered".to_owned());
        }
        self.frontends.insert(
            frontend_id,
            CdpSessionFrontendRoute {
                kind: CdpSessionFrontendKind::Browser,
                target_id: None,
                base_session_id: None,
                sink,
            },
        );
        Ok(())
    }

    pub(super) fn register_page_frontend(
        &mut self,
        frontend_id: u64,
        target_id: String,
        session_id: String,
        sink: CdpSocketSink,
    ) -> Result<(), String> {
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
    ) -> Result<(), String> {
        if self.frontends.contains_key(&frontend_id) {
            return Err("CDP frontend id is already registered".to_owned());
        }
        if self.sessions.contains_key(&session_id) {
            return Err("CDP frontend session is already registered".to_owned());
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
                base_session_id: Some(session_id),
                sink,
            },
        );
        Ok(())
    }

    pub(super) fn unregister_browser_frontend(&mut self, frontend_id: u64) -> bool {
        self.unregister_session_frontend(frontend_id, CdpSessionFrontendKind::Browser)
            .is_some()
    }

    pub(super) fn unregister_page_frontend(&mut self, frontend_id: u64) -> Option<String> {
        self.unregister_session_frontend(frontend_id, CdpSessionFrontendKind::Page)
            .and_then(|route| route.base_session_id)
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
        if method.as_deref() == Some("Target.attachedToTarget")
            && let Some(child_session_id) = target_event_session_id.as_deref()
        {
            if self.private_sessions.contains(child_session_id) {
                return None;
            }
            if let Some(parent_session_id) = parent_session_id.as_deref() {
                if let Some(parent) = self.sessions.get(parent_session_id) {
                    self.register_child_session(
                        parent.frontend_id,
                        Some(parent_session_id),
                        child_session_id,
                    );
                }
            } else if let Some(frontend_id) = self.browser_frontend_id() {
                self.register_child_session(frontend_id, None, child_session_id);
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
            if self
                .sessions
                .get(session_id)
                .is_some_and(|session| matches!(session.kind, FrontendSessionKind::Base))
            {
                // Base sessions are private transport adapters and never
                // surface on their frontend's wire protocol.
                self.remove_session_descendants(session_id);
                return None;
            }
            self.remove_child_session_cascade(session_id);
        }

        let frontend_id = self.browser_frontend_id()?;
        let sink = self.frontends.get(&frontend_id)?.sink.clone();
        Some((CdpRoutedFrontend { frontend_id, sink }, message))
    }

    fn browser_frontend_id(&self) -> Option<u64> {
        self.frontends.iter().find_map(|(frontend_id, route)| {
            (route.kind == CdpSessionFrontendKind::Browser).then_some(*frontend_id)
        })
    }

    fn register_child_session(
        &mut self,
        frontend_id: u64,
        parent_session_id: Option<&str>,
        child_session_id: &str,
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
            .register_browser_frontend(5, test_sink())
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
                }),
                None,
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
    fn browser_child_session_is_preserved_on_browser_frontend() {
        let mut routing = CdpFrontendRoutingState::default();
        routing
            .register_browser_frontend(5, test_sink())
            .expect("register browser frontend");
        routing.register_child_session(5, None, "SID-client-child");

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
    fn browser_frontend_preserves_root_wire_shape() {
        let mut routing = CdpFrontendRoutingState::default();
        routing
            .register_browser_frontend(5, test_sink())
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
        assert!(command_json.get("sessionId").is_none());
        let internal_id = command_json["id"].as_u64().expect("internal id");

        let (_, response) = routing
            .route_message(
                json!({
                    "id": internal_id,
                    "result": {},
                }),
                None,
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
            .register_browser_frontend(5, test_sink())
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
            .register_browser_frontend(5, test_sink())
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
            .register_browser_frontend(5, test_sink())
            .expect("register browser frontend");
        routing
            .register_page_frontend(10, "TID-1".to_owned(), "SID-page".to_owned(), test_sink())
            .expect("register page frontend");
        routing.register_child_session(5, None, "SID-browser-child");

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
                    "params": {
                        "targetId": "TID-root",
                        "sessionId": "SID-browser-child",
                    },
                }),
                None,
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
            .register_browser_frontend(5, root_sink)
            .expect("register browser frontend");
        router
            .register_page_frontend(10, "TID-page".to_owned(), "SID-page".to_owned(), page_sink)
            .expect("register page frontend");

        assert!(
            router.enqueue_protocol_output_sequence(ProtocolOutputSequence::from_messages(vec![
                json!({
                    "method": "Target.targetCreated",
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
    fn second_browser_frontend_is_rejected_without_replacing_first() {
        let mut routing = CdpFrontendRoutingState::default();
        routing
            .register_browser_frontend(5, test_sink())
            .expect("register first browser frontend");

        assert_eq!(
            routing
                .register_browser_frontend(6, test_sink())
                .expect_err("reject second browser frontend"),
            "CDP owner already has a browser frontend"
        );
        assert!(routing.frontend_by_id(5).is_some());
        assert!(routing.frontend_by_id(6).is_none());
    }

    #[test]
    fn private_control_session_events_do_not_reach_browser_frontend() {
        let mut routing = CdpFrontendRoutingState::default();
        routing
            .register_browser_frontend(5, test_sink())
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
            .register_browser_frontend(5, test_sink())
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
