use std::{mem, num::NonZeroU64};

use serde_json::{json, Map, Value};
use thiserror::Error;

use crate::{
    encode_host_event_bridge, ControllerCommand, HostEvent, JavaScriptEvaluationErrorKind,
    LoadStatusKind, SimpleDialogKind,
};

const NAVIGATION_FALLBACK_TIMEOUT_MS: u64 = 5_000;
const DIALOG_FALLBACK_TIMEOUT_MS: u64 = 60_000;

/// Servo-free lifecycle and pending-control reducer for native browser adapters.
#[derive(Debug)]
pub struct PortableController {
    controller_instance_id: NonZeroU64,
    generation: u64,
    next_request: u64,
    // ponytail: linear lookup keeps this state ordered and tiny; index it if pending controls
    // become high-volume.
    pending: Vec<PendingRequest>,
}

#[derive(Debug)]
struct PendingRequest {
    id: String,
    kind: PendingKind,
}

#[derive(Debug)]
enum PendingKind {
    Navigation,
    Dialog {
        kind: SimpleDialogKind,
        default_value: Option<String>,
    },
    Evaluation {
        evaluation_id: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RequestKind {
    Navigation,
    Dialog,
    Evaluation,
}

impl RequestKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Navigation => "navigation",
            Self::Dialog => "dialog",
            Self::Evaluation => "evaluation",
        }
    }
}

impl PendingKind {
    fn request_kind(&self) -> RequestKind {
        match self {
            Self::Navigation => RequestKind::Navigation,
            Self::Dialog { .. } => RequestKind::Dialog,
            Self::Evaluation { .. } => RequestKind::Evaluation,
        }
    }
}

#[derive(Debug, Default)]
pub struct PortableControllerResult {
    effects: Vec<Value>,
    events: Vec<HostEvent>,
}

impl PortableControllerResult {
    pub fn to_json(&self) -> String {
        let effects = serde_json::to_string(&self.effects)
            .expect("portable controller effects contain only JSON values");
        let events = self
            .events
            .iter()
            .map(encode_host_event_bridge)
            .collect::<Vec<_>>()
            .join(",");
        format!(r#"{{"version":1,"effects":{effects},"events":[{events}]}}"#)
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
#[error("{message}")]
pub struct PortableControllerError {
    code: &'static str,
    message: String,
    internal: bool,
}

impl PortableControllerError {
    pub fn code(&self) -> &'static str {
        self.code
    }

    pub fn is_internal(&self) -> bool {
        self.internal
    }

    pub fn to_result_json(&self) -> String {
        let code = serde_json::to_string(self.code).expect("error code is valid JSON");
        let message = serde_json::to_string(&self.message).expect("error message is valid JSON");
        format!(
            r#"{{"version":1,"effects":[],"events":[],"error":{{"code":{code},"message":{message}}}}}"#
        )
    }

    fn invalid(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            internal: false,
        }
    }

    fn internal(message: impl Into<String>) -> Self {
        Self {
            code: "internalError",
            message: message.into(),
            internal: true,
        }
    }
}

impl PortableController {
    pub fn new(controller_instance_id: NonZeroU64) -> Self {
        Self {
            controller_instance_id,
            generation: 1,
            next_request: 0,
            pending: Vec::new(),
        }
    }

    pub fn dispatch_json(
        &mut self,
        input: &str,
    ) -> Result<PortableControllerResult, PortableControllerError> {
        let value: Value = serde_json::from_str(input).map_err(|error| {
            PortableControllerError::invalid("invalidEnvelope", format!("invalid JSON: {error}"))
        })?;
        let object = value.as_object().ok_or_else(|| {
            PortableControllerError::invalid("invalidEnvelope", "input must be a JSON object")
        })?;
        validate_envelope(object)?;

        if object.contains_key("command") {
            self.dispatch_command(object)
        } else if object.contains_key("observation") {
            self.dispatch_observation(object)
        } else {
            self.dispatch_lifecycle(object)
        }
    }

    pub fn destroy(mut self) -> PortableControllerResult {
        let mut result = self.finish_pending();
        result.effects.push(json!({"name": "destroy"}));
        result
    }

    fn dispatch_command(
        &mut self,
        object: &Map<String, Value>,
    ) -> Result<PortableControllerResult, PortableControllerError> {
        let command = ControllerCommand::from_object(object).map_err(|error| {
            PortableControllerError::invalid("invalidCommand", error.to_string())
        })?;
        let mut result = PortableControllerResult::default();

        match command {
            ControllerCommand::LoadUrl(request) => {
                result
                    .effects
                    .push(json!({"name": "loadUrl", "url": request.url}));
            }
            ControllerCommand::Reload => result.effects.push(json!({"name": "reload"})),
            ControllerCommand::GoBack => result.effects.push(json!({"name": "goBack"})),
            ControllerCommand::GoForward => result.effects.push(json!({"name": "goForward"})),
            ControllerCommand::Focus => result.effects.push(json!({"name": "focus"})),
            ControllerCommand::Blur => result.effects.push(json!({"name": "blur"})),
            ControllerCommand::EvaluateJavaScript {
                evaluation_id,
                script,
            } => {
                if self.pending.iter().any(|request| {
                    matches!(
                        &request.kind,
                        PendingKind::Evaluation {
                            evaluation_id: pending_id
                        } if pending_id == &evaluation_id
                    )
                }) {
                    return Err(PortableControllerError::invalid(
                        "duplicateEvaluationId",
                        format!("evaluationId {evaluation_id:?} is already pending"),
                    ));
                }
                let request_id = self.allocate_request(RequestKind::Evaluation)?;
                self.pending.push(PendingRequest {
                    id: request_id.clone(),
                    kind: PendingKind::Evaluation {
                        evaluation_id: evaluation_id.clone(),
                    },
                });
                result.effects.push(json!({
                    "name": "evaluateJavaScript",
                    "requestId": request_id,
                    "evaluationId": evaluation_id,
                    "script": script,
                }));
            }
            ControllerCommand::ResolveNavigationRequest {
                navigation_id,
                allow,
            } => {
                self.take_pending(&navigation_id, RequestKind::Navigation)?;
                result
                    .effects
                    .push(resolve_navigation(navigation_id, allow));
            }
            ControllerCommand::ResolveSimpleDialog {
                dialog_id,
                confirmed,
                prompt_value,
            } => {
                let index = self.pending_index(&dialog_id, RequestKind::Dialog)?;
                let (kind, default_value) = match &self.pending[index].kind {
                    PendingKind::Dialog {
                        kind,
                        default_value,
                    } => (*kind, default_value.clone()),
                    _ => unreachable!("pending kind was validated"),
                };
                if kind != SimpleDialogKind::Prompt && prompt_value.is_some() {
                    return Err(PortableControllerError::invalid(
                        "invalidCommand",
                        "promptValue is only valid for prompt dialogs",
                    ));
                }
                let prompt_value = if kind == SimpleDialogKind::Prompt && confirmed {
                    Some(prompt_value.or(default_value).unwrap_or_default())
                } else {
                    None
                };
                self.pending.remove(index);
                result.effects.push(resolve_dialog(
                    dialog_id.clone(),
                    kind,
                    confirmed,
                    prompt_value,
                ));
                result
                    .events
                    .push(HostEvent::SimpleDialogDismissed { dialog_id });
            }
            ControllerCommand::StopLoading => {
                return Err(PortableControllerError::invalid(
                    "invalidCommand",
                    "native stop loading is unsupported by the portable controller",
                ));
            }
            ControllerCommand::ResolveContextMenu { .. }
            | ControllerCommand::DismissContextMenu { .. } => {
                return Err(PortableControllerError::invalid(
                    "invalidCommand",
                    "context-menu commands are not supported by the portable controller",
                ));
            }
        }

        Ok(result)
    }

    fn dispatch_observation(
        &mut self,
        object: &Map<String, Value>,
    ) -> Result<PortableControllerResult, PortableControllerError> {
        let observation = required_string(object, "observation", "invalidObservation")?;
        let mut result = PortableControllerResult::default();

        match observation.as_str() {
            "navigationRequest" => {
                let url = required_non_empty_string(object, "url", "invalidObservation")?;
                let navigation_id = self.allocate_request(RequestKind::Navigation)?;
                self.pending.push(PendingRequest {
                    id: navigation_id.clone(),
                    kind: PendingKind::Navigation,
                });
                result.effects.push(json!({
                    "name": "retainNavigationCompletion",
                    "navigationId": navigation_id,
                }));
                result.effects.push(schedule_timeout(
                    navigation_id.clone(),
                    RequestKind::Navigation,
                ));
                result
                    .events
                    .push(HostEvent::NavigationRequested { navigation_id, url });
            }
            "simpleDialogRequest" => {
                let kind =
                    parse_dialog_kind(&required_string(object, "kind", "invalidObservation")?)?;
                let message = required_string(object, "message", "invalidObservation")?;
                let default_value = optional_string(object, "defaultValue", "invalidObservation")?;
                if kind != SimpleDialogKind::Prompt && default_value.is_some() {
                    return Err(PortableControllerError::invalid(
                        "invalidObservation",
                        "defaultValue is only valid for prompt dialogs",
                    ));
                }
                let dialog_id = self.allocate_request(RequestKind::Dialog)?;
                self.pending.push(PendingRequest {
                    id: dialog_id.clone(),
                    kind: PendingKind::Dialog {
                        kind,
                        default_value: default_value.clone(),
                    },
                });
                result.effects.push(json!({
                    "name": "retainDialogCompletion",
                    "dialogId": dialog_id,
                    "kind": kind.as_str(),
                }));
                result
                    .effects
                    .push(schedule_timeout(dialog_id.clone(), RequestKind::Dialog));
                result.events.push(HostEvent::SimpleDialogRequested {
                    dialog_id,
                    kind,
                    message,
                    default_value,
                });
            }
            "javaScriptEvaluationResult" => {
                let request_id =
                    required_non_empty_string(object, "requestId", "invalidObservation")?;
                let ok = required_bool(object, "ok", "invalidObservation")?;
                let value_json = optional_string(object, "valueJson", "invalidObservation")?;
                let error_type = optional_string(object, "errorType", "invalidObservation")?;
                let error_type = validate_evaluation_result(ok, value_json.as_deref(), error_type)?;
                let pending = self.take_pending(&request_id, RequestKind::Evaluation)?;
                let PendingKind::Evaluation { evaluation_id } = pending.kind else {
                    unreachable!("pending kind was validated")
                };
                result.events.push(HostEvent::JavaScriptEvaluationResult {
                    evaluation_id,
                    ok,
                    value_json,
                    error_type,
                });
            }
            "urlChanged" => result.events.push(HostEvent::UrlChanged {
                url: required_string(object, "url", "invalidObservation")?,
            }),
            "pageTitleChanged" => result.events.push(HostEvent::PageTitleChanged {
                title: optional_string(object, "title", "invalidObservation")?,
            }),
            "loadStatusChanged" => {
                let status =
                    parse_load_status(&required_string(object, "status", "invalidObservation")?)?;
                let url = required_string(object, "url", "invalidObservation")?;
                let (entries, current, can_go_back, can_go_forward) = history_fields(object)?;
                result.events.push(HostEvent::UrlChanged { url });
                result.events.push(HostEvent::LoadStatusChanged { status });
                result.events.push(HostEvent::HistoryChanged {
                    entries,
                    current,
                    can_go_back,
                    can_go_forward,
                });
            }
            "historyChanged" => {
                let (entries, current, can_go_back, can_go_forward) = history_fields(object)?;
                result.events.push(HostEvent::HistoryChanged {
                    entries,
                    current,
                    can_go_back,
                    can_go_forward,
                });
            }
            "focusChanged" => result.events.push(HostEvent::FocusChanged {
                is_focused: required_bool(object, "isFocused", "invalidObservation")?,
            }),
            "nativeTimeout" => {
                let request_id =
                    required_non_empty_string(object, "requestId", "invalidObservation")?;
                let kind =
                    parse_timeout_kind(&required_string(object, "kind", "invalidObservation")?)?;
                let pending = self.take_pending(&request_id, kind)?;
                match pending.kind {
                    PendingKind::Navigation => {
                        result.effects.push(resolve_navigation(request_id, true));
                    }
                    PendingKind::Dialog { kind, .. } => {
                        let (confirmed, prompt_value) = default_dialog_resolution(kind);
                        result.effects.push(resolve_dialog(
                            request_id.clone(),
                            kind,
                            confirmed,
                            prompt_value,
                        ));
                        result.events.push(HostEvent::SimpleDialogDismissed {
                            dialog_id: request_id,
                        });
                    }
                    PendingKind::Evaluation { .. } => {
                        unreachable!("evaluation is not a timeout kind")
                    }
                }
            }
            other => {
                return Err(PortableControllerError::invalid(
                    "invalidObservation",
                    format!("observation {other:?} is unsupported"),
                ));
            }
        }

        Ok(result)
    }

    fn dispatch_lifecycle(
        &mut self,
        object: &Map<String, Value>,
    ) -> Result<PortableControllerResult, PortableControllerError> {
        let lifecycle = required_string(object, "lifecycle", "invalidLifecycle")?;
        if lifecycle != "reset" {
            return Err(PortableControllerError::invalid(
                "invalidLifecycle",
                format!("lifecycle {lifecycle:?} is unsupported"),
            ));
        }
        let generation = self.generation.checked_add(1).ok_or_else(|| {
            PortableControllerError::internal("controller generation space is exhausted")
        })?;
        let mut result = self.finish_pending();
        self.generation = generation;
        result
            .effects
            .push(json!({"name": "reset", "generation": generation}));
        Ok(result)
    }

    fn allocate_request(&mut self, kind: RequestKind) -> Result<String, PortableControllerError> {
        self.next_request = self.next_request.checked_add(1).ok_or_else(|| {
            PortableControllerError::internal("controller request ID space is exhausted")
        })?;
        Ok(format!(
            "{}-{}-{}-{}",
            kind.as_str(),
            self.controller_instance_id,
            self.generation,
            self.next_request
        ))
    }

    fn pending_index(
        &self,
        id: &str,
        expected: RequestKind,
    ) -> Result<usize, PortableControllerError> {
        if let Some((index, request)) = self
            .pending
            .iter()
            .enumerate()
            .find(|(_, request)| request.id == id)
        {
            let actual = request.kind.request_kind();
            if actual != expected {
                return Err(wrong_kind(id, expected, actual));
            }
            return Ok(index);
        }

        let (actual, controller_instance_id, generation) =
            parse_request_id(id).ok_or_else(|| {
                PortableControllerError::invalid(
                    "invalidRequestId",
                    format!("request ID {id:?} is malformed"),
                )
            })?;
        if controller_instance_id != self.controller_instance_id {
            return Err(PortableControllerError::invalid(
                "staleRequest",
                format!("request {id} belongs to a retired controller"),
            ));
        }
        if actual != expected {
            return Err(wrong_kind(id, expected, actual));
        }
        if generation != self.generation {
            return Err(PortableControllerError::invalid(
                "staleRequest",
                format!("request {id} belongs to an inactive generation"),
            ));
        }
        Err(PortableControllerError::invalid(
            "requestNotPending",
            format!("request {id} is no longer pending"),
        ))
    }

    fn take_pending(
        &mut self,
        id: &str,
        expected: RequestKind,
    ) -> Result<PendingRequest, PortableControllerError> {
        let index = self.pending_index(id, expected)?;
        Ok(self.pending.remove(index))
    }

    fn finish_pending(&mut self) -> PortableControllerResult {
        let mut result = PortableControllerResult::default();
        for pending in mem::take(&mut self.pending) {
            match pending.kind {
                PendingKind::Navigation => {
                    result.effects.push(resolve_navigation(pending.id, true));
                }
                PendingKind::Dialog { kind, .. } => {
                    let (confirmed, prompt_value) = default_dialog_resolution(kind);
                    result.effects.push(resolve_dialog(
                        pending.id.clone(),
                        kind,
                        confirmed,
                        prompt_value,
                    ));
                    result.events.push(HostEvent::SimpleDialogDismissed {
                        dialog_id: pending.id,
                    });
                }
                PendingKind::Evaluation { evaluation_id } => {
                    result.effects.push(json!({
                        "name": "invalidateEvaluation",
                        "requestId": pending.id,
                    }));
                    result.events.push(HostEvent::JavaScriptEvaluationResult {
                        evaluation_id,
                        ok: false,
                        value_json: None,
                        error_type: Some(JavaScriptEvaluationErrorKind::WebViewNotReady),
                    });
                }
            }
        }
        result
    }
}

fn validate_envelope(object: &Map<String, Value>) -> Result<(), PortableControllerError> {
    if object.get("version").and_then(Value::as_u64) != Some(1) {
        return Err(PortableControllerError::invalid(
            "invalidEnvelope",
            "version must be the integer 1",
        ));
    }
    let discriminators = ["command", "observation", "lifecycle"]
        .into_iter()
        .filter(|key| object.contains_key(*key))
        .count();
    if discriminators != 1 {
        return Err(PortableControllerError::invalid(
            "invalidEnvelope",
            "input must contain exactly one of command, observation, or lifecycle",
        ));
    }
    Ok(())
}

fn required_string(
    object: &Map<String, Value>,
    key: &str,
    code: &'static str,
) -> Result<String, PortableControllerError> {
    match object.get(key) {
        Some(Value::String(value)) => Ok(value.clone()),
        Some(_) => Err(PortableControllerError::invalid(
            code,
            format!("{key} must be a string"),
        )),
        None => Err(PortableControllerError::invalid(
            code,
            format!("{key} is required"),
        )),
    }
}

fn required_non_empty_string(
    object: &Map<String, Value>,
    key: &str,
    code: &'static str,
) -> Result<String, PortableControllerError> {
    let value = required_string(object, key, code)?;
    if value.trim().is_empty() {
        return Err(PortableControllerError::invalid(
            code,
            format!("{key} must not be empty"),
        ));
    }
    Ok(value)
}

fn optional_string(
    object: &Map<String, Value>,
    key: &str,
    code: &'static str,
) -> Result<Option<String>, PortableControllerError> {
    match object.get(key) {
        Some(Value::String(value)) => Ok(Some(value.clone())),
        Some(Value::Null) | None => Ok(None),
        Some(_) => Err(PortableControllerError::invalid(
            code,
            format!("{key} must be a string or null"),
        )),
    }
}

fn required_bool(
    object: &Map<String, Value>,
    key: &str,
    code: &'static str,
) -> Result<bool, PortableControllerError> {
    match object.get(key) {
        Some(Value::Bool(value)) => Ok(*value),
        Some(_) => Err(PortableControllerError::invalid(
            code,
            format!("{key} must be a boolean"),
        )),
        None => Err(PortableControllerError::invalid(
            code,
            format!("{key} is required"),
        )),
    }
}

fn history_fields(
    object: &Map<String, Value>,
) -> Result<(Vec<String>, usize, bool, bool), PortableControllerError> {
    let entries = match object.get("entries") {
        Some(Value::Array(entries)) => entries
            .iter()
            .map(|entry| {
                entry.as_str().map(str::to_owned).ok_or_else(|| {
                    PortableControllerError::invalid(
                        "invalidObservation",
                        "entries must contain only strings",
                    )
                })
            })
            .collect::<Result<Vec<_>, _>>()?,
        Some(_) => {
            return Err(PortableControllerError::invalid(
                "invalidObservation",
                "entries must be an array",
            ));
        }
        None => {
            return Err(PortableControllerError::invalid(
                "invalidObservation",
                "entries is required",
            ));
        }
    };
    let current = object
        .get("current")
        .and_then(Value::as_u64)
        .and_then(|value| usize::try_from(value).ok())
        .ok_or_else(|| {
            PortableControllerError::invalid(
                "invalidObservation",
                "current must be a nonnegative integer",
            )
        })?;
    if (entries.is_empty() && current != 0) || (!entries.is_empty() && current >= entries.len()) {
        return Err(PortableControllerError::invalid(
            "invalidObservation",
            "current must identify an entry, or be zero for empty history",
        ));
    }
    Ok((
        entries,
        current,
        required_bool(object, "canGoBack", "invalidObservation")?,
        required_bool(object, "canGoForward", "invalidObservation")?,
    ))
}

fn parse_dialog_kind(value: &str) -> Result<SimpleDialogKind, PortableControllerError> {
    match value {
        "alert" => Ok(SimpleDialogKind::Alert),
        "confirm" => Ok(SimpleDialogKind::Confirm),
        "prompt" => Ok(SimpleDialogKind::Prompt),
        _ => Err(PortableControllerError::invalid(
            "invalidObservation",
            format!("dialog kind {value:?} is unsupported"),
        )),
    }
}

fn parse_load_status(value: &str) -> Result<LoadStatusKind, PortableControllerError> {
    match value {
        "Started" => Ok(LoadStatusKind::Started),
        "HeadParsed" => Ok(LoadStatusKind::HeadParsed),
        "Complete" => Ok(LoadStatusKind::Complete),
        _ => Err(PortableControllerError::invalid(
            "invalidObservation",
            format!("load status {value:?} is unsupported"),
        )),
    }
}

fn parse_timeout_kind(value: &str) -> Result<RequestKind, PortableControllerError> {
    match value {
        "navigation" => Ok(RequestKind::Navigation),
        "dialog" => Ok(RequestKind::Dialog),
        _ => Err(PortableControllerError::invalid(
            "invalidObservation",
            format!("timeout kind {value:?} is unsupported"),
        )),
    }
}

fn parse_request_id(value: &str) -> Option<(RequestKind, NonZeroU64, u64)> {
    let mut parts = value.split('-');
    let kind = match parts.next()? {
        "navigation" => RequestKind::Navigation,
        "dialog" => RequestKind::Dialog,
        "evaluation" => RequestKind::Evaluation,
        _ => return None,
    };
    let controller_instance_id = parts.next()?.parse().ok().and_then(NonZeroU64::new)?;
    let generation = parts.next()?.parse().ok()?;
    let request = parts.next()?.parse::<u64>().ok()?;
    (generation > 0 && request > 0 && parts.next().is_none()).then_some((
        kind,
        controller_instance_id,
        generation,
    ))
}

fn wrong_kind(id: &str, expected: RequestKind, actual: RequestKind) -> PortableControllerError {
    PortableControllerError::invalid(
        "wrongRequestKind",
        format!(
            "request {id} is {}, not {}",
            actual.as_str(),
            expected.as_str()
        ),
    )
}

fn validate_evaluation_result(
    ok: bool,
    value_json: Option<&str>,
    error_type: Option<String>,
) -> Result<Option<JavaScriptEvaluationErrorKind>, PortableControllerError> {
    if ok {
        let value_json = value_json.ok_or_else(|| {
            PortableControllerError::invalid(
                "invalidObservation",
                "successful evaluation requires valueJson",
            )
        })?;
        serde_json::from_str::<Value>(value_json).map_err(|error| {
            PortableControllerError::invalid(
                "invalidObservation",
                format!("valueJson must contain valid JSON: {error}"),
            )
        })?;
        if error_type.is_some() {
            return Err(PortableControllerError::invalid(
                "invalidObservation",
                "successful evaluation must not contain errorType",
            ));
        }
        return Ok(None);
    }
    if value_json.is_some() {
        return Err(PortableControllerError::invalid(
            "invalidObservation",
            "failed evaluation must not contain valueJson",
        ));
    }
    let error_type = error_type.ok_or_else(|| {
        PortableControllerError::invalid(
            "invalidObservation",
            "failed evaluation requires errorType",
        )
    })?;
    let error_type = match error_type.as_str() {
        "DocumentNotFound" => JavaScriptEvaluationErrorKind::DocumentNotFound,
        "CompilationFailure" => JavaScriptEvaluationErrorKind::CompilationFailure,
        "EvaluationFailure" => JavaScriptEvaluationErrorKind::EvaluationFailure,
        "InternalError" => JavaScriptEvaluationErrorKind::InternalError,
        "WebViewNotReady" => JavaScriptEvaluationErrorKind::WebViewNotReady,
        "SerializationError" => JavaScriptEvaluationErrorKind::SerializationError,
        _ => {
            return Err(PortableControllerError::invalid(
                "invalidObservation",
                format!("evaluation error type {error_type:?} is unsupported"),
            ));
        }
    };
    Ok(Some(error_type))
}

fn resolve_navigation(navigation_id: String, allow: bool) -> Value {
    json!({
        "name": "resolveNavigationCompletion",
        "navigationId": navigation_id,
        "allow": allow,
    })
}

fn resolve_dialog(
    dialog_id: String,
    kind: SimpleDialogKind,
    confirmed: bool,
    prompt_value: Option<String>,
) -> Value {
    json!({
        "name": "resolveDialogCompletion",
        "dialogId": dialog_id,
        "kind": kind.as_str(),
        "confirmed": confirmed,
        "promptValue": prompt_value,
    })
}

fn schedule_timeout(request_id: String, kind: RequestKind) -> Value {
    let timeout_ms = match kind {
        RequestKind::Navigation => NAVIGATION_FALLBACK_TIMEOUT_MS,
        RequestKind::Dialog => DIALOG_FALLBACK_TIMEOUT_MS,
        RequestKind::Evaluation => unreachable!("evaluation requests do not schedule timeouts"),
    };
    json!({
        "name": "scheduleTimeout",
        "requestId": request_id,
        "kind": kind.as_str(),
        "timeoutMs": timeout_ms,
    })
}

fn default_dialog_resolution(kind: SimpleDialogKind) -> (bool, Option<String>) {
    match kind {
        SimpleDialogKind::Alert => (true, None),
        SimpleDialogKind::Confirm | SimpleDialogKind::Prompt => (false, None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn new_controller(instance: u64) -> PortableController {
        PortableController::new(NonZeroU64::new(instance).unwrap())
    }

    fn dispatch(controller: &mut PortableController, input: &str) -> Value {
        serde_json::from_str(&controller.dispatch_json(input).unwrap().to_json()).unwrap()
    }

    fn request_id(result: &Value, effect: usize, key: &str) -> String {
        result["effects"][effect][key].as_str().unwrap().to_owned()
    }

    #[test]
    fn stop_loading_is_explicitly_unsupported_without_effects_or_events() {
        let mut controller = new_controller(1);
        let error = controller
            .dispatch_json(r#"{"version":1,"command":"stopLoading"}"#)
            .unwrap_err();
        let result: Value = serde_json::from_str(&error.to_result_json()).unwrap();
        assert_eq!(result["error"]["code"], "invalidCommand");
        assert!(result["error"]["message"]
            .as_str()
            .unwrap()
            .contains("unsupported"));
        assert_eq!(result["effects"], json!([]));
        assert_eq!(result["events"], json!([]));
        assert!(controller.pending.is_empty());
    }

    #[test]
    fn locks_strict_v1_command_and_result_fixtures() {
        let mut controller = new_controller(1);
        assert_eq!(
            PortableControllerResult::default().to_json(),
            r#"{"version":1,"effects":[],"events":[]}"#
        );
        assert_eq!(
            dispatch(&mut controller, r#"{"version":1,"command":"reload"}"#),
            json!({"version": 1, "effects": [{"name": "reload"}], "events": []})
        );
        assert_eq!(
            dispatch(
                &mut controller,
                r#"{"version":1,"command":"loadUrl","url":"servo.org"}"#,
            ),
            json!({
                "version": 1,
                "effects": [{"name": "loadUrl", "url": "https://servo.org/"}],
                "events": [],
            })
        );
    }

    #[test]
    fn rejects_invalid_envelope_discrimination() {
        let invalid = [
            r#"{"command":"reload"}"#,
            r#"{"version":2,"command":"reload"}"#,
            r#"{"version":1}"#,
            r#"{"version":1,"command":"reload","lifecycle":"reset"}"#,
            r#"[]"#,
        ];
        for input in invalid {
            assert_eq!(
                new_controller(1).dispatch_json(input).unwrap_err().code(),
                "invalidEnvelope"
            );
        }
    }

    #[test]
    fn resolves_and_times_out_navigation_once() {
        let mut controller = new_controller(1);
        let requested = dispatch(
            &mut controller,
            r#"{"version":1,"observation":"navigationRequest","url":"https://servo.org/"}"#,
        );
        let first = request_id(&requested, 0, "navigationId");
        assert_eq!(first, "navigation-1-1-1");
        assert_eq!(requested["effects"][1]["name"], "scheduleTimeout");
        assert_eq!(
            requested["effects"][1]["timeoutMs"],
            NAVIGATION_FALLBACK_TIMEOUT_MS
        );
        assert_eq!(requested["events"][0]["name"], "navigationRequested");
        assert_eq!(
            dispatch(
                &mut controller,
                &format!(
                    r#"{{"version":1,"command":"resolveNavigationRequest","navigationId":"{first}","allow":false}}"#
                ),
            )["effects"][0],
            json!({
                "name": "resolveNavigationCompletion",
                "navigationId": first,
                "allow": false,
            })
        );
        assert_eq!(
            controller
                .dispatch_json(&format!(
                    r#"{{"version":1,"command":"resolveNavigationRequest","navigationId":"{first}","allow":true}}"#
                ))
                .unwrap_err()
                .code(),
            "requestNotPending"
        );

        let requested = dispatch(
            &mut controller,
            r#"{"version":1,"observation":"navigationRequest","url":"about:blank"}"#,
        );
        let second = request_id(&requested, 0, "navigationId");
        let timed_out = dispatch(
            &mut controller,
            &format!(
                r#"{{"version":1,"observation":"nativeTimeout","requestId":"{second}","kind":"navigation"}}"#
            ),
        );
        assert_eq!(timed_out["effects"][0]["allow"], true);
        assert_eq!(
            controller
                .dispatch_json(&format!(
                    r#"{{"version":1,"observation":"nativeTimeout","requestId":"{second}","kind":"navigation"}}"#
                ))
                .unwrap_err()
                .code(),
            "requestNotPending"
        );
    }

    #[test]
    fn applies_each_dialog_default_and_prompt_value() {
        for (kind, confirmed) in [("alert", true), ("confirm", false), ("prompt", false)] {
            let mut controller = new_controller(1);
            let default = if kind == "prompt" {
                r#","defaultValue":"Servo""#
            } else {
                ""
            };
            let request = dispatch(
                &mut controller,
                &format!(
                    r#"{{"version":1,"observation":"simpleDialogRequest","kind":"{kind}","message":"message"{default}}}"#
                ),
            );
            let id = request_id(&request, 0, "dialogId");
            assert_eq!(
                request["effects"][1]["timeoutMs"],
                DIALOG_FALLBACK_TIMEOUT_MS
            );
            assert_ne!(
                request["effects"][1]["timeoutMs"], NAVIGATION_FALLBACK_TIMEOUT_MS,
                "dialog fallback must remain longer than navigation fail-open"
            );
            let timed_out = dispatch(
                &mut controller,
                &format!(
                    r#"{{"version":1,"observation":"nativeTimeout","requestId":"{id}","kind":"dialog"}}"#
                ),
            );
            assert_eq!(timed_out["effects"][0]["confirmed"], confirmed);
            assert_eq!(timed_out["effects"][0]["promptValue"], Value::Null);
            assert_eq!(timed_out["events"][0]["name"], "simpleDialogDismissed");
        }

        let mut controller = new_controller(1);
        let request = dispatch(
            &mut controller,
            r#"{"version":1,"observation":"simpleDialogRequest","kind":"prompt","message":"Name?","defaultValue":"Servo"}"#,
        );
        let id = request_id(&request, 0, "dialogId");
        let resolved = dispatch(
            &mut controller,
            &format!(
                r#"{{"version":1,"command":"resolveSimpleDialog","dialogId":"{id}","confirmed":true}}"#
            ),
        );
        assert_eq!(resolved["effects"][0]["promptValue"], "Servo");

        for (kind, confirmed) in [("alert", true), ("confirm", true)] {
            let mut controller = new_controller(1);
            let request = dispatch(
                &mut controller,
                &format!(
                    r#"{{"version":1,"observation":"simpleDialogRequest","kind":"{kind}","message":"message"}}"#
                ),
            );
            let id = request_id(&request, 0, "dialogId");
            let resolved = dispatch(
                &mut controller,
                &format!(
                    r#"{{"version":1,"command":"resolveSimpleDialog","dialogId":"{id}","confirmed":{confirmed}}}"#
                ),
            );
            assert_eq!(resolved["effects"][0]["kind"], kind);
            assert_eq!(resolved["effects"][0]["confirmed"], confirmed);
        }
    }

    #[test]
    fn completes_evaluation_and_rejects_duplicate_or_late_ids() {
        let mut controller = new_controller(1);
        let started = dispatch(
            &mut controller,
            r#"{"version":1,"command":"evaluateJavaScript","evaluationId":"client-1","script":"document.title"}"#,
        );
        let request = request_id(&started, 0, "requestId");
        assert_eq!(request, "evaluation-1-1-1");
        assert_eq!(
            controller
                .dispatch_json(
                    r#"{"version":1,"command":"evaluateJavaScript","evaluationId":"client-1","script":"1"}"#,
                )
                .unwrap_err()
                .code(),
            "duplicateEvaluationId"
        );
        let completed = dispatch(
            &mut controller,
            &format!(
                r#"{{"version":1,"observation":"javaScriptEvaluationResult","requestId":"{request}","ok":true,"valueJson":"\"Servo\""}}"#
            ),
        );
        assert_eq!(
            completed["events"][0]["payload"]["evaluationId"],
            "client-1"
        );
        assert_eq!(completed["events"][0]["payload"]["valueJson"], r#""Servo""#);
        assert_eq!(
            controller
                .dispatch_json(&format!(
                    r#"{{"version":1,"observation":"javaScriptEvaluationResult","requestId":"{request}","ok":true,"valueJson":"null"}}"#
                ))
                .unwrap_err()
                .code(),
            "requestNotPending"
        );
    }

    #[test]
    fn reset_and_destroy_finish_pending_in_request_order() {
        let mut controller = new_controller(1);
        let navigation = dispatch(
            &mut controller,
            r#"{"version":1,"observation":"navigationRequest","url":"about:blank"}"#,
        );
        let navigation_id = request_id(&navigation, 0, "navigationId");
        let evaluation = dispatch(
            &mut controller,
            r#"{"version":1,"command":"evaluateJavaScript","evaluationId":"client-reset","script":"1"}"#,
        );
        let evaluation_id = request_id(&evaluation, 0, "requestId");
        let reset = dispatch(&mut controller, r#"{"version":1,"lifecycle":"reset"}"#);
        assert_eq!(
            reset["effects"]
                .as_array()
                .unwrap()
                .iter()
                .map(|effect| effect["name"].as_str().unwrap())
                .collect::<Vec<_>>(),
            [
                "resolveNavigationCompletion",
                "invalidateEvaluation",
                "reset"
            ]
        );
        assert_eq!(
            reset["events"][0]["payload"]["errorType"],
            "WebViewNotReady"
        );
        assert_eq!(reset["effects"][2]["generation"], 2);
        assert_eq!(
            controller
                .dispatch_json(&format!(
                    r#"{{"version":1,"command":"resolveNavigationRequest","navigationId":"{navigation_id}","allow":false}}"#
                ))
                .unwrap_err()
                .code(),
            "staleRequest"
        );
        assert_eq!(
            controller
                .dispatch_json(&format!(
                    r#"{{"version":1,"observation":"javaScriptEvaluationResult","requestId":"{evaluation_id}","ok":true,"valueJson":"1"}}"#
                ))
                .unwrap_err()
                .code(),
            "staleRequest"
        );

        let dialog = dispatch(
            &mut controller,
            r#"{"version":1,"observation":"simpleDialogRequest","kind":"confirm","message":"Continue?"}"#,
        );
        let id = request_id(&dialog, 0, "dialogId");
        let destroyed: Value = serde_json::from_str(&controller.destroy().to_json()).unwrap();
        assert_eq!(destroyed["effects"][0]["dialogId"], id);
        assert_eq!(destroyed["effects"][1]["name"], "destroy");
    }

    #[test]
    fn rejects_wrong_kind_without_consuming_pending_request() {
        let mut controller = new_controller(1);
        let request = dispatch(
            &mut controller,
            r#"{"version":1,"observation":"simpleDialogRequest","kind":"confirm","message":"Continue?"}"#,
        );
        let id = request_id(&request, 0, "dialogId");
        assert_eq!(
            controller
                .dispatch_json(&format!(
                    r#"{{"version":1,"command":"resolveNavigationRequest","navigationId":"{id}","allow":true}}"#
                ))
                .unwrap_err()
                .code(),
            "wrongRequestKind"
        );
        assert!(controller
            .dispatch_json(&format!(
                r#"{{"version":1,"command":"resolveSimpleDialog","dialogId":"{id}","confirmed":false}}"#
            ))
            .is_ok());
    }

    #[test]
    fn passes_browser_snapshots_through_without_retaining_them() {
        let mut controller = new_controller(1);
        let result = dispatch(
            &mut controller,
            r#"{"version":1,"observation":"loadStatusChanged","status":"Started","url":"https://snapshot.invalid/unique","entries":["https://history.invalid/unique"],"current":0,"canGoBack":false,"canGoForward":false}"#,
        );
        assert_eq!(
            result["events"]
                .as_array()
                .unwrap()
                .iter()
                .map(|event| event["name"].as_str().unwrap())
                .collect::<Vec<_>>(),
            ["urlChanged", "loadStatusChanged", "historyChanged"]
        );
        let state = format!("{controller:?}");
        assert!(!state.contains("snapshot.invalid"));
        assert!(!state.contains("history.invalid"));
        assert!(
            dispatch(
                &mut controller,
                r#"{"version":1,"observation":"pageTitleChanged","title":"Unique title"}"#,
            )["events"][0]["name"]
                == "pageTitleChanged"
        );
        assert_eq!(
            dispatch(
                &mut controller,
                r#"{"version":1,"observation":"focusChanged","isFocused":true}"#,
            )["events"][0]["name"],
            "focusChanged"
        );
        assert_eq!(
            format!("{controller:?}"),
            "PortableController { controller_instance_id: 1, generation: 1, next_request: 0, pending: [] }"
        );
    }

    #[test]
    fn controller_instances_never_share_or_consume_request_ids() {
        let mut retired = new_controller(1);
        let old = request_id(
            &dispatch(
                &mut retired,
                r#"{"version":1,"observation":"navigationRequest","url":"https://old.invalid/"}"#,
            ),
            0,
            "navigationId",
        );
        let mut replacement = new_controller(2);
        let current = request_id(
            &dispatch(
                &mut replacement,
                r#"{"version":1,"observation":"navigationRequest","url":"https://new.invalid/"}"#,
            ),
            0,
            "navigationId",
        );

        assert_ne!(old, current);
        assert_eq!(
            replacement
                .dispatch_json(&format!(
                    r#"{{"version":1,"command":"resolveNavigationRequest","navigationId":"{old}","allow":false}}"#
                ))
                .unwrap_err()
                .code(),
            "staleRequest"
        );
        assert!(replacement
            .dispatch_json(&format!(
                r#"{{"version":1,"command":"resolveNavigationRequest","navigationId":"{current}","allow":true}}"#
            ))
            .is_ok());
    }
}
