use lok_test::lok::*;
use lok_test::pdfium::*;
use std::path::PathBuf;

#[tokio::test(flavor = "multi_thread")]
async fn test_lok_from_second_binary() {
    let dir = PathBuf::from("/tmp/lok-test-bin2");
    std::fs::create_dir_all(&dir).unwrap();
    let input = dir.join("test.txt");
    let output = dir.join("test.pdf");
    std::fs::write(&input, "Hello from binary 2").unwrap();
    LOK_POOL.process(LokInput { input, output: output.clone(), format: "pdf".into() }).await.unwrap();
    assert!(output.exists());
    println!("LOK from bin2: OK ({} bytes)", std::fs::metadata(&output).unwrap().len());
}

#[tokio::test(flavor = "multi_thread")]
async fn test_heavy_from_second_binary() {
    let result = HEAVY_POOL.process(HeavyIn(100)).await.unwrap();
    assert_eq!(result.0, 200);
    println!("Heavy from bin2: OK");
}
