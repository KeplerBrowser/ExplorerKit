use std::error::Error;
use std::fmt;

use serde_json::{Map, Value};

use crate::{ContextMenuAction, NavigationRequest};

/// Rust-owned mounted-controller commands and control responses.
///
/// Native adapters transport the opaque JSON envelope. Command names, payload validation,
/// request identifiers, and fallback-facing semantics stay in Rust.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ControllerCommand {
    LoadUrl(NavigationRequest),
    Reload,
    StopLoading,
    GoBack,
    GoForward,
    Focus,
    Blur,
    EvaluateJavaScript {
        evaluation_id: String,
        script: String,
    },
    ResolveNavigationRequest {
        navigation_id: String,
        allow: bool,
    },
    ResolveSimpleDialog {
        dialog_id: String,
        confirmed: bool,
        prompt_value: Option<String>,
    },
    ResolveContextMenu {
        context_menu_id: String,
        action: ContextMenuAction,
    },
    DismissContextMenu {
        context_menu_id: String,
    },
}

impl ControllerCommand {
    pub fn from_json(command_json: &str) -> Result<Self, ControllerCommandError> {
        let value: Value = serde_json::from_str(command_json).map_err(|error| {
            ControllerCommandError::InvalidEnvelope(format!(
                "controller command JSON is invalid: {error}"
            ))
        })?;
        let object = value.as_object().ok_or_else(|| {
            ControllerCommandError::InvalidEnvelope(
                "controller command envelope must be a JSON object".to_owned(),
            )
        })?;

        Self::from_object(object)
    }

    pub(crate) fn from_object(object: &Map<String, Value>) -> Result<Self, ControllerCommandError> {
        validate_version(object)?;
        let command = required_string_field(object, "command")?;

        match command.as_str() {
            "loadUrl" => Ok(Self::LoadUrl(
                NavigationRequest::new(&required_string_field(object, "url")?)
                    .map_err(|error| ControllerCommandError::InvalidUrl(error.to_string()))?,
            )),
            "reload" => Ok(Self::Reload),
            "stopLoading" => Ok(Self::StopLoading),
            "goBack" => Ok(Self::GoBack),
            "goForward" => Ok(Self::GoForward),
            "focus" => Ok(Self::Focus),
            "blur" => Ok(Self::Blur),
            "evaluateJavaScript" => Ok(Self::EvaluateJavaScript {
                evaluation_id: required_non_empty_string_field(object, "evaluationId")?,
                script: required_string_field(object, "script")?,
            }),
            "resolveNavigationRequest" => Ok(Self::ResolveNavigationRequest {
                navigation_id: required_non_empty_string_field(object, "navigationId")?,
                allow: required_bool_field(object, "allow")?,
            }),
            "resolveSimpleDialog" => Ok(Self::ResolveSimpleDialog {
                dialog_id: required_non_empty_string_field(object, "dialogId")?,
                confirmed: required_bool_field(object, "confirmed")?,
                prompt_value: optional_string_field(object, "promptValue")?,
            }),
            "resolveContextMenu" => {
                let action = required_string_field(object, "action")?;
                let action = ContextMenuAction::from_str(&action).ok_or_else(|| {
                    ControllerCommandError::InvalidEnvelope(format!(
                        "action {action:?} is unsupported for resolveContextMenu"
                    ))
                })?;
                Ok(Self::ResolveContextMenu {
                    context_menu_id: required_non_empty_string_field(object, "contextMenuId")?,
                    action,
                })
            }
            "dismissContextMenu" => Ok(Self::DismissContextMenu {
                context_menu_id: required_non_empty_string_field(object, "contextMenuId")?,
            }),
            other => Err(ControllerCommandError::InvalidEnvelope(format!(
                "controller command {other:?} is unsupported"
            ))),
        }
    }

    pub fn validate(&self) -> Result<(), ControllerCommandError> {
        match self {
            Self::LoadUrl(_)
            | Self::Reload
            | Self::StopLoading
            | Self::GoBack
            | Self::GoForward
            | Self::Focus
            | Self::Blur => Ok(()),
            Self::EvaluateJavaScript { evaluation_id, .. } => {
                validate_non_empty_identifier("evaluationId", evaluation_id)
            }
            Self::ResolveNavigationRequest { navigation_id, .. } => {
                validate_non_empty_identifier("navigationId", navigation_id)
            }
            Self::ResolveSimpleDialog { dialog_id, .. } => {
                validate_non_empty_identifier("dialogId", dialog_id)
            }
            Self::ResolveContextMenu {
                context_menu_id, ..
            }
            | Self::DismissContextMenu { context_menu_id } => {
                validate_non_empty_identifier("contextMenuId", context_menu_id)
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ControllerCommandError {
    InvalidControllerHandle,
    ExpiredControllerHandle,
    InvalidUrl(String),
    InvalidEnvelope(String),
    Backend(String),
}

impl fmt::Display for ControllerCommandError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidControllerHandle => formatter.write_str("controller handle is invalid"),
            Self::ExpiredControllerHandle => {
                formatter.write_str("controller handle is no longer valid")
            }
            Self::InvalidUrl(message) | Self::InvalidEnvelope(message) | Self::Backend(message) => {
                formatter.write_str(message)
            }
        }
    }
}

impl Error for ControllerCommandError {}

fn validate_non_empty_identifier(key: &str, value: &str) -> Result<(), ControllerCommandError> {
    if value.trim().is_empty() {
        return Err(ControllerCommandError::InvalidEnvelope(format!(
            "{key} must not be empty"
        )));
    }
    Ok(())
}

fn validate_version(object: &Map<String, Value>) -> Result<(), ControllerCommandError> {
    let Some(version) = object.get("version") else {
        return Ok(());
    };
    if version.as_u64() == Some(1) {
        return Ok(());
    }
    Err(ControllerCommandError::InvalidEnvelope(
        "version must be 1 when provided".to_owned(),
    ))
}

fn required_string_field(
    object: &Map<String, Value>,
    key: &str,
) -> Result<String, ControllerCommandError> {
    match object.get(key) {
        Some(Value::String(value)) => Ok(value.clone()),
        Some(_) => Err(ControllerCommandError::InvalidEnvelope(format!(
            "{key} must be a string"
        ))),
        None => Err(ControllerCommandError::InvalidEnvelope(format!(
            "{key} is required"
        ))),
    }
}

fn required_non_empty_string_field(
    object: &Map<String, Value>,
    key: &str,
) -> Result<String, ControllerCommandError> {
    let value = required_string_field(object, key)?;
    validate_non_empty_identifier(key, &value)?;
    Ok(value)
}

fn optional_string_field(
    object: &Map<String, Value>,
    key: &str,
) -> Result<Option<String>, ControllerCommandError> {
    match object.get(key) {
        Some(Value::String(value)) => Ok(Some(value.clone())),
        Some(Value::Null) | None => Ok(None),
        Some(_) => Err(ControllerCommandError::InvalidEnvelope(format!(
            "{key} must be a string or null"
        ))),
    }
}

fn required_bool_field(
    object: &Map<String, Value>,
    key: &str,
) -> Result<bool, ControllerCommandError> {
    match object.get(key) {
        Some(Value::Bool(value)) => Ok(*value),
        Some(_) => Err(ControllerCommandError::InvalidEnvelope(format!(
            "{key} must be a boolean"
        ))),
        None => Err(ControllerCommandError::InvalidEnvelope(format!(
            "{key} is required"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_baseline_command_envelopes() {
        assert_eq!(
            ControllerCommand::from_json(r#"{"command":"reload"}"#),
            Ok(ControllerCommand::Reload)
        );
        assert_eq!(
            ControllerCommand::from_json(r#"{"version":1,"command":"goBack"}"#),
            Ok(ControllerCommand::GoBack)
        );
        assert_eq!(
            ControllerCommand::from_json(r#"{"command":"loadUrl","url":"example.com"}"#),
            Ok(ControllerCommand::LoadUrl(
                NavigationRequest::new("example.com").expect("url should normalize")
            ))
        );
        assert_eq!(
            ControllerCommand::from_json(
                r#"{"command":"evaluateJavaScript","evaluationId":"evaluation-1","script":"document.title"}"#
            ),
            Ok(ControllerCommand::EvaluateJavaScript {
                evaluation_id: "evaluation-1".to_owned(),
                script: "document.title".to_owned(),
            })
        );
    }

    #[test]
    fn parses_control_response_envelopes() {
        assert_eq!(
            ControllerCommand::from_json(
                r#"{"command":"resolveNavigationRequest","navigationId":"navigation-1","allow":false}"#
            ),
            Ok(ControllerCommand::ResolveNavigationRequest {
                navigation_id: "navigation-1".to_owned(),
                allow: false,
            })
        );
        assert_eq!(
            ControllerCommand::from_json(
                r#"{"command":"resolveSimpleDialog","dialogId":"dialog-1","confirmed":true,"promptValue":"Servo"}"#
            ),
            Ok(ControllerCommand::ResolveSimpleDialog {
                dialog_id: "dialog-1".to_owned(),
                confirmed: true,
                prompt_value: Some("Servo".to_owned()),
            })
        );
        assert_eq!(
            ControllerCommand::from_json(
                r#"{"command":"resolveContextMenu","contextMenuId":"context-menu-1","action":"copy-link"}"#
            ),
            Ok(ControllerCommand::ResolveContextMenu {
                context_menu_id: "context-menu-1".to_owned(),
                action: ContextMenuAction::CopyLink,
            })
        );
        assert_eq!(
            ControllerCommand::from_json(
                r#"{"command":"dismissContextMenu","contextMenuId":"context-menu-1"}"#
            ),
            Ok(ControllerCommand::DismissContextMenu {
                context_menu_id: "context-menu-1".to_owned(),
            })
        );
    }

    #[test]
    fn stop_loading_uses_the_shared_parser_without_changing_extra_field_handling() {
        for input in [
            r#"{"version":1,"command":"stopLoading"}"#,
            r#"{"version":1,"command":"stopLoading","extra":true}"#,
        ] {
            let command = ControllerCommand::from_json(input).unwrap();
            assert_eq!(command, ControllerCommand::StopLoading);
            command.validate().unwrap();
        }
    }

    #[test]
    fn rejects_invalid_controller_command_envelopes() {
        assert_eq!(
            ControllerCommand::from_json(r#"{"version":2,"command":"reload"}"#),
            Err(ControllerCommandError::InvalidEnvelope(
                "version must be 1 when provided".to_owned(),
            ))
        );
        assert_eq!(
            ControllerCommand::from_json(
                r#"{"command":"resolveNavigationRequest","navigationId":"","allow":true}"#
            ),
            Err(ControllerCommandError::InvalidEnvelope(
                "navigationId must not be empty".to_owned(),
            ))
        );
        assert_eq!(
            ControllerCommand::from_json(
                r#"{"command":"evaluateJavaScript","evaluationId":"   ","script":"1 + 1"}"#
            ),
            Err(ControllerCommandError::InvalidEnvelope(
                "evaluationId must not be empty".to_owned(),
            ))
        );
        assert_eq!(
            ControllerCommand::from_json(
                r#"{"command":"resolveContextMenu","contextMenuId":"context-menu-1","action":"unsupported"}"#
            ),
            Err(ControllerCommandError::InvalidEnvelope(
                "action \"unsupported\" is unsupported for resolveContextMenu".to_owned(),
            ))
        );
    }

    #[test]
    fn rejects_invalid_load_url_envelopes() {
        assert!(matches!(
            ControllerCommand::from_json(r#"{"command":"loadUrl","url":"https:///"}"#),
            Err(ControllerCommandError::InvalidUrl(message)) if message.contains("invalid url")
        ));
    }
}
