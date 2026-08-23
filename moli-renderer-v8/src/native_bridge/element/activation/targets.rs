use crate::{
    RendererPendingPopupActivation, RendererPendingWindowOpenEvent,
    RendererPopupDisposition, RendererPopupNewTargetDisposition, RendererTopLevelNavigationRequest,
    context_bootstrap::{
        dispatch_cross_document_navigation_navigate_event_for_window,
        dispatch_cross_document_navigation_navigate_event_for_window_with_form_data,
        runtime_window_dispatch_scope,
    },
    document_runtime::{DocumentPolicyContainer, DomHandle},
    native_bridge::context_host::{
        ChildBrowsingContextNavigationRequest, FormSubmissionChildNavigationTarget,
        OwnerDispatchScope, PendingFormSubmissionChildNavigation, WindowExecutionContextIdentity,
    },
    util::{context_host_ptr_from_context_slot, context_host_ptr_from_window_object, v8str},
};

use super::super::super::JsContextHost;

/// A browsing-context keyword whose meaning is fixed by HTML.
///
/// Parsing happens once, at the DOM navigation boundary. Downstream routing
/// consumes this type instead of matching raw strings, so ASCII case variants
/// cannot accidentally fall through to named-frame or popup creation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SpecialBrowsingContextTarget {
    Current,
    Parent,
    Top,
    Blank,
}

impl SpecialBrowsingContextTarget {
    pub(crate) fn parse(target_name: &str) -> Option<Self> {
        if target_name.eq_ignore_ascii_case("_self") {
            Some(Self::Current)
        } else if target_name.eq_ignore_ascii_case("_parent") {
            Some(Self::Parent)
        } else if target_name.eq_ignore_ascii_case("_top") {
            Some(Self::Top)
        } else if target_name.eq_ignore_ascii_case("_blank") {
            Some(Self::Blank)
        } else {
            None
        }
    }
}

pub(crate) enum NamedBrowsingContextNavigationTarget<'s> {
    CurrentTopLevel {
        window: v8::Local<'s, v8::Object>,
        target_context: v8::Local<'s, v8::Context>,
    },
    CurrentPageChild {
        window: v8::Local<'s, v8::Object>,
        handle: DomHandle,
        browsing_context_id: crate::browsing_context_model::BrowsingContextId,
    },
    RelatedTopLevel {
        window: v8::Local<'s, v8::Object>,
        target_context: v8::Local<'s, v8::Context>,
        page: crate::RendererResolvedPopupTarget,
    },
    RelatedRemoteTopLevel {
        window: v8::Local<'s, v8::Object>,
        target: crate::script_vm::RendererRemoteTopLevelWindowProxyTarget,
    },
    RelatedRemotePageChild {
        window: v8::Local<'s, v8::Object>,
        target: crate::script_vm::RendererRemoteTopLevelWindowProxyTarget,
        frame: Box<crate::script_vm::RendererRemoteFrameSnapshot>,
    },
    RelatedPageChild {
        window: v8::Local<'s, v8::Object>,
        owner_host_ptr: *mut JsContextHost,
        handle: DomHandle,
        page: crate::RendererResolvedPopupTarget,
        browsing_context_id: crate::browsing_context_model::BrowsingContextId,
    },
}

impl<'s> NamedBrowsingContextNavigationTarget<'s> {
    pub(crate) fn window(&self) -> v8::Local<'s, v8::Object> {
        match self {
            Self::CurrentTopLevel { window, .. }
            | Self::CurrentPageChild { window, .. }
            | Self::RelatedTopLevel { window, .. }
            | Self::RelatedRemoteTopLevel { window, .. }
            | Self::RelatedRemotePageChild { window, .. }
            | Self::RelatedPageChild { window, .. } => *window,
        }
    }

    pub(crate) fn related_top_level_page(&self) -> Option<crate::RendererResolvedPopupTarget> {
        match self {
            Self::RelatedTopLevel { page, .. } => Some(*page),
            Self::RelatedRemoteTopLevel { target, .. } => Some(target.residence),
            Self::CurrentTopLevel { .. }
            | Self::CurrentPageChild { .. }
            | Self::RelatedRemotePageChild { .. }
            | Self::RelatedPageChild { .. } => None,
        }
    }

    pub(crate) fn related_local_top_level_context(&self) -> Option<v8::Local<'s, v8::Context>> {
        match self {
            Self::RelatedTopLevel { target_context, .. } => Some(*target_context),
            Self::CurrentTopLevel { .. }
            | Self::CurrentPageChild { .. }
            | Self::RelatedRemoteTopLevel { .. }
            | Self::RelatedRemotePageChild { .. }
            | Self::RelatedPageChild { .. } => None,
        }
    }

    fn related_top_level_target(&self) -> Option<RelatedTopLevelNavigationTarget<'s>> {
        match self {
            Self::RelatedTopLevel {
                window,
                target_context,
                page,
            } => Some(RelatedTopLevelNavigationTarget::Local {
                window: *window,
                target_context: *target_context,
                page: *page,
            }),
            Self::RelatedRemoteTopLevel { target, .. } => {
                Some(RelatedTopLevelNavigationTarget::Remote {
                    target: target.clone(),
                })
            }
            Self::CurrentTopLevel { .. }
            | Self::CurrentPageChild { .. }
            | Self::RelatedRemotePageChild { .. }
            | Self::RelatedPageChild { .. } => None,
        }
    }

    pub(crate) fn navigate_existing_context(
        &self,
        scope: &mut v8::PinScope<'s, '_>,
        source_host_ptr: *mut JsContextHost,
        resolved_url: &str,
        navigation_source: Option<crate::RendererTopLevelNavigationSource>,
        source_element: Option<v8::Local<'s, v8::Object>>,
    ) -> bool {
        match self {
            Self::CurrentTopLevel { window, .. } => navigate_target_window_location(
                scope,
                source_host_ptr,
                *window,
                resolved_url,
                navigation_source,
            ),
            Self::CurrentPageChild { window, handle, .. } => navigate_resolved_child_target(
                scope,
                source_host_ptr,
                *handle,
                *window,
                resolved_url,
                source_element,
            ),
            Self::RelatedPageChild {
                window,
                owner_host_ptr,
                handle,
                ..
            } => navigate_resolved_child_target(
                scope,
                *owner_host_ptr,
                *handle,
                *window,
                resolved_url,
                source_element,
            ),
            Self::RelatedRemotePageChild { target, frame, .. } => {
                let Some(target_url) = url::Url::parse(resolved_url).ok() else {
                    return false;
                };
                let request = child_navigation_request_from_top_level_source(
                    target_url,
                    navigation_source.as_ref(),
                );
                let command = crate::runtime::RendererRemoteWindowProxyCommand::navigate_frame(
                    frame.token,
                    target.residence,
                    target.channel,
                    crate::runtime::RendererRemoteWindowProxyNavigationKind::Assign,
                    request,
                    None,
                );
                unsafe { &*source_host_ptr }.append_live_turn_owner_action(
                    crate::runtime::RendererOwnerAction::RemoteWindowProxy(command),
                )
            }
            Self::RelatedTopLevel { .. } | Self::RelatedRemoteTopLevel { .. } => false,
        }
    }
}

enum RelatedTopLevelNavigationTarget<'s> {
    Local {
        window: v8::Local<'s, v8::Object>,
        target_context: v8::Local<'s, v8::Context>,
        page: crate::RendererResolvedPopupTarget,
    },
    Remote {
        target: crate::script_vm::RendererRemoteTopLevelWindowProxyTarget,
    },
}

impl RelatedTopLevelNavigationTarget<'_> {
    fn page(&self) -> crate::RendererResolvedPopupTarget {
        match self {
            Self::Local { page, .. } => *page,
            Self::Remote { target, .. } => target.residence,
        }
    }
}

fn navigate_resolved_child_target<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    target_host_ptr: *mut JsContextHost,
    target_handle: DomHandle,
    target_window: v8::Local<'s, v8::Object>,
    resolved_url: &str,
    source_element: Option<v8::Local<'s, v8::Object>>,
) -> bool {
    let target_host = unsafe { &mut *target_host_ptr };
    let target_url = url::Url::parse(resolved_url).ok();
    let target_is_same_origin_with_top = target_url
        .as_ref()
        .is_some_and(|url| moli_url::same_origin(target_host.document_url(), url));
    let target_is_same_document_with_child = target_url.as_ref().is_some_and(|url| {
        target_host
            .child_browsing_context_current_url(target_handle)
            .is_some_and(|current| urls_refer_to_same_document(&current, url))
    });
    if ((target_is_same_origin_with_top
        && target_host.child_browsing_context_is_same_origin_with_top(target_handle))
        || target_is_same_document_with_child)
        && !dispatch_cross_document_navigation_navigate_event_for_window(
            scope,
            target_window,
            resolved_url,
            source_element,
            false,
            None,
        )
    {
        return true;
    }
    target_host.navigate_child_browsing_context_to_url(scope, target_handle, resolved_url)
}

/// Resolve one ordinary browsing-context name using Blink's frame-tree order:
/// the source frame subtree, the rest of its Page, then every live related
/// Page's complete frame tree. The browser/protocol name projection is not an
/// input to this decision.
pub(crate) fn resolve_named_browsing_context_target_for_navigation<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    source_host_ptr: *mut JsContextHost,
    source_scope: OwnerDispatchScope,
    target_name: &str,
    destination_url: &str,
) -> Option<NamedBrowsingContextNavigationTarget<'s>> {
    if target_name.is_empty() || SpecialBrowsingContextTarget::parse(target_name).is_some() {
        return None;
    }

    let source_host = unsafe { &mut *source_host_ptr };
    let source_identity = if source_host.entered_owner_dispatch_scope(scope) == source_scope {
        source_host.current_runtime_window_execution_context_identity(scope)
    } else {
        source_host.current_registered_window_execution_context_identity(source_scope)
    }?;
    let destination_url = url::Url::parse(destination_url).ok()?;
    source_host.sync_child_browsing_context_subtree(scope, source_host.document_handle());
    let current_page_handles = source_host.child_browsing_context_handles_in_document_order();
    let top_level_targets = source_host.related_page_top_level_targets_for_navigation();
    let source_top = top_level_targets.iter().find(|target| target.is_source);

    match source_scope {
        OwnerDispatchScope::Top => {
            if let Some(source_top) = source_top
                && source_top.name == target_name
                && source_host
                    .can_navigate_browsing_context(
                        scope,
                        source_identity,
                        source_host,
                        OwnerDispatchScope::Top,
                        &destination_url,
                    )
                    .is_ok()
                && let Some(crate::script_vm::RendererRelatedTopLevelWindowProxyResolution::Local {
                    window_proxy,
                    context,
                }) = source_host
                    .related_page_target_for_window_proxy_endpoint(scope, source_top.endpoint)
            {
                return Some(NamedBrowsingContextNavigationTarget::CurrentTopLevel {
                    window: window_proxy,
                    target_context: context,
                });
            }
            for handle in current_page_handles.iter().copied() {
                if let Some(target) = resolve_child_navigation_candidate(
                    scope,
                    source_host_ptr,
                    source_identity,
                    source_host_ptr,
                    handle,
                    target_name,
                    None,
                    &destination_url,
                ) {
                    return Some(target);
                }
            }
        }
        OwnerDispatchScope::Child(source_handle) => {
            for handle in current_page_handles
                .iter()
                .copied()
                .filter(|handle| child_handle_is_in_subtree(source_host, *handle, source_handle))
            {
                if let Some(target) = resolve_child_navigation_candidate(
                    scope,
                    source_host_ptr,
                    source_identity,
                    source_host_ptr,
                    handle,
                    target_name,
                    None,
                    &destination_url,
                ) {
                    return Some(target);
                }
            }
            if let Some(source_top) = source_top
                && source_top.name == target_name
                && source_host
                    .can_navigate_browsing_context(
                        scope,
                        source_identity,
                        source_host,
                        OwnerDispatchScope::Top,
                        &destination_url,
                    )
                    .is_ok()
                && let Some(crate::script_vm::RendererRelatedTopLevelWindowProxyResolution::Local {
                    window_proxy,
                    context,
                }) = source_host
                    .related_page_target_for_window_proxy_endpoint(scope, source_top.endpoint)
            {
                return Some(NamedBrowsingContextNavigationTarget::CurrentTopLevel {
                    window: window_proxy,
                    target_context: context,
                });
            }
            for handle in current_page_handles
                .iter()
                .copied()
                .filter(|handle| !child_handle_is_in_subtree(source_host, *handle, source_handle))
            {
                if let Some(target) = resolve_child_navigation_candidate(
                    scope,
                    source_host_ptr,
                    source_identity,
                    source_host_ptr,
                    handle,
                    target_name,
                    None,
                    &destination_url,
                ) {
                    return Some(target);
                }
            }
        }
    }

    for candidate in top_level_targets {
        if candidate.is_source {
            continue;
        }
        let Some(resolution) =
            source_host.related_page_target_for_window_proxy_endpoint(scope, candidate.endpoint)
        else {
            continue;
        };
        match resolution {
            crate::script_vm::RendererRelatedTopLevelWindowProxyResolution::Local {
                window_proxy,
                context,
            } => {
                let Some(target_host_ptr) = context_host_ptr_from_context_slot(context) else {
                    continue;
                };
                let target_host = unsafe { &mut *target_host_ptr };
                if candidate.name == target_name
                    && source_host
                        .can_navigate_browsing_context(
                            scope,
                            source_identity,
                            target_host,
                            OwnerDispatchScope::Top,
                            &destination_url,
                        )
                        .is_ok()
                {
                    return Some(NamedBrowsingContextNavigationTarget::RelatedTopLevel {
                        window: window_proxy,
                        target_context: context,
                        page: candidate.residence,
                    });
                }
                target_host
                    .sync_child_browsing_context_subtree(scope, target_host.document_handle());
                let handles = target_host.child_browsing_context_handles_in_document_order();
                for handle in handles {
                    if let Some(target) = resolve_child_navigation_candidate(
                        scope,
                        source_host_ptr,
                        source_identity,
                        target_host_ptr,
                        handle,
                        target_name,
                        Some(candidate.residence),
                        &destination_url,
                    ) {
                        return Some(target);
                    }
                }
            }
            crate::script_vm::RendererRelatedTopLevelWindowProxyResolution::Remote(target) => {
                if candidate.name == target_name
                    && source_host
                        .can_navigate_remote_top_level_browsing_context(
                            source_identity,
                            &target,
                            &destination_url,
                        )
                        .is_ok()
                {
                    let window = source_host
                        .remote_top_level_window_proxy_for_endpoint(scope, candidate.endpoint)?;
                    return Some(
                        NamedBrowsingContextNavigationTarget::RelatedRemoteTopLevel {
                            window,
                            target,
                        },
                    );
                }
                let Some(frame_tree) =
                    source_host.related_page_remote_frame_tree_snapshot(candidate.endpoint)
                else {
                    continue;
                };
                for frame in frame_tree {
                    if frame.name != target_name
                        || source_host
                            .can_navigate_remote_frame_browsing_context(
                                source_identity,
                                &frame,
                                &destination_url,
                            )
                            .is_err()
                    {
                        continue;
                    }
                    let window =
                        source_host.remote_frame_window_proxy_for_token(scope, frame.token)?;
                    return Some(
                        NamedBrowsingContextNavigationTarget::RelatedRemotePageChild {
                            window,
                            target,
                            frame: Box::new(frame),
                        },
                    );
                }
            }
        }
    }
    None
}

fn child_navigation_request_from_top_level_source(
    target_url: url::Url,
    source: Option<&crate::RendererTopLevelNavigationSource>,
) -> ChildBrowsingContextNavigationRequest {
    let request = ChildBrowsingContextNavigationRequest::new(
        target_url.clone(),
        "GET".to_owned(),
        None,
        Vec::new(),
    );
    let Some(source) = source else {
        return request;
    };
    let Some(source_url) = url::Url::parse(source.source_url()).ok() else {
        return request;
    };
    let navigation_referrer = if source.suppresses_referrer() {
        String::new()
    } else {
        moli_fetch::referrer_header_value(&source_url, &target_url, None, source.referrer_policy())
            .unwrap_or_default()
    };
    let document_referrer = if source.suppresses_referrer() {
        String::new()
    } else {
        moli_fetch::navigation_referrer_value(
            &source_url,
            &target_url,
            None,
            source.referrer_policy(),
        )
        .unwrap_or_default()
    };
    request.with_navigation_source(source_url, navigation_referrer, document_referrer)
}

fn child_handle_is_in_subtree(
    host: &JsContextHost,
    mut candidate: DomHandle,
    root: DomHandle,
) -> bool {
    loop {
        if candidate == root {
            return true;
        }
        let Some(parent) = host.child_browsing_context_parent_handle(candidate) else {
            return false;
        };
        candidate = parent;
    }
}

fn resolve_child_navigation_candidate<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    source_host_ptr: *mut JsContextHost,
    source_identity: WindowExecutionContextIdentity,
    target_host_ptr: *mut JsContextHost,
    handle: DomHandle,
    target_name: &str,
    target_page: Option<crate::RendererResolvedPopupTarget>,
    destination_url: &url::Url,
) -> Option<NamedBrowsingContextNavigationTarget<'s>> {
    let target_host = unsafe { &*target_host_ptr };
    if !target_host.child_browsing_context_matches_name_for_navigation(handle, target_name)
        || unsafe { &*source_host_ptr }
            .can_navigate_browsing_context(
                scope,
                source_identity,
                target_host,
                OwnerDispatchScope::Child(handle),
                destination_url,
            )
            .is_err()
    {
        return None;
    }
    let observer_can_access = source_can_access_target_scope(
        source_host_ptr,
        source_identity,
        target_host_ptr,
        OwnerDispatchScope::Child(handle),
    );
    let browsing_context_id = target_host.child_browsing_context_id_for_handle(handle)?;
    let window = unsafe { &mut *target_host_ptr }
        .child_browsing_context_window_for_navigation_observer(
            scope,
            handle,
            observer_can_access,
        )?;
    match target_page {
        None => Some(NamedBrowsingContextNavigationTarget::CurrentPageChild {
            window,
            handle,
            browsing_context_id,
        }),
        Some(page) => Some(NamedBrowsingContextNavigationTarget::RelatedPageChild {
            window,
            owner_host_ptr: target_host_ptr,
            handle,
            page,
            browsing_context_id,
        }),
    }
}

fn source_can_access_target_scope(
    source_host_ptr: *mut JsContextHost,
    source_identity: WindowExecutionContextIdentity,
    target_host_ptr: *mut JsContextHost,
    target_scope: OwnerDispatchScope,
) -> bool {
    let source_host = unsafe { &*source_host_ptr };
    if source_host_ptr == target_host_ptr {
        source_host
            .window_execution_context_can_access_dispatch_scope(source_identity, target_scope)
    } else {
        source_host.window_execution_context_can_access_related_page_dispatch_scope(
            source_identity,
            unsafe { &*target_host_ptr },
            target_scope,
        )
    }
}

fn navigate_target_window_location(
    scope: &mut v8::PinScope<'_, '_>,
    runtime_ptr: *mut JsContextHost,
    window: v8::Local<'_, v8::Object>,
    resolved_url: &str,
    source: Option<crate::RendererTopLevelNavigationSource>,
) -> bool {
    let Some(value) = crate::util::v8_string(scope, resolved_url) else {
        return false;
    };
    let previous_source =
        unsafe { &mut *runtime_ptr }.replace_active_top_level_navigation_source(source);
    let navigated = window
        .set(scope, v8str(scope, "location").into(), value.into())
        .unwrap_or(false);
    let _ =
        unsafe { &mut *runtime_ptr }.replace_active_top_level_navigation_source(previous_source);
    navigated
}

fn queue_top_level_location_navigation(
    scope: &mut v8::PinScope<'_, '_>,
    runtime_ptr: *mut JsContextHost,
    resolved_url: &str,
    source: Option<crate::RendererTopLevelNavigationSource>,
) -> bool {
    let runtime = unsafe { &mut *runtime_ptr };
    let source = source.or_else(|| {
        runtime.renderer_top_level_navigation_source_for_dispatch_scope(
            runtime.entered_owner_dispatch_scope(scope),
            false,
        )
    });
    let mut request = RendererTopLevelNavigationRequest::get(resolved_url.to_owned());
    if let Some(source) = source {
        request = request.with_source(source);
    }
    runtime.record_pending_renderer_top_level_navigation_request(request, None);
    true
}

fn is_javascript_navigation_request(request: &RendererTopLevelNavigationRequest) -> bool {
    url::Url::parse(request.url()).is_ok_and(|url| url.scheme() == "javascript")
}

fn queue_renderer_owned_top_level_navigation_for_host(
    target_host_ptr: *mut JsContextHost,
    request: RendererTopLevelNavigationRequest,
) -> bool {
    let target_host = unsafe { &mut *target_host_ptr };
    if target_host.root_document_lifecycle_identity().is_none() {
        return false;
    }
    target_host.record_cross_page_renderer_top_level_navigation_request(request, None)
}

fn queue_renderer_owned_top_level_javascript_url_navigation_for_host(
    target_host_ptr: *mut JsContextHost,
    request: RendererTopLevelNavigationRequest,
) -> bool {
    is_javascript_navigation_request(&request)
        && queue_renderer_owned_top_level_navigation_for_host(target_host_ptr, request)
}

fn cancel_pending_renderer_owned_javascript_url_navigation_for_host(
    target_host_ptr: *mut JsContextHost,
) {
    let target_host = unsafe { &mut *target_host_ptr };
    if target_host.pending_location_navigation_scheme_is("javascript") {
        target_host.clear_pending_location_navigation();
    }
}

fn queue_renderer_owned_top_level_javascript_url_navigation_in_context(
    target_context: v8::Local<'_, v8::Context>,
    request: RendererTopLevelNavigationRequest,
) -> bool {
    context_host_ptr_from_context_slot(target_context).is_some_and(|target_host_ptr| {
        queue_renderer_owned_top_level_javascript_url_navigation_for_host(target_host_ptr, request)
    })
}

fn cancel_pending_renderer_owned_javascript_url_navigation_in_context(
    target_context: v8::Local<'_, v8::Context>,
) {
    if let Some(target_host_ptr) = context_host_ptr_from_context_slot(target_context) {
        cancel_pending_renderer_owned_javascript_url_navigation_for_host(target_host_ptr);
    }
}

/// Sends a JavaScript URL directly to the real Page that owns `target_window`.
///
/// Target selection/creation is synchronous, but the Page owner follows this
/// pending slot on its next networking-task turn. Protocol therefore receives
/// a creation/reuse observation without a second destination navigation.
pub(crate) fn queue_renderer_owned_top_level_javascript_url_navigation_for_window(
    scope: &mut v8::PinScope<'_, '_>,
    target_window: v8::Local<'_, v8::Object>,
    request: RendererTopLevelNavigationRequest,
) -> bool {
    context_host_ptr_from_window_object(scope, target_window).is_some_and(|target_host_ptr| {
        queue_renderer_owned_top_level_javascript_url_navigation_for_host(target_host_ptr, request)
    })
}

/// Queues the first destination on a newly staged related Page itself.
///
/// The popup activation then owns only creation/observation. A later Location
/// assignment in the same author turn replaces this exact target-local slot,
/// so direct Browser and protocol owners cannot replay an older activation URL
/// after adopting the Page.
pub(crate) fn queue_renderer_owned_top_level_navigation_for_window(
    scope: &mut v8::PinScope<'_, '_>,
    target_window: v8::Local<'_, v8::Object>,
    request: RendererTopLevelNavigationRequest,
) -> bool {
    context_host_ptr_from_window_object(scope, target_window).is_some_and(|target_host_ptr| {
        queue_renderer_owned_top_level_navigation_for_host(target_host_ptr, request)
    })
}

/// Cancels a JavaScript URL task when a later browser-owned navigation wins
/// the same real Page target before that task receives an owner turn.
pub(crate) fn cancel_pending_renderer_owned_javascript_url_navigation_for_window(
    scope: &mut v8::PinScope<'_, '_>,
    target_window: v8::Local<'_, v8::Object>,
) {
    if let Some(target_host_ptr) = context_host_ptr_from_window_object(scope, target_window) {
        cancel_pending_renderer_owned_javascript_url_navigation_for_host(target_host_ptr);
    }
}

fn queue_popup_target_navigation(
    scope: &mut v8::PinScope<'_, '_>,
    runtime_ptr: *mut JsContextHost,
    target_name: &str,
    resolved_url: &str,
    exposes_opener: bool,
) -> bool {
    let runtime = unsafe { &mut *runtime_ptr };
    let dispatch_scope = runtime.entered_owner_dispatch_scope(scope);
    let Some((_, root_document, source)) =
        runtime.renderer_window_document_source_for_dispatch_scope(dispatch_scope)
    else {
        return false;
    };
    let creator_policy_container = runtime
        .document_policy_container_snapshot_for_owner(dispatch_scope)
        .unwrap_or_default();
    let admission = match runtime.admit_new_auxiliary_browsing_context(creator_policy_container) {
        Ok(admission) => admission,
        Err(_) => return true,
    };
    let creation_user_activation = admission.user_activation();
    let window_open_event = RendererPendingWindowOpenEvent::browser_window(
        resolved_url,
        target_name,
        creation_user_activation.user_gesture(),
    );
    runtime.record_pending_popup_activation(
        RendererPendingPopupActivation::window(
            root_document,
            source,
            exposes_opener,
            None,
            resolved_url.to_owned(),
            target_name.to_owned(),
            RendererPopupDisposition::Foreground,
        )
        .with_initial_auxiliary_state(None, None)
        .with_creation_user_activation(creation_user_activation),
        Some(window_open_event),
    );
    true
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::native_bridge) struct ElementPopupRelations {
    pub(in crate::native_bridge) suppress_opener: bool,
    pub(in crate::native_bridge) suppress_referrer: bool,
}

pub(in crate::native_bridge) fn element_popup_relations(
    runtime: &JsContextHost,
    source_handle: DomHandle,
    target_name: &str,
) -> ElementPopupRelations {
    let rel = runtime
        .dom_host()
        .node(source_handle)
        .and_then(crate::dom::native::Node::as_element)
        .and_then(|element| element.attribute("rel"))
        .unwrap_or_default();
    let mut has_opener = false;
    let mut has_noopener = false;
    let mut has_noreferrer = false;
    for token in rel.split_ascii_whitespace() {
        if token.eq_ignore_ascii_case("opener") {
            has_opener = true;
        } else if token.eq_ignore_ascii_case("noopener") {
            has_noopener = true;
        } else if token.eq_ignore_ascii_case("noreferrer") {
            has_noreferrer = true;
        }
    }
    ElementPopupRelations {
        suppress_opener: has_noreferrer
            || has_noopener
            || (target_name.eq_ignore_ascii_case("_blank") && !has_opener),
        suppress_referrer: has_noreferrer,
    }
}

struct ElementPopupCreator<'s> {
    opener: v8::Local<'s, v8::Object>,
    base_url: url::Url,
    policy_container: DocumentPolicyContainer,
    document_url: url::Url,
}

fn element_popup_creator<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    runtime_ptr: *mut JsContextHost,
    source_handle: DomHandle,
) -> Option<ElementPopupCreator<'s>> {
    let runtime = unsafe { &*runtime_ptr };
    let document = runtime.dom_host().owner_document_handle(source_handle)?;
    let base_url = runtime.document_base_url_for_handle(document);
    let raw_document_url = runtime.document_url_for_handle(document);
    if document == runtime.document_handle() {
        let policy_container = runtime.document_policy_container().clone();
        return Some(ElementPopupCreator {
            opener: scope.get_current_context().global(scope),
            base_url,
            document_url: outgoing_navigation_source_url(&raw_document_url, &policy_container),
            policy_container,
        });
    }
    let child_handle = runtime.child_browsing_context_host_for_document_handle(document)?;
    let policy_container =
        runtime.child_browsing_context_policy_container_snapshot(child_handle)?;
    Some(ElementPopupCreator {
        opener: runtime.existing_child_browsing_context_window_wrapper(scope, child_handle)?,
        base_url,
        document_url: outgoing_navigation_source_url(&raw_document_url, &policy_container),
        policy_container,
    })
}

fn outgoing_navigation_source_url(
    document_url: &url::Url,
    policy_container: &DocumentPolicyContainer,
) -> url::Url {
    if document_url.scheme() == "about"
        && let Ok(inherited_source) = url::Url::parse(&policy_container.document_referrer)
    {
        return inherited_source;
    }
    document_url.clone()
}

fn popup_disposition_for_current_input(runtime: &JsContextHost) -> RendererPopupDisposition {
    match runtime
        .current_input_event()
        .map(crate::native_bridge::CurrentInputEvent::navigation_policy)
    {
        Some(crate::native_bridge::InputNavigationPolicy::NewBackgroundSurface) => {
            RendererPopupDisposition::Background
        }
        Some(
            crate::native_bridge::InputNavigationPolicy::Current
            | crate::native_bridge::InputNavigationPolicy::Download
            | crate::native_bridge::InputNavigationPolicy::NewWindow
            | crate::native_bridge::InputNavigationPolicy::NewForegroundSurface,
        )
        | None => RendererPopupDisposition::Foreground,
    }
}

fn navigate_hyperlink_popup_target<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    runtime_ptr: *mut JsContextHost,
    source_handle: DomHandle,
    target_name: &str,
    resolved_url: &str,
    disposition: RendererPopupDisposition,
    resolved_related_target: Option<RelatedTopLevelNavigationTarget<'s>>,
) -> bool {
    navigate_element_popup_target(
        scope,
        runtime_ptr,
        source_handle,
        target_name,
        RendererTopLevelNavigationRequest::get(resolved_url.to_owned()),
        disposition,
        resolved_related_target,
        None,
        ElementPopupDestinationPolicy::Always,
    )
}

pub(in crate::native_bridge) fn navigate_form_auxiliary_target<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    runtime_ptr: *mut JsContextHost,
    form_handle: DomHandle,
    target_name: &str,
    navigation_request: RendererTopLevelNavigationRequest,
    source_element: Option<v8::Local<'s, v8::Object>>,
    form_data_entries: Option<&[(String, v8::Global<v8::Value>)]>,
    user_initiated: bool,
) -> bool {
    let disposition = popup_disposition_for_current_input(unsafe { &*runtime_ptr });
    navigate_element_popup_target(
        scope,
        runtime_ptr,
        form_handle,
        target_name,
        navigation_request,
        disposition,
        None,
        Some(FormPopupNavigationEvent {
            source_element,
            form_data_entries,
            user_initiated,
        }),
        ElementPopupDestinationPolicy::SourceFormPolicies,
    )
}

#[allow(clippy::too_many_arguments)]
pub(in crate::native_bridge) fn navigate_form_named_target<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    runtime_ptr: *mut JsContextHost,
    form_handle: DomHandle,
    target_name: &str,
    navigation_request: RendererTopLevelNavigationRequest,
    source_element: Option<v8::Local<'s, v8::Object>>,
    form_data_entries: Option<&[(String, v8::Global<v8::Value>)]>,
    user_initiated: bool,
    cancel_all_previous_child_targets: bool,
) -> bool {
    let disposition = popup_disposition_for_current_input(unsafe { &*runtime_ptr });
    let relations = element_popup_relations(unsafe { &*runtime_ptr }, form_handle, target_name);
    let source = unsafe { &*runtime_ptr }
        .renderer_top_level_navigation_source_for_node(form_handle, relations.suppress_referrer);
    let navigation_request = if let Some(source) = source {
        navigation_request.with_source(source)
    } else {
        navigation_request
    };
    let Some(source_scope) =
        browsing_context_dispatch_scope_for_node(scope, runtime_ptr, form_handle)
    else {
        return false;
    };
    let resolved_target = resolve_named_browsing_context_target_for_navigation(
        scope,
        runtime_ptr,
        source_scope,
        target_name,
        navigation_request.url(),
    );
    let event = FormPopupNavigationEvent {
        source_element,
        form_data_entries,
        user_initiated,
    };
    let destination_navigation_allowed =
        unsafe { &*runtime_ptr }.document_allows_form_submission_for_node(form_handle);
    if !destination_navigation_allowed {
        return match resolved_target {
            Some(_) => true,
            None => navigate_element_popup_target(
                scope,
                runtime_ptr,
                form_handle,
                target_name,
                navigation_request,
                disposition,
                None,
                None,
                ElementPopupDestinationPolicy::SourceFormPolicies,
            ),
        };
    }
    if resolved_target.is_some()
        && !source_node_javascript_url_allowed_by_csp(
            scope,
            runtime_ptr,
            form_handle,
            navigation_request.url(),
        )
    {
        return true;
    }
    if resolved_target.is_some()
        && !source_node_form_action_allowed_by_csp_with_runtime(
            scope,
            unsafe { &mut *runtime_ptr },
            form_handle,
            navigation_request.url(),
        )
    {
        return true;
    }
    if cancel_all_previous_child_targets {
        let pending = unsafe { &mut *runtime_ptr }
            .take_pending_form_submission_child_navigations_for_form(form_handle);
        cancel_pending_form_submission_child_navigations(scope, runtime_ptr, pending);
    }
    match resolved_target {
        Some(NamedBrowsingContextNavigationTarget::CurrentTopLevel {
            window,
            target_context,
        }) => navigate_resolved_top_level_form_target(
            scope,
            runtime_ptr,
            window,
            target_context,
            navigation_request,
            event,
        ),
        Some(NamedBrowsingContextNavigationTarget::CurrentPageChild {
            handle,
            browsing_context_id,
            ..
        }) => navigate_resolved_child_form_target(
            scope,
            runtime_ptr,
            runtime_ptr,
            handle,
            FormSubmissionChildNavigationTarget::current_page(browsing_context_id),
            form_handle,
            target_name,
            navigation_request,
            event,
            cancel_all_previous_child_targets,
        ),
        Some(NamedBrowsingContextNavigationTarget::RelatedTopLevel {
            window,
            target_context,
            page,
        }) => navigate_element_popup_target(
            scope,
            runtime_ptr,
            form_handle,
            target_name,
            navigation_request,
            disposition,
            Some(RelatedTopLevelNavigationTarget::Local {
                window,
                target_context,
                page,
            }),
            Some(event),
            ElementPopupDestinationPolicy::Always,
        ),
        Some(NamedBrowsingContextNavigationTarget::RelatedRemoteTopLevel { target, .. }) => {
            navigate_element_popup_target(
                scope,
                runtime_ptr,
                form_handle,
                target_name,
                navigation_request,
                disposition,
                Some(RelatedTopLevelNavigationTarget::Remote { target }),
                Some(event),
                ElementPopupDestinationPolicy::Always,
            )
        }
        Some(NamedBrowsingContextNavigationTarget::RelatedRemotePageChild {
            target,
            frame,
            ..
        }) => navigate_resolved_remote_child_form_target(
            scope,
            runtime_ptr,
            *frame,
            target,
            form_handle,
            target_name,
            navigation_request,
            cancel_all_previous_child_targets,
        ),
        Some(NamedBrowsingContextNavigationTarget::RelatedPageChild {
            owner_host_ptr,
            handle,
            page,
            browsing_context_id,
            ..
        }) => {
            let Some(root_document) =
                (unsafe { &*owner_host_ptr }).root_document_lifecycle_identity()
            else {
                tracing::warn!(
                    ?page,
                    ?browsing_context_id,
                    "refusing related child form navigation without an exact target root Document"
                );
                return false;
            };
            navigate_resolved_child_form_target(
                scope,
                runtime_ptr,
                owner_host_ptr,
                handle,
                FormSubmissionChildNavigationTarget::related_page(
                    page,
                    root_document,
                    browsing_context_id,
                ),
                form_handle,
                target_name,
                navigation_request,
                event,
                cancel_all_previous_child_targets,
            )
        }
        None => navigate_element_popup_target(
            scope,
            runtime_ptr,
            form_handle,
            target_name,
            navigation_request,
            disposition,
            None,
            Some(event),
            ElementPopupDestinationPolicy::SourceFormPolicies,
        ),
    }
}

#[allow(clippy::too_many_arguments)]
fn navigate_resolved_remote_child_form_target(
    scope: &mut v8::PinScope<'_, '_>,
    source_host_ptr: *mut JsContextHost,
    frame: crate::script_vm::RendererRemoteFrameSnapshot,
    target: crate::script_vm::RendererRemoteTopLevelWindowProxyTarget,
    form_handle: DomHandle,
    target_name: &str,
    navigation_request: RendererTopLevelNavigationRequest,
    cancel_all_previous_child_targets: bool,
) -> bool {
    let scheduler_target = FormSubmissionChildNavigationTarget::remote_page(
        target.residence,
        target.channel,
        frame.token,
    );
    if !cancel_all_previous_child_targets {
        let pending = unsafe { &mut *source_host_ptr }
            .take_previous_pending_form_submission_child_navigations(form_handle, scheduler_target);
        cancel_pending_form_submission_child_navigations(scope, source_host_ptr, pending);
    }
    let Some(request) = form_child_navigation_request(
        unsafe { &*source_host_ptr },
        form_handle,
        target_name,
        &navigation_request,
    ) else {
        return false;
    };
    let scheduler_id = crate::runtime::RendererRemoteFrameNavigationId::allocate();
    let command = crate::runtime::RendererRemoteWindowProxyCommand::navigate_frame(
        frame.token,
        target.residence,
        target.channel,
        crate::runtime::RendererRemoteWindowProxyNavigationKind::Assign,
        request,
        Some(scheduler_id),
    );
    if !unsafe { &*source_host_ptr }.append_live_turn_owner_action(
        crate::runtime::RendererOwnerAction::RemoteWindowProxy(command),
    ) {
        return false;
    }
    unsafe { &mut *source_host_ptr }.mark_pending_form_submission_child_navigation(
        form_handle,
        PendingFormSubmissionChildNavigation::remote(scheduler_target, scheduler_id),
    );
    true
}

#[allow(clippy::too_many_arguments)]
fn navigate_resolved_top_level_form_target<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    target_host_ptr: *mut JsContextHost,
    target_window: v8::Local<'s, v8::Object>,
    target_context: v8::Local<'s, v8::Context>,
    navigation_request: RendererTopLevelNavigationRequest,
    event: FormPopupNavigationEvent<'s, '_>,
) -> bool {
    if !dispatch_related_page_form_navigation_event(
        scope,
        target_window,
        target_context,
        navigation_request.url(),
        event,
    ) {
        return true;
    }
    unsafe { &mut *target_host_ptr }
        .record_pending_renderer_top_level_navigation_request(navigation_request, None);
    true
}

#[allow(clippy::too_many_arguments)]
fn navigate_resolved_child_form_target<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    source_host_ptr: *mut JsContextHost,
    target_host_ptr: *mut JsContextHost,
    target_handle: DomHandle,
    target: FormSubmissionChildNavigationTarget,
    form_handle: DomHandle,
    target_name: &str,
    navigation_request: RendererTopLevelNavigationRequest,
    event: FormPopupNavigationEvent<'s, '_>,
    cancel_all_previous_child_targets: bool,
) -> bool {
    if !cancel_all_previous_child_targets {
        let pending = unsafe { &mut *source_host_ptr }
            .take_previous_pending_form_submission_child_navigations(form_handle, target);
        cancel_pending_form_submission_child_navigations(scope, source_host_ptr, pending);
    }

    let Some(request) = form_child_navigation_request(
        unsafe { &*source_host_ptr },
        form_handle,
        target_name,
        &navigation_request,
    ) else {
        return false;
    };
    if !dispatch_child_form_navigation_event(
        scope,
        target_host_ptr,
        target_handle,
        navigation_request.url(),
        event,
    ) {
        return true;
    }
    let Some(navigation_load) = (unsafe { &mut *target_host_ptr })
        .queue_deferred_child_browsing_context_navigation_request(target_handle, request, false)
    else {
        return false;
    };
    unsafe { &mut *source_host_ptr }.mark_pending_form_submission_child_navigation(
        form_handle,
        PendingFormSubmissionChildNavigation::new(target, navigation_load),
    );
    true
}

fn cancel_pending_form_submission_child_navigations<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    source_host_ptr: *mut JsContextHost,
    pending_navigations: Vec<PendingFormSubmissionChildNavigation>,
) {
    for pending in pending_navigations {
        let target = pending.target();
        if let FormSubmissionChildNavigationTarget::RemotePage {
            page,
            channel,
            frame,
        } = target
        {
            let Some(scheduler_id) = pending.remote_scheduler_id() else {
                continue;
            };
            let command = crate::runtime::RendererRemoteWindowProxyCommand::cancel_frame_navigation(
                frame,
                page,
                channel,
                scheduler_id,
            );
            let _ = unsafe { &*source_host_ptr }.append_live_turn_owner_action(
                crate::runtime::RendererOwnerAction::RemoteWindowProxy(command),
            );
            continue;
        }
        let target_host_ptr = match target {
            FormSubmissionChildNavigationTarget::CurrentPage { .. } => source_host_ptr,
            FormSubmissionChildNavigationTarget::RelatedPage {
                page,
                root_document,
                ..
            } => {
                let Some(target_context) = (unsafe { &*source_host_ptr })
                    .related_page_current_context_for_residence(scope, page)
                else {
                    continue;
                };
                let Some(target_host_ptr) = context_host_ptr_from_context_slot(target_context)
                else {
                    continue;
                };
                if unsafe { &*target_host_ptr }.root_document_lifecycle_identity()
                    != Some(root_document)
                {
                    continue;
                }
                target_host_ptr
            }
            FormSubmissionChildNavigationTarget::RemotePage { .. } => {
                unreachable!("remote scheduler cancellation returned above")
            }
        };
        let Some(navigation_load) = pending.navigation_load() else {
            continue;
        };
        let _ = unsafe { &mut *target_host_ptr }
            .cancel_pending_form_submission_child_navigation_if_matches(
                scope,
                target.browsing_context_id(),
                navigation_load,
            );
    }
}

fn form_child_navigation_request(
    source_host: &JsContextHost,
    source_handle: DomHandle,
    target_name: &str,
    navigation_request: &RendererTopLevelNavigationRequest,
) -> Option<ChildBrowsingContextNavigationRequest> {
    let source_document = source_host
        .dom_host()
        .owner_document_handle(source_handle)?;
    let raw_source_url = source_host.document_url_for_handle(source_document);
    let target_url = url::Url::parse(navigation_request.url()).ok()?;
    let relations = element_popup_relations(source_host, source_handle, target_name);
    let policy_container = source_document_policy_container(source_host, source_document);
    let source_url = policy_container
        .as_ref()
        .map(|policy| outgoing_navigation_source_url(&raw_source_url, policy))
        .unwrap_or(raw_source_url);
    let referrer_policy = policy_container.and_then(|policy| policy.referrer_policy);
    let navigation_referrer = if relations.suppress_referrer {
        String::new()
    } else {
        moli_fetch::referrer_header_value(
            &source_url,
            &target_url,
            None,
            referrer_policy.as_deref(),
        )
        .unwrap_or_default()
    };
    let document_referrer = if relations.suppress_referrer {
        String::new()
    } else {
        moli_fetch::navigation_referrer_value(
            &source_url,
            &target_url,
            None,
            referrer_policy.as_deref(),
        )
        .unwrap_or_default()
    };
    Some(
        ChildBrowsingContextNavigationRequest::new(
            target_url,
            navigation_request.request_method().to_owned(),
            navigation_request.request_body().map(ToOwned::to_owned),
            navigation_request.request_headers().to_vec(),
        )
        .with_navigation_source(source_url, navigation_referrer, document_referrer),
    )
}

fn source_document_policy_container(
    source_host: &JsContextHost,
    source_document: DomHandle,
) -> Option<DocumentPolicyContainer> {
    if source_document == source_host.document_handle() {
        return Some(source_host.document_policy_container().clone());
    }
    source_host
        .child_browsing_context_host_for_document_handle(source_document)
        .and_then(|handle| source_host.child_browsing_context_policy_container_snapshot(handle))
}

fn dispatch_child_form_navigation_event<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    target_host_ptr: *mut JsContextHost,
    target_handle: DomHandle,
    resolved_url: &str,
    event: FormPopupNavigationEvent<'s, '_>,
) -> bool {
    let source_element = event
        .source_element
        .map(|source_element| v8::Global::new(scope, source_element));
    let target_context = {
        let target_host = unsafe { &mut *target_host_ptr };
        let Ok(target_context) =
            target_host.ensure_prebootstrapped_child_default_context(scope, target_handle)
        else {
            return true;
        };
        v8::Global::new(scope, target_context)
    };
    let target_context = v8::Local::new(scope, &target_context);
    let target_scope = &mut v8::ContextScope::new(scope, target_context);
    let Some(target_window) = (unsafe { &*target_host_ptr })
        .existing_child_browsing_context_window_wrapper(target_scope, target_handle)
    else {
        return true;
    };
    let form_data = match event.form_data_entries {
        Some(entries) => {
            let Some(form_data) = crate::form_data_object_from_entries(target_scope, entries)
            else {
                return false;
            };
            Some(form_data)
        }
        None => None,
    };
    let source_element = source_element
        .as_ref()
        .map(|source_element| v8::Local::new(target_scope, source_element));
    dispatch_cross_document_navigation_navigate_event_for_window_with_form_data(
        target_scope,
        target_window,
        resolved_url,
        source_element,
        event.user_initiated,
        None,
        form_data,
    )
}

struct FormPopupNavigationEvent<'s, 'entries> {
    source_element: Option<v8::Local<'s, v8::Object>>,
    form_data_entries: Option<&'entries [(String, v8::Global<v8::Value>)]>,
    user_initiated: bool,
}

#[derive(Clone, Copy)]
enum ElementPopupDestinationPolicy {
    Always,
    SourceFormPolicies,
}

impl ElementPopupDestinationPolicy {
    fn allows_form_submission(self, runtime: &JsContextHost, source_handle: DomHandle) -> bool {
        match self {
            Self::Always => true,
            Self::SourceFormPolicies => {
                runtime.document_allows_form_submission_for_node(source_handle)
            }
        }
    }

    fn allows_form_action(
        self,
        scope: &mut v8::PinScope<'_, '_>,
        runtime: &mut JsContextHost,
        source_handle: DomHandle,
        resolved_url: &str,
    ) -> bool {
        match self {
            Self::Always => true,
            Self::SourceFormPolicies => source_node_form_action_allowed_by_csp_with_runtime(
                scope,
                runtime,
                source_handle,
                resolved_url,
            ),
        }
    }

    fn allows_navigation(
        self,
        scope: &mut v8::PinScope<'_, '_>,
        runtime: &mut JsContextHost,
        source_handle: DomHandle,
        resolved_url: &str,
    ) -> bool {
        self.allows_form_submission(runtime, source_handle)
            && self.allows_form_action(scope, runtime, source_handle, resolved_url)
    }

    fn requires_initial_empty_creation(self) -> bool {
        matches!(self, Self::SourceFormPolicies)
    }
}

fn popup_activation_with_destination_policy(
    activation: RendererPendingPopupActivation,
    navigation_request: RendererTopLevelNavigationRequest,
    destination_navigation_allowed: bool,
) -> RendererPendingPopupActivation {
    if !destination_navigation_allowed {
        return activation.without_destination_navigation();
    }
    if is_javascript_navigation_request(&navigation_request) {
        return activation.without_destination_navigation_with_requested_url_observation();
    }
    activation.with_navigation_request(navigation_request)
}

fn navigate_element_popup_target<'s, 'entries>(
    scope: &mut v8::PinScope<'s, '_>,
    runtime_ptr: *mut JsContextHost,
    source_handle: DomHandle,
    target_name: &str,
    navigation_request: RendererTopLevelNavigationRequest,
    disposition: RendererPopupDisposition,
    resolved_related_target: Option<RelatedTopLevelNavigationTarget<'s>>,
    form_navigation_event: Option<FormPopupNavigationEvent<'s, 'entries>>,
    destination_policy: ElementPopupDestinationPolicy,
) -> bool {
    let relations = element_popup_relations(unsafe { &*runtime_ptr }, source_handle, target_name);
    let Some(dispatch_scope) =
        browsing_context_dispatch_scope_for_node(scope, runtime_ptr, source_handle)
    else {
        return false;
    };
    let Some((_, root_document, source)) =
        unsafe { &*runtime_ptr }.renderer_window_document_source_for_dispatch_scope(dispatch_scope)
    else {
        return false;
    };
    let Some(navigation_source) = (unsafe { &*runtime_ptr })
        .renderer_top_level_navigation_source_for_dispatch_scope(
            dispatch_scope,
            relations.suppress_referrer,
        )
    else {
        return false;
    };
    let navigation_request = navigation_request.with_source(navigation_source);
    let resolved_url = navigation_request.url();
    let Some(mut creator) = element_popup_creator(scope, runtime_ptr, source_handle) else {
        let runtime = unsafe { &mut *runtime_ptr };
        if !source_javascript_url_allows_new_context_by_policy(
            scope,
            runtime,
            dispatch_scope,
            resolved_url,
        ) {
            return true;
        }
        let creator_policy_container = runtime
            .document_policy_container_snapshot_for_owner(dispatch_scope)
            .unwrap_or_default();
        let admission = match runtime.admit_new_auxiliary_browsing_context(creator_policy_container)
        {
            Ok(admission) => admission,
            Err(_) => return true,
        };
        let creation_user_activation = admission.user_activation();
        let destination_navigation_allowed =
            destination_policy.allows_navigation(scope, runtime, source_handle, resolved_url);
        let window_open_event = RendererPendingWindowOpenEvent::browser_window(
            resolved_url,
            target_name,
            creation_user_activation.user_gesture(),
        );
        let activation = popup_activation_with_destination_policy(
            RendererPendingPopupActivation::window(
                root_document,
                source,
                !relations.suppress_opener,
                None,
                resolved_url.to_owned(),
                target_name.to_owned(),
                disposition,
            ),
            navigation_request,
            destination_navigation_allowed,
        )
        .with_initial_auxiliary_state(None, None)
        .with_creation_user_activation(creation_user_activation);
        runtime.record_pending_popup_activation(activation, Some(window_open_event));
        return true;
    };
    let initial_document_referrer = if relations.suppress_referrer {
        String::new()
    } else {
        creator.document_url.to_string()
    };
    creator.policy_container.document_referrer = initial_document_referrer.clone();
    let target_url = url::Url::parse(resolved_url).ok();
    let navigation_referrer = if relations.suppress_referrer {
        String::new()
    } else {
        target_url
            .as_ref()
            .and_then(|target_url| {
                moli_fetch::referrer_header_value(
                    &creator.document_url,
                    target_url,
                    None,
                    creator.policy_container.referrer_policy.as_deref(),
                )
            })
            .unwrap_or_default()
    };
    let document_referrer = if relations.suppress_referrer {
        String::new()
    } else if target_url.as_ref().is_some_and(moli_url::is_about_blank) {
        initial_document_referrer.clone()
    } else {
        target_url
            .as_ref()
            .and_then(|target_url| {
                moli_fetch::navigation_referrer_value(
                    &creator.document_url,
                    target_url,
                    None,
                    creator.policy_container.referrer_policy.as_deref(),
                )
            })
            .unwrap_or_default()
    };
    let opener = (!relations.suppress_opener).then_some(creator.opener);
    let runtime = unsafe { &mut *runtime_ptr };
    let ordinary_target_name = (SpecialBrowsingContextTarget::parse(target_name).is_none()
        && !target_name.is_empty())
    .then_some(target_name);
    let resolved_related_target = ordinary_target_name
        .is_some()
        .then(|| {
            resolved_related_target.or_else(|| {
                runtime
                    .related_page_named_target_for_navigation(
                        scope,
                        ordinary_target_name.expect("ordinary target name was checked"),
                        None,
                    )
                    .map(
                        |(window, target_context, page)| RelatedTopLevelNavigationTarget::Local {
                            window,
                            target_context,
                            page,
                        },
                    )
            })
        })
        .flatten();
    if let Some(target) = resolved_related_target {
        let resolved_target_page = target.page();
        let destination_navigation_allowed =
            destination_policy.allows_navigation(scope, runtime, source_handle, resolved_url);
        if !destination_navigation_allowed {
            return true;
        }
        if let RelatedTopLevelNavigationTarget::Local {
            window,
            target_context,
            ..
        } = &target
        {
            if let Some(form_navigation_event) = form_navigation_event
                && !dispatch_related_page_form_navigation_event(
                    scope,
                    *window,
                    *target_context,
                    resolved_url,
                    form_navigation_event,
                )
            {
                return true;
            }
            if is_javascript_navigation_request(&navigation_request)
                && !queue_renderer_owned_top_level_javascript_url_navigation_in_context(
                    *target_context,
                    navigation_request.clone(),
                )
            {
                tracing::warn!(
                    ?resolved_target_page,
                    "selected related javascript URL target lost its renderer Page owner"
                );
            } else if !is_javascript_navigation_request(&navigation_request) {
                cancel_pending_renderer_owned_javascript_url_navigation_in_context(*target_context);
            }
        } else if is_javascript_navigation_request(&navigation_request) {
            // Cross-agent targets are cross-origin in the currently supported
            // split model, so source-side CanNavigate must have rejected this
            // before target selection. Keep a defensive no-navigation result
            // if a stale caller bypasses that resolver.
            return true;
        }
        let activation = popup_activation_with_destination_policy(
            RendererPendingPopupActivation::window(
                root_document,
                source,
                !relations.suppress_opener,
                None,
                resolved_url.to_owned(),
                target_name.to_owned(),
                disposition,
            ),
            navigation_request.clone(),
            true,
        )
        .with_navigation_referrers(
            navigation_referrer,
            initial_document_referrer,
            document_referrer,
        )
        .with_resolved_target_page(resolved_target_page);
        runtime.record_pending_popup_activation(activation, None);
        return true;
    }
    if !source_javascript_url_allows_new_context_by_policy(
        scope,
        runtime,
        dispatch_scope,
        resolved_url,
    ) {
        return true;
    }
    let admission = match runtime.admit_new_auxiliary_browsing_context(creator.policy_container) {
        Ok(admission) => admission,
        Err(_) => return true,
    };
    let creation_user_activation = admission.user_activation();
    let auxiliary_browsing_context_policy = admission.renderer_auxiliary_browsing_context_policy();
    let creator_policy = admission.into_creation_policy();
    if relations.suppress_opener
        && (target_name.eq_ignore_ascii_case("_blank") || ordinary_target_name.is_some())
        && let Some(pending_auxiliary_page) = runtime.reserve_pending_auxiliary_page(false)
    {
        let new_target_disposition = if ordinary_target_name.is_some() {
            RendererPopupNewTargetDisposition::FreshNamed
        } else {
            RendererPopupNewTargetDisposition::FreshUnnamed
        };
        let destination_navigation_allowed =
            destination_policy.allows_navigation(scope, runtime, source_handle, resolved_url);
        let activation = popup_activation_with_destination_policy(
            RendererPendingPopupActivation::window(
                root_document,
                source,
                false,
                None,
                resolved_url.to_owned(),
                target_name.to_owned(),
                disposition,
            ),
            navigation_request.clone(),
            destination_navigation_allowed,
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
        runtime.record_pending_popup_activation(
            activation,
            Some(RendererPendingWindowOpenEvent::browser_window(
                resolved_url,
                target_name,
                creation_user_activation.user_gesture(),
            )),
        );
        return true;
    }
    let synchronous_creation_url = if destination_policy.requires_initial_empty_creation() {
        "about:blank"
    } else {
        resolved_url
    };
    let creator_policy_container = creator_policy.into_policy_container();
    let opened_popup = opener.and_then(|opener| {
        runtime.open_renderer_owned_related_auxiliary_page(
            scope,
            runtime_ptr,
            opener,
            None,
            target_name,
            synchronous_creation_url,
            creator.base_url.clone(),
            creator_policy_container.clone(),
        )
    });
    let Some(opened_popup) = opened_popup else {
        let destination_navigation_allowed =
            destination_policy.allows_navigation(scope, runtime, source_handle, resolved_url);
        let window_open_event = RendererPendingWindowOpenEvent::browser_window(
            resolved_url,
            target_name,
            creation_user_activation.user_gesture(),
        );
        let activation = popup_activation_with_destination_policy(
            RendererPendingPopupActivation::window(
                root_document,
                source,
                !relations.suppress_opener,
                None,
                resolved_url.to_owned(),
                target_name.to_owned(),
                disposition,
            ),
            navigation_request.clone(),
            destination_navigation_allowed,
        )
        .with_navigation_referrers(
            navigation_referrer,
            initial_document_referrer,
            document_referrer,
        )
        .with_initial_auxiliary_state(None, None)
        .with_creation_user_activation(creation_user_activation);
        runtime.record_pending_popup_activation(activation, Some(window_open_event));
        return true;
    };
    let popup_id = opened_popup.popup_id;
    let session_storage_store = Some(opened_popup.captured_session_storage_store.clone());
    let initial_empty_document_storage_key = Some(
        opened_popup
            .captured_initial_empty_document_storage_key
            .clone(),
    );
    let pending_auxiliary_page = opened_popup.pending_auxiliary_page;
    let new_target_disposition =
        (!relations.suppress_opener).then_some(RendererPopupNewTargetDisposition::Related);
    let window_open_event = RendererPendingWindowOpenEvent::browser_window(
        resolved_url,
        target_name,
        creation_user_activation.user_gesture(),
    );
    let destination_navigation_allowed =
        destination_policy.allows_navigation(scope, runtime, source_handle, resolved_url);
    let destination_queued = destination_navigation_allowed
        && queue_renderer_owned_top_level_navigation_for_window(
            scope,
            opened_popup.window,
            navigation_request.clone(),
        );
    if destination_navigation_allowed && !destination_queued {
        tracing::warn!(
            popup_id,
            "new related element target lost its staged renderer Page navigation owner"
        );
    }
    let activation = popup_activation_with_destination_policy(
        RendererPendingPopupActivation::window(
            root_document,
            source,
            !relations.suppress_opener,
            Some(popup_id),
            resolved_url.to_owned(),
            target_name.to_owned(),
            disposition,
        ),
        navigation_request,
        destination_navigation_allowed,
    );
    let activation = if destination_queued {
        activation.without_destination_navigation_with_requested_url_observation()
    } else {
        activation
    }
    .with_navigation_referrers(
        navigation_referrer,
        initial_document_referrer,
        document_referrer,
    )
    .with_initial_auxiliary_state(session_storage_store, initial_empty_document_storage_key)
    .with_pending_auxiliary_page(Some(pending_auxiliary_page));
    let activation = if let Some(disposition) = new_target_disposition {
        activation.with_new_target_disposition(disposition)
    } else {
        activation
    };
    let activation = activation.with_creation_user_activation(creation_user_activation);
    runtime.record_pending_popup_activation(activation, Some(window_open_event));
    true
}

fn dispatch_related_page_form_navigation_event<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    target_window: v8::Local<'s, v8::Object>,
    target_context: v8::Local<'s, v8::Context>,
    resolved_url: &str,
    event: FormPopupNavigationEvent<'s, '_>,
) -> bool {
    let target_scope = &mut v8::ContextScope::new(scope, target_context);
    let form_data = match event.form_data_entries {
        Some(entries) => {
            let Some(form_data) = crate::form_data_object_from_entries(target_scope, entries)
            else {
                return false;
            };
            Some(form_data)
        }
        None => None,
    };
    dispatch_cross_document_navigation_navigate_event_for_window_with_form_data(
        target_scope,
        target_window,
        resolved_url,
        event.source_element,
        event.user_initiated,
        None,
        form_data,
    )
}

pub(in crate::native_bridge) fn source_node_javascript_url_allowed_by_csp(
    scope: &mut v8::PinScope<'_, '_>,
    runtime_ptr: *mut JsContextHost,
    source_handle: DomHandle,
    resolved_url: &str,
) -> bool {
    let Ok(url) = url::Url::parse(resolved_url) else {
        return true;
    };
    if url.scheme() != "javascript" {
        return true;
    }
    let Some(owner) = (unsafe { &*runtime_ptr }).owner_dispatch_scope_for_node(source_handle)
    else {
        return false;
    };
    source_javascript_url_allowed_by_csp_for_owner(
        scope,
        unsafe { &mut *runtime_ptr },
        owner,
        resolved_url,
    )
}

pub(in crate::native_bridge) fn source_node_form_action_allowed_by_csp(
    scope: &mut v8::PinScope<'_, '_>,
    runtime_ptr: *mut JsContextHost,
    source_handle: DomHandle,
    resolved_url: &str,
) -> bool {
    source_node_form_action_allowed_by_csp_with_runtime(
        scope,
        unsafe { &mut *runtime_ptr },
        source_handle,
        resolved_url,
    )
}

fn source_node_form_action_allowed_by_csp_with_runtime(
    scope: &mut v8::PinScope<'_, '_>,
    runtime: &mut JsContextHost,
    source_handle: DomHandle,
    resolved_url: &str,
) -> bool {
    let Ok(request_url) = url::Url::parse(resolved_url) else {
        return true;
    };
    let Some(owner) = runtime.owner_dispatch_scope_for_node(source_handle) else {
        return false;
    };
    runtime.allows_form_action_by_csp(scope, owner, &request_url)
}

pub(crate) fn source_javascript_url_allowed_by_csp_for_owner(
    scope: &mut v8::PinScope<'_, '_>,
    runtime: &mut JsContextHost,
    owner: OwnerDispatchScope,
    resolved_url: &str,
) -> bool {
    let Ok(url) = url::Url::parse(resolved_url) else {
        return true;
    };
    if url.scheme() != "javascript" {
        return true;
    }
    let source = crate::native_bridge::javascript_url_csp_source(&url);
    runtime.allows_inline_javascript_navigation_by_csp(scope, owner, &source)
}

/// Blink only runs the source realm's full JavaScript-URL pre-navigation
/// check when target lookup missed and the navigation is about to create a new
/// auxiliary browsing context. Existing targets run source CSP after
/// selection, then defer Trusted Types entirely to their own execution realm.
pub(crate) fn source_javascript_url_allows_new_context_by_policy(
    scope: &mut v8::PinScope<'_, '_>,
    runtime: &mut JsContextHost,
    owner: OwnerDispatchScope,
    resolved_url: &str,
) -> bool {
    if !source_javascript_url_allowed_by_csp_for_owner(scope, runtime, owner, resolved_url) {
        return false;
    }
    let Ok(url) = url::Url::parse(resolved_url) else {
        return true;
    };
    if url.scheme() != "javascript" {
        return true;
    }
    let source = crate::native_bridge::javascript_url_source(&url);
    let requirements = runtime.trusted_types_for_script_requirements(scope);
    crate::context_bootstrap::trusted_script_string_for_javascript_navigation(
        scope,
        &source,
        requirements,
    )
    .is_some_and(|source| !source.is_empty())
}

fn browsing_context_window_for_dispatch_scope<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    runtime_ptr: *mut JsContextHost,
    dispatch_scope: crate::native_bridge::OwnerDispatchScope,
) -> Option<v8::Local<'s, v8::Object>> {
    let runtime = unsafe { &*runtime_ptr };
    match dispatch_scope {
        crate::native_bridge::OwnerDispatchScope::Top => {
            Some(scope.get_current_context().global(scope))
        }
        crate::native_bridge::OwnerDispatchScope::Child(handle) => {
            runtime.existing_child_browsing_context_window_wrapper(scope, handle)
        }
    }
}

fn browsing_context_dispatch_scope_for_node(
    scope: &mut v8::PinScope<'_, '_>,
    runtime_ptr: *mut JsContextHost,
    source_handle: DomHandle,
) -> Option<crate::native_bridge::OwnerDispatchScope> {
    let runtime = unsafe { &*runtime_ptr };
    let document = runtime.dom_host().owner_document_handle(source_handle)?;
    if document == runtime.document_handle() {
        return Some(crate::native_bridge::OwnerDispatchScope::Top);
    }
    runtime
        .child_browsing_context_handle_by_document_handle(scope, document)
        .map(crate::native_bridge::OwnerDispatchScope::Child)
}

pub(in crate::native_bridge) fn source_node_can_navigate_top_level(
    scope: &mut v8::PinScope<'_, '_>,
    runtime_ptr: *mut JsContextHost,
    source_handle: DomHandle,
    destination_url: &url::Url,
) -> bool {
    let Some(source_scope) =
        browsing_context_dispatch_scope_for_node(scope, runtime_ptr, source_handle)
    else {
        return false;
    };
    source_dispatch_scope_can_navigate_top_level(scope, runtime_ptr, source_scope, destination_url)
}

fn source_dispatch_scope_can_navigate_top_level(
    scope: &mut v8::PinScope<'_, '_>,
    runtime_ptr: *mut JsContextHost,
    source_scope: OwnerDispatchScope,
    destination_url: &url::Url,
) -> bool {
    source_dispatch_scope_can_navigate_target(
        scope,
        runtime_ptr,
        source_scope,
        runtime_ptr,
        OwnerDispatchScope::Top,
        destination_url,
    )
}

fn source_dispatch_scope_can_navigate_target(
    scope: &mut v8::PinScope<'_, '_>,
    source_host_ptr: *mut JsContextHost,
    source_scope: OwnerDispatchScope,
    target_host_ptr: *mut JsContextHost,
    target_scope: OwnerDispatchScope,
    destination_url: &url::Url,
) -> bool {
    let source_host = unsafe { &*source_host_ptr };
    let source_identity = if source_host.entered_owner_dispatch_scope(scope) == source_scope {
        source_host.current_runtime_window_execution_context_identity(scope)
    } else {
        source_host.current_registered_window_execution_context_identity(source_scope)
    };
    source_identity.is_some_and(|source_identity| {
        source_host
            .can_navigate_browsing_context(
                scope,
                source_identity,
                unsafe { &*target_host_ptr },
                target_scope,
                destination_url,
            )
            .is_ok()
    })
}

fn navigate_special_target_from_window<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    runtime_ptr: *mut JsContextHost,
    source_window: v8::Local<'s, v8::Object>,
    target: Option<SpecialBrowsingContextTarget>,
    resolved_url: &str,
    source: Option<crate::RendererTopLevelNavigationSource>,
) -> Option<v8::Local<'s, v8::Object>> {
    let target_window = special_target_window_from_source_window(scope, source_window, target)?;
    let global = scope.get_current_context().global(scope);
    let destination_url = url::Url::parse(resolved_url).ok()?;
    let source_scope = unsafe { &*runtime_ptr }.entered_owner_dispatch_scope(scope);
    let target_host_ptr =
        crate::native_bridge::cross_origin_window_target_host_ptr(scope, target_window)
            .or_else(|| context_host_ptr_from_window_object(scope, target_window));
    let target_scope =
        if crate::native_bridge::is_cross_origin_related_top_window_proxy(scope, target_window) {
            Some(OwnerDispatchScope::Top)
        } else {
            runtime_window_dispatch_scope(scope, target_window)
        };
    let target_can_navigate =
        target_host_ptr
            .zip(target_scope)
            .is_some_and(|(target_host_ptr, target_scope)| {
                source_dispatch_scope_can_navigate_target(
                    scope,
                    runtime_ptr,
                    source_scope,
                    target_host_ptr,
                    target_scope,
                    &destination_url,
                )
            });
    if !target_can_navigate {
        // Target selection succeeded even though CanNavigate refused the
        // navigation (or one endpoint went stale). Element/window.open callers
        // silently keep that existing target instead of surfacing Location's
        // synchronous sandbox exception or falling through to popup creation.
        return Some(target_window);
    }
    let navigated = if target_window.strict_equals(global.into()) {
        queue_top_level_location_navigation(scope, runtime_ptr, resolved_url, source)
    } else {
        navigate_target_window_location(scope, runtime_ptr, target_window, resolved_url, source)
    };
    if navigated { Some(target_window) } else { None }
}

fn special_target_window_from_source_window<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    source_window: v8::Local<'s, v8::Object>,
    target: Option<SpecialBrowsingContextTarget>,
) -> Option<v8::Local<'s, v8::Object>> {
    match target {
        None | Some(SpecialBrowsingContextTarget::Current) => source_window,
        Some(SpecialBrowsingContextTarget::Top) => source_window
            .get(scope, v8str(scope, "top").into())
            .and_then(|value| v8::Local::<v8::Object>::try_from(value).ok())?,
        Some(SpecialBrowsingContextTarget::Parent) => source_window
            .get(scope, v8str(scope, "parent").into())
            .and_then(|value| v8::Local::<v8::Object>::try_from(value).ok())?,
        Some(SpecialBrowsingContextTarget::Blank) => return None,
    }
    .into()
}

pub(crate) fn existing_browsing_context_target_window<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    runtime_ptr: *mut JsContextHost,
    target: SpecialBrowsingContextTarget,
) -> Option<v8::Local<'s, v8::Object>> {
    assert_ne!(target, SpecialBrowsingContextTarget::Blank);
    let dispatch_scope = unsafe { &*runtime_ptr }.entered_owner_dispatch_scope(scope);
    let source_window =
        browsing_context_window_for_dispatch_scope(scope, runtime_ptr, dispatch_scope)?;
    special_target_window_from_source_window(scope, source_window, Some(target))
}

pub(crate) fn navigate_existing_browsing_context_target<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    runtime_ptr: *mut JsContextHost,
    target: SpecialBrowsingContextTarget,
    resolved_url: &str,
    navigation_source: Option<crate::RendererTopLevelNavigationSource>,
) -> Option<v8::Local<'s, v8::Object>> {
    assert_ne!(
        target,
        SpecialBrowsingContextTarget::Blank,
        "a new-context target cannot use existing-context navigation"
    );
    let dispatch_scope = unsafe { &*runtime_ptr }.entered_owner_dispatch_scope(scope);
    let source_window =
        browsing_context_window_for_dispatch_scope(scope, runtime_ptr, dispatch_scope)?;
    navigate_special_target_from_window(
        scope,
        runtime_ptr,
        source_window,
        Some(target),
        resolved_url,
        navigation_source,
    )
}

pub(super) fn navigate_hyperlink_source_browsing_context(
    scope: &mut v8::PinScope<'_, '_>,
    runtime_ptr: *mut JsContextHost,
    source_handle: DomHandle,
    resolved_url: &str,
) -> bool {
    let Some(dispatch_scope) =
        browsing_context_dispatch_scope_for_node(scope, runtime_ptr, source_handle)
    else {
        return false;
    };
    match dispatch_scope {
        crate::native_bridge::OwnerDispatchScope::Top => false,
        crate::native_bridge::OwnerDispatchScope::Child(handle) => unsafe { &mut *runtime_ptr }
            .navigate_child_browsing_context_to_url(scope, handle, resolved_url),
    }
}

pub(crate) fn navigate_target_browsing_context<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    runtime_ptr: *mut JsContextHost,
    target_name: Option<&str>,
    resolved_url: &str,
    navigation_source: Option<crate::RendererTopLevelNavigationSource>,
    source_element: Option<v8::Local<'s, v8::Object>>,
    exposes_opener: bool,
) -> bool {
    let special_target = target_name.and_then(SpecialBrowsingContextTarget::parse);
    if target_name.is_none()
        || matches!(
            special_target,
            Some(
                SpecialBrowsingContextTarget::Current
                    | SpecialBrowsingContextTarget::Top
                    | SpecialBrowsingContextTarget::Parent
            )
        )
    {
        return match special_target {
            Some(target) => {
                let dispatch_scope = unsafe { &*runtime_ptr }.entered_owner_dispatch_scope(scope);
                let Some(source_window) =
                    browsing_context_window_for_dispatch_scope(scope, runtime_ptr, dispatch_scope)
                else {
                    return false;
                };
                navigate_special_target_from_window(
                    scope,
                    runtime_ptr,
                    source_window,
                    Some(target),
                    resolved_url,
                    navigation_source,
                )
                .is_some()
            }
            None => {
                let dispatch_scope = unsafe { &*runtime_ptr }.entered_owner_dispatch_scope(scope);
                let Some(source_window) =
                    browsing_context_window_for_dispatch_scope(scope, runtime_ptr, dispatch_scope)
                else {
                    return false;
                };
                navigate_special_target_from_window(
                    scope,
                    runtime_ptr,
                    source_window,
                    None,
                    resolved_url,
                    navigation_source,
                )
                .is_some()
            }
        };
    }
    if special_target == Some(SpecialBrowsingContextTarget::Blank) {
        return queue_popup_target_navigation(
            scope,
            runtime_ptr,
            "_blank",
            resolved_url,
            exposes_opener,
        );
    }
    let Some(target_name) = target_name else {
        unreachable!("missing target was handled as the source browsing context");
    };
    navigate_named_iframe_target(
        scope,
        runtime_ptr,
        target_name,
        resolved_url,
        source_element,
    ) || queue_popup_target_navigation(
        scope,
        runtime_ptr,
        target_name,
        resolved_url,
        exposes_opener,
    )
}

pub(in crate::native_bridge) fn navigate_hyperlink_target_browsing_context<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    runtime_ptr: *mut JsContextHost,
    source_handle: DomHandle,
    target_name: Option<&str>,
    resolved_url: &str,
    source_element: Option<v8::Local<'s, v8::Object>>,
    popup_disposition: RendererPopupDisposition,
) -> bool {
    let special_target = target_name.and_then(SpecialBrowsingContextTarget::parse);
    if special_target == Some(SpecialBrowsingContextTarget::Blank) {
        return navigate_hyperlink_popup_target(
            scope,
            runtime_ptr,
            source_handle,
            "_blank",
            resolved_url,
            popup_disposition,
            None,
        );
    }
    if let Some(target_name) = target_name
        && special_target.is_none()
    {
        let resolved_target =
            browsing_context_dispatch_scope_for_node(scope, runtime_ptr, source_handle).and_then(
                |source_scope| {
                    resolve_named_browsing_context_target_for_navigation(
                        scope,
                        runtime_ptr,
                        source_scope,
                        target_name,
                        resolved_url,
                    )
                },
            );
        if resolved_target.is_some()
            && !source_node_javascript_url_allowed_by_csp(
                scope,
                runtime_ptr,
                source_handle,
                resolved_url,
            )
        {
            return true;
        }
        if let Some(target) = resolved_target.as_ref()
            && target.related_top_level_page().is_none()
        {
            let relations =
                element_popup_relations(unsafe { &*runtime_ptr }, source_handle, target_name);
            let navigation_source = unsafe { &*runtime_ptr }
                .renderer_top_level_navigation_source_for_node(
                    source_handle,
                    relations.suppress_referrer,
                );
            return target.navigate_existing_context(
                scope,
                runtime_ptr,
                resolved_url,
                navigation_source,
                source_element,
            );
        }
        let resolved_related_target = resolved_target
            .as_ref()
            .and_then(NamedBrowsingContextNavigationTarget::related_top_level_target);
        return navigate_hyperlink_popup_target(
            scope,
            runtime_ptr,
            source_handle,
            target_name,
            resolved_url,
            popup_disposition,
            resolved_related_target,
        );
    }
    let Some(dispatch_scope) =
        browsing_context_dispatch_scope_for_node(scope, runtime_ptr, source_handle)
    else {
        return false;
    };
    let Some(source_window) =
        browsing_context_window_for_dispatch_scope(scope, runtime_ptr, dispatch_scope)
    else {
        return false;
    };
    let relations = element_popup_relations(
        unsafe { &*runtime_ptr },
        source_handle,
        target_name.unwrap_or("_self"),
    );
    let navigation_source = unsafe { &*runtime_ptr }
        .renderer_top_level_navigation_source_for_node(source_handle, relations.suppress_referrer);
    if !source_node_javascript_url_allowed_by_csp(scope, runtime_ptr, source_handle, resolved_url) {
        return true;
    }
    navigate_special_target_from_window(
        scope,
        runtime_ptr,
        source_window,
        special_target,
        resolved_url,
        navigation_source,
    )
    .is_some()
}

pub(crate) fn navigate_named_iframe_target<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    runtime_ptr: *mut JsContextHost,
    target_name: &str,
    resolved_url: &str,
    source_element: Option<v8::Local<'s, v8::Object>>,
) -> bool {
    navigate_named_iframe_target_from_document(
        scope,
        runtime_ptr,
        target_name,
        resolved_url,
        None,
        source_element,
    )
}

pub(in crate::native_bridge) fn named_iframe_target_handle_for_navigation(
    scope: &mut v8::PinScope<'_, '_>,
    runtime_ptr: *mut JsContextHost,
    target_name: &str,
    source_document: Option<DomHandle>,
) -> Option<DomHandle> {
    let runtime = unsafe { &mut *runtime_ptr };
    if let Some(document) = source_document
        && let Some(handle) = runtime
            .child_browsing_context_handle_by_name_for_navigation_from_document(
                scope,
                target_name,
                document,
            )
    {
        return Some(handle);
    }
    runtime.child_browsing_context_handle_by_name_for_navigation(scope, target_name)
}

pub(in crate::native_bridge) fn navigate_named_iframe_target_from_document<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    runtime_ptr: *mut JsContextHost,
    target_name: &str,
    resolved_url: &str,
    source_document: Option<DomHandle>,
    source_element: Option<v8::Local<'s, v8::Object>>,
) -> bool {
    let target_iframe =
        named_iframe_target_handle_for_navigation(scope, runtime_ptr, target_name, source_document);
    let Some(target_iframe) = target_iframe else {
        return false;
    };
    let runtime = unsafe { &mut *runtime_ptr };
    let target_url = url::Url::parse(resolved_url).ok();
    let target_is_same_origin_with_top = target_url
        .as_ref()
        .is_some_and(|url| moli_url::same_origin(runtime.document_url(), url));
    let target_is_same_document_with_child = target_url.as_ref().is_some_and(|url| {
        runtime
            .child_browsing_context_current_url(target_iframe)
            .is_some_and(|current| urls_refer_to_same_document(&current, url))
    });
    if ((target_is_same_origin_with_top
        && runtime.child_browsing_context_is_same_origin_with_top(target_iframe))
        || target_is_same_document_with_child)
        && let Some(window) =
            runtime.existing_child_browsing_context_window_wrapper(scope, target_iframe)
        && !dispatch_cross_document_navigation_navigate_event_for_window(
            scope,
            window,
            resolved_url,
            source_element,
            false,
            None,
        )
    {
        return true;
    }
    runtime.navigate_child_browsing_context_to_url(scope, target_iframe, resolved_url)
}

fn urls_refer_to_same_document(current: &url::Url, target: &url::Url) -> bool {
    let mut current = current.clone();
    current.set_fragment(None);
    let mut target = target.clone();
    target.set_fragment(None);
    current == target
}
