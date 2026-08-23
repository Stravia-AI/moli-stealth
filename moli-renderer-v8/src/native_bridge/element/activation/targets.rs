use crate::{
    RendererPendingPopupActivation, RendererPendingWindowOpenEvent,
    RendererPopupDisposition, RendererPopupNewTargetDisposition, RendererTopLevelNavigationRequest,
    context_bootstrap::{
        dispatch_cross_document_navigation_navigate_event_for_window,
        dispatch_cross_document_navigation_navigate_event_for_window_with_form_data,
    },
    document_runtime::{DocumentPolicyContainer, DomHandle},
    native_bridge::context_host::{
        ChildBrowsingContextNavigationRequest, FormSubmissionChildNavigationTarget,
        OwnerDispatchScope, PendingFormSubmissionChildNavigation, WindowExecutionContextIdentity,
    },
    util::{context_host_ptr_from_context_slot, v8str},
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
            | Self::RelatedPageChild { window, .. } => *window,
        }
    }

    pub(crate) fn related_top_level_page(&self) -> Option<crate::RendererResolvedPopupTarget> {
        match self {
            Self::RelatedTopLevel { page, .. } => Some(*page),
            Self::CurrentTopLevel { .. }
            | Self::CurrentPageChild { .. }
            | Self::RelatedPageChild { .. } => None,
        }
    }

    fn related_top_level_target(
        &self,
    ) -> Option<(
        v8::Local<'s, v8::Object>,
        v8::Local<'s, v8::Context>,
        crate::RendererResolvedPopupTarget,
    )> {
        match self {
            Self::RelatedTopLevel {
                window,
                target_context,
                page,
            } => Some((*window, *target_context, *page)),
            Self::CurrentTopLevel { .. }
            | Self::CurrentPageChild { .. }
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
            Self::RelatedTopLevel { .. } => false,
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
    if matches!(source_scope, OwnerDispatchScope::LightweightPopup(_)) {
        return None;
    }
    let source_identity = if source_host.entered_owner_dispatch_scope(scope) == source_scope {
        source_host.current_runtime_window_execution_context_identity(scope)
    } else {
        source_host.current_registered_window_execution_context_identity(source_scope)
    }?;
    let destination_is_javascript =
        url::Url::parse(destination_url).is_ok_and(|url| url.scheme() == "javascript");
    source_host.sync_child_browsing_context_subtree(scope, source_host.document_handle());
    let current_page_handles = source_host.child_browsing_context_handles_in_document_order();
    let top_level_targets = source_host.related_page_top_level_targets_for_navigation(scope);
    let source_top = top_level_targets
        .iter()
        .find(|(_, _, _, _, is_source)| *is_source);

    match source_scope {
        OwnerDispatchScope::Top => {
            if let Some((window, target_context, _, name, _)) = source_top
                && name == target_name
                && (!destination_is_javascript
                    || source_can_access_target_scope(
                        source_host_ptr,
                        source_identity,
                        source_host_ptr,
                        OwnerDispatchScope::Top,
                    ))
            {
                return Some(NamedBrowsingContextNavigationTarget::CurrentTopLevel {
                    window: *window,
                    target_context: *target_context,
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
                    destination_is_javascript,
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
                    destination_is_javascript,
                ) {
                    return Some(target);
                }
            }
            if let Some((window, target_context, _, name, _)) = source_top
                && name == target_name
                && (!destination_is_javascript
                    || source_can_access_target_scope(
                        source_host_ptr,
                        source_identity,
                        source_host_ptr,
                        OwnerDispatchScope::Top,
                    ))
            {
                return Some(NamedBrowsingContextNavigationTarget::CurrentTopLevel {
                    window: *window,
                    target_context: *target_context,
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
                    destination_is_javascript,
                ) {
                    return Some(target);
                }
            }
        }
        OwnerDispatchScope::LightweightPopup(_) => unreachable!(),
    }

    for (window, target_context, page, name, is_source) in top_level_targets {
        if is_source {
            continue;
        }
        let Some(target_host_ptr) = context_host_ptr_from_context_slot(target_context) else {
            continue;
        };
        if name == target_name
            && (!destination_is_javascript
                || source_can_access_target_scope(
                    source_host_ptr,
                    source_identity,
                    target_host_ptr,
                    OwnerDispatchScope::Top,
                ))
        {
            return Some(NamedBrowsingContextNavigationTarget::RelatedTopLevel {
                window,
                target_context,
                page,
            });
        }
        let target_host = unsafe { &mut *target_host_ptr };
        target_host.sync_child_browsing_context_subtree(scope, target_host.document_handle());
        let handles = target_host.child_browsing_context_handles_in_document_order();
        for handle in handles {
            if let Some(target) = resolve_child_navigation_candidate(
                scope,
                source_host_ptr,
                source_identity,
                target_host_ptr,
                handle,
                target_name,
                Some(page),
                destination_is_javascript,
            ) {
                return Some(target);
            }
        }
    }
    None
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
    destination_is_javascript: bool,
) -> Option<NamedBrowsingContextNavigationTarget<'s>> {
    let target_host = unsafe { &*target_host_ptr };
    if !target_host.child_browsing_context_matches_name_for_navigation(handle, target_name)
        || !source_can_navigate_child_target(
            source_host_ptr,
            source_identity,
            target_host_ptr,
            handle,
            destination_is_javascript,
        )
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

fn source_can_navigate_child_target(
    source_host_ptr: *mut JsContextHost,
    source_identity: WindowExecutionContextIdentity,
    target_host_ptr: *mut JsContextHost,
    handle: DomHandle,
    destination_is_javascript: bool,
) -> bool {
    if source_host_ptr == target_host_ptr
        && source_identity.dispatch_scope() == OwnerDispatchScope::Child(handle)
    {
        return true;
    }
    if destination_is_javascript {
        return source_can_access_target_scope(
            source_host_ptr,
            source_identity,
            target_host_ptr,
            OwnerDispatchScope::Child(handle),
        );
    }

    let target_host = unsafe { &*target_host_ptr };
    let mut target_scope = OwnerDispatchScope::Child(handle);
    loop {
        if source_can_access_target_scope(
            source_host_ptr,
            source_identity,
            target_host_ptr,
            target_scope,
        ) {
            return true;
        }
        target_scope = match target_scope {
            OwnerDispatchScope::Child(child) => target_host
                .child_browsing_context_parent_handle(child)
                .map(OwnerDispatchScope::Child)
                .unwrap_or(OwnerDispatchScope::Top),
            OwnerDispatchScope::Top => return false,
            OwnerDispatchScope::LightweightPopup(_) => return false,
        };
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
    let window_open_event = RendererPendingWindowOpenEvent::browser_window(
        resolved_url,
        target_name,
        runtime.protocol_user_gesture_activation(),
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
        .with_initial_auxiliary_state(None, None),
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
    if let Some(popup_id) = runtime.lightweight_popup_id_for_document_handle(document) {
        let policy_container = runtime
            .lightweight_popup_policy_container(popup_id)?
            .clone();
        return Some(ElementPopupCreator {
            opener: runtime.lightweight_popup_window(scope, popup_id)?,
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
    resolved_related_target: Option<(
        v8::Local<'s, v8::Object>,
        v8::Local<'s, v8::Context>,
        crate::RendererResolvedPopupTarget,
    )>,
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
            Some((window, target_context, page)),
            Some(event),
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
        ),
    }
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
        .queue_deferred_child_browsing_context_navigation_request(target_handle, request)
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
        };
        let _ = unsafe { &mut *target_host_ptr }
            .cancel_pending_form_submission_child_navigation_if_matches(
                scope,
                target.browsing_context_id(),
                pending.navigation_load(),
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
    if let Some(popup_id) = source_host.lightweight_popup_id_for_document_handle(source_document) {
        return source_host
            .lightweight_popup_policy_container(popup_id)
            .cloned();
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

fn navigate_element_popup_target<'s, 'entries>(
    scope: &mut v8::PinScope<'s, '_>,
    runtime_ptr: *mut JsContextHost,
    source_handle: DomHandle,
    target_name: &str,
    navigation_request: RendererTopLevelNavigationRequest,
    disposition: RendererPopupDisposition,
    resolved_related_target: Option<(
        v8::Local<'s, v8::Object>,
        v8::Local<'s, v8::Context>,
        crate::RendererResolvedPopupTarget,
    )>,
    form_navigation_event: Option<FormPopupNavigationEvent<'s, 'entries>>,
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
        let window_open_event = RendererPendingWindowOpenEvent::browser_window(
            resolved_url,
            target_name,
            runtime.protocol_user_gesture_activation(),
        );
        runtime.record_pending_popup_activation(
            RendererPendingPopupActivation::window(
                root_document,
                source,
                !relations.suppress_opener,
                None,
                resolved_url.to_owned(),
                target_name.to_owned(),
                disposition,
            )
            .with_navigation_request(navigation_request)
            .with_initial_auxiliary_state(None, None),
            Some(window_open_event),
        );
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
    let resolved_related_target = (target_url
        .as_ref()
        .is_some_and(|url| url.scheme() != "javascript")
        && ordinary_target_name.is_some())
    .then(|| {
        resolved_related_target.or_else(|| {
            runtime.related_page_named_target_for_navigation(
                scope,
                ordinary_target_name.expect("ordinary target name was checked"),
                None,
            )
        })
    })
    .flatten();
    if let Some((target_window, target_context, resolved_target_page)) = resolved_related_target {
        if let Some(form_navigation_event) = form_navigation_event
            && !dispatch_related_page_form_navigation_event(
                scope,
                target_window,
                target_context,
                resolved_url,
                form_navigation_event,
            )
        {
            return true;
        }
        runtime.record_pending_popup_activation(
            RendererPendingPopupActivation::window(
                root_document,
                source,
                !relations.suppress_opener,
                None,
                resolved_url.to_owned(),
                target_name.to_owned(),
                disposition,
            )
            .with_navigation_request(navigation_request.clone())
            .with_navigation_referrers(
                navigation_referrer,
                initial_document_referrer,
                document_referrer,
            )
            .with_resolved_target_page(resolved_target_page),
            None,
        );
        return true;
    }
    if relations.suppress_opener
        && (target_name.eq_ignore_ascii_case("_blank") || ordinary_target_name.is_some())
        && target_url
            .as_ref()
            .is_some_and(|url| url.scheme() != "javascript")
        && let Some(pending_auxiliary_page) = runtime.reserve_pending_auxiliary_page(false)
    {
        let new_target_disposition = if ordinary_target_name.is_some() {
            RendererPopupNewTargetDisposition::FreshNamed
        } else {
            RendererPopupNewTargetDisposition::FreshUnnamed
        };
        let user_gesture = runtime.protocol_user_gesture_activation();
        runtime.record_pending_popup_activation(
            RendererPendingPopupActivation::window(
                root_document,
                source,
                false,
                None,
                resolved_url.to_owned(),
                target_name.to_owned(),
                disposition,
            )
            .with_navigation_request(navigation_request.clone())
            .with_navigation_referrers(
                navigation_referrer,
                initial_document_referrer,
                document_referrer,
            )
            .with_pending_auxiliary_page(Some(pending_auxiliary_page))
            .with_new_target_disposition(new_target_disposition),
            Some(RendererPendingWindowOpenEvent::browser_window(
                resolved_url,
                target_name,
                user_gesture,
            )),
        );
        return true;
    }
    let Some(opened_popup) = runtime.open_lightweight_popup_window(
        scope,
        runtime_ptr,
        opener,
        None,
        target_name,
        resolved_url,
        Some(!relations.suppress_opener),
        true,
        creator.base_url,
        creator.policy_container,
    ) else {
        let window_open_event = RendererPendingWindowOpenEvent::browser_window(
            resolved_url,
            target_name,
            runtime.protocol_user_gesture_activation(),
        );
        runtime.record_pending_popup_activation(
            RendererPendingPopupActivation::window(
                root_document,
                source,
                !relations.suppress_opener,
                None,
                resolved_url.to_owned(),
                target_name.to_owned(),
                disposition,
            )
            .with_navigation_request(navigation_request.clone())
            .with_navigation_referrers(
                navigation_referrer,
                initial_document_referrer,
                document_referrer,
            )
            .with_initial_auxiliary_state(None, None),
            Some(window_open_event),
        );
        return true;
    };
    let popup_id = opened_popup.popup_id;
    let session_storage_store = opened_popup
        .captured_session_storage_store
        .clone()
        .or_else(|| runtime.lightweight_popup_session_storage_store(popup_id));
    let initial_empty_document_storage_key = opened_popup
        .captured_initial_empty_document_storage_key
        .clone()
        .or_else(|| runtime.lightweight_popup_initial_empty_document_storage_key(popup_id));
    let pending_auxiliary_page = opened_popup.pending_auxiliary_page;
    let new_target_disposition = (opened_popup.created_new_browsing_context
        && pending_auxiliary_page.is_some()
        && !relations.suppress_opener)
        .then_some(RendererPopupNewTargetDisposition::Related);
    let user_gesture = runtime.protocol_user_gesture_activation();
    let window_open_event = opened_popup.created_new_browsing_context.then(|| {
        RendererPendingWindowOpenEvent::browser_window(resolved_url, target_name, user_gesture)
    });
    let activation = RendererPendingPopupActivation::window(
        root_document,
        source,
        !relations.suppress_opener,
        Some(popup_id),
        resolved_url.to_owned(),
        target_name.to_owned(),
        disposition,
    )
    .with_navigation_request(navigation_request)
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
    runtime.record_pending_popup_activation(activation, window_open_event);
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

fn hyperlink_javascript_url_allowed_by_csp(
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
    let source = crate::native_bridge::javascript_url_csp_source(&url);
    unsafe { &mut *runtime_ptr }.allows_inline_javascript_navigation_by_csp(scope, owner, &source)
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
        crate::native_bridge::OwnerDispatchScope::LightweightPopup(popup_id) => {
            runtime.lightweight_popup_window(scope, popup_id)
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
    if let Some(popup_id) = runtime.lightweight_popup_id_for_document_handle(document) {
        return Some(crate::native_bridge::OwnerDispatchScope::LightweightPopup(
            popup_id,
        ));
    }
    runtime
        .child_browsing_context_handle_by_document_handle(scope, document)
        .map(crate::native_bridge::OwnerDispatchScope::Child)
}

fn navigate_special_target_from_window<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    runtime_ptr: *mut JsContextHost,
    source_window: v8::Local<'s, v8::Object>,
    target: Option<SpecialBrowsingContextTarget>,
    resolved_url: &str,
    source: Option<crate::RendererTopLevelNavigationSource>,
) -> Option<v8::Local<'s, v8::Object>> {
    let global = scope.get_current_context().global(scope);
    let target_window = match target {
        None | Some(SpecialBrowsingContextTarget::Current) => source_window,
        Some(SpecialBrowsingContextTarget::Top) => source_window
            .get(scope, v8str(scope, "top").into())
            .and_then(|value| v8::Local::<v8::Object>::try_from(value).ok())?,
        Some(SpecialBrowsingContextTarget::Parent) => source_window
            .get(scope, v8str(scope, "parent").into())
            .and_then(|value| v8::Local::<v8::Object>::try_from(value).ok())?,
        Some(SpecialBrowsingContextTarget::Blank) => return None,
    };
    let navigated = if target_window.strict_equals(global.into()) {
        queue_top_level_location_navigation(scope, runtime_ptr, resolved_url, source)
    } else {
        navigate_target_window_location(scope, runtime_ptr, target_window, resolved_url, source)
    };
    if navigated { Some(target_window) } else { None }
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
        crate::native_bridge::OwnerDispatchScope::LightweightPopup(popup_id) => {
            let Some(source_window) =
                unsafe { &*runtime_ptr }.lightweight_popup_window(scope, popup_id)
            else {
                return false;
            };
            navigate_special_target_from_window(
                scope,
                runtime_ptr,
                source_window,
                Some(SpecialBrowsingContextTarget::Current),
                resolved_url,
                unsafe { &*runtime_ptr }
                    .renderer_top_level_navigation_source_for_dispatch_scope(dispatch_scope, false),
            )
            .is_some()
        }
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
    if !hyperlink_javascript_url_allowed_by_csp(scope, runtime_ptr, source_handle, resolved_url) {
        return true;
    }
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
