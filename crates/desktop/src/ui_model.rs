//! Presentation contract. Only the worker owns SSH, disk I/O, and sync plans.
//! Credentials are one-shot command payloads, never part of snapshots.
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug)]
pub(crate) struct Device {
    pub id: String,
    pub name: String,
    pub address: String,
    pub status: String,
    pub is_local: bool,
    pub platform: String,
}
#[derive(Clone, Debug)]
pub(crate) struct ConfigItem {
    pub id: String,
    pub name: String,
    pub category: String,
    pub detail: String,
}
#[derive(Clone, Debug)]
pub(crate) struct Change {
    pub id: String,
    pub device: String,
    pub path: String,
    pub before: String,
    pub after: String,
    pub conflict: bool,
    pub source_available: bool,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Choice {
    Source,
    Target,
}
#[derive(Clone, Debug, Default)]
pub(crate) struct FileReviewDetails {
    pub source_hash: Option<String>,
    pub target_hash: Option<String>,
    pub permission_policy: bool,
}
#[derive(Clone, Debug, Default)]
pub(crate) struct Snapshot {
    pub devices: Vec<Device>,
    pub source_id: Option<String>,
    pub items: Vec<ConfigItem>,
    pub changes: Vec<Change>,
    /// Bounded, already-redacted text only. Hidden/binary previews never enter the code diff.
    pub text_previews: BTreeMap<String, (String, String)>,
    pub file_review_details: BTreeMap<String, FileReviewDetails>,
    pub plan_id: Option<String>,
    pub results: Vec<(String, String)>,
    pub has_last_selection: bool,
    pub selection_loaded: bool,
    pub blocked_targets: Vec<(String, String)>,
    pub selected_item_ids: Vec<String>,
    pub target_ids: Vec<String>,
    pub command_details_reviewed: bool,
}
/// Deliberately not Debug: credential payloads must never reach logs.
pub(crate) enum Command {
    Refresh,
    AddDevice {
        name: String,
        ssh_line: String,
        root: String,
    },
    RemoveDevice(String),
    SelectSource(String),
    Prepare {
        source: String,
        targets: Vec<String>,
        items: Vec<String>,
    },
    SaveSelection {
        source: String,
        targets: Vec<String>,
        items: Vec<String>,
    },
    CancelReview,
    CancelOperation,
    RepeatLast,
    RevealDetails {
        plan_id: String,
    },
    Apply {
        plan_id: String,
        choices: BTreeMap<String, Choice>,
    },
    TrustHost {
        request_id: String,
        accepted: bool,
    },
    Authenticate {
        request_id: String,
        secret: zeroize::Zeroizing<String>,
    },
    CancelAuthentication {
        request_id: String,
    },
}
pub(crate) enum Event {
    Snapshot(Snapshot),
    SelectionSaved {
        has_last_selection: bool,
    },
    Busy(String),
    Error(String),
    TrustHost {
        request_id: String,
        host: String,
        fingerprint: String,
    },
    Authentication {
        request_id: String,
        prompt: String,
    },
    AuthenticationFinished {
        request_id: String,
    },
}

impl Snapshot {
    /// Restore the exact reviewed scope, especially after Repeat last selection.
    /// Ordinary refreshes never replace the user's unsent staging choices.
    pub(crate) fn restored_selection(
        &self,
        previous_plan: Option<&str>,
    ) -> Option<(BTreeSet<String>, BTreeSet<String>)> {
        if self.plan_id.is_none() || self.plan_id.as_deref() == previous_plan {
            return None;
        }
        Some((
            self.selected_item_ids.iter().cloned().collect(),
            self.target_ids.iter().cloned().collect(),
        ))
    }

    /// Conflicts require an explicit decision; ordinary incoming changes default
    /// to Source. This same rule drives the disabled state and apply payload.
    pub(crate) fn unresolved_count(&self, choices: &BTreeMap<String, Choice>) -> usize {
        self.changes
            .iter()
            .filter(|change| {
                (change.conflict && !choices.contains_key(&change.id))
                    || (!change.source_available
                        && choices.get(&change.id).copied().unwrap_or(Choice::Source)
                            == Choice::Source)
            })
            .count()
    }

    pub(crate) fn needs_command_details(&self, choices: &BTreeMap<String, Choice>) -> bool {
        !self.command_details_reviewed
            && self.changes.iter().any(|change| {
                change.path.starts_with("mcp.json")
                    && choices.get(&change.id).copied().unwrap_or(Choice::Source) == Choice::Source
            })
    }

    pub(crate) fn reviewed_choices(
        &self,
        choices: &BTreeMap<String, Choice>,
    ) -> Option<BTreeMap<String, Choice>> {
        if self.plan_id.is_none()
            || self.unresolved_count(choices) != 0
            || self.needs_command_details(choices)
        {
            return None;
        }
        Some(
            self.changes
                .iter()
                .map(|change| {
                    (
                        change.id.clone(),
                        choices.get(&change.id).copied().unwrap_or(Choice::Source),
                    )
                })
                .collect(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn review(conflict: bool) -> Snapshot {
        Snapshot {
            plan_id: Some("review-1".into()),
            changes: vec![Change {
                id: "device/settings/theme".into(),
                device: "device".into(),
                path: "settings/theme".into(),
                before: "old".into(),
                after: "new".into(),
                conflict,
                source_available: true,
            }],
            ..Default::default()
        }
    }
    #[test]
    fn conflict_has_no_implicit_source_choice() {
        let snapshot = review(true);
        assert_eq!(snapshot.unresolved_count(&BTreeMap::new()), 1);
        assert!(snapshot.reviewed_choices(&BTreeMap::new()).is_none());
    }
    #[test]
    fn all_explicit_conflict_choices_are_valid() {
        for choice in [Choice::Source, Choice::Target] {
            let snapshot = review(true);
            let choices = BTreeMap::from([("device/settings/theme".into(), choice)]);
            assert_eq!(snapshot.unresolved_count(&choices), 0);
            assert_eq!(snapshot.reviewed_choices(&choices), Some(choices));
        }
    }
    #[test]
    fn ordinary_change_defaults_to_source_only_with_a_plan() {
        let mut snapshot = review(false);
        assert_eq!(
            snapshot.reviewed_choices(&BTreeMap::new()).unwrap()["device/settings/theme"],
            Choice::Source
        );
        snapshot.plan_id = None;
        assert!(snapshot.reviewed_choices(&BTreeMap::new()).is_none());
    }
    #[test]
    fn choices_from_an_old_plan_are_not_submitted() {
        let snapshot = review(false);
        let old_choices = BTreeMap::from([("removed-change".into(), Choice::Source)]);
        assert!(
            !snapshot
                .reviewed_choices(&old_choices)
                .unwrap()
                .contains_key("removed-change")
        );
    }
    #[test]
    fn repeat_restores_exact_review_scope_but_refresh_does_not() {
        let mut snapshot = review(false);
        snapshot.selected_item_ids = vec!["settings".into()];
        snapshot.target_ids = vec!["laptop".into()];
        let (items, targets) = snapshot.restored_selection(None).unwrap();
        assert_eq!(items, BTreeSet::from(["settings".into()]));
        assert_eq!(targets, BTreeSet::from(["laptop".into()]));
        assert!(snapshot.restored_selection(Some("review-1")).is_none());
        snapshot.plan_id = None;
        assert!(snapshot.restored_selection(Some("review-1")).is_none());
    }
    #[test]
    fn mcp_source_choices_require_explicit_detail_review() {
        let mut snapshot = review(false);
        snapshot.changes[0].path = "mcp.json/servers/example/command".into();
        assert!(snapshot.reviewed_choices(&BTreeMap::new()).is_none());
        let choices = BTreeMap::from([("device/settings/theme".into(), Choice::Target)]);
        assert!(snapshot.reviewed_choices(&choices).is_some());
        snapshot.command_details_reviewed = true;
        assert!(snapshot.reviewed_choices(&BTreeMap::new()).is_some());
    }
    #[test]
    fn unavailable_source_requires_target() {
        let mut snapshot = review(true);
        snapshot.changes[0].source_available = false;
        let source = BTreeMap::from([("device/settings/theme".into(), Choice::Source)]);
        assert_eq!(snapshot.unresolved_count(&source), 1);
        assert!(snapshot.reviewed_choices(&source).is_none());
        let choices = BTreeMap::from([("device/settings/theme".into(), Choice::Target)]);
        assert_eq!(snapshot.unresolved_count(&choices), 0);
        assert!(snapshot.reviewed_choices(&choices).is_some());
    }
}
