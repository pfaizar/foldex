//! Workspace geometry and tab membership; no filesystem or renderer ownership.
use egui::{Rect, pos2};
use serde::{Deserialize, Serialize};

#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Node {
    Leaf {
        group: u64,
    },
    Split {
        id: u64,
        vertical: bool,
        children: Vec<Node>,
        weights: Vec<f32>,
    },
}

#[derive(Clone, Serialize, Deserialize)]
pub struct GroupState {
    pub id: u64,
    /// Indexes into the serialized workspace tab array, never filesystem row IDs.
    pub tabs: Vec<usize>,
    pub active: usize,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct TreeState {
    pub root: Node,
    pub groups: Vec<GroupState>,
    pub focused: u64,
}

#[derive(Clone, Copy)]
pub struct Divider {
    pub node: u64,
    pub after: usize,
    pub vertical: bool,
    pub rect: Rect,
    pub extent: f32,
}

impl Node {
    fn visual_bounds(&self, groups: &[(u64, Rect)]) -> Option<Rect> {
        match self {
            Self::Leaf { group } => groups
                .iter()
                .find(|(id, _)| id == group)
                .map(|(_, rect)| *rect)
                .filter(|rect| rect.is_finite()),
            Self::Split { children, .. } => children
                .iter()
                .filter_map(|child| child.visual_bounds(groups))
                .reduce(Rect::union),
        }
    }
    pub fn visual_divider(&self, node: u64, after: usize, groups: &[(u64, Rect)]) -> Option<Rect> {
        match self {
            Self::Split {
                id,
                vertical,
                children,
                ..
            } if *id == node => {
                let left = children.get(after)?.visual_bounds(groups)?;
                let right = children.get(after + 1)?.visual_bounds(groups)?;
                Some(if *vertical {
                    Rect::from_min_max(
                        pos2(left.left().min(right.left()), left.bottom()),
                        pos2(
                            left.right().max(right.right()),
                            right.top().max(left.bottom()),
                        ),
                    )
                } else {
                    Rect::from_min_max(
                        pos2(left.right(), left.top().min(right.top())),
                        pos2(
                            right.left().max(left.right()),
                            left.bottom().max(right.bottom()),
                        ),
                    )
                })
            }
            Self::Split { children, .. } => children
                .iter()
                .find_map(|child| child.visual_divider(node, after, groups)),
            _ => None,
        }
    }
    pub fn hash_state(&self, h: &mut impl std::hash::Hasher) {
        use std::hash::Hash;
        match self {
            Self::Leaf { group } => {
                0_u8.hash(h);
                group.hash(h);
            }
            Self::Split {
                id,
                vertical,
                children,
                weights,
            } => {
                1_u8.hash(h);
                id.hash(h);
                vertical.hash(h);
                children.len().hash(h);
                for weight in weights {
                    weight.to_bits().hash(h);
                }
                for child in children {
                    child.hash_state(h);
                }
            }
        }
    }
    pub fn leaf(group: u64) -> Self {
        Self::Leaf { group }
    }

    pub fn contains(&self, group: u64) -> bool {
        match self {
            Self::Leaf { group: own } => *own == group,
            Self::Split { children, .. } => children.iter().any(|child| child.contains(group)),
        }
    }

    /// Splitting within a same-axis sibling group redistributes that group.
    pub fn split(&mut self, target: u64, new_group: u64, node_id: u64, vertical: bool) -> bool {
        match self {
            Self::Leaf { group } if *group == target => {
                *self = Self::Split {
                    id: node_id,
                    vertical,
                    children: vec![Self::leaf(target), Self::leaf(new_group)],
                    weights: vec![1.0, 1.0],
                };
                true
            }
            Self::Split {
                vertical: axis,
                children,
                weights,
                ..
            } => {
                if *axis == vertical
                    && let Some(at) = children
                        .iter()
                        .position(|child| matches!(child, Self::Leaf { group } if *group == target))
                {
                    children.insert(at + 1, Self::leaf(new_group));
                    *weights = vec![1.0; children.len()];
                    true
                } else {
                    children
                        .iter_mut()
                        .any(|child| child.split(target, new_group, node_id, vertical))
                }
            }
            _ => false,
        }
    }

    /// Returns false only if removing this subtree's last leaf.
    pub fn remove(&mut self, group: u64) -> bool {
        match self {
            Self::Leaf { group: own } => *own != group,
            Self::Split {
                children, weights, ..
            } => {
                let mut at = 0;
                while at < children.len() {
                    if children[at].remove(group) {
                        at += 1;
                    } else {
                        children.remove(at);
                        if at < weights.len() {
                            weights.remove(at);
                        }
                    }
                }
                if children.is_empty() {
                    return false;
                }
                if children.len() == 1 {
                    *self = children.pop().unwrap();
                }
                true
            }
        }
    }

    pub fn leaves(&self, out: &mut Vec<u64>) {
        match self {
            Self::Leaf { group } => out.push(*group),
            Self::Split { children, .. } => children.iter().for_each(|child| child.leaves(out)),
        }
    }

    pub fn geometry(
        &self,
        rect: Rect,
        pixels_per_point: f32,
        gap: f32,
        leaves: &mut Vec<(u64, Rect)>,
        dividers: &mut Vec<Divider>,
    ) {
        match self {
            Self::Leaf { group } => leaves.push((*group, rect)),
            Self::Split {
                id,
                vertical,
                children,
                weights,
            } => {
                if children.is_empty() {
                    return;
                }
                let extent = if *vertical {
                    rect.height()
                } else {
                    rect.width()
                };
                let gap = gap.min(extent.max(0.0) / children.len() as f32);
                let usable = (extent - gap * children.len().saturating_sub(1) as f32).max(0.0);
                let weight = |at: usize| {
                    weights
                        .get(at)
                        .copied()
                        .filter(|w| w.is_finite() && *w > 0.0)
                        .unwrap_or(1.0)
                        .clamp(0.000001, 1_000_000.0)
                };
                let sum: f32 = (0..children.len()).map(weight).sum();
                let origin = if *vertical { rect.top() } else { rect.left() };
                let limit = if *vertical {
                    rect.bottom()
                } else {
                    rect.right()
                };
                let snap = |value: f32| (value * pixels_per_point).round() / pixels_per_point;
                let mut cursor = origin;
                let mut cumulative = 0.0;
                for (at, child) in children.iter().enumerate() {
                    cumulative += weight(at);
                    let end = if at + 1 == children.len() {
                        limit
                    } else {
                        snap(origin + usable * cumulative / sum + gap * at as f32)
                            .clamp(cursor, limit)
                    };
                    let area = if *vertical {
                        Rect::from_min_max(pos2(rect.left(), cursor), pos2(rect.right(), end))
                    } else {
                        Rect::from_min_max(pos2(cursor, rect.top()), pos2(end, rect.bottom()))
                    };
                    child.geometry(area, pixels_per_point, gap, leaves, dividers);
                    if at + 1 < children.len() {
                        let next = (end + gap).min(limit);
                        let divider = if *vertical {
                            Rect::from_min_max(pos2(rect.left(), end), pos2(rect.right(), next))
                        } else {
                            Rect::from_min_max(pos2(end, rect.top()), pos2(next, rect.bottom()))
                        };
                        dividers.push(Divider {
                            node: *id,
                            after: at,
                            vertical: *vertical,
                            rect: divider,
                            extent: usable,
                        });
                        cursor = next;
                    }
                }
            }
        }
    }

    /// Only the two adjacent siblings change size; other divider anchors stay fixed.
    pub fn resize(&mut self, node: u64, after: usize, delta: f32, extent: f32) -> bool {
        match self {
            Self::Split {
                id,
                children,
                weights,
                ..
            } if *id == node => {
                if after + 1 >= children.len() || extent <= 0.0 {
                    return false;
                }
                if weights.len() != children.len()
                    || weights.iter().any(|w| !w.is_finite() || *w <= 0.0)
                {
                    *weights = vec![1.0; children.len()];
                }
                let sum: f32 = weights.iter().sum();
                let pair = weights[after] + weights[after + 1];
                let minimum = (12.0 / extent * sum).min(pair * 0.45).max(pair * 0.001);
                let first = (weights[after] + delta / extent * sum).clamp(minimum, pair - minimum);
                weights[after] = first;
                weights[after + 1] = pair - first;
                true
            }
            Self::Split { children, .. } => children
                .iter_mut()
                .any(|child| child.resize(node, after, delta, extent)),
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn nested_splits_preserve_tabs_and_equal_siblings_without_overlapping() {
        let mut node = Node::leaf(1);
        assert!(node.split(1, 2, 10, false));
        assert!(node.split(2, 3, 11, true));
        assert!(node.split(3, 4, 12, false));
        assert!(node.split(4, 5, 13, false));
        let (mut leaves, mut dividers) = (vec![], vec![]);
        node.geometry(
            Rect::from_min_max(pos2(280.0, 0.0), pos2(1128.0, 785.0)),
            1.25,
            4.0,
            &mut leaves,
            &mut dividers,
        );
        assert_eq!(leaves.len(), 5);
        for (at, (_, rect)) in leaves.iter().enumerate() {
            assert!(rect.is_finite() && rect.width() >= 0.0 && rect.height() >= 0.0);
            for (_, other) in &leaves[at + 1..] {
                assert!(!rect.intersect(*other).is_positive());
            }
        }
        let widths: Vec<_> = leaves
            .iter()
            .filter(|(id, _)| *id >= 3)
            .map(|(_, rect)| rect.width())
            .collect();
        assert!(
            (widths.iter().copied().fold(0.0, f32::max)
                - widths.iter().copied().fold(f32::INFINITY, f32::min))
                < 1.0
        );
        for group in [5, 4, 3, 2] {
            assert!(node.remove(group));
        }
        assert!(matches!(node, Node::Leaf { group: 1 }));
    }
}
