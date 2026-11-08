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
            Self::None => tr!("なし", "None"),
            Self::Folder => tr!("元のフォルダー", "Original folder"),
            Self::Type => tr!("種類", "Type"),
            Self::Created => tr!("作成日", "Date created"),
            Self::Modified => tr!("更新日", "Date modified"),
            Self::Size => tr!("サイズ", "Size"),
            Self::Initial => tr!("名前の先頭", "Name initial"),
            Self::State => tr!("復元可否", "Recovery status"),
        }
    }
}
#[derive(Clone, Debug)]
pub enum Row {
    Group(String, usize),
    File(usize),
}

pub fn file_type(c: &Candidate) -> String {
    let extension = rescue_core::file_extension(&c.name, c.stream_name.as_deref());
    if extension.is_empty() {
        tr!("ファイル", "File").into()
    } else {
        tr_format!("{} ファイル", "{} file", extension.to_uppercase())
    }
}
pub fn state(c: &Candidate) -> &'static str {
    if !c.supported {
        tr!("復元不可", "Unavailable")
    } else if c.extent_evidence == rescue_core::ExtentEvidence::EstimatedContiguous {
        tr!("推定（内容未確認）", "Estimated (unverified)")
    } else if c.warnings.is_empty() {
        tr!("復元候補", "Candidate")
    } else {
        tr!("要確認", "Review needed")
    }
}
pub fn extent_label(evidence: rescue_core::ExtentEvidence) -> &'static str {
    use rescue_core::ExtentEvidence::*;
    match evidence {
        EstimatedContiguous => tr!(
            "推定連続配置・元の内容との一致は未確認",
            "Estimated contiguous layout; original content unverified"
        ),
        Metadata => tr!("残存する管理情報", "Surviving filesystem metadata"),
        MetadataRemnant => tr!(
            "現在の管理領域外に残存する記録",
            "Metadata remnant outside the current table"
        ),
        ResidualFatChain => tr!("残存するFATチェーン", "Surviving FAT chain"),
        SingleCluster => tr!(
            "開始クラスタ内の範囲",
            "Extent within the known first cluster"
        ),
        NoFatChain => tr!(
            "連続配置を示すexFAT管理情報",
            "exFAT metadata declares a contiguous extent"
        ),
        PngValidated | JpegValidated | PdfStructure | ZipCrcValidated | MediaStructure => tr!(
            "形式の構造を検証・元の内容との一致は未確認",
            "Format structure checked; original content unverified"
        ),
        Unknown => tr!("配置の根拠は不明", "Extent evidence unknown"),
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
        Some(d) if d.unix_seconds.is_some() => tr!(
            "管理情報に記録された日時・タイムゾーン",
            "Date and timezone recorded in the metadata"
        ),
        Some(_) => tr!(
            "元の媒体のローカル時刻。タイムゾーンは不明です。",
            "Local time on the source medium. Timezone unknown."
        ),
        None => tr!(
            "日時の管理情報が残っていません。",
            "No date metadata remains."
        ),
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
                (Some(sa), Some(sb)) => sa.cmp(&sb).then_with(|| {
                    let fraction = |value: &str| {
                        let mut result = [b'0'; 9];
                        if let Some((_, rest)) = value.split_once('.') {
                            for (i, c) in rest
                                .bytes()
                                .take_while(u8::is_ascii_digit)
                                .take(9)
                                .enumerate()
                            {
                                result[i] = c;
                            }
                        }
                        result
                    };
                    fraction(&a.value).cmp(&fraction(&b.value))
                }),
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
            .unwrap_or_else(|| tr!("ルート / 階層不明", "Root / Unknown hierarchy").into()),
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
                .unwrap_or_else(|| tr!("日時不明", "Unknown date").into())
        }
        GroupBy::Size => match c.size {
            None => tr!("サイズ不明", "Unknown size"),
            Some(0) => "0 B",
            Some(1..=1048575) => tr!("1 B – 1 MB未満", "1 B – less than 1 MB"),
            Some(1048576..=1073741823) => tr!("1 MB – 1 GB未満", "1 MB – less than 1 GB"),
            Some(_) => tr!("1 GB以上", "1 GB or larger"),
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
            .unwrap_or_else(|| tr!("名前不明", "Unknown name").into()),
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

/// Keep the global SQL order, including group and unknown-value ranks. Group
/// counts describe only the visible page and never imply a whole-index count.
pub fn indexed_rows(candidates: &[Candidate], by: GroupBy, closed: &BTreeSet<String>) -> Vec<Row> {
    let mut rows = vec![];
    let mut at = 0;
    while at < candidates.len() {
        let key = group(&candidates[at], by);
        let mut end = at + 1;
        while end < candidates.len() && group(&candidates[end], by) == key {
            end += 1;
        }
        if by != GroupBy::None {
            rows.push(Row::Group(key.clone(), end - at));
        }
        if !closed.contains(&key) {
            rows.extend((at..end).map(Row::File));
        }
        at = end;
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;
    fn c(id: &str, size: Option<u64>) -> Candidate {
        Candidate {
            fragment_offset: None,
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
