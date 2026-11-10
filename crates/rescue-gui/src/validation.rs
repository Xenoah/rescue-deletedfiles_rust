//! Fast form checks run while editing; filesystem checks only run on submission.
use crate::{App, SaveMode, SourceMode, Text};
use std::path::Path;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Field {
    Source,
    Destination,
    Work,
    OriginalRoot,
    Extensions,
    Folder,
    Depth,
    RangeStart,
    RangeEnd,
}
impl Field {
    pub fn advanced(self) -> bool {
        !matches!(self, Self::Source | Self::Destination)
    }
}
#[derive(Clone, Debug)]
pub struct Issue {
    pub field: Field,
    pub message: Text,
}
#[derive(Clone)]
pub struct PathIssue {
    issue: Issue,
    value: String,
    source: String,
    mode: SaveMode,
}
impl App {
    fn field_value(&self, field: Field) -> &str {
        match field {
            Field::Source => &self.source,
            Field::Destination => &self.destination,
            Field::Work => &self.work,
            Field::OriginalRoot => &self.original_root,
            Field::Extensions => &self.extensions,
            Field::Folder => &self.path_prefix,
            Field::Depth => &self.depth,
            Field::RangeStart => &self.range_start,
            Field::RangeEnd => &self.range_end,
        }
    }

    pub fn configuration_issues(&self) -> Vec<Issue> {
        let mut issues = Vec::new();
        let mut add = |field, message| issues.push(Issue { field, message });
        if self.source.trim().is_empty() {
            add(
                Field::Source,
                message!(
                    "スキャンするディスクまたはイメージを指定してください。",
                    "Choose a disk or image to scan."
                ),
            );
        }
        if self.save_mode != SaveMode::Original && self.destination.trim().is_empty() {
            add(
                Field::Destination,
                message!(
                    "復元したファイルの保存先を指定してください。",
                    "Choose where to save the recovered files."
                ),
            );
        }
        let extensions = self
            .extensions
            .split([',', ';', ' ', '\n'])
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .collect();
        if (rescue_core::SearchSettings {
            extensions,
            ..Default::default()
        })
        .validate()
        .is_err()
        {
            add(
                Field::Extensions,
                message!(
                    "拡張子は32バイト以内・64個までで指定してください。パスは入力できません。",
                    "Enter up to 64 extensions, at most 32 bytes each, without paths."
                ),
            );
        }
        let number = |text: &str| text.trim().parse::<u64>();
        if self.carve {
            for (field, value) in [
                (Field::RangeStart, &self.range_start),
                (Field::RangeEnd, &self.range_end),
            ] {
                if !value.trim().is_empty() && number(value).is_err() {
                    add(
                        field,
                        message!(
                            "0以上の整数を入力してください（最大18446744073709551615）。",
                            "Enter a nonnegative whole number (maximum 18446744073709551615)."
                        ),
                    );
                }
            }
            if let (Ok(start), Ok(end)) = (
                number(if self.range_start.trim().is_empty() {
                    "0"
                } else {
                    &self.range_start
                }),
                number(&self.range_end),
            ) && end < start
            {
                add(
                    Field::RangeEnd,
                    message!(
                        "終了バイトは開始バイト以上にしてください。",
                        "The end byte must be at or after the start byte."
                    ),
                );
            }
        } else {
            if !self.depth.trim().is_empty() && number(&self.depth).map_or(true, |d| d > 256) {
                add(
                    Field::Depth,
                    message!(
                        "検索深度は0～256の整数で指定してください。空欄は上限なしです。",
                        "Enter a depth from 0 to 256, or leave it blank for no limit."
                    ),
                );
            }
            if self.path_prefix.len() > 4096 {
                add(
                    Field::Folder,
                    message!(
                        "検索フォルダーは4096バイト以内にしてください。",
                        "Keep the search folder within 4096 bytes."
                    ),
                );
            }
        }
        for error in &self.path_issues {
            if error.value == self.field_value(error.issue.field)
                && error.source == self.source
                && error.mode == self.save_mode
            {
                issues.push(error.issue.clone());
            }
        }
        issues
    }

    pub fn field_error(&self, field: Field) -> Option<String> {
        if !self.show_validation && self.field_value(field).trim().is_empty() {
            return None;
        }
        self.configuration_issues()
            .into_iter()
            .find(|i| i.field == field)
            .map(|i| i.message.to_string())
    }

    pub fn remember_path_issue(&mut self, field: Field, message: Text) {
        self.path_issues.push(PathIssue {
            issue: Issue { field, message },
            value: self.field_value(field).into(),
            source: self.source.clone(),
            mode: self.save_mode,
        });
    }

    pub fn validate_configuration(&mut self) -> bool {
        self.show_validation = true;
        self.status_error = false;
        self.path_issues.clear();
        if !self.configuration_issues().is_empty() {
            return false;
        }
        // Raw Windows devices do not support ordinary file metadata. Let the
        // engine perform its native read-only device and permission checks.
        if !rescue_core::is_device(Path::new(&self.source)) {
            match std::fs::metadata(&self.source) {
                Ok(meta)
                    if meta.is_dir()
                        || self.source_mode == SourceMode::Image && !meta.is_file() =>
                {
                    self.remember_path_issue(
                        Field::Source,
                        message!(
                            "読み取るイメージファイルを指定してください。フォルダーは使用できません。",
                            "Choose an image file, not a folder."
                        ),
                    );
                }
                Err(error) => {
                    self.remember_path_issue(Field::Source, message!("入力を確認できません。パス・接続・アクセス権を確認してください: {error}", "Cannot access the source. Check its path, connection and permissions: {error}"));
                }
                _ => {}
            }
        }
        for (field, directory) in [
            (Field::Destination, self.save_mode != SaveMode::Zip),
            (Field::Work, true),
            (Field::OriginalRoot, true),
        ] {
            if field == Field::Destination && self.save_mode == SaveMode::Original
                || field == Field::OriginalRoot && self.save_mode != SaveMode::Original
                || self.field_value(field).trim().is_empty()
            {
                continue;
            }
            let path = Path::new(self.field_value(field));
            let wrong_kind = path.exists() && path.is_dir() != directory;
            let blocked_parent = path
                .ancestors()
                .skip(1)
                .find(|p| p.exists())
                .is_some_and(|p| !p.is_dir());
            if wrong_kind || blocked_parent {
                self.remember_path_issue(field, if directory {
                    message!("フォルダーのパスを指定してください。ファイルをフォルダーとして使用できません。", "Choose a folder path. A file cannot be used as a folder.")
                } else {
                    message!("ZIPファイルのパスを指定してください。フォルダーは指定できません。", "Choose a ZIP file path, not a folder.")
                });
            } else if field == Field::Destination && path == Path::new(&self.source) {
                self.remember_path_issue(
                    field,
                    message!(
                        "入力イメージとは異なる保存先を指定してください。",
                        "Choose a destination different from the source image."
                    ),
                );
            }
        }
        self.configuration_issues().is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn errors_follow_active_fields_and_clear_after_correction() {
        let mut app = App::blank();
        assert!(app.field_error(Field::Source).is_none());
        assert!(!app.validate_configuration());
        assert!(app.field_error(Field::Source).is_some());
        assert!(app.field_error(Field::Destination).is_some());
        assert!(!app.busy());
        app.source = "disk.raw".into();
        app.destination = "output".into();
        app.depth = "257".into();
        app.extensions = "folder/jpg".into();
        assert!(app.field_error(Field::Depth).is_some());
        assert!(app.field_error(Field::Extensions).is_some());
        app.depth = "2".into();
        app.extensions = "jpg, pdf".into();
        assert!(app.configuration_issues().is_empty());
        app.carve = true;
        app.depth = "bad but inactive".into();
        app.range_start = "200".into();
        app.range_end = "100".into();
        assert_eq!(app.configuration_issues()[0].field, Field::RangeEnd);
        app.range_end = "18446744073709551616".into();
        assert!(app.field_error(Field::RangeEnd).is_some());
        app.range_end = "300".into();
        assert!(app.configuration_issues().is_empty());
        app.save_mode = SaveMode::Original;
        app.destination.clear();
        assert!(app.configuration_issues().is_empty());
        app.remember_path_issue(Field::Work, "unsafe work path".into());
        assert!(app.field_error(Field::Work).is_some());
        app.work = "fixed".into();
        assert!(app.field_error(Field::Work).is_none());
    }
}

#[cfg(test)]
mod path_tests {
    use super::*;
    #[test]
    fn bad_source_and_output_paths_are_attached_to_their_fields_without_creating_outputs() {
        let root = std::env::temp_dir().join(format!("rescue-validation-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let source = root.join("source.raw");
        std::fs::write(&source, b"synthetic input").unwrap();
        let mut app = App::blank();
        app.source_mode = SourceMode::Image;
        app.source = root.to_string_lossy().into_owned();
        app.destination = root.join("output").to_string_lossy().into_owned();
        assert!(!app.validate_configuration());
        assert!(app.field_error(Field::Source).is_some());
        app.source = source.to_string_lossy().into_owned();
        assert!(app.validate_configuration());
        assert!(!root.join("output").exists());
        app.destination = source.to_string_lossy().into_owned();
        assert!(!app.validate_configuration());
        assert!(app.field_error(Field::Destination).is_some());
        app.save_mode = SaveMode::Zip;
        app.destination = root.to_string_lossy().into_owned();
        assert!(!app.validate_configuration());
        assert!(app.field_error(Field::Destination).is_some());
        app.destination = root.join("output.zip").to_string_lossy().into_owned();
        app.work = source.join("work").to_string_lossy().into_owned();
        assert!(!app.validate_configuration());
        assert!(app.field_error(Field::Work).is_some());
        app.work.clear();
        assert!(app.validate_configuration());
        assert!(!root.join("output.zip").exists());
        assert_eq!(std::fs::read(source).unwrap(), b"synthetic input");
        std::fs::remove_dir_all(root).unwrap();
    }
}
