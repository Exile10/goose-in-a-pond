//! How long the pond keeps a context item (PAI-8 P1), honouring the `HEADLESS_BY_DESIGN` settings
//! `retention_events_by_category` and `retention_sensitive_days` (PAI-8 3.1); the UI half is owed.
//! Trap: `0` means keep forever in these settings, so a plain `u32::min` answers "delete now" for
//! "keep forever" and destroys data. Use [`Window`] and [`Window::stricter`] instead of a `u32`.

use std::collections::HashMap;

use chrono::{DateTime, Duration, Utc};

use crate::context::domain::SourceKind;
use crate::security::domain::event::{EventCategory, PrivacySensitivity};
use crate::user_data::domain::settings::Settings;

/// Ceiling on a retention window, in days. Matches `pruning.rs`'s `MAX_RETENTION_DAYS`, because
/// `Utc::now() - Duration::days(n)` panics rather than erroring outside chrono's range and these
/// numbers come from user-editable settings. ~100 years is forever for any real deployment.
pub const MAX_RETENTION_DAYS: i64 = 36_500;

/// A retention window: some number of days, or forever.
///
/// A newtype rather than a `u32` because the `0 = forever` convention makes
/// ordinary integer comparison wrong in the dangerous direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Window {
    Days(u32),
    Forever,
}

impl Window {
    /// Read one setting value, applying the `0 = forever` convention.
    pub fn from_setting(days: u32) -> Self {
        if days == 0 {
            Window::Forever
        } else {
            Window::Days(days)
        }
    }

    /// The stricter of two windows, the one that deletes sooner. `Forever` is the identity, not
    /// the zero: an item's category window and the sensitive-data cap both apply, and a
    /// per-category setting of 90 days must not extend a 7-day sensitive cap set to bound it.
    pub fn stricter(self, other: Window) -> Window {
        match (self, other) {
            (Window::Forever, w) | (w, Window::Forever) => w,
            (Window::Days(a), Window::Days(b)) => Window::Days(a.min(b)),
        }
    }

    /// The instant before which an item is past this window. `None` = forever.
    pub fn cutoff(self, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
        match self {
            Window::Forever => None,
            Window::Days(d) => Some(now - Duration::days((d as i64).min(MAX_RETENTION_DAYS))),
        }
    }
}

/// The retention policy in force, read from [`Settings`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextRetention {
    /// Per-`EventCategory` override, snake_case key to days. The same map
    /// `prune_events` reads, so a household that set "sensor: 5" gets five days
    /// of sensor readings in both stores.
    by_category: HashMap<String, u32>,
    /// Fallback when a category has no override.
    baseline_days: u32,
    /// Cap on anything classified `Sensitive` or above, whatever its category.
    sensitive_days: u32,
}

impl ContextRetention {
    pub fn new(by_category: HashMap<String, u32>, baseline_days: u32, sensitive_days: u32) -> Self {
        Self {
            by_category,
            baseline_days,
            sensitive_days,
        }
    }

    pub fn from_settings(s: &Settings) -> Self {
        Self::new(
            s.retention_events_by_category.clone(),
            s.retention_events_days,
            s.retention_sensitive_days,
        )
    }

    /// The settings key an [`EventCategory`] is stored under, derived through serde as
    /// `pruning.rs`'s `category_key` does. A hand-written table could disagree with the keys the
    /// settings map holds, and would present as "the retention I configured is being ignored".
    pub fn category_key(category: EventCategory) -> String {
        serde_json::to_value(category)
            .ok()
            .and_then(|v| v.as_str().map(String::from))
            .unwrap_or_default()
    }

    /// The window governing items from `kind` classified `sensitivity`.
    pub fn window_for(&self, kind: SourceKind, sensitivity: PrivacySensitivity) -> Window {
        let key = Self::category_key(kind.retention_category());
        let category = Window::from_setting(
            self.by_category
                .get(&key)
                .copied()
                .unwrap_or(self.baseline_days),
        );
        if sensitivity >= PrivacySensitivity::Sensitive {
            category.stricter(Window::from_setting(self.sensitive_days))
        } else {
            category
        }
    }

    /// Every (kind, sensitivity-class) bucket a purge has to sweep, with its cutoff. A `None`
    /// cutoff means keep forever and the caller skips it. Quantified over [`SourceKind::ALL`] and
    /// both sensitivity classes so a new source kind is swept the day it is added.
    pub fn sweep_plan(&self, now: DateTime<Utc>) -> Vec<RetentionBucket> {
        let mut plan = Vec::new();
        for kind in SourceKind::ALL {
            for sensitive in [false, true] {
                let sensitivity = if sensitive {
                    PrivacySensitivity::Sensitive
                } else {
                    PrivacySensitivity::Internal
                };
                let window = self.window_for(kind, sensitivity);
                plan.push(RetentionBucket {
                    kind,
                    sensitive,
                    cutoff: window.cutoff(now),
                });
            }
        }
        plan
    }
}

/// One bucket of a retention sweep.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetentionBucket {
    pub kind: SourceKind,
    /// `true` selects items classified `Sensitive` or above; `false` the rest.
    pub sensitive: bool,
    /// Items older than this go. `None` = this bucket is kept forever.
    pub cutoff: Option<DateTime<Utc>>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn retention(baseline: u32, sensitive: u32) -> ContextRetention {
        ContextRetention::new(HashMap::new(), baseline, sensitive)
    }

    /// The trap. `0` is forever in every one of these settings, so combining
    /// windows with a plain integer `min` answers "delete everything now" for
    /// "keep everything always".
    #[test]
    fn zero_means_forever_and_never_wins_a_min() {
        assert_eq!(Window::from_setting(0), Window::Forever);
        assert_eq!(
            Window::Forever.stricter(Window::Days(7)),
            Window::Days(7),
            "forever must be the identity of `stricter`, not its zero"
        );
        assert_eq!(Window::Days(7).stricter(Window::Forever), Window::Days(7));
        assert_eq!(Window::Forever.stricter(Window::Forever), Window::Forever);
        assert_eq!(Window::Days(30).stricter(Window::Days(7)), Window::Days(7));
        assert_eq!(Window::Forever.cutoff(Utc::now()), None);
    }

    /// The sensitive cap is a CAP: a generous per-category setting must not
    /// extend it. This is the pairing the user configured on purpose.
    #[test]
    fn the_sensitive_cap_bounds_a_generous_category() {
        let mut by_category = HashMap::new();
        by_category.insert("camera".to_string(), 90);
        let r = ContextRetention::new(by_category, 30, 7);

        assert_eq!(
            r.window_for(SourceKind::Camera, PrivacySensitivity::Sensitive),
            Window::Days(7),
            "a camera item is Sensitive, so the 7-day cap must beat the 90-day category"
        );
        assert_eq!(
            r.window_for(SourceKind::Camera, PrivacySensitivity::Internal),
            Window::Days(90),
            "an item below Sensitive is governed by its category alone"
        );
    }

    /// A category with no override falls back to the baseline, and the key it
    /// looks under is the one the settings map actually holds.
    #[test]
    fn the_category_key_is_the_one_settings_uses() {
        assert_eq!(
            ContextRetention::category_key(EventCategory::Sensor),
            "sensor"
        );
        assert_eq!(
            ContextRetention::category_key(EventCategory::Network),
            "network"
        );

        let mut by_category = HashMap::new();
        by_category.insert("sensor".to_string(), 5);
        let r = ContextRetention::new(by_category, 30, 0);
        assert_eq!(
            r.window_for(SourceKind::Sensor, PrivacySensitivity::Internal),
            Window::Days(5),
            "the sensor override was not found under the key settings stores it under"
        );
        assert_eq!(
            r.window_for(SourceKind::Voice, PrivacySensitivity::Internal),
            Window::Days(30),
            "a category with no override falls back to the baseline"
        );
    }

    /// Defaults, taken from `Settings` rather than restated, so this fails if
    /// the shipped default changes and nobody revisits what it means here.
    #[test]
    fn the_default_settings_produce_a_bounded_window() {
        let r = ContextRetention::from_settings(&Settings::default());
        for kind in SourceKind::ALL {
            let window = r.window_for(kind, PrivacySensitivity::Sensitive);
            assert!(
                matches!(window, Window::Days(d) if d <= 30),
                "{} keeps sensitive items for {:?} by default",
                kind.as_str(),
                window
            );
        }
    }

    /// The sweep is quantified over the enum, so a kind added tomorrow is
    /// swept tomorrow. Pinned by count, because the failure is silent: a bucket
    /// missing from the plan is a bucket nothing ever deletes.
    #[test]
    fn the_sweep_plan_covers_every_kind_and_both_classes() {
        let plan = retention(30, 7).sweep_plan(Utc::now());
        assert_eq!(
            plan.len(),
            SourceKind::ALL.len() * 2,
            "a source kind or a sensitivity class is missing from the sweep, and rows in it \
             would never be deleted by anything"
        );
        for kind in SourceKind::ALL {
            assert!(
                plan.iter().any(|b| b.kind == kind && b.sensitive),
                "no sensitive bucket for {}",
                kind.as_str()
            );
            assert!(
                plan.iter().any(|b| b.kind == kind && !b.sensitive),
                "no ordinary bucket for {}",
                kind.as_str()
            );
        }
        assert!(
            plan.iter().all(|b| b.cutoff.is_some()),
            "every bucket has a finite window under these settings"
        );
    }

    /// Vacuity control for the test above: with `0` everywhere the plan is the
    /// same length and every cutoff is `None`. If `from_setting` ever stopped
    /// reading `0` as forever, the count assertion alone would not notice.
    #[test]
    fn a_forever_policy_produces_the_same_buckets_and_no_cutoffs() {
        let plan = retention(0, 0).sweep_plan(Utc::now());
        assert_eq!(plan.len(), SourceKind::ALL.len() * 2);
        assert!(
            plan.iter().all(|b| b.cutoff.is_none()),
            "a retention of 0 must delete nothing"
        );
    }

    /// A user-supplied retention big enough to overflow chrono's arithmetic
    /// must clamp rather than panic; `pruning.rs` hit this first.
    #[test]
    fn an_absurd_window_clamps_instead_of_panicking() {
        let cutoff = Window::Days(u32::MAX).cutoff(Utc::now());
        assert!(cutoff.is_some());
    }
}
