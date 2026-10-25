//! WKWebView's native completion and the existing tagged result transport.

use block2::RcBlock;
use explorerkit_embedder::{HostEvent, JavaScriptEvaluationErrorKind};
use objc2::runtime::AnyObject;
use objc2_core_foundation::{CFBoolean, CFType};
use objc2_foundation::{
    NSArray, NSDictionary, NSError, NSJSONSerialization, NSNull, NSNumber, NSString,
};
use objc2_web_kit::{WKErrorCode, WKErrorDomain};
use serde_json::{Map, Number, Value};
use wry::{WebView, WebViewExtMacOS};

use super::SystemCallbackSink;

#[derive(Debug, PartialEq, Eq)]
pub(super) enum NativeEvaluationError {
    ProcessTerminated,
    ViewInvalidated,
    Result(JavaScriptEvaluationErrorKind),
}

pub(super) fn evaluate(
    native: &WebView,
    sink: SystemCallbackSink,
    id: &str,
    token: u64,
    script: &str,
) {
    let id = id.to_owned();
    let completion = RcBlock::new(move |value: *mut AnyObject, error: *mut NSError| {
        // SAFETY: WebKit supplies objects borrowed for this completion and calls
        // it on the main thread. Neither pointer escapes the callback.
        let result = unsafe {
            match error.as_ref() {
                Some(error) => Err(native_error(error)),
                None => serialize(value.as_ref()).map_err(NativeEvaluationError::Result),
            }
        };
        sink.complete_evaluation(&id, token, result);
    });
    // SAFETY: the host checks the AppKit thread and committed-document eligibility.
    // Use WRY's retained WKWebView; WebKit copies the block for asynchronous use.
    // The block retains only the callback sink, never the WKWebView or its owner.
    unsafe {
        native
            .webview()
            .evaluateJavaScript_completionHandler(&NSString::from_str(script), Some(&completion));
    }
}

pub(super) fn native_error(error: &NSError) -> NativeEvaluationError {
    use JavaScriptEvaluationErrorKind as Kind;
    // SAFETY: WKErrorDomain is WebKit's immutable framework constant.
    if &*error.domain() != unsafe { WKErrorDomain } {
        return NativeEvaluationError::Result(Kind::InternalError);
    }
    match WKErrorCode(error.code()) {
        WKErrorCode::WebContentProcessTerminated => NativeEvaluationError::ProcessTerminated,
        WKErrorCode::WebViewInvalidated => NativeEvaluationError::ViewInvalidated,
        WKErrorCode::JavaScriptExceptionOccurred => {
            NativeEvaluationError::Result(Kind::EvaluationFailure)
        }
        WKErrorCode::JavaScriptResultTypeIsUnsupported => {
            NativeEvaluationError::Result(Kind::SerializationError)
        }
        _ => NativeEvaluationError::Result(Kind::InternalError),
    }
}

fn serialize(value: Option<&AnyObject>) -> Result<String, JavaScriptEvaluationErrorKind> {
    if let Some(value) = value {
        // WebKit can bridge cyclic containers without a native evaluation error.
        // Validate with Foundation before walking the graph: its JSON rules reject
        // cycles, unsupported values/keys and excessive nesting. The envelope lets
        // the same native validation accept top-level scalars as well as containers.
        let envelope = NSArray::from_slice(&[value]);
        // SAFETY: envelope is an NSArray retaining the borrowed native result.
        if !unsafe { NSJSONSerialization::isValidJSONObject(&envelope) } {
            return Err(JavaScriptEvaluationErrorKind::SerializationError);
        }
    }
    serde_json::to_string(&tagged_value(value)?)
        .map_err(|_| JavaScriptEvaluationErrorKind::SerializationError)
}

fn tagged_value(value: Option<&AnyObject>) -> Result<Value, JavaScriptEvaluationErrorKind> {
    use JavaScriptEvaluationErrorKind::SerializationError;
    let Some(value) = value else {
        return Ok(tag("null", None));
    };
    if value.downcast_ref::<NSNull>().is_some() {
        return Ok(tag("null", None));
    }
    if let Some(string) = value.downcast_ref::<NSString>() {
        return Ok(tag("string", Some(Value::String(string.to_string()))));
    }
    if let Some(number) = value.downcast_ref::<NSNumber>() {
        // SAFETY: Foundation NSNumber is toll-free bridged to CFNumber/CFBoolean.
        // Use the CF type ID, since ObjC encodings cannot distinguish bool and i8.
        let cf = unsafe { &*(number as *const NSNumber).cast::<CFType>() };
        if let Some(boolean) = cf.downcast_ref::<CFBoolean>() {
            return Ok(tag("boolean", Some(Value::Bool(boolean.as_bool()))));
        }
        let number = Number::from_f64(number.as_f64()).ok_or(SerializationError)?;
        return Ok(tag("number", Some(Value::Number(number))));
    }
    if let Some(array) = value.downcast_ref::<NSArray>() {
        let mut values = Vec::with_capacity(array.len());
        for value in array {
            values.push(tagged_value(Some(&value))?);
        }
        return Ok(tag("array", Some(Value::Array(values))));
    }
    if let Some(object) = value.downcast_ref::<NSDictionary>() {
        let (keys, values) = object.to_vecs();
        let mut entries = Map::new();
        for (key, value) in keys.iter().zip(&values) {
            let key = key.downcast_ref::<NSString>().ok_or(SerializationError)?;
            entries.insert(key.to_string(), tagged_value(Some(value))?);
        }
        return Ok(tag("object", Some(Value::Object(entries))));
    }
    Err(SerializationError)
}

fn tag(kind: &str, value: Option<Value>) -> Value {
    let mut object = Map::new();
    object.insert("type".to_owned(), Value::String(kind.to_owned()));
    if let Some(value) = value {
        object.insert("value".to_owned(), value);
    }
    Value::Object(object)
}

pub(super) fn evaluation_event(
    id: impl Into<String>,
    result: Result<String, JavaScriptEvaluationErrorKind>,
) -> HostEvent {
    let (ok, value_json, error_type) = match result {
        Ok(value) => (true, Some(value), None),
        Err(error) => (false, None, Some(error)),
    };
    HostEvent::JavaScriptEvaluationResult {
        evaluation_id: id.into(),
        ok,
        value_json,
        error_type,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use objc2_foundation::{ns_string, NSMutableArray, NSMutableDictionary, NSObject};
    use serde_json::json;

    #[test]
    fn webkit_values_use_the_existing_recursive_tags() {
        let null = NSNull::null();
        let boolean = NSNumber::new_bool(true);
        let number = NSNumber::new_f64(42.5);
        let zero = NSNumber::new_i8(0);
        let array = NSArray::from_slice(&[
            &*null as &AnyObject,
            &*boolean,
            &*number,
            &*zero,
            ns_string!(""),
            ns_string!("DOM \"snapshot\"\n雪"),
        ]);
        let object = NSDictionary::from_slices(&[ns_string!("data")], &[&*array]);
        let encoded = serialize(Some(&object)).unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&encoded).unwrap(),
            json!({"type":"object", "value": {
                "data": {"type":"array", "value": [
                    {"type":"null"},
                    {"type":"boolean", "value":true},
                    {"type":"number", "value":42.5},
                    {"type":"number", "value":0.0},
                    {"type":"string", "value":""},
                    {"type":"string", "value":"DOM \"snapshot\"\n雪"}
                ]}
            }})
        );
        assert_eq!(serialize(None).unwrap(), r#"{"type":"null"}"#);
    }

    #[test]
    fn unsupported_native_values_never_become_success_sentinels() {
        for number in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            let number = NSNumber::new_f64(number);
            assert_eq!(
                serialize(Some(&number)),
                Err(JavaScriptEvaluationErrorKind::SerializationError)
            );
        }
        let unsupported = NSObject::new();
        let non_string_key = NSNumber::new_i32(1);
        let dictionary = NSDictionary::from_slices(&[&*non_string_key], &[ns_string!("value")]);
        for value in [&*unsupported as &AnyObject, &*dictionary] {
            assert_eq!(
                serialize(Some(value)),
                Err(JavaScriptEvaluationErrorKind::SerializationError)
            );
        }
    }

    #[test]
    fn native_json_validation_rejects_cycles_but_preserves_shared_values() {
        let array = NSMutableArray::<AnyObject>::new();
        array.addObject(&array);
        let encoded = serialize(Some(&array));
        array.removeAllObjects(); // Break the native retain cycle owned by this test.
        assert_eq!(
            encoded,
            Err(JavaScriptEvaluationErrorKind::SerializationError)
        );

        let object = NSMutableDictionary::<NSString, AnyObject>::new();
        object.insert(ns_string!("self"), &object);
        let encoded = serialize(Some(&object));
        object.removeAllObjects();
        assert_eq!(
            encoded,
            Err(JavaScriptEvaluationErrorKind::SerializationError)
        );

        let child = NSDictionary::from_slices(&[ns_string!("text")], &[ns_string!("shared")]);
        let shared = NSArray::from_slice(&[&*child, &*child]);
        let encoded = serialize(Some(&shared)).unwrap();
        let parsed: Value = serde_json::from_str(&encoded).unwrap();
        assert_eq!(parsed["value"][0], parsed["value"][1]);
        assert_eq!(parsed["value"][0]["value"]["text"]["value"], "shared");
    }

    #[test]
    fn native_json_nesting_limits_fail_without_recursive_conversion() {
        let mut value: objc2::rc::Retained<AnyObject> = NSNull::null().into_super().into_super();
        for depth in 1..=512 {
            value = NSArray::from_slice(&[&*value]).into_super().into_super();
            let envelope = NSArray::from_slice(&[&*value]);
            // SAFETY: envelope retains a native Foundation array tree.
            if unsafe { NSJSONSerialization::isValidJSONObject(&envelope) } {
                assert!(serialize(Some(&value)).is_ok(), "native JSON depth {depth}");
            } else {
                assert_eq!(
                    serialize(Some(&value)),
                    Err(JavaScriptEvaluationErrorKind::SerializationError),
                    "native JSON depth {depth}"
                );
            }
        }
    }

    #[test]
    fn native_error_domain_and_code_determine_the_failure() {
        use JavaScriptEvaluationErrorKind as Kind;
        let cases = [
            (
                WKErrorCode::WebContentProcessTerminated,
                NativeEvaluationError::ProcessTerminated,
            ),
            (
                WKErrorCode::WebViewInvalidated,
                NativeEvaluationError::ViewInvalidated,
            ),
            (
                WKErrorCode::JavaScriptExceptionOccurred,
                NativeEvaluationError::Result(Kind::EvaluationFailure),
            ),
            (
                WKErrorCode::JavaScriptResultTypeIsUnsupported,
                NativeEvaluationError::Result(Kind::SerializationError),
            ),
            (
                WKErrorCode::Unknown,
                NativeEvaluationError::Result(Kind::InternalError),
            ),
        ];
        for (code, expected) in cases {
            // SAFETY: both domains are immutable strings and no user-info object is supplied.
            let error =
                unsafe { NSError::errorWithDomain_code_userInfo(WKErrorDomain, code.0, None) };
            assert_eq!(native_error(&error), expected);
            let other = unsafe {
                NSError::errorWithDomain_code_userInfo(ns_string!("OtherDomain"), code.0, None)
            };
            assert_eq!(
                native_error(&other),
                NativeEvaluationError::Result(Kind::InternalError)
            );
        }
    }
}
