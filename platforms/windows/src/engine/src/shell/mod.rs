// Windows shell + system integrations.
//
// Mirror of macOS engine/Sources/FileIDEngine/Shell/. Each submodule is a
// thin RAII wrapper over a Win32 / WinRT API — they're deliberately small
// so the per-call surface stays comparable to the macOS counterparts.
//
//   reveal     → SHOpenFolderAndSelectItems     (NSWorkspace.activateFileViewerSelecting)
//   trash      → IFileOperation::DeleteItem      (FileManager.trashItem, 8-parallel)
//   thumbnail  → IThumbnailProvider              (QLThumbnailGenerator)
//   ocr        → Windows.Media.Ocr (WinRT)       (VNRecognizeText)
//   tags       → IPropertyStore System.Keywords  (URLResourceKey.tagNamesKey)
//   video      → Media Foundation IMFSourceReader (AVAssetImageGenerator)
//   sleep      → SetThreadExecutionState         (IOPMAssertion)
//
// All public surface of these submodules sits behind plain Rust types so
// the rest of the engine doesn't take a windows-rs dep transitively.

pub mod reveal;
pub mod sleep;
pub mod tags;
pub mod thumbnail;
pub mod trash;
pub mod ocr;
pub mod video;
