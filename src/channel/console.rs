//! The terminal channel: a simple REPL for everyday chatting and debugging.

use crate::agent::Agent;
use anyhow::Result;
use std::io::Write;
use std::sync::Arc;
use tokio::sync::Mutex;

pub async fn run(agent: Arc<Mutex<Agent>>) -> Result<()> {
    println!(
        "{} v{} — terminal channel",
        env!("CARGO_PKG_NAME"),
        env!("CARGO_PKG_VERSION")
    );
    println!("Type /help for commands, /quit to exit.\n");
    let stdin = std::io::stdin();
    loop {
        print!("you> ");
        std::io::stdout().flush()?;
        let mut line = String::new();
        // A blocking read. That is fine here: tokio's multithreaded runtime
        // keeps the scheduler and other background tasks running on its
        // other worker threads.
        let bytes_read = stdin.read_line(&mut line)?;
        if bytes_read == 0 {
            // EOF (Ctrl+D): no more input will ever arrive — leave cleanly
            // instead of spinning on an empty line.
            println!("\nbye!");
            return Ok(());
        }
        match line.trim() {
            "" => continue,
            "/quit" | "/exit" => {
                println!("bye!");
                return Ok(());
            }
            "/help" => {
                println!(
                    "/quit or /exit — leave the chat\n/help — this help\nanything else goes straight to the model"
                );
                continue;
            }
            text => {
                let mut agent = agent.lock().await;
                match agent.handle(text).await {
                    Ok(reply) => println!("\n{}\n", reply.trim()),
                    // Show the error but keep chatting — a failed model call
                    // must not kill the session.
                    Err(e) => eprintln!("\nerror: {e:#}\n"),
                }
            }
        }
    }
}
