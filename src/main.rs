#[macro_use]
pub mod multiprocessor;
pub mod lok;
pub mod dummy;

#[cfg(test)]
mod tests {
    use super::lok::*;
    use super::dummy::*;
    use std::path::PathBuf;

    #[tokio::test(flavor = "multi_thread")]
    async fn test_dummy_pool() {
        let r = DUMMY_POOL.process(DummyIn(21)).await.unwrap();
        assert_eq!(r.0, 42);
        println!("Dummy pool: OK");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_lok_convert() {
        let dir = PathBuf::from("/tmp/lok-test-ci");
        std::fs::create_dir_all(&dir).unwrap();
        let input = dir.join("test.txt");
        let output = dir.join("test.pdf");
        std::fs::write(&input, "Hello").unwrap();
        LOK_POOL.process(LokInput { input, output: output.clone(), format: "pdf".into() }).await.unwrap();
        assert!(output.exists());
        println!("LOK convert: OK ({} bytes)", std::fs::metadata(&output).unwrap().len());
    }
}

fn main() {}
