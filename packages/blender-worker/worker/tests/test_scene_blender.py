"""Focused Blender-runtime regression for evaluated camera parenting."""
import os
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
if ROOT not in sys.path:
    sys.path.insert(0, ROOT)

try:
    import bpy
except ModuleNotFoundError:
    import unittest
    raise unittest.SkipTest("requires Blender's bundled Python runtime")
from bpy_extras.object_utils import world_to_camera_view

from renderright.scene import animate, setup


def _objects():
    bpy.ops.mesh.primitive_cube_add(size=1.0, location=(-0.45, 0.0, 0.0))
    first = bpy.context.object
    bpy.ops.mesh.primitive_cube_add(size=0.7, location=(0.45, 0.0, 0.15))
    second = bpy.context.object
    bpy.context.view_layer.update()
    return [first, second]


def _params():
    return {"width": 640, "height": 360, "samples": 1,
            "env": "studio_dark", "transparent": True}


def _assert_center_visible(scene, camera, center):
    scene.camera = camera
    bpy.context.view_layer.update()
    point = world_to_camera_view(scene, camera, center)
    assert point.z > 0.0, "camera center target must be in front of camera"
    assert 0.0 < point.x < 1.0 and 0.0 < point.y < 1.0, "camera center target must project onscreen"


def test_film_cameras_keep_world_pose_and_see_center():
    bpy.ops.wm.read_factory_settings(use_empty=True)
    objects = _objects()
    data = setup(objects, _params())
    scene = bpy.context.scene
    animate("film", data, 16)
    assert len(data["shots"]) == 4, "full film should have four camera shots"
    for shot in data["shots"]:
        camera = bpy.data.objects["Film camera - " + shot["id"]]
        for frame in (shot["start_frame"], shot["end_frame"] - 1):
            scene.frame_set(frame)
            _assert_center_visible(scene, camera, data["center"])


def test_turntable_parenting_preserves_starting_camera_pose():
    bpy.ops.wm.read_factory_settings(use_empty=True)
    objects = _objects()
    data = setup(objects, _params())
    scene = bpy.context.scene
    bpy.context.view_layer.update()
    original = data["camera"].matrix_world.copy()
    animate("turntable", data, 16)
    scene.frame_set(0)
    bpy.context.view_layer.update()
    actual = data["camera"].matrix_world
    error = max(abs(original[row][col] - actual[row][col]) for row in range(4) for col in range(4))
    assert error < 1e-5, "turntable first frame must preserve the original camera world matrix"


test_film_cameras_keep_world_pose_and_see_center()
test_turntable_parenting_preserves_starting_camera_pose()
print("RenderRight scene camera Blender regressions passed")
