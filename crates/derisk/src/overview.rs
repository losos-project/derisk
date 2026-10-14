//! The overview screen: workspace strip, window grid and widget column.

use std::path::Path;

use mcsapi::Geometry;
use serde::{Deserialize, Serialize};

use crate::{
    adaptive::FormFactor,
    geom::{centered, inset, rect},
};

// The overview's widgets used to be a fixed enum drawn here (Clock,
// Calendar, Battery, Suggestions, Notes, Units). Each is now a widget plugin
// under plugins/widgets/, so the board is whatever plugins are loaded; see
// `crate::widgets`.

/// Regions of the overview.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct OverviewLayout {
    /// Workspace thumbnails.
    pub workspaces: Geometry,
    /// Window grid.
    pub windows: Geometry,
    /// Widget column (or stack on phones).
    pub widgets: Geometry,
}

impl OverviewLayout {
    /// Splits `area` for the form factor.
    pub fn new(area: Geometry, form_factor: FormFactor) -> Self {
        let area = inset(area, 16);
        let (x, y, w, h) = (area.loc.x, area.loc.y, area.size.w, area.size.h);
        match form_factor {
            FormFactor::Phone => {
                let widgets_h = h * 2 / 5;
                let strip = 56.min(h / 6);
                Self {
                    widgets: rect(x, y, w, widgets_h),
                    windows: rect(
                        x,
                        y + widgets_h + 12,
                        w,
                        (h - widgets_h - strip - 24).max(1),
                    ),
                    workspaces: rect(x, y + h - strip, w, strip),
                }
            }
            FormFactor::Tablet | FormFactor::Desktop => {
                let widgets_w = 320.min(w / 3);
                let strip = 110.min(h / 6);
                let main_w = w - widgets_w - 16;
                Self {
                    workspaces: rect(x, y, main_w, strip),
                    windows: rect(x, y + strip + 16, main_w, (h - strip - 16).max(1)),
                    widgets: rect(x + main_w + 16, y, widgets_w, h),
                }
            }
        }
    }
}

/// `count` equal cells side by side across `area` (the workspace strip).
pub fn row(count: usize, area: Geometry, gap: i32) -> Vec<Geometry> {
    if count == 0 {
        return Vec::new();
    }
    let n = count as i32;
    let w = ((area.size.w - gap * (n - 1)) / n).max(1);
    (0..n)
        .map(|i| rect(area.loc.x + i * (w + gap), area.loc.y, w, area.size.h))
        .collect()
}

/// Grid cells for `count` windows inside `area`, filled row by row.
///
/// Columns are chosen so cells stay close to the area's aspect ratio.
pub fn grid(count: usize, area: Geometry, gap: i32) -> impl Iterator<Item = Geometry> {
    let aspect = f64::from(area.size.w) / f64::from(area.size.h.max(1));
    // No windows is laid out as one, so nothing divides by zero; the range
    // below still yields no cells.
    let n = count.max(1);
    let cols = ((n as f64 * aspect).sqrt().ceil() as usize).clamp(1, n);
    let rows = n.div_ceil(cols);
    let cw = (area.size.w - gap * (cols as i32 - 1)) / cols as i32;
    let ch = (area.size.h - gap * (rows as i32 - 1)) / rows as i32;
    (0..count).map(move |i| {
        let (r, c) = ((i / cols) as i32, (i % cols) as i32);
        // Center a short last row.
        let in_row = if r as usize == rows - 1 {
            count - (rows - 1) * cols
        } else {
            cols
        } as i32;
        let offset = (cols as i32 - in_row) * (cw + gap) / 2;
        rect(
            area.loc.x + offset + c * (cw + gap),
            area.loc.y + r * (ch + gap),
            cw.max(1),
            ch.max(1),
        )
    })
}

/// Scales a window frame to fit a cell, preserving its aspect ratio.
pub fn fit(window: Geometry, cell: Geometry) -> Geometry {
    let scale = (f64::from(cell.size.w) / f64::from(window.size.w.max(1)))
        .min(f64::from(cell.size.h) / f64::from(window.size.h.max(1)))
        .min(1.0);
    centered(
        cell,
        (f64::from(window.size.w) * scale) as i32,
        (f64::from(window.size.h) * scale) as i32,
    )
}

/// Where `frame`, a window on a screen whose work area is `area`, sits in
/// `screen`, that work area drawn in miniature (from [`fit`]): the
/// overview's workspace cells show their windows where they really are.
pub fn miniature(frame: Geometry, area: Geometry, screen: Geometry) -> Geometry {
    let sx = f64::from(screen.size.w) / f64::from(area.size.w.max(1));
    let sy = f64::from(screen.size.h) / f64::from(area.size.h.max(1));
    let x = |v: i32| screen.loc.x + (f64::from(v - area.loc.x) * sx).round() as i32;
    let y = |v: i32| screen.loc.y + (f64::from(v - area.loc.y) * sy).round() as i32;
    let (x0, y0) = (x(frame.loc.x), y(frame.loc.y));
    let (x1, y1) = (x(frame.loc.x + frame.size.w), y(frame.loc.y + frame.size.h));
    rect(x0, y0, (x1 - x0).max(1), (y1 - y0).max(1))
}

/// Battery state for the battery widget.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Battery {
    /// Charge, 0–100.
    pub percent: u8,
    /// Whether it is charging.
    pub charging: bool,
    /// Whether the device runs on this battery. A full or "not charging"
    /// battery on external power is neither charging nor discharging.
    #[serde(default)]
    pub discharging: bool,
}

impl Battery {
    /// Reads the first battery under a `/sys/class/power_supply`-like directory.
    pub fn read(power_supply: &Path) -> Option<Self> {
        let mut entries: Vec<_> = std::fs::read_dir(power_supply)
            .ok()?
            .filter_map(Result::ok)
            .map(|e| e.path())
            .collect();
        entries.sort();
        let read = |dir: &Path, name: &str| std::fs::read_to_string(dir.join(name)).ok();
        let external_power = entries.iter().any(|dir| {
            read(dir, "type").is_some_and(|kind| kind.trim() != "Battery")
                && read(dir, "online").is_some_and(|online| online.trim() == "1")
        });
        entries.iter().find_map(|dir| {
            if read(dir, "type")?.trim() != "Battery" {
                return None;
            }
            let percent = read(dir, "capacity")?.trim().parse::<u8>().ok()?.min(100);
            let status = read(dir, "status").unwrap_or_default();
            let status = status.trim();
            Some(Self {
                percent,
                charging: matches!(status, "Charging" | "Full"),
                discharging: status == "Discharging" && !external_power,
            })
        })
    }
}
