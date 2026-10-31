//! Adaptation to the device (form factor) and to the user (habits).

use std::collections::BTreeMap;

use mcsapi::Layout;
use serde::{Deserialize, Serialize};

use crate::snap::SnapConfig;

/// The gap between tiled windows until Settings says otherwise, in logical
/// pixels: Settings' own default for `desktop.gaps`. Tablets once had 10 and
/// desktops 8; the one setting now covers both.
pub const DEFAULT_GAP: i32 = 5;

/// The broad class of device the shell is running on.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FormFactor {
    /// Small portrait/landscape screens: one window at a time.
    Phone,
    /// Medium or touch-first screens: larger targets.
    Tablet,
    /// Pointer-first screens.
    Desktop,
}

/// Shell metrics chosen for an output.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Profile {
    /// Detected form factor.
    pub form_factor: FormFactor,
    /// Default tiling layout for workspaces.
    pub layout: Layout,
    /// Gap between tiles, in logical pixels. Settings' `desktop.gaps`
    /// replaces it on every profile but a phone's (see
    /// [`crate::shell::Shell::set_gaps`]).
    pub gap: i32,
    /// Height of the top bar (global menu, tray, clock).
    pub top_bar: i32,
    /// Height of the touch navigation bar along the bottom edge (see
    /// [`crate::mobile`]); 0 where there is none.
    pub nav_bar: i32,
    /// Snap trigger distances.
    pub snap: SnapConfig,
    /// Whether the primary input is touch.
    pub touch: bool,
}

impl Profile {
    /// Picks metrics for an output of `width`×`height` logical pixels.
    pub fn detect(width: i32, height: i32, touch: bool) -> Self {
        let short = width.min(height);
        let form_factor = if short < 600 {
            FormFactor::Phone
        } else if touch && (short < 900 || width < 1600) {
            // Only a touchscreen earns the larger targets. A 1280×800 or
            // 1366×768 laptop driven by a pointer used to land here too and
            // lost a title bar's worth of every window to finger-sized chrome.
            FormFactor::Tablet
        } else {
            FormFactor::Desktop
        };
        match form_factor {
            FormFactor::Phone => Self {
                form_factor,
                layout: Layout::Monocle,
                gap: 0,
                top_bar: 32,
                nav_bar: 56,
                snap: SnapConfig {
                    edge: 24,
                    corner: 0,
                },
                touch,
            },
            FormFactor::Tablet => Self {
                form_factor,
                layout: Layout::Spiral,
                gap: DEFAULT_GAP,
                top_bar: 32,
                nav_bar: 0,
                snap: SnapConfig {
                    edge: 24,
                    corner: 128,
                },
                touch,
            },
            FormFactor::Desktop => Self {
                form_factor,
                layout: Layout::Spiral,
                gap: DEFAULT_GAP,
                top_bar: 28,
                nav_bar: 0,
                snap: SnapConfig::default(),
                touch,
            },
        }
    }
}

/// Learns which apps the user launches at which hour, to suggest them.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Habits {
    launches: BTreeMap<String, [u32; 24]>,
}

impl Habits {
    /// Records a launch of `app` during `hour` (0–23).
    pub fn record(&mut self, app: &str, hour: u8) {
        let app = app.trim().to_lowercase();
        if app.is_empty() {
            return;
        }
        let slot = &mut self.launches.entry(app).or_default()[usize::from(hour % 24)];
        *slot = slot.saturating_add(1);
    }

    /// Up to `limit` apps, best first, for `hour`.
    ///
    /// Launches in the same hour weigh most, neighbouring hours less, and the
    /// all-day total breaks ties before the name does.
    pub fn suggestions(&self, hour: u8, limit: usize) -> Vec<String> {
        let h = usize::from(hour % 24);
        let mut scored: Vec<(u64, &String)> = self
            .launches
            .iter()
            .map(|(app, hours)| {
                let at = |i: usize| u64::from(hours[i % 24]);
                let score = 4 * at(h)
                    + 2 * (at(h + 23) + at(h + 1))
                    + hours.iter().copied().map(u64::from).sum::<u64>();
                (score, app)
            })
            .collect();
        scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(b.1)));
        scored
            .into_iter()
            .take(limit)
            .map(|(_, app)| app.clone())
            .collect()
    }
}
