use crate::terminal::model::index::Point;

pub use warpui::cursor_trail::{
    CursorTrailConfig, CursorTrailPrimitive, CursorTrailUpdate, cursor_trail_repaint_interval,
};

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

impl warpui::cursor_trail::CursorTrailKey for CursorTrailKey {
    fn same_surface(self, other: Self) -> bool {
        self.surface == other.surface
    }

    fn cell_distance_to(self, other: Self) -> usize {
        self.point.row.abs_diff(other.point.row) + self.point.col.abs_diff(other.point.col)
    }
}

pub type CursorTrailSnapshot = warpui::cursor_trail::CursorTrailSnapshot<CursorTrailKey>;
pub type CursorTrailState = warpui::cursor_trail::CursorTrailState<CursorTrailKey>;
pub type CursorTrailStateHandle = warpui::cursor_trail::CursorTrailStateHandle<CursorTrailKey>;

#[cfg(test)]
mod tests {
    use super::*;
    use pathfinder_color::ColorU;
    use pathfinder_geometry::rect::RectF;
    use pathfinder_geometry::vector::vec2f;
    use std::time::Duration;

    fn snapshot(surface: CursorTrailSurface, row: usize, col: usize) -> CursorTrailSnapshot {
        CursorTrailSnapshot {
            key: CursorTrailKey {
                surface,
                point: Point { row, col },
            },
            bounds: RectF::new(vec2f(col as f32 * 8., row as f32 * 16.), vec2f(8., 16.)),
            cell_size: vec2f(8., 16.),
            visible: true,
            color: ColorU::new(10, 20, 30, 255),
        }
    }

    #[test]
    fn terminal_cursor_trail_key_resets_between_surfaces() {
        let mut state = CursorTrailState::default();
        let config = CursorTrailConfig::from_enabled(true);
        let now = instant::Instant::now();

        assert_eq!(
            state.update(
                config,
                Some(snapshot(CursorTrailSurface::BlockList, 0, 0)),
                now
            ),
            CursorTrailUpdate::default()
        );
        assert_eq!(
            state.update(
                config,
                Some(snapshot(CursorTrailSurface::AltScreen, 0, 5)),
                now + Duration::from_millis(10),
            ),
            CursorTrailUpdate::default()
        );
    }
}
