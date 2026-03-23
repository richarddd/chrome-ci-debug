#[macro_use]
pub mod multiprocessor;
pub mod lok;
pub mod pdfium;

#[cfg(test)]
mod tests {
    use super::lok::*;
    use super::pdfium::*;
    use std::path::PathBuf;

    #[tokio::test(flavor = "multi_thread")]
    async fn test_lok_convert() {
        let dir = PathBuf::from("/tmp/lok-test-1");
        std::fs::create_dir_all(&dir).unwrap();
        let input = dir.join("test.txt");
        let output = dir.join("test.pdf");
        std::fs::write(&input, "Hello from test 1").unwrap();
        LOK_POOL.process(LokInput { input, output: output.clone(), format: "pdf".into() }).await.unwrap();
        assert!(output.exists());
        println!("LOK convert 1: OK ({} bytes)", std::fs::metadata(&output).unwrap().len());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_lok_concurrent() {
        let mut handles = vec![];
        for i in 0..4 {
            let dir = PathBuf::from(format!("/tmp/lok-concurrent-{i}"));
            std::fs::create_dir_all(&dir).unwrap();
            let input = dir.join("test.txt");
            let output = dir.join("test.pdf");
            std::fs::write(&input, format!("Hello concurrent {i}")).unwrap();
            handles.push(tokio::spawn(async move {
                LOK_POOL.process(LokInput { input, output: output.clone(), format: "pdf".into() }).await.unwrap();
                assert!(output.exists());
            }));
        }
        for h in handles {
            h.await.unwrap();
        }
        println!("LOK concurrent: OK");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_heavy_pool() {
        let result = HEAVY_POOL.process(HeavyIn(21)).await.unwrap();
        assert_eq!(result.0, 42);
        println!("Heavy pool: OK");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_heavy_concurrent() {
        let mut handles = vec![];
        for i in 0..10 {
            handles.push(tokio::spawn(async move {
                let r = HEAVY_POOL.process(HeavyIn(i)).await.unwrap();
                assert_eq!(r.0, i * 2);
            }));
        }
        for h in handles {
            h.await.unwrap();
        }
        println!("Heavy concurrent: OK");
    }
}

fn main() {}
