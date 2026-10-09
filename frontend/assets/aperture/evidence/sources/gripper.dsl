module replacement_gripper

# One-piece inspection fixture for a future gripper; this is not an actuated assembly.
command GRIPPER(
    jaw_gap: float @ui(widget="slider", label="Jaw opening", min=12.0, max=42.0, step=2.0) = 20.0,
    finger_height: float @ui(widget="slider", label="Finger height", min=20.0, max=50.0, step=2.0) = 32.0,
    mounting_bore: float @ui(label="Mounting clearance", min=3.0, max=8.0) = 4.5
) -> solid:
    require jaw_gap >= 12.0
    require jaw_gap <= 42.0
    require mounting_bore >= 3.0
    let base: solid = box(80.0, 46.0, 10.0)
    let offset: float = jaw_gap / 2.0 + 6.0
    let left: solid = translate(box(12.0, 24.0, finger_height), 0.0 - offset, 0.0, finger_height / 2.0 + 4.0)
    let right: solid = translate(box(12.0, 24.0, finger_height), offset, 0.0, finger_height / 2.0 + 4.0)
    let body: solid = union(base, left, right)
    let a: solid = translate(cylinder(mounting_bore / 2.0, 12.0), -31.0, -15.0, -6.0)
    let b: solid = translate(cylinder(mounting_bore / 2.0, 12.0), -31.0, 15.0, -6.0)
    let c: solid = translate(cylinder(mounting_bore / 2.0, 12.0), 31.0, -15.0, -6.0)
    let d: solid = translate(cylinder(mounting_bore / 2.0, 12.0), 31.0, 15.0, -6.0)
    emit difference(body, a, b, c, d)
