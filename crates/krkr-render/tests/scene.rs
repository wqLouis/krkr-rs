//! Integration tests for the abstract render tree.
//!
//! Relocated from the inline `#[cfg(test)]` module in `src/lib.rs`.

use krkr_api::geometry::RectF;
use krkr_render::{RenderContent, RenderNode, RenderNodeId, RenderScene};

#[test]
fn scene_construction_links_parent_and_children() {
    let mut scene = RenderScene::new(RenderNodeId(0));
    assert!(scene.is_empty());

    let root = scene.push(RenderNode {
        content: RenderContent::Solid([1.0, 0.0, 0.0, 1.0]),
        ..RenderNode::default()
    });
    assert_eq!(root, RenderNodeId(0));
    assert_eq!(scene.len(), 1);

    let child = scene.push(RenderNode {
        parent: Some(root),
        content: RenderContent::None,
        dst: RectF::new(1.0, 2.0, 3.0, 4.0),
        z: 7,
        ..RenderNode::default()
    });
    scene.node_mut(root).expect("root").children.push(child);

    assert_eq!(scene.node(child).unwrap().parent, Some(root));
    assert_eq!(scene.node(root).unwrap().children, vec![child]);
    assert_eq!(
        scene.node(child).unwrap().dst,
        RectF::new(1.0, 2.0, 3.0, 4.0)
    );
    assert_eq!(scene.node(child).unwrap().z, 7);
    assert!(scene.node(RenderNodeId(99)).is_none());

    scene.background = Some([0.0, 0.0, 0.0, 1.0]);
    assert!(scene.background.is_some());
}
