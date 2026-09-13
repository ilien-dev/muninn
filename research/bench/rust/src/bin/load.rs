use model2vec_rs::model::StaticModel;
use std::time::Instant;
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let t0 = Instant::now();
    let model = StaticModel::from_pretrained("minishlab/potion-code-16M-v2", None, None, None)?;
    let load = t0.elapsed().as_secs_f64() * 1000.0;
    let t = Instant::now();
    let _ = model.encode(&["primera consulta tras arrancar".to_string()]);
    let first = t.elapsed().as_secs_f64() * 1000.0;
    let t = Instant::now();
    for _ in 0..100 { let _ = model.encode(&["consulta de calentamiento".to_string()]); }
    let warm = t.elapsed().as_secs_f64() * 1000.0 / 100.0;
    println!("carga {load:.1} ms | primer encode {first:.3} ms | encode caliente {warm:.3} ms");
    Ok(())
}
