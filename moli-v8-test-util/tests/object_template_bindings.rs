use std::pin::pin;

fn eval<'s>(scope: &mut v8::PinScope<'s, '_>, source: &str) -> v8::Local<'s, v8::Value> {
    let source = v8::String::new(scope, source).expect("test source should fit in a V8 string");
    let script = v8::Script::compile(scope, source, None).expect("test source should compile");
    script.run(scope).expect("test source should run")
}

fn return_42(
    _scope: &mut v8::PinScope<'_, '_>,
    _args: v8::FunctionCallbackArguments<'_>,
    mut rv: v8::ReturnValue<v8::Value>,
) {
    rv.set_int32(42);
}

fn record_setter_value(
    scope: &mut v8::PinScope<'_, '_>,
    args: v8::FunctionCallbackArguments<'_>,
    _rv: v8::ReturnValue<v8::Value>,
) {
    let key = v8::String::new(scope, "setterValue").unwrap();
    assert_eq!(args.this().set(scope, key.into(), args.get(0)), Some(true));
}

fn empty_constructor(
    _scope: &mut v8::PinScope<'_, '_>,
    _args: v8::FunctionCallbackArguments<'_>,
    _rv: v8::ReturnValue<v8::Value>,
) {
}

unsafe extern "C" fn deny_cross_context_access(
    _accessing_context: v8::Local<'_, v8::Context>,
    _accessed_object: v8::Local<'_, v8::Object>,
    _data: v8::Local<'_, v8::Value>,
) -> bool {
    false
}

fn miss_named_get<'s>(
    _scope: &mut v8::PinScope<'s, '_>,
    _key: v8::Local<'s, v8::Name>,
    _args: v8::PropertyCallbackArguments<'s>,
    _rv: v8::ReturnValue<v8::Value>,
) -> v8::Intercepted {
    v8::Intercepted::kNo
}

fn miss_indexed_get<'s>(
    _scope: &mut v8::PinScope<'s, '_>,
    _index: u32,
    _args: v8::PropertyCallbackArguments<'s>,
    _rv: v8::ReturnValue<v8::Value>,
) -> v8::Intercepted {
    v8::Intercepted::kNo
}

fn reject_named_set<'s>(
    _scope: &mut v8::PinScope<'s, '_>,
    _key: v8::Local<'s, v8::Name>,
    _value: v8::Local<'s, v8::Value>,
    _args: v8::PropertyCallbackArguments<'s>,
    mut rv: v8::ReturnValue<v8::Boolean>,
) -> v8::Intercepted {
    rv.set_bool(false);
    v8::Intercepted::kYes
}

fn reject_indexed_set<'s>(
    _scope: &mut v8::PinScope<'s, '_>,
    _index: u32,
    _value: v8::Local<'s, v8::Value>,
    _args: v8::PropertyCallbackArguments<'s>,
    mut rv: v8::ReturnValue<v8::Boolean>,
) -> v8::Intercepted {
    rv.set_bool(false);
    v8::Intercepted::kYes
}

fn reject_named_define<'s>(
    _scope: &mut v8::PinScope<'s, '_>,
    _key: v8::Local<'s, v8::Name>,
    _descriptor: &v8::PropertyDescriptor,
    _args: v8::PropertyCallbackArguments<'s>,
    mut rv: v8::ReturnValue<v8::Boolean>,
) -> v8::Intercepted {
    rv.set_bool(false);
    v8::Intercepted::kYes
}

fn reject_indexed_define<'s>(
    _scope: &mut v8::PinScope<'s, '_>,
    _index: u32,
    _descriptor: &v8::PropertyDescriptor,
    _args: v8::PropertyCallbackArguments<'s>,
    mut rv: v8::ReturnValue<v8::Boolean>,
) -> v8::Intercepted {
    rv.set_bool(false);
    v8::Intercepted::kYes
}

#[test]
fn denied_access_handlers_preserve_boolean_setter_results() {
    moli_v8_test_util::ensure_v8();
    let mut isolate = v8::Isolate::new(v8::CreateParams::default());
    let scope = pin!(v8::HandleScope::new(&mut isolate));
    let scope = &mut scope.init();

    let owner_template = v8::ObjectTemplate::new(scope);
    owner_template.set_security_token_access_check_and_handlers(
        deny_cross_context_access,
        v8::NamedPropertyHandlerConfiguration::new()
            .getter(miss_named_get)
            .setter(reject_named_set)
            .definer(reject_named_define),
        v8::IndexedPropertyHandlerConfiguration::new()
            .getter(miss_indexed_get)
            .setter(reject_indexed_set)
            .definer(reject_indexed_define),
    );
    let owner_context = v8::Context::new(
        scope,
        v8::ContextOptions {
            global_template: Some(owner_template),
            ..Default::default()
        },
    );
    let caller_context = v8::Context::new(
        scope,
        v8::ContextOptions {
            global_template: Some(owner_template),
            ..Default::default()
        },
    );
    let owner_token = v8::String::new(scope, "owner-token").unwrap();
    owner_context.set_security_token(owner_token.into());
    let caller_token = v8::String::new(scope, "caller-token").unwrap();
    caller_context.set_security_token(caller_token.into());
    let target = v8::Global::new(scope, owner_context.global(scope));

    let caller_scope = &mut v8::ContextScope::new(scope, caller_context);
    let target = v8::Local::new(caller_scope, &target);
    let target_key = v8::String::new(caller_scope, "target").unwrap();
    assert_eq!(
        caller_context
            .global(caller_scope)
            .set(caller_scope, target_key.into(), target.into()),
        Some(true)
    );

    let result = eval(
        caller_scope,
        r#"
JSON.stringify([
  Reflect.set(target, "name", 1),
  Reflect.set(target, 0, 1),
  (() => {
    "use strict";
    try { target.strictName = 1; return false; }
    catch (error) { return error instanceof TypeError; }
  })(),
  (() => {
    "use strict";
    try { target[2] = 1; return false; }
    catch (error) { return error instanceof TypeError; }
  })()
])
"#,
    );
    assert_eq!(
        result.to_rust_string_lossy(caller_scope),
        "[false,false,true,true]"
    );
}

#[test]
fn nullable_object_and_function_template_handles_remain_empty_v8_locals() {
    moli_v8_test_util::ensure_v8();
    let mut isolate = v8::Isolate::new(v8::CreateParams::default());
    let scope = pin!(v8::HandleScope::new(&mut isolate));
    let scope = &mut scope.init();
    let context = v8::Context::new(scope, Default::default());
    let scope = &mut v8::ContextScope::new(scope, context);

    let getter = v8::FunctionTemplate::new(scope, return_42);
    let setter = v8::FunctionTemplate::new(scope, record_setter_value);

    let object_template = v8::ObjectTemplate::new(scope);
    let getter_only = v8::String::new(scope, "getterOnly").unwrap();
    object_template.set_accessor_property(
        getter_only.into(),
        Some(getter),
        None,
        v8::PropertyAttribute::NONE,
    );
    let setter_only = v8::String::new(scope, "setterOnly").unwrap();
    object_template.set_accessor_property(
        setter_only.into(),
        None,
        Some(setter),
        v8::PropertyAttribute::NONE,
    );
    let object = object_template
        .new_instance(scope)
        .expect("object template with nullable accessors should instantiate");
    let object_name = v8::String::new(scope, "object").unwrap();
    assert_eq!(
        context
            .global(scope)
            .set(scope, object_name.into(), object.into()),
        Some(true)
    );

    let function_template = v8::FunctionTemplate::new(scope, empty_constructor);
    let getter_only = v8::String::new(scope, "getterOnly").unwrap();
    function_template.set_accessor_property(
        getter_only.into(),
        Some(getter),
        None,
        v8::PropertyAttribute::NONE,
    );
    let setter_only = v8::String::new(scope, "setterOnly").unwrap();
    function_template.set_accessor_property(
        setter_only.into(),
        None,
        Some(setter),
        v8::PropertyAttribute::NONE,
    );
    let function = function_template
        .get_function(scope)
        .expect("function template with nullable accessors should instantiate");
    let function_name = v8::String::new(scope, "TemplateFunction").unwrap();
    assert_eq!(
        context
            .global(scope)
            .set(scope, function_name.into(), function.into()),
        Some(true)
    );

    let result = eval(
        scope,
        r#"
JSON.stringify([
  object.getterOnly,
  (object.setterOnly = 7, object.setterValue),
  TemplateFunction.getterOnly,
  (TemplateFunction.setterOnly = 9, TemplateFunction.setterValue)
])
"#,
    );
    assert_eq!(result.to_rust_string_lossy(scope), "[42,7,42,9]");
}
