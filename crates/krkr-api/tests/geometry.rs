//! Integration tests for [`krkr_api::geometry`].

use krkr_api::geometry::*;

#[test]
fn rect_edges_and_center() {
    let r = RectF::new(2.0, 4.0, 6.0, 8.0);
    assert_eq!(r.right(), 8.0);
    assert_eq!(r.bottom(), 12.0);
    assert_eq!(r.center(), (5.0, 8.0));
    assert_eq!(r.min(), (2.0, 4.0));
    assert_eq!(r.max(), (8.0, 12.0));
}

#[test]
fn rect_from_min_max() {
    let r = RectF::from_min_max(1.0, 2.0, 5.0, 7.0);
    assert_eq!(r, RectF::new(1.0, 2.0, 4.0, 5.0));
}

#[test]
fn rect_contains_is_half_open() {
    let r = RectF::new(0.0, 0.0, 10.0, 10.0);
    assert!(r.contains((0.0, 0.0)));
    assert!(r.contains((9.999, 9.999)));
    assert!(!r.contains((10.0, 5.0)));
    assert!(!r.contains((5.0, -0.001)));
    assert!(!r.contains((-0.001, 5.0)));
}

#[test]
fn rect_intersection_and_union() {
    let a = RectF::new(0.0, 0.0, 10.0, 10.0);
    let b = RectF::new(5.0, 5.0, 10.0, 10.0);
    assert_eq!(a.intersection(&b), Some(RectF::new(5.0, 5.0, 5.0, 5.0)));
    assert_eq!(a.union(&b), RectF::new(0.0, 0.0, 15.0, 15.0));

    let c = RectF::new(20.0, 20.0, 1.0, 1.0);
    assert_eq!(a.intersection(&c), None);
    assert!(RectF::default().is_empty());
}

#[test]
fn pixel_region_helpers() {
    let r = PixelRegion::new(2, 4, 6, 8);
    assert_eq!(r.right(), 8);
    assert_eq!(r.bottom(), 12);
    assert_eq!(r.area(), 48);
    assert!(r.contains((2, 4)));
    assert!(!r.contains((8, 4)));
    assert!(!r.contains((2, 12)));
}

#[test]
fn pixel_region_saturates_and_intersects() {
    let r = PixelRegion::new(u32::MAX - 1, 0, 4, 1);
    assert_eq!(r.right(), u32::MAX);

    let a = PixelRegion::new(0, 0, 10, 10);
    let b = PixelRegion::new(5, 5, 10, 10);
    assert_eq!(a.intersection(&b), Some(PixelRegion::new(5, 5, 5, 5)));
    assert_eq!(a.union(&b), PixelRegion::new(0, 0, 15, 15));
    assert_eq!(a.intersection(&PixelRegion::new(20, 20, 1, 1)), None);
}

#[test]
fn affine_applies_translation_and_scale() {
    let a = Affine2::scale(2.0, 3.0).then(&Affine2::translation(1.0, -1.0));
    assert_eq!(a.transform_point((2.0, 2.0)), (5.0, 5.0));
    assert_eq!(a.transform_vector((2.0, 2.0)), (4.0, 6.0));
}

#[test]
fn affine_rotation_is_quarter_turn() {
    let r = Affine2::rotation(std::f32::consts::FRAC_PI_2);
    let (x, y) = r.transform_point((1.0, 0.0));
    assert!(x.abs() < 1e-6);
    assert!((y - 1.0).abs() < 1e-6);
}

#[test]
fn affine_inverse_round_trips() {
    let a = Affine2::from_transform2d(&Transform2D {
        translate: (3.0, -2.0),
        scale: (2.0, 4.0),
        rotation: 0.5,
        pivot: (10.0, 10.0),
    });
    let inv = a.inverse().expect("non-singular");
    let p = (7.0, 9.0);
    let q = a.transform_point(p);
    let back = inv.transform_point(q);
    assert!((back.0 - p.0).abs() < 1e-3);
    assert!((back.1 - p.1).abs() < 1e-3);
    assert!(Affine2::scale(0.0, 1.0).inverse().is_none());
}

#[test]
fn transform2d_pivots_rotation() {
    let t = Transform2D {
        translate: (0.0, 0.0),
        scale: (1.0, 1.0),
        rotation: std::f32::consts::FRAC_PI_2,
        pivot: (1.0, 0.0),
    };
    let p = t.to_affine().transform_point((1.0, 0.0));
    assert!((p.0 - 1.0).abs() < 1e-6);
    assert!(p.1.abs() < 1e-6);
    assert!(Transform2D::default().is_identity());
}
