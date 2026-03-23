use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Serialize, Deserialize, Clone)]
pub struct LokInput { pub input: PathBuf, pub output: PathBuf, pub format: String }
#[derive(Serialize, Deserialize)]
pub struct LokOutput;

fn lok_init() -> Result<libreofficekit::Office> {
    unsafe {
        std::env::set_var("SAL_USE_VCLPLUGIN", "svp");
        std::env::set_var("UserInstallation",
            format!("file:///tmp/lok_profile_{}", std::process::id()));
    }
    let log = |msg: &str| {
        let _ = std::fs::OpenOptions::new().create(true).append(true)
            .open("/tmp/forkpool_deferred.log")
            .and_then(|mut f| { use std::io::Write; writeln!(f, "pid={} {msg}", std::process::id()) });
    };
    log(&format!("lok_init called, ppid={}", nix::unistd::getppid()));
    let path = libreofficekit::Office::find_install_path()
        .ok_or_else(|| anyhow::anyhow!("LibreOffice not found"))?;
    log(&format!("calling Office::new at {}", path.display()));
    let result = libreofficekit::Office::new(&path);
    log(&format!("Office::new result={:?}", result.as_ref().map(|_| "ok")));
    result.map_err(|e| anyhow::anyhow!("{e:?}"))
}

pub fn lok_work(office: &libreofficekit::Office, input: LokInput) -> Result<LokOutput> {
    use libreofficekit::DocUrl;
    let i = DocUrl::from_path(&input.input)?;
    let o = DocUrl::from_path(&input.output)?;
    let mut doc = office.document_load(&i)?;
    doc.save_as(&o, &input.format, None)?;
    Ok(LokOutput)
}

fork_pool!(LOK_POOL, LokInput => LokOutput, { init: lok_init, work: lok_work, concurrency: 1, deferred: true });
