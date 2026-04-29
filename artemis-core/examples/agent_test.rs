use artemis_core::resolve;
use artemis_agent::Agent;

fn main() {
    // Create our own runtime instead of using SHARED_RUNTIME
    let rt = tokio::runtime::Runtime::new().unwrap();
    let resolved = resolve("deepseek-v4-pro").expect("resolve");
    println!("provider={}", resolved.provider);
    
    let mut agent = Agent::new(resolved);
    println!("calling send_message via our runtime...");
    
    let events = agent.send_message("Say hi in one word.");
    println!("got {} events", events.len());
    
    drop(rt);
}
