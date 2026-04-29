#[tokio::main]
async fn main() {
    for model in &["minimax-m2.7", "minimax-m2.5"] {
        match artemis_core::resolve(model) {
            Ok(r) => println!(
                "{}: provider={} proto={:?} base={}",
                model, r.provider, r.api_protocol, r.base_url
            ),
            Err(e) => println!("{}: RESOLVE ERROR {:?}", model, e),
        }
    }
}
