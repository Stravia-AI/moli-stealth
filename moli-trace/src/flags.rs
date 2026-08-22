use std::sync::OnceLock;

pub const ENV_CDP_NAV_TIMING: &str = "MOLI_CDP_NAV_TIMING";
pub const ENV_BROWSER_OWNER_TRACE: &str = "MOLI_BROWSER_OWNER_TRACE";
pub const ENV_BROWSER_OWNER_TRACE_JSONL: &str = "MOLI_BROWSER_OWNER_TRACE_JSONL";
pub const ENV_CDP_RUNTIME_TRACE: &str = "MOLI_CDP_RUNTIME_TRACE";
pub const ENV_CMD_PROBE: &str = "MOLI_CMD_PROBE";
pub const ENV_V8_EXCEPTION_PROBE: &str = "MOLI_V8_EXCEPTION_PROBE";
pub const ENV_DOM_BINDING_TIMING: &str = "MOLI_DOM_BINDING_TIMING";
pub const ENV_DEFER_WAIT_PROBE: &str = "MOLI_DEFER_WAIT_PROBE";
pub const ENV_DCL_WAIT_PROBE: &str = "MOLI_DCL_WAIT_PROBE";
pub const ENV_MODULE_LOAD_TRACE: &str = "MOLI_MODULE_LOAD_TRACE";
pub const ENV_WINDOW_MESSAGE_TRACE: &str = "MOLI_WINDOW_MESSAGE_TRACE";
pub const ENV_STYLE_INVALIDATION_TRACE: &str = "MOLI_STYLE_INVALIDATION_TRACE";
pub const ENV_CPU_PROFILE: &str = "MOLI_CPU_PROFILE";
pub const ENV_DISABLE_PARSER_MODULE_DEPENDENCY_PREWARM: &str =
    "MOLI_DISABLE_PARSER_MODULE_DEPENDENCY_PREWARM";

pub fn cdp_nav_timing_enabled() -> bool {
    static FLAG: OnceLock<bool> = OnceLock::new();
    *FLAG.get_or_init(|| env_flag_present(ENV_CDP_NAV_TIMING))
}

pub fn browser_owner_trace_enabled() -> bool {
    static FLAG: OnceLock<bool> = OnceLock::new();
    *FLAG.get_or_init(|| {
        browser_owner_human_trace_enabled()
            || std::env::var_os(ENV_BROWSER_OWNER_TRACE_JSONL).is_some_and(|path| !path.is_empty())
    })
}

pub fn browser_owner_human_trace_enabled() -> bool {
    static FLAG: OnceLock<bool> = OnceLock::new();
    *FLAG.get_or_init(|| env_flag_present(ENV_BROWSER_OWNER_TRACE))
}

pub fn cdp_runtime_trace_enabled() -> bool {
    static FLAG: OnceLock<bool> = OnceLock::new();
    *FLAG.get_or_init(|| env_flag_present(ENV_CDP_RUNTIME_TRACE))
}

pub fn command_probe_enabled() -> bool {
    static FLAG: OnceLock<bool> = OnceLock::new();
    *FLAG.get_or_init(|| env_flag_equals_one(ENV_CMD_PROBE))
}

pub fn v8_exception_probe_enabled() -> bool {
    static FLAG: OnceLock<bool> = OnceLock::new();
    *FLAG.get_or_init(|| env_flag_present(ENV_V8_EXCEPTION_PROBE))
}

pub fn dom_binding_timing_enabled() -> bool {
    static FLAG: OnceLock<bool> = OnceLock::new();
    *FLAG.get_or_init(|| env_flag_present(ENV_DOM_BINDING_TIMING))
}

pub fn promise_hook_trace_enabled() -> bool {
    dom_binding_timing_enabled() || cdp_runtime_trace_enabled()
}

pub fn defer_wait_probe_enabled() -> bool {
    static FLAG: OnceLock<bool> = OnceLock::new();
    *FLAG.get_or_init(|| env_flag_present(ENV_DEFER_WAIT_PROBE))
}

pub fn dcl_wait_probe_enabled() -> bool {
    static FLAG: OnceLock<bool> = OnceLock::new();
    *FLAG.get_or_init(|| env_flag_present(ENV_DCL_WAIT_PROBE))
}

pub fn module_load_trace_enabled() -> bool {
    static FLAG: OnceLock<bool> = OnceLock::new();
    *FLAG.get_or_init(|| env_flag_present(ENV_MODULE_LOAD_TRACE))
}

pub fn window_message_trace_enabled() -> bool {
    static FLAG: OnceLock<bool> = OnceLock::new();
    *FLAG.get_or_init(|| env_flag_present(ENV_WINDOW_MESSAGE_TRACE))
}

pub fn style_invalidation_trace_enabled() -> bool {
    static FLAG: OnceLock<bool> = OnceLock::new();
    *FLAG.get_or_init(|| env_flag_present(ENV_STYLE_INVALIDATION_TRACE))
}

pub fn cpu_profile_enabled() -> bool {
    static FLAG: OnceLock<bool> = OnceLock::new();
    *FLAG.get_or_init(|| env_flag_present(ENV_CPU_PROFILE))
}

pub fn parser_module_dependency_prewarm_enabled() -> bool {
    static FLAG: OnceLock<bool> = OnceLock::new();
    *FLAG.get_or_init(|| !env_flag_present(ENV_DISABLE_PARSER_MODULE_DEPENDENCY_PREWARM))
}

fn env_flag_present(name: &str) -> bool {
    std::env::var_os(name).is_some()
}

fn env_flag_equals_one(name: &str) -> bool {
    std::env::var(name).ok().as_deref() == Some("1")
}
