use super::super::{BridgeHandle, JsContextHost, ReflectorId};
use crate::util::context_host_ptr_from_global_bridge;

fn object_reflector_id(
    scope: &mut v8::PinScope<'_, '_>,
    object: v8::Local<'_, v8::Object>,
) -> std::result::Result<ReflectorId, String> {
    let field_index = match object.internal_field_count() {
        1 => 0,
        // Window wrappers retain their host pointer in field 0 because Window
        // brand checks can cross realm boundaries.
        2 => 1,
        _ => return Err("wrapper had an invalid reflector field count".to_owned()),
    };
    let value = object
        .get_internal_field(scope, field_index)
        .ok_or_else(|| "wrapper missing reflector field".to_owned())?;
    let value = v8::Local::<v8::Value>::try_from(value)
        .map_err(|_| "wrapper reflector field had invalid type".to_owned())?;
    let number = value
        .number_value(scope)
        .ok_or_else(|| "wrapper reflector field was not numeric".to_owned())?;
    if !number.is_finite() || number.fract() != 0.0 || number <= 0.0 {
        return Err("wrapper reflector field was invalid".to_owned());
    }
    Ok(ReflectorId::from_raw(number as u64))
}

pub(in crate::native_bridge) fn bridge_handle_from_object(
    scope: &mut v8::PinScope<'_, '_>,
    object: v8::Local<'_, v8::Object>,
) -> std::result::Result<(*mut JsContextHost, BridgeHandle), String> {
    let runtime_ptr = context_host_ptr_from_global_bridge(scope)
        .ok_or_else(|| "wrapper context was missing its native host".to_owned())?;
    let reflector_id = object_reflector_id(scope, object)?;
    let handle = unsafe { &*runtime_ptr }
        .native_bridge()
        .bridge_handle(reflector_id)
        .ok_or_else(|| format!("missing bridge identity `{}`", reflector_id.raw()))?;
    Ok((runtime_ptr, handle))
}
