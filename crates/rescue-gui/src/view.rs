use rescue_core::{Candidate, FileTimestamp};
use std::{
    cmp::Ordering,
    collections::{BTreeMap, BTreeSet},
};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ViewStyle {
    #[default]
    Details,
    Icons,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SortColumn {
    #[default]
    Name,
    Created,
    Modified,
    Type,
    Size,
    State,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum GroupBy {
    #[default]
    None,
    Folder,
    Type,
    Created,
    Modified,
    Size,
    Initial,
    State,
}
impl GroupBy {
    pub const ALL: [Self; 8] = [
        Self::None,
        Self::Folder,
        Self::Type,
        Self::Created,
        Self::Modified,
        Self::Size,
        Self::Initial,
        Self::State,
    ];
    pub fn label(self) -> &'static str {
        match self {
            Self::None => "なし",
            Self::Folder => "元のフォルダー",
            Self::Type => "種類",
            Self::Created => "作成日",
            Self::Modified => "更新日",
            Self::Size => "サイズ",
            Self::Initial => "名前の先頭",
            Self::State => "復元可否",
        }
    }
}
#[derive(Clone, Debug)]
pub enum Row {
    Group(String, usize),
    File(usize),
}

pub fn file_type(c: &Candidate) -> String {
    let name = c.name.split(':').next().unwrap_or(&c.name);
    name.rsplit_once('.')
        .filter(|(base, ext)| !base.is_empty() && !ext.is_empty())
        .map(|(_, ext)| format!("{} ファイル", ext.to_uppercase()))
        .unwrap_or_else(|| "ファイル".into())
}
pub fn state(c: &Candidate) -> &'static str {
    if !c.supported {
        "復元不可"
    } else if c.warnings.is_empty() {
        "復元候補"
    } else {
        "要確認"
    }
}
pub fn size_text(size: Option<u64>) -> String {
    match size {
        None => "—".into(),
        Some(n) if n < 1024 => format!("{n} B"),
        Some(n) if n < 1024 * 1024 => format!("{:.1} KB", n as f64 / 1024.0),
        Some(n) if n < 1024 * 1024 * 1024 => format!("{:.1} MB", n as f64 / (1024.0 * 1024.0)),
        Some(n) => format!("{:.2} GB", n as f64 / (1024.0 * 1024.0 * 1024.0)),
    }
}
pub fn date_text(date: &Option<FileTimestamp>) -> String {
    date.as_ref()
        .map(|v| v.value.replace('T', " "))
        .unwrap_or_else(|| "—".into())
}
pub fn date_hint(date: &Option<FileTimestamp>) -> &'static str {
    match date {
        Some(d) if d.unix_seconds.is_some() => "管理情報に記録された日時・タイムゾーン",
        Some(_) => "元の媒体のローカル時刻。タイムゾーンは不明です。",
        None => "日時の管理情報が残っていません。",
    }
}
fn ordered(value: Ordering, descending: bool) -> Ordering {
    if descending { value.reverse() } else { value }
}
fn compare_date(
    a: &Option<FileTimestamp>,
    b: &Option<FileTimestamp>,
    descending: bool,
) -> Ordering {
    // Keep known UTC instants, unknown-zone local times, and missing dates in separate
    // ranks. Comparing mixed representations lexically would be non-transitive.
    let rank = |v: &Option<FileTimestamp>| match v {
        Some(d) if d.unix_seconds.is_some() => 0,
        Some(_) => 1,
        None => 2,
    };
    rank(a).cmp(&rank(b)).then_with(|| match (a, b) {
        (Some(a), Some(b)) => ordered(
            match (a.unix_seconds, b.unix_seconds) {
                (Some(a), Some(b)) => a.cmp(&b),
                _ => a.value.cmp(&b.value),
            },
            descending,
        ),
        _ => Ordering::Equal,
    })
}
pub fn compare(a: &Candidate, b: &Candidate, column: SortColumn, descending: bool) -> Ordering {
    let value = match column {
        SortColumn::Created => compare_date(&a.created, &b.created, descending),
        SortColumn::Modified => compare_date(&a.modified, &b.modified, descending),
        SortColumn::Size => match (a.size, b.size) {
            (Some(a), Some(b)) => ordered(a.cmp(&b), descending),
            (None, Some(_)) => Ordering::Greater,
            (Some(_), None) => Ordering::Less,
            _ => Ordering::Equal,
        },
        SortColumn::Name => ordered(
            a.name.to_lowercase().cmp(&b.name.to_lowercase()),
            descending,
        ),
        SortColumn::Type => ordered(file_type(a).cmp(&file_type(b)), descending),
        SortColumn::State => ordered(state(a).cmp(state(b)), descending),
    };
    value.then_with(|| a.id.cmp(&b.id))
}
fn group(c: &Candidate, by: GroupBy) -> String {
    match by {
        GroupBy::None => String::new(),
        GroupBy::Folder => c
            .original_path
            .replace('\\', "/")
            .rsplit_once('/')
            .map(|(p, _)| p.to_owned())
            .unwrap_or_else(|| "ルート / 階層不明".into()),
        GroupBy::Type => file_type(c),
        GroupBy::State => state(c).into(),
        GroupBy::Created | GroupBy::Modified => {
            let date = if by == GroupBy::Created {
                &c.created
            } else {
                &c.modified
            };
            date.as_ref()
                .map(|d| d.value.chars().take(10).collect())
                .unwrap_or_else(|| "日時不明".into())
        }
        GroupBy::Size => match c.size {
            None => "サイズ不明",
            Some(0) => "0 B",
            Some(1..=1048575) => "1 B – 1 MB未満",
            Some(1048576..=1073741823) => "1 MB – 1 GB未満",
            Some(_) => "1 GB以上",
        }
        .into(),
        GroupBy::Initial => c
            .name
            .chars()
            .next()
            .map(|c| {
                if c.is_ascii_digit() {
                    "0 – 9".into()
                } else {
                    c.to_uppercase().to_string()
                }
            })
            .unwrap_or_else(|| "名前不明".into()),
    }
}
pub fn rows(
    candidates: &[Candidate],
    filter: &str,
    sort: SortColumn,
    descending: bool,
    by: GroupBy,
    closed: &BTreeSet<String>,
) -> Vec<Row> {
    let needle = filter.to_lowercase();
    let mut groups = BTreeMap::<String, Vec<usize>>::new();
    for (i, c) in candidates
        .iter()
        .enumerate()
        .filter(|(_, c)| c.matches_path_text(&needle))
    {
        groups.entry(group(c, by)).or_default().push(i);
    }
    let mut rows = Vec::new();
    for (key, mut indices) in groups {
        indices.sort_by(|a, b| compare(&candidates[*a], &candidates[*b], sort, descending));
        if by != GroupBy::None {
            rows.push(Row::Group(key.clone(), indices.len()));
        }
        if !closed.contains(&key) {
            rows.extend(indices.into_iter().map(Row::File));
        }
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;
    fn c(id: &str, size: Option<u64>) -> Candidate {
        Candidate {
            id: id.into(),
            record: 0,
            sequence: 0,
            stream_name: None,
            name: format!("{id}.png"),
            original_path: format!("photos/{id}.png"),
            path_verified: true,
            alternate_names: Vec::new(),
            size,
            supported: true,
            warnings: vec![],
            record_sha256: String::new(),
            extent_evidence: Default::default(),
            allocation_status: Default::default(),
            created: None,
            modified: None,
        }
    }
    #[test]
    fn size_sort_is_numeric_and_unknown_stays_last_in_both_directions() {
        let items = [
            c("large", Some(100)),
            c("small", Some(9)),
            c("unknown", None),
        ];
        for (descending, expected) in [(false, [1, 0, 2]), (true, [0, 1, 2])] {
            let indices: Vec<_> = rows(
                &items,
                "",
                SortColumn::Size,
                descending,
                GroupBy::None,
                &BTreeSet::new(),
            )
            .into_iter()
            .map(|r| match r {
                Row::File(i) => i,
                _ => panic!(),
            })
            .collect();
            assert_eq!(indices, expected);
        }
    }
    #[test]
    fn timezone_sort_uses_instants_and_groups_unknown_zones_separately() {
        let mut early = c("early", None);
        let mut later = c("later", None);
        let mut local = c("local", None);
        early.created = Some(FileTimestamp {
            value: "2026-01-02T09:00:00+09:00".into(),
            unix_seconds: Some(10),
        });
        later.created = Some(FileTimestamp {
            value: "2026-01-02T01:00:00Z".into(),
            unix_seconds: Some(20),
        });
        local.created = Some(FileTimestamp {
            value: "1980-01-01T00:00:00".into(),
            unix_seconds: None,
        });
        assert_eq!(
            compare(&early, &later, SortColumn::Created, false),
            Ordering::Less
        );
        assert_eq!(
            compare(&later, &local, SortColumn::Created, true),
            Ordering::Less
        );
    }
    #[test]
    fn collapsed_groups_keep_counts_and_filter_matches_full_path() {
        let items = [c("a", Some(1)), c("b", Some(2))];
        let closed = BTreeSet::from(["photos".into()]);
        let rows = rows(
            &items,
            "PHOTOS",
            SortColumn::Name,
            false,
            GroupBy::Folder,
            &closed,
        );
        assert!(matches!(&rows[..],[Row::Group(name,2)] if name == "photos"));
    }
}
