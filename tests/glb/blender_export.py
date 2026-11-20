import bpy,sys,math
from pathlib import Path
out=Path(sys.argv[sys.argv.index('--')+1]);out.mkdir(exist_ok=True)
for n in ['cube','uv-sphere','torus','cone','two-meshes','material','animation','textured','camera-only']:
 bpy.ops.wm.read_factory_settings(use_empty=True)
 if n=='camera-only':
  bpy.ops.object.camera_add(location=(1,2,3));bpy.context.scene.camera=bpy.context.object
 elif n=='uv-sphere':bpy.ops.mesh.primitive_uv_sphere_add(segments=16,ring_count=8)
 elif n=='torus':bpy.ops.mesh.primitive_torus_add(major_segments=16,minor_segments=8)
 elif n=='cone':bpy.ops.mesh.primitive_cone_add(vertices=13)
 else:bpy.ops.mesh.primitive_cube_add()
 obj=bpy.context.object;obj.name=n
 if n=='two-meshes':bpy.ops.mesh.primitive_ico_sphere_add(subdivisions=1,location=(3,0,0))
 if n in ['material','textured']:
  mat=bpy.data.materials.new('Material');mat.use_nodes=True
  shader=mat.node_tree.nodes.get('Principled BSDF');shader.inputs['Base Color'].default_value=(0.2,0.7,0.1,1)
  if n=='textured':
   image=bpy.data.images.new('Pixels',width=2,height=2);image.pixels[:]=[1,0,0,1,0,1,0,1,0,0,1,1,1,1,0,1];image.pack()
   tex=mat.node_tree.nodes.new('ShaderNodeTexImage');tex.image=image;mat.node_tree.links.new(tex.outputs['Color'],shader.inputs['Base Color'])
  obj.data.materials.append(mat)
 if n=='animation':
  obj.keyframe_insert(data_path='location',frame=1);obj.location=(2,3,4);obj.keyframe_insert(data_path='location',frame=12)
 bpy.ops.export_scene.gltf(filepath=str(out/(n+'.glb')),export_format='GLB',export_cameras=True,export_animations=n=='animation')
 print('EXPORTED',n,flush=True)
