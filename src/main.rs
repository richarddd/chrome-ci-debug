use chromiumoxide::browser::{Browser, BrowserConfig};
use futures::StreamExt;
use std::path::PathBuf;

fn chrome_path() -> Option<PathBuf> {
    [
        "/opt/bin/chromium",
        "/usr/bin/chromium-browser",
        "/usr/bin/chromium",
        "/usr/bin/google-chrome-stable",
        "/usr/bin/google-chrome",
    ]
    .iter()
    .map(PathBuf::from)
    .find(|p| p.exists())
}

#[tokio::main]
async fn main() {
    let path = chrome_path();
    println!("Chrome path: {:?}", path);

    // First try running chrome directly to see stderr
    if let Some(ref p) = path {
        println!("\n=== Direct chrome launch test ===");
        let output = std::process::Command::new(p)
            .args([
                "--headless",
                "--no-sandbox",
                "--no-zygote",
                "--single-process",
                "--disable-gpu",
                "--disable-dev-shm-usage",
                "--remote-debugging-port=0",
                "--user-data-dir=/tmp/chrome-direct-test",
                "about:blank",
            ])
            .output();
        match output {
            Ok(o) => {
                println!("Exit status: {}", o.status);
                println!("stdout: {}", String::from_utf8_lossy(&o.stdout));
                println!("stderr: {}", String::from_utf8_lossy(&o.stderr));
            }
            Err(e) => println!("Failed to run: {}", e),
        }
    }

    // Now try via chromiumoxide
    println!("\n=== chromiumoxide launch test ===");
    let user_data_dir = std::env::temp_dir().join("chrome-oxide-test");

    let mut builder = BrowserConfig::builder()
        .no_sandbox()
        .args(vec![
            "--no-zygote",
            "--single-process",
            "--disable-gpu",
            "--disable-dev-shm-usage",
        ])
        .user_data_dir(&user_data_dir);

    if let Some(p) = path {
        builder = builder.chrome_executable(p);
    }

    let config = builder.build().unwrap();

    match Browser::launch(config).await {
        Ok((browser, mut handler)) => {
            println!("Browser launched successfully!");
            tokio::spawn(async move {
                while let Some(h) = handler.next().await {
                    if h.is_err() {
                        break;
                    }
                }
            });
            let page = browser.new_page("about:blank").await;
            println!("New page result: {:?}", page.is_ok());
            drop(browser);
        }
        Err(e) => {
            println!("Browser launch failed: {}", e);
            println!("Debug: {:?}", e);
        }
    }
}
