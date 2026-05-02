use lattice_agent::Agent;
use lattice_core::resolve;

fn main() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let resolved = resolve("deepseek-v4-pro").expect("resolve");
    println!("provider={}", resolved.provider);

    rt.block_on(async {
        let mut agent = Agent::new(resolved);
        println!("calling send_message via our runtime...");

        let events = agent.send_message("Say hi in one word.").await;
        println!("got {} events", events.len());
    });
}
