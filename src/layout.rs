use ratatui::layout::Rect;

use crate::types::PaneId;

pub const MIN_COLS: u16 = 20;
pub const MIN_ROWS: u16 = 5;
pub const SPLITTER_HIT: u16 = 3;
pub const RATIO_MIN: f32 = 0.05;
pub const RATIO_MAX: f32 = 0.95;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SplitDirection {
    Vertical,
    Horizontal,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Branch {
    A,
    B,
}

#[derive(Clone, Debug)]
pub enum Node {
    Leaf(PaneId),
    Split {
        dir: SplitDirection,
        ratio: f32,
        a: Box<Node>,
        b: Box<Node>,
    },
}

#[derive(Clone, Debug)]
pub struct PaneLayout {
    pub id: PaneId,
    pub rect: Rect,
}

#[derive(Clone, Debug)]
pub struct Splitter {
    pub path: Vec<Branch>,
    pub dir: SplitDirection,
    pub area: Rect,
    pub pos: u16,
    pub hit_rect: Rect,
}

#[derive(Clone, Debug, Default)]
pub struct LayoutSnapshot {
    pub panes: Vec<PaneLayout>,
    pub splitters: Vec<Splitter>,
}

impl Node {
    pub fn leaf_count(&self) -> usize {
        match self {
            Node::Leaf(_) => 1,
            Node::Split { a, b, .. } => a.leaf_count() + b.leaf_count(),
        }
    }

    pub fn split_orientation(&self) -> (bool, bool) {
        match self {
            Node::Leaf(_) => (true, true),
            Node::Split { dir, a, b, .. } => {
                let (a_vert, a_horiz) = a.split_orientation();
                let (b_vert, b_horiz) = b.split_orientation();
                (
                    a_vert && b_vert && *dir == SplitDirection::Vertical,
                    a_horiz && b_horiz && *dir == SplitDirection::Horizontal,
                )
            }
        }
    }
}

pub fn build_snapshot(node: &mut Node, area: Rect) -> LayoutSnapshot {
    let mut snapshot = LayoutSnapshot::default();
    let mut path = Vec::new();
    layout_node(node, area, &mut path, &mut snapshot);
    snapshot
}

pub fn update_ratio(node: &mut Node, path: &[Branch], new_ratio: f32) -> bool {
    match node {
        Node::Leaf(_) => false,
        Node::Split { ratio, a, b, .. } => {
            if path.is_empty() {
                *ratio = new_ratio.clamp(RATIO_MIN, RATIO_MAX);
                true
            } else {
                match path[0] {
                    Branch::A => update_ratio(a, &path[1..], new_ratio),
                    Branch::B => update_ratio(b, &path[1..], new_ratio),
                }
            }
        }
    }
}

pub fn replace_leaf_with_split(
    node: &mut Node,
    target: PaneId,
    dir: SplitDirection,
    new_id: PaneId,
) -> bool {
    match node {
        Node::Leaf(id) => {
            if *id == target {
                *node = Node::Split {
                    dir,
                    ratio: 0.5,
                    a: Box::new(Node::Leaf(*id)),
                    b: Box::new(Node::Leaf(new_id)),
                };
                true
            } else {
                false
            }
        }
        Node::Split { a, b, .. } => {
            replace_leaf_with_split(a, target, dir, new_id)
                || replace_leaf_with_split(b, target, dir, new_id)
        }
    }
}

pub fn remove_pane(node: Node, target: PaneId) -> Option<Node> {
    match node {
        Node::Leaf(id) => {
            if id == target {
                None
            } else {
                Some(Node::Leaf(id))
            }
        }
        Node::Split { dir, ratio, a, b } => {
            let left = remove_pane(*a, target);
            let right = remove_pane(*b, target);
            match (left, right) {
                (None, None) => None,
                (Some(node), None) | (None, Some(node)) => Some(node),
                (Some(a), Some(b)) => Some(Node::Split {
                    dir,
                    ratio,
                    a: Box::new(a),
                    b: Box::new(b),
                }),
            }
        }
    }
}

pub fn pane_rect(snapshot: &LayoutSnapshot, id: PaneId) -> Option<Rect> {
    snapshot
        .panes
        .iter()
        .find(|pane| pane.id == id)
        .map(|p| p.rect)
}

pub fn split_ratio_for_position(splitter: &Splitter, x: u16, y: u16) -> f32 {
    let total = match splitter.dir {
        SplitDirection::Vertical => splitter.area.width,
        SplitDirection::Horizontal => splitter.area.height,
    };
    if total == 0 {
        return 0.5;
    }

    let min_a = match splitter.dir {
        SplitDirection::Vertical => MIN_COLS,
        SplitDirection::Horizontal => MIN_ROWS,
    };
    let min_b = min_a;
    let ratio_min = (min_a as f32 / total as f32).max(RATIO_MIN);
    let ratio_max = (1.0 - min_b as f32 / total as f32).min(RATIO_MAX);

    let ratio = match splitter.dir {
        SplitDirection::Vertical => {
            let offset = x.saturating_sub(splitter.area.x);
            offset as f32 / total as f32
        }
        SplitDirection::Horizontal => {
            let offset = y.saturating_sub(splitter.area.y);
            offset as f32 / total as f32
        }
    };

    if ratio_min > ratio_max {
        0.5
    } else {
        ratio.clamp(ratio_min, ratio_max)
    }
}

fn layout_node(node: &mut Node, area: Rect, path: &mut Vec<Branch>, snapshot: &mut LayoutSnapshot) {
    match node {
        Node::Leaf(id) => {
            snapshot.panes.push(PaneLayout {
                id: *id,
                rect: area,
            });
        }
        Node::Split { dir, ratio, a, b } => {
            let (rect_a, rect_b, pos, used_ratio) = split_area(area, *dir, *ratio);
            *ratio = used_ratio;
            let hit_rect = splitter_hit_rect(area, *dir, pos);
            snapshot.splitters.push(Splitter {
                path: path.clone(),
                dir: *dir,
                area,
                pos,
                hit_rect,
            });

            path.push(Branch::A);
            layout_node(a, rect_a, path, snapshot);
            path.pop();

            path.push(Branch::B);
            layout_node(b, rect_b, path, snapshot);
            path.pop();
        }
    }
}

fn split_area(area: Rect, dir: SplitDirection, ratio: f32) -> (Rect, Rect, u16, f32) {
    let total = match dir {
        SplitDirection::Vertical => area.width,
        SplitDirection::Horizontal => area.height,
    };

    if total == 0 {
        return (area, area, area.x, ratio);
    }

    let min_a = match dir {
        SplitDirection::Vertical => MIN_COLS,
        SplitDirection::Horizontal => MIN_ROWS,
    };
    let min_b = min_a;
    let ratio_min = (min_a as f32 / total as f32).max(RATIO_MIN);
    let ratio_max = (1.0 - min_b as f32 / total as f32).min(RATIO_MAX);
    let clamped = if ratio_min > ratio_max {
        0.5
    } else {
        ratio.clamp(ratio_min, ratio_max)
    };

    let total_i = total as i32;
    let min_a_i = min_a as i32;
    let min_b_i = min_b as i32;
    let max_a = total_i - min_b_i;
    let mut a_size = (total as f32 * clamped).round() as i32;
    if max_a < min_a_i {
        a_size = total_i / 2;
    } else {
        a_size = a_size.clamp(min_a_i, max_a);
    }
    let a_size = (a_size as u16).min(total);
    let b_size = total.saturating_sub(a_size);
    let used_ratio = if total == 0 {
        clamped
    } else {
        a_size as f32 / total as f32
    };

    match dir {
        SplitDirection::Vertical => {
            let rect_a = Rect {
                x: area.x,
                y: area.y,
                width: a_size,
                height: area.height,
            };
            let rect_b = Rect {
                x: area.x.saturating_add(a_size),
                y: area.y,
                width: b_size,
                height: area.height,
            };
            let pos = rect_b.x;
            (rect_a, rect_b, pos, used_ratio)
        }
        SplitDirection::Horizontal => {
            let rect_a = Rect {
                x: area.x,
                y: area.y,
                width: area.width,
                height: a_size,
            };
            let rect_b = Rect {
                x: area.x,
                y: area.y.saturating_add(a_size),
                width: area.width,
                height: b_size,
            };
            let pos = rect_b.y;
            (rect_a, rect_b, pos, used_ratio)
        }
    }
}

fn splitter_hit_rect(area: Rect, dir: SplitDirection, pos: u16) -> Rect {
    let hit = SPLITTER_HIT.min(match dir {
        SplitDirection::Vertical => area.width,
        SplitDirection::Horizontal => area.height,
    });
    let half = hit / 2;

    match dir {
        SplitDirection::Vertical => {
            let min_x = area.x;
            let max_x = area.x.saturating_add(area.width.saturating_sub(hit));
            let start = pos.saturating_sub(half).clamp(min_x, max_x);
            Rect {
                x: start,
                y: area.y,
                width: hit,
                height: area.height,
            }
        }
        SplitDirection::Horizontal => {
            let min_y = area.y;
            let max_y = area.y.saturating_add(area.height.saturating_sub(hit));
            let start = pos.saturating_sub(half).clamp(min_y, max_y);
            Rect {
                x: area.x,
                y: start,
                width: area.width,
                height: hit,
            }
        }
    }
}
