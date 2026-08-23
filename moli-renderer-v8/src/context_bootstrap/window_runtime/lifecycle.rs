use crate::{
    context_bootstrap::{window_accessors::window_child_context_handle, window_receiver},
    util::context_host_ptr_from_global_bridge,
    webidl,
};

fn valid_window_receiver<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    receiver: v8::Local<'s, v8::Object>,
) -> bool {
    if window_receiver::is_window_receiver(scope, receiver) {
        return true;
    }
    webidl::throw_type_error(scope, "Window operation called on incompatible receiver.");
    false
}

pub(in crate::context_bootstrap) fn window_close_callback<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    args: v8::FunctionCallbackArguments<'s>,
    _rv: v8::ReturnValue<'_, v8::Value>,
) {
    let receiver = args.this();
    if !valid_window_receiver(scope, receiver) {
        return;
    }
    // Nested browsing contexts do not own a top-level target. Their legacy
    // `Window.close()` surface remains a no-op.
    if window_child_context_handle(scope, receiver).is_some() {
        return;
    }
    let Some(host_ptr) = context_host_ptr_from_global_bridge(scope) else {
        return;
    };
    let _ = unsafe { &*host_ptr }.request_top_level_browsing_context_close();
}

pub(in crate::context_bootstrap) fn window_closed_getter<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    args: v8::FunctionCallbackArguments<'s>,
    mut rv: v8::ReturnValue<'_, v8::Value>,
) {
    let receiver = args.this();
    if !valid_window_receiver(scope, receiver) {
        return;
    }
    if window_child_context_handle(scope, receiver).is_some() {
        rv.set_bool(false);
        return;
    }
    let closed = context_host_ptr_from_global_bridge(scope)
        .is_none_or(|host_ptr| unsafe { &*host_ptr }.top_level_browsing_context_is_closed());
    rv.set_bool(closed);
}
