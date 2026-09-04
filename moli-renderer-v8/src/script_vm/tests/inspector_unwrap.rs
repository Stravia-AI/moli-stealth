use std::cell::RefCell;
use std::pin::pin;
use std::rc::Rc;

#[derive(Default)]
struct RecordingChannel {
    responses: Rc<RefCell<Vec<(i32, String)>>>,
    notifications: Rc<RefCell<Vec<String>>>,
}

impl RecordingChannel {
    fn new(
        responses: Rc<RefCell<Vec<(i32, String)>>>,
        notifications: Rc<RefCell<Vec<String>>>,
    ) -> Self {
        Self {
            responses,
            notifications,
        }
    }
}

impl v8::inspector::ChannelImpl for RecordingChannel {
    fn send_response(&self, call_id: i32, message: v8::UniquePtr<v8::inspector::StringBuffer>) {
        let message = message
            .as_ref()
            .map(|message| format!("{}", message.string()))
            .unwrap_or_default();
        self.responses.borrow_mut().push((call_id, message));
    }

    fn send_notification(&self, message: v8::UniquePtr<v8::inspector::StringBuffer>) {
        let message = message
            .as_ref()
            .map(|message| format!("{}", message.string()))
            .unwrap_or_default();
        self.notifications.borrow_mut().push(message);
    }

    fn flush_protocol_notifications(&self) {}
}

struct TestInspectorClient;

impl v8::inspector::V8InspectorClientImpl for TestInspectorClient {}

#[test]
fn unwrap_object_returns_live_value_and_context() {
    crate::ensure_v8_for_test();

    let mut isolate = v8::Isolate::new(Default::default());
    let inspector_client = v8::inspector::V8InspectorClient::new(Box::new(TestInspectorClient));
    let inspector = v8::inspector::V8Inspector::create(&mut isolate, inspector_client);
    let responses = Rc::new(RefCell::new(Vec::new()));
    let notifications = Rc::new(RefCell::new(Vec::new()));
    let channel = v8::inspector::Channel::new(Box::new(RecordingChannel::new(
        responses.clone(),
        notifications.clone(),
    )));
    let session = inspector.connect(
        1,
        channel,
        v8::inspector::StringView::empty(),
        v8::inspector::V8InspectorClientTrustLevel::FullyTrusted,
    );

    let scope = pin!(v8::HandleScope::new(&mut isolate));
    let scope = &mut scope.init();
    let context = v8::Context::new(scope, Default::default());
    let scope = &mut v8::ContextScope::new(scope, context);
    inspector.context_created(
        context,
        1,
        v8::inspector::StringView::from(&b"test"[..]),
        v8::inspector::StringView::empty(),
        v8::inspector::StringView::from(&b"{}"[..]),
    );
    let execution_context_id = v8::inspector::V8Inspector::execution_context_id(context);

    let request = format!(
        r#"{{"id":1,"method":"Runtime.evaluate","params":{{"expression":"({{ answer: 42 }})","contextId":{},"objectGroup":"unwrap-test"}}}}"#,
        execution_context_id
    );
    session.dispatch_protocol_message(v8::inspector::StringView::from(request.as_bytes()));

    let response = responses
        .borrow()
        .iter()
        .find(|(call_id, _)| *call_id == 1)
        .map(|(_, message)| message.clone())
        .expect("Runtime.evaluate should respond synchronously");
    let object_id = extract_json_string_field(&response, "objectId")
        .unwrap_or_else(|| panic!("response should contain objectId: {response}"));

    let (value, context, mut object_group) = session
        .unwrap_object(scope, v8::inspector::StringView::from(object_id.as_bytes()))
        .unwrap_or_else(|error| {
            let message = error
                .as_ref()
                .map(|error| format!("{}", error.string()))
                .unwrap_or_default();
            panic!("objectId should unwrap: {message}");
        });
    assert!(value.is_object());
    assert_eq!(
        v8::inspector::V8Inspector::execution_context_id(context),
        execution_context_id
    );
    assert_eq!(
        object_group
            .as_mut()
            .map(|group| format!("{}", group.string()))
            .as_deref(),
        Some("unwrap-test")
    );

    let object = value
        .try_cast::<v8::Object>()
        .expect("unwrapped value should be an object");
    let answer_key: v8::Local<'_, v8::Value> = v8::String::new(scope, "answer").unwrap().into();
    let answer = object
        .get(scope, answer_key)
        .expect("answer property should read");
    assert_eq!(answer.integer_value(scope), Some(42));

    let error = session
        .unwrap_object(
            scope,
            v8::inspector::StringView::from(&b"not-a-valid-object-id"[..]),
        )
        .expect_err("invalid objectId should fail");
    let message = error
        .as_ref()
        .map(|error| format!("{}", error.string()))
        .unwrap_or_default();
    assert!(!message.is_empty());
}

fn extract_json_string_field(json: &str, field: &str) -> Option<String> {
    let needle = format!("\"{field}\":\"");
    let mut chars = json[json.find(&needle)? + needle.len()..].chars();
    let mut result = String::new();
    while let Some(ch) = chars.next() {
        match ch {
            '"' => return Some(result),
            '\\' => result.push(chars.next()?),
            _ => result.push(ch),
        }
    }
    None
}
