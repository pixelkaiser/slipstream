use crate::terminal::model::ansi::CursorShape;
use crate::terminal::model::index::Point;
use instant::Instant;
use pathfinder_color::ColorU;
use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;
use warpui::geometry::rect::RectF;
use warpui::geometry::vector::Vector2F;

const CURSOR_TRAIL_TRIGGER_DELAY: Duration = Duration::from_millis(1);
const CURSOR_TRAIL_START_THRESHOLD_CELLS: usize = 2;
const CURSOR_TRAIL_FAST_DECAY: Duration = Duration::from_millis(100);
const CURSOR_TRAIL_SLOW_DECAY: Duration = Duration::from_millis(400);
const CURSOR_TRAIL_REPAINT_INTERVAL: Duration = Duration::from_millis(16);
const CURSOR_TRAIL_PIXEL_EPSILON: f32 = 0.5;
const CURSOR_TRAIL_ALPHA: f32 = 0.72;

#[derive(Clone, Default)]
pub struct CursorTrailStateHandle(Rc<RefCell<CursorTrailState>>);

impl CursorTrailStateHandle {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn update(
        &self,
        config: CursorTrailConfig,
        snapshot: Option<CursorTrailSnapshot>,
        now: Instant,
    ) -> CursorTrailUpdate {
        self.0.borrow_mut().update(config, snapshot, now)
    }

    pub fn reset(&self) {
        self.0.borrow_mut().reset();
    }
}

#[derive(Clone, Copy)]
pub struct CursorTrailConfig {
    pub enabled: bool,
    pub trigger_delay: Duration,
    pub start_threshold_cells: usize,
    pub decay_fast: Duration,
    pub decay_slow: Duration,
}

impl CursorTrailConfig {
    pub fn from_enabled(enabled: bool) -> Self {
        Self {
            enabled,
            trigger_delay: CURSOR_TRAIL_TRIGGER_DELAY,
            start_threshold_cells: CURSOR_TRAIL_START_THRESHOLD_CELLS,
            decay_fast: CURSOR_TRAIL_FAST_DECAY,
            decay_slow: CURSOR_TRAIL_SLOW_DECAY,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CursorTrailSurface {
    BlockList,
    AltScreen,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CursorTrailKey {
    pub surface: CursorTrailSurface,
    pub point: Point,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CursorTrailSnapshot {
    pub key: CursorTrailKey,
    pub bounds: RectF,
    pub cell_size: Vector2F,
    pub shape: CursorShape,
    pub color: ColorU,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CursorTrailPrimitive {
    pub corners: [Vector2F; 4],
    pub cursor_bounds: RectF,
    pub color: ColorU,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct CursorTrailUpdate {
    pub primitive: Option<CursorTrailPrimitive>,
    pub needs_repaint: bool,
}

#[derive(Clone, Debug)]
struct PendingTrail {
    start_at: Instant,
    from_bounds: RectF,
    target: CursorTrailSnapshot,
}

#[derive(Clone, Debug)]
struct ActiveTrail {
    corners: [Vector2F; 4],
    target: CursorTrailSnapshot,
    updated_at: Instant,
}

#[derive(Clone, Debug, Default)]
pub struct CursorTrailState {
    last_snapshot: Option<CursorTrailSnapshot>,
    pending: Option<PendingTrail>,
    active: Option<ActiveTrail>,
}

impl CursorTrailState {
    pub fn reset(&mut self) {
        self.last_snapshot = None;
        self.pending = None;
        self.active = None;
    }

    pub fn update(
        &mut self,
        config: CursorTrailConfig,
        snapshot: Option<CursorTrailSnapshot>,
        now: Instant,
    ) -> CursorTrailUpdate {
        let Some(snapshot) = snapshot else {
            self.reset();
            return CursorTrailUpdate::default();
        };

        if !config.enabled || snapshot.shape == CursorShape::Hidden {
            self.reset_to(snapshot);
            return CursorTrailUpdate::default();
        }

        if let Some(last_snapshot) = self.last_snapshot {
            if should_reset_for_geometry_change(last_snapshot, snapshot) {
                self.reset_to(snapshot);
                return CursorTrailUpdate::default();
            }

            if last_snapshot.key != snapshot.key {
                self.pending = None;
                self.active = None;

                if last_snapshot.key.surface != snapshot.key.surface
                    || cursor_cell_distance(last_snapshot.key.point, snapshot.key.point)
                        <= config.start_threshold_cells
                {
                    self.reset_to(snapshot);
                    return CursorTrailUpdate::default();
                }

                self.pending = Some(PendingTrail {
                    start_at: now + config.trigger_delay,
                    from_bounds: last_snapshot.bounds,
                    target: snapshot,
                });
                self.last_snapshot = Some(snapshot);
                return CursorTrailUpdate {
                    primitive: None,
                    needs_repaint: true,
                };
            }
        } else {
            self.reset_to(snapshot);
            return CursorTrailUpdate::default();
        }

        self.last_snapshot = Some(snapshot);

        if let Some(pending) = self.pending.take() {
            if now < pending.start_at {
                self.pending = Some(pending);
                return CursorTrailUpdate {
                    primitive: None,
                    needs_repaint: true,
                };
            }

            self.active = Some(ActiveTrail {
                corners: rect_corners(pending.from_bounds),
                target: pending.target,
                updated_at: now,
            });
        }

        let Some(active) = &mut self.active else {
            return CursorTrailUpdate::default();
        };

        active.target = snapshot;
        update_corners(active, config, now);

        let needs_render = corners_need_render(active.corners, rect_corners(snapshot.bounds));
        if !needs_render {
            self.active = None;
            return CursorTrailUpdate::default();
        }

        CursorTrailUpdate {
            primitive: Some(CursorTrailPrimitive {
                corners: active.corners,
                cursor_bounds: snapshot.bounds,
                color: with_scaled_alpha(snapshot.color, CURSOR_TRAIL_ALPHA),
            }),
            needs_repaint: true,
        }
    }

    fn reset_to(&mut self, snapshot: CursorTrailSnapshot) {
        self.last_snapshot = Some(snapshot);
        self.pending = None;
        self.active = None;
    }
}

pub fn cursor_trail_repaint_interval() -> Duration {
    CURSOR_TRAIL_REPAINT_INTERVAL
}

fn rect_corners(bounds: RectF) -> [Vector2F; 4] {
    [
        bounds.origin(),
        bounds.upper_right(),
        bounds.lower_right(),
        bounds.lower_left(),
    ]
}

fn should_reset_for_geometry_change(
    previous: CursorTrailSnapshot,
    current: CursorTrailSnapshot,
) -> bool {
    previous.key == current.key
        && (!approx_vec_eq(previous.cell_size, current.cell_size)
            || !approx_rect_eq(previous.bounds, current.bounds))
}

fn approx_rect_eq(a: RectF, b: RectF) -> bool {
    approx_vec_eq(a.origin(), b.origin()) && approx_vec_eq(a.size(), b.size())
}

fn approx_vec_eq(a: Vector2F, b: Vector2F) -> bool {
    (a.x() - b.x()).abs() < f32::EPSILON && (a.y() - b.y()).abs() < f32::EPSILON
}

fn cursor_cell_distance(a: Point, b: Point) -> usize {
    a.row.abs_diff(b.row) + a.col.abs_diff(b.col)
}

fn update_corners(active: &mut ActiveTrail, config: CursorTrailConfig, now: Instant) {
    if now <= active.updated_at {
        return;
    }

    let dt = (now - active.updated_at).as_secs_f32();
    active.updated_at = now;

    let target_corners = rect_corners(active.target.bounds);
    let cursor_center = active.target.bounds.origin() + active.target.bounds.size() * 0.5;
    let cursor_diag_2 = active.target.bounds.size().length() * 0.5;
    if cursor_diag_2 <= f32::EPSILON {
        return;
    }

    let mut dots = [0.; 4];
    let mut has_delta = false;
    for i in 0..4 {
        let delta = target_corners[i] - active.corners[i];
        let delta_len = delta.length();
        if delta_len <= f32::EPSILON {
            continue;
        }

        has_delta = true;
        let corner_vector = target_corners[i] - cursor_center;
        dots[i] = (delta.x() * corner_vector.x() + delta.y() * corner_vector.y())
            / cursor_diag_2
            / delta_len;
    }

    if !has_delta {
        return;
    }

    let mut min_dot = f32::MAX;
    let mut max_dot = f32::MIN;
    for dot in dots {
        min_dot = min_dot.min(dot);
        max_dot = max_dot.max(dot);
    }

    let decay_fast = config.decay_fast.as_secs_f32().max(f32::EPSILON);
    let decay_slow = config.decay_slow.as_secs_f32().max(f32::EPSILON);

    for i in 0..4 {
        let delta = target_corners[i] - active.corners[i];
        if delta.length() <= f32::EPSILON {
            continue;
        }

        let decay = if (max_dot - min_dot).abs() <= f32::EPSILON {
            decay_slow
        } else {
            decay_slow + (decay_fast - decay_slow) * (dots[i] - min_dot) / (max_dot - min_dot)
        };
        let step = 1.0 - 2f32.powf(-10.0 * dt / decay);
        active.corners[i] += delta * step;
    }
}

fn corners_need_render(corners: [Vector2F; 4], target_corners: [Vector2F; 4]) -> bool {
    corners
        .into_iter()
        .zip(target_corners)
        .any(|(corner, target)| {
            (corner.x() - target.x()).abs() >= CURSOR_TRAIL_PIXEL_EPSILON
                || (corner.y() - target.y()).abs() >= CURSOR_TRAIL_PIXEL_EPSILON
        })
}

fn with_scaled_alpha(color: ColorU, opacity: f32) -> ColorU {
    ColorU::new(
        color.r,
        color.g,
        color.b,
        ((color.a as f32) * opacity.clamp(0., 1.)).round() as u8,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal::model::ansi::CursorShape;
    use warpui::geometry::vector::vec2f;

    fn instant(base: Instant, ms: u64) -> Instant {
        base + Duration::from_millis(ms)
    }

    fn snapshot(surface: CursorTrailSurface, row: usize, col: usize) -> CursorTrailSnapshot {
        CursorTrailSnapshot {
            key: CursorTrailKey {
                surface,
                point: Point { row, col },
            },
            bounds: RectF::new(vec2f(col as f32 * 10., row as f32 * 20.), vec2f(10., 20.)),
            cell_size: vec2f(10., 20.),
            shape: CursorShape::Block,
            color: ColorU::white(),
        }
    }

    #[test]
    fn disabled_state_resets_without_rendering() {
        let mut state = CursorTrailState::default();
        let config = CursorTrailConfig::from_enabled(false);
        let base = Instant::now();

        assert_eq!(
            state.update(
                config,
                Some(snapshot(CursorTrailSurface::BlockList, 0, 0)),
                instant(base, 0)
            ),
            CursorTrailUpdate::default()
        );
        assert!(state.last_snapshot.is_some());
        assert!(state.active.is_none());
    }

    #[test]
    fn hidden_cursor_resets_without_rendering() {
        let mut state = CursorTrailState::default();
        let config = CursorTrailConfig::from_enabled(true);
        let base = Instant::now();
        let mut hidden = snapshot(CursorTrailSurface::BlockList, 0, 0);
        hidden.shape = CursorShape::Hidden;

        assert_eq!(
            state.update(config, Some(hidden), instant(base, 0)),
            CursorTrailUpdate::default()
        );
        assert!(state.active.is_none());
    }

    #[test]
    fn small_moves_are_skipped() {
        let mut state = CursorTrailState::default();
        let config = CursorTrailConfig::from_enabled(true);
        let base = Instant::now();

        state.update(
            config,
            Some(snapshot(CursorTrailSurface::BlockList, 0, 0)),
            instant(base, 0),
        );
        let update = state.update(
            config,
            Some(snapshot(CursorTrailSurface::BlockList, 0, 2)),
            instant(base, 1),
        );

        assert_eq!(update, CursorTrailUpdate::default());
        assert!(state.pending.is_none());
        assert!(state.active.is_none());
    }

    #[test]
    fn large_moves_wait_for_trigger_delay() {
        let mut state = CursorTrailState::default();
        let config = CursorTrailConfig::from_enabled(true);
        let base = Instant::now();

        state.update(
            config,
            Some(snapshot(CursorTrailSurface::BlockList, 0, 0)),
            instant(base, 0),
        );
        let pending = state.update(
            config,
            Some(snapshot(CursorTrailSurface::BlockList, 0, 5)),
            instant(base, 10),
        );
        assert!(pending.primitive.is_none());
        assert!(pending.needs_repaint);

        let active = state.update(
            config,
            Some(snapshot(CursorTrailSurface::BlockList, 0, 5)),
            instant(base, 12),
        );
        assert!(active.primitive.is_some());
        assert!(active.needs_repaint);
    }

    #[test]
    fn corners_decay_toward_target() {
        let mut state = CursorTrailState::default();
        let config = CursorTrailConfig::from_enabled(true);
        let base = Instant::now();

        state.update(
            config,
            Some(snapshot(CursorTrailSurface::BlockList, 0, 0)),
            instant(base, 0),
        );
        state.update(
            config,
            Some(snapshot(CursorTrailSurface::BlockList, 0, 5)),
            instant(base, 10),
        );
        let first = state
            .update(
                config,
                Some(snapshot(CursorTrailSurface::BlockList, 0, 5)),
                instant(base, 12),
            )
            .primitive
            .unwrap();
        let second = state
            .update(
                config,
                Some(snapshot(CursorTrailSurface::BlockList, 0, 5)),
                instant(base, 60),
            )
            .primitive
            .unwrap();

        assert!(second.corners[0].x() > first.corners[0].x());
        assert!(second.corners[0].x() < 50.);
    }

    #[test]
    fn scroll_only_geometry_change_resets() {
        let mut state = CursorTrailState::default();
        let config = CursorTrailConfig::from_enabled(true);
        let base = Instant::now();
        let first = snapshot(CursorTrailSurface::BlockList, 3, 4);
        let mut scrolled = first;
        scrolled.bounds = RectF::new(first.bounds.origin() + vec2f(0., -40.), first.bounds.size());

        state.update(config, Some(first), instant(base, 0));
        let update = state.update(config, Some(scrolled), instant(base, 10));

        assert_eq!(update, CursorTrailUpdate::default());
        assert!(state.pending.is_none());
        assert!(state.active.is_none());
    }
}
