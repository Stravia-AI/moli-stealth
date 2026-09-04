/// Returns the embedder-owned CDP subtype for a value while V8 Inspector is
/// constructing its `Runtime.RemoteObject`.
///
/// Moli's reflector lookup is document-local, so enter the object's creation
/// context before resolving its wrapper identity. This also covers a node from
/// an iframe returned through a different execution context.
pub(super) fn subtype<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    value: v8::Local<'s, v8::Value>,
) -> Option<v8::UniquePtr<v8::inspector::StringBuffer>> {
    let object = v8::Local::<v8::Object>::try_from(value).ok()?;
    let creation_context = object.get_creation_context(scope)?;
    let scope = &mut v8::ContextScope::new(scope, creation_context);
    if !crate::native_bridge::object_is_node_wrapper_or_detached(scope, object) {
        return None;
    }
    Some(inspector_string_buffer("node"))
}

/// Preserves the RemoteObject description V8 produced before Moli supplied a
/// custom subtype. V8 requires a description callback for custom subtypes and
/// otherwise discards the subtype; the wrapper constructor name is the same
/// description used by the former output-boundary completion path.
pub(super) fn description<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    value: v8::Local<'s, v8::Value>,
) -> Option<v8::UniquePtr<v8::inspector::StringBuffer>> {
    let object = v8::Local::<v8::Object>::try_from(value).ok()?;
    let description = object.get_constructor_name().to_rust_string_lossy(scope);
    Some(inspector_string_buffer(&description))
}

fn inspector_string_buffer(value: &str) -> v8::UniquePtr<v8::inspector::StringBuffer> {
    v8::inspector::StringBuffer::create(v8::inspector::StringView::from(value.as_bytes()))
}
