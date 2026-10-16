use crate::{HostEvent, JavaScriptEvaluationErrorKind};
use serde_json::{Map, Number, Value};
use servo::{JSValue, JavaScriptEvaluationError};

pub fn host_event_from_javascript_evaluation_result(
    evaluation_id: impl Into<String>,
    result: Result<JSValue, JavaScriptEvaluationError>,
) -> HostEvent {
    let evaluation_id = evaluation_id.into();

    match result {
        Ok(value) => match serialize_javascript_value_json(&value) {
            Ok(value_json) => HostEvent::JavaScriptEvaluationResult {
                evaluation_id,
                ok: true,
                value_json: Some(value_json),
                error_type: None,
            },
            Err(()) => HostEvent::JavaScriptEvaluationResult {
                evaluation_id,
                ok: false,
                value_json: None,
                error_type: Some(JavaScriptEvaluationErrorKind::SerializationError),
            },
        },
        Err(error) => HostEvent::JavaScriptEvaluationResult {
            evaluation_id,
            ok: false,
            value_json: None,
            error_type: Some(map_javascript_evaluation_error(&error)),
        },
    }
}

#[allow(
    clippy::result_unit_err,
    reason = "Preserve the existing serialization failure contract."
)]
pub fn serialize_javascript_value_json(value: &JSValue) -> Result<String, ()> {
    serde_json::to_string(&serialize_javascript_value(value)?).map_err(|_| ())
}

fn map_javascript_evaluation_error(
    error: &JavaScriptEvaluationError,
) -> JavaScriptEvaluationErrorKind {
    match error {
        JavaScriptEvaluationError::DocumentNotFound => {
            JavaScriptEvaluationErrorKind::DocumentNotFound
        }
        JavaScriptEvaluationError::CompilationFailure => {
            JavaScriptEvaluationErrorKind::CompilationFailure
        }
        JavaScriptEvaluationError::EvaluationFailure(_) => {
            JavaScriptEvaluationErrorKind::EvaluationFailure
        }
        JavaScriptEvaluationError::InternalError => JavaScriptEvaluationErrorKind::InternalError,
        JavaScriptEvaluationError::WebViewNotReady => {
            JavaScriptEvaluationErrorKind::WebViewNotReady
        }
        JavaScriptEvaluationError::SerializationError(_) => {
            JavaScriptEvaluationErrorKind::SerializationError
        }
    }
}

fn serialize_javascript_value(value: &JSValue) -> Result<Value, ()> {
    match value {
        JSValue::Undefined => Ok(tagged_value("undefined")),
        JSValue::Null => Ok(tagged_value("null")),
        JSValue::Boolean(value) => Ok(tagged_scalar_value("boolean", Value::Bool(*value))),
        JSValue::Number(value) => {
            let Some(number) = Number::from_f64(*value) else {
                return Err(());
            };
            Ok(tagged_scalar_value("number", Value::Number(number)))
        }
        JSValue::String(value) => Ok(tagged_scalar_value("string", Value::String(value.clone()))),
        JSValue::Element(value) => Ok(tagged_scalar_value("element", Value::String(value.clone()))),
        JSValue::ShadowRoot(value) => Ok(tagged_scalar_value(
            "shadowRoot",
            Value::String(value.clone()),
        )),
        JSValue::Frame(value) => Ok(tagged_scalar_value("frame", Value::String(value.clone()))),
        JSValue::Window(value) => Ok(tagged_scalar_value("window", Value::String(value.clone()))),
        JSValue::Array(values) => Ok(tagged_scalar_value(
            "array",
            Value::Array(
                values
                    .iter()
                    .map(serialize_javascript_value)
                    .collect::<Result<Vec<_>, _>>()?,
            ),
        )),
        JSValue::Object(values) => {
            let mut entries: Vec<_> = values.iter().collect();
            entries.sort_by_key(|(key, _)| *key);

            let mut object_value = Map::new();
            for (key, value) in entries {
                object_value.insert(key.clone(), serialize_javascript_value(value)?);
            }

            Ok(tagged_scalar_value("object", Value::Object(object_value)))
        }
    }
}

fn tagged_value(tag: &str) -> Value {
    let mut object = Map::new();
    object.insert("type".to_owned(), Value::String(tag.to_owned()));
    Value::Object(object)
}

fn tagged_scalar_value(tag: &str, value: Value) -> Value {
    let mut object = Map::new();
    object.insert("type".to_owned(), Value::String(tag.to_owned()));
    object.insert("value".to_owned(), value);
    Value::Object(object)
}

#[cfg(test)]
mod tests {
    use super::*;
    use servo::{JavaScriptErrorInfo, JavaScriptEvaluationResultSerializationError};
    use std::collections::HashMap;

    #[test]
    fn serializes_javascript_values_to_stable_json() {
        let mut object = HashMap::new();
        object.insert("zeta".to_owned(), JSValue::Boolean(false));
        object.insert("alpha".to_owned(), JSValue::String("Servo".to_owned()));

        let value = JSValue::Array(vec![
            JSValue::Undefined,
            JSValue::Null,
            JSValue::Boolean(true),
            JSValue::Number(42.5),
            JSValue::String("hello".to_owned()),
            JSValue::Element("element-1".to_owned()),
            JSValue::ShadowRoot("shadow-root-1".to_owned()),
            JSValue::Frame("frame-1".to_owned()),
            JSValue::Window("window-1".to_owned()),
            JSValue::Object(object),
        ]);

        assert_eq!(
            serialize_javascript_value_json(&value),
            Ok(concat!(
                r#"{"type":"array","value":["#,
                r#"{"type":"undefined"},"#,
                r#"{"type":"null"},"#,
                r#"{"type":"boolean","value":true},"#,
                r#"{"type":"number","value":42.5},"#,
                r#"{"type":"string","value":"hello"},"#,
                r#"{"type":"element","value":"element-1"},"#,
                r#"{"type":"shadowRoot","value":"shadow-root-1"},"#,
                r#"{"type":"frame","value":"frame-1"},"#,
                r#"{"type":"window","value":"window-1"},"#,
                r#"{"type":"object","value":{"alpha":{"type":"string","value":"Servo"},"zeta":{"type":"boolean","value":false}}}"#,
                r#"]}"#
            )
            .to_owned())
        );
    }

    #[test]
    fn rejects_non_finite_numbers_for_json_transport() {
        assert_eq!(
            serialize_javascript_value_json(&JSValue::Number(f64::NAN)),
            Err(())
        );
        assert_eq!(
            serialize_javascript_value_json(&JSValue::Number(f64::INFINITY)),
            Err(())
        );
    }

    #[test]
    fn maps_servo_results_to_transport_host_events() {
        assert_eq!(
            host_event_from_javascript_evaluation_result(
                "evaluation-1",
                Ok(JSValue::String("Servo".to_owned())),
            ),
            HostEvent::JavaScriptEvaluationResult {
                evaluation_id: "evaluation-1".to_owned(),
                ok: true,
                value_json: Some(r#"{"type":"string","value":"Servo"}"#.to_owned()),
                error_type: None,
            }
        );

        assert_eq!(
            host_event_from_javascript_evaluation_result(
                "evaluation-2",
                Err(JavaScriptEvaluationError::CompilationFailure),
            ),
            HostEvent::JavaScriptEvaluationResult {
                evaluation_id: "evaluation-2".to_owned(),
                ok: false,
                value_json: None,
                error_type: Some(JavaScriptEvaluationErrorKind::CompilationFailure),
            }
        );

        assert_eq!(
            host_event_from_javascript_evaluation_result(
                "evaluation-3",
                Err(JavaScriptEvaluationError::EvaluationFailure(Some(
                    JavaScriptErrorInfo {
                        message: "boom".to_owned(),
                        filename: "https://example.com/app.js".to_owned(),
                        stack: None,
                        line_number: 12,
                        column: 4,
                    }
                ))),
            ),
            HostEvent::JavaScriptEvaluationResult {
                evaluation_id: "evaluation-3".to_owned(),
                ok: false,
                value_json: None,
                error_type: Some(JavaScriptEvaluationErrorKind::EvaluationFailure),
            }
        );

        assert_eq!(
            host_event_from_javascript_evaluation_result(
                "evaluation-4",
                Err(JavaScriptEvaluationError::SerializationError(
                    JavaScriptEvaluationResultSerializationError::DetachedShadowRoot,
                )),
            ),
            HostEvent::JavaScriptEvaluationResult {
                evaluation_id: "evaluation-4".to_owned(),
                ok: false,
                value_json: None,
                error_type: Some(JavaScriptEvaluationErrorKind::SerializationError),
            }
        );
    }

    #[test]
    fn maps_local_transport_serialization_failures_to_serialization_errors() {
        assert_eq!(
            host_event_from_javascript_evaluation_result(
                "evaluation-5",
                Ok(JSValue::Number(f64::NEG_INFINITY)),
            ),
            HostEvent::JavaScriptEvaluationResult {
                evaluation_id: "evaluation-5".to_owned(),
                ok: false,
                value_json: None,
                error_type: Some(JavaScriptEvaluationErrorKind::SerializationError),
            }
        );
    }
}
