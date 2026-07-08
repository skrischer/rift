use termy_terminal_ui::TmuxPaneState;

#[derive(Debug, Clone, PartialEq)]
pub enum LayoutNode {
    Pane(String),
    Split {
        horizontal: bool,
        children: Vec<(f32, LayoutNode)>,
    },
}

pub fn build_layout(panes: &[TmuxPaneState]) -> LayoutNode {
    if panes.is_empty() {
        return LayoutNode::Pane(String::new());
    }
    if panes.len() == 1 {
        return LayoutNode::Pane(panes[0].id.clone());
    }

    if let Some(groups) = partition_by_left(panes) {
        let total: f32 = groups.iter().map(|g| group_width(g) as f32).sum();
        let children = groups
            .iter()
            .map(|g| (group_width(g) as f32 / total, build_layout(g)))
            .collect();
        return LayoutNode::Split {
            horizontal: true,
            children,
        };
    }

    if let Some(groups) = partition_by_top(panes) {
        let total: f32 = groups.iter().map(|g| group_height(g) as f32).sum();
        let children = groups
            .iter()
            .map(|g| (group_height(g) as f32 / total, build_layout(g)))
            .collect();
        return LayoutNode::Split {
            horizontal: false,
            children,
        };
    }

    LayoutNode::Pane(panes[0].id.clone())
}

/// First non-empty pane id in a subtree, used as the resize target for the
/// border on that side of a split.
pub fn first_pane_id(node: &LayoutNode) -> Option<&str> {
    match node {
        LayoutNode::Pane(id) => (!id.is_empty()).then_some(id.as_str()),
        LayoutNode::Split { children, .. } => {
            children.iter().find_map(|(_, child)| first_pane_id(child))
        }
    }
}

fn partition_by_left(panes: &[TmuxPaneState]) -> Option<Vec<Vec<TmuxPaneState>>> {
    let mut sorted: Vec<_> = panes.to_vec();
    sorted.sort_by_key(|p| p.left);

    let (first, rest) = sorted.split_first()?;
    let mut groups: Vec<Vec<TmuxPaneState>> = Vec::new();
    let mut current = vec![first.clone()];
    let mut max_right = first.left + first.width;

    for pane in rest {
        if pane.left >= max_right {
            groups.push(std::mem::replace(&mut current, vec![pane.clone()]));
            max_right = pane.left + pane.width;
        } else {
            current.push(pane.clone());
            max_right = max_right.max(pane.left + pane.width);
        }
    }
    groups.push(current);

    (groups.len() > 1).then_some(groups)
}

fn partition_by_top(panes: &[TmuxPaneState]) -> Option<Vec<Vec<TmuxPaneState>>> {
    let mut sorted: Vec<_> = panes.to_vec();
    sorted.sort_by_key(|p| p.top);

    let (first, rest) = sorted.split_first()?;
    let mut groups: Vec<Vec<TmuxPaneState>> = Vec::new();
    let mut current = vec![first.clone()];
    let mut max_bottom = first.top + first.height;

    for pane in rest {
        if pane.top >= max_bottom {
            groups.push(std::mem::replace(&mut current, vec![pane.clone()]));
            max_bottom = pane.top + pane.height;
        } else {
            current.push(pane.clone());
            max_bottom = max_bottom.max(pane.top + pane.height);
        }
    }
    groups.push(current);

    (groups.len() > 1).then_some(groups)
}

fn group_width(panes: &[TmuxPaneState]) -> u16 {
    let min_left = panes.iter().map(|p| p.left).min().unwrap_or(0);
    let max_right = panes.iter().map(|p| p.left + p.width).max().unwrap_or(0);
    max_right - min_left
}

fn group_height(panes: &[TmuxPaneState]) -> u16 {
    let min_top = panes.iter().map(|p| p.top).min().unwrap_or(0);
    let max_bottom = panes.iter().map(|p| p.top + p.height).max().unwrap_or(0);
    max_bottom - min_top
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pane(id: &str, left: u16, top: u16, width: u16, height: u16) -> TmuxPaneState {
        TmuxPaneState {
            id: id.to_string(),
            window_id: String::new(),
            session_id: String::new(),
            is_active: false,
            left,
            top,
            width,
            height,
            cursor_x: 0,
            cursor_y: 0,
            current_path: String::new(),
            current_command: String::new(),
        }
    }

    #[test]
    fn test_build_layout_single_pane() {
        let panes = vec![pane("%0", 0, 0, 130, 40)];
        assert_eq!(build_layout(&panes), LayoutNode::Pane("%0".into()));
    }

    #[test]
    fn test_build_layout_horizontal_split() {
        let panes = vec![pane("%0", 0, 0, 65, 40), pane("%1", 66, 0, 64, 40)];
        match build_layout(&panes) {
            LayoutNode::Split {
                horizontal,
                children,
            } => {
                assert!(horizontal);
                assert_eq!(children.len(), 2);
                assert_eq!(children[0].1, LayoutNode::Pane("%0".into()));
                assert_eq!(children[1].1, LayoutNode::Pane("%1".into()));
                assert!((children[0].0 - 0.504).abs() < 0.01);
            }
            other => panic!("expected horizontal split, got {other:?}"),
        }
    }

    #[test]
    fn test_build_layout_vertical_split() {
        let panes = vec![pane("%0", 0, 0, 130, 20), pane("%1", 0, 21, 130, 19)];
        match build_layout(&panes) {
            LayoutNode::Split {
                horizontal,
                children,
            } => {
                assert!(!horizontal);
                assert_eq!(children.len(), 2);
                assert_eq!(children[0].1, LayoutNode::Pane("%0".into()));
                assert_eq!(children[1].1, LayoutNode::Pane("%1".into()));
            }
            other => panic!("expected vertical split, got {other:?}"),
        }
    }

    #[test]
    fn test_build_layout_nested() {
        // | A | B |
        // | C C C |
        let panes = vec![
            pane("%0", 0, 0, 65, 20),
            pane("%1", 66, 0, 64, 20),
            pane("%2", 0, 21, 130, 19),
        ];
        match build_layout(&panes) {
            LayoutNode::Split {
                horizontal,
                children,
            } => {
                assert!(!horizontal, "top-level should be vertical split");
                assert_eq!(children.len(), 2);
                match &children[0].1 {
                    LayoutNode::Split {
                        horizontal,
                        children,
                    } => {
                        assert!(horizontal, "top row should be horizontal split");
                        assert_eq!(children.len(), 2);
                    }
                    other => panic!("expected nested horizontal split, got {other:?}"),
                }
                assert_eq!(children[1].1, LayoutNode::Pane("%2".into()));
            }
            other => panic!("expected vertical split, got {other:?}"),
        }
    }

    #[test]
    fn test_build_layout_three_columns() {
        let panes = vec![
            pane("%0", 0, 0, 42, 40),
            pane("%1", 43, 0, 43, 40),
            pane("%2", 87, 0, 43, 40),
        ];
        match build_layout(&panes) {
            LayoutNode::Split {
                horizontal,
                children,
            } => {
                assert!(horizontal);
                assert_eq!(children.len(), 3);
            }
            other => panic!("expected 3-way horizontal split, got {other:?}"),
        }
    }

    #[test]
    fn test_build_layout_empty() {
        assert_eq!(build_layout(&[]), LayoutNode::Pane(String::new()));
    }

    #[test]
    fn test_partition_by_left_empty_returns_none() {
        assert_eq!(partition_by_left(&[]), None);
    }

    #[test]
    fn test_partition_by_top_empty_returns_none() {
        assert_eq!(partition_by_top(&[]), None);
    }

    #[test]
    fn test_first_pane_id_single() {
        assert_eq!(first_pane_id(&LayoutNode::Pane("%3".into())), Some("%3"));
    }

    #[test]
    fn test_first_pane_id_empty_pane_is_none() {
        assert_eq!(first_pane_id(&LayoutNode::Pane(String::new())), None);
    }

    #[test]
    fn test_first_pane_id_nested_returns_leftmost() {
        let panes = vec![
            pane("%0", 0, 0, 65, 20),
            pane("%1", 66, 0, 64, 20),
            pane("%2", 0, 21, 130, 19),
        ];
        assert_eq!(first_pane_id(&build_layout(&panes)), Some("%0"));
    }
}
