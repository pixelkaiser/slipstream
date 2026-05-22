use instant::Instant;
use pathfinder_color::ColorU;
use pathfinder_geometry::rect::RectF;
use pathfinder_geometry::vector::Vector2F;
use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

const CURSOR_TRAIL_TRIGGER_DELAY: Duration = Duration::from_millis(1);
const CURSOR_TRAIL_START_THRESHOLD_CELLS: usize = 2;
const CURSOR_TRAIL_FAST_DECAY: Duration = Duration::from_millis(100);
const CURSOR_TRAIL_SLOW_DECAY: Duration = Duration::from_millis(400);
const CURSOR_TRAIL_REPAINT_INTERVAL: Duration = Duration::from_millis(16);
const CURSOR_TRAIL_PIXEL_EPSILON: f32 = 0.5;
const CURSOR_TRAIL_ALPHA: f32 = 0.72;

pub trait CursorTrailKey: Copy + Eq {
    fn same_surface(self, _other: Self) -> bool {
        let _ = self;
        true
    }

    fn cell_distance_to(self, other: Self) -> usize;
}

pub struct CursorTrailStateHandle<K: CursorTrailKey>(Rc<RefCell<CursorTrailState<K>>>);

impl<K: CursorTrailKey> Clone for CursorTrailStateHandle<K> {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}

impl<K: CursorTrailKey> Default for CursorTrailStateHandle<K> {
    fn default() -> Self {
        Self(Rc::new(RefCell::new(CursorTrailState::default())))
    }
}

impl<K: CursorTrailKey> CursorTrailStateHandle<K> {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn update(
        &self,
        config: CursorTrailConfig,
        snapshot: Option<CursorTrailSnapshot<K>>,
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

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CursorTrailSnapshot<K: CursorTrailKey> {
    pub key: K,
    pub bounds: RectF,
    pub cell_size: Vector2F,
    pub visible: bool,
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
struct PendingTrail<K: CursorTrailKey> {
    start_at: Instant,
    from_bounds: RectF,
    target: CursorTrailSnapshot<K>,
}

#[derive(Clone, Debug)]
struct ActiveTrail<K: CursorTrailKey> {
    corners: [Vector2F; 4],
    target: CursorTrailSnapshot<K>,
    updated_at: Instant,
}

#[derive(Clone, Debug)]
pub struct CursorTrailState<K: CursorTrailKey> {
    last_snapshot: Option<CursorTrailSnapshot<K>>,
    pending: Option<PendingTrail<K>>,
    active: Option<ActiveTrail<K>>,
}

impl<K: CursorTrailKey> Default for CursorTrailState<K> {
    fn default() -> Self {
        Self {
            last_snapshot: None,
            pending: None,
            active: None,
        }
    }
}

impl<K: CursorTrailKey> CursorTrailState<K> {
    pub fn reset(&mut self) {
        self.last_snapshot = None;
        self.pending = None;
        self.active = None;
    }

    pub fn update(
        &mut self,
        config: CursorTrailConfig,
        snapshot: Option<CursorTrailSnapshot<K>>,
        now: Instant,
    ) -> CursorTrailUpdate {
        let Some(snapshot) = snapshot else {
            self.reset();
            return CursorTrailUpdate::default();
        };

        if !config.enabled || !snapshot.visible {
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

                if !last_snapshot.key.same_surface(snapshot.key)
                    || last_snapshot.key.cell_distance_to(snapshot.key)
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

    fn reset_to(&mut self, snapshot: CursorTrailSnapshot<K>) {
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

fn should_reset_for_geometry_change<K: CursorTrailKey>(
    previous: CursorTrailSnapshot<K>,
    current: CursorTrailSnapshot<K>,
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

fn update_corners<K: CursorTrailKey>(
    active: &mut ActiveTrail<K>,
    config: CursorTrailConfig,
    now: Instant,
) {
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
    use pathfinder_geometry::rect::RectF;
    use pathfinder_geometry::vector::vec2f;

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    struct TestKey {
        surface: u8,
        row: usize,
        col: usize,
    }

    impl CursorTrailKey for TestKey {
        fn same_surface(self, other: Self) -> bool {
            self.surface == other.surface
        }

        fn cell_distance_to(self, other: Self) -> usize {
            self.row.abs_diff(other.row) + self.col.abs_diff(other.col)
        }
    }

    fn snapshot(surface: u8, row: usize, col: usize) -> CursorTrailSnapshot<TestKey> {
        CursorTrailSnapshot {
            key: TestKey { surface, row, col },
            bounds: RectF::new(vec2f(col as f32 * 8., row as f32 * 16.), vec2f(8., 16.)),
            cell_size: vec2f(8., 16.),
            visible: true,
            color: ColorU::new(10, 20, 30, 255),
        }
    }

    #[test]
    fn disabled_cursor_trail_does_not_render() {
        let mut state = CursorTrailState::default();
        let config = CursorTrailConfig::from_enabled(false);
        let now = Instant::now();

        assert_eq!(
            state.update(config, Some(snapshot(0, 0, 0)), now),
            CursorTrailUpdate::default()
        );
        assert_eq!(
            state.update(
                config,
                Some(snapshot(0, 0, 10)),
                now + Duration::from_millis(10)
            ),
            CursorTrailUpdate::default()
        );
    }

    #[test]
    fn hidden_cursor_resets_without_rendering() {
        let mut state = CursorTrailState::default();
        let config = CursorTrailConfig::from_enabled(true);
        let now = Instant::now();
        let mut hidden = snapshot(0, 0, 0);
        hidden.visible = false;

        assert_eq!(
            state.update(config, Some(hidden), now),
            CursorTrailUpdate::default()
        );
    }

    #[test]
    fn adjacent_cursor_move_does_not_start_trail() {
        let mut state = CursorTrailState::default();
        let config = CursorTrailConfig::from_enabled(true);
        let now = Instant::now();

        assert_eq!(
            state.update(config, Some(snapshot(0, 0, 0)), now),
            CursorTrailUpdate::default()
        );
        let update = state.update(
            config,
            Some(snapshot(0, 0, 2)),
            now + Duration::from_millis(10),
        );
        assert_eq!(update, CursorTrailUpdate::default());
    }

    #[test]
    fn distant_cursor_move_renders_after_trigger_delay() {
        let mut state = CursorTrailState::default();
        let config = CursorTrailConfig::from_enabled(true);
        let now = Instant::now();

        assert_eq!(
            state.update(config, Some(snapshot(0, 0, 0)), now),
            CursorTrailUpdate::default()
        );
        let pending = state.update(
            config,
            Some(snapshot(0, 0, 5)),
            now + Duration::from_millis(10),
        );
        assert_eq!(
            pending,
            CursorTrailUpdate {
                primitive: None,
                needs_repaint: true,
            }
        );

        let rendered = state.update(
            config,
            Some(snapshot(0, 0, 5)),
            now + Duration::from_millis(20),
        );
        assert!(rendered.primitive.is_some());
        assert!(rendered.needs_repaint);
    }

    #[test]
    fn surface_change_resets_without_rendering() {
        let mut state = CursorTrailState::default();
        let config = CursorTrailConfig::from_enabled(true);
        let now = Instant::now();

        assert_eq!(
            state.update(config, Some(snapshot(0, 0, 0)), now),
            CursorTrailUpdate::default()
        );
        assert_eq!(
            state.update(
                config,
                Some(snapshot(1, 0, 5)),
                now + Duration::from_millis(10)
            ),
            CursorTrailUpdate::default()
        );
    }
}
