use std::{path::PathBuf, sync::{Arc, atomic::{AtomicBool, Ordering}}, io::Read};
use anyhow::{anyhow, bail, Result};
use sha2::{Digest, Sha256};
use super::native_admission::{reserve_host, HostAdmissionError};
use crate::resource_scheduler::{Demand,ResourceScheduler};
#[derive(Debug,PartialEq,Eq)]
pub(crate) struct RuntimeInputs(pub(crate) Vec<(PathBuf,String)>);
struct Abandoned(Arc<AtomicBool>);
impl Drop for Abandoned {fn drop(&mut self){self.0.store(true,Ordering::Relaxed);}}
pub(crate) async fn capture(cancel:Arc<AtomicBool>)->Result<RuntimeInputs> {
    let paths=super::native_runtime::candidates("llama-server","FILEID_LLAMA_SERVER",cancel.clone()).await?;
    if paths.is_empty(){bail!("No installed server runtime for resident owner");}
    capture_paths(paths,cancel).await
}
pub(crate) async fn capture_paths(paths:Vec<PathBuf>,cancel:Arc<AtomicBool>)->Result<RuntimeInputs> {
    let resources=reserve_host(ResourceScheduler::shared(),Demand{memory_mb:16,cpu:1,io:1},cancel.clone()).await?;
    let abandoned=Arc::new(AtomicBool::new(false));let _guard=Abandoned(abandoned.clone());
    tokio::task::spawn_blocking(move || {
        let _resources=resources;let cancelled=||cancel.load(Ordering::Relaxed)||abandoned.load(Ordering::Relaxed);let mut inputs=Vec::new();
        for path in paths {
            if cancelled(){bail!("Runtime fingerprint cancelled");}
            let canonical = std::fs::canonicalize(&path)?;
            let is_canonical = canonical == path || crate::util::path_safety::strip_extended_length(&canonical) == crate::util::path_safety::strip_extended_length(&path);
            if !path.is_absolute() || !is_canonical { bail!("Runtime fingerprint requires canonical absolute path"); }
            let before=std::fs::symlink_metadata(&path)?;if !before.is_file()||before.len()==0||before.len()>200_000_000 {bail!("Runtime fingerprint requires bounded regular bytes");}
            let mut file=std::fs::File::open(&path)?;let opened=file.metadata()?;if !same(&before,&opened)?{bail!("Runtime changed before fingerprint");}
            let mut hash=Sha256::new();let mut buffer=vec![0;1024*1024];let mut size=0u64;
            loop {if cancelled(){bail!("Runtime fingerprint cancelled");}let count=file.read(&mut buffer)?;if count==0{break;}size+=count as u64;if size>before.len(){bail!("Runtime grew during fingerprint");}hash.update(&buffer[..count]);}
            if size!=before.len()||!same(&opened,&file.metadata()?)?||!same(&opened,&std::fs::symlink_metadata(&path)?)?{bail!("Runtime changed during fingerprint");}
            let identity = format!("{}:{}:{:?}", hex::encode(hash.finalize()), opened.len(), opened.modified()?);
            #[cfg(unix)] let identity = { use std::os::unix::fs::MetadataExt; format!("{identity}:{}:{}", opened.dev(), opened.ino()) };
            inputs.push((path,identity));
        }
        if cancelled(){bail!("Runtime fingerprint cancelled");}Ok(RuntimeInputs(inputs))
    }).await.map_err(|e|HostAdmissionError(anyhow!(e)))?.map_err(|e|HostAdmissionError(e).into())
}
fn same(a:&std::fs::Metadata,b:&std::fs::Metadata)->Result<bool> {
    let result=a.is_file()&&b.is_file()&&a.len()==b.len()&&a.modified()?==b.modified()?;
    #[cfg(unix)] let result={use std::os::unix::fs::MetadataExt;result&&a.dev()==b.dev()&&a.ino()==b.ino()};Ok(result)
}
#[cfg(test)]
#[path="runtime_cache_tests.rs"]
mod tests;
