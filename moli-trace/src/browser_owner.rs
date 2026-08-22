use std::{
    fs::OpenOptions,
    io::{BufWriter, Write},
    sync::OnceLock,
};

use parking_lot::Mutex;
use serde::Serialize;

use crate::ENV_BROWSER_OWNER_TRACE_JSONL;

const BROWSER_OWNER_TRACE_SCHEMA_VERSION: u8 = 1;

/// Protocol-neutral renderer Document identity written to the Browser Owner
/// machine trace.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub struct BrowserOwnerTraceDocument {
    renderer_page_id: u64,
    document_generation: u64,
    lifecycle_epoch: u64,
}

impl BrowserOwnerTraceDocument {
    pub fn new(renderer_page_id: u64, document_generation: u64, lifecycle_epoch: u64) -> Self {
        Self {
            renderer_page_id,
            document_generation,
            lifecycle_epoch,
        }
    }
}

/// Stable, machine-readable projection of one Browser Owner trace record.
///
/// This schema deliberately contains no URL, cookie, authorization, body,
/// frontend session, or wire command payload. Optional owner fields remain
/// explicit `null` values so an incomplete correlation cannot be mistaken for
/// a complete Browser fact.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct BrowserOwnerTraceRecord<'a> {
    schema_version: u8,
    browser_instance_id: Option<u64>,
    browser_context_id: Option<&'a str>,
    target_id: Option<&'a str>,
    page_residence_generation: Option<u64>,
    navigation_request_id: Option<u64>,
    renderer_agent_attachment_id: Option<u64>,
    document_lifecycle_identity: Option<BrowserOwnerTraceDocument>,
    browser_action_id: Option<u64>,
    browser_fact_sequence: Option<u64>,
    source: &'a str,
    navigation_origin: Option<&'a str>,
    owner_state_before: &'a str,
    owner_state_after: &'a str,
    frontend_projection_sequence: Option<u64>,
    renderer_lifecycle_sequence: Option<u64>,
    renderer_lifecycle_kind: Option<&'a str>,
    renderer_lifecycle_reason: Option<&'a str>,
    renderer_lifecycle_last_reached: Option<&'a str>,
    stage: &'a str,
}

impl<'a> BrowserOwnerTraceRecord<'a> {
    pub fn new(
        stage: &'a str,
        source: &'a str,
        owner_state_before: &'a str,
        owner_state_after: &'a str,
    ) -> Self {
        Self {
            schema_version: BROWSER_OWNER_TRACE_SCHEMA_VERSION,
            browser_instance_id: None,
            browser_context_id: None,
            target_id: None,
            page_residence_generation: None,
            navigation_request_id: None,
            renderer_agent_attachment_id: None,
            document_lifecycle_identity: None,
            browser_action_id: None,
            browser_fact_sequence: None,
            source,
            navigation_origin: None,
            owner_state_before,
            owner_state_after,
            frontend_projection_sequence: None,
            renderer_lifecycle_sequence: None,
            renderer_lifecycle_kind: None,
            renderer_lifecycle_reason: None,
            renderer_lifecycle_last_reached: None,
            stage,
        }
    }

    pub fn with_browser_instance_id(mut self, value: Option<u64>) -> Self {
        self.browser_instance_id = value;
        self
    }

    pub fn with_browser_context_id(mut self, value: Option<&'a str>) -> Self {
        self.browser_context_id = value;
        self
    }

    pub fn with_target_id(mut self, value: Option<&'a str>) -> Self {
        self.target_id = value;
        self
    }

    pub fn with_page_residence_generation(mut self, value: Option<u64>) -> Self {
        self.page_residence_generation = value;
        self
    }

    pub fn with_navigation_request_id(mut self, value: Option<u64>) -> Self {
        self.navigation_request_id = value;
        self
    }

    pub fn with_renderer_agent_attachment_id(mut self, value: Option<u64>) -> Self {
        self.renderer_agent_attachment_id = value;
        self
    }

    pub fn with_document_lifecycle_identity(
        mut self,
        value: Option<BrowserOwnerTraceDocument>,
    ) -> Self {
        self.document_lifecycle_identity = value;
        self
    }

    pub fn with_browser_action_id(mut self, value: Option<u64>) -> Self {
        self.browser_action_id = value;
        self
    }

    pub fn with_browser_fact_sequence(mut self, value: Option<u64>) -> Self {
        self.browser_fact_sequence = value;
        self
    }

    pub fn with_navigation_origin(mut self, value: Option<&'a str>) -> Self {
        self.navigation_origin = value;
        self
    }

    pub fn with_frontend_projection_sequence(mut self, value: Option<u64>) -> Self {
        self.frontend_projection_sequence = value;
        self
    }

    pub fn with_renderer_lifecycle(
        mut self,
        sequence: Option<u64>,
        kind: Option<&'a str>,
        reason: Option<&'a str>,
        last_reached: Option<&'a str>,
    ) -> Self {
        self.renderer_lifecycle_sequence = sequence;
        self.renderer_lifecycle_kind = kind;
        self.renderer_lifecycle_reason = reason;
        self.renderer_lifecycle_last_reached = last_reached;
        self
    }
}

/// Appends one complete JSON object followed by one newline when an explicit
/// machine-trace path is configured.
///
/// Diagnostics failures are intentionally non-fatal to browser execution.
/// The benchmark consumer treats a missing or truncated trace as its own gate
/// failure instead of allowing observability I/O to become execution authority.
pub fn emit_browser_owner_trace_record(record: &BrowserOwnerTraceRecord<'_>) {
    let Some(serialized) = serde_json::to_vec(record).ok() else {
        return;
    };
    let Some(sink) = browser_owner_trace_jsonl_sink() else {
        return;
    };
    let mut writer = sink.lock();
    if writer.write_all(&serialized).is_err() || writer.write_all(b"\n").is_err() {
        return;
    }
    let _ = writer.flush();
}

fn browser_owner_trace_jsonl_sink() -> Option<&'static Mutex<BufWriter<std::fs::File>>> {
    static SINK: OnceLock<Option<Mutex<BufWriter<std::fs::File>>>> = OnceLock::new();
    SINK.get_or_init(|| {
        let path = std::env::var_os(ENV_BROWSER_OWNER_TRACE_JSONL)?;
        if path.is_empty() {
            return None;
        }
        OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .ok()
            .map(BufWriter::new)
            .map(Mutex::new)
    })
    .as_ref()
}
