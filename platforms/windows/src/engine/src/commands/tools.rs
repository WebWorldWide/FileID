use crate::ipc::{CatalogChapter, EventPayload, IpcEvent, ToolCapability, ToolOutput, ToolRecipe, ToolRequest, ToolResponse, Wrap};
use crate::ipc::sink::Sink;
use crate::util::{path_safety::stable_path_hash, read_only};
use anyhow::{bail, Context, Result};
use image::{DynamicImage, ImageDecoder, ImageFormat, ImageReader, Limits};
use parking_lot::Mutex;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{collections::HashSet, fs::{self, File, OpenOptions}, io::{Read, Write, Seek, SeekFrom}, path::Path, sync::Arc};

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all="camelCase")]
struct Item { output: ToolOutput, source_hash: String, chapters: Vec<CatalogChapter> }
#[derive(Serialize, Deserialize)]
struct Plan {
    version: u32, r#type: String, recipe: ToolRecipe, items: Vec<Item>,
    #[serde(rename="stagePaths", default, skip_serializing_if="Option::is_none")]
    stage_paths: Option<Vec<String>>,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all="camelCase")]
struct Receipt {
    output: ToolOutput,
    hash: String,
    #[serde(rename="derivedID", default, skip_serializing_if="Option::is_none")]
    derived_id: Option<i64>,
    #[serde(default, skip_serializing_if="Option::is_none")]
    recovery_path: Option<String>,
}

pub async fn handle(sink: Sink, database: Option<Arc<Mutex<Connection>>>, request: ToolRequest) {
    let id = request.request_id.clone();
    let result = tokio::task::spawn_blocking(move || {
        let db = database.context("The catalog database is unavailable")?;
        let mut conn = db.lock();
        execute(&mut conn, &request)
    }).await;
    let response = match result {
        Ok(Ok(response)) => response,
        Ok(Err(error)) => response(&id,"error", &error.to_string()),
        Err(error) => response(&id,"error", &error.to_string()),
    };
    sink.send(IpcEvent::now(EventPayload::ToolResponse(Wrap::new(response)))).await;
}
fn response(id: &str, status: &str, message: &str) -> ToolResponse {
    ToolResponse {request_id:id.into(),status:status.into(),message:message.into(),operation_id:None,outputs:vec![],capabilities:vec![]}
}
fn capabilities() -> Vec<ToolCapability> {
    vec![ToolCapability{id:"photo".into(),available:true,input_formats:vec!["png".into(),"jpeg".into()],output_formats:vec!["png".into(),"jpeg".into(),"tiff".into()],detail:"Single-image conversion and bounded downsize. EXIF orientation is applied. Camera/location metadata is stripped; inputs with embedded ICC profiles are rejected until color-managed conversion is available. Output is 8-bit SDR. JPEG transparency is flattened onto white. HEIC/TIFF inputs are not yet supported on this adapter.".into()},
    ToolCapability{id:"chapters".into(),available:true,input_formats:vec!["catalog chapters".into()],output_formats:vec!["json".into(),"vtt".into()],detail:"Export current chapter markers. WebVTT is a chapter cue list, not speech subtitles.".into()},
    ToolCapability{id:"video".into(),available:false,input_formats:vec![],output_formats:vec![],detail:"Native video export is currently macOS-only; the portable worker is not available.".into()},
    ToolCapability{id:"videoEnhancement".into(),available:false,input_formats:vec![],output_formats:vec![],detail:"Stabilization, AI upscaling, and tracked reframing are not installed yet.".into()}]
}
fn supports(recipe: &ToolRecipe) -> bool {
    (1..=8192).contains(&recipe.max_dimension) &&
    ((recipe.kind=="photo" && ["png","jpeg","tiff"].contains(&recipe.format.as_str())) ||
     (recipe.kind=="chapters" && ["json","vtt"].contains(&recipe.format.as_str())))
}
fn hash(path: &Path) -> Result<String> {
    let metadata=fs::symlink_metadata(path)?;
    if !metadata.is_file() || metadata.file_type().is_symlink() { bail!("Choose a regular file, not a link or folder") }
    let mut file=File::open(path)?; let mut hash=Sha256::new(); let mut bytes=[0u8;1024*1024];
    loop { let n=file.read(&mut bytes)?; if n==0 {break} hash.update(&bytes[..n]); }
    Ok(format!("{:x}",hash.finalize()))
}
fn persist(conn:&Connection,id:&str,receipts:&[Receipt],state:&str)->Result<()> {
    conn.execute("UPDATE catalog_operations SET inverse_json=?1,state=?2 WHERE id=?3",params![serde_json::to_string(receipts)?,state,id])?; Ok(())
}
fn load(conn:&Connection,id:&str)->Result<(Plan,Vec<Receipt>,String)> {
    let (plan,inverse,state):(String,String,String)=conn.query_row("SELECT plan_json,inverse_json,state FROM catalog_operations WHERE id=?1",[id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?;
    let plan:Plan=serde_json::from_str(&plan)?;
    if plan.r#type!="export" || plan.version!=1 {bail!("The export plan is unavailable")}
    Ok((plan,serde_json::from_str(&inverse)?,state))
}

pub fn execute(conn:&mut Connection,request:&ToolRequest)->Result<ToolResponse> {
    if request.request_id.is_empty() || request.request_id.chars().count()>200 {bail!("Invalid tools request")}
    let mut result=response(&request.request_id,"ok","");
    match request.action.as_str() {
        "capabilities"=>result.capabilities=capabilities(),
        "history"=> {
            let id:Option<String>=conn.query_row("SELECT id FROM catalog_operations WHERE json_extract(plan_json,'$.type')='export' AND state IN ('completed','failed') ORDER BY rowid DESC LIMIT 1",[],|r|r.get(0)).optional()?;
            if let Some(id)=id {let (_,receipts,_)=load(conn,&id)?;result.operation_id=Some(id);result.outputs=receipts.into_iter().map(|r|r.output).collect();result.message="Last export operation.".into();}
            else {result.message="No completed export history.".into();}
        }
        "preview"=> {
            let ids=request.file_ids.as_ref().context("Choose files")?;
            let recipe=request.recipe.as_ref().context("Choose a recipe")?;
            if ids.is_empty() || ids.len()>100 || ids.iter().collect::<HashSet<_>>().len()!=ids.len() || !supports(recipe) {bail!("Unsupported export recipe")}
            let destination=Path::new(request.destination.as_deref().context("Choose an output folder")?);
            read_only::require_source_mutation(destination)?;
            if !destination.is_absolute() || !destination.is_dir() {bail!("The output folder must already exist")}
            let mut items=vec![]; let mut reserved=HashSet::new();
            for &file_id in ids {
                let source:String=conn.query_row("SELECT path_text FROM files WHERE id=?1",[file_id],|r|r.get(0))?;
                let source_path=Path::new(&source);
                let chapters=super::catalog::chapters(conn,file_id)?.into_iter().filter(|c|!c.stale).collect::<Vec<_>>();
                if recipe.kind=="chapters" && chapters.is_empty(){bail!("A selected file has no current chapter markers")}
                if recipe.kind=="photo" {validate_image(source_path)?;}
                let source_hash=hash(source_path)?;
                let stem=source_path.file_stem().and_then(|s|s.to_str()).context("Invalid source name")?.chars().take(50).collect::<String>();
                let label=if recipe.kind=="chapters" {"Chapters"} else {"Export"};
                let ext=if recipe.format=="jpeg" {"jpg"} else {&recipe.format};
                let mut index=1;
                let output=loop {
                    let suffix=if index==1 {String::new()} else {format!(" ({index})")};
                    let output=destination.join(format!("{stem} - {label}{suffix}.{ext}"));
                    read_only::require_source_mutation(&output)?;
                    index+=1;
                    let key=output.file_name().context("Invalid output name")?.to_string_lossy().to_lowercase();
                    if !output.exists() && reserved.insert(key) {break output;}
                };
                items.push(Item{output:ToolOutput{file_id,source_path:source,output_path:output.to_string_lossy().into_owned(),state:"pending".into(),message:String::new()},source_hash,chapters});
            }
            let plan=Plan{version:1,r#type:"export".into(),recipe:recipe.clone(),items,stage_paths:None};
            let id=uuid::Uuid::new_v4().to_string();
            conn.execute("INSERT INTO catalog_operations(id,plan_json,inverse_json,state,created_at) VALUES(?1,?2,'[]','preview',?3)",params![id,serde_json::to_string(&plan)?,chrono::Utc::now().timestamp_millis() as f64 / 1000.0])?;
            result.outputs=plan.items.into_iter().map(|i|i.output).collect(); result.operation_id=Some(id);
            result.message="Creates new files. Originals are preserved; photo outputs strip private metadata and normalize to 8-bit SDR.".into();
        }
        "execute"=> {
            let id=request.operation_id.as_deref().context("Preview an export first")?;
            let (mut plan,previous,state)=load(conn,id)?;
            if !supports(&plan.recipe) {bail!("This export recipe is unavailable on this platform")}
            if state!="preview" || !previous.is_empty() {bail!("This plan was already executed. Preview a fresh plan")}
            for item in &plan.items {
                if plan.recipe.kind=="chapters" {
                    let current=super::catalog::chapters(conn,item.output.file_id)?.into_iter().filter(|c|!c.stale).collect::<Vec<_>>();
                    if serde_json::to_value(&current)? != serde_json::to_value(&item.chapters)? {bail!("Chapter markers changed after preview. Preview again")}
                }
                let output=Path::new(&item.output.output_path); read_only::require_source_mutation(output)?;
                if hash(Path::new(&item.output.source_path))?!=item.source_hash {bail!("A source changed after preview. Preview again")}
                if output.exists() {bail!("An output name is occupied. Preview again")}
            }
            conn.execute("UPDATE catalog_operations SET state='running' WHERE id=?1",[id])?;
            let mut receipts=vec![];
            let run=(||->Result<()> {
                for item in plan.items.clone() {
                    let output=Path::new(&item.output.output_path); let source=Path::new(&item.output.source_path);
                    let stage=output.parent().context("Invalid output folder")?.join(format!(".FileIDExport-{}.part",uuid::Uuid::new_v4()));
                    read_only::require_writable(&stage)?;
                    plan.stage_paths.get_or_insert_with(Vec::new).push(stage.to_string_lossy().into_owned());
                    conn.execute("UPDATE catalog_operations SET plan_json=?1 WHERE id=?2",params![serde_json::to_string(&plan)?,id])?;
                    let mut file=OpenOptions::new().write(true).create_new(true).open(&stage)?;
                    let prepared=(||->Result<()> {
                        if plan.recipe.kind=="photo" {export_photo(source,&mut file,&plan.recipe)?;}
                        else {file.write_all(&export_chapters(&item.chapters,&plan.recipe.format)?)?;}
                        file.sync_all()?; drop(file);
                        if plan.recipe.kind=="photo" {
                            let decoder=ImageReader::open(&stage)?.with_guessed_format()?.into_decoder()?;
                            let decoded=DynamicImage::from_decoder(decoder)?;
                            if decoded.width()==0 || decoded.height()==0 || decoded.width().max(decoded.height())>plan.recipe.max_dimension {bail!("Output validation failed")}
                        }
                        if hash(source)?!=item.source_hash {bail!("A source changed during export; its output was not published")}
                        let digest=hash(&stage)?;
                        let mut receipt=Receipt{output:item.output.clone(),hash:digest,derived_id:None,recovery_path:None};
                        receipt.output.message="Prepared output; review this path after interrupted publication.".into();
                        receipts.push(receipt); persist(conn,id,&receipts,"running")?;
                        read_only::require_writable(output)?; fs::hard_link(&stage,output)?;
                        let last=receipts.last_mut().context("Missing export receipt")?;
                        last.output.state="completed".into(); last.output.message="Original preserved.".into();
                        let metadata=output.metadata()?;
                        let modified=metadata.modified()?.duration_since(std::time::UNIX_EPOCH)?.as_secs_f64();
                        let kind=if plan.recipe.kind=="photo" {"image"} else {"doc"};
                        let tx=conn.transaction()?;
                        tx.execute("INSERT INTO files(path_text,path_hash,size_bytes,scanned_at,modified_at,kind,extension) VALUES(?1,?2,?3,0,?4,?5,?6)",params![item.output.output_path,stable_path_hash(&item.output.output_path),metadata.len(),modified,kind,output.extension().and_then(|s|s.to_str()).unwrap_or_default()])?;
                        let derived=tx.last_insert_rowid();
                        tx.execute("INSERT INTO catalog_assets(original_id,derived_id,role,recipe_json) VALUES(?1,?2,'export',?3)",params![item.output.file_id,derived,serde_json::to_string(&plan.recipe)?])?;
                        last.derived_id=Some(derived); persist(&tx,id,&receipts,"running")?; tx.commit()?;
                        Ok(())
                    })();
                    let _=fs::remove_file(&stage); prepared?;
                }
                Ok(())
            })();
            match run { Ok(())=>{persist(conn,id,&receipts,"completed")?;result.message="Export complete. Undo moves unchanged exports into internal recovery storage.".into();}, Err(error)=>{persist(conn,id,&receipts,"failed")?;result.status="error".into();result.message=error.to_string();} }
            result.operation_id=Some(id.into()); result.outputs=receipts.into_iter().map(|r|r.output).collect();
        }
        "cancel"=>bail!("Export cancellation is not yet available in this adapter; it does not isolate image decoders in workers"),
        "undo"=> {
            let id=request.operation_id.as_deref().context("Choose an export operation")?;
            let (_,mut receipts,state)=load(conn,id)?;
            for receipt in &receipts {
                if receipt.output.state!="completed" {continue}
                let source=Path::new(&receipt.output.output_path);read_only::require_source_mutation(source)?;
                if !source.exists() && receipt.recovery_path.as_deref().is_some_and(|p|Path::new(p).is_file()) {continue}
                if hash(source)?!=receipt.hash {bail!("An export was edited. Undo leaves all remaining exports untouched")}
            }
            if !["completed","failed"].contains(&state.as_str()) {bail!("This operation cannot be undone")}
            let db_path:String=conn.query_row("SELECT file FROM pragma_database_list WHERE name='main'",[],|r|r.get(0))?;
            let directory=Path::new(&db_path).parent().context("Catalog has no recovery folder")?.join("ExportRecovery").join(id);
            read_only::require_writable(&directory)?; fs::create_dir_all(&directory)?;
            for index in 0..receipts.len() {
                if receipts[index].output.state!="completed" {continue}
                let source=std::path::PathBuf::from(&receipts[index].output.output_path);
                read_only::require_source_mutation(&source)?;
                if let Some(path)=receipts[index].recovery_path.clone() {
                    let recovery=Path::new(&path);read_only::require_writable(recovery)?;
                    if !source.exists() && recovery.is_file() && hash(recovery)?==receipts[index].hash {
                        if let Some(derived)=receipts[index].derived_id {conn.execute("DELETE FROM files WHERE id=?1",[derived])?;}
                        receipts[index].output.state="undone".into(); receipts[index].output.message=format!("Recoverable at {path}");persist(conn,id,&receipts,&state)?;continue;
                    }
                }
                if hash(&source)?!=receipts[index].hash {bail!("An export was edited. Undo leaves it untouched")}
                let target=directory.join(format!("{index}-{}",source.file_name().context("Invalid export path")?.to_string_lossy()));
                read_only::require_writable(&target)?;
                if target.exists() {bail!("Recovery path is occupied")}
                receipts[index].recovery_path=Some(target.to_string_lossy().into_owned()); persist(conn,id,&receipts,&state)?;
                fs::rename(&source,&target)?;
                if let Some(derived)=receipts[index].derived_id {conn.execute("DELETE FROM files WHERE id=?1",[derived])?;}
                receipts[index].output.state="undone".into(); receipts[index].output.message=format!("Recoverable at {}",target.display()); persist(conn,id,&receipts,&state)?;
            }
            persist(conn,id,&receipts,"undone")?;
            result.operation_id=Some(id.into()); result.outputs=receipts.into_iter().map(|r|r.output).collect();result.message="Exports moved to internal recovery storage. Originals are unchanged.".into();
        }
        _=>bail!("Unsupported tools action"),
    }
    Ok(result)
}

fn validate_image(path:&Path)->Result<()> {
    let format=ImageReader::open(path)?.with_guessed_format()?.format().context("Unknown image format")?;
    if ![ImageFormat::Png,ImageFormat::Jpeg].contains(&format) {bail!("Only single-image PNG and JPEG inputs are supported by this adapter")}
    if format==ImageFormat::Png {
        let mut file=File::open(path)?; let size=file.metadata()?.len(); let mut offset=8u64;
        while offset+12<=size {
            file.seek(SeekFrom::Start(offset))?;
            let mut header=[0u8;8];file.read_exact(&mut header)?;
            let length=u64::from(u32::from_be_bytes(header[..4].try_into()?));
            if &header[4..]==b"acTL" {bail!("Animated PNG cannot be flattened by this tool")}
            let next=offset.checked_add(length).and_then(|n|n.checked_add(12)).context("Malformed PNG")?;
            if next>size {bail!("Malformed PNG")}
            offset=next;
        }
    }
    Ok(())
}
fn export_photo(source:&Path,file:&mut File,recipe:&ToolRecipe)->Result<()> {
    validate_image(source)?;
    let mut reader=ImageReader::open(source)?.with_guessed_format()?;
    let mut limits=Limits::default();limits.max_alloc=Some(512*1024*1024);limits.max_image_width=Some(65536);limits.max_image_height=Some(65536);reader.limits(limits);
    let mut decoder=reader.into_decoder()?;
    let (width,height)=decoder.dimensions();
    if u64::from(width)*u64::from(height)>32_000_000 {bail!("This adapter currently limits input images to 32 megapixels")}
    if decoder.icc_profile()?.is_some() {bail!("This adapter requires untagged sRGB input; use color-managed conversion for embedded ICC profiles")}
    let orientation=decoder.orientation()?;
    let mut image=DynamicImage::from_decoder(decoder)?;image.apply_orientation(orientation);
    if image.width().max(image.height())>recipe.max_dimension {image=image.resize(recipe.max_dimension,recipe.max_dimension,image::imageops::FilterType::Lanczos3);}
    let rgba=image.to_rgba8();
    image=if recipe.format=="jpeg" {
        let mut rgb=image::RgbImage::new(rgba.width(),rgba.height());
        for (x,y,pixel) in rgba.enumerate_pixels() {
            let a=u16::from(pixel[3]);let blend=|c:u8|->u8 {((u16::from(c)*a+255*(255-a)+127)/255) as u8};
            rgb.put_pixel(x,y,image::Rgb([blend(pixel[0]),blend(pixel[1]),blend(pixel[2])]));
        }
        DynamicImage::ImageRgb8(rgb)
    } else {DynamicImage::ImageRgba8(rgba)};
    let format=match recipe.format.as_str() {"png"=>ImageFormat::Png,"jpeg"=>ImageFormat::Jpeg,"tiff"=>ImageFormat::Tiff,_=>bail!("Unsupported output")};
    image.write_to(file,format)?; Ok(())
}
fn export_chapters(chapters:&[CatalogChapter],format:&str)->Result<Vec<u8>> {
    if format=="json" {return Ok(serde_json::to_vec_pretty(chapters)?)}
    let stamp=|seconds:f64| {let ms=(seconds.min(359_999_999.0)*1000.0).round() as u64;format!("{:02}:{:02}:{:02}.{:03}",ms/3_600_000,ms/60_000%60,ms/1000%60,ms%1000)};
    let mut text=String::from("WEBVTT\n\n");
    for (i,c) in chapters.iter().enumerate() {let title=c.title.replace("-->","→").replace('&',"&amp;").replace('<',"&lt;").replace('>',"&gt;").replace(['\n','\r']," ");use std::fmt::Write as _;
        write!(&mut text,"{}\n{} --> {}\n{}\n\n",i+1,stamp(c.start_seconds),stamp(c.end_seconds.max(c.start_seconds+0.001)),title)?;}
    Ok(text.into_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Fixture {root: std::path::PathBuf, source: std::path::PathBuf, conn: Connection}
    impl Fixture {
        fn new()->Self {
            let root=std::env::temp_dir().join(format!("FileIDToolsTest-{}",uuid::Uuid::new_v4()));fs::create_dir_all(&root).unwrap();
            let source=root.join("Portrait.png");
            image::RgbaImage::from_pixel(32,16,image::Rgba([255,0,0,0])).save(&source).unwrap();
            let conn=crate::db::open_writer(&root.join("catalog.sqlite")).unwrap();
            conn.execute("INSERT INTO files(id,path_text,path_hash,size_bytes,scanned_at,kind,extension) VALUES(1,?1,1,100,0,'image','png')",[source.to_str().unwrap()]).unwrap();
            Self{root,source,conn}
        }
        fn request(&self,format:&str)->ToolRequest {ToolRequest{request_id:"p".into(),action:"preview".into(),file_ids:Some(vec![1]),destination:Some(self.root.to_string_lossy().into_owned()),recipe:Some(ToolRecipe{kind:"photo".into(),format:format.into(),max_dimension:16}),operation_id:None}}
    }
    impl Drop for Fixture {fn drop(&mut self){let _=fs::remove_dir_all(&self.root);}}
    fn action(id:Option<String>,action:&str)->ToolRequest {ToolRequest{request_id:action.into(),action:action.into(),operation_id:id,file_ids:None,destination:None,recipe:None}}
    #[test]
    fn mac_video_plan_cannot_execute_as_chapter_text_on_portable_adapter() {
        let mut fixture=Fixture::new();
        let request=fixture.request("png");
        let preview=execute(&mut fixture.conn,&request).unwrap();
        let id=preview.operation_id.clone().unwrap();
        fixture.conn.execute("UPDATE catalog_operations SET plan_json=json_set(plan_json,'$.recipe.kind','video','$.recipe.format','mp4','$.recipe.maxDimension',1280) WHERE id=?1",[&id]).unwrap();
        let error=execute(&mut fixture.conn,&action(Some(id.clone()),"execute")).unwrap_err();
        assert!(error.to_string().contains("unavailable on this platform"));
        assert_eq!(fixture.conn.query_row("SELECT state FROM catalog_operations WHERE id=?1",[&id],|r|r.get::<_,String>(0)).unwrap(),"preview");
        assert!(!Path::new(&preview.outputs[0].output_path).exists());
        assert_eq!(fixture.conn.query_row("SELECT COUNT(*) FROM catalog_assets",[],|r|r.get::<_,i64>(0)).unwrap(),0);
    }
    #[test]
    fn conversion_validates_outputs_links_derivatives_and_undo_is_recoverable() {
        let mut fixture=Fixture::new();let request=fixture.request("jpeg");let original=hash(&fixture.source).unwrap();
        let preview=execute(&mut fixture.conn,&request).unwrap();
        let exported=execute(&mut fixture.conn,&action(preview.operation_id.clone(),"execute")).unwrap();assert_eq!(exported.status,"ok");
        let output=std::path::PathBuf::from(&exported.outputs[0].output_path);let image=image::open(&output).unwrap().to_rgb8();assert_eq!(image.dimensions(),(16,8));assert_eq!(*image.get_pixel(0,0),image::Rgb([255,255,255]));
        assert_eq!(hash(&fixture.source).unwrap(),original);
        assert_eq!(fixture.conn.query_row("SELECT COUNT(*) FROM catalog_assets",[],|r|r.get::<_,i64>(0)).unwrap(),1);
        let history=execute(&mut fixture.conn,&action(None,"history")).unwrap();assert_eq!(history.operation_id,preview.operation_id);
        let undone=execute(&mut fixture.conn,&action(preview.operation_id,"undo")).unwrap();assert_eq!(undone.outputs[0].state,"undone");assert!(!output.exists());assert_eq!(hash(&fixture.source).unwrap(),original);
        assert_eq!(fixture.conn.query_row("SELECT COUNT(*) FROM catalog_assets",[],|r|r.get::<_,i64>(0)).unwrap(),0);
    }
    #[test]
    fn execution_rejects_changed_sources_and_occupied_outputs() {
        let mut fixture=Fixture::new();let request=fixture.request("png");let preview=execute(&mut fixture.conn,&request).unwrap();let output=Path::new(&preview.outputs[0].output_path);
        fs::write(output,b"Existing").unwrap();assert!(execute(&mut fixture.conn,&action(preview.operation_id.clone(),"execute")).is_err());assert_eq!(fs::read(output).unwrap(),b"Existing");
        fs::remove_file(output).unwrap();fs::write(&fixture.source,b"Changed").unwrap();assert!(execute(&mut fixture.conn,&action(preview.operation_id,"execute")).is_err());assert!(!output.exists());
    }
    #[test]
    fn undo_keeps_edited_exports_and_rejects_protected_destinations() {
        let mut fixture=Fixture::new();let request=fixture.request("png");let preview=execute(&mut fixture.conn,&request).unwrap();let exported=execute(&mut fixture.conn,&action(preview.operation_id.clone(),"execute")).unwrap();let output=Path::new(&exported.outputs[0].output_path);
        fs::write(output,b"Edited").unwrap();assert!(execute(&mut fixture.conn,&action(preview.operation_id,"undo")).is_err());assert_eq!(fs::read(output).unwrap(),b"Edited");
        let mut protected=request;protected.destination=Some("/Volumes/Adlon".into());assert!(execute(&mut fixture.conn,&protected).is_err());
    }
    #[test]
    fn restart_cleanup_preserves_unrelated_files_and_sources() {
        let fixture=Fixture::new();let stage=fixture.root.join(format!(".FileIDExport-{}.part",uuid::Uuid::new_v4()));let unrelated=fixture.root.join(".FileIDExport-user.part");
        fs::write(&stage,b"Partial output").unwrap();fs::write(&unrelated,b"Unrelated").unwrap();let original=hash(&fixture.source).unwrap();
        let item=Item{output:ToolOutput{file_id:1,source_path:fixture.source.to_string_lossy().into_owned(),output_path:fixture.root.join("Export.png").to_string_lossy().into_owned(),state:"pending".into(),message:String::new()},source_hash:original.clone(),chapters:vec![]};
        let plan=Plan{version:1,r#type:"export".into(),recipe:ToolRecipe{kind:"photo".into(),format:"png".into(),max_dimension:16},items:vec![item],stage_paths:Some(vec![stage.to_string_lossy().into_owned(),unrelated.to_string_lossy().into_owned()])};
        fixture.conn.execute("INSERT INTO catalog_operations(id,plan_json,inverse_json,state,created_at) VALUES('interrupted',?1,'[]','running',0)",[serde_json::to_string(&plan).unwrap()]).unwrap();
        recover(&fixture.conn).unwrap();let deadline=std::time::Instant::now()+std::time::Duration::from_secs(6);
        while stage.exists() && std::time::Instant::now()<deadline {std::thread::sleep(std::time::Duration::from_millis(50));}
        assert!(!stage.exists());assert!(unrelated.exists());assert_eq!(hash(&fixture.source).unwrap(),original);
    }

    #[test]
    fn chapter_export_escapes_cues_and_uses_nonzero_intervals() {
        let c=CatalogChapter{id:"gift".into(),file_id:1,start_seconds:1.234,end_seconds:1.234,title:"Gift <opening> -->\nGrandma".into(),summary:String::new(),source_revision:"fixture".into(),model_version:"user".into(),confidence:1.0,user_edited:true,stale:false};
        let text=String::from_utf8(export_chapters(std::slice::from_ref(&c),"vtt").unwrap()).unwrap();assert!(text.contains("00:00:01.234 --> 00:00:01.235"));assert!(text.contains("Gift &lt;opening&gt; → Grandma"));
        let decoded:Vec<CatalogChapter>=serde_json::from_slice(&export_chapters(&[c],"json").unwrap()).unwrap();assert_eq!(decoded[0].source_revision,"fixture");
    }
}


pub(crate) fn recover(conn:&Connection)->Result<()> {
    conn.execute("UPDATE catalog_operations SET state='failed' WHERE state='running' AND json_extract(plan_json,'$.type')='export'",[])?;
    let mut statement=conn.prepare("SELECT plan_json FROM catalog_operations WHERE state='failed' AND json_extract(plan_json,'$.type')='export'")?;
    let plans=statement.query_map([],|r|r.get::<_,String>(0))?.collect::<rusqlite::Result<Vec<_>>>()?.into_iter().filter_map(|j|serde_json::from_str::<Plan>(&j).ok()).collect::<Vec<_>>();
    if plans.is_empty() {return Ok(())}
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_secs(2));
        for plan in plans {
            for path in plan.stage_paths.unwrap_or_default() {
                let stage=Path::new(&path);
                let name=stage.file_stem().and_then(|s|s.to_str()).unwrap_or_default();
                if stage.extension().and_then(|s|s.to_str())!=Some("part") || name.strip_prefix(".FileIDExport-").is_none_or(|s|uuid::Uuid::parse_str(s).is_err()) {continue}
                if !plan.items.iter().any(|i|Path::new(&i.output.output_path).parent()==stage.parent()) || read_only::require_writable(stage).is_err() {continue}
                if fs::symlink_metadata(stage).is_ok_and(|m|m.is_file() && !m.file_type().is_symlink()) {let _=fs::remove_file(stage);}
            }
        }
    });
    Ok(())
}
