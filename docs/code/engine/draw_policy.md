# `engine::draw_policy` — the subtree draw policy

Source: `crates/engine/src/draw_policy.rs`

The GPU is the primary path: a layer's subtree is drawn node by node on the
GPU when the backend can draw every node in it. A node whose blend mode reads
the destination needs a copy of the destination's pixels, and a backend that
cannot supply one cannot draw that node at all. The CPU compositor reads the
destination for every pixel, so it draws such a mode exactly, and the
composited subtree is handed to the GPU as **one texture**.

The decision is made **once per subtree**, never per node. A subtree's nodes
blend against each other — the later node's mode is evaluated over the pixels
the earlier one wrote — so a subtree split between the two paths would break
that order, and the CPU half would need the copy of the destination the GPU
holds.

---

## 1. Responsibility

* Decide, for the scene, which subtrees the backend cannot draw.
* Composite those subtrees on the CPU and hand them to the projection as one
  texture each.

---

## 2. State it owns

None. The policy is a function over the [`Scene`](scene.md): it reads the
layer tree and writes the composited content into the scene's own bitmaps.
The engine keeps the list of layers it has made cache consumers, so a layer
that stops being a root gives its cache up.

---

## 3. Public API

```rust
pub fn cpu_composite_roots(scene: &Scene) -> Vec<u32>;
pub fn composite_cpu_subtrees(scene: &mut Scene, registered: &mut Vec<u32>);
```

`cpu_composite_roots` returns the layer each composited subtree is rooted at.
`composite_cpu_subtrees` composites them and keeps `registered` in step with
the roots.

---

## 4. The four facts the decision reads

| fact | where it comes from |
|---|---|
| the blend modes of the layers in the subtree | [`Scene::layers`](scene.md) — `LayerState::blend` |
| the clip kinds the projection composes for them | [`engine::projection`](projection.md) — `composed_clip` over `compose_layer` |
| the backend's capabilities | `render::capabilities` — the engine copies `supports_destination_dependent_blend` into `Scene::gpu_draws_destination_dependent_blends` |
| the modes that read the destination | `render::blend::needs_destination` — the render layer's name for `BlendMode::is_destination_dependent` |

The last one is the render layer's own: the engine asks it whether a mode
reads the destination and never learns which modes those are.

---

## 5. Where a subtree is rooted

The composited subtree is drawn with the blend of the layer it is rooted at,
so the root is the **nearest ancestor whose own blend the backend can draw**.
A layer with no such ancestor has no root: the backend draws its own node.

The root is also the layer that becomes a consumer of its own cache, so the
projection draws the composited subtree in place of the layer's own image and
its descendants — the same seam a whole-subtree transition uses.

---

## 6. Invariants

1. **The decision is per subtree.** A subtree is either drawn node by node on
   the GPU or composited on the CPU as one piece; there is no per-node
   choice.
2. **The composite is the GPU path's result.** The CPU compositor reads the
   shared table of per-channel functions, and the GPU's
   destination-dependent program is generated from that same table, so the
   two agree pixel for pixel.
3. **A subtree the CPU cannot draw stays on the GPU.** A clip the CPU cannot
   draw, and a subtree a running transition owns, are left alone.

---

## 7. Tests

`crates/engine/tests/draw_policy.rs` covers the decision, the composite and
the projection that draws it.
