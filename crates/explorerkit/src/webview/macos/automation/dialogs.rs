//! One Rust pending entry shared by native automation and Runtime answers.
//! Native blocks are held separately and invoked only after all registry borrows end.
use super::{HostError, SystemCallbackSink};
use block2::{DynBlock, RcBlock};
use explorerkit_embedder::{SimpleDialogKind, SimpleDialogRequest};
use objc2_foundation::NSString;

pub(in crate::webview::macos) enum Completion {
    Alert(RcBlock<dyn Fn()>),
    Confirm(RcBlock<dyn Fn(objc2::runtime::Bool)>),
    Prompt(RcBlock<dyn Fn(*mut NSString)>),
}
impl Completion {
    pub(in crate::webview::macos) fn alert(block: &DynBlock<dyn Fn()>) -> Self {
        Self::Alert(block.copy())
    }
    pub(in crate::webview::macos) fn confirm(
        block: &DynBlock<dyn Fn(objc2::runtime::Bool)>,
    ) -> Self {
        Self::Confirm(block.copy())
    }
    pub(in crate::webview::macos) fn prompt(block: &DynBlock<dyn Fn(*mut NSString)>) -> Self {
        Self::Prompt(block.copy())
    }
    fn finish(self, confirmed: bool, value: Option<&str>) {
        match self {
            Self::Alert(block) => block.call(()),
            Self::Confirm(block) => block.call((objc2::runtime::Bool::new(confirmed),)),
            Self::Prompt(block) => {
                let value = value.map(NSString::from_str);
                block.call((value
                    .as_ref()
                    .map(|s| objc2::rc::Retained::as_ptr(s).cast_mut())
                    .unwrap_or(std::ptr::null_mut()),));
            }
        }
    }
}
pub(in crate::webview::macos) struct PendingDialog {
    id: String,
    completion: Completion,
}
pub(in crate::webview::macos) fn enabled(sink: &SystemCallbackSink) -> bool {
    sink.callbacks
        .borrow()
        .views
        .get(&sink.target)
        .is_some_and(|s| s.automation.eligible)
}
pub(in crate::webview::macos) fn capture(
    sink: &SystemCallbackSink,
    kind: SimpleDialogKind,
    message: String,
    default_value: Option<String>,
    completion: Completion,
) {
    // WebKit serializes modal dialogs for a page. Terminal native defaults settle
    // any replaced/retired completion before publishing its successor.
    retire(sink);
    let mut completion = Some(completion);
    sink.publish(|state| {
        let state = &mut state.automation;
        state.next_dialog += 1;
        let id = format!(
            "webkit-dialog-{}-{}-{}",
            sink.target.0, sink.target.1, state.next_dialog
        );
        state.prompts.show_simple_dialog(SimpleDialogRequest {
            dialog_id: id.clone(),
            kind,
            message,
            default_value: default_value.clone(),
        });
        state.dialog = Some(PendingDialog {
            id,
            completion: completion.take().unwrap(),
        });
        true
    });
    if let Some(completion) = completion {
        completion.finish(false, None);
    }
    flush(sink);
}
fn flush(sink: &SystemCallbackSink) {
    sink.publish(|state| {
        state.events.extend(state.automation.prompts.drain_events());
        true
    });
}
pub(in crate::webview::macos) fn pending(sink: &SystemCallbackSink) -> Option<SimpleDialogRequest> {
    let registry = sink.callbacks.borrow();
    let state = &registry.views.get(&sink.target)?.automation;
    state
        .prompts
        .pending_simple_dialog(&state.dialog.as_ref()?.id)
        .cloned()
}
pub(in crate::webview::macos) fn current_input(sink: &SystemCallbackSink) -> Option<String> {
    let registry = sink.callbacks.borrow();
    let state = &registry.views.get(&sink.target)?.automation;
    state
        .prompts
        .simple_dialog_value(&state.dialog.as_ref()?.id)
        .map(str::to_owned)
}
pub(in crate::webview::macos) fn set_input(sink: &SystemCallbackSink, input: String) {
    let mut registry = sink.callbacks.borrow_mut();
    if let Some(state) = registry.views.get_mut(&sink.target) {
        if let Some(dialog) = &state.automation.dialog {
            let _ = state
                .automation
                .prompts
                .set_simple_dialog_value(&dialog.id, input);
        }
    }
}
pub(in crate::webview::macos) fn resolve(
    sink: &SystemCallbackSink,
    id: &str,
    confirmed: bool,
    value: Option<&str>,
) -> Result<(), HostError> {
    let (completion, value) = {
        let mut registry = sink.callbacks.borrow_mut();
        let state = registry
            .views
            .get_mut(&sink.target)
            .ok_or_else(|| HostError::new("dialog view retired"))?;
        if state.automation.dialog.as_ref().is_none_or(|d| d.id != id) {
            return Err(HostError::new("dialog is no longer pending"));
        }
        let value = state
            .automation
            .prompts
            .answer_simple_dialog(id, confirmed, value)
            .map_err(HostError::new)?;
        let completion = state.automation.dialog.take().unwrap().completion;
        state.events.extend(state.automation.prompts.drain_events());
        (completion, value)
    };
    completion.finish(confirmed, value.as_deref());
    sink.wake();
    Ok(())
}
pub(in crate::webview::macos) fn retire(sink: &SystemCallbackSink) {
    let completion = {
        let mut registry = sink.callbacks.borrow_mut();
        let Some(state) = registry.views.get_mut(&sink.target) else {
            return;
        };
        let Some(dialog) = state.automation.dialog.take() else {
            return;
        };
        state.automation.prompts.hide_embedder_control(&dialog.id);
        state.events.extend(state.automation.prompts.drain_events());
        dialog.completion
    };
    // These are WebKit's absent-handler meanings at terminal view/process loss,
    // not a timer and never an automation-disconnect answer.
    completion.finish(false, None);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        cell::{Cell, RefCell},
        rc::Rc,
        sync::Arc,
    };
    fn sink() -> SystemCallbackSink {
        SystemCallbackSink::register_raw(
            Rc::new(RefCell::new(super::super::super::SystemCallbacks::new(
                Arc::new(|| {}),
            ))),
            1,
            1,
        )
    }
    #[test]
    fn process_death_settles_native_confirmation_once_and_retires_shared_prompt() {
        let sink = sink();
        let calls = Rc::new(Cell::new(0));
        let observed = calls.clone();
        let block = RcBlock::new(move |answer: objc2::runtime::Bool| {
            assert!(!answer.as_bool());
            observed.set(observed.get() + 1);
        });
        capture(
            &sink,
            SimpleDialogKind::Confirm,
            "question".into(),
            None,
            Completion::confirm(&block),
        );
        let id = pending(&sink).unwrap().dialog_id;
        sink.process_terminated();
        assert!(pending(&sink).is_none());
        assert!(resolve(&sink, &id, true, None).is_err());
        sink.invalidate();
        assert_eq!(calls.get(), 1);
    }
    #[test]
    fn native_prompt_is_not_answered_until_host_reply_or_terminal_retirement() {
        let sink = sink();
        let calls = Rc::new(Cell::new(0));
        let observed = calls.clone();
        let block = RcBlock::new(move |answer: *mut NSString| {
            assert!(answer.is_null());
            observed.set(observed.get() + 1);
        });
        capture(
            &sink,
            SimpleDialogKind::Prompt,
            "question".into(),
            Some("default".into()),
            Completion::prompt(&block),
        );
        assert_eq!(calls.get(), 0);
        retire(&sink);
        retire(&sink);
        assert_eq!(calls.get(), 1);
        assert_eq!(
            sink.drain()
                .iter()
                .filter(|event| matches!(
                    event,
                    explorerkit_embedder::HostEvent::SimpleDialogDismissed { .. }
                ))
                .count(),
            1
        );
    }
}
