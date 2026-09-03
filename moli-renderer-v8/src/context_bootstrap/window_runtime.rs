use super::*;

mod base64;
mod date_locale;
mod dialogs;
mod navigator;
mod performance;
mod service_worker;
mod structured_clone;
mod window_features;

use moli_webapi_declare::WebApiObject;
use moli_webidl_callback::PreparedWebIdlCallbackFunction;

use crate::callback_invocation::invoke_synchronous_webidl_callback_function;

pub(super) use base64::{window_atob_callback, window_btoa_callback};
pub(super) use date_locale::{
    current_date_locale_overrides, date_to_locale_date_string_callback,
    date_to_locale_string_callback, date_to_locale_time_string_callback,
};
pub(crate) use date_locale::{
    set_date_locale_override_for_current_context, set_date_timezone_override_for_current_context,
};
pub(super) use dialogs::entered_window_api_base_url;
pub(super) use dialogs::{window_alert_callback, window_confirm_callback, window_prompt_callback};
pub(crate) use dialogs::{
    window_const_false_callback, window_noop_callback, window_open_callback, window_stop_callback,
};
pub(crate) use navigator::{
    LegacyStorageQuotaCallbackOutcome, LegacyStorageQuotaCallbackTask,
    LegacyStorageQuotaCallbackTaskEffect,
};
pub(super) use navigator::{
    MEDIA_DEVICES_BRAND_SLOT, PERMISSIONS_BRAND_SLOT, build_legacy_storage_info_object,
    build_legacy_storage_quota_object, build_navigator_ua_data_object,
    global_caches_getter_callback, navigator_get_battery_callback, navigator_java_enabled_callback,
    navigator_media_devices_enumerate_devices_callback,
    navigator_media_devices_get_user_media_callback, navigator_permissions_query_callback,
    navigator_send_beacon_callback, navigator_storage_estimate_callback,
    navigator_storage_get_directory_callback, navigator_storage_persist_callback,
    navigator_storage_persisted_callback, navigator_ua_data_get_high_entropy_values_callback,
    navigator_ua_data_to_json_callback, navigator_vibrate_callback,
    permission_status_name_getter_callback, permission_status_state_getter_callback,
    storage_bucket_caches_getter_callback, storage_bucket_durability_callback,
    storage_bucket_estimate_callback, storage_bucket_expires_callback,
    storage_bucket_get_directory_callback, storage_bucket_indexed_db_getter_callback,
    storage_bucket_manager_delete_callback, storage_bucket_manager_keys_callback,
    storage_bucket_manager_open_callback, storage_bucket_name_getter_callback,
    storage_bucket_persist_callback, storage_bucket_persisted_callback,
    storage_bucket_set_expires_callback,
};
pub(super) use performance::performance_now_callback;
pub(crate) use service_worker::{
    ServiceWorkerClientMessageCallbackDispatchEffect, ServiceWorkerClientMessageDispatchEffect,
    ServiceWorkerInternalEventCallbackDispatchEffect, dispatch_service_worker_client_message_body,
    dispatch_service_worker_controller_change, dispatch_service_worker_lifecycle_notification,
    settle_service_worker_ready_completion, settle_service_worker_register_completion,
    settle_service_worker_unregister_completion,
};
pub(super) use service_worker::{
    install_initial_service_worker_ready_promise,
    navigator_service_worker_controller_getter_callback,
    navigator_service_worker_controllerchange_handler_getter_callback,
    navigator_service_worker_controllerchange_handler_setter_callback,
    navigator_service_worker_get_registration_callback,
    navigator_service_worker_get_registrations_callback,
    navigator_service_worker_message_handler_getter_callback,
    navigator_service_worker_message_handler_setter_callback,
    navigator_service_worker_messageerror_handler_getter_callback,
    navigator_service_worker_messageerror_handler_setter_callback,
    navigator_service_worker_register_callback, service_worker_object_set_owner_scope,
};
pub(super) use structured_clone::window_structured_clone_callback;

#[derive(Default, WebApiObject)]
#[webapi(interface = "Object")]
struct ChildWindowOwnMethodsDeclaration {
    #[webapi(method, length = 0, callback = window_open_callback)]
    open: (),
    #[webapi(method, length = 0, callback = window_noop_callback)]
    close: (),
    #[webapi(method, length = 0, callback = window_noop_callback)]
    blur: (),
    #[webapi(method, length = 0, callback = window_const_false_callback)]
    find: (),
    #[webapi(method, length = 0, callback = window_stop_callback)]
    stop: (),
    #[webapi(method, length = 0, callback = window_noop_callback)]
    print: (),
}

pub(crate) fn install_child_window_own_methods<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    window: v8::Local<'s, v8::Object>,
) -> std::result::Result<(), moli_webapi_declare::BindError> {
    ChildWindowOwnMethodsDeclaration::default().initialize(scope, window)
}

fn set_chrome_property<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    object: v8::Local<'s, v8::Object>,
    name: &str,
    value: v8::Local<'s, v8::Value>,
) -> anyhow::Result<()> {
    let property_name = name;
    let name =
        v8_string(scope, property_name).ok_or_else(|| anyhow!("failed to allocate chrome key"))?;
    object
        .set(scope, name.into(), value)
        .filter(|set| *set)
        .ok_or_else(|| anyhow!("failed to set chrome.{property_name}"))?;
    Ok(())
}

fn chrome_native_function<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    name: &str,
    callback: impl v8::MapFnTo<v8::FunctionCallback>,
) -> anyhow::Result<v8::Local<'s, v8::Function>> {
    let function = v8::Function::new(scope, callback)
        .ok_or_else(|| anyhow!("failed to allocate chrome.{name}"))?;
    let name = v8_string(scope, name).ok_or_else(|| anyhow!("failed to allocate function name"))?;
    function.set_name(name);
    Ok(function)
}

fn chrome_csi_callback(
    scope: &mut v8::PinScope<'_, '_>,
    _args: v8::FunctionCallbackArguments<'_>,
    mut rv: v8::ReturnValue<'_, v8::Value>,
) {
    let object = v8::Object::new(scope);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0.0, |duration| duration.as_secs_f64() * 1000.0);
    for name in ["startE", "onloadT", "pageT"] {
        let _ = set_chrome_property(scope, object, name, v8::Number::new(scope, now).into());
    }
    let _ = set_chrome_property(scope, object, "tran", v8::Integer::new(scope, 15).into());
    rv.set(object.into());
}

fn chrome_load_times_callback(
    scope: &mut v8::PinScope<'_, '_>,
    _args: v8::FunctionCallbackArguments<'_>,
    mut rv: v8::ReturnValue<'_, v8::Value>,
) {
    let object = v8::Object::new(scope);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0.0, |duration| duration.as_secs_f64());
    for name in [
        "commitLoadTime",
        "finishDocumentLoadTime",
        "finishLoadTime",
        "firstPaintTime",
        "requestTime",
        "startLoadTime",
    ] {
        let _ = set_chrome_property(scope, object, name, v8::Number::new(scope, now).into());
    }
    let _ = set_chrome_property(
        scope,
        object,
        "firstPaintAfterLoadTime",
        v8::Number::new(scope, 0.0).into(),
    );
    for (name, value) in [
        ("connectionInfo", "h2"),
        ("navigationType", "Other"),
        ("npnNegotiatedProtocol", "h2"),
    ] {
        if let Some(value) = v8_string(scope, value) {
            let _ = set_chrome_property(scope, object, name, value.into());
        }
    }
    for name in [
        "wasAlternateProtocolAvailable",
        "wasFetchedViaSpdy",
        "wasNpnNegotiated",
    ] {
        let _ = set_chrome_property(scope, object, name, v8::Boolean::new(scope, true).into());
    }
    rv.set(object.into());
}

fn chrome_app_get_details_callback(
    scope: &mut v8::PinScope<'_, '_>,
    _args: v8::FunctionCallbackArguments<'_>,
    mut rv: v8::ReturnValue<'_, v8::Value>,
) {
    rv.set(v8::null(scope).into());
}

fn chrome_app_get_is_installed_callback(
    scope: &mut v8::PinScope<'_, '_>,
    _args: v8::FunctionCallbackArguments<'_>,
    mut rv: v8::ReturnValue<'_, v8::Value>,
) {
    rv.set(v8::Boolean::new(scope, false).into());
}

fn chrome_app_running_state_callback(
    scope: &mut v8::PinScope<'_, '_>,
    _args: v8::FunctionCallbackArguments<'_>,
    mut rv: v8::ReturnValue<'_, v8::Value>,
) {
    if let Some(value) = v8_string(scope, "cannot_run") {
        rv.set(value.into());
    }
}

fn chrome_app_install_state_callback<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    args: v8::FunctionCallbackArguments<'s>,
    _rv: v8::ReturnValue<'s, v8::Value>,
) {
    let callback = args.get(0);
    let Ok(callback) = v8::Local::<v8::Function>::try_from(callback) else {
        return;
    };
    let Some(value) = v8_string(scope, "not_installed") else {
        return;
    };
    let callback_object: v8::Local<'_, v8::Object> = callback.into();
    let Some(relevant_context) = callback_object.get_creation_context(scope) else {
        return;
    };
    let incumbent_context = scope.get_current_context();
    let Some(callback) = PreparedWebIdlCallbackFunction::try_new(
        scope,
        callback_object,
        relevant_context,
        incumbent_context,
    ) else {
        return;
    };
    let receiver = v8::undefined(scope).into();
    let _ =
        invoke_synchronous_webidl_callback_function(scope, &callback, receiver, &[value.into()]);
}

pub(super) fn install_chrome_runtime_state<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    global: v8::Local<'s, v8::Object>,
) -> anyhow::Result<()> {
    let chrome = v8::Object::new(scope);
    let app = v8::Object::new(scope);
    set_chrome_property(
        scope,
        app,
        "isInstalled",
        v8::Boolean::new(scope, false).into(),
    )?;
    let get_details = chrome_native_function(scope, "getDetails", chrome_app_get_details_callback)?;
    set_chrome_property(scope, app, "getDetails", get_details.into())?;
    let get_is_installed = chrome_native_function(
        scope,
        "getIsInstalled",
        chrome_app_get_is_installed_callback,
    )?;
    set_chrome_property(scope, app, "getIsInstalled", get_is_installed.into())?;
    let install_state =
        chrome_native_function(scope, "installState", chrome_app_install_state_callback)?;
    set_chrome_property(scope, app, "installState", install_state.into())?;
    let running_state =
        chrome_native_function(scope, "runningState", chrome_app_running_state_callback)?;
    set_chrome_property(scope, app, "runningState", running_state.into())?;
    set_chrome_property(scope, chrome, "app", app.into())?;
    let csi = chrome_native_function(scope, "csi", chrome_csi_callback)?;
    set_chrome_property(scope, chrome, "csi", csi.into())?;
    let load_times = chrome_native_function(scope, "loadTimes", chrome_load_times_callback)?;
    set_chrome_property(scope, chrome, "loadTimes", load_times.into())?;
    set_chrome_property(scope, global, "chrome", chrome.into())
}
