"""Run inside Blender to compare scene, mesh, image and animation data after recovery."""
import bpy
import hashlib
import json
import struct
import sys
from pathlib import Path

args = sys.argv[sys.argv.index("--") + 1:]
out = Path(args[0])
result = {}
for path in sorted(Path(args[1]).glob("*.glb")):
    bpy.ops.wm.read_factory_settings(use_empty=True)
    bpy.ops.import_scene.gltf(filepath=str(path))
    objects = []
    for obj in sorted(bpy.context.scene.objects, key=lambda o: o.name):
        item = {"name": obj.name, "type": obj.type}
        if obj.type == "MESH":
            mesh = obj.data
            mesh.calc_loop_triangles()
            vertices = b"".join(struct.pack("<3f", *v.co) for v in mesh.vertices)
            topology = b"".join(struct.pack("<3I", *f.vertices) for f in mesh.loop_triangles)
            item.update(vertices=len(mesh.vertices), triangles=len(mesh.loop_triangles),
                        geometry_sha256=hashlib.sha256(vertices + topology).hexdigest())
        objects.append(item)
    pixels = {image.name: hashlib.sha256(b"".join(struct.pack("<f", x) for x in image.pixels)).hexdigest()
              for image in bpy.data.images if image.has_data}
    result[path.name] = dict(objects=objects, images=pixels, animations=sorted(a.name for a in bpy.data.actions))
out.write_text(json.dumps(result, indent=2, sort_keys=True) + "\n")
