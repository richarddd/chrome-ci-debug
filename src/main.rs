use chromiumoxide::browser::{Browser, BrowserConfig};
use futures::StreamExt;
use std::path::PathBuf;

fn chrome_path() -> Option<PathBuf> {
    [
        "/usr/bin/google-chrome-stable",
        "/usr/bin/google-chrome",
        "/opt/bin/chromium",
        "/usr/bin/chromium-browser",
        "/usr/bin/chromium",
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
        println!("\n=== Direct chrome launch test ({}) ===", p.display());
        let mut child = std::process::Command::new(p)
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
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .expect("failed to spawn");

        std::thread::sleep(std::time::Duration::from_secs(5));
        match child.try_wait() {
            Ok(Some(status)) => {
                let output = child.wait_with_output().unwrap();
                println!("Exited: {}", status);
                println!("stderr: {}", String::from_utf8_lossy(&output.stderr));
            }
            Ok(None) => {
                println!("Still running after 5s — chrome works, killing");
                child.kill().ok();
            }
            Err(e) => println!("Error: {}", e),
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
