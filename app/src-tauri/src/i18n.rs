use crate::models::{LanguagePreference, SupportedLocale};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NativeText {
    pub app_title: &'static str,
    pub open: &'static str,
    pub quit: &'static str,
}

pub fn native_text(locale: SupportedLocale) -> NativeText {
    match locale {
        SupportedLocale::ZhCn => NativeText {
            app_title: "对话归档",
            open: "打开对话归档",
            quit: "退出",
        },
        SupportedLocale::EnUs => NativeText {
            app_title: "Conversation Archive",
            open: "Open Conversation Archive",
            quit: "Quit",
        },
    }
}

pub fn resolve_locale<I, S>(preference: LanguagePreference, system_locales: I) -> SupportedLocale
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    if let Some(locale) = preference.supported_locale() {
        return locale;
    }

    if system_locales.into_iter().any(|locale| {
        let normalized = locale.as_ref().replace('_', "-");
        normalized.eq_ignore_ascii_case("zh")
            || normalized
                .get(..3)
                .is_some_and(|prefix| prefix.eq_ignore_ascii_case("zh-"))
    }) {
        SupportedLocale::ZhCn
    } else {
        SupportedLocale::EnUs
    }
}

pub fn resolve_native_locale(preference: LanguagePreference) -> SupportedLocale {
    resolve_locale(preference, sys_locale::get_locales())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{LanguagePreference, SupportedLocale};

    #[test]
    fn explicit_locale_wins_over_system_locales() {
        assert_eq!(
            resolve_locale(LanguagePreference::EnUs, ["zh-CN"]),
            SupportedLocale::EnUs
        );
    }

    #[test]
    fn system_locale_uses_chinese_when_any_os_preference_is_zh() {
        assert_eq!(
            resolve_locale(LanguagePreference::System, ["en-US", "zh-Hans-CN"]),
            SupportedLocale::ZhCn
        );
        assert_eq!(
            resolve_locale(LanguagePreference::System, ["zh"]),
            SupportedLocale::ZhCn
        );
    }

    #[test]
    fn system_locale_defaults_to_english_for_non_chinese_or_empty_lists() {
        assert_eq!(
            resolve_locale(LanguagePreference::System, ["en-GB", "fr-FR"]),
            SupportedLocale::EnUs
        );
        assert_eq!(
            resolve_locale(LanguagePreference::System, std::iter::empty::<&str>()),
            SupportedLocale::EnUs
        );
    }

    #[test]
    fn native_dictionary_contains_window_and_tray_copy() {
        assert_eq!(
            native_text(SupportedLocale::EnUs).app_title,
            "Conversation Archive"
        );
        assert_eq!(native_text(SupportedLocale::EnUs).quit, "Quit");
        assert_eq!(native_text(SupportedLocale::ZhCn).open, "打开对话归档");
    }
}
