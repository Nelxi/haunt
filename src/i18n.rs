//! Interface languages.
//!
//! The English text is the key: `tr!("Save")`, `tr!("{n} saved {n:server|servers}", n = 3)`.
//! Each language is a table of (English, translation) pairs in `src/i18n/<code>.rs`; a
//! missing entry falls back to English. A test checks that every `tr!` key in the sources
//! is translated everywhere and that the placeholders match.
//!
//! Placeholders: `{name}` is replaced by the argument; `{name:one|few|many}` picks a word
//! form by the number in `name` using the language's plural rules (English and the
//! Romance/Germanic languages use two forms, `{n:file|files}`; Russian and Ukrainian three).

use std::collections::HashMap;
use std::fmt::Display;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU8, Ordering};

mod de;
mod es;
mod fr;
mod ru;
mod uk;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lang {
    En,
    Ru,
    Uk,
    De,
    Es,
    Fr,
}

impl Lang {
    pub const ALL: [Lang; 6] = [Lang::En, Lang::Ru, Lang::Uk, Lang::De, Lang::Es, Lang::Fr];

    /// The code stored in the config.
    pub fn code(self) -> &'static str {
        match self {
            Lang::En => "en",
            Lang::Ru => "ru",
            Lang::Uk => "uk",
            Lang::De => "de",
            Lang::Es => "es",
            Lang::Fr => "fr",
        }
    }

    /// The language's own name, shown in the picker.
    pub fn native_name(self) -> &'static str {
        match self {
            Lang::En => "English",
            Lang::Ru => "Русский",
            Lang::Uk => "Українська",
            Lang::De => "Deutsch",
            Lang::Es => "Español",
            Lang::Fr => "Français",
        }
    }

    pub fn from_code(code: &str) -> Option<Lang> {
        let code = code.trim().to_ascii_lowercase();
        let base = code.split(['-', '_', '.']).next().unwrap_or("");
        Lang::ALL.into_iter().find(|l| l.code() == base)
    }

    fn table(self) -> &'static [(&'static str, &'static str)] {
        match self {
            Lang::En => &[],
            Lang::Ru => ru::T,
            Lang::Uk => uk::T,
            Lang::De => de::T,
            Lang::Es => es::T,
            Lang::Fr => fr::T,
        }
    }

    fn index(self) -> usize {
        Lang::ALL.iter().position(|l| *l == self).unwrap_or(0)
    }
}

static CURRENT: AtomicU8 = AtomicU8::new(0);

/// `setting` is the config value: a language code, or empty for the system language.
pub fn set(setting: &str) {
    let lang = Lang::from_code(setting).unwrap_or_else(system);
    CURRENT.store(lang.index() as u8, Ordering::Relaxed);
}

pub fn current() -> Lang {
    Lang::ALL[(CURRENT.load(Ordering::Relaxed) as usize).min(Lang::ALL.len() - 1)]
}

/// The Windows display language (or `LANG` elsewhere); English when it is not one of ours.
pub fn system() -> Lang {
    #[cfg(windows)]
    {
        // SAFETY: no arguments, returns a LANGID.
        let id = unsafe { windows_sys::Win32::Globalization::GetUserDefaultUILanguage() };
        match id & 0x3ff {
            0x19 | 0x23 | 0x3f => Lang::Ru, // Russian; Belarusian and Kazakh users mostly read Russian
            0x22 => Lang::Uk,
            0x07 => Lang::De,
            0x0a => Lang::Es,
            0x0c => Lang::Fr,
            _ => Lang::En,
        }
    }
    #[cfg(not(windows))]
    {
        std::env::var("LC_ALL")
            .or_else(|_| std::env::var("LANG"))
            .ok()
            .and_then(|v| Lang::from_code(&v))
            .unwrap_or(Lang::En)
    }
}

fn maps() -> &'static [HashMap<&'static str, &'static str>] {
    static MAPS: OnceLock<Vec<HashMap<&'static str, &'static str>>> = OnceLock::new();
    MAPS.get_or_init(|| Lang::ALL.iter().map(|l| l.table().iter().copied().collect()).collect())
}

/// The text for `key` in the current language.
pub fn tr(key: &'static str) -> &'static str {
    tr_in(current(), key)
}

pub fn tr_in(lang: Lang, key: &'static str) -> &'static str {
    maps()[lang.index()].get(key).copied().unwrap_or(key)
}

/// Which plural form (0, 1 or 2) a number takes.
fn plural_form(lang: Lang, n: i64) -> usize {
    let n = n.unsigned_abs();
    match lang {
        Lang::Ru | Lang::Uk => {
            let (d, h) = (n % 10, n % 100);
            if d == 1 && h != 11 {
                0
            } else if (2..=4).contains(&d) && !(12..=14).contains(&h) {
                1
            } else {
                2
            }
        }
        Lang::Fr => usize::from(n > 1),
        _ => usize::from(n != 1),
    }
}

/// Fills `{name}` and `{name:a|b|c}` in `text` from `args`.
pub fn format_in(lang: Lang, text: &str, args: &[(&str, &dyn Display)]) -> String {
    let mut out = String::with_capacity(text.len() + 16);
    let mut rest = text;
    while let Some(open) = rest.find('{') {
        out.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        let Some(close) = after.find('}') else {
            out.push_str(&rest[open..]);
            return out;
        };
        let inner = &after[..close];
        let (name, forms) = match inner.split_once(':') {
            Some((n, f)) => (n, Some(f)),
            None => (inner, None),
        };
        match args.iter().find(|(n, _)| *n == name) {
            Some((_, value)) => match forms {
                None => out.push_str(&value.to_string()),
                Some(forms) => {
                    let n: i64 = value.to_string().trim().parse().unwrap_or(0);
                    let forms: Vec<&str> = forms.split('|').collect();
                    let i = plural_form(lang, n).min(forms.len().saturating_sub(1));
                    out.push_str(forms.get(i).copied().unwrap_or(""));
                }
            },
            // Not ours (or a typo): keep it visible rather than silently dropping text.
            None => out.push_str(&rest[open..open + close + 2]),
        }
        rest = &after[close + 1..];
    }
    out.push_str(rest);
    out
}

pub fn format(text: &str, args: &[(&str, &dyn Display)]) -> String {
    format_in(current(), text, args)
}

/// `tr!("Save")` → `&'static str`; `tr!("{n} {n:file|files}", n = count)` → `String`.
#[macro_export]
macro_rules! tr {
    ($key:literal) => {
        $crate::i18n::tr($key)
    };
    ($key:literal, $($name:ident = $value:expr),+ $(,)?) => {
        $crate::i18n::format($crate::i18n::tr($key), &[$((stringify!($name), &$value as &dyn std::fmt::Display)),+])
    };
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    /// Every `tr!("...")` key in the sources, unescaped.
    fn source_keys() -> BTreeSet<String> {
        let mut keys = BTreeSet::new();
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut stack = vec![dir];
        while let Some(d) = stack.pop() {
            for e in std::fs::read_dir(&d).unwrap().flatten() {
                let p = e.path();
                if p.is_dir() {
                    stack.push(p);
                    continue;
                }
                // The tables and this module (its docs show `tr!` examples) are not sources of keys.
                let is_i18n = p.parent().is_some_and(|x| x.ends_with("i18n")) || p.file_name().is_some_and(|n| n == "i18n.rs");
                if p.extension().is_none_or(|x| x != "rs") || is_i18n {
                    continue;
                }
                let text = std::fs::read_to_string(&p).unwrap();
                let mut rest = text.as_str();
                while let Some(i) = rest.find("tr!(") {
                    // Long keys sit on their own line: `tr!(\n    "...",`.
                    let after = rest[i + 4..].trim_start();
                    let Some(s) = after.strip_prefix('"') else {
                        rest = &rest[i + 4..];
                        continue;
                    };
                    let mut key = String::new();
                    let mut chars = s.chars();
                    while let Some(c) = chars.next() {
                        match c {
                            '\\' => match chars.next() {
                                Some('n') => key.push('\n'),
                                Some('"') => key.push('"'),
                                Some('\\') => key.push('\\'),
                                Some(o) => {
                                    key.push('\\');
                                    key.push(o);
                                }
                                None => break,
                            },
                            '"' => break,
                            c => key.push(c),
                        }
                    }
                    keys.insert(key);
                    rest = s;
                }
            }
        }
        keys
    }

    fn placeholders(s: &str) -> BTreeSet<String> {
        let mut set = BTreeSet::new();
        let mut rest = s;
        while let Some(i) = rest.find('{') {
            let Some(j) = rest[i..].find('}') else { break };
            let inner = &rest[i + 1..i + j];
            set.insert(inner.split(':').next().unwrap_or("").to_string());
            rest = &rest[i + j + 1..];
        }
        set
    }

    #[test]
    fn every_key_is_translated_with_the_same_placeholders() {
        let keys = source_keys();
        assert!(keys.len() > 100, "found only {} keys", keys.len());
        let mut problems = Vec::new();
        for lang in Lang::ALL.into_iter().filter(|l| *l != Lang::En) {
            let table: HashMap<&str, &str> = lang.table().iter().copied().collect();
            if table.len() != lang.table().len() {
                problems.push(format!("{lang:?}: duplicate keys"));
            }
            for k in &keys {
                match table.get(k.as_str()) {
                    None => problems.push(format!("{lang:?}: missing {k:?}")),
                    Some(v) if placeholders(v) != placeholders(k) => {
                        problems.push(format!("{lang:?}: placeholders differ in {k:?}"))
                    }
                    Some(v) if v.trim().is_empty() => problems.push(format!("{lang:?}: empty {k:?}")),
                    _ => {}
                }
            }
            for (k, _) in lang.table() {
                if !keys.contains(*k) {
                    problems.push(format!("{lang:?}: unused {k:?}"));
                }
            }
        }
        assert!(problems.is_empty(), "{}", problems.join("\n"));
    }

    #[test]
    fn plurals_follow_each_language() {
        let f = |lang, n: i64| format_in(lang, "{n} {n:a|b|c}", &[("n", &n)]);
        assert_eq!(f(Lang::En, 1), "1 a");
        assert_eq!(f(Lang::En, 0), "0 b");
        assert_eq!(f(Lang::En, 5), "5 b");
        assert_eq!(f(Lang::Fr, 0), "0 a");
        assert_eq!(f(Lang::Fr, 2), "2 b");
        for (n, form) in [(1, "a"), (21, "a"), (11, "c"), (2, "b"), (24, "b"), (12, "c"), (5, "c"), (0, "c"), (101, "a"), (112, "c")] {
            assert_eq!(f(Lang::Ru, n), format!("{n} {form}"), "ru {n}");
            assert_eq!(f(Lang::Uk, n), format!("{n} {form}"), "uk {n}");
        }
    }

    #[test]
    fn unknown_placeholders_stay_visible() {
        assert_eq!(format_in(Lang::En, "a {x} b", &[]), "a {x} b");
        assert_eq!(format_in(Lang::En, "{host}:{port}", &[("host", &"h"), ("port", &22)]), "h:22");
        assert_eq!(format_in(Lang::En, "open {", &[]), "open {");
    }

    #[test]
    fn codes_parse_with_regions() {
        assert_eq!(Lang::from_code("ru_RU.UTF-8"), Some(Lang::Ru));
        assert_eq!(Lang::from_code("de-AT"), Some(Lang::De));
        assert_eq!(Lang::from_code(""), None);
        assert_eq!(Lang::from_code("xx"), None);
    }
}
