//! usd-lite: a layer read, composed with itself as every layer it names, and walked.
#![no_main]
struct Echo(Vec<u8>);
impl usd_lite::Resolver for Echo {
    fn resolve(&self, asset: &str, _: &str) -> Option<(String, Vec<u8>)> {
        Some((asset.to_string(), self.0.clone()))
    }
}
libfuzzer_sys::fuzz_target!(|data: &[u8]| {
    let limits = usd_lite::Limits::default().with_max_elements(1 << 20).with_max_decompressed(64 << 20).with_max_prims(2_000);
    let _ = usd_lite::detect(data);
    let Ok(layer) = usd_lite::read_layer(data, &limits) else { return };
    let stage = usd_lite::compose(layer, "root.usda", &Echo(data.to_vec()), &limits);
    let layer = &stage.layer;
    let _ = (usd_lite::geom::up_axis(layer), usd_lite::geom::meters_per_unit(layer));
    let mut queue = layer.root_prims();
    let mut seen = 0;
    while let Some(prim) = queue.pop() {
        seen += 1;
        if seen > 5000 {
            break;
        }
        let _ = layer.type_name(&prim);
        let _ = usd_lite::geom::local_transform(layer, &prim);
        let _ = usd_lite::geom::is_drawn(layer, &prim);
        let _ = usd_lite::geom::mesh(layer, &prim);
        if let Some(m) = usd_lite::geom::bound_material(layer, &prim, true) {
            let _ = usd_lite::geom::preview_surface(layer, &m);
        }
        queue.extend(layer.children(&prim));
    }
});
