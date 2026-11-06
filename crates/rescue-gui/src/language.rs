//! UI-owned translations. Source names, paths and engine diagnostics are data:
//! they never pass through a replacement dictionary.
use std::{cell::Cell, fmt};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Language {
    #[default]
    Japanese,
    English,
}

thread_local! {
    static CURRENT: Cell<Language> = const { Cell::new(Language::Japanese) };
}

pub struct Guard(Language);
impl Drop for Guard {
    fn drop(&mut self) {
        CURRENT.set(self.0);
    }
}
impl Language {
    pub fn current() -> Self {
        CURRENT.get()
    }
    pub fn enter(self) -> Guard {
        Guard(CURRENT.replace(self))
    }
    pub fn name(self) -> &'static str {
        match self {
            Self::Japanese => "日本語",
            Self::English => "English",
        }
    }
}

pub fn choose(japanese: &'static str, english: &'static str) -> &'static str {
    match Language::current() {
        Language::Japanese => japanese,
        Language::English => english,
    }
}
pub fn with<T>(language: Language, build: impl FnOnce() -> T) -> T {
    let _guard = language.enter();
    build()
}

/// Keep both forms of asynchronous status messages. Switching the UI language
/// does not require restarting a job or translating any of its interpolated data.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Text {
    japanese: String,
    english: String,
}
impl Text {
    pub fn new(japanese: String, english: String) -> Self {
        Self { japanese, english }
    }
    pub fn is_empty(&self) -> bool {
        self.japanese.is_empty() && self.english.is_empty()
    }
    pub fn clear(&mut self) {
        self.japanese.clear();
        self.english.clear();
    }
    pub fn get(&self, language: Language) -> &str {
        match language {
            Language::Japanese => &self.japanese,
            Language::English => &self.english,
        }
    }
}
impl From<String> for Text {
    fn from(value: String) -> Self {
        Self::new(value.clone(), value)
    }
}
impl From<&str> for Text {
    fn from(value: &str) -> Self {
        value.to_owned().into()
    }
}
impl fmt::Display for Text {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.get(Language::current()))
    }
}
impl From<&Text> for eframe::egui::WidgetText {
    fn from(value: &Text) -> Self {
        value.to_string().into()
    }
}
impl From<&Text> for eframe::egui::RichText {
    fn from(value: &Text) -> Self {
        Self::new(value.to_string())
    }
}

macro_rules! tr {
    ($ja:literal, $en:literal) => {
        crate::language::choose($ja, $en)
    };
}
macro_rules! tr_format {
    ($ja:literal, $en:literal $($args:tt)*) => {
        match crate::language::Language::current() {
            crate::language::Language::Japanese => format!($ja $($args)*),
            crate::language::Language::English => format!($en $($args)*),
        }
    };
}
macro_rules! message {
    ($ja:literal, $en:literal $(, $arg:expr)* $(,)?) => {
        message!(@bind [$ja, $en] [a b c d e f g h i j k l] [] [$($arg),*])
    };
    (@bind [$ja:literal, $en:literal] [$name:ident $($rest:ident)*] [$($bound:ident),*] [$arg:expr $(,$tail:expr)*]) => {{
        // Evaluate each argument once, including expressions that create a report.
        match &$arg {
            $name => message!(@bind [$ja, $en] [$($rest)*] [$($bound,)* $name] [$($tail),*]),
        }
    }};
    (@bind [$ja:literal, $en:literal] [$($rest:ident)*] [$($bound:ident),*] []) => {
        crate::language::Text::new(
            crate::language::with(crate::language::Language::Japanese, || format!($ja $(,$bound)*)),
            crate::language::with(crate::language::Language::English, || format!($en $(,$bound)*)),
        )
    };
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn scope_and_nested_message_arguments_follow_the_selected_language() {
        let _guard = Language::English.enter();
        let value = message!("保存: {}", "Saved: {}", message!("完了", "complete"));
        assert_eq!(value.get(Language::Japanese), "保存: 完了");
        assert_eq!(value.get(Language::English), "Saved: complete");
        assert_eq!(Language::current(), Language::English);
        assert_eq!(tr_format!("{}件", "{} files", 3), "3 files");
    }
    #[test]
    fn formatting_evaluates_each_argument_once() {
        let mut count = 0;
        let value = message!("件数 {}", "Count {}", {
            count += 1;
            count
        });
        assert_eq!(count, 1);
        assert_eq!(value.get(Language::Japanese), "件数 1");
        assert_eq!(value.get(Language::English), "Count 1");
    }
    #[test]
    fn worker_messages_change_language_without_changing_user_data() {
        let path = "C:/資料/保存完了.txt";
        let message = std::thread::spawn(move || message!("保存先: {path}", "Destination: {path}"))
            .join()
            .unwrap();
        assert_eq!(
            message.get(Language::English),
            "Destination: C:/資料/保存完了.txt"
        );
        assert_eq!(
            message.get(Language::Japanese),
            "保存先: C:/資料/保存完了.txt"
        );
        let diagnostic = Text::from(path);
        assert_eq!(diagnostic.get(Language::English), path);
    }
}
