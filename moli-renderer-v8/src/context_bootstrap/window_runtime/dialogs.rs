use super::super::{
    location_navigation::{LocationNavigationKind, navigate_location_object},
    navigation_cancellation::inform_about_canceled_navigation_for_window,
};
use crate::{
    context_bootstrap::CHILD_BROWSING_CONTEXT_HANDLE_SLOT,
    document_runtime::{DocumentPolicyContainer, DomHandle},
    native_bridge::{
        InputNavigationPolicy, child_window_handle_from_marker_data,
        element::{
            SpecialBrowsingContextTarget,
            cancel_pending_renderer_owned_javascript_url_navigation_for_window,
            existing_browsing_context_target_window, navigate_existing_browsing_context_target,
            queue_renderer_owned_top_level_javascript_url_navigation_for_window,
            resolve_named_browsing_context_target_for_navigation,
            source_javascript_url_allowed_by_csp_for_owner,
            source_javascript_url_allows_new_context_by_policy,
        },
        entered_child_window_handle,
    },
    runtime::{
        RendererPendingJavaScriptDialog, RendererPendingPopupActivation,
        RendererPendingWindowOpenEvent, RendererPopupNewTargetDisposition,
        RendererTopLevelNavigationRequest,
    },
    util::{context_host_ptr_from_global_bridge, get_private_value},
    webidl,
};
use url::Url;

use super::window_features::WindowOpenFeatures;

#[derive(webidl::WebIdlArgs)]
#[webidl(prefix = "Window dialog")]
struct WindowDialogMessageArgs {
    #[webidl(default = "")]
    message: String,
}

#[derive(webidl::WebIdlArgs)]
#[webidl(prefix = "Window.prompt")]
struct WindowPromptArgs {
    #[webidl(default = "")]
    message: String,
    #[webidl(default = "")]
    default_prompt: String,
}

#[derive(webidl::WebIdlArgs)]
#[webidl(prefix = "Window.open")]
struct WindowOpenArgs {
    #[webidl(default = "", converter = "usv_string")]
    raw_url: String,
    #[webidl(default = "")]
    target_name: String,
    #[webidl(default = "")]
    features: String,
}

pub(in crate::context_bootstrap) fn window_alert_callback<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    args: v8::FunctionCallbackArguments<'s>,
    _rv: v8::ReturnValue<'_, v8::Value>,
) {
    let Some(parsed) = webidl::parse_args::<WindowDialogMessageArgs>(scope, &args) else {
        return;
    };
    let _ = open_dialog(scope, "alert", &parsed.message, "");
}

pub(crate) fn window_noop_callback(
    _scope: &mut v8::PinScope<'_, '_>,
    _args: v8::FunctionCallbackArguments<'_>,
    _rv: v8::ReturnValue<'_, v8::Value>,
) {
}

pub(crate) fn window_stop_callback<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    args: v8::FunctionCallbackArguments<'s>,
    _rv: v8::ReturnValue<'_, v8::Value>,
) {
    inform_about_canceled_navigation_for_window(scope, args.this());
}

pub(in crate::context_bootstrap) fn window_confirm_callback<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    args: v8::FunctionCallbackArguments<'s>,
    mut rv: v8::ReturnValue<'_, v8::Value>,
) {
    let Some(parsed) = webidl::parse_args::<WindowDialogMessageArgs>(scope, &args) else {
        return;
    };
    let accepted =
        open_dialog(scope, "confirm", &parsed.message, "").is_some_and(|result| result.accepted);
    rv.set(v8::Boolean::new(scope, accepted).into());
}

pub(in crate::context_bootstrap) fn window_prompt_callback<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    args: v8::FunctionCallbackArguments<'s>,
    mut rv: v8::ReturnValue<'_, v8::Value>,
) {
    let Some(parsed) = webidl::parse_args::<WindowPromptArgs>(scope, &args) else {
        return;
    };
    if let Some(result) = open_dialog(scope, "prompt", &parsed.message, &parsed.default_prompt)
        && result.accepted
    {
        if let Some(user_input) = v8::String::new(scope, &result.user_input) {
            rv.set(user_input.into());
        }
        return;
    }
    rv.set(v8::null(scope).into());
}

pub(crate) fn window_const_false_callback(
    scope: &mut v8::PinScope<'_, '_>,
    _args: v8::FunctionCallbackArguments<'_>,
    mut rv: v8::ReturnValue<'_, v8::Value>,
) {
    rv.set(v8::Boolean::new(scope, false).into());
}

pub(crate) fn window_open_callback<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    args: v8::FunctionCallbackArguments<'s>,
    mut rv: v8::ReturnValue<'_, v8::Value>,
) {
    let Some(parsed) = webidl::parse_args::<WindowOpenArgs>(scope, &args) else {
        return;
    };
    let Some(host_ptr) = context_host_ptr_from_global_bridge(scope) else {
        rv.set(v8::null(scope).into());
        return;
    };
    let entered_window = {
        let host = unsafe { &*host_ptr };
        window_open_entered_window(scope, host).unwrap_or_else(|| args.this())
    };
    let source_scope = window_open_receiver_child_handle(scope, entered_window)
        .map(crate::native_bridge::OwnerDispatchScope::Child)
        .unwrap_or_else(|| unsafe { &*host_ptr }.entered_owner_dispatch_scope(scope));
    let special_target = SpecialBrowsingContextTarget::parse(&parsed.target_name);
    let entered_base_url = {
        let host = unsafe { &*host_ptr };
        entered_window_api_base_url(scope, host)
    };
    let url = if parsed.raw_url.is_empty() {
        Url::parse("about:blank").expect("about:blank should parse")
    } else {
        match Url::options()
            .base_url(Some(&entered_base_url))
            .parse(&parsed.raw_url)
        {
            Ok(url) => url,
            Err(_) => {
                webidl::throw_dom_exception(
                    scope,
                    "SyntaxError",
                    "Unable to open a window with an invalid URL.",
                );
                return;
            }
        }
    };
    if special_target == Some(SpecialBrowsingContextTarget::Current) {
        if !source_javascript_url_allowed_by_csp_for_owner(
            scope,
            unsafe { &mut *host_ptr },
            source_scope,
            url.as_str(),
        ) {
            rv.set(entered_window.into());
            return;
        }
        navigate_window_open_self(scope, entered_window, url.as_str(), &mut rv);
        return;
    }
    let parsed_features = WindowOpenFeatures::parse(&parsed.features);
    let suppress_opener = parsed_features.suppresses_opener();
    let suppress_referrer = parsed_features.suppresses_referrer();
    let mut creator_policy_container = {
        let host = unsafe { &*host_ptr };
        host.document_policy_container_snapshot_for_owner(source_scope)
            .unwrap_or_else(|| window_open_entered_policy_container(scope, host))
    };
    let entered_document_url = {
        let host = unsafe { &*host_ptr };
        window_open_entered_document_url(scope, host)
    };
    let initial_document_referrer = if suppress_referrer {
        String::new()
    } else {
        entered_document_url.to_string()
    };
    creator_policy_container.document_referrer = initial_document_referrer.clone();
    let navigation_referrer = if suppress_referrer {
        String::new()
    } else {
        moli_fetch::referrer_header_value(
            &entered_document_url,
            &url,
            None,
            creator_policy_container.referrer_policy.as_deref(),
        )
        .unwrap_or_default()
    };
    let document_referrer = if suppress_referrer {
        String::new()
    } else if moli_url::is_about_blank(&url) {
        initial_document_referrer.clone()
    } else {
        moli_fetch::navigation_referrer_value(
            &entered_document_url,
            &url,
            None,
            creator_policy_container.referrer_policy.as_deref(),
        )
        .unwrap_or_default()
    };
    let is_javascript_url = url.scheme() == "javascript";
    let url = url.to_string();
    let navigation_source = unsafe { &*host_ptr }
        .renderer_top_level_navigation_source_for_dispatch_scope(source_scope, suppress_referrer);
    let javascript_navigation_request = is_javascript_url.then(|| {
        let request = RendererTopLevelNavigationRequest::get(url.clone());
        navigation_source
            .clone()
            .map_or(request.clone(), |source| request.with_source(source))
    });
    if let Some(
        target @ (SpecialBrowsingContextTarget::Parent | SpecialBrowsingContextTarget::Top),
    ) = special_target
    {
        let Some(target_window) = existing_browsing_context_target_window(scope, host_ptr, target)
        else {
            rv.set(v8::null(scope).into());
            return;
        };
        if !source_javascript_url_allowed_by_csp_for_owner(
            scope,
            unsafe { &mut *host_ptr },
            source_scope,
            &url,
        ) {
            rv.set(target_window.into());
            return;
        }
        match navigate_existing_browsing_context_target(
            scope,
            host_ptr,
            target,
            &url,
            navigation_source.clone(),
        ) {
            Some(window) => rv.set(window.into()),
            None => rv.set(v8::null(scope).into()),
        }
        return;
    }
    let resolved_named_target =
        trackable_named_popup_target_name(&parsed.target_name).and_then(|name| {
            resolve_named_browsing_context_target_for_navigation(
                scope,
                host_ptr,
                source_scope,
                name,
                &url,
            )
        });
    let existing_named_target_allows_source_navigation =
        resolved_named_target.as_ref().is_none_or(|_| {
            source_javascript_url_allowed_by_csp_for_owner(
                scope,
                unsafe { &mut *host_ptr },
                source_scope,
                &url,
            )
        });
    if let Some(target) = resolved_named_target.as_ref()
        && target.related_top_level_page().is_none()
    {
        if existing_named_target_allows_source_navigation {
            let _ = target.navigate_existing_context(
                scope,
                host_ptr,
                &url,
                navigation_source.clone(),
                None,
            );
        }
        if suppress_opener {
            rv.set(v8::null(scope).into());
        } else {
            rv.set(target.window().into());
        }
        return;
    }
    let host = unsafe { &mut *host_ptr };
    let Some((_, root_document, source)) =
        host.renderer_window_document_source_for_dispatch_scope(source_scope)
    else {
        rv.set(v8::null(scope).into());
        return;
    };
    let popup_disposition = match host
        .current_input_event()
        .map(crate::native_bridge::CurrentInputEvent::navigation_policy)
    {
        Some(InputNavigationPolicy::NewBackgroundSurface) => {
            crate::RendererPopupDisposition::Background
        }
        Some(
            InputNavigationPolicy::Current
            | InputNavigationPolicy::Download
            | InputNavigationPolicy::NewWindow
            | InputNavigationPolicy::NewForegroundSurface,
        )
        | None => crate::RendererPopupDisposition::Foreground,
    };
    if let Some(target) = resolved_named_target.as_ref()
        && let Some(resolved_target_page) = target.related_top_level_page()
    {
        if !suppress_opener
            && !host.replace_related_page_top_level_opener(
                scope,
                resolved_target_page,
                entered_window,
            )
        {
            rv.set(v8::null(scope).into());
            return;
        }
        if !existing_named_target_allows_source_navigation {
            if suppress_opener {
                rv.set(v8::null(scope).into());
            } else {
                rv.set(target.window().into());
            }
            return;
        }
        if target.related_local_top_level_context().is_some() {
            if let Some(request) = javascript_navigation_request.clone()
                && !queue_renderer_owned_top_level_javascript_url_navigation_for_window(
                    scope,
                    target.window(),
                    request,
                )
            {
                tracing::warn!(
                    ?resolved_target_page,
                    "selected related javascript URL target lost its renderer Page owner"
                );
            } else if !is_javascript_url {
                cancel_pending_renderer_owned_javascript_url_navigation_for_window(
                    scope,
                    target.window(),
                );
            }
        } else if is_javascript_url {
            // Current remote targets are cross-origin script-agent splits;
            // their source-side CanNavigate check rejects javascript: before
            // this branch. Do not accidentally queue the URL on the facade's
            // observing Page if a stale caller reaches here.
            if suppress_opener {
                rv.set(v8::null(scope).into());
            } else {
                rv.set(target.window().into());
            }
            return;
        }
        let activation = popup_activation_for_javascript_url(
            RendererPendingPopupActivation::window(
                root_document,
                source,
                !suppress_opener,
                None,
                url,
                parsed.target_name,
                popup_disposition,
            ),
            is_javascript_url,
        )
        .with_navigation_referrers(
            navigation_referrer,
            initial_document_referrer,
            document_referrer,
        )
        .with_resolved_target_page(resolved_target_page);
        host.record_pending_popup_activation(activation, None);
        if suppress_opener {
            rv.set(v8::null(scope).into());
        } else {
            rv.set(target.window().into());
        }
        return;
    }
    let opener = (!suppress_opener).then_some(entered_window);
    let opener_child_handle =
        opener.and_then(|opener| window_open_receiver_child_handle(scope, opener));
    let existing_lightweight_target = (opener.is_some()
        && popup_target_can_use_lightweight_window(&parsed.target_name, &url))
    .then(|| {
        host.live_lightweight_popup_id_for_name(&parsed.target_name)
            .and_then(|popup_id| host.lightweight_popup_window(scope, popup_id))
    })
    .flatten();
    if let Some(existing_window) = existing_lightweight_target {
        if !source_javascript_url_allowed_by_csp_for_owner(scope, host, source_scope, &url) {
            if suppress_opener {
                rv.set(v8::null(scope).into());
            } else {
                rv.set(existing_window.into());
            }
            return;
        }
        let Some(opened_popup) = host.reopen_existing_lightweight_popup_window(
            scope,
            opener,
            opener_child_handle,
            &parsed.target_name,
            &url,
            entered_base_url.clone(),
            creator_policy_container.clone(),
        ) else {
            if suppress_opener {
                rv.set(v8::null(scope).into());
            } else {
                rv.set(existing_window.into());
            }
            return;
        };
        let popup_id = opened_popup.popup_id;
        let session_storage_store = host.lightweight_popup_session_storage_store(popup_id);
        let initial_empty_document_storage_key =
            host.lightweight_popup_initial_empty_document_storage_key(popup_id);
        let activation = popup_activation_for_javascript_url(
            RendererPendingPopupActivation::window(
                root_document,
                source,
                !suppress_opener,
                Some(popup_id),
                url,
                parsed.target_name,
                popup_disposition,
            ),
            is_javascript_url,
        )
        .with_navigation_referrers(
            navigation_referrer,
            initial_document_referrer,
            document_referrer,
        )
        .with_initial_auxiliary_state(session_storage_store, initial_empty_document_storage_key);
        host.record_pending_popup_activation(activation, None);
        if suppress_opener {
            rv.set(v8::null(scope).into());
        } else {
            rv.set(opened_popup.window.into());
        }
        return;
    }
    if !source_javascript_url_allows_new_context_by_policy(scope, host, source_scope, &url) {
        rv.set(v8::null(scope).into());
        return;
    }
    let admission = match host.admit_new_auxiliary_browsing_context(creator_policy_container) {
        Ok(admission) => admission,
        Err(_) => {
            rv.set(v8::null(scope).into());
            return;
        }
    };
    let creation_user_activation = admission.user_activation();
    let auxiliary_browsing_context_policy = admission.renderer_auxiliary_browsing_context_policy();
    let creator_policy = admission.into_creation_policy();
    let window_open_event = RendererPendingWindowOpenEvent {
        url: url.clone(),
        window_name: if parsed.target_name.is_empty() {
            "_blank".to_owned()
        } else {
            parsed.target_name.clone()
        },
        window_features: parsed_features.enabled_feature_strings(),
        user_gesture: creation_user_activation.user_gesture(),
    };
    if suppress_opener
        && popup_target_can_use_fresh_page_without_local_proxy(&parsed.target_name, &url)
        && let Some(pending_auxiliary_page) = host.reserve_pending_auxiliary_page(false)
    {
        let new_target_disposition =
            if trackable_named_popup_target_name(&parsed.target_name).is_some() {
                RendererPopupNewTargetDisposition::FreshNamed
            } else {
                RendererPopupNewTargetDisposition::FreshUnnamed
            };
        let activation = popup_activation_for_javascript_url(
            RendererPendingPopupActivation::window(
                root_document,
                source,
                false,
                None,
                url,
                parsed.target_name,
                popup_disposition,
            ),
            is_javascript_url,
        )
        .with_navigation_referrers(
            navigation_referrer,
            initial_document_referrer,
            document_referrer,
        )
        .with_pending_auxiliary_page(Some(pending_auxiliary_page))
        .with_auxiliary_browsing_context_policy(auxiliary_browsing_context_policy)
        .with_new_target_disposition(new_target_disposition)
        .with_creation_user_activation(creation_user_activation);
        host.record_pending_popup_activation(activation, Some(window_open_event));
        rv.set(v8::null(scope).into());
        return;
    }
    if popup_target_can_use_lightweight_window(&parsed.target_name, &url)
        && let Some(opened_popup) = host.open_lightweight_popup_window(
            scope,
            host_ptr,
            opener,
            opener_child_handle,
            &parsed.target_name,
            &url,
            Some(!suppress_opener),
            true,
            entered_base_url,
            creator_policy,
        )
    {
        let popup_id = opened_popup.popup_id;
        let session_storage_store = opened_popup
            .captured_session_storage_store
            .clone()
            .or_else(|| host.lightweight_popup_session_storage_store(popup_id));
        let initial_empty_document_storage_key = opened_popup
            .captured_initial_empty_document_storage_key
            .clone()
            .or_else(|| host.lightweight_popup_initial_empty_document_storage_key(popup_id));
        let pending_auxiliary_page = opened_popup.pending_auxiliary_page;
        let new_target_disposition = (opened_popup.created_new_browsing_context
            && pending_auxiliary_page.is_some()
            && !suppress_opener)
            .then_some(RendererPopupNewTargetDisposition::Related);
        let window_open_event = opened_popup
            .created_new_browsing_context
            .then_some(window_open_event);
        if pending_auxiliary_page.is_some()
            && let Some(request) = javascript_navigation_request.clone()
            && !queue_renderer_owned_top_level_javascript_url_navigation_for_window(
                scope,
                opened_popup.window,
                request,
            )
        {
            tracing::warn!(
                popup_id,
                "new related javascript URL target lost its staged renderer Page owner"
            );
        }
        let activation = popup_activation_for_javascript_url(
            RendererPendingPopupActivation::window(
                root_document,
                source,
                !suppress_opener,
                Some(popup_id),
                url,
                parsed.target_name,
                popup_disposition,
            ),
            is_javascript_url,
        )
        .with_navigation_referrers(
            navigation_referrer,
            initial_document_referrer,
            document_referrer,
        )
        .with_initial_auxiliary_state(session_storage_store, initial_empty_document_storage_key)
        .with_pending_auxiliary_page(pending_auxiliary_page);
        let activation = if let Some(disposition) = new_target_disposition {
            activation.with_new_target_disposition(disposition)
        } else {
            activation
        };
        let activation = if opened_popup.created_new_browsing_context {
            activation.with_creation_user_activation(creation_user_activation)
        } else {
            activation
        };
        host.record_pending_popup_activation(activation, window_open_event);
        if suppress_opener {
            rv.set(v8::null(scope).into());
        } else {
            rv.set(opened_popup.window.into());
        }
        return;
    }
    let activation = popup_activation_for_javascript_url(
        RendererPendingPopupActivation::window(
            root_document,
            source,
            !suppress_opener,
            None,
            url,
            parsed.target_name,
            popup_disposition,
        ),
        is_javascript_url,
    )
    .with_navigation_referrers(
        navigation_referrer,
        initial_document_referrer,
        document_referrer,
    )
    .with_initial_auxiliary_state(None, None)
    .with_creation_user_activation(creation_user_activation);
    host.record_pending_popup_activation(activation, Some(window_open_event));
    rv.set(v8::null(scope).into());
}

fn window_open_receiver_child_handle<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    receiver: v8::Local<'s, v8::Object>,
) -> Option<DomHandle> {
    get_private_value(scope, receiver, CHILD_BROWSING_CONTEXT_HANDLE_SLOT)
        .and_then(|value| child_window_handle_from_marker_data(scope, value))
}

pub(in crate::context_bootstrap) fn entered_window_api_base_url(
    scope: &mut v8::PinScope<'_, '_>,
    host: &crate::native_bridge::JsContextHost,
) -> Url {
    if let Some(handle) = entered_child_window_handle(scope)
        && let Some(url) = host.child_browsing_context_base_url(handle)
    {
        return url;
    }
    if let Some(url) = host.active_lightweight_popup_base_url(scope) {
        return url;
    }
    host.dom_host()
        .document_base_url()
        .unwrap_or_else(|| host.document_url().clone())
}

fn window_open_entered_document_url(
    scope: &mut v8::PinScope<'_, '_>,
    host: &crate::native_bridge::JsContextHost,
) -> Url {
    if let Some(handle) = entered_child_window_handle(scope) {
        return host.document_url_for_child_context(handle);
    }
    if let Some(popup_id) = crate::native_bridge::active_lightweight_popup_id(scope)
        && let Some(url) = host.lightweight_popup_document_url(popup_id)
    {
        return url;
    }
    host.dom_host()
        .document_url()
        .cloned()
        .unwrap_or_else(|| host.document_url().clone())
}

fn window_open_entered_policy_container(
    scope: &mut v8::PinScope<'_, '_>,
    host: &crate::native_bridge::JsContextHost,
) -> DocumentPolicyContainer {
    if let Some(handle) = entered_child_window_handle(scope)
        && let Some(policy_container) =
            host.child_browsing_context_policy_container_snapshot(handle)
    {
        return policy_container;
    }
    if let Some(policy_container) = host.active_lightweight_popup_policy_container(scope) {
        return policy_container.clone();
    }
    host.document_policy_container().clone()
}

fn window_open_entered_window<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    host: &crate::native_bridge::JsContextHost,
) -> Option<v8::Local<'s, v8::Object>> {
    match host.entered_owner_dispatch_scope(scope) {
        crate::native_bridge::OwnerDispatchScope::Top => {
            Some(scope.get_current_context().global(scope))
        }
        crate::native_bridge::OwnerDispatchScope::Child(handle) => {
            host.existing_child_browsing_context_window_wrapper(scope, handle)
        }
        crate::native_bridge::OwnerDispatchScope::LightweightPopup(popup_id) => {
            host.lightweight_popup_window(scope, popup_id)
        }
    }
}

fn popup_target_can_use_lightweight_window(target_name: &str, href: &str) -> bool {
    Url::parse(href).is_ok()
        && (target_name.is_empty()
            || SpecialBrowsingContextTarget::parse(target_name)
                == Some(SpecialBrowsingContextTarget::Blank)
            || trackable_named_popup_target_name(target_name).is_some())
}

fn popup_target_can_use_fresh_page_without_local_proxy(target_name: &str, href: &str) -> bool {
    Url::parse(href).is_ok()
        && (target_name.is_empty()
            || SpecialBrowsingContextTarget::parse(target_name)
                == Some(SpecialBrowsingContextTarget::Blank)
            || trackable_named_popup_target_name(target_name).is_some())
}

fn popup_activation_for_javascript_url(
    activation: RendererPendingPopupActivation,
    is_javascript_url: bool,
) -> RendererPendingPopupActivation {
    if is_javascript_url {
        activation.without_destination_navigation_with_requested_url_observation()
    } else {
        activation
    }
}

fn trackable_named_popup_target_name(target_name: &str) -> Option<&str> {
    if target_name.is_empty() || SpecialBrowsingContextTarget::parse(target_name).is_some() {
        return None;
    }
    Some(target_name)
}

fn navigate_window_open_self<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    receiver: v8::Local<'s, v8::Object>,
    url: &str,
    rv: &mut v8::ReturnValue<'_, v8::Value>,
) {
    let Some(location) =
        super::super::navigation_window::window_location_for_holder(scope, receiver)
    else {
        rv.set(v8::null(scope).into());
        return;
    };
    navigate_location_object(
        scope,
        location,
        LocationNavigationKind::Assign,
        Some(url.to_owned()),
    );
    rv.set(receiver.into());
}

fn open_dialog(
    scope: &mut v8::PinScope<'_, '_>,
    dialog_type: &str,
    message: &str,
    default_prompt: &str,
) -> Option<crate::runtime::RendererJavaScriptDialogResult> {
    let host_ptr = context_host_ptr_from_global_bridge(scope)?;
    let host = unsafe { &mut *host_ptr };
    // Protocol handling starts only after this request is bound to an exact
    // Page/Document source. A standalone or stale realm uses the headless
    // default result instead of claiming a dialog that cannot be emitted.
    let (target, source_document, source) = host.current_renderer_window_document_source(scope)?;
    let source_url = window_open_entered_document_url(scope, host).to_string();
    let dialog_id = host.allocate_javascript_dialog_id();
    host.open_modal_javascript_dialog(
        target,
        RendererPendingJavaScriptDialog::new(
            dialog_id,
            source_document,
            source,
            source_url,
            dialog_type.to_owned(),
            message.to_owned(),
            default_prompt.to_owned(),
            None,
        ),
    )
}
