//! Document-load metadata and the SDK's generic pixel preference.

use super::{DataCompletion, MAX_PIXELS};
use block2::RcBlock;
use serde_json::Value;
use std::collections::BTreeMap;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Key {
    pub url: String,
    pub attributes: BTreeMap<String, String>,
}

pub(in crate::webview::macos) struct Pending {
    pub(super) key: Option<Key>,
    pub(super) decision: RcBlock<dyn Fn(*mut DataCompletion)>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::webview::macos) enum Candidate {
    Matched { index: usize, size: Option<u32> },
    Compatibility,
    Rejected,
}

#[derive(Default)]
enum Metadata {
    #[default]
    NotStarted,
    Collecting,
    Ready(Vec<Record>),
    Unavailable,
}

struct Record {
    key: Key,
    media: bool,
    sizes: Vec<String>,
}

#[derive(Default)]
pub(in crate::webview::macos) struct Selection {
    metadata: Metadata,
    pending: Vec<Pending>,
    best: Option<(u32, usize)>,
}

impl Selection {
    pub(super) fn hold(&mut self, pending: Pending) -> Result<bool, Pending> {
        match self.metadata {
            Metadata::NotStarted => {
                self.metadata = Metadata::Collecting;
                self.pending.push(pending);
                Ok(true)
            }
            Metadata::Collecting => {
                self.pending.push(pending);
                Ok(false)
            }
            _ => Err(pending),
        }
    }

    pub(super) fn settle(&mut self, json: Option<&str>) -> Option<Vec<Pending>> {
        if !matches!(self.metadata, Metadata::Collecting) {
            return None;
        }
        self.metadata = json
            .and_then(records)
            .map(Metadata::Ready)
            .unwrap_or(Metadata::Unavailable);
        Some(std::mem::take(&mut self.pending))
    }

    pub(super) fn classify(&self, key: Option<&Key>, svg: bool) -> Candidate {
        let Metadata::Ready(records) = &self.metadata else {
            return Candidate::Compatibility;
        };
        let Some((index, record)) = records
            .iter()
            .enumerate()
            .rev()
            .find(|(_, record)| Some(&record.key) == key)
        else {
            return Candidate::Compatibility;
        };
        if !record.media || !supported_type(record.key.attributes.get("type"), svg) {
            return Candidate::Rejected;
        }
        Candidate::Matched {
            index,
            size: declared_size(&record.sizes),
        }
    }

    pub(in crate::webview::macos) fn accept(
        &mut self,
        candidate: Candidate,
        width: u32,
        height: u32,
    ) -> bool {
        match candidate {
            Candidate::Rejected => false,
            Candidate::Compatibility => self.best.is_none(),
            Candidate::Matched { index, size } => {
                let rank = (width.min(height).min(size.unwrap_or(u32::MAX)), index);
                if self.best.is_some_and(|best| rank <= best) {
                    return false;
                }
                self.best = Some(rank);
                true
            }
        }
    }

    pub(in crate::webview::macos) fn retire(&mut self) -> Vec<Pending> {
        self.metadata = Metadata::NotStarted;
        self.best = None;
        std::mem::take(&mut self.pending)
    }
}

pub(in crate::webview::macos) fn decline(pending: Vec<Pending>) {
    for request in pending {
        request.decision.call((std::ptr::null_mut(),));
    }
}

fn records(json: &str) -> Option<Vec<Record>> {
    let value: Value = serde_json::from_str(json).ok()?;
    value
        .as_array()?
        .iter()
        .map(|record| {
            Some(Record {
                key: Key {
                    url: record.get("url")?.as_str()?.to_owned(),
                    attributes: record
                        .get("attributes")?
                        .as_object()?
                        .iter()
                        .map(|(key, value)| Some((key.clone(), value.as_str()?.to_owned())))
                        .collect::<Option<_>>()?,
                },
                media: record.get("media")?.as_bool()?,
                sizes: record
                    .get("sizes")?
                    .as_array()?
                    .iter()
                    .map(|size| size.as_str().map(str::to_owned))
                    .collect::<Option<_>>()?,
            })
        })
        .collect()
}

fn supported_type(value: Option<&String>, svg: bool) -> bool {
    let Some(value) = value.filter(|value| !value.is_empty()) else {
        return true;
    };
    let Ok(mime) = value.parse::<mime::Mime>() else {
        return true;
    };
    if mime.essence_str() == "image/svg+xml" {
        return svg;
    }
    image::ImageFormat::from_mime_type(mime.essence_str())
        .is_some_and(|format| format.reading_enabled())
}

// HTML rel=icon dimension grammar; DOM already split the ASCII-space tokens.
fn declared_size(tokens: &[String]) -> Option<u32> {
    if tokens.iter().any(|token| token.eq_ignore_ascii_case("any")) {
        return None;
    }
    tokens
        .iter()
        .filter_map(|token| {
            let (width, height) = token.split_once(['x', 'X'])?;
            let dimension = |text: &str| {
                if text.starts_with('0') || !text.bytes().all(|byte| byte.is_ascii_digit()) {
                    return None;
                }
                text.parse::<u32>().ok().filter(|value| *value > 0)
            };
            let (width, height) = (dimension(width)?, dimension(height)?);
            (u64::from(width) * u64::from(height) <= MAX_PIXELS).then_some(width.min(height))
        })
        .max()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::{cell::Cell, rc::Rc};

    fn record(url: &str, media: bool, kind: Option<&str>, sizes: &[&str]) -> Value {
        let mut attributes = BTreeMap::from([("rel", "icon")]);
        if let Some(kind) = kind {
            attributes.insert("type", kind);
        }
        json!({"url":url,"attributes":attributes,"media":media,"sizes":sizes})
    }
    fn ready(records: &[Value]) -> Selection {
        let mut selection = Selection {
            metadata: Metadata::Collecting,
            ..Default::default()
        };
        assert!(selection
            .settle(Some(&serde_json::to_string(records).unwrap()))
            .unwrap()
            .is_empty());
        selection
    }
    fn key(url: &str, kind: Option<&str>) -> Key {
        let mut attributes = BTreeMap::from([("rel".into(), "icon".into())]);
        if let Some(kind) = kind {
            attributes.insert("type".into(), kind.into());
        }
        Key {
            url: url.into(),
            attributes,
        }
    }
    #[test]
    fn successful_preference_survives_both_completion_orders_and_other_failures() {
        for reverse in [false, true] {
            let mut selection = ready(&[
                record("a", true, None, &["32x32"]),
                record("b", true, None, &["32x32"]),
            ]);
            let a = selection.classify(Some(&key("a", None)), false);
            let b = selection.classify(Some(&key("b", None)), false);
            let order = if reverse { [b, a] } else { [a, b] };
            for candidate in order {
                selection.accept(candidate, 32, 32);
            }
            assert_eq!(selection.best, Some((32, 1)));
            assert!(!selection.accept(Candidate::Compatibility, 256, 256));
            assert!(!selection.accept(a, 32, 32));
        }
        let mut selection = ready(&[
            record("a", true, None, &["16x16"]),
            record("b", true, None, &["32x32"]),
        ]);
        let a = selection.classify(Some(&key("a", None)), false);
        let b = selection.classify(Some(&key("b", None)), false);
        assert!(selection.accept(a, 16, 16)); // Preferred B can fail without invalidating A.
        assert!(selection.accept(b, 32, 32));
        assert!(!selection.accept(a, 16, 16));
    }
    #[test]
    fn media_and_type_rejections_cannot_escape_to_unmatched_fallback() {
        let selection = ready(&[
            record("dark", false, None, &[]),
            record("pdf", true, Some("application/pdf"), &[]),
            record("svg", true, Some("IMAGE/SVG+XML; charset=utf-8"), &[]),
        ]);
        assert_eq!(
            selection.classify(Some(&key("dark", None)), true),
            Candidate::Rejected
        );
        assert_eq!(
            selection.classify(Some(&key("pdf", Some("application/pdf"))), true),
            Candidate::Rejected
        );
        assert_eq!(
            selection.classify(
                Some(&key("svg", Some("IMAGE/SVG+XML; charset=utf-8"))),
                false
            ),
            Candidate::Rejected
        );
        assert!(matches!(
            selection.classify(
                Some(&key("svg", Some("IMAGE/SVG+XML; charset=utf-8"))),
                true
            ),
            Candidate::Matched { .. }
        ));
        for hint in [
            None,
            Some(""),
            Some("not a mime"),
            Some("IMAGE/PNG; charset=binary"),
        ] {
            assert!(supported_type(hint.map(str::to_owned).as_ref(), false));
        }
    }
    #[test]
    fn duplicates_use_last_tree_index_but_actual_resolution_controls_upgrades() {
        let mut selection = ready(&[
            record("a", true, None, &["64x64"]),
            record("a", true, None, &["64x64"]),
        ]);
        let a = selection.classify(Some(&key("a", None)), true);
        assert_eq!(
            a,
            Candidate::Matched {
                index: 1,
                size: Some(64)
            }
        );
        assert!(selection.accept(a, 16, 16));
        assert!(!selection.accept(a, 16, 16));
        assert!(selection.accept(a, 32, 32));
        assert!(!selection.accept(a, 16, 16));
        assert_eq!(selection.best, Some((32, 1)));
    }
    #[test]
    fn sizes_follow_html_tokens_without_infinite_any_or_overstated_resolution() {
        let tokens = |values: &[&str]| values.iter().map(|v| v.to_string()).collect::<Vec<_>>();
        assert_eq!(declared_size(&tokens(&["16x16", "32X48"])), Some(32));
        assert_eq!(declared_size(&tokens(&["16x16", "ANY"])), None);
        assert_eq!(
            declared_size(&tokens(&[
                "00x16",
                "0x16",
                "16x",
                "16x16x16",
                "-1x2",
                "4294967296x1",
                "4096x4096"
            ])),
            None
        );
        let mut selection = Selection::default();
        assert!(selection.accept(
            Candidate::Matched {
                index: 0,
                size: Some(96)
            },
            2,
            2
        ));
        assert_eq!(selection.best, Some((2, 0)));
    }
    #[test]
    fn unavailable_and_unmatched_keep_useful_images_until_a_matched_success() {
        let mut selection = ready(&[record("a", true, None, &[])]);
        let unknown = selection.classify(Some(&key("unmatched", None)), true);
        assert_eq!(unknown, Candidate::Compatibility);
        assert!(selection.accept(unknown, 32, 32));
        assert!(selection.accept(unknown, 16, 16));
        assert!(selection.accept(selection.classify(Some(&key("a", None)), true), 1, 1));
        assert!(!selection.accept(unknown, 1024, 1024));
        selection.retire();
        assert!(selection.accept(Candidate::Compatibility, 32, 32));
    }
    #[test]
    fn pending_native_decisions_are_copied_and_consumed_once() {
        let calls = Rc::new(Cell::new(0));
        let pending = || {
            let calls = calls.clone();
            Pending {
                key: None,
                decision: RcBlock::new(move |data: *mut DataCompletion| {
                    assert!(data.is_null());
                    calls.set(calls.get() + 1);
                }),
            }
        };
        let mut selection = Selection::default();
        assert_eq!(selection.hold(pending()).ok(), Some(true));
        assert_eq!(selection.hold(pending()).ok(), Some(false));
        let held = selection.settle(None).unwrap();
        assert!(selection.settle(Some("[]")).is_none());
        decline(held);
        assert_eq!(calls.get(), 2);
        assert!(selection.retire().is_empty());
        selection.hold(pending()).ok().unwrap();
        decline(selection.retire());
        assert_eq!(calls.get(), 3);
    }
}
