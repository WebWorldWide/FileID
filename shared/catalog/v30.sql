CREATE INDEX catalog_files_dir ON files(rtrim(replace(path_text,char(92),'/'),replace(replace(path_text,char(92),'/'),'/','')),failed,size_bytes);
