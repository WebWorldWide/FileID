ALTER TABLE ocr_text ADD COLUMN source TEXT NOT NULL DEFAULT 'ocr' CHECK(source IN ('ocr','vlm'));
ALTER TABLE ocr_text ADD COLUMN handwritten INTEGER NOT NULL DEFAULT 0 CHECK(handwritten IN (0,1));
CREATE TRIGGER ocr_text_vlm_invalidate AFTER UPDATE OF size_bytes,modified_at ON files
 WHEN old.size_bytes != new.size_bytes OR old.modified_at IS NOT new.modified_at BEGIN
 DELETE FROM ocr_text WHERE file_id=new.id AND source='vlm';
END;
