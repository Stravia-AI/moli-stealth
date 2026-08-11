use crate::document_runtime::DomHandle;
use crate::native_bridge::element::element_attribute_for_object;
use crate::util::v8_string;
use moli_webapi_declare::ObjectLiteralDeclaration;

use super::super::super::super::JsContextHost;

pub(super) fn document_all_items_array<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    runtime_ptr: *mut JsContextHost,
    document_handle: DomHandle,
) -> Option<v8::Local<'s, v8::Array>> {
    let runtime = unsafe { &mut *runtime_ptr };
    let handles =
        runtime
            .dom_host()
            .resolve_live_collection(document_handle, "tagName", Some("*"), true)?;
    let array = v8::Array::new(scope, handles.len() as i32);
    let mut visible_index = 0u32;
    for handle in handles {
        let Some(value) = runtime
            .native_bridge_mut()
            .wrap_handle(scope, runtime_ptr, handle)
            .map(Into::<v8::Local<'_, v8::Value>>::into)
        else {
            continue;
        };
        let _ = array.set_index(scope, visible_index, value);
        visible_index += 1;
    }
    Some(array)
}

pub(super) fn document_all_named_lookup<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    items: v8::Local<'s, v8::Array>,
) -> v8::Local<'s, v8::Object> {
    let lookup = ObjectLiteralDeclaration::bind(scope);
    let length = items.length();
    for index in 0..length {
        let Some(item) = items.get_index(scope, index) else {
            continue;
        };
        let Ok(item) = v8::Local::<v8::Object>::try_from(item) else {
            continue;
        };
        for attribute_name in ["id", "name"] {
            let Some(key_text) = element_attribute_for_object(scope, item, attribute_name) else {
                continue;
            };
            if key_text.is_empty() {
                continue;
            }
            let Some(key) = v8_string(scope, &key_text) else {
                continue;
            };
            if lookup
                .as_object()
                .get(scope, key.into())
                .is_some_and(|existing| !existing.is_null_or_undefined())
            {
                continue;
            }
            lookup.set_value_property(scope, key.into(), item.into());
        }
    }
    lookup.into_object()
}
