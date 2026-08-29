use crate::util::{get_private_value, set_private_value};

const INTERNAL_JAVASCRIPT_URL_EVAL_PERMIT_SLOT: &str = "__moliInternalJavascriptUrlEvalPermit";

/// Scoped permission for the direct eval used by the lightweight-popup
/// javascript-URL adapter.
///
/// Lightweight popups do not own a V8 Context, so their adapter uses direct
/// eval to recover Script completion semantics while resolving globals
/// through the popup Window object. The source has already passed the target
/// Document's CSP and Trusted Types checks. The isolate callback consumes the
/// private marker on the first compilation; nested page-authored eval calls
/// therefore go through the normal policy path.
pub(crate) struct InternalJavascriptUrlEvalPermit {
    previous: Option<v8::Global<v8::Value>>,
}

pub(crate) fn arm_internal_javascript_url_eval(
    scope: &mut v8::PinScope<'_, '_>,
) -> InternalJavascriptUrlEvalPermit {
    let global = scope.get_current_context().global(scope);
    let previous = get_private_value(scope, global, INTERNAL_JAVASCRIPT_URL_EVAL_PERMIT_SLOT)
        .map(|value| v8::Global::new(scope, value));
    let marker = v8::Object::new(scope);
    set_private_value(
        scope,
        global,
        INTERNAL_JAVASCRIPT_URL_EVAL_PERMIT_SLOT,
        marker.into(),
    );
    InternalJavascriptUrlEvalPermit { previous }
}

pub(crate) fn restore_internal_javascript_url_eval(
    scope: &mut v8::PinScope<'_, '_>,
    permit: InternalJavascriptUrlEvalPermit,
) {
    let global = scope.get_current_context().global(scope);
    let previous = permit
        .previous
        .as_ref()
        .map(|value| v8::Local::new(scope, value))
        .unwrap_or_else(|| v8::undefined(scope).into());
    set_private_value(
        scope,
        global,
        INTERNAL_JAVASCRIPT_URL_EVAL_PERMIT_SLOT,
        previous,
    );
}

pub(crate) fn consume_internal_javascript_url_eval(scope: &mut v8::PinScope<'_, '_>) -> bool {
    let global = scope.get_current_context().global(scope);
    let armed = get_private_value(scope, global, INTERNAL_JAVASCRIPT_URL_EVAL_PERMIT_SLOT)
        .is_some_and(|marker| marker.is_object());
    if !armed {
        return false;
    }
    let empty = v8::undefined(scope);
    set_private_value(
        scope,
        global,
        INTERNAL_JAVASCRIPT_URL_EVAL_PERMIT_SLOT,
        empty.into(),
    );
    true
}
