use super::{
    super::{JsContextHost, PendingWindowMessageEndpoint, WindowExecutionContextIdentity},
    cross_origin_property_descriptor_map::realm_local_cross_origin_function,
};
use crate::{
    browsing_context_model::{BrowsingContextId, BrowsingContextKind},
    context_bootstrap::{
        CHILD_BROWSING_CONTEXT_HANDLE_SLOT, LocationNavigationKind, WINDOW_OPENER_SLOT,
        dispatch_cross_document_navigation_navigate_event_for_window,
        navigate_top_level_window_location_from_cross_origin,
    },
    definitions::define_get_set_property,
    document_runtime::DomHandle,
    document_script_scheduler::FrameDocumentClassicScriptSchedulerWork,
    native_bridge::{
        bridge::throw_dom_exception,
        child_window_surface::bind_materialized_child_window_indexed_db_factory,
        helpers::set_object_slot,
    },
    util::{
        context_host_ptr_from_context_slot, context_host_ptr_from_global_bridge, get_private_value,
        new_null_prototype_object, serialize_v8_array, serialize_v8_iter_array, set_null_prototype,
        set_private_value, throw_type_error, v8_string, v8str,
    },
    webidl, window_host,
};
use moli_webapi_declare::WebApiObject;
use std::{
    collections::{HashMap, HashSet},
    rc::Rc,
};

#[derive(Clone, Copy)]
struct ChildWindowProxyFacadeContextHandle(DomHandle);

#[derive(Default)]
pub(in crate::native_bridge::context_host) struct ChildWindowProxyRecords {
    records: HashMap<BrowsingContextId, ChildWindowProxyRecord>,
    context_ids_by_owner_handle: HashMap<DomHandle, BrowsingContextId>,
    detached_content_window_wrappers: HashMap<DomHandle, v8::Global<v8::Object>>,
    detached_content_document_wrappers: HashMap<DomHandle, v8::Global<v8::Object>>,
}

#[derive(Default)]
struct ChildWindowProxyRecord {
    live_window_wrapper: Option<v8::Global<v8::Object>>,
    facade_context: Option<v8::Global<v8::Context>>,
    browsing_context_parent_window: Option<v8::Global<v8::Object>>,
    browsing_context_top_window: Option<v8::Global<v8::Object>>,
    cross_origin_endpoint_projections:
        HashMap<PendingWindowMessageEndpoint, v8::Global<v8::Object>>,
    realm_top_window_wrapper: Option<v8::Global<v8::Object>>,
    // Once any Realm can retain this browsing context's WindowProxy, later
    // navigation must rebind that exact shell instead of replacing it.
    window_proxy_exposed: bool,
    cross_origin_window_proxy: Option<v8::Global<v8::Object>>,
    cross_origin_access_surface: Option<v8::Global<v8::Object>>,
    default_execution_context_id: Option<i64>,
}

impl ChildWindowProxyRecords {
    pub(in crate::native_bridge::context_host) fn bind_nested_browsing_context(
        &mut self,
        handle: DomHandle,
        browsing_context_id: BrowsingContextId,
    ) {
        assert_eq!(
            browsing_context_id.kind(),
            BrowsingContextKind::Nested,
            "child WindowProxy adapter requires a nested browsing context"
        );
        self.context_ids_by_owner_handle
            .retain(|bound_handle, bound_id| {
                *bound_handle == handle || *bound_id != browsing_context_id
            });
        let previous_id = self
            .context_ids_by_owner_handle
            .insert(handle, browsing_context_id);
        if let Some(previous_id) = previous_id
            && previous_id != browsing_context_id
            && !self.records.contains_key(&browsing_context_id)
            && let Some(record) = self.records.remove(&previous_id)
        {
            // Preserve the current child observable behavior when an owner
            // element is rebound while giving the record its authoritative
            // context-identity key.
            self.records.insert(browsing_context_id, record);
        }
    }

    fn context_id(&self, handle: DomHandle) -> Option<BrowsingContextId> {
        self.context_ids_by_owner_handle.get(&handle).copied()
    }

    fn record(&self, handle: DomHandle) -> Option<&ChildWindowProxyRecord> {
        self.records.get(&self.context_id(handle)?)
    }

    fn existing_record_mut(&mut self, handle: DomHandle) -> Option<&mut ChildWindowProxyRecord> {
        let context_id = self.context_id(handle)?;
        self.records.get_mut(&context_id)
    }

    fn record_mut(&mut self, handle: DomHandle) -> &mut ChildWindowProxyRecord {
        let context_id = self
            .context_id(handle)
            .expect("child WindowProxy materialization requires a bound browsing-context identity");
        self.records.entry(context_id).or_default()
    }

    pub(in crate::native_bridge::context_host) fn shell<'s>(
        &self,
        scope: &mut v8::PinScope<'s, '_, ()>,
        handle: DomHandle,
    ) -> Option<v8::Local<'s, v8::Object>> {
        let record = self.record(handle)?;
        record
            .live_window_wrapper
            .as_ref()
            .or(record.cross_origin_window_proxy.as_ref())
            .map(|window| v8::Local::new(scope, window))
    }

    pub(in crate::native_bridge::context_host) fn live_window<'s>(
        &self,
        scope: &mut v8::PinScope<'s, '_, ()>,
        handle: DomHandle,
    ) -> Option<v8::Local<'s, v8::Object>> {
        self.record(handle)?
            .live_window_wrapper
            .as_ref()
            .map(|window| v8::Local::new(scope, window))
    }

    pub(in crate::native_bridge::context_host) fn has_live_window(
        &self,
        handle: DomHandle,
    ) -> bool {
        self.record(handle)
            .is_some_and(|record| record.live_window_wrapper.is_some())
    }

    pub(in crate::native_bridge::context_host) fn set_facade_context(
        &mut self,
        handle: DomHandle,
        context: v8::Global<v8::Context>,
    ) {
        let record = self.record_mut(handle);
        record.facade_context = Some(context);
        // Endpoint projections are wrappers in the accessing child realm. A
        // replacement LocalWindow gets fresh wrappers even though the stable
        // target WindowProxy identities remain the same.
        record.cross_origin_endpoint_projections.clear();
    }

    pub(in crate::native_bridge::context_host) fn take_facade_context(
        &mut self,
        handle: DomHandle,
    ) -> Option<v8::Global<v8::Context>> {
        self.existing_record_mut(handle)?.facade_context.take()
    }

    pub(in crate::native_bridge::context_host) fn promote_shell_to_live(
        &mut self,
        scope: &mut v8::PinScope<'_, '_>,
        handle: DomHandle,
        window: v8::Local<'_, v8::Object>,
    ) {
        let record = self.record_mut(handle);
        record.cross_origin_window_proxy = None;
        record.cross_origin_access_surface = None;
        record.live_window_wrapper = Some(v8::Global::new(scope, window));
    }

    pub(in crate::native_bridge::context_host) fn set_browsing_context_parent_top(
        &mut self,
        scope: &mut v8::PinScope<'_, '_>,
        handle: DomHandle,
        parent: v8::Local<'_, v8::Object>,
        top: v8::Local<'_, v8::Object>,
    ) {
        let record = self.record_mut(handle);
        record
            .browsing_context_parent_window
            .get_or_insert_with(|| v8::Global::new(scope, parent));
        record
            .browsing_context_top_window
            .get_or_insert_with(|| v8::Global::new(scope, top));
    }

    pub(in crate::native_bridge::context_host) fn browsing_context_parent<'s>(
        &self,
        scope: &mut v8::PinScope<'s, '_, ()>,
        handle: DomHandle,
    ) -> Option<v8::Local<'s, v8::Object>> {
        self.record(handle)?
            .browsing_context_parent_window
            .as_ref()
            .map(|parent| v8::Local::new(scope, parent))
    }

    pub(in crate::native_bridge::context_host) fn browsing_context_top<'s>(
        &self,
        scope: &mut v8::PinScope<'s, '_, ()>,
        handle: DomHandle,
    ) -> Option<v8::Local<'s, v8::Object>> {
        self.record(handle)?
            .browsing_context_top_window
            .as_ref()
            .map(|top| v8::Local::new(scope, top))
    }

    fn cross_origin_endpoint_projection<'s>(
        &self,
        scope: &mut v8::PinScope<'s, '_>,
        handle: DomHandle,
        endpoint: PendingWindowMessageEndpoint,
    ) -> Option<v8::Local<'s, v8::Object>> {
        self.record(handle)?
            .cross_origin_endpoint_projections
            .get(&endpoint)
            .map(|projection| v8::Local::new(scope, projection))
    }

    fn set_cross_origin_endpoint_projection(
        &mut self,
        scope: &mut v8::PinScope<'_, '_>,
        handle: DomHandle,
        endpoint: PendingWindowMessageEndpoint,
        projection: v8::Local<'_, v8::Object>,
    ) {
        self.record_mut(handle)
            .cross_origin_endpoint_projections
            .insert(endpoint, v8::Global::new(scope, projection));
    }

    pub(in crate::native_bridge::context_host) fn set_realm_top(
        &mut self,
        scope: &mut v8::PinScope<'_, '_>,
        handle: DomHandle,
        top: v8::Local<'_, v8::Object>,
    ) {
        if moli_trace::window_message_trace_enabled() {
            let global = scope.get_current_context().global(scope);
            tracing::info!(
                target: "moli_window_message_trace",
                handle = handle.index(),
                top_is_target_global = top.strict_equals(global.into()),
                stage = "child_window_proxy_realm_top_installed",
            );
        }
        self.record_mut(handle).realm_top_window_wrapper = Some(v8::Global::new(scope, top));
    }

    pub(in crate::native_bridge::context_host) fn realm_top<'s>(
        &self,
        scope: &mut v8::PinScope<'s, '_>,
        handle: DomHandle,
    ) -> Option<v8::Local<'s, v8::Object>> {
        self.record(handle)?
            .realm_top_window_wrapper
            .as_ref()
            .map(|top| v8::Local::new(scope, top))
    }

    pub(in crate::native_bridge::context_host) fn mark_window_proxy_exposed(
        &mut self,
        handle: DomHandle,
    ) {
        self.record_mut(handle).window_proxy_exposed = true;
    }

    pub(in crate::native_bridge::context_host) fn window_proxy_exposed(
        &self,
        handle: DomHandle,
    ) -> bool {
        self.record(handle)
            .is_some_and(|record| record.window_proxy_exposed)
    }

    pub(in crate::native_bridge::context_host) fn cross_origin_proxy<'s>(
        &self,
        scope: &mut v8::PinScope<'s, '_>,
        handle: DomHandle,
    ) -> Option<v8::Local<'s, v8::Object>> {
        self.record(handle)?
            .cross_origin_window_proxy
            .as_ref()
            .map(|proxy| v8::Local::new(scope, proxy))
    }

    pub(in crate::native_bridge::context_host) fn has_cross_origin_proxy(
        &self,
        handle: DomHandle,
    ) -> bool {
        self.record(handle)
            .is_some_and(|record| record.cross_origin_window_proxy.is_some())
    }

    pub(in crate::native_bridge::context_host) fn set_cross_origin_proxy(
        &mut self,
        scope: &mut v8::PinScope<'_, '_>,
        handle: DomHandle,
        proxy: v8::Local<'_, v8::Object>,
    ) {
        self.record_mut(handle).cross_origin_window_proxy = Some(v8::Global::new(scope, proxy));
    }

    fn set_cross_origin_access_surface(
        &mut self,
        scope: &mut v8::PinScope<'_, '_>,
        handle: DomHandle,
        access_surface: v8::Local<'_, v8::Object>,
    ) {
        self.record_mut(handle).cross_origin_access_surface =
            Some(v8::Global::new(scope, access_surface));
    }

    fn cross_origin_handler_data<'s>(
        &self,
        scope: &mut v8::PinScope<'s, '_>,
        handle: DomHandle,
    ) -> Option<(v8::Local<'s, v8::Object>, v8::Local<'s, v8::Object>)> {
        let record = self.record(handle)?;
        let access_surface = record.cross_origin_access_surface.as_ref()?;
        let window_proxy = record
            .live_window_wrapper
            .as_ref()
            .or(record.cross_origin_window_proxy.as_ref())?;
        Some((
            v8::Local::new(scope, access_surface),
            v8::Local::new(scope, window_proxy),
        ))
    }

    pub(in crate::native_bridge::context_host) fn set_default_execution_context_id(
        &mut self,
        handle: DomHandle,
        execution_context_id: i64,
    ) {
        self.record_mut(handle).default_execution_context_id = Some(execution_context_id);
    }

    pub(in crate::native_bridge::context_host) fn clear_default_execution_context_id(
        &mut self,
        handle: DomHandle,
    ) {
        let Some(record) = self.existing_record_mut(handle) else {
            return;
        };
        record.default_execution_context_id = None;
        record.realm_top_window_wrapper = None;
    }

    pub(in crate::native_bridge::context_host) fn clear_default_execution_context_id_if_matches(
        &mut self,
        handle: DomHandle,
        expected_execution_context_id: i64,
    ) -> bool {
        // A replacement Document reuses the browsing-context handle. Its context
        // binding must survive delayed retirement of the previous LocalWindow.
        let Some(record) = self.existing_record_mut(handle) else {
            return false;
        };
        if record.default_execution_context_id != Some(expected_execution_context_id) {
            return false;
        }
        record.default_execution_context_id = None;
        record.realm_top_window_wrapper = None;
        true
    }

    pub(in crate::native_bridge::context_host) fn default_execution_context_id(
        &self,
        handle: DomHandle,
    ) -> Option<i64> {
        self.record(handle)?.default_execution_context_id
    }

    pub(in crate::native_bridge::context_host) fn clear_live_records(&mut self, handle: DomHandle) {
        if let Some(context_id) = self.context_ids_by_owner_handle.remove(&handle) {
            self.records.remove(&context_id);
        }
    }

    pub(in crate::native_bridge::context_host) fn retain_live_records(
        &mut self,
        live_handles: &HashSet<DomHandle>,
    ) {
        self.context_ids_by_owner_handle
            .retain(|handle, _| live_handles.contains(handle));
        let live_context_ids = self
            .context_ids_by_owner_handle
            .values()
            .copied()
            .collect::<HashSet<_>>();
        self.records
            .retain(|context_id, _| live_context_ids.contains(context_id));
    }

    pub(in crate::native_bridge::context_host) fn detached_content_document<'s>(
        &self,
        scope: &mut v8::PinScope<'s, '_>,
        handle: DomHandle,
    ) -> Option<v8::Local<'s, v8::Object>> {
        self.detached_content_document_wrappers
            .get(&handle)
            .map(|wrapper| v8::Local::new(scope, wrapper))
    }

    pub(in crate::native_bridge::context_host) fn set_detached_content_document(
        &mut self,
        scope: &mut v8::PinScope<'_, '_>,
        handle: DomHandle,
        document: v8::Local<'_, v8::Object>,
    ) {
        self.detached_content_document_wrappers
            .insert(handle, v8::Global::new(scope, document));
    }

    pub(in crate::native_bridge::context_host) fn detached_content_window<'s>(
        &self,
        scope: &mut v8::PinScope<'s, '_>,
        handle: DomHandle,
    ) -> Option<v8::Local<'s, v8::Object>> {
        self.detached_content_window_wrappers
            .get(&handle)
            .map(|wrapper| v8::Local::new(scope, wrapper))
    }

    pub(in crate::native_bridge::context_host) fn set_detached_content_window(
        &mut self,
        scope: &mut v8::PinScope<'_, '_>,
        handle: DomHandle,
        window: v8::Local<'_, v8::Object>,
    ) {
        self.detached_content_window_wrappers
            .insert(handle, v8::Global::new(scope, window));
    }

    pub(in crate::native_bridge::context_host) fn clear_detached_content_surfaces(
        &mut self,
        handle: DomHandle,
    ) {
        self.detached_content_window_wrappers.remove(&handle);
        self.detached_content_document_wrappers.remove(&handle);
    }
}

const CROSS_ORIGIN_WINDOW_LOCATION_SLOT: &str = "__moliCrossOriginWindowLocation";
const CROSS_ORIGIN_LOCATION_PROXY_SLOT: &str = "__moliCrossOriginLocationProxy";
const CROSS_ORIGIN_LOCATION_PROXY_SELF_SLOT: &str = "__moliCrossOriginLocationProxySelf";
const CROSS_ORIGIN_WINDOW_EXPOSED_PROPERTY_NAMES: &[&str] = &[
    "window",
    "self",
    "location",
    "close",
    "closed",
    "focus",
    "blur",
    "frames",
    "length",
    "top",
    "opener",
    "parent",
    "postMessage",
];
const DETACHED_CROSS_ORIGIN_WINDOW_PROXY_SLOT: &str = "__moliDetachedCrossOriginWindowProxy";
const CROSS_ORIGIN_TOP_WINDOW_PROXY_SLOT: &str = "__moliCrossOriginTopWindowProxy";
const CROSS_ORIGIN_RELATED_TOP_WINDOW_TARGET_SLOT: &str = "__moliCrossOriginRelatedTopWindowTarget";
const CLOSED_TOP_LEVEL_WINDOW_ACCESS_SURFACE_SLOT: &str = "__moliClosedTopLevelWindowAccessSurface";
const CLOSED_TOP_LEVEL_WINDOW_MARKER_SLOT: &str = "__moliClosedTopLevelWindow";
const CROSS_ORIGIN_LIGHTWEIGHT_POPUP_ID_SLOT: &str = "__moliCrossOriginLightweightPopupId";
const CROSS_ORIGIN_WINDOW_NAMED_CHILD_SLOTS: &str = "__moliCrossOriginWindowNamedChildSlots";

const CROSS_ORIGIN_ACCESS_ERROR: &str =
    "Blocked a frame with a different origin from accessing a cross-origin frame.";

const CROSS_ORIGIN_REALM_WINDOW_CLOSE_SLOT: &str = "__lmCrossOriginRealmWindowClose";
const CROSS_ORIGIN_REALM_WINDOW_FOCUS_SLOT: &str = "__lmCrossOriginRealmWindowFocus";
const CROSS_ORIGIN_REALM_WINDOW_BLUR_SLOT: &str = "__lmCrossOriginRealmWindowBlur";
const CROSS_ORIGIN_REALM_WINDOW_POST_MESSAGE_SLOT: &str = "__lmCrossOriginRealmWindowPostMessage";
const CROSS_ORIGIN_REALM_WINDOW_LOCATION_GETTER_SLOT: &str =
    "__lmCrossOriginRealmWindowLocationGetter";
const CROSS_ORIGIN_REALM_WINDOW_WINDOW_GETTER_SLOT: &str = "__lmCrossOriginRealmWindowWindowGetter";
const CROSS_ORIGIN_REALM_WINDOW_FRAMES_GETTER_SLOT: &str = "__lmCrossOriginRealmWindowFramesGetter";
const CROSS_ORIGIN_REALM_WINDOW_SELF_GETTER_SLOT: &str = "__lmCrossOriginRealmWindowSelfGetter";
const CROSS_ORIGIN_REALM_WINDOW_TOP_GETTER_SLOT: &str = "__lmCrossOriginRealmWindowTopGetter";
const CROSS_ORIGIN_REALM_WINDOW_PARENT_GETTER_SLOT: &str = "__lmCrossOriginRealmWindowParentGetter";
const CROSS_ORIGIN_REALM_WINDOW_OPENER_GETTER_SLOT: &str = "__lmCrossOriginRealmWindowOpenerGetter";
const CROSS_ORIGIN_REALM_WINDOW_CLOSED_GETTER_SLOT: &str = "__lmCrossOriginRealmWindowClosedGetter";
const CROSS_ORIGIN_REALM_WINDOW_LENGTH_GETTER_SLOT: &str = "__lmCrossOriginRealmWindowLengthGetter";
const CROSS_ORIGIN_REALM_WINDOW_LOCATION_SETTER_SLOT: &str =
    "__lmCrossOriginRealmWindowLocationSetter";
const CROSS_ORIGIN_REALM_LOCATION_REPLACE_SLOT: &str = "__lmCrossOriginRealmLocationReplace";
const CROSS_ORIGIN_REALM_LOCATION_HREF_SETTER_SLOT: &str = "__lmCrossOriginRealmLocationHrefSetter";

fn cross_origin_window_method_function<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    name: &str,
) -> Option<v8::Local<'s, v8::Function>> {
    match name {
        "close" => realm_local_cross_origin_function(
            scope,
            CROSS_ORIGIN_REALM_WINDOW_CLOSE_SLOT,
            "close",
            0,
            cross_origin_window_close_callback,
        ),
        "focus" => realm_local_cross_origin_function(
            scope,
            CROSS_ORIGIN_REALM_WINDOW_FOCUS_SLOT,
            "focus",
            0,
            cross_origin_window_noop_callback,
        ),
        "blur" => realm_local_cross_origin_function(
            scope,
            CROSS_ORIGIN_REALM_WINDOW_BLUR_SLOT,
            "blur",
            0,
            cross_origin_window_noop_callback,
        ),
        "postMessage" => realm_local_cross_origin_function(
            scope,
            CROSS_ORIGIN_REALM_WINDOW_POST_MESSAGE_SLOT,
            "postMessage",
            1,
            window_host::window_post_message_callback,
        ),
        _ => None,
    }
}

fn cross_origin_window_attribute_getter_function<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    name: &str,
) -> Option<v8::Local<'s, v8::Function>> {
    match name {
        "location" => realm_local_cross_origin_function(
            scope,
            CROSS_ORIGIN_REALM_WINDOW_LOCATION_GETTER_SLOT,
            "get location",
            0,
            cross_origin_window_location_attribute_getter_callback,
        ),
        "window" => realm_local_cross_origin_function(
            scope,
            CROSS_ORIGIN_REALM_WINDOW_WINDOW_GETTER_SLOT,
            "get window",
            0,
            cross_origin_window_window_attribute_getter_callback,
        ),
        "frames" => realm_local_cross_origin_function(
            scope,
            CROSS_ORIGIN_REALM_WINDOW_FRAMES_GETTER_SLOT,
            "get frames",
            0,
            cross_origin_window_frames_attribute_getter_callback,
        ),
        "self" => realm_local_cross_origin_function(
            scope,
            CROSS_ORIGIN_REALM_WINDOW_SELF_GETTER_SLOT,
            "get self",
            0,
            cross_origin_window_self_attribute_getter_callback,
        ),
        "top" => realm_local_cross_origin_function(
            scope,
            CROSS_ORIGIN_REALM_WINDOW_TOP_GETTER_SLOT,
            "get top",
            0,
            cross_origin_window_top_attribute_getter_callback,
        ),
        "parent" => realm_local_cross_origin_function(
            scope,
            CROSS_ORIGIN_REALM_WINDOW_PARENT_GETTER_SLOT,
            "get parent",
            0,
            cross_origin_window_parent_attribute_getter_callback,
        ),
        "opener" => realm_local_cross_origin_function(
            scope,
            CROSS_ORIGIN_REALM_WINDOW_OPENER_GETTER_SLOT,
            "get opener",
            0,
            cross_origin_window_opener_attribute_getter_callback,
        ),
        "closed" => realm_local_cross_origin_function(
            scope,
            CROSS_ORIGIN_REALM_WINDOW_CLOSED_GETTER_SLOT,
            "get closed",
            0,
            cross_origin_window_closed_attribute_getter_callback,
        ),
        "length" => realm_local_cross_origin_function(
            scope,
            CROSS_ORIGIN_REALM_WINDOW_LENGTH_GETTER_SLOT,
            "get length",
            0,
            cross_origin_window_length_attribute_getter_callback,
        ),
        _ => None,
    }
}

fn cross_origin_window_location_setter_function<'s>(
    scope: &mut v8::PinScope<'s, '_>,
) -> Option<v8::Local<'s, v8::Function>> {
    realm_local_cross_origin_function(
        scope,
        CROSS_ORIGIN_REALM_WINDOW_LOCATION_SETTER_SLOT,
        "set location",
        1,
        cross_origin_window_location_attribute_setter_callback,
    )
}

fn cross_origin_location_replace_function<'s>(
    scope: &mut v8::PinScope<'s, '_>,
) -> Option<v8::Local<'s, v8::Function>> {
    realm_local_cross_origin_function(
        scope,
        CROSS_ORIGIN_REALM_LOCATION_REPLACE_SLOT,
        "replace",
        1,
        cross_origin_location_replace_callback,
    )
}

fn cross_origin_location_href_setter_function<'s>(
    scope: &mut v8::PinScope<'s, '_>,
) -> Option<v8::Local<'s, v8::Function>> {
    realm_local_cross_origin_function(
        scope,
        CROSS_ORIGIN_REALM_LOCATION_HREF_SETTER_SLOT,
        "set href",
        1,
        cross_origin_location_href_attribute_setter_callback,
    )
}

fn cross_origin_window_attribute_descriptor<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    name: &str,
) -> Option<v8::Local<'s, v8::Object>> {
    let getter = cross_origin_window_attribute_getter_function(scope, name)?;
    let setter = if name == "location" {
        cross_origin_window_location_setter_function(scope)?.into()
    } else {
        v8::undefined(scope).into()
    };
    CrossOriginAccessorPropertyDescriptorDeclaration::new(getter.into(), setter, false, true)
        .bind(scope)
        .ok()
}

fn cross_origin_window_method_descriptor<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    name: &str,
) -> Option<v8::Local<'s, v8::Object>> {
    let function = cross_origin_window_method_function(scope, name)?;
    CrossOriginPropertyDescriptorDeclaration::new(function.into(), false, false, true)
        .bind(scope)
        .ok()
}

fn cross_origin_location_replace_descriptor<'s>(
    scope: &mut v8::PinScope<'s, '_>,
) -> Option<v8::Local<'s, v8::Object>> {
    let function = cross_origin_location_replace_function(scope)?;
    CrossOriginPropertyDescriptorDeclaration::new(function.into(), false, false, true)
        .bind(scope)
        .ok()
}

fn cross_origin_location_href_descriptor<'s>(
    scope: &mut v8::PinScope<'s, '_>,
) -> Option<v8::Local<'s, v8::Object>> {
    let setter = cross_origin_location_href_setter_function(scope)?;
    CrossOriginAccessorPropertyDescriptorDeclaration::new(
        v8::undefined(scope).into(),
        setter.into(),
        false,
        true,
    )
    .bind(scope)
    .ok()
}

pub(crate) fn install_child_window_proxy_access_check_handlers(
    global_template: v8::Local<'_, v8::ObjectTemplate>,
) {
    global_template.set_immutable_proto();
    global_template.set_security_token_access_check_and_handlers(
        window_access_check_callback,
        v8::NamedPropertyHandlerConfiguration::new()
            .getter(child_window_cross_origin_named_getter)
            .setter(child_window_cross_origin_named_setter)
            .query(child_window_cross_origin_named_query)
            .deleter(child_window_cross_origin_named_deleter)
            .enumerator(child_window_cross_origin_named_enumerator)
            .definer(child_window_cross_origin_named_definer)
            .descriptor(child_window_cross_origin_named_descriptor),
        v8::IndexedPropertyHandlerConfiguration::new()
            .getter(child_window_cross_origin_indexed_getter)
            .setter(child_window_cross_origin_indexed_setter)
            .query(child_window_cross_origin_indexed_query)
            .deleter(child_window_cross_origin_indexed_deleter)
            .enumerator(child_window_cross_origin_indexed_enumerator)
            .definer(child_window_cross_origin_indexed_definer)
            .descriptor(child_window_cross_origin_indexed_descriptor),
    );
}

unsafe extern "C" fn window_access_check_callback(
    accessing_context: v8::Local<'_, v8::Context>,
    accessed_object: v8::Local<'_, v8::Object>,
    _data: v8::Local<'_, v8::Value>,
) -> bool {
    let scope = std::pin::pin!(unsafe { v8::CallbackScope::new(accessed_object) });
    let scope = &mut scope.init();
    window_access_is_allowed(scope, accessing_context, accessed_object)
}

pub(super) fn window_access_is_allowed(
    scope: &mut v8::PinScope<'_, '_>,
    accessing_context: v8::Local<'_, v8::Context>,
    accessed_object: v8::Local<'_, v8::Object>,
) -> bool {
    let Some(accessed_context) = accessed_object.get_creation_context(scope) else {
        return false;
    };
    if crate::util::page_context_is_disconnected(accessing_context)
        || crate::util::page_context_is_disconnected(accessed_context)
    {
        return false;
    }
    if accessing_context == accessed_context {
        return true;
    }

    let Some((accessing_host_ptr, accessing_identity)) = (|| {
        let host_ptr = crate::util::context_host_ptr_from_context_slot(accessing_context)?;
        let identity = unsafe { &*host_ptr }
            .window_execution_context_identity_for_access_check(accessing_context)?;
        Some((host_ptr, identity))
    })() else {
        return false;
    };
    let Some((accessed_host_ptr, accessed_identity)) = (|| {
        let host_ptr = crate::util::context_host_ptr_from_context_slot(accessed_context)?;
        let identity = unsafe { &*host_ptr }
            .window_execution_context_identity_for_access_check(accessed_context)?;
        Some((host_ptr, identity))
    })() else {
        return false;
    };
    if accessing_host_ptr == accessed_host_ptr {
        let host = unsafe { &*accessing_host_ptr };
        return host.window_execution_context_can_access(accessing_identity, accessed_identity);
    }
    let accessing_host = unsafe { &*accessing_host_ptr };
    let accessed_host = unsafe { &*accessed_host_ptr };
    accessing_host.window_execution_context_can_access_related_page(
        accessing_identity,
        accessed_host,
        accessed_identity,
    )
}

impl JsContextHost {
    pub(in crate::native_bridge::context_host) fn inform_about_canceled_child_navigation_before_detach(
        &mut self,
        scope: &mut v8::PinScope<'_, '_>,
        handle: DomHandle,
    ) {
        let Some(window) = self.child_window_proxy_records.live_window(scope, handle) else {
            return;
        };
        let Some(context) = window.get_creation_context(scope) else {
            return;
        };
        let window = v8::Global::new(scope, window);
        let context = v8::Global::new(scope, context);
        let context = v8::Local::new(scope, &context);
        let child_scope = &mut v8::ContextScope::new(scope, context);
        let window = v8::Local::new(child_scope, &window);
        crate::context_bootstrap::inform_about_canceled_navigation_for_window(child_scope, window);
    }

    pub(in crate::native_bridge::context_host) fn refresh_child_window_access_surfaces_after_origin_mutation(
        &mut self,
        scope: &mut v8::PinScope<'_, '_>,
    ) {
        let handles = self.child_browsing_context_handles_in_document_order();
        for handle in handles {
            let dispatch_scope = super::super::OwnerDispatchScope::Child(handle);
            let Some(owner) = self.current_window_execution_context_owner(dispatch_scope) else {
                continue;
            };
            let Some((_, context)) = self.window_execution_context(scope, owner, dispatch_scope)
            else {
                continue;
            };
            let context = v8::Global::new(scope, context);
            let child_context = v8::Local::new(scope, &context);
            let child_scope = &mut v8::ContextScope::new(scope, child_context);
            let Some(window) = self
                .child_window_proxy_records
                .live_window(child_scope, handle)
            else {
                continue;
            };
            self.sync_child_browsing_context_window_parent_top_slots(child_scope, handle, window);
        }
    }
}

#[derive(webidl::WebIdlArgs)]
#[webidl(prefix = "Location.replace")]
struct CrossOriginLocationReplaceArgs {
    #[webidl(required, converter = "usv_string")]
    url: String,
}

#[derive(Default, WebApiObject)]
#[webapi(interface = "Object")]
struct CrossOriginWindowMethodsDeclaration {
    #[webapi(
        method,
        length = 1,
        callback = window_host::window_post_message_callback,
        readonly
    )]
    post_message: (),
    #[webapi(method, callback = cross_origin_window_noop_callback, readonly)]
    blur: (),
    #[webapi(method, callback = cross_origin_window_close_callback, readonly)]
    close: (),
    #[webapi(method, callback = cross_origin_window_noop_callback, readonly)]
    focus: (),
}

#[derive(WebApiObject)]
#[webapi(interface = "Object", data_properties, enumerable)]
struct CrossOriginPropertyDescriptorDeclaration<'scope> {
    value: v8::Local<'scope, v8::Value>,
    writable: bool,
    enumerable: bool,
    configurable: bool,
}

#[derive(WebApiObject)]
#[webapi(interface = "Object", data_properties, enumerable)]
struct CrossOriginAccessorPropertyDescriptorDeclaration<'scope> {
    get: v8::Local<'scope, v8::Value>,
    set: v8::Local<'scope, v8::Value>,
    enumerable: bool,
    configurable: bool,
}

#[derive(Default, WebApiObject)]
#[webapi(interface = "Object")]
struct CrossOriginLocationMethodsDeclaration {
    #[webapi(
        method,
        length = 1,
        callback = cross_origin_location_replace_callback,
        readonly
    )]
    replace: (),
}

#[derive(WebApiObject)]
#[webapi(interface = "Object")]
struct CrossOriginLocationProxyHandlerDeclaration {
    #[webapi(method, length = 3, callback = cross_origin_location_proxy_get_callback)]
    get: (),
    #[webapi(method, length = 2, callback = cross_origin_location_proxy_has_callback)]
    has: (),
    #[webapi(method, length = 4, callback = cross_origin_location_proxy_set_callback)]
    set: (),
    #[webapi(
        method,
        length = 2,
        callback = cross_origin_location_proxy_get_own_property_descriptor_callback
    )]
    get_own_property_descriptor: (),
    #[webapi(method, length = 2, callback = cross_origin_window_denied_callback)]
    delete_property: (),
    #[webapi(method, length = 3, callback = cross_origin_window_denied_callback)]
    define_property: (),
    #[webapi(
        method,
        length = 2,
        callback = cross_origin_location_proxy_set_prototype_of_callback
    )]
    set_prototype_of: (),
    #[webapi(
        method,
        length = 1,
        callback = cross_origin_location_proxy_prevent_extensions_callback
    )]
    prevent_extensions: (),
}

#[derive(WebApiObject)]
#[webapi(interface = "Object")]
struct CrossOriginWindowProxyHandlerDeclaration {
    #[webapi(method, length = 3, callback = cross_origin_window_proxy_get_callback)]
    get: (),
    #[webapi(method, length = 2, callback = cross_origin_window_proxy_has_callback)]
    has: (),
    #[webapi(method, length = 4, callback = cross_origin_window_proxy_set_callback)]
    set: (),
    #[webapi(
        method,
        length = 2,
        callback = cross_origin_window_proxy_get_own_property_descriptor_callback
    )]
    get_own_property_descriptor: (),
    #[webapi(
        method,
        length = 1,
        callback = cross_origin_window_proxy_own_keys_callback
    )]
    own_keys: (),
    #[webapi(method, length = 2, callback = cross_origin_window_denied_callback)]
    delete_property: (),
    #[webapi(method, length = 3, callback = cross_origin_window_denied_callback)]
    define_property: (),
    #[webapi(
        method,
        length = 2,
        callback = cross_origin_window_proxy_set_prototype_of_callback
    )]
    set_prototype_of: (),
    #[webapi(
        method,
        length = 1,
        callback = cross_origin_window_proxy_prevent_extensions_callback
    )]
    prevent_extensions: (),
}

#[derive(Default, WebApiObject)]
#[webapi(interface = "Object")]
struct CrossOriginWindowLiveAccessorsDeclaration {
    #[webapi(
        accessor_property,
        getter = cross_origin_window_closed_getter_callback,
        setter = cross_origin_window_denied_callback
    )]
    closed: (),
    #[webapi(
        accessor_property,
        getter = cross_origin_window_length_getter_callback,
        setter = cross_origin_window_denied_callback
    )]
    length: (),
    #[webapi(
        accessor_property,
        getter = cross_origin_window_location_getter_callback,
        setter = cross_origin_location_navigate_setter_callback
    )]
    location: (),
}

#[derive(Default, WebApiObject)]
#[webapi(interface = "Object")]
struct CrossOriginTopLevelWindowOpenerAccessorDeclaration {
    #[webapi(
        accessor_property,
        getter = cross_origin_top_level_window_opener_getter_callback,
        setter = cross_origin_window_denied_callback
    )]
    opener: (),
}

#[derive(Default, WebApiObject)]
#[webapi(interface = "Object")]
struct CrossOriginWindowLocationAccessorDeclaration {
    #[webapi(
        accessor_property,
        getter = cross_origin_window_location_getter_callback,
        setter = cross_origin_location_navigate_setter_callback
    )]
    location: (),
}

impl JsContextHost {
    pub(in crate::native_bridge::context_host) fn install_default_world_state_for_child_window(
        &mut self,
        scope: &mut v8::PinScope<'_, '_>,
        handle: DomHandle,
        window: v8::Local<'_, v8::Object>,
        document: v8::Local<'_, v8::Object>,
    ) {
        let _ = document;
        self.install_default_runtime_bindings_for_child_window(scope, handle, window);
    }

    pub(crate) fn request_child_frame_realm_materialization(&mut self, handle: DomHandle) {
        let Some(owner) = self
            .frame_owner_store
            .current_child_document_task_owner(handle)
        else {
            return;
        };
        let _ = self.request_child_frame_realm_materialization_for_owner(handle, owner);
    }

    pub(crate) fn request_child_frame_realm_materialization_for_owner(
        &mut self,
        handle: DomHandle,
        owner: crate::frame_owner_model::FrameDocumentTaskOwner,
    ) -> Option<crate::frame_owner_model::FrameRealmMaterializationRequest> {
        if self
            .frame_owner_store
            .current_child_document_task_owner(handle)
            != Some(owner)
        {
            return None;
        }
        self.frame_owner_store.ensure_child_realm(handle)?;
        if let Some(realm_id) = self
            .frame_owner_store
            .current_reserved_realm_id_for_document_task_owner(owner)
        {
            self.bind_pending_child_modulepreload_work_to_first_realm(handle, owner, realm_id);
        }
        let request = self
            .frame_owner_store
            .request_child_realm_materialization(handle, owner)?;
        if !matches!(
            request,
            crate::frame_owner_model::FrameRealmMaterializationRequest::NewlyQueued { .. }
        ) {
            return Some(request);
        }
        let target =
            crate::page_task_queue::RendererPageChildRealmMaterializationTarget::new(handle, owner);
        if self
            .page_child_frame_task_sender()
            .send_realm_materialization(target)
            .is_err()
        {
            let _ = self
                .frame_owner_store
                .rollback_child_realm_materialization_request(handle, owner, request.realm_id());
            return None;
        }
        Some(request)
    }

    pub(crate) fn child_current_document_is_initial_empty(&self, handle: DomHandle) -> bool {
        self.frame_owner_store
            .current_child_document_creation_kind(handle)
            .is_some_and(crate::frame_owner_model::DocumentCreationKind::is_initial_empty)
    }

    pub(crate) fn retire_child_frame_realm_materialization_request(
        &mut self,
        handle: DomHandle,
        owner: crate::frame_owner_model::FrameDocumentTaskOwner,
    ) -> bool {
        let retired = self
            .frame_owner_store
            .retire_child_realm_materialization_request(handle, owner);
        if retired {
            self.signal_page_child_realm_materialization_reconsideration_if_installed();
        }
        retired
    }

    pub(crate) fn has_child_frame_realm_materialization_request(
        &self,
        handle: DomHandle,
        owner: crate::frame_owner_model::FrameDocumentTaskOwner,
    ) -> bool {
        self.frame_owner_store
            .child_realm_materialization_is_queued(handle, owner)
    }

    pub(crate) fn has_pending_child_frame_realm_materialization(&self) -> bool {
        self.frame_owner_store
            .has_queued_child_realm_materialization()
    }

    pub(crate) fn fail_child_frame_realm_materialization(
        &mut self,
        handle: DomHandle,
        owner: crate::frame_owner_model::FrameDocumentTaskOwner,
    ) -> bool {
        let failed = self
            .frame_owner_store
            .fail_child_realm_materialization(handle, owner);
        if failed {
            self.child_window_proxy_records
                .clear_default_execution_context_id(handle);
        }
        failed
    }

    pub(in crate::native_bridge::context_host) fn clear_live_child_window_proxy_records(
        &mut self,
        handle: DomHandle,
    ) {
        self.child_window_proxy_records.clear_live_records(handle);
    }

    pub(in crate::native_bridge::context_host) fn retain_live_child_window_proxy_records(
        &mut self,
        live_handles: &HashSet<DomHandle>,
    ) {
        self.child_window_proxy_records
            .retain_live_records(live_handles);
    }

    pub(crate) fn take_child_window_proxy_shell_for_realm<'s>(
        &mut self,
        scope: &mut v8::PinScope<'s, '_, ()>,
        handle: DomHandle,
    ) -> Option<v8::Local<'s, v8::Object>> {
        let shell = self.child_window_proxy_records.shell(scope, handle)?;
        if let Some(context) = self.child_window_proxy_records.take_facade_context(handle) {
            v8::Local::new(scope, &context).detach_global();
        }
        Some(shell)
    }

    pub(crate) fn promote_child_window_proxy_shell_to_realm<'s>(
        &mut self,
        scope: &mut v8::PinScope<'s, '_>,
        handle: DomHandle,
        shell: v8::Local<'s, v8::Object>,
    ) {
        self.child_window_proxy_records
            .promote_shell_to_live(scope, handle, shell);
    }

    pub(crate) fn preserve_child_window_proxy_between_realms<'s>(
        &mut self,
        scope: &mut v8::PinScope<'s, '_, ()>,
        handle: DomHandle,
    ) -> bool {
        // A same-origin caller can retain functions from the retired inner
        // global. Keep that proxy detached until the replacement LocalWindow
        // takes it over so those closures continue to resolve against their
        // old inner global. Cross-origin callers can observe only the stable
        // WindowProxy whitelist, so park that proxy on a restricted facade.
        if self.top_window_can_access_child(handle) {
            return true;
        }
        let Some(window_proxy) = self.child_window_proxy_records.live_window(scope, handle) else {
            return false;
        };
        let Some(context) = self
            .bridge
            .bindings
            .attach_window_proxy_shell_to_facade(scope, window_proxy)
        else {
            return false;
        };
        crate::util::install_context_host_pointer_slot(
            context,
            self as *mut Self,
            self.context_host_liveness_handle(),
        );
        let previous = context.set_slot(Rc::new(ChildWindowProxyFacadeContextHandle(handle)));
        debug_assert!(previous.is_none());
        // Keep V8's unique default security token. A facade is not a real
        // LocalWindow realm, so no external context may bypass the access
        // handlers merely because it shares the pending document's origin.
        let facade_context = v8::Global::new(scope, context);
        let child_frame_count = self.child_browsing_context_child_frame_count(handle);
        let named_indices = self.child_browsing_context_child_frame_named_indices(handle);
        let parent = self
            .child_window_proxy_records
            .browsing_context_parent(scope, handle)
            .unwrap_or(window_proxy);
        let top = self
            .child_window_proxy_records
            .browsing_context_top(scope, handle)
            .unwrap_or(parent);

        let facade_scope = &mut v8::ContextScope::new(scope, context);
        let window_proxy = context.global(facade_scope);
        install_live_cross_origin_child_window_surface(
            facade_scope,
            window_proxy,
            handle,
            parent,
            top,
            window_proxy,
            child_frame_count,
            &named_indices,
        );
        let access_surface = new_null_prototype_object(facade_scope);
        install_live_cross_origin_child_window_surface(
            facade_scope,
            access_surface,
            handle,
            parent,
            top,
            window_proxy,
            child_frame_count,
            &named_indices,
        );
        self.child_window_proxy_records
            .set_cross_origin_access_surface(facade_scope, handle, access_surface);
        self.child_window_proxy_records
            .set_facade_context(handle, facade_context);
        true
    }

    pub(crate) fn child_window_proxy_shell_is_exposed(&self, handle: DomHandle) -> bool {
        self.child_window_proxy_records.window_proxy_exposed(handle)
    }

    pub(crate) fn install_child_window_proxy_cross_origin_access_surface<'s>(
        &mut self,
        scope: &mut v8::PinScope<'s, '_>,
        handle: DomHandle,
        window_proxy: v8::Local<'s, v8::Object>,
        realm_parent: v8::Local<'s, v8::Object>,
        realm_top: v8::Local<'s, v8::Object>,
    ) {
        // parent/top on a WindowProxy are browsing-context identities. Do not
        // replace them with the caller-specific projections installed on a
        // replacement inner global.
        let parent = self
            .child_window_proxy_records
            .browsing_context_parent(scope, handle)
            .unwrap_or(realm_parent);
        let top = self
            .child_window_proxy_records
            .browsing_context_top(scope, handle)
            .unwrap_or(realm_top);
        let surface = new_null_prototype_object(scope);
        // A live Document projects children through its authoritative registry
        // in the access handlers. Snapshot slots are reserved for facades that
        // have no current LocalWindow/Document owner.
        install_live_cross_origin_child_window_surface(
            scope,
            surface,
            handle,
            parent,
            top,
            window_proxy,
            0,
            &[],
        );
        self.child_window_proxy_records
            .set_cross_origin_access_surface(scope, handle, surface);
    }

    pub(crate) fn install_top_level_window_proxy_cross_origin_access_surface<'s>(
        &mut self,
        scope: &mut v8::PinScope<'s, '_>,
        window_proxy: v8::Local<'s, v8::Object>,
    ) {
        let surface = new_null_prototype_object(scope);
        let opener = get_private_value(scope, window_proxy, WINDOW_OPENER_SLOT)
            .unwrap_or_else(|| v8::null(scope).into());
        install_live_cross_origin_top_level_window_surface(scope, surface, window_proxy, opener);
        self.top_level_cross_origin_window_access_surface = Some(v8::Global::new(scope, surface));
    }

    /// Reattaches the stable top-level WindowProxy to a host-free facade after
    /// final Page discard.
    ///
    /// The facade deliberately retains only the cross-origin Window whitelist.
    /// Its access surface is stored on the V8 proxy itself so related Pages can
    /// keep observing the identity after this `JsContextHost` is destroyed.
    pub(crate) fn park_closed_top_level_window_proxy<'s>(
        &mut self,
        scope: &mut v8::PinScope<'s, '_, ()>,
        window_proxy: v8::Local<'s, v8::Object>,
        opener: v8::Local<'s, v8::Value>,
    ) -> bool {
        // Closing does not sever the auxiliary relationship. Chromium keeps
        // `closedWindow.opener` pointing at the original opener until an
        // explicit opener/group-severing policy says otherwise.
        let Some(context) = self
            .bridge
            .bindings
            .attach_window_proxy_shell_to_facade(scope, window_proxy)
        else {
            return false;
        };
        let facade_scope = &mut v8::ContextScope::new(scope, context);
        let window_proxy = context.global(facade_scope);
        let surface = new_null_prototype_object(facade_scope);
        install_live_cross_origin_top_level_window_surface(
            facade_scope,
            surface,
            window_proxy,
            opener,
        );
        let closed: v8::Local<'s, v8::Value> = v8::Boolean::new(facade_scope, true).into();
        set_private_value(
            facade_scope,
            surface,
            CLOSED_TOP_LEVEL_WINDOW_MARKER_SLOT,
            closed,
        );
        let closed: v8::Local<'s, v8::Value> = v8::Boolean::new(facade_scope, true).into();
        set_private_value(
            facade_scope,
            window_proxy,
            CLOSED_TOP_LEVEL_WINDOW_MARKER_SLOT,
            closed,
        );
        set_private_value(
            facade_scope,
            window_proxy,
            CLOSED_TOP_LEVEL_WINDOW_ACCESS_SURFACE_SLOT,
            surface.into(),
        );
        crate::util::disconnect_page_context(facade_scope);
        true
    }

    fn top_level_cross_origin_window_handler_data<'s>(
        &self,
        scope: &mut v8::PinScope<'s, '_>,
        window_proxy: v8::Local<'s, v8::Object>,
    ) -> Option<(v8::Local<'s, v8::Object>, v8::Local<'s, v8::Object>)> {
        let surface = self
            .top_level_cross_origin_window_access_surface
            .as_ref()
            .map(|surface| v8::Local::new(scope, surface))?;
        Some((surface, window_proxy))
    }

    pub(crate) fn child_browsing_context_window_proxy_for_top<'s>(
        &mut self,
        scope: &mut v8::PinScope<'s, '_>,
        handle: DomHandle,
    ) -> Option<v8::Local<'s, v8::Object>> {
        if self.child_browsing_context_is_same_origin_with_top(handle) {
            return self.child_browsing_context_window_wrapper(scope, handle);
        }
        if let Some(proxy) = self.child_window_proxy_records.live_window(scope, handle) {
            return Some(proxy);
        }
        if self.child_window_proxy_records.window_proxy_exposed(handle)
            && let Some(proxy) = self.ensure_top_exposed_cross_origin_window_proxy(scope, handle)
        {
            return Some(proxy);
        }
        self.child_browsing_context_cross_origin_window_proxy(scope, handle)
    }

    pub(crate) fn mark_child_browsing_context_window_proxy_exposed(&mut self, handle: DomHandle) {
        self.child_window_proxy_records
            .mark_window_proxy_exposed(handle);
    }

    pub(crate) fn child_browsing_context_cross_origin_window_proxy<'s>(
        &mut self,
        scope: &mut v8::PinScope<'s, '_>,
        handle: DomHandle,
    ) -> Option<v8::Local<'s, v8::Object>> {
        if let Some(work) = self.refresh_child_browsing_context(scope, handle) {
            self.push_child_document_script_ready_input(work);
        }
        let child_frame_count = self.child_browsing_context_child_frame_count(handle);
        if let Some(proxy) = self
            .child_window_proxy_records
            .cross_origin_proxy(scope, handle)
        {
            return Some(proxy);
        }
        if !self.child_browsing_contexts.contains_key(&handle) {
            return None;
        }

        let named_indices = self.child_browsing_context_child_frame_named_indices(handle);
        let (proxy, facade_context) = self.bridge.bindings.instantiate_window_proxy_shell(scope);
        let global = scope.get_current_context().global(scope);
        let top = self.child_browsing_context_root_window(scope, handle, global);
        let parent = self.child_browsing_context_parent_window(scope, handle, top);
        self.child_window_proxy_records
            .set_browsing_context_parent_top(scope, handle, parent, top);
        let facade_context_local = v8::Local::new(scope, &facade_context);
        crate::util::install_context_host_pointer_slot(
            facade_context_local,
            self as *mut Self,
            self.context_host_liveness_handle(),
        );
        let previous =
            facade_context_local.set_slot(Rc::new(ChildWindowProxyFacadeContextHandle(handle)));
        debug_assert!(previous.is_none());
        {
            let facade_scope = &mut v8::ContextScope::new(scope, facade_context_local);
            let facade_proxy = facade_context_local.global(facade_scope);
            debug_assert!(facade_proxy.strict_equals(proxy.into()));
            set_null_prototype(facade_scope, facade_proxy);
            install_live_cross_origin_child_window_surface(
                facade_scope,
                facade_proxy,
                handle,
                parent,
                top,
                facade_proxy,
                child_frame_count,
                &named_indices,
            );
            let access_surface = new_null_prototype_object(facade_scope);
            install_live_cross_origin_child_window_surface(
                facade_scope,
                access_surface,
                handle,
                parent,
                top,
                facade_proxy,
                child_frame_count,
                &named_indices,
            );
            self.child_window_proxy_records
                .set_cross_origin_access_surface(facade_scope, handle, access_surface);
            self.child_window_proxy_records.set_cross_origin_proxy(
                facade_scope,
                handle,
                facade_proxy,
            );
        }
        self.child_window_proxy_records
            .set_facade_context(handle, facade_context);
        self.child_window_proxy_records
            .cross_origin_proxy(scope, handle)
    }

    pub(crate) fn ensure_top_exposed_cross_origin_window_proxy<'s>(
        &mut self,
        scope: &mut v8::PinScope<'s, '_>,
        handle: DomHandle,
    ) -> Option<v8::Local<'s, v8::Object>> {
        if let Some(proxy) = self.child_window_proxy_records.live_window(scope, handle) {
            return Some(proxy);
        }
        self.child_browsing_context_cross_origin_window_proxy(scope, handle)
    }

    pub(crate) fn child_browsing_context_window_wrapper<'s>(
        &mut self,
        scope: &mut v8::PinScope<'s, '_>,
        handle: DomHandle,
    ) -> Option<v8::Local<'s, v8::Object>> {
        let (wrapper, ready_work) =
            self.child_browsing_context_window_wrapper_with_ready_work(scope, handle);
        for work in ready_work {
            self.push_child_document_script_ready_input(work);
        }
        wrapper
    }

    pub(crate) fn child_browsing_context_window_wrapper_with_ready_work<'s>(
        &mut self,
        scope: &mut v8::PinScope<'s, '_>,
        handle: DomHandle,
    ) -> (
        Option<v8::Local<'s, v8::Object>>,
        Vec<FrameDocumentClassicScriptSchedulerWork>,
    ) {
        self.child_browsing_context_window_wrapper_with_projection_authority(scope, handle, false)
    }

    fn child_browsing_context_window_wrapper_with_projection_authority<'s>(
        &mut self,
        scope: &mut v8::PinScope<'s, '_>,
        handle: DomHandle,
        may_promote_cross_origin_facade: bool,
    ) -> (
        Option<v8::Local<'s, v8::Object>>,
        Vec<FrameDocumentClassicScriptSchedulerWork>,
    ) {
        let mut ready_work = self
            .refresh_child_browsing_context(scope, handle)
            .into_iter()
            .collect::<Vec<_>>();
        if self.child_window_proxy_records.has_live_window(handle) {
            if self.child_default_execution_context_id(handle).is_none()
                && let Err(error) = self.ensure_prebootstrapped_child_default_context(scope, handle)
            {
                tracing::warn!(
                    %error,
                    child_handle = handle.index(),
                    "failed to attach the stable child WindowProxy to its current LocalWindow"
                );
                return (None, ready_work);
            }
            let (_, document_ready_work) =
                self.child_browsing_context_document_wrapper_with_ready_work(scope, handle);
            ready_work.extend(document_ready_work);
            let Some(wrapper) = self.child_window_proxy_records.live_window(scope, handle) else {
                return (None, ready_work);
            };
            self.sync_child_browsing_context_window_parent_top_slots(scope, handle, wrapper);
            bind_materialized_child_window_indexed_db_factory(scope, wrapper, handle);
            return (Some(wrapper), ready_work);
        }
        if self
            .child_window_proxy_records
            .has_cross_origin_proxy(handle)
            && !self.child_browsing_context_is_same_origin_with_top(handle)
            && !may_promote_cross_origin_facade
        {
            return (
                self.child_window_proxy_records
                    .cross_origin_proxy(scope, handle),
                ready_work,
            );
        }
        if !self.child_browsing_contexts.contains_key(&handle) {
            return (None, ready_work);
        }

        if let Err(error) = self.ensure_prebootstrapped_child_default_context(scope, handle) {
            tracing::warn!(
                %error,
                child_handle = handle.index(),
                "failed to bootstrap child LocalWindow context"
            );
            return (None, ready_work);
        }
        let (_, document_ready_work) =
            self.child_browsing_context_document_wrapper_with_ready_work(scope, handle);
        ready_work.extend(document_ready_work);
        let Some(wrapper) = self.child_window_proxy_records.live_window(scope, handle) else {
            return (None, ready_work);
        };
        self.sync_child_browsing_context_window_parent_top_slots(scope, handle, wrapper);
        bind_materialized_child_window_indexed_db_factory(scope, wrapper, handle);
        (Some(wrapper), ready_work)
    }

    fn child_browsing_context_window_wrapper_for_authorized_observer<'s>(
        &mut self,
        scope: &mut v8::PinScope<'s, '_>,
        handle: DomHandle,
    ) -> Option<v8::Local<'s, v8::Object>> {
        let (wrapper, ready_work) = self
            .child_browsing_context_window_wrapper_with_projection_authority(scope, handle, true);
        for work in ready_work {
            self.push_child_document_script_ready_input(work);
        }
        wrapper
    }

    pub(crate) fn existing_child_browsing_context_window_wrapper<'s>(
        &self,
        scope: &mut v8::PinScope<'s, '_>,
        handle: DomHandle,
    ) -> Option<v8::Local<'s, v8::Object>> {
        self.child_window_proxy_records.live_window(scope, handle)
    }

    pub(crate) fn child_browsing_context_top_window_for_current_realm<'s>(
        &self,
        scope: &mut v8::PinScope<'s, '_>,
        handle: DomHandle,
    ) -> Option<v8::Local<'s, v8::Object>> {
        self.child_window_proxy_records.realm_top(scope, handle)
    }

    pub(in crate::native_bridge::context_host) fn child_browsing_context_parent_window<'s>(
        &mut self,
        scope: &mut v8::PinScope<'s, '_>,
        handle: DomHandle,
        top: v8::Local<'s, v8::Object>,
    ) -> v8::Local<'s, v8::Object> {
        if let Some(window) =
            self.child_browsing_context_popup_owner_window_for_realm(scope, handle)
        {
            return window;
        }
        self.child_browsing_context_parent_handle(handle)
            .and_then(|parent| self.existing_child_browsing_context_window_wrapper(scope, parent))
            .unwrap_or(top)
    }

    pub(in crate::native_bridge::context_host) fn sync_child_browsing_context_window_parent_top_slots<
        's,
    >(
        &mut self,
        scope: &mut v8::PinScope<'s, '_>,
        handle: DomHandle,
        window: v8::Local<'s, v8::Object>,
    ) {
        let global = scope.get_current_context().global(scope);
        let same_origin_with_top = self.child_browsing_context_is_same_origin_with_top(handle);
        let existing_parent =
            Self::child_window_non_cross_origin_object_slot(scope, window, "__moliWindowParent");
        let existing_top =
            Self::child_window_non_cross_origin_object_slot(scope, window, "__moliWindowTop");
        let stable_parent = self
            .child_window_proxy_records
            .browsing_context_parent(scope, handle);
        let stable_top = self
            .child_window_proxy_records
            .browsing_context_top(scope, handle);
        let root = stable_top
            .or(existing_top)
            .filter(|top| !is_cross_origin_window_proxy(scope, *top))
            .unwrap_or_else(|| self.child_browsing_context_root_window(scope, handle, global));
        let popup_owner = self.child_browsing_context_popup_owner_window_for_realm(scope, handle);
        let top = if let Some(window) = popup_owner {
            window
        } else if same_origin_with_top {
            root
        } else {
            self.cross_origin_window_endpoint_projection_for_child(
                scope,
                handle,
                PendingWindowMessageEndpoint::TopWindow,
            )
            .unwrap_or(window)
        };
        let parent = if let Some(window) = popup_owner {
            window
        } else if same_origin_with_top {
            stable_parent
                .or(existing_parent)
                .unwrap_or_else(|| self.child_browsing_context_parent_window(scope, handle, top))
        } else {
            self.child_browsing_context_parent_window(scope, handle, top)
        };
        set_object_slot(scope, window, "__moliWindowParent", parent.into());
        set_object_slot(scope, window, "__moliWindowTop", top.into());
    }

    pub(in crate::native_bridge::context_host) fn child_window_object_slot<'s>(
        scope: &mut v8::PinScope<'s, '_>,
        window: v8::Local<'s, v8::Object>,
        name: &str,
    ) -> Option<v8::Local<'s, v8::Object>> {
        let key = v8_string(scope, name)?;
        window
            .get(scope, key.into())
            .and_then(|value| v8::Local::<v8::Object>::try_from(value).ok())
    }

    pub(in crate::native_bridge::context_host) fn child_window_non_cross_origin_object_slot<'s>(
        scope: &mut v8::PinScope<'s, '_>,
        window: v8::Local<'s, v8::Object>,
        name: &str,
    ) -> Option<v8::Local<'s, v8::Object>> {
        let object = Self::child_window_object_slot(scope, window, name)?;
        (!is_cross_origin_window_proxy(scope, object)).then_some(object)
    }

    pub(in crate::native_bridge::context_host) fn child_browsing_context_parent_top_for_realm_global<
        's,
    >(
        &mut self,
        scope: &mut v8::PinScope<'s, '_>,
        handle: DomHandle,
        global: v8::Local<'s, v8::Object>,
    ) -> (v8::Local<'s, v8::Object>, v8::Local<'s, v8::Object>) {
        let Some(window) = self.existing_child_browsing_context_window_wrapper(scope, handle)
        else {
            let top = self
                .child_window_proxy_records
                .browsing_context_top(scope, handle)
                .unwrap_or_else(|| self.child_browsing_context_root_window(scope, handle, global));
            let parent = self
                .child_window_proxy_records
                .browsing_context_parent(scope, handle)
                .unwrap_or_else(|| self.child_browsing_context_parent_window(scope, handle, top));
            return (parent, top);
        };

        if let Some(popup_owner) =
            self.child_browsing_context_popup_owner_window_for_realm(scope, handle)
        {
            return (popup_owner, popup_owner);
        }

        if self.child_browsing_context_is_same_origin_with_top(handle) {
            let top = self
                .child_window_proxy_records
                .browsing_context_top(scope, handle)
                .or_else(|| {
                    Self::child_window_non_cross_origin_object_slot(
                        scope,
                        window,
                        "__moliWindowTop",
                    )
                })
                .unwrap_or_else(|| self.child_browsing_context_root_window(scope, handle, global));
            let parent = self
                .child_window_proxy_records
                .browsing_context_parent(scope, handle)
                .or_else(|| {
                    Self::child_window_non_cross_origin_object_slot(
                        scope,
                        window,
                        "__moliWindowParent",
                    )
                })
                .unwrap_or_else(|| self.child_browsing_context_parent_window(scope, handle, top));
            return (parent, top);
        }

        let existing_top = Self::child_window_object_slot(scope, window, "__moliWindowTop");
        let top = existing_top
            .filter(|top| is_cross_origin_window_proxy(scope, *top))
            .or_else(|| {
                self.cross_origin_window_endpoint_projection_for_child(
                    scope,
                    handle,
                    PendingWindowMessageEndpoint::TopWindow,
                )
            })
            .unwrap_or(global);
        let parent = self.child_browsing_context_parent_window(scope, handle, top);
        (parent, top)
    }

    pub(in crate::native_bridge::context_host) fn child_browsing_context_root_window<'s>(
        &mut self,
        scope: &mut v8::PinScope<'s, '_>,
        handle: DomHandle,
        fallback: v8::Local<'s, v8::Object>,
    ) -> v8::Local<'s, v8::Object> {
        if let Some(window) =
            self.child_browsing_context_popup_owner_window_for_realm(scope, handle)
        {
            return window;
        }
        if let Some(parent_handle) = self.child_browsing_context_parent_handle(handle)
            && let Some(parent_top) = self
                .child_window_proxy_records
                .browsing_context_top(scope, parent_handle)
        {
            return parent_top;
        }
        let top_scope = super::super::OwnerDispatchScope::Top;
        if let Some(top_owner) = self.current_window_execution_context_owner(top_scope)
            && let Some((_, top_context)) =
                self.window_execution_context(scope, top_owner, top_scope)
        {
            return top_context.global(scope);
        }
        fallback
    }

    fn child_browsing_context_popup_owner_window_for_realm<'s>(
        &mut self,
        scope: &mut v8::PinScope<'s, '_>,
        handle: DomHandle,
    ) -> Option<v8::Local<'s, v8::Object>> {
        let popup_id = self.child_browsing_context_popup_owner_id(handle)?;
        if self.child_window_can_access_lightweight_popup(handle, popup_id) {
            return self.lightweight_popup_window(scope, popup_id);
        }
        self.cross_origin_window_endpoint_projection_for_child(
            scope,
            handle,
            PendingWindowMessageEndpoint::LightweightPopup(popup_id),
        )
    }

    fn cross_origin_window_endpoint_projection_for_child<'s>(
        &mut self,
        scope: &mut v8::PinScope<'s, '_>,
        accessing_handle: DomHandle,
        endpoint: PendingWindowMessageEndpoint,
    ) -> Option<v8::Local<'s, v8::Object>> {
        match endpoint {
            PendingWindowMessageEndpoint::TopWindow => {
                let top_scope = super::super::OwnerDispatchScope::Top;
                if let Some(top_owner) = self.current_window_execution_context_owner(top_scope)
                    && let Some((_, top_context)) =
                        self.window_execution_context(scope, top_owner, top_scope)
                {
                    return Some(top_context.global(scope));
                }
                if let Some(projection) = self
                    .child_window_proxy_records
                    .cross_origin_endpoint_projection(scope, accessing_handle, endpoint)
                {
                    return Some(projection);
                }
                let projection = build_cross_origin_top_window_proxy(scope);
                self.child_window_proxy_records
                    .set_cross_origin_endpoint_projection(
                        scope,
                        accessing_handle,
                        endpoint,
                        projection,
                    );
                let storage = cross_origin_proxy_storage_object(scope, projection);
                set_cross_origin_object_slot(scope, storage, "opener", v8::null(scope).into());
                Some(projection)
            }
            PendingWindowMessageEndpoint::ChildWindow(handle) => {
                self.child_browsing_context_cross_origin_window_proxy(scope, handle)
            }
            PendingWindowMessageEndpoint::LightweightPopup(popup_id) => {
                if let Some(projection) = self
                    .child_window_proxy_records
                    .cross_origin_endpoint_projection(scope, accessing_handle, endpoint)
                {
                    return Some(projection);
                }
                let projection = build_cross_origin_popup_window_proxy(scope, popup_id);
                self.child_window_proxy_records
                    .set_cross_origin_endpoint_projection(
                        scope,
                        accessing_handle,
                        endpoint,
                        projection,
                    );
                let opener = if let Some(opener_endpoint) =
                    self.lightweight_popup_opener_endpoint(popup_id)
                    && opener_endpoint != endpoint
                {
                    self.cross_origin_window_endpoint_projection_for_child(
                        scope,
                        accessing_handle,
                        opener_endpoint,
                    )
                    .map(v8::Local::<v8::Value>::from)
                    .unwrap_or_else(|| v8::null(scope).into())
                } else {
                    v8::null(scope).into()
                };
                let storage = cross_origin_proxy_storage_object(scope, projection);
                set_cross_origin_object_slot(scope, storage, "opener", opener);
                Some(projection)
            }
        }
    }
}

impl JsContextHost {
    pub(crate) fn child_performance_navigation_type(&self, handle: DomHandle) -> String {
        self.child_browsing_contexts
            .get(&handle)
            .map(|entry| entry.performance_navigation_type())
            .unwrap_or("navigate")
            .to_owned()
    }

    pub(crate) fn child_performance_time_origin(&self, handle: DomHandle) -> f64 {
        self.child_browsing_contexts
            .get(&handle)
            .map(|entry| entry.performance_time_origin_millis())
            .unwrap_or_else(moli_time::unix_epoch_millis)
    }
}

fn install_cross_origin_window_index_slots<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    window: v8::Local<'s, v8::Object>,
    count: usize,
    parent: v8::Local<'s, v8::Object>,
    top: v8::Local<'s, v8::Object>,
) {
    for index in 0..count.min(u32::MAX as usize) {
        let index_name = index.to_string();
        let Some(key) = v8_string(scope, &index_name) else {
            continue;
        };
        if window.has_own_property(scope, key.into()).unwrap_or(false) {
            continue;
        }
        let child = build_detached_cross_origin_window_index_proxy(scope, parent, top);
        let _ = window.define_own_property(
            scope,
            key.into(),
            child.into(),
            cross_origin_index_property_attributes(),
        );
    }
}

fn install_cross_origin_window_named_slots<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    window: v8::Local<'s, v8::Object>,
    named_indices: &[(usize, String)],
    parent: v8::Local<'s, v8::Object>,
    top: v8::Local<'s, v8::Object>,
) {
    let current_names = named_indices
        .iter()
        .filter_map(|(_, name)| {
            is_cross_origin_named_child_slot_name(name).then_some(name.as_str())
        })
        .collect::<std::collections::HashSet<_>>();
    remove_stale_cross_origin_window_named_slots(scope, window, &current_names);

    for (index, name) in named_indices {
        if !is_cross_origin_named_child_slot_name(name) {
            continue;
        }
        let Some(key) = v8_string(scope, name) else {
            continue;
        };
        if name != "then" && window.has_own_property(scope, key.into()).unwrap_or(false) {
            continue;
        }
        let value = window.get_index(scope, *index as u32).unwrap_or_else(|| {
            build_detached_cross_origin_window_index_proxy(scope, parent, top).into()
        });
        let _ = window.define_own_property(
            scope,
            key.into(),
            value,
            cross_origin_named_property_attributes(),
        );
    }
    set_cross_origin_window_named_slot_registry(scope, window, &current_names);
}

fn is_cross_origin_named_child_slot_name(name: &str) -> bool {
    cross_origin_named_child_can_shadow(name)
}

fn remove_stale_cross_origin_window_named_slots<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    window: v8::Local<'s, v8::Object>,
    current_names: &std::collections::HashSet<&str>,
) {
    let Some(previous_names) = cross_origin_window_named_slot_registry(scope, window) else {
        return;
    };
    for name in previous_names {
        if current_names.contains(name.as_str()) {
            continue;
        }
        let Some(key) = v8_string(scope, &name) else {
            continue;
        };
        if name == "then" {
            set_cross_origin_object_slot(scope, window, "then", v8::undefined(scope).into());
        } else {
            let _ = window.delete(scope, key.into());
        }
    }
}

fn cross_origin_window_named_slot_registry<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    window: v8::Local<'s, v8::Object>,
) -> Option<Vec<String>> {
    let value = get_private_value(scope, window, CROSS_ORIGIN_WINDOW_NAMED_CHILD_SLOTS)?;
    let array = v8::Local::<v8::Array>::try_from(value).ok()?;
    let mut names = Vec::new();
    for index in 0..array.length() {
        if let Some(name) = array
            .get_index(scope, index)
            .and_then(|value| value.to_string(scope))
        {
            names.push(name.to_rust_string_lossy(scope));
        }
    }
    Some(names)
}

fn set_cross_origin_window_named_slot_registry<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    window: v8::Local<'s, v8::Object>,
    names: &std::collections::HashSet<&str>,
) {
    let names = names.iter().copied().collect::<Vec<_>>();
    let array =
        serialize_v8_array(scope, names.as_slice()).unwrap_or_else(|| v8::Array::new(scope, 0));
    set_private_value(
        scope,
        window,
        CROSS_ORIGIN_WINDOW_NAMED_CHILD_SLOTS,
        array.into(),
    );
}

fn install_live_cross_origin_child_window_surface<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    window: v8::Local<'s, v8::Object>,
    handle: DomHandle,
    parent: v8::Local<'s, v8::Object>,
    top: v8::Local<'s, v8::Object>,
    indexed_parent: v8::Local<'s, v8::Object>,
    child_frame_count: usize,
    named_indices: &[(usize, String)],
) {
    install_cross_origin_window_identity_slots(scope, window, handle, parent, top);
    install_cross_origin_window_index_slots(scope, window, child_frame_count, indexed_parent, top);
    install_cross_origin_symbol_slots(scope, window, "Window");
    let location = build_cross_origin_location_proxy(scope, handle);
    set_private_value(
        scope,
        window,
        CROSS_ORIGIN_WINDOW_LOCATION_SLOT,
        location.into(),
    );
    CrossOriginWindowLiveAccessorsDeclaration::default()
        .initialize(scope, window)
        .expect("cross-origin Window accessors declaration should initialize");
    set_cross_origin_object_slot(scope, window, "opener", v8::null(scope).into());
    set_cross_origin_object_slot(scope, window, "then", v8::undefined(scope).into());
    install_cross_origin_window_methods(scope, window);
    install_cross_origin_window_named_slots(scope, window, named_indices, indexed_parent, top);
}

fn install_live_cross_origin_top_level_window_surface<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    surface: v8::Local<'s, v8::Object>,
    window_proxy: v8::Local<'s, v8::Object>,
    opener: v8::Local<'s, v8::Value>,
) {
    let target_marker: v8::Local<'s, v8::Value> = window_proxy.into();
    set_private_value(
        scope,
        surface,
        CROSS_ORIGIN_RELATED_TOP_WINDOW_TARGET_SLOT,
        target_marker,
    );
    set_private_value(
        scope,
        window_proxy,
        CROSS_ORIGIN_RELATED_TOP_WINDOW_TARGET_SLOT,
        target_marker,
    );
    set_cross_origin_object_slot(scope, surface, "self", window_proxy.into());
    set_cross_origin_object_slot(scope, surface, "window", window_proxy.into());
    set_cross_origin_object_slot(scope, surface, "parent", window_proxy.into());
    set_cross_origin_object_slot(scope, surface, "top", window_proxy.into());
    set_cross_origin_object_slot(scope, surface, "frames", window_proxy.into());
    // Live top-level indexed and named access is projected directly from the
    // target Page's child registry by the WindowProxy handlers below. Keeping
    // snapshots on this surface would leave stale indices after removal and
    // incorrectly expose named children through [[OwnPropertyKeys]].
    install_cross_origin_symbol_slots(scope, surface, "Window");
    let location = build_detached_cross_origin_location_proxy(scope);
    let location_storage = cross_origin_proxy_storage_object(scope, location);
    set_private_value(
        scope,
        location_storage,
        CROSS_ORIGIN_RELATED_TOP_WINDOW_TARGET_SLOT,
        window_proxy.into(),
    );
    set_private_value(
        scope,
        surface,
        CROSS_ORIGIN_WINDOW_LOCATION_SLOT,
        location.into(),
    );
    set_private_value(
        scope,
        window_proxy,
        CROSS_ORIGIN_WINDOW_LOCATION_SLOT,
        location.into(),
    );
    CrossOriginWindowLiveAccessorsDeclaration::default()
        .initialize(scope, surface)
        .expect("cross-origin top-level Window accessors declaration should initialize");
    set_private_value(scope, surface, WINDOW_OPENER_SLOT, opener);
    CrossOriginTopLevelWindowOpenerAccessorDeclaration::default()
        .initialize(scope, surface)
        .expect("cross-origin top-level Window opener accessor should initialize");
    set_cross_origin_object_slot(scope, surface, "then", v8::undefined(scope).into());
    install_cross_origin_window_methods(scope, surface);
}

fn build_detached_cross_origin_window_index_proxy<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    parent: v8::Local<'s, v8::Object>,
    top: v8::Local<'s, v8::Object>,
) -> v8::Local<'s, v8::Object> {
    let window = new_null_prototype_object(scope);
    set_private_value(
        scope,
        window,
        DETACHED_CROSS_ORIGIN_WINDOW_PROXY_SLOT,
        v8::Boolean::new(scope, true).into(),
    );
    set_cross_origin_object_slot(scope, window, "parent", parent.into());
    set_cross_origin_object_slot(scope, window, "top", top.into());
    set_cross_origin_object_slot(
        scope,
        window,
        "closed",
        v8::Boolean::new(scope, false).into(),
    );
    set_cross_origin_object_slot(scope, window, "opener", v8::null(scope).into());
    set_cross_origin_object_slot(scope, window, "then", v8::undefined(scope).into());
    set_cross_origin_object_slot(scope, window, "length", v8::Number::new(scope, 0.0).into());
    let location = build_detached_cross_origin_location_proxy(scope);
    set_private_value(
        scope,
        window,
        CROSS_ORIGIN_WINDOW_LOCATION_SLOT,
        location.into(),
    );
    CrossOriginWindowLocationAccessorDeclaration::default()
        .initialize(scope, window)
        .expect("cross-origin Window location accessor declaration should initialize");
    install_cross_origin_window_methods(scope, window);
    install_cross_origin_symbol_slots(scope, window, "Window");
    let Some(proxy) = wrap_detached_cross_origin_window(scope, window) else {
        return window;
    };
    set_cross_origin_object_slot(scope, window, "self", proxy.into());
    set_cross_origin_object_slot(scope, window, "window", proxy.into());
    set_cross_origin_object_slot(scope, window, "frames", proxy.into());
    proxy
}

fn build_cross_origin_top_window_proxy<'s>(
    scope: &mut v8::PinScope<'s, '_>,
) -> v8::Local<'s, v8::Object> {
    build_cross_origin_top_level_window_proxy(scope, PendingWindowMessageEndpoint::TopWindow)
}

fn build_cross_origin_popup_window_proxy<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    popup_id: u64,
) -> v8::Local<'s, v8::Object> {
    build_cross_origin_top_level_window_proxy(
        scope,
        PendingWindowMessageEndpoint::LightweightPopup(popup_id),
    )
}

fn build_cross_origin_top_level_window_proxy<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    endpoint: PendingWindowMessageEndpoint,
) -> v8::Local<'s, v8::Object> {
    let window = new_null_prototype_object(scope);
    match endpoint {
        PendingWindowMessageEndpoint::TopWindow => set_private_value(
            scope,
            window,
            window_host::TOP_WINDOW_MESSAGE_ENDPOINT_SLOT,
            v8::Boolean::new(scope, true).into(),
        ),
        PendingWindowMessageEndpoint::LightweightPopup(popup_id) => set_private_value(
            scope,
            window,
            CROSS_ORIGIN_LIGHTWEIGHT_POPUP_ID_SLOT,
            v8::BigInt::new_from_u64(scope, popup_id).into(),
        ),
        PendingWindowMessageEndpoint::ChildWindow(_) => {
            unreachable!("top-level Window projection cannot target a child frame")
        }
    }
    set_private_value(
        scope,
        window,
        DETACHED_CROSS_ORIGIN_WINDOW_PROXY_SLOT,
        v8::Boolean::new(scope, true).into(),
    );
    set_private_value(
        scope,
        window,
        CROSS_ORIGIN_TOP_WINDOW_PROXY_SLOT,
        v8::Boolean::new(scope, true).into(),
    );
    set_cross_origin_object_slot(scope, window, "then", v8::undefined(scope).into());
    let location = build_detached_cross_origin_location_proxy(scope);
    set_private_value(
        scope,
        window,
        CROSS_ORIGIN_WINDOW_LOCATION_SLOT,
        location.into(),
    );
    CrossOriginWindowLiveAccessorsDeclaration::default()
        .initialize(scope, window)
        .expect("cross-origin top Window accessors declaration should initialize");
    install_cross_origin_window_methods(scope, window);
    install_cross_origin_symbol_slots(scope, window, "Window");
    let Some(proxy) = wrap_detached_cross_origin_window(scope, window) else {
        return window;
    };
    set_cross_origin_window_self_identity_slots(scope, window, proxy);
    proxy
}

fn is_cross_origin_window_proxy<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    object: v8::Local<'s, v8::Object>,
) -> bool {
    get_cross_origin_proxy_private_value(scope, object, CROSS_ORIGIN_WINDOW_LOCATION_SLOT).is_some()
        || get_cross_origin_proxy_private_value(
            scope,
            object,
            DETACHED_CROSS_ORIGIN_WINDOW_PROXY_SLOT,
        )
        .is_some()
        || get_cross_origin_proxy_private_value(scope, object, CROSS_ORIGIN_TOP_WINDOW_PROXY_SLOT)
            .is_some()
}

fn cross_origin_proxy_storage_object<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    object: v8::Local<'s, v8::Object>,
) -> v8::Local<'s, v8::Object> {
    let value: v8::Local<'s, v8::Value> = object.into();
    let Ok(proxy) = v8::Local::<v8::Proxy>::try_from(value) else {
        return object;
    };
    let Ok(target) = v8::Local::<v8::Object>::try_from(proxy.get_target(scope)) else {
        return object;
    };
    let is_moli_cross_origin_proxy =
        get_private_value(scope, target, CROSS_ORIGIN_LOCATION_PROXY_SLOT).is_some()
            || get_private_value(scope, target, DETACHED_CROSS_ORIGIN_WINDOW_PROXY_SLOT).is_some()
            || get_private_value(scope, target, CROSS_ORIGIN_TOP_WINDOW_PROXY_SLOT).is_some();
    if is_moli_cross_origin_proxy {
        target
    } else {
        object
    }
}

fn get_cross_origin_proxy_private_value<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    object: v8::Local<'s, v8::Object>,
    slot: &str,
) -> Option<v8::Local<'s, v8::Value>> {
    let storage = cross_origin_proxy_storage_object(scope, object);
    get_private_value(scope, storage, slot)
}

fn set_cross_origin_window_self_identity_slots(
    scope: &mut v8::PinScope<'_, '_>,
    window: v8::Local<'_, v8::Object>,
    identity: v8::Local<'_, v8::Object>,
) {
    set_cross_origin_object_slot(scope, window, "self", identity.into());
    set_cross_origin_object_slot(scope, window, "window", identity.into());
    set_cross_origin_object_slot(scope, window, "parent", identity.into());
    set_cross_origin_object_slot(scope, window, "top", identity.into());
    set_cross_origin_object_slot(scope, window, "frames", identity.into());
}

fn wrap_detached_cross_origin_window<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    target: v8::Local<'s, v8::Object>,
) -> Option<v8::Local<'s, v8::Object>> {
    let handler = CrossOriginWindowProxyHandlerDeclaration {
        get: (),
        has: (),
        set: (),
        get_own_property_descriptor: (),
        own_keys: (),
        delete_property: (),
        define_property: (),
        set_prototype_of: (),
        prevent_extensions: (),
    }
    .bind(scope)
    .ok()?;
    let proxy = v8::Proxy::new(scope, target, handler)?;
    let proxy: v8::Local<'s, v8::Value> = proxy.into();
    v8::Local::<v8::Object>::try_from(proxy).ok()
}

fn install_cross_origin_window_methods<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    object: v8::Local<'s, v8::Object>,
) {
    CrossOriginWindowMethodsDeclaration::default()
        .initialize(scope, object)
        .expect("cross-origin Window methods declaration should initialize");
}

fn install_cross_origin_window_identity_slots(
    scope: &mut v8::PinScope<'_, '_>,
    window: v8::Local<'_, v8::Object>,
    handle: DomHandle,
    parent: v8::Local<'_, v8::Object>,
    top: v8::Local<'_, v8::Object>,
) {
    let handle_value = v8::Number::new(scope, handle.index() as f64);
    set_private_value(
        scope,
        window,
        CHILD_BROWSING_CONTEXT_HANDLE_SLOT,
        handle_value.into(),
    );
    set_cross_origin_object_slot(scope, window, "self", window.into());
    set_cross_origin_object_slot(scope, window, "window", window.into());
    set_cross_origin_object_slot(scope, window, "parent", parent.into());
    set_cross_origin_object_slot(scope, window, "top", top.into());
    set_cross_origin_object_slot(scope, window, "frames", window.into());
}

pub(in crate::native_bridge::context_host::child_frame_runtime) fn install_child_window_identity_slots<
    's,
>(
    scope: &mut v8::PinScope<'s, '_>,
    window: v8::Local<'s, v8::Object>,
    handle: DomHandle,
    parent: v8::Local<'s, v8::Object>,
    top: v8::Local<'s, v8::Object>,
) {
    let handle_value = v8::Number::new(scope, handle.index() as f64);
    set_private_value(
        scope,
        window,
        CHILD_BROWSING_CONTEXT_HANDLE_SLOT,
        handle_value.into(),
    );
    set_object_slot(scope, window, "__moliWindowSelf", window.into());
    set_object_slot(scope, window, "__moliWindowParent", parent.into());
    set_object_slot(scope, window, "__moliWindowTop", top.into());
    set_object_slot(scope, window, "__moliWindowFrames", window.into());
}

fn set_cross_origin_object_slot(
    scope: &mut v8::PinScope<'_, '_>,
    object: v8::Local<'_, v8::Object>,
    name: &'static str,
    value: v8::Local<'_, v8::Value>,
) {
    let _ = object.define_own_property(
        scope,
        v8str(scope, name).into(),
        value,
        cross_origin_property_attributes(),
    );
}

pub(super) fn build_cross_origin_location_proxy<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    handle: DomHandle,
) -> v8::Local<'s, v8::Object> {
    let target = new_cross_origin_location_proxy_target(scope);
    set_private_value(
        scope,
        target,
        CROSS_ORIGIN_LOCATION_PROXY_SLOT,
        v8::Boolean::new(scope, true).into(),
    );
    set_private_value(
        scope,
        target,
        CHILD_BROWSING_CONTEXT_HANDLE_SLOT,
        v8::Number::new(scope, handle.index() as f64).into(),
    );
    install_cross_origin_symbol_slots(scope, target, "Location");
    install_cross_origin_location_href(scope, target);
    install_cross_origin_location_methods(scope, target);
    set_cross_origin_object_slot(scope, target, "then", v8::undefined(scope).into());
    let location = wrap_cross_origin_location_proxy(scope, target).unwrap_or(target);
    set_private_value(
        scope,
        target,
        CROSS_ORIGIN_LOCATION_PROXY_SELF_SLOT,
        location.into(),
    );
    location
}

fn build_detached_cross_origin_location_proxy<'s>(
    scope: &mut v8::PinScope<'s, '_>,
) -> v8::Local<'s, v8::Object> {
    let target = new_cross_origin_location_proxy_target(scope);
    set_private_value(
        scope,
        target,
        CROSS_ORIGIN_LOCATION_PROXY_SLOT,
        v8::Boolean::new(scope, true).into(),
    );
    install_cross_origin_symbol_slots(scope, target, "Location");
    install_cross_origin_location_href(scope, target);
    install_cross_origin_location_methods(scope, target);
    set_cross_origin_object_slot(scope, target, "then", v8::undefined(scope).into());
    let location = wrap_cross_origin_location_proxy(scope, target).unwrap_or(target);
    set_private_value(
        scope,
        target,
        CROSS_ORIGIN_LOCATION_PROXY_SELF_SLOT,
        location.into(),
    );
    location
}

fn new_cross_origin_location_proxy_target<'s>(
    scope: &mut v8::PinScope<'s, '_>,
) -> v8::Local<'s, v8::Object> {
    new_null_prototype_object(scope)
}

fn wrap_cross_origin_location_proxy<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    target: v8::Local<'s, v8::Object>,
) -> Option<v8::Local<'s, v8::Object>> {
    let handler = CrossOriginLocationProxyHandlerDeclaration {
        get: (),
        has: (),
        set: (),
        get_own_property_descriptor: (),
        delete_property: (),
        define_property: (),
        set_prototype_of: (),
        prevent_extensions: (),
    }
    .bind(scope)
    .ok()?;
    let proxy = v8::Proxy::new(scope, target, handler)?;
    let proxy: v8::Local<'s, v8::Value> = proxy.into();
    v8::Local::<v8::Object>::try_from(proxy).ok()
}

fn install_cross_origin_location_href<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    location: v8::Local<'s, v8::Object>,
) {
    let Some(setter) = v8::Function::builder(cross_origin_location_navigate_setter_callback)
        .length(1)
        .data(location.into())
        .build(scope)
    else {
        return;
    };
    if let Some(setter_name) = v8_string(scope, "set href") {
        setter.set_name(setter_name);
    }
    let _ = define_get_set_property(
        scope,
        location,
        v8str(scope, "href").into(),
        v8::undefined(scope).into(),
        setter.into(),
        cross_origin_property_attributes(),
        "href",
    );
}

fn install_cross_origin_location_methods<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    location: v8::Local<'s, v8::Object>,
) {
    CrossOriginLocationMethodsDeclaration::default()
        .initialize(scope, location)
        .expect("cross-origin Location methods declaration should initialize");
}

fn install_cross_origin_symbol_slots(
    scope: &mut v8::PinScope<'_, '_>,
    object: v8::Local<'_, v8::Object>,
    _to_string_tag: &'static str,
) {
    let undefined = v8::undefined(scope).into();
    let _ = object.define_own_property(
        scope,
        v8::Symbol::get_to_string_tag(scope).into(),
        undefined,
        cross_origin_property_attributes(),
    );
    for symbol in [
        v8::Symbol::get_has_instance(scope),
        v8::Symbol::get_is_concat_spreadable(scope),
    ] {
        let _ = object.define_own_property(
            scope,
            symbol.into(),
            undefined,
            cross_origin_property_attributes(),
        );
    }
}

fn child_window_cross_origin_access_surface<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    holder: v8::Local<'s, v8::Object>,
) -> Option<v8::Local<'s, v8::Object>> {
    child_window_cross_origin_handler_data(scope, holder).map(|(surface, _)| surface)
}

fn child_window_cross_origin_proxy_self<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    holder: v8::Local<'s, v8::Object>,
) -> v8::Local<'s, v8::Object> {
    child_window_cross_origin_handler_data(scope, holder)
        .map(|(_, window_proxy)| window_proxy)
        .unwrap_or(holder)
}

fn related_top_window_registry_host_ptr<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    holder: v8::Local<'s, v8::Object>,
) -> Option<*mut JsContextHost> {
    let target = cross_origin_related_top_window_target(scope, holder)?;
    let context = target.get_creation_context(scope)?;
    context_host_ptr_from_context_slot(context)
}

/// Ephemeral observer authority resolved from the incumbent Context slot for
/// one WindowProxy callback. It is never cached beyond that callback.
#[derive(Clone, Copy)]
struct CrossOriginWindowObserver {
    host_ptr: *mut JsContextHost,
    identity: WindowExecutionContextIdentity,
}

impl CrossOriginWindowObserver {
    fn resolve(scope: &mut v8::PinScope<'_, '_>) -> Option<Self> {
        let context = cross_origin_accessing_context(scope);
        let host_ptr = context_host_ptr_from_context_slot(context)?;
        let identity =
            unsafe { &*host_ptr }.window_execution_context_identity_for_access_check(context)?;
        Some(Self { host_ptr, identity })
    }

    fn can_access_child(self, target_host_ptr: *mut JsContextHost, handle: DomHandle) -> bool {
        let target_scope = super::super::OwnerDispatchScope::Child(handle);
        let accessing_host = unsafe { &*self.host_ptr };
        if self.host_ptr == target_host_ptr {
            return accessing_host
                .window_execution_context_can_access_dispatch_scope(self.identity, target_scope);
        }
        let target_host = unsafe { &*target_host_ptr };
        accessing_host.window_execution_context_can_access_related_page_dispatch_scope(
            self.identity,
            target_host,
            target_scope,
        )
    }
}

/// Ephemeral target registry authority resolved for one WindowProxy callback.
/// The observer is resolved separately because incumbent and target Contexts
/// may belong to different related Page hosts in the same script agent.
#[derive(Clone, Copy)]
struct CrossOriginWindowChildRegistryOwner {
    host_ptr: *mut JsContextHost,
    parent: Option<DomHandle>,
    observer: Option<CrossOriginWindowObserver>,
}

impl CrossOriginWindowChildRegistryOwner {
    fn sync(self, scope: &mut v8::PinScope<'_, '_>) -> bool {
        let host = unsafe { &mut *self.host_ptr };
        let root = match self.parent {
            Some(parent) => {
                let Some(document) = host.child_browsing_context_document_handle(parent) else {
                    return false;
                };
                document
            }
            None => host.document_handle(),
        };
        host.sync_child_browsing_context_subtree(scope, root);
        true
    }

    fn child_count(self) -> usize {
        let host = unsafe { &*self.host_ptr };
        match self.parent {
            Some(parent) => host
                .child_browsing_context_child_frame_handles(parent)
                .len(),
            None => host.child_browsing_context_count(),
        }
    }

    fn child_handle_by_index(self, index: usize) -> Option<DomHandle> {
        let host = unsafe { &*self.host_ptr };
        match self.parent {
            Some(parent) => host.child_browsing_context_child_frame_handle_by_index(parent, index),
            None => host.child_browsing_context_handle_by_index(index),
        }
    }

    fn named_child_handle(self, name: &str) -> Option<DomHandle> {
        unsafe { &*self.host_ptr }.child_browsing_context_named_child_handle(self.parent, name)
    }

    fn child_window<'s>(
        self,
        scope: &mut v8::PinScope<'s, '_>,
        handle: DomHandle,
    ) -> Option<v8::Local<'s, v8::Object>> {
        let observer_can_access = self
            .observer
            .is_some_and(|observer| observer.can_access_child(self.host_ptr, handle));
        let host = unsafe { &mut *self.host_ptr };
        let window = if observer_can_access {
            host.child_browsing_context_window_wrapper_for_authorized_observer(scope, handle)
        } else {
            host.child_browsing_context_window_proxy_for_top(scope, handle)
        };
        if window.is_some() {
            host.mark_child_browsing_context_window_proxy_exposed(handle);
        }
        window
    }
}

fn cross_origin_window_child_registry_owner<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    holder: v8::Local<'s, v8::Object>,
) -> Option<CrossOriginWindowChildRegistryOwner> {
    let observer = CrossOriginWindowObserver::resolve(scope);
    if let Some(host_ptr) = related_top_window_registry_host_ptr(scope, holder) {
        return Some(CrossOriginWindowChildRegistryOwner {
            host_ptr,
            parent: None,
            observer,
        });
    }

    let parent = child_handle_from_object(scope, holder)?;
    let holder_context = holder.get_creation_context(scope)?;
    let host_ptr = context_host_ptr_from_context_slot(holder_context)?;
    let host = unsafe { &*host_ptr };
    if !host.child_browsing_context_is_live(parent)
        || host
            .child_browsing_context_document_handle(parent)
            .is_none()
    {
        return None;
    }
    Some(CrossOriginWindowChildRegistryOwner {
        host_ptr,
        parent: Some(parent),
        observer,
    })
}

fn cross_origin_window_child_by_index<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    holder: v8::Local<'s, v8::Object>,
    index: u32,
) -> Option<Option<v8::Local<'s, v8::Object>>> {
    let owner = cross_origin_window_child_registry_owner(scope, holder)?;
    if !owner.sync(scope) {
        return None;
    }
    let Some(handle) = owner.child_handle_by_index(index as usize) else {
        return Some(None);
    };
    Some(owner.child_window(scope, handle))
}

fn cross_origin_window_child_count<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    holder: v8::Local<'s, v8::Object>,
) -> Option<usize> {
    let owner = cross_origin_window_child_registry_owner(scope, holder)?;
    owner.sync(scope).then(|| owner.child_count())
}

fn cross_origin_named_child_can_shadow(name: &str) -> bool {
    !name.is_empty()
        && name.parse::<u32>().is_err()
        && !CROSS_ORIGIN_WINDOW_EXPOSED_PROPERTY_NAMES.contains(&name)
}

enum CrossOriginWindowNamedChildLookup<'s> {
    Fallback,
    Missing,
    Value(v8::Local<'s, v8::Object>),
}

fn cross_origin_window_named_child<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    holder: v8::Local<'s, v8::Object>,
    key: v8::Local<'s, v8::Name>,
) -> CrossOriginWindowNamedChildLookup<'s> {
    let Ok(key) = v8::Local::<v8::String>::try_from(key) else {
        return CrossOriginWindowNamedChildLookup::Fallback;
    };
    let name = key.to_rust_string_lossy(scope);
    if !cross_origin_named_child_can_shadow(&name) {
        return CrossOriginWindowNamedChildLookup::Fallback;
    }
    let Some(owner) = cross_origin_window_child_registry_owner(scope, holder) else {
        return CrossOriginWindowNamedChildLookup::Fallback;
    };
    if !owner.sync(scope) {
        return CrossOriginWindowNamedChildLookup::Fallback;
    }
    let Some(handle) = owner.named_child_handle(&name) else {
        return if name == "then" {
            CrossOriginWindowNamedChildLookup::Fallback
        } else {
            CrossOriginWindowNamedChildLookup::Missing
        };
    };
    owner
        .child_window(scope, handle)
        .map(CrossOriginWindowNamedChildLookup::Value)
        .unwrap_or(CrossOriginWindowNamedChildLookup::Missing)
}

fn child_window_cross_origin_handler_data<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    holder: v8::Local<'s, v8::Object>,
) -> Option<(v8::Local<'s, v8::Object>, v8::Local<'s, v8::Object>)> {
    if let Some(surface) =
        get_private_value(scope, holder, CLOSED_TOP_LEVEL_WINDOW_ACCESS_SURFACE_SLOT)
            .and_then(|value| v8::Local::<v8::Object>::try_from(value).ok())
    {
        return Some((surface, holder));
    }
    let holder_context = holder.get_creation_context(scope)?;
    let host_ptr = crate::util::context_host_ptr_from_context_slot(holder_context)?;
    let host = unsafe { &*host_ptr };
    if let Some(identity) = host.window_execution_context_identity_for_access_check(holder_context)
        && host.window_execution_context_identity_is_current(identity)
    {
        if !host.window_execution_context_identity_is_default_world(identity) {
            return None;
        }
        return match identity.dispatch_scope() {
            super::super::OwnerDispatchScope::Top => {
                host.top_level_cross_origin_window_handler_data(scope, holder)
            }
            super::super::OwnerDispatchScope::Child(handle) => host
                .child_window_proxy_records
                .cross_origin_handler_data(scope, handle),
            super::super::OwnerDispatchScope::LightweightPopup(_) => None,
        };
    }

    // A WindowProxy belongs to the browsing context, not to one LocalWindow
    // generation. A parked facade context keeps the stable proxy attached
    // while the previous realm is retired and the replacement is pending.
    let handle = holder_context
        .get_slot::<ChildWindowProxyFacadeContextHandle>()?
        .0;
    if !host.child_browsing_context_is_live(handle) {
        return None;
    }
    host.child_window_proxy_records
        .cross_origin_handler_data(scope, handle)
}

fn child_window_cross_origin_identity_name(
    scope: &mut v8::PinScope<'_, '_>,
    key: v8::Local<'_, v8::Name>,
) -> bool {
    v8::Local::<v8::String>::try_from(key)
        .ok()
        .map(|key| key.to_rust_string_lossy(scope))
        .is_some_and(|key| matches!(key.as_str(), "self" | "window" | "frames"))
}

fn child_window_cross_origin_named_getter<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    key: v8::Local<'s, v8::Name>,
    args: v8::PropertyCallbackArguments<'s>,
    mut rv: v8::ReturnValue<'_, v8::Value>,
) -> v8::Intercepted {
    let holder = args.holder();
    if child_window_cross_origin_identity_name(scope, key) {
        rv.set(child_window_cross_origin_proxy_self(scope, holder).into());
        return v8::Intercepted::kYes;
    }
    match cross_origin_window_named_child(scope, holder, key) {
        CrossOriginWindowNamedChildLookup::Value(child) => {
            rv.set(child.into());
            return v8::Intercepted::kYes;
        }
        CrossOriginWindowNamedChildLookup::Missing => {
            throw_cross_origin_location_security_error(scope);
            return v8::Intercepted::kYes;
        }
        CrossOriginWindowNamedChildLookup::Fallback => {}
    }
    let Some(surface) = child_window_cross_origin_access_surface(scope, holder) else {
        throw_cross_origin_location_security_error(scope);
        return v8::Intercepted::kYes;
    };
    if let Ok(key_string) = v8::Local::<v8::String>::try_from(key)
        && surface.has_own_property(scope, key).unwrap_or(false)
    {
        let name = key_string.to_rust_string_lossy(scope);
        if let Some(function) = cross_origin_window_method_function(scope, &name) {
            rv.set(function.into());
            return v8::Intercepted::kYes;
        }
    }
    if surface.has_own_property(scope, key).unwrap_or(false)
        && let Some(value) = surface.get(scope, key.into())
    {
        rv.set(value);
        return v8::Intercepted::kYes;
    }
    throw_cross_origin_location_security_error(scope);
    v8::Intercepted::kYes
}

fn child_window_cross_origin_named_setter<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    key: v8::Local<'s, v8::Name>,
    value: v8::Local<'s, v8::Value>,
    args: v8::PropertyCallbackArguments<'s>,
    _rv: v8::ReturnValue<'_, ()>,
) -> v8::Intercepted {
    let Some(surface) = child_window_cross_origin_access_surface(scope, args.holder()) else {
        throw_cross_origin_location_security_error(scope);
        return v8::Intercepted::kYes;
    };
    let is_location = v8::Local::<v8::String>::try_from(key)
        .ok()
        .map(|key| key.to_rust_string_lossy(scope))
        .as_deref()
        == Some("location");
    if is_location {
        let _ = surface.set(scope, key.into(), value);
        return v8::Intercepted::kYes;
    }
    throw_cross_origin_location_security_error(scope);
    v8::Intercepted::kYes
}

fn child_window_cross_origin_named_query<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    key: v8::Local<'s, v8::Name>,
    args: v8::PropertyCallbackArguments<'s>,
    mut rv: v8::ReturnValue<'_, v8::Integer>,
) -> v8::Intercepted {
    if child_window_cross_origin_identity_name(scope, key) {
        rv.set_int32(cross_origin_property_attributes().as_u32() as i32);
        return v8::Intercepted::kYes;
    }
    match cross_origin_window_named_child(scope, args.holder(), key) {
        CrossOriginWindowNamedChildLookup::Value(_) => {
            rv.set_int32(cross_origin_named_property_attributes().as_u32() as i32);
            return v8::Intercepted::kYes;
        }
        CrossOriginWindowNamedChildLookup::Missing => {
            throw_cross_origin_location_security_error(scope);
            return v8::Intercepted::kYes;
        }
        CrossOriginWindowNamedChildLookup::Fallback => {}
    }
    let Some(surface) = child_window_cross_origin_access_surface(scope, args.holder()) else {
        throw_cross_origin_location_security_error(scope);
        return v8::Intercepted::kYes;
    };
    if surface.has_own_property(scope, key).unwrap_or(false) {
        rv.set_int32(cross_origin_property_attributes().as_u32() as i32);
        return v8::Intercepted::kYes;
    }
    throw_cross_origin_location_security_error(scope);
    v8::Intercepted::kYes
}

fn child_window_cross_origin_named_deleter<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    _key: v8::Local<'s, v8::Name>,
    _args: v8::PropertyCallbackArguments<'s>,
    _rv: v8::ReturnValue<'_, v8::Boolean>,
) -> v8::Intercepted {
    throw_cross_origin_location_security_error(scope);
    v8::Intercepted::kYes
}

fn child_window_cross_origin_named_definer<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    _key: v8::Local<'s, v8::Name>,
    _descriptor: &v8::PropertyDescriptor,
    _args: v8::PropertyCallbackArguments<'s>,
    _rv: v8::ReturnValue<'_, ()>,
) -> v8::Intercepted {
    throw_cross_origin_location_security_error(scope);
    v8::Intercepted::kYes
}

fn child_window_cross_origin_named_enumerator<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    callback_args: v8::PropertyCallbackArguments<'s>,
    mut rv: v8::ReturnValue<'_, v8::Array>,
) {
    let Some(surface) = child_window_cross_origin_access_surface(scope, callback_args.holder())
    else {
        rv.set(v8::Array::new(scope, 0));
        return;
    };
    rv.set(ordered_cross_origin_window_own_keys(scope, surface, false));
}

fn ordered_cross_origin_window_own_keys<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    target: v8::Local<'s, v8::Object>,
    include_indices: bool,
) -> v8::Local<'s, v8::Array> {
    let mut property_names = v8::GetPropertyNamesArgsBuilder::new();
    property_names.property_filter(v8::PropertyFilter::ALL_PROPERTIES);
    property_names.key_conversion(v8::KeyConversionMode::ConvertToString);
    let names = target
        .get_own_property_names(scope, property_names.build())
        .unwrap_or_else(|| v8::Array::new(scope, 0));
    let mut indices = Vec::new();
    let mut exposed_names = Vec::new();
    let mut then = None;
    let mut symbols = Vec::new();
    for index in 0..names.length() {
        let Some(key) = names.get_index(scope, index) else {
            continue;
        };
        if let Ok(name) = v8::Local::<v8::String>::try_from(key) {
            let name = name.to_rust_string_lossy(scope);
            let array_index = name
                .parse::<u32>()
                .ok()
                .filter(|index| *index != u32::MAX)
                .is_some_and(|index| index.to_string() == name);
            if array_index {
                if include_indices {
                    indices.push(key);
                }
                continue;
            }
            if name == "then" {
                then = Some(key);
            } else if CROSS_ORIGIN_WINDOW_EXPOSED_PROPERTY_NAMES.contains(&name.as_str()) {
                exposed_names.push(key);
            }
        } else if key.is_symbol() {
            symbols.push(key);
        }
    }
    indices.append(&mut exposed_names);
    if let Some(then) = then {
        indices.push(then);
    }
    indices.append(&mut symbols);
    v8::Array::new_with_elements(scope, &indices)
}

fn child_window_cross_origin_named_descriptor<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    key: v8::Local<'s, v8::Name>,
    args: v8::PropertyCallbackArguments<'s>,
    mut rv: v8::ReturnValue<'_, v8::Value>,
) -> v8::Intercepted {
    match cross_origin_window_named_child(scope, args.holder(), key) {
        CrossOriginWindowNamedChildLookup::Value(child) => {
            let Ok(descriptor) =
                CrossOriginPropertyDescriptorDeclaration::new(child.into(), false, false, true)
                    .bind(scope)
            else {
                return v8::Intercepted::kNo;
            };
            rv.set(descriptor.into());
            return v8::Intercepted::kYes;
        }
        CrossOriginWindowNamedChildLookup::Missing => {
            throw_cross_origin_location_security_error(scope);
            return v8::Intercepted::kYes;
        }
        CrossOriginWindowNamedChildLookup::Fallback => {}
    }
    let Some(surface) = child_window_cross_origin_access_surface(scope, args.holder()) else {
        throw_cross_origin_location_security_error(scope);
        return v8::Intercepted::kYes;
    };
    if !surface.has_own_property(scope, key).unwrap_or(false) {
        throw_cross_origin_location_security_error(scope);
        return v8::Intercepted::kYes;
    }
    if let Ok(key_string) = v8::Local::<v8::String>::try_from(key) {
        let name = key_string.to_rust_string_lossy(scope);
        if let Some(descriptor) = cross_origin_window_method_descriptor(scope, &name) {
            rv.set(descriptor.into());
            return v8::Intercepted::kYes;
        }
        if let Some(descriptor) = cross_origin_window_attribute_descriptor(scope, &name) {
            rv.set(descriptor.into());
            return v8::Intercepted::kYes;
        }
    }
    let Some(descriptor) = surface.get_own_property_descriptor(scope, key) else {
        throw_cross_origin_location_security_error(scope);
        return v8::Intercepted::kYes;
    };
    rv.set(descriptor);
    v8::Intercepted::kYes
}

fn child_window_cross_origin_indexed_getter<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    index: u32,
    args: v8::PropertyCallbackArguments<'s>,
    mut rv: v8::ReturnValue<'_, v8::Value>,
) -> v8::Intercepted {
    match cross_origin_window_child_by_index(scope, args.holder(), index) {
        Some(Some(child)) => {
            rv.set(child.into());
            return v8::Intercepted::kYes;
        }
        Some(None) => {
            throw_cross_origin_location_security_error(scope);
            return v8::Intercepted::kYes;
        }
        None => {}
    }
    let Some(surface) = child_window_cross_origin_access_surface(scope, args.holder()) else {
        throw_cross_origin_location_security_error(scope);
        return v8::Intercepted::kYes;
    };
    let Some(key) = v8_string(scope, &index.to_string()) else {
        throw_cross_origin_location_security_error(scope);
        return v8::Intercepted::kYes;
    };
    if !surface.has_own_property(scope, key.into()).unwrap_or(false) {
        throw_cross_origin_location_security_error(scope);
        return v8::Intercepted::kYes;
    }
    match surface.get(scope, key.into()) {
        Some(value) => rv.set(value),
        None => throw_cross_origin_location_security_error(scope),
    }
    v8::Intercepted::kYes
}

fn child_window_cross_origin_indexed_setter<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    _index: u32,
    _value: v8::Local<'s, v8::Value>,
    _args: v8::PropertyCallbackArguments<'s>,
    _rv: v8::ReturnValue<'_, ()>,
) -> v8::Intercepted {
    throw_cross_origin_location_security_error(scope);
    v8::Intercepted::kYes
}

fn child_window_cross_origin_indexed_query<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    index: u32,
    args: v8::PropertyCallbackArguments<'s>,
    mut rv: v8::ReturnValue<'_, v8::Integer>,
) -> v8::Intercepted {
    match cross_origin_window_child_by_index(scope, args.holder(), index) {
        Some(Some(_)) => {
            rv.set_int32(cross_origin_index_property_attributes().as_u32() as i32);
            return v8::Intercepted::kYes;
        }
        Some(None) => {
            throw_cross_origin_location_security_error(scope);
            return v8::Intercepted::kYes;
        }
        None => {}
    }
    let Some(surface) = child_window_cross_origin_access_surface(scope, args.holder()) else {
        throw_cross_origin_location_security_error(scope);
        return v8::Intercepted::kYes;
    };
    let Some(key) = v8_string(scope, &index.to_string()) else {
        throw_cross_origin_location_security_error(scope);
        return v8::Intercepted::kYes;
    };
    if !surface.has_own_property(scope, key.into()).unwrap_or(false) {
        throw_cross_origin_location_security_error(scope);
        return v8::Intercepted::kYes;
    }
    rv.set_int32(cross_origin_index_property_attributes().as_u32() as i32);
    v8::Intercepted::kYes
}

fn child_window_cross_origin_indexed_deleter<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    _index: u32,
    _args: v8::PropertyCallbackArguments<'s>,
    _rv: v8::ReturnValue<'_, v8::Boolean>,
) -> v8::Intercepted {
    throw_cross_origin_location_security_error(scope);
    v8::Intercepted::kYes
}

fn child_window_cross_origin_indexed_definer<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    _index: u32,
    _descriptor: &v8::PropertyDescriptor,
    _args: v8::PropertyCallbackArguments<'s>,
    _rv: v8::ReturnValue<'_, ()>,
) -> v8::Intercepted {
    throw_cross_origin_location_security_error(scope);
    v8::Intercepted::kYes
}

fn child_window_cross_origin_indexed_enumerator<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    args: v8::PropertyCallbackArguments<'s>,
    mut rv: v8::ReturnValue<'_, v8::Array>,
) {
    let count = cross_origin_window_child_count(scope, args.holder()).unwrap_or_else(|| {
        child_window_cross_origin_access_surface(scope, args.holder())
            .and_then(|surface| child_handle_from_object(scope, surface))
            .and_then(|handle| {
                context_host_ptr_from_global_bridge(scope).map(|host_ptr| {
                    unsafe { &mut *host_ptr }.child_browsing_context_child_frame_count(handle)
                })
            })
            .unwrap_or(0)
    });
    let array = serialize_v8_iter_array(
        scope,
        (0..count.min(u32::MAX as usize)).map(|index| index as u32),
    )
    .unwrap_or_else(|| v8::Array::new(scope, 0));
    rv.set(array);
}

fn child_window_cross_origin_indexed_descriptor<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    index: u32,
    args: v8::PropertyCallbackArguments<'s>,
    mut rv: v8::ReturnValue<'_, v8::Value>,
) -> v8::Intercepted {
    match cross_origin_window_child_by_index(scope, args.holder(), index) {
        Some(Some(child)) => {
            let Ok(descriptor) =
                CrossOriginPropertyDescriptorDeclaration::new(child.into(), false, true, true)
                    .bind(scope)
            else {
                return v8::Intercepted::kNo;
            };
            rv.set(descriptor.into());
            return v8::Intercepted::kYes;
        }
        Some(None) => {
            throw_cross_origin_location_security_error(scope);
            return v8::Intercepted::kYes;
        }
        None => {}
    }
    let Some(surface) = child_window_cross_origin_access_surface(scope, args.holder()) else {
        throw_cross_origin_location_security_error(scope);
        return v8::Intercepted::kYes;
    };
    let Some(key) = v8_string(scope, &index.to_string()) else {
        throw_cross_origin_location_security_error(scope);
        return v8::Intercepted::kYes;
    };
    if !surface.has_own_property(scope, key.into()).unwrap_or(false) {
        throw_cross_origin_location_security_error(scope);
        return v8::Intercepted::kYes;
    }
    let Some(value) = surface.get(scope, key.into()) else {
        throw_cross_origin_location_security_error(scope);
        return v8::Intercepted::kYes;
    };
    let Ok(descriptor) =
        CrossOriginPropertyDescriptorDeclaration::new(value, false, true, true).bind(scope)
    else {
        return v8::Intercepted::kNo;
    };
    rv.set(descriptor.into());
    v8::Intercepted::kYes
}

fn cross_origin_window_denied_callback<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    _args: v8::FunctionCallbackArguments<'s>,
    _rv: v8::ReturnValue<'_, v8::Value>,
) {
    throw_cross_origin_location_security_error(scope);
}

fn cross_origin_window_proxy_get_callback<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    args: v8::FunctionCallbackArguments<'s>,
    mut rv: v8::ReturnValue<'_, v8::Value>,
) {
    let Ok(target) = v8::Local::<v8::Object>::try_from(args.get(0)) else {
        throw_cross_origin_location_security_error(scope);
        return;
    };
    let Ok(key) = v8::Local::<v8::Name>::try_from(args.get(1)) else {
        throw_cross_origin_location_security_error(scope);
        return;
    };
    if !target.has_own_property(scope, key).unwrap_or(false) {
        throw_cross_origin_location_security_error(scope);
        return;
    }
    if let Ok(key_string) = v8::Local::<v8::String>::try_from(key) {
        let name = key_string.to_rust_string_lossy(scope);
        if let Some(function) = cross_origin_window_method_function(scope, &name) {
            rv.set(function.into());
            return;
        }
    }
    let receiver = v8::Local::<v8::Object>::try_from(args.get(2)).unwrap_or(target);
    match target.get_with_receiver(scope, key.into(), receiver) {
        Some(value) => rv.set(value),
        None => rv.set_undefined(),
    }
}

fn cross_origin_window_proxy_has_callback<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    args: v8::FunctionCallbackArguments<'s>,
    mut rv: v8::ReturnValue<'_, v8::Value>,
) {
    let Ok(target) = v8::Local::<v8::Object>::try_from(args.get(0)) else {
        throw_cross_origin_location_security_error(scope);
        return;
    };
    let Ok(key) = v8::Local::<v8::Name>::try_from(args.get(1)) else {
        throw_cross_origin_location_security_error(scope);
        return;
    };
    if target.has_own_property(scope, key).unwrap_or(false) {
        rv.set_bool(true);
        return;
    }
    throw_cross_origin_location_security_error(scope);
}

fn cross_origin_window_proxy_set_callback<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    args: v8::FunctionCallbackArguments<'s>,
    mut rv: v8::ReturnValue<'_, v8::Value>,
) {
    let Ok(target) = v8::Local::<v8::Object>::try_from(args.get(0)) else {
        throw_cross_origin_location_security_error(scope);
        return;
    };
    let Ok(key) = v8::Local::<v8::String>::try_from(args.get(1)) else {
        throw_cross_origin_location_security_error(scope);
        return;
    };
    if key.to_rust_string_lossy(scope) != "location"
        || !target.has_own_property(scope, key.into()).unwrap_or(false)
    {
        throw_cross_origin_location_security_error(scope);
        return;
    }
    let receiver = v8::Local::<v8::Object>::try_from(args.get(3)).unwrap_or(target);
    let assigned = target
        .set_with_receiver(scope, key.into(), args.get(2), receiver)
        .unwrap_or(false);
    rv.set_bool(assigned);
}

fn cross_origin_window_proxy_get_own_property_descriptor_callback<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    args: v8::FunctionCallbackArguments<'s>,
    mut rv: v8::ReturnValue<'_, v8::Value>,
) {
    let Ok(target) = v8::Local::<v8::Object>::try_from(args.get(0)) else {
        throw_cross_origin_location_security_error(scope);
        return;
    };
    let Ok(key) = v8::Local::<v8::Name>::try_from(args.get(1)) else {
        throw_cross_origin_location_security_error(scope);
        return;
    };
    if !target.has_own_property(scope, key).unwrap_or(false) {
        throw_cross_origin_location_security_error(scope);
        return;
    }
    if let Ok(key_string) = v8::Local::<v8::String>::try_from(key) {
        let name = key_string.to_rust_string_lossy(scope);
        if let Some(descriptor) = cross_origin_window_method_descriptor(scope, &name) {
            rv.set(descriptor.into());
            return;
        }
        if let Some(descriptor) = cross_origin_window_attribute_descriptor(scope, &name) {
            rv.set(descriptor.into());
            return;
        }
    }
    let Some(descriptor) = target.get_own_property_descriptor(scope, key) else {
        throw_cross_origin_location_security_error(scope);
        return;
    };
    rv.set(descriptor);
}

fn cross_origin_window_proxy_own_keys_callback<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    args: v8::FunctionCallbackArguments<'s>,
    mut rv: v8::ReturnValue<'_, v8::Value>,
) {
    let Ok(target) = v8::Local::<v8::Object>::try_from(args.get(0)) else {
        throw_cross_origin_location_security_error(scope);
        return;
    };
    rv.set(ordered_cross_origin_window_own_keys(scope, target, true).into());
}

fn cross_origin_window_proxy_set_prototype_of_callback<'s>(
    _scope: &mut v8::PinScope<'s, '_>,
    args: v8::FunctionCallbackArguments<'s>,
    mut rv: v8::ReturnValue<'_, v8::Value>,
) {
    rv.set_bool(args.get(1).is_null());
}

fn cross_origin_window_proxy_prevent_extensions_callback<'s>(
    _scope: &mut v8::PinScope<'s, '_>,
    _args: v8::FunctionCallbackArguments<'s>,
    mut rv: v8::ReturnValue<'_, v8::Value>,
) {
    rv.set_bool(false);
}

pub(crate) fn is_cross_origin_location_proxy<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    object: v8::Local<'s, v8::Object>,
) -> bool {
    get_cross_origin_proxy_private_value(scope, object, CROSS_ORIGIN_LOCATION_PROXY_SLOT).is_some()
}

pub(crate) fn is_cross_origin_top_window_proxy<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    object: v8::Local<'s, v8::Object>,
) -> bool {
    get_cross_origin_proxy_private_value(scope, object, CROSS_ORIGIN_TOP_WINDOW_PROXY_SLOT)
        .is_some_and(|value| value.boolean_value(scope))
}

pub(crate) fn top_level_window_proxy_is_finally_closed<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    object: v8::Local<'s, v8::Object>,
) -> bool {
    get_cross_origin_proxy_private_value(scope, object, CLOSED_TOP_LEVEL_WINDOW_MARKER_SLOT)
        .is_some_and(|value| value.boolean_value(scope))
}

pub(crate) fn cross_origin_lightweight_popup_id<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    object: v8::Local<'s, v8::Object>,
) -> Option<u64> {
    let value = get_cross_origin_proxy_private_value(
        scope,
        object,
        CROSS_ORIGIN_LIGHTWEIGHT_POPUP_ID_SLOT,
    )?;
    let value = v8::Local::<v8::BigInt>::try_from(value).ok()?;
    let (popup_id, lossless) = value.u64_value();
    lossless.then_some(popup_id)
}

fn cross_origin_accessing_context<'s>(
    scope: &mut v8::PinScope<'s, '_>,
) -> v8::Local<'s, v8::Context> {
    // The access surface lives in the target realm, while V8 exposes the
    // initiating Realm as the incumbent context for cross-origin callbacks.
    // Engine entry points without an incumbent script fall back to the
    // currently entered Context.
    scope
        .get_incumbent_context()
        .unwrap_or_else(|| scope.get_current_context())
}

pub(crate) fn throw_cross_origin_location_security_error(scope: &mut v8::PinScope<'_, '_>) {
    let accessing_context = {
        let context = cross_origin_accessing_context(scope);
        v8::Global::new(scope, context)
    };
    let accessing_context = v8::Local::new(scope, &accessing_context);
    if accessing_context == scope.get_current_context() {
        throw_dom_exception(scope, "SecurityError", 18, CROSS_ORIGIN_ACCESS_ERROR);
        return;
    }
    let accessing_scope = &mut v8::ContextScope::new(scope, accessing_context);
    throw_dom_exception(
        accessing_scope,
        "SecurityError",
        18,
        CROSS_ORIGIN_ACCESS_ERROR,
    );
}

fn is_cross_origin_location_href_key_value<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    key: v8::Local<'s, v8::Value>,
) -> bool {
    let Ok(key) = v8::Local::<v8::String>::try_from(key) else {
        return false;
    };
    key.to_rust_string_lossy(scope) == "href"
}

fn cross_origin_location_proxy_get_callback<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    args: v8::FunctionCallbackArguments<'s>,
    mut rv: v8::ReturnValue<'_, v8::Value>,
) {
    if is_cross_origin_location_href_key_value(scope, args.get(1)) {
        throw_cross_origin_location_security_error(scope);
        return;
    }
    let Ok(target) = v8::Local::<v8::Object>::try_from(args.get(0)) else {
        rv.set_undefined();
        return;
    };
    let Ok(key) = v8::Local::<v8::Name>::try_from(args.get(1)) else {
        throw_cross_origin_location_security_error(scope);
        return;
    };
    if !target.has_own_property(scope, key).unwrap_or(false) {
        throw_cross_origin_location_security_error(scope);
        return;
    }
    if let Ok(key_string) = v8::Local::<v8::String>::try_from(key)
        && key_string.to_rust_string_lossy(scope) == "replace"
        && let Some(function) = cross_origin_location_replace_function(scope)
    {
        rv.set(function.into());
        return;
    }
    let receiver = v8::Local::<v8::Object>::try_from(args.get(2)).unwrap_or(target);
    match target.get_with_receiver(scope, key.into(), receiver) {
        Some(value) => rv.set(value),
        None => rv.set_undefined(),
    }
}

fn cross_origin_location_proxy_has_callback<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    args: v8::FunctionCallbackArguments<'s>,
    mut rv: v8::ReturnValue<'_, v8::Value>,
) {
    let Ok(target) = v8::Local::<v8::Object>::try_from(args.get(0)) else {
        throw_cross_origin_location_security_error(scope);
        return;
    };
    let Ok(key) = v8::Local::<v8::Name>::try_from(args.get(1)) else {
        throw_cross_origin_location_security_error(scope);
        return;
    };
    if target.has_own_property(scope, key).unwrap_or(false) {
        rv.set_bool(true);
        return;
    }
    throw_cross_origin_location_security_error(scope);
}

fn cross_origin_location_proxy_set_receiver<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    target: v8::Local<'s, v8::Object>,
    receiver: v8::Local<'s, v8::Value>,
) -> Option<v8::Local<'s, v8::Object>> {
    if let Some(expected) = get_private_value(scope, target, CROSS_ORIGIN_LOCATION_PROXY_SELF_SLOT)
        && receiver.strict_equals(expected)
    {
        return Some(target);
    }
    let receiver = v8::Local::<v8::Object>::try_from(receiver).ok()?;
    is_cross_origin_location_proxy(scope, receiver).then_some(receiver)
}

fn cross_origin_location_proxy_set_callback<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    args: v8::FunctionCallbackArguments<'s>,
    mut rv: v8::ReturnValue<'_, v8::Value>,
) {
    let Ok(target) = v8::Local::<v8::Object>::try_from(args.get(0)) else {
        rv.set(v8::Boolean::new(scope, false).into());
        return;
    };
    if !is_cross_origin_location_href_key_value(scope, args.get(1)) {
        throw_cross_origin_location_security_error(scope);
        return;
    }
    let Some(receiver) = cross_origin_location_proxy_set_receiver(scope, target, args.get(3))
    else {
        throw_cross_origin_illegal_invocation(scope);
        rv.set(v8::Boolean::new(scope, false).into());
        return;
    };
    let navigated = cross_origin_location_navigate(scope, receiver, args.get(2));
    rv.set(v8::Boolean::new(scope, navigated).into());
}

fn cross_origin_location_proxy_get_own_property_descriptor_callback<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    args: v8::FunctionCallbackArguments<'s>,
    mut rv: v8::ReturnValue<'_, v8::Value>,
) {
    let Ok(target) = v8::Local::<v8::Object>::try_from(args.get(0)) else {
        throw_cross_origin_location_security_error(scope);
        return;
    };
    let Ok(key) = v8::Local::<v8::Name>::try_from(args.get(1)) else {
        throw_cross_origin_location_security_error(scope);
        return;
    };
    if !target.has_own_property(scope, key).unwrap_or(false) {
        throw_cross_origin_location_security_error(scope);
        return;
    }
    if let Ok(key_string) = v8::Local::<v8::String>::try_from(key) {
        match key_string.to_rust_string_lossy(scope).as_str() {
            "replace" => {
                if let Some(descriptor) = cross_origin_location_replace_descriptor(scope) {
                    rv.set(descriptor.into());
                    return;
                }
            }
            "href" => {
                if let Some(descriptor) = cross_origin_location_href_descriptor(scope) {
                    rv.set(descriptor.into());
                    return;
                }
            }
            _ => {}
        }
    }
    let Some(descriptor) = target.get_own_property_descriptor(scope, key) else {
        throw_cross_origin_location_security_error(scope);
        return;
    };
    rv.set(descriptor);
}

fn cross_origin_location_proxy_set_prototype_of_callback<'s>(
    _scope: &mut v8::PinScope<'s, '_>,
    args: v8::FunctionCallbackArguments<'s>,
    mut rv: v8::ReturnValue<'_, v8::Value>,
) {
    rv.set_bool(args.get(1).is_null());
}

fn cross_origin_location_proxy_prevent_extensions_callback<'s>(
    _scope: &mut v8::PinScope<'s, '_>,
    _args: v8::FunctionCallbackArguments<'s>,
    mut rv: v8::ReturnValue<'_, v8::Value>,
) {
    rv.set_bool(false);
}

fn is_cross_origin_window_function_receiver<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    receiver: v8::Local<'s, v8::Object>,
) -> bool {
    child_handle_from_object(scope, receiver).is_some()
        || cross_origin_related_top_window_target(scope, receiver).is_some()
        || get_cross_origin_proxy_private_value(
            scope,
            receiver,
            DETACHED_CROSS_ORIGIN_WINDOW_PROXY_SLOT,
        )
        .is_some()
        || get_cross_origin_proxy_private_value(
            scope,
            receiver,
            CLOSED_TOP_LEVEL_WINDOW_ACCESS_SURFACE_SLOT,
        )
        .is_some()
}

fn cross_origin_window_function_receiver_surface<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    receiver: v8::Local<'s, v8::Object>,
) -> Option<v8::Local<'s, v8::Object>> {
    if !is_cross_origin_window_function_receiver(scope, receiver) {
        return None;
    }
    let storage = cross_origin_proxy_storage_object(scope, receiver);
    if get_private_value(scope, storage, DETACHED_CROSS_ORIGIN_WINDOW_PROXY_SLOT).is_some() {
        return Some(storage);
    }
    child_window_cross_origin_access_surface(scope, receiver)
}

fn cross_origin_window_attribute_getter_callback<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    args: v8::FunctionCallbackArguments<'s>,
    mut rv: v8::ReturnValue<'_, v8::Value>,
    name: &'static str,
) {
    let receiver = args.this();
    let Some(surface) = cross_origin_window_function_receiver_surface(scope, receiver) else {
        throw_cross_origin_illegal_invocation(scope);
        return;
    };
    if matches!(name, "window" | "self" | "frames") {
        rv.set(receiver.into());
        return;
    }
    let Some(surface_context) = surface.get_creation_context(scope) else {
        throw_cross_origin_illegal_invocation(scope);
        return;
    };
    let surface = v8::Global::new(scope, surface);
    let surface_context = v8::Global::new(scope, surface_context);
    let surface_context = v8::Local::new(scope, &surface_context);
    let surface_scope = &mut v8::ContextScope::new(scope, surface_context);
    let surface = v8::Local::new(surface_scope, &surface);
    match surface.get(surface_scope, v8str(surface_scope, name).into()) {
        Some(value) => rv.set(value),
        None => throw_cross_origin_illegal_invocation(surface_scope),
    }
}

fn cross_origin_window_location_attribute_getter_callback<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    args: v8::FunctionCallbackArguments<'s>,
    rv: v8::ReturnValue<'_, v8::Value>,
) {
    cross_origin_window_attribute_getter_callback(scope, args, rv, "location");
}

fn cross_origin_window_window_attribute_getter_callback<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    args: v8::FunctionCallbackArguments<'s>,
    rv: v8::ReturnValue<'_, v8::Value>,
) {
    cross_origin_window_attribute_getter_callback(scope, args, rv, "window");
}

fn cross_origin_window_frames_attribute_getter_callback<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    args: v8::FunctionCallbackArguments<'s>,
    rv: v8::ReturnValue<'_, v8::Value>,
) {
    cross_origin_window_attribute_getter_callback(scope, args, rv, "frames");
}

fn cross_origin_window_self_attribute_getter_callback<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    args: v8::FunctionCallbackArguments<'s>,
    rv: v8::ReturnValue<'_, v8::Value>,
) {
    cross_origin_window_attribute_getter_callback(scope, args, rv, "self");
}

fn cross_origin_window_top_attribute_getter_callback<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    args: v8::FunctionCallbackArguments<'s>,
    rv: v8::ReturnValue<'_, v8::Value>,
) {
    cross_origin_window_attribute_getter_callback(scope, args, rv, "top");
}

fn cross_origin_window_parent_attribute_getter_callback<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    args: v8::FunctionCallbackArguments<'s>,
    rv: v8::ReturnValue<'_, v8::Value>,
) {
    cross_origin_window_attribute_getter_callback(scope, args, rv, "parent");
}

fn cross_origin_window_opener_attribute_getter_callback<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    args: v8::FunctionCallbackArguments<'s>,
    rv: v8::ReturnValue<'_, v8::Value>,
) {
    cross_origin_window_attribute_getter_callback(scope, args, rv, "opener");
}

fn cross_origin_window_closed_attribute_getter_callback<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    args: v8::FunctionCallbackArguments<'s>,
    rv: v8::ReturnValue<'_, v8::Value>,
) {
    cross_origin_window_attribute_getter_callback(scope, args, rv, "closed");
}

fn cross_origin_window_length_attribute_getter_callback<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    args: v8::FunctionCallbackArguments<'s>,
    rv: v8::ReturnValue<'_, v8::Value>,
) {
    cross_origin_window_attribute_getter_callback(scope, args, rv, "length");
}

fn cross_origin_window_noop_callback<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    args: v8::FunctionCallbackArguments<'s>,
    _rv: v8::ReturnValue<'_, v8::Value>,
) {
    if !is_cross_origin_window_function_receiver(scope, args.this()) {
        throw_cross_origin_illegal_invocation(scope);
    }
}

fn cross_origin_window_close_callback<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    args: v8::FunctionCallbackArguments<'s>,
    rv: v8::ReturnValue<'_, v8::Value>,
) {
    if cross_origin_related_top_window_target(scope, args.this()).is_some() {
        if let Some(host_ptr) = cross_origin_window_target_host_ptr(scope, args.this()) {
            let _ = unsafe { &*host_ptr }.request_top_level_browsing_context_close();
        }
        return;
    }
    cross_origin_window_noop_callback(scope, args, rv);
}

fn cross_origin_window_closed_getter_callback<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    args: v8::FunctionCallbackArguments<'s>,
    mut rv: v8::ReturnValue<'_, v8::Value>,
) {
    if get_cross_origin_proxy_private_value(scope, args.this(), CLOSED_TOP_LEVEL_WINDOW_MARKER_SLOT)
        .is_some_and(|value| value.boolean_value(scope))
    {
        rv.set_bool(true);
        return;
    }
    if cross_origin_related_top_window_target(scope, args.this()).is_some() {
        let closed = cross_origin_window_target_host_ptr(scope, args.this())
            .is_none_or(|host_ptr| unsafe { &*host_ptr }.top_level_browsing_context_is_closed());
        rv.set_bool(closed);
        return;
    }
    rv.set_bool(false);
}

fn cross_origin_top_level_window_opener_getter_callback<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    args: v8::FunctionCallbackArguments<'s>,
    mut rv: v8::ReturnValue<'_, v8::Value>,
) {
    let receiver = args.this();
    if let Some(host_ptr) = related_top_window_registry_host_ptr(scope, receiver)
        && let Some(opener) = unsafe { &*host_ptr }.top_level_opener_value(scope)
    {
        rv.set(opener);
        return;
    }
    let opener = get_cross_origin_proxy_private_value(scope, receiver, WINDOW_OPENER_SLOT)
        .unwrap_or_else(|| v8::null(scope).into());
    if let Ok(opener_window) = v8::Local::<v8::Object>::try_from(opener)
        && top_level_window_proxy_is_finally_closed(scope, opener_window)
    {
        rv.set_null();
        return;
    }
    rv.set(opener);
}

fn cross_origin_window_length_getter_callback<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    args: v8::FunctionCallbackArguments<'s>,
    mut rv: v8::ReturnValue<'_, v8::Value>,
) {
    if let Some(count) = cross_origin_window_child_count(scope, args.this()) {
        rv.set_uint32(count as u32);
        return;
    }
    let Some(host_ptr) = context_host_ptr_from_global_bridge(scope) else {
        rv.set_int32(0);
        return;
    };
    if get_cross_origin_proxy_private_value(scope, args.this(), CROSS_ORIGIN_TOP_WINDOW_PROXY_SLOT)
        .is_some()
    {
        let count = unsafe { &*host_ptr }.child_browsing_context_count();
        rv.set_uint32(count as u32);
        return;
    }
    let Some(handle) = child_handle_from_object(scope, args.this()) else {
        throw_cross_origin_illegal_invocation(scope);
        return;
    };
    let count = unsafe { &mut *host_ptr }.child_browsing_context_child_frame_count(handle);
    rv.set_uint32(count as u32);
}

fn cross_origin_window_location_getter_callback<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    args: v8::FunctionCallbackArguments<'s>,
    mut rv: v8::ReturnValue<'_, v8::Value>,
) {
    if let Some(location) =
        get_cross_origin_proxy_private_value(scope, args.this(), CROSS_ORIGIN_WINDOW_LOCATION_SLOT)
    {
        rv.set(location);
    } else {
        throw_cross_origin_illegal_invocation(scope);
    }
}

fn cross_origin_location_navigate_setter_callback<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    args: v8::FunctionCallbackArguments<'s>,
    _rv: v8::ReturnValue<'_, v8::Value>,
) {
    if let Ok(expected_receiver) = v8::Local::<v8::Object>::try_from(args.data()) {
        let receiver = cross_origin_proxy_storage_object(scope, args.this());
        if !receiver.strict_equals(expected_receiver.into()) {
            throw_cross_origin_illegal_invocation(scope);
            return;
        }
    }
    let _ = cross_origin_location_navigate(scope, args.this(), args.get(0));
}

fn cross_origin_window_location_attribute_setter_callback<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    args: v8::FunctionCallbackArguments<'s>,
    _rv: v8::ReturnValue<'_, v8::Value>,
) {
    if !is_cross_origin_window_function_receiver(scope, args.this()) {
        throw_cross_origin_illegal_invocation(scope);
        return;
    }
    let _ = cross_origin_location_navigate(scope, args.this(), args.get(0));
}

fn cross_origin_location_href_attribute_setter_callback<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    args: v8::FunctionCallbackArguments<'s>,
    _rv: v8::ReturnValue<'_, v8::Value>,
) {
    if !is_cross_origin_location_proxy(scope, args.this()) {
        throw_cross_origin_illegal_invocation(scope);
        return;
    }
    let _ = cross_origin_location_navigate(scope, args.this(), args.get(0));
}

fn cross_origin_location_navigate<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    receiver: v8::Local<'s, v8::Object>,
    value: v8::Local<'s, v8::Value>,
) -> bool {
    let raw = match webidl::convert::<webidl::UsvString>(
        scope,
        value,
        webidl::Context::member("Location", "href"),
    ) {
        Ok(value) => value.0,
        Err(error) => {
            webidl::throw_error(scope, &error);
            return false;
        }
    };
    cross_origin_location_navigate_raw(scope, receiver, LocationNavigationKind::Assign, raw)
}

fn cross_origin_location_navigate_raw<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    receiver: v8::Local<'s, v8::Object>,
    kind: LocationNavigationKind,
    raw: String,
) -> bool {
    let related_window = cross_origin_related_top_window_target(scope, receiver)
        .map(|window| v8::Global::new(scope, window));
    let child_handle = child_handle_from_object(scope, receiver);
    if related_window.is_none() && child_handle.is_none() {
        throw_cross_origin_illegal_invocation(scope);
        return false;
    }
    let Some(target_context) = cross_origin_window_target_context(scope, receiver) else {
        return false;
    };
    let target_context = v8::Global::new(scope, target_context);
    let target_context = v8::Local::new(scope, &target_context);
    let target_scope = &mut v8::ContextScope::new(scope, target_context);
    let Some(host_ptr) = context_host_ptr_from_context_slot(target_context) else {
        return false;
    };
    if let Some(window) = related_window {
        let window = v8::Local::new(target_scope, &window);
        return navigate_top_level_window_location_from_cross_origin(
            target_scope,
            window,
            kind,
            raw,
        );
    }

    let handle = child_handle.expect("cross-origin child navigation must retain its handle");
    let host = unsafe { &mut *host_ptr };
    let target = host.resolve_child_browsing_context_url(handle, &raw);
    match kind {
        LocationNavigationKind::Assign => {
            if cross_origin_location_target_is_same_document(host, handle, &target)
                && let Some(window) =
                    host.existing_child_browsing_context_window_wrapper(target_scope, handle)
                && !dispatch_cross_document_navigation_navigate_event_for_window(
                    target_scope,
                    window,
                    target.as_str(),
                    None,
                    false,
                    None,
                )
            {
                return false;
            }
            let _ =
                host.navigate_child_browsing_context_to_url(target_scope, handle, target.as_str());
        }
        LocationNavigationKind::Replace => {
            let _ = host.queue_child_browsing_context_navigation_from_existing_seed(
                handle,
                target.as_str(),
                true,
            );
        }
        LocationNavigationKind::Reload => return false,
    }
    true
}

fn cross_origin_location_target_is_same_document(
    host: &JsContextHost,
    handle: DomHandle,
    target: &url::Url,
) -> bool {
    host.child_browsing_context_current_url(handle)
        .is_some_and(|current| {
            let mut current = current;
            current.set_fragment(None);
            let mut target = target.clone();
            target.set_fragment(None);
            current == target
        })
}

fn cross_origin_location_replace_callback<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    args: v8::FunctionCallbackArguments<'s>,
    _rv: v8::ReturnValue<'_, v8::Value>,
) {
    if !is_cross_origin_location_proxy(scope, args.this()) {
        throw_cross_origin_illegal_invocation(scope);
        return;
    }
    let Some(parsed) = webidl::parse_args::<CrossOriginLocationReplaceArgs>(scope, &args) else {
        return;
    };
    let _ = cross_origin_location_navigate_raw(
        scope,
        args.this(),
        LocationNavigationKind::Replace,
        parsed.url,
    );
}

fn child_handle_from_object<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    object: v8::Local<'s, v8::Object>,
) -> Option<DomHandle> {
    get_cross_origin_proxy_private_value(scope, object, CHILD_BROWSING_CONTEXT_HANDLE_SLOT)
        .and_then(|value| value.number_value(scope))
        .filter(|value| value.is_finite() && *value >= 0.0 && value.fract() == 0.0)
        .map(|value| DomHandle::new(value as usize))
}

fn cross_origin_related_top_window_target<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    object: v8::Local<'s, v8::Object>,
) -> Option<v8::Local<'s, v8::Object>> {
    get_cross_origin_proxy_private_value(scope, object, CROSS_ORIGIN_RELATED_TOP_WINDOW_TARGET_SLOT)
        .and_then(|value| v8::Local::<v8::Object>::try_from(value).ok())
}

pub(crate) fn is_cross_origin_related_top_window_proxy<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    object: v8::Local<'s, v8::Object>,
) -> bool {
    cross_origin_related_top_window_target(scope, object).is_some()
}

fn cross_origin_window_target_context<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    object: v8::Local<'s, v8::Object>,
) -> Option<v8::Local<'s, v8::Context>> {
    cross_origin_related_top_window_target(scope, object)
        .unwrap_or(object)
        .get_creation_context(scope)
}

pub(crate) fn cross_origin_window_target_host_ptr<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    object: v8::Local<'s, v8::Object>,
) -> Option<*mut JsContextHost> {
    // This non-owning pointer is callback-scoped. The Context slot performs
    // the shared liveness check; callers must never cache the returned value.
    let context = cross_origin_window_target_context(scope, object)?;
    context_host_ptr_from_context_slot(context)
}

pub(crate) fn throw_cross_origin_type_error(scope: &mut v8::PinScope<'_, '_>, message: &str) {
    let accessing_context = {
        let context = cross_origin_accessing_context(scope);
        v8::Global::new(scope, context)
    };
    let accessing_context = v8::Local::new(scope, &accessing_context);
    if accessing_context == scope.get_current_context() {
        throw_type_error(scope, message);
        return;
    }
    let accessing_scope = &mut v8::ContextScope::new(scope, accessing_context);
    throw_type_error(accessing_scope, message);
}

fn throw_cross_origin_illegal_invocation(scope: &mut v8::PinScope<'_, '_>) {
    throw_cross_origin_type_error(
        scope,
        "Failed to execute cross-origin Window operation: Illegal invocation.",
    );
}

fn cross_origin_property_attributes() -> v8::PropertyAttribute {
    v8::PropertyAttribute::DONT_ENUM | v8::PropertyAttribute::READ_ONLY
}

fn cross_origin_index_property_attributes() -> v8::PropertyAttribute {
    v8::PropertyAttribute::READ_ONLY
}

fn cross_origin_named_property_attributes() -> v8::PropertyAttribute {
    v8::PropertyAttribute::DONT_ENUM | v8::PropertyAttribute::READ_ONLY
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn child_window_proxy_record_is_keyed_by_browsing_context_identity() {
        let handle = DomHandle::new(41);
        let first_id = BrowsingContextId::nested(7);
        let replacement_id = BrowsingContextId::nested(8);
        let mut records = ChildWindowProxyRecords::default();

        records.bind_nested_browsing_context(handle, first_id);
        records.record_mut(handle).window_proxy_exposed = true;
        assert!(records.records.contains_key(&first_id));

        records.bind_nested_browsing_context(handle, replacement_id);
        assert_eq!(records.context_id(handle), Some(replacement_id));
        assert!(records.records.contains_key(&replacement_id));
        assert!(!records.records.contains_key(&first_id));
        assert!(records.window_proxy_exposed(handle));
    }
}
