use super::helpers::{
    attribute_node_for_index, attribute_node_for_name, named_node_map_attribute_names,
    named_node_map_element, reserved_named_node_map_key,
};
use super::*;
use indexmap::IndexSet;
use moli_webapi_declare::DataPropertyDescriptorDeclaration;

const NAMED_NODE_MAP_EXPANDO_STORE_SLOT: &str = "__moliNamedNodeMapExpandoStore";

fn named_node_map_expando_store<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    holder: v8::Local<'s, v8::Object>,
    create: bool,
) -> Option<v8::Local<'s, v8::Object>> {
    if let Some(store) = get_private_value(scope, holder, NAMED_NODE_MAP_EXPANDO_STORE_SLOT)
        .and_then(|value| v8::Local::<v8::Object>::try_from(value).ok())
    {
        return Some(store);
    }
    if !create {
        return None;
    }
    let store = v8::Object::new(scope);
    crate::util::set_null_prototype(scope, store);
    set_private_value(
        scope,
        holder,
        NAMED_NODE_MAP_EXPANDO_STORE_SLOT,
        store.into(),
    );
    Some(store)
}

fn named_node_map_expando_descriptor<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    holder: v8::Local<'s, v8::Object>,
    key: v8::Local<'s, v8::Name>,
) -> Option<v8::Local<'s, v8::Object>> {
    let store = named_node_map_expando_store(scope, holder, false)?;
    store
        .get_own_property_descriptor(scope, key)
        .and_then(|value| v8::Local::<v8::Object>::try_from(value).ok())
}

fn named_node_map_descriptor_flag(
    scope: &mut v8::PinScope<'_, '_>,
    descriptor: v8::Local<'_, v8::Object>,
    property: &'static str,
) -> bool {
    descriptor
        .get(scope, v8str(scope, property).into())
        .is_some_and(|value| value.boolean_value(scope))
}

fn named_node_map_descriptor_attributes(
    scope: &mut v8::PinScope<'_, '_>,
    descriptor: v8::Local<'_, v8::Object>,
) -> v8::PropertyAttribute {
    let mut attributes = v8::PropertyAttribute::NONE;
    let is_data_descriptor = descriptor
        .has_own_property(scope, v8str(scope, "value").into())
        .unwrap_or(false);
    if is_data_descriptor && !named_node_map_descriptor_flag(scope, descriptor, "writable") {
        attributes = attributes | v8::PropertyAttribute::READ_ONLY;
    }
    if !named_node_map_descriptor_flag(scope, descriptor, "enumerable") {
        attributes = attributes | v8::PropertyAttribute::DONT_ENUM;
    }
    if !named_node_map_descriptor_flag(scope, descriptor, "configurable") {
        attributes = attributes | v8::PropertyAttribute::DONT_DELETE;
    }
    attributes
}

fn named_node_map_expando_get<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    holder: v8::Local<'s, v8::Object>,
    key: v8::Local<'s, v8::Name>,
) -> Option<v8::Local<'s, v8::Value>> {
    let store = named_node_map_expando_store(scope, holder, false)?;
    if !store.has_own_property(scope, key).unwrap_or(false) {
        return None;
    }
    store.get_with_receiver(scope, key.into(), holder)
}

fn named_node_map_expando_set<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    holder: v8::Local<'s, v8::Object>,
    key: v8::Local<'s, v8::Name>,
    value: v8::Local<'s, v8::Value>,
) -> Option<bool> {
    let store = named_node_map_expando_store(scope, holder, true)?;
    let Some(descriptor) = store
        .get_own_property_descriptor(scope, key)
        .and_then(|value| v8::Local::<v8::Object>::try_from(value).ok())
    else {
        return store.define_own_property(scope, key, value, v8::PropertyAttribute::NONE);
    };
    if descriptor
        .has_own_property(scope, v8str(scope, "value").into())
        .unwrap_or(false)
    {
        if !named_node_map_descriptor_flag(scope, descriptor, "writable") {
            return Some(false);
        }
        let attributes = named_node_map_descriptor_attributes(scope, descriptor);
        return store.define_own_property(scope, key, value, attributes);
    }
    let setter = descriptor.get(scope, v8str(scope, "set").into())?;
    if setter.is_undefined() {
        return Some(false);
    }
    let setter = v8::Local::<v8::Function>::try_from(setter).ok()?;
    setter.call(scope, holder.into(), &[value]).map(|_| true)
}

fn named_node_map_expando_keys<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    holder: v8::Local<'s, v8::Object>,
) -> Vec<v8::Local<'s, v8::Value>> {
    let Some(store) = named_node_map_expando_store(scope, holder, false) else {
        return Vec::new();
    };
    let mut args = v8::GetPropertyNamesArgsBuilder::new();
    args.mode(v8::KeyCollectionMode::OwnOnly)
        .property_filter(v8::PropertyFilter::ALL_PROPERTIES)
        .index_filter(v8::IndexFilter::SkipIndices)
        .key_conversion(v8::KeyConversionMode::ConvertToString);
    let Some(keys) = store.get_own_property_names(scope, args.build()) else {
        return Vec::new();
    };
    (0..keys.length())
        .filter_map(|index| keys.get_index(scope, index))
        .collect()
}

fn named_node_map_named_property_names<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    element: v8::Local<'s, v8::Object>,
) -> Vec<String> {
    let is_html_element_in_html_document =
        named_node_map_element_is_html_element_in_html_document(scope, element);
    let mut keys = IndexSet::new();
    for name in named_node_map_attribute_names(scope, element) {
        if reserved_named_node_map_key(&name) {
            continue;
        }
        if is_html_element_in_html_document && name != name.to_ascii_lowercase() {
            continue;
        }
        keys.insert(name);
    }
    keys.into_iter().collect()
}

fn named_node_map_element_is_html_element_in_html_document<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    element: v8::Local<'s, v8::Object>,
) -> bool {
    if let Some(document_kind) = detached_state_string(scope, element, "documentKind") {
        return document_kind == "html"
            && detached_element_namespace_uri(scope, element).as_deref() == Some(XHTML_NS);
    }
    let Ok((runtime_ptr, handle)) = node_runtime_and_handle_from_object(scope, element) else {
        return object_string_property(scope, element, "namespaceURI").as_deref() == Some(XHTML_NS);
    };
    let dom_host = unsafe { &*runtime_ptr }.dom_host();
    let Some(node) = dom_host.node(handle) else {
        return false;
    };
    let is_html_element = node
        .as_element()
        .is_some_and(|element| element.namespace() == XHTML_NS);
    let is_html_document = node
        .owner_document()
        .and_then(|document| dom_host.node(document))
        .and_then(crate::dom::native::Node::as_document)
        .is_some_and(|document| document.is_html_document());
    is_html_element && is_html_document
}

pub(in crate::native_bridge::document) fn named_node_map_length_getter_callback<'a>(
    scope: &mut v8::PinScope<'a, '_>,
    args: v8::FunctionCallbackArguments<'a>,
    mut rv: v8::ReturnValue<'_, v8::Value>,
) {
    let Some(element) = named_node_map_element(scope, args.this()) else {
        rv.set(v8::Integer::new(scope, 0).into());
        return;
    };
    let length = named_node_map_attribute_names(scope, element).len() as i32;
    rv.set(v8::Integer::new(scope, length).into());
}

pub(super) fn named_node_map_indexed_getter<'a>(
    scope: &mut v8::PinScope<'a, '_>,
    index: u32,
    args: v8::PropertyCallbackArguments<'a>,
    mut rv: v8::ReturnValue<'_, v8::Value>,
) -> v8::Intercepted {
    let Some(element) = named_node_map_element(scope, args.holder()) else {
        return v8::Intercepted::kNo;
    };
    let Some(value) = attribute_node_for_index(scope, element, index as usize) else {
        return v8::Intercepted::kNo;
    };
    rv.set(value);
    v8::Intercepted::kYes
}

pub(super) fn named_node_map_indexed_query<'a>(
    scope: &mut v8::PinScope<'a, '_>,
    index: u32,
    args: v8::PropertyCallbackArguments<'a>,
    mut rv: v8::ReturnValue<'_, v8::Integer>,
) -> v8::Intercepted {
    let Some(element) = named_node_map_element(scope, args.holder()) else {
        return v8::Intercepted::kNo;
    };
    if named_node_map_attribute_names(scope, element).len() <= index as usize {
        return v8::Intercepted::kNo;
    }
    rv.set_int32(v8::PropertyAttribute::READ_ONLY.as_u32() as i32);
    v8::Intercepted::kYes
}

pub(super) fn named_node_map_indexed_setter(
    _scope: &mut v8::PinScope<'_, '_>,
    _index: u32,
    _value: v8::Local<'_, v8::Value>,
    _args: v8::PropertyCallbackArguments<'_>,
    mut rv: v8::ReturnValue<'_, v8::Boolean>,
) -> v8::Intercepted {
    rv.set_bool(false);
    v8::Intercepted::kYes
}

pub(super) fn named_node_map_indexed_deleter<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    index: u32,
    args: v8::PropertyCallbackArguments<'s>,
    mut rv: v8::ReturnValue<'_, v8::Boolean>,
) -> v8::Intercepted {
    let Some(element) = named_node_map_element(scope, args.holder()) else {
        return v8::Intercepted::kNo;
    };
    if named_node_map_attribute_names(scope, element).len() <= index as usize {
        return v8::Intercepted::kNo;
    }
    rv.set_bool(false);
    v8::Intercepted::kYes
}

pub(super) fn named_node_map_indexed_definer(
    _scope: &mut v8::PinScope<'_, '_>,
    _index: u32,
    _descriptor: &v8::PropertyDescriptor,
    _args: v8::PropertyCallbackArguments<'_>,
    mut rv: v8::ReturnValue<'_, v8::Boolean>,
) -> v8::Intercepted {
    rv.set_bool(false);
    v8::Intercepted::kYes
}

pub(super) fn named_node_map_indexed_enumerator<'a>(
    scope: &mut v8::PinScope<'a, '_>,
    args: v8::PropertyCallbackArguments<'a>,
    mut rv: v8::ReturnValue<'_, v8::Array>,
) {
    let Some(element) = named_node_map_element(scope, args.holder()) else {
        rv.set(v8::Array::new(scope, 0));
        return;
    };
    let keys = (0..named_node_map_attribute_names(scope, element).len())
        .map(|index| v8::Integer::new(scope, index as i32).into())
        .collect::<Vec<_>>();
    rv.set(v8::Array::new_with_elements(scope, &keys));
}

pub(super) fn named_node_map_indexed_descriptor<'a>(
    scope: &mut v8::PinScope<'a, '_>,
    index: u32,
    args: v8::PropertyCallbackArguments<'a>,
    mut rv: v8::ReturnValue<'_, v8::Value>,
) -> v8::Intercepted {
    let Some(element) = named_node_map_element(scope, args.holder()) else {
        return v8::Intercepted::kNo;
    };
    let Some(value) = attribute_node_for_index(scope, element, index as usize) else {
        return v8::Intercepted::kNo;
    };
    let Ok(descriptor) = DataPropertyDescriptorDeclaration::new(value, false, true).bind(scope)
    else {
        return v8::Intercepted::kNo;
    };
    rv.set(descriptor.into());
    v8::Intercepted::kYes
}

pub(super) fn named_node_map_named_getter<'a>(
    scope: &mut v8::PinScope<'a, '_>,
    key: v8::Local<'a, v8::Name>,
    args: v8::PropertyCallbackArguments<'a>,
    mut rv: v8::ReturnValue<'_, v8::Value>,
) -> v8::Intercepted {
    let Some(element) = named_node_map_element(scope, args.holder()) else {
        return v8::Intercepted::kNo;
    };
    let Ok(key_string) = v8::Local::<v8::String>::try_from(key) else {
        if let Some(value) = named_node_map_expando_get(scope, args.holder(), key) {
            rv.set(value);
            return v8::Intercepted::kYes;
        }
        return v8::Intercepted::kNo;
    };
    let key_name = key_string.to_rust_string_lossy(scope);
    if key_name.parse::<u32>().is_ok() || reserved_named_node_map_key(&key_name) {
        return v8::Intercepted::kNo;
    }
    if let Some(value) = named_node_map_expando_get(scope, args.holder(), key_string.into()) {
        rv.set(value);
        return v8::Intercepted::kYes;
    }
    let Some(value) = attribute_node_for_name(scope, element, &key_name) else {
        return v8::Intercepted::kNo;
    };
    if value.is_null_or_undefined() {
        return v8::Intercepted::kNo;
    }
    rv.set(value);
    v8::Intercepted::kYes
}

pub(super) fn named_node_map_named_query<'a>(
    scope: &mut v8::PinScope<'a, '_>,
    key: v8::Local<'a, v8::Name>,
    args: v8::PropertyCallbackArguments<'a>,
    mut rv: v8::ReturnValue<'_, v8::Integer>,
) -> v8::Intercepted {
    let Some(element) = named_node_map_element(scope, args.holder()) else {
        return v8::Intercepted::kNo;
    };
    let Ok(key_string) = v8::Local::<v8::String>::try_from(key) else {
        if let Some(descriptor) = named_node_map_expando_descriptor(scope, args.holder(), key) {
            rv.set_int32(named_node_map_descriptor_attributes(scope, descriptor).as_u32() as i32);
            return v8::Intercepted::kYes;
        }
        return v8::Intercepted::kNo;
    };
    let key_name = key_string.to_rust_string_lossy(scope);
    if key_name.parse::<u32>().is_ok() || reserved_named_node_map_key(&key_name) {
        return v8::Intercepted::kNo;
    }
    if let Some(descriptor) =
        named_node_map_expando_descriptor(scope, args.holder(), key_string.into())
    {
        rv.set_int32(named_node_map_descriptor_attributes(scope, descriptor).as_u32() as i32);
        return v8::Intercepted::kYes;
    }
    if !named_node_map_named_property_names(scope, element)
        .into_iter()
        .any(|name| name == key_name)
    {
        return v8::Intercepted::kNo;
    }
    rv.set_int32(v8::PropertyAttribute::DONT_ENUM.as_u32() as i32);
    v8::Intercepted::kYes
}

pub(super) fn named_node_map_named_descriptor<'a>(
    scope: &mut v8::PinScope<'a, '_>,
    key: v8::Local<'a, v8::Name>,
    args: v8::PropertyCallbackArguments<'a>,
    mut rv: v8::ReturnValue<'_, v8::Value>,
) -> v8::Intercepted {
    let Some(element) = named_node_map_element(scope, args.holder()) else {
        return v8::Intercepted::kNo;
    };
    let Ok(key_string) = v8::Local::<v8::String>::try_from(key) else {
        if let Some(descriptor) = named_node_map_expando_descriptor(scope, args.holder(), key) {
            rv.set(descriptor.into());
            return v8::Intercepted::kYes;
        }
        return v8::Intercepted::kNo;
    };
    let key_name = key_string.to_rust_string_lossy(scope);
    if key_name.parse::<u32>().is_ok() || reserved_named_node_map_key(&key_name) {
        return v8::Intercepted::kNo;
    }
    if let Some(descriptor) =
        named_node_map_expando_descriptor(scope, args.holder(), key_string.into())
    {
        rv.set(descriptor.into());
        return v8::Intercepted::kYes;
    }
    if !named_node_map_named_property_names(scope, element)
        .into_iter()
        .any(|name| name == key_name)
    {
        return v8::Intercepted::kNo;
    }
    let Some(value) = attribute_node_for_name(scope, element, &key_name) else {
        return v8::Intercepted::kNo;
    };
    if value.is_null_or_undefined() {
        return v8::Intercepted::kNo;
    }
    let Ok(descriptor) = DataPropertyDescriptorDeclaration::new(value, false, false).bind(scope)
    else {
        return v8::Intercepted::kNo;
    };
    rv.set(descriptor.into());
    v8::Intercepted::kYes
}

pub(super) fn named_node_map_named_setter<'a>(
    scope: &mut v8::PinScope<'a, '_>,
    key: v8::Local<'a, v8::Name>,
    value: v8::Local<'a, v8::Value>,
    args: v8::PropertyCallbackArguments<'a>,
    mut rv: v8::ReturnValue<'_, v8::Boolean>,
) -> v8::Intercepted {
    let Some(element) = named_node_map_element(scope, args.holder()) else {
        return v8::Intercepted::kNo;
    };
    let Ok(key_string) = v8::Local::<v8::String>::try_from(key) else {
        return match named_node_map_expando_set(scope, args.holder(), key, value) {
            Some(true) => v8::Intercepted::kYes,
            Some(false) => {
                rv.set_bool(false);
                v8::Intercepted::kYes
            }
            None => v8::Intercepted::kYes,
        };
    };
    let key_name = key_string.to_rust_string_lossy(scope);
    if key_name.parse::<u32>().is_ok() || reserved_named_node_map_key(&key_name) {
        return v8::Intercepted::kNo;
    }
    let has_expando =
        named_node_map_expando_descriptor(scope, args.holder(), key_string.into()).is_some();
    if !has_expando
        && named_node_map_named_property_names(scope, element)
            .into_iter()
            .any(|name| name == key_name)
    {
        rv.set_bool(false);
        return v8::Intercepted::kYes;
    }
    match named_node_map_expando_set(scope, args.holder(), key_string.into(), value) {
        Some(true) => v8::Intercepted::kYes,
        Some(false) => {
            rv.set_bool(false);
            v8::Intercepted::kYes
        }
        None => v8::Intercepted::kYes,
    }
}

pub(super) fn named_node_map_named_deleter<'a>(
    scope: &mut v8::PinScope<'a, '_>,
    key: v8::Local<'a, v8::Name>,
    args: v8::PropertyCallbackArguments<'a>,
    mut rv: v8::ReturnValue<'_, v8::Boolean>,
) -> v8::Intercepted {
    let Some(element) = named_node_map_element(scope, args.holder()) else {
        return v8::Intercepted::kNo;
    };
    let Ok(key_string) = v8::Local::<v8::String>::try_from(key) else {
        let Some(store) = named_node_map_expando_store(scope, args.holder(), false) else {
            return v8::Intercepted::kNo;
        };
        if !store.has_own_property(scope, key).unwrap_or(false) {
            return v8::Intercepted::kNo;
        }
        rv.set_bool(store.delete(scope, key.into()).unwrap_or(false));
        return v8::Intercepted::kYes;
    };
    let key_name = key_string.to_rust_string_lossy(scope);
    if key_name.parse::<u32>().is_ok() || reserved_named_node_map_key(&key_name) {
        return v8::Intercepted::kNo;
    }
    if let Some(store) = named_node_map_expando_store(scope, args.holder(), false)
        && store
            .has_own_property(scope, key_string.into())
            .unwrap_or(false)
    {
        rv.set_bool(store.delete(scope, key_string.into()).unwrap_or(false));
        return v8::Intercepted::kYes;
    }
    if named_node_map_named_property_names(scope, element)
        .into_iter()
        .any(|name| name == key_name)
    {
        rv.set_bool(false);
        return v8::Intercepted::kYes;
    }
    v8::Intercepted::kNo
}

pub(super) fn named_node_map_named_definer<'a>(
    scope: &mut v8::PinScope<'a, '_>,
    key: v8::Local<'a, v8::Name>,
    descriptor: &v8::PropertyDescriptor,
    args: v8::PropertyCallbackArguments<'a>,
    mut rv: v8::ReturnValue<'_, v8::Boolean>,
) -> v8::Intercepted {
    let Some(element) = named_node_map_element(scope, args.holder()) else {
        return v8::Intercepted::kNo;
    };
    let Ok(key_string) = v8::Local::<v8::String>::try_from(key) else {
        let Some(store) = named_node_map_expando_store(scope, args.holder(), true) else {
            return v8::Intercepted::kNo;
        };
        rv.set_bool(
            store
                .define_property(scope, key, descriptor)
                .unwrap_or(false),
        );
        return v8::Intercepted::kYes;
    };
    let key_name = key_string.to_rust_string_lossy(scope);
    if key_name.parse::<u32>().is_ok() || reserved_named_node_map_key(&key_name) {
        return v8::Intercepted::kNo;
    }
    let has_expando =
        named_node_map_expando_descriptor(scope, args.holder(), key_string.into()).is_some();
    if !has_expando
        && named_node_map_named_property_names(scope, element)
            .into_iter()
            .any(|name| name == key_name)
    {
        rv.set_bool(false);
        return v8::Intercepted::kYes;
    }
    let Some(store) = named_node_map_expando_store(scope, args.holder(), true) else {
        return v8::Intercepted::kNo;
    };
    rv.set_bool(
        store
            .define_property(scope, key_string.into(), descriptor)
            .unwrap_or(false),
    );
    v8::Intercepted::kYes
}

pub(super) fn named_node_map_named_enumerator<'a>(
    scope: &mut v8::PinScope<'a, '_>,
    args: v8::PropertyCallbackArguments<'a>,
    mut rv: v8::ReturnValue<'_, v8::Array>,
) {
    let Some(element) = named_node_map_element(scope, args.holder()) else {
        rv.set(v8::Array::new(scope, 0));
        return;
    };
    let mut keys = Vec::new();
    for name in named_node_map_named_property_names(scope, element) {
        let Some(key) = v8_string(scope, &name) else {
            continue;
        };
        if named_node_map_expando_descriptor(scope, args.holder(), key.into()).is_none() {
            keys.push(key.into());
        }
    }
    keys.extend(named_node_map_expando_keys(scope, args.holder()));
    rv.set(v8::Array::new_with_elements(scope, &keys));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ensure_v8_for_test;

    fn set_descriptor_flag(
        scope: &mut v8::PinScope<'_, '_>,
        descriptor: v8::Local<'_, v8::Object>,
        property: &'static str,
        value: bool,
    ) {
        let key = v8str(scope, property);
        let value = v8::Boolean::new(scope, value);
        assert_eq!(descriptor.set(scope, key.into(), value.into()), Some(true));
    }

    #[test]
    fn descriptor_attributes_only_apply_read_only_to_data_descriptors() {
        ensure_v8_for_test();
        let mut isolate = v8::Isolate::new(v8::CreateParams::default());
        let scope = std::pin::pin!(v8::HandleScope::new(&mut isolate));
        let scope = &mut scope.init();
        let context = v8::Context::new(scope, Default::default());
        let scope = &mut v8::ContextScope::new(scope, context);

        let accessor = v8::Object::new(scope);
        let get_key = v8str(scope, "get");
        let undefined = v8::undefined(scope);
        assert_eq!(
            accessor.set(scope, get_key.into(), undefined.into()),
            Some(true)
        );
        set_descriptor_flag(scope, accessor, "enumerable", false);
        set_descriptor_flag(scope, accessor, "configurable", false);
        let accessor_attributes = named_node_map_descriptor_attributes(scope, accessor);
        assert_eq!(
            accessor_attributes.as_u32(),
            (v8::PropertyAttribute::DONT_ENUM | v8::PropertyAttribute::DONT_DELETE).as_u32()
        );

        let data = v8::Object::new(scope);
        let value_key = v8str(scope, "value");
        let undefined = v8::undefined(scope);
        assert_eq!(
            data.set(scope, value_key.into(), undefined.into()),
            Some(true)
        );
        set_descriptor_flag(scope, data, "writable", false);
        set_descriptor_flag(scope, data, "enumerable", false);
        set_descriptor_flag(scope, data, "configurable", false);
        let data_attributes = named_node_map_descriptor_attributes(scope, data);
        assert_eq!(
            data_attributes.as_u32(),
            (v8::PropertyAttribute::READ_ONLY
                | v8::PropertyAttribute::DONT_ENUM
                | v8::PropertyAttribute::DONT_DELETE)
                .as_u32()
        );
    }
}
