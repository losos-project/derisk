//! Windows-style snapping: edge/corner drop zones and Super+arrow nudging.

use mcsapi::Geometry;
use serde::{Deserialize, Serialize};

use crate::geom::{Point, gap_insets, inset_sides, rect};

/// A region of the work area that a window can be snapped into.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SnapZone {
    /// Left half.
    Left,
    /// Right half.
    Right,
    /// Top-left quarter.
    TopLeft,
    /// Top-right quarter.
    TopRight,
    /// Bottom-left quarter.
    BottomLeft,
    /// Bottom-right quarter.
    BottomRight,
    /// The whole work area.
    Maximize,
}

/// Arrow direction for keyboard snapping (Super+arrow).
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    /// Left arrow.
    Left,
    /// Right arrow.
    Right,
    /// Up arrow.
    Up,
    /// Down arrow.
    Down,
}

/// The outcome of nudging a window with Super+arrow.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Nudge {
    /// Snap into the zone.
    Snap(SnapZone),
    /// Return to the free-floating restore geometry.
    Restore,
    /// Minimize the window.
    Minimize,
}

/// Distances, in logical pixels, that trigger snapping while dragging.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SnapConfig {
    /// How close the pointer must be to an edge.
    pub edge: i32,
    /// How far along an edge a corner (quarter) zone extends.
    pub corner: i32,
}

impl Default for SnapConfig {
    fn default() -> Self {
        Self {
            edge: 12,
            corner: 96,
        }
    }
}

impl SnapZone {
    /// The zone's frame inside `area`, separated from neighbours by `gap`.
    ///
    /// A maximized window has no neighbours to be separated from, so it
    /// takes the whole area rather than leaving a strip of wallpaper round it.
    pub fn geometry(self, area: Geometry, gap: i32) -> Geometry {
        let whole = area;
        let ((area_before, area_after), (tile_before, tile_after)) = gap_insets(gap);
        let area = inset_sides(area, area_before, area_after);
        let (x, y, w, h) = (area.loc.x, area.loc.y, area.size.w, area.size.h);
        let (lw, th) = (w / 2, h / 2);
        let raw = match self {
            Self::Left => rect(x, y, lw, h),
            Self::Right => rect(x + lw, y, w - lw, h),
            Self::TopLeft => rect(x, y, lw, th),
            Self::TopRight => rect(x + lw, y, w - lw, th),
            Self::BottomLeft => rect(x, y + th, lw, h - th),
            Self::BottomRight => rect(x + lw, y + th, w - lw, h - th),
            Self::Maximize => return whole,
        };
        inset_sides(raw, tile_before, tile_after)
    }

    /// The zone that "Snap Assist" offers to fill after snapping into `self`.
    pub fn complement(self) -> Option<Self> {
        match self {
            Self::Left => Some(Self::Right),
            Self::Right => Some(Self::Left),
            Self::TopLeft => Some(Self::TopRight),
            Self::TopRight => Some(Self::TopLeft),
            Self::BottomLeft => Some(Self::BottomRight),
            Self::BottomRight => Some(Self::BottomLeft),
            Self::Maximize => None,
        }
    }

    /// Windows' Super+arrow behaviour starting from `current` (`None` = floating/tiled).
    pub fn nudge(current: Option<Self>, direction: Direction) -> Nudge {
        use Direction as D;
        use SnapZone as Z;
        match (current, direction) {
            (None, D::Left) => Nudge::Snap(Z::Left),
            (None, D::Right) => Nudge::Snap(Z::Right),
            (None, D::Up) => Nudge::Snap(Z::Maximize),
            (None, D::Down) => Nudge::Minimize,
            (Some(Z::Maximize), D::Down) => Nudge::Restore,
            (Some(Z::Maximize), D::Left) => Nudge::Snap(Z::Left),
            (Some(Z::Maximize), D::Right) => Nudge::Snap(Z::Right),
            (Some(Z::Maximize), D::Up) => Nudge::Snap(Z::Maximize),
            (Some(Z::Left), D::Up) => Nudge::Snap(Z::TopLeft),
            (Some(Z::Left), D::Down) => Nudge::Snap(Z::BottomLeft),
            (Some(Z::Left), D::Right) => Nudge::Restore,
            (Some(Z::Left), D::Left) => Nudge::Snap(Z::Left),
            (Some(Z::Right), D::Up) => Nudge::Snap(Z::TopRight),
            (Some(Z::Right), D::Down) => Nudge::Snap(Z::BottomRight),
            (Some(Z::Right), D::Left) => Nudge::Restore,
            (Some(Z::Right), D::Right) => Nudge::Snap(Z::Right),
            (Some(Z::TopLeft), D::Up) => Nudge::Snap(Z::Maximize),
            (Some(Z::TopLeft), D::Down) => Nudge::Snap(Z::Left),
            (Some(Z::TopLeft), D::Right) => Nudge::Snap(Z::TopRight),
            (Some(Z::TopLeft), D::Left) => Nudge::Snap(Z::TopLeft),
            (Some(Z::TopRight), D::Up) => Nudge::Snap(Z::Maximize),
            (Some(Z::TopRight), D::Down) => Nudge::Snap(Z::Right),
            (Some(Z::TopRight), D::Left) => Nudge::Snap(Z::TopLeft),
            (Some(Z::TopRight), D::Right) => Nudge::Snap(Z::TopRight),
            (Some(Z::BottomLeft), D::Up) => Nudge::Snap(Z::Left),
            (Some(Z::BottomLeft), D::Down) => Nudge::Minimize,
            (Some(Z::BottomLeft), D::Right) => Nudge::Snap(Z::BottomRight),
            (Some(Z::BottomLeft), D::Left) => Nudge::Snap(Z::BottomLeft),
            (Some(Z::BottomRight), D::Up) => Nudge::Snap(Z::Right),
            (Some(Z::BottomRight), D::Down) => Nudge::Minimize,
            (Some(Z::BottomRight), D::Left) => Nudge::Snap(Z::BottomLeft),
            (Some(Z::BottomRight), D::Right) => Nudge::Snap(Z::BottomRight),
        }
    }
}

/// The zone under a dragged pointer, if it touches an edge of `area`.
///
/// Like Windows: left/right edges snap halves, the top edge maximizes, and
/// the ends of each edge snap quarters. The bottom edge only snaps corners.
/// Points above the work area (e.g. over the top bar) count as the top edge.
pub fn zone_at(point: Point, area: Geometry, config: SnapConfig) -> Option<SnapZone> {
    let (px, py) = point;
    let (left, top) = (area.loc.x, area.loc.y);
    let (right, bottom) = (left + area.size.w - 1, top + area.size.h - 1);
    let near_left = px <= left + config.edge;
    let near_right = px >= right - config.edge;
    let near_top = py <= top + config.edge;
    let near_bottom = py >= bottom - config.edge;
    let top_end = py <= top + config.corner;
    let bottom_end = py >= bottom - config.corner;
    let left_end = px <= left + config.corner;
    let right_end = px >= right - config.corner;

    if near_left {
        Some(if top_end {
            SnapZone::TopLeft
        } else if bottom_end {
            SnapZone::BottomLeft
        } else {
            SnapZone::Left
        })
    } else if near_right {
        Some(if top_end {
            SnapZone::TopRight
        } else if bottom_end {
            SnapZone::BottomRight
        } else {
            SnapZone::Right
        })
    } else if near_top {
        Some(if left_end {
            SnapZone::TopLeft
        } else if right_end {
            SnapZone::TopRight
        } else {
            SnapZone::Maximize
        })
    } else if near_bottom && left_end {
        Some(SnapZone::BottomLeft)
    } else if near_bottom && right_end {
        Some(SnapZone::BottomRight)
    } else {
        None
    }
}
